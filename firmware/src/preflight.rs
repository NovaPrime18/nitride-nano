//! First-enable interlock: hardware self-tests that run before the converter is
//! ever allowed to produce output.
//!
//! Most "it blew up on the bench" scenarios are detectable *before* the output
//! stage is commanded on:
//!
//! * a broken analogue reference — the firmware then cannot see the output at
//!   all, so every regulation and protection decision is blind;
//! * a missing current monitor — no input over-current/over-voltage backstop;
//! * an invalid thermistor — no thermal supervision;
//! * a shorted or unsoldered gate-drive line — the three-pin power-up sequence
//!   would silently invert, i.e. the exact "half-enabled output stage" hazard;
//! * an input above the design limit.
//!
//! Each check logs exactly one `preflight:` line with the measured values, a
//! `[ ok ]` / `[warn]` / `[FAIL]` tag, and — for anything that is not `ok` —
//! what to do about it. Any `[FAIL]` **blocks the converter enable** (the
//! supply controller refuses, `Fault::PreflightFailed` is latched so the OLED
//! and the status LED say so) unless [`board::PREFLIGHT_BLOCK_ENABLE`] is
//! cleared for a supervised bench run.
//!
//! The split is deliberate: a *warning* still allows an enable, because it
//! describes a bench configuration that is not by itself hazardous (no USB-C
//! attached, so no PD controller and no input monitor; a stale output charge;
//! a display that is not seated). A *failure* means the firmware would be
//! driving power hardware it cannot measure or supervise — that is the case
//! worth refusing.

use crate::board;

/// One check's outcome. Kept as a log-free value so callers stay readable.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Warn,
    Fail,
    /// Not applicable in this configuration (e.g. no PD bus on an XT90 feed):
    /// neither a failure nor a warning, but worth naming so the log is honest.
    Skip,
}

/// Running tally of the boot self-test, and the single gate bit the supply
/// controller consumes ([`Preflight::may_enable`]).
pub struct Preflight {
    pub checks: u8,
    pub warned: u8,
    pub failed: u8,
    pub skipped: u8,
    /// True when the USB-PD side is powered (USB-C VBUS present). When false the
    /// PD controller and both INA228s are simply absent by design and their
    /// checks report "not applicable" instead of warnings.
    pd_bus_powered: bool,
}

impl Preflight {
    pub const fn new(pd_bus_powered: bool) -> Self {
        Self {
            checks: 0,
            warned: 0,
            failed: 0,
            skipped: 0,
            pd_bus_powered,
        }
    }

    fn tally(&mut self, v: Verdict) {
        self.checks = self.checks.saturating_add(1);
        match v {
            Verdict::Pass => {}
            Verdict::Warn => self.warned = self.warned.saturating_add(1),
            Verdict::Fail => self.failed = self.failed.saturating_add(1),
            Verdict::Skip => self.skipped = self.skipped.saturating_add(1),
        }
    }

    /// True when the converter may be enabled. `PREFLIGHT_BLOCK_ENABLE = false`
    /// downgrades failures to log-only for a supervised bench run.
    pub fn may_enable(&self) -> bool {
        self.failed == 0 || !board::PREFLIGHT_BLOCK_ENABLE
    }

    // -----------------------------------------------------------------
    // 1. Analogue reference (the check that matters most)
    // -----------------------------------------------------------------

    /// The ADC/DAC reference must be real: the internal 1.212 V reference reads
    /// ~1500 counts against VREF+ = 3.3 V, on *two* ADC instances so a single
    /// damaged ADC is also visible.
    ///
    /// This is the check that fails on a broken VREF+/VDDA/VSSA joint or a
    /// damaged analogue front-end, and it is fatal: the DACs share the same
    /// reference, so CV/CC commands would be arbitrary.
    pub fn adc_reference(&mut self, vrefint_adc1: u16, vrefint_adc5: u16, ts_raw: u16) {
        let ok1 = (1_200..=1_800).contains(&vrefint_adc1);
        let ok5 = (1_200..=1_800).contains(&vrefint_adc5);
        let ts_ok = ts_raw != 0 && ts_raw < 4095;
        let v = if ok1 && ok5 && ts_ok {
            Verdict::Pass
        } else {
            Verdict::Fail
        };
        self.tally(v);
        match v {
            Verdict::Pass => defmt::info!(
                "preflight: [ ok ] adc reference: VREFINT ADC1={} ADC5={} internal-TS={} (VREF+ = 3.3 V)",
                vrefint_adc1,
                vrefint_adc5,
                ts_raw
            ),
            _ => defmt::error!(
                "preflight: [FAIL] adc reference is not usable: VREFINT ADC1={} ADC5={} (expect ~1200..1800), internal-TS={} (must be inside the rails) - the ADC and BOTH DACs share VREF+/VDDA (U2 pins 20/21) and the analogue ground (exposed pad); with this failed every regulation and protection decision is blind. Reflow VREF+/VDDA/EP before enabling anything",
                vrefint_adc1,
                vrefint_adc5,
                ts_raw
            ),
        }
    }

    // -----------------------------------------------------------------
    // 2. Thermal sensors
    // -----------------------------------------------------------------

    /// Both thermistors must read a plausible temperature. A failed NTC reads as
    /// `NTC_INVALID_C` and latches `Fault::OverTemp` at runtime anyway; failing
    /// here just says so earlier and more clearly.
    pub fn thermistors(&mut self, conv_c: i32, in_c: i32) {
        if !board::NTC_SENSE_ENABLE {
            self.tally(Verdict::Warn);
            defmt::warn!(
                "preflight: [warn] thermistors NOT supervised (board::NTC_SENSE_ENABLE = false): converter={} C input={} C - there is no over-temperature protection in this build",
                conv_c,
                in_c
            );
            return;
        }
        let plausible = |t: i32| (-40..=125).contains(&t);
        let v = if plausible(conv_c) && plausible(in_c) {
            Verdict::Pass
        } else {
            Verdict::Fail
        };
        self.tally(v);
        match v {
            Verdict::Pass => defmt::info!(
                "preflight: [ ok ] thermistors: converter={} C input={} C (limit {} C)",
                conv_c,
                in_c,
                board::NTC_OVERTEMP_C
            ),
            _ => defmt::error!(
                "preflight: [FAIL] thermistor readings are not plausible: converter={} C input={} C (a failed NTC reads 1000 C) - there would be NO thermal protection. Check the NTC joints and the 3V3 divider, or set board::NTC_SENSE_ENABLE = false for a bench run with no thermistors fitted",
                conv_c,
                in_c
            ),
        }
    }

    // -----------------------------------------------------------------
    // 3. Power-path gate lines
    // -----------------------------------------------------------------

    /// The three power-path gates plus the range select must read back at the
    /// level the firmware drives them to. `PB2` (PWR_G) shorted high would close
    /// the load switch with no converter regulation behind it; `PB3`
    /// (CNV-EN-LOADEN) shorted high would release the bridge while the firmware
    /// believes it is parked. Those are precisely the half-enabled states this
    /// interlock exists to prevent.
    pub fn power_path_pins(&mut self, pb1: bool, pb2: bool, pb3: bool, pb6: bool) {
        // Safe boot state: PB1 high (disable asserted), PB2/PB3/PB6 low.
        let v = if pb1 && !pb2 && !pb3 && !pb6 {
            Verdict::Pass
        } else {
            Verdict::Fail
        };
        self.tally(v);
        match v {
            Verdict::Pass => defmt::info!(
                "preflight: [ ok ] power-path gate lines read back as driven: PB1=1 (chip disabled) PB2=0 (load open) PB3=0 (bridge parked) PB6=0 (x1 range)"
            ),
            _ => defmt::error!(
                "preflight: [FAIL] power-path gate line readback mismatch: PB1={} PB2={} PB3={} PB6={} (expected 1/0/0/0) - one of those pins is shorted to a rail or not connected, so the three-pin enable sequence cannot be trusted. Check the PB1/PB2/PB3/PB6 joints and Q12/Q7/Q13 before enabling",
                pb1 as u8,
                pb2 as u8,
                pb3 as u8,
                pb6 as u8
            ),
        }
    }

    // -----------------------------------------------------------------
    // 4. Current / power monitors
    // -----------------------------------------------------------------

    /// Input monitor (U9) presence. Without it there is no software
    /// input-over-current/over-voltage backstop; the converter still has its own
    /// CC/CV loop and the output over-voltage guard, so this is a warning on a
    /// current-limited bench supply, but it must be a conscious choice.
    pub fn input_monitor(&mut self, present: bool) {
        let v = if present {
            Verdict::Pass
        } else if self.pd_bus_powered {
            Verdict::Warn
        } else {
            Verdict::Skip
        };
        self.tally(v);
        match v {
            Verdict::Pass => defmt::info!(
                "preflight: [ ok ] input INA228 (U9) present - input over-current/over-voltage backstop active"
            ),
            Verdict::Skip => defmt::info!(
                "preflight: [ -- ] input monitor n/a: no PD bus (XT90/DC input mode - the TPS26750's +3V3_LDO is off, so U9 is unreachable). Input current is NOT software-monitored in this mode; its over-current is left to the source's own limit and the hardware"
            ),
            _ => defmt::warn!(
                "preflight: [warn] input INA228 (U9) is not answering on a powered PD bus - check its joints and address straps (0x40); the input over-current/over-voltage backstop is inactive"
            ),
        }
    }

    /// Output monitor (U12) presence. Not fatal: the sequencer refuses to close
    /// the load switch without it (`OUTPUT_SWITCH_REQUIRE_INA`), so the safe
    /// outcome is automatic.
    pub fn output_monitor(&mut self, present: bool) {
        let v = if present {
            Verdict::Pass
        } else if self.pd_bus_powered {
            Verdict::Warn
        } else {
            Verdict::Skip
        };
        self.tally(v);
        match v {
            Verdict::Pass => defmt::info!(
                "preflight: [ ok ] output INA228 (U12) present - load-switch closure can be verified"
            ),
            Verdict::Skip => defmt::info!(
                "preflight: [ -- ] output monitor n/a: no PD bus (XT90/DC input mode). Displayed output current falls back to ISMON, and the load switch is confirmed by output voltage alone instead of by U12's shunt current"
            ),
            _ => defmt::warn!(
                "preflight: [warn] output INA228 (U12) is not answering - an enable will run the ramp but the sequencer will refuse to close the load switch (OUTPUT_SWITCH_REQUIRE_INA), so the load stays disconnected"
            ),
        }
    }

    /// TPS26750 USB-PD controller presence. Purely informational: an XT90/DC
    /// feed is a legitimate bench input.
    pub fn pd_controller(&mut self, present: bool) {
        let v = if present {
            Verdict::Pass
        } else if self.pd_bus_powered {
            Verdict::Warn
        } else {
            Verdict::Skip
        };
        self.tally(v);
        match v {
            Verdict::Pass => defmt::info!("preflight: [ ok ] TPS26750 USB-PD controller present"),
            Verdict::Skip => defmt::info!(
                "preflight: [ -- ] USB-PD n/a: no PD bus (XT90/DC input mode) - XT90 is a first-class input, input voltage comes from the ADC divider. Attach a USB-C source any time to bring the PD side up"
            ),
            _ => defmt::warn!(
                "preflight: [warn] TPS26750 is not answering on a powered PD bus - check the controller's joints and its EEPROM image (JP4/JP5 bridged, BENCH §4)"
            ),
        }
    }

    // -----------------------------------------------------------------
    // 5. Input and output state
    // -----------------------------------------------------------------

    /// Input bus sanity: a real input below the design limit, and a warning (not
    /// an error) when nothing is attached — the converter simply cannot run then.
    pub fn input_voltage(&mut self, vin_mv: u32, vin_is_measured: bool) {
        if !vin_is_measured {
            self.tally(Verdict::Warn);
            defmt::warn!(
                "preflight: [warn] input voltage cannot be measured (no input monitor and no ADC fallback) - the converter cannot start until an input is attached"
            );
            return;
        }
        let v = if vin_mv > board::VIN_MAX_MV {
            Verdict::Fail
        } else if vin_mv < 3_000 {
            Verdict::Warn
        } else {
            Verdict::Pass
        };
        self.tally(v);
        match v {
            Verdict::Pass => defmt::info!(
                "preflight: [ ok ] input voltage {} mV (design max {} mV)",
                vin_mv,
                board::VIN_MAX_MV
            ),
            Verdict::Warn => defmt::warn!(
                "preflight: [warn] input voltage is only {} mV - attach the XT90/DC feed or a USB-C source; an enable will fail to reach the setpoint",
                vin_mv
            ),
            Verdict::Fail => defmt::error!(
                "preflight: [FAIL] input voltage {} mV exceeds the {} mV design maximum - DO NOT enable; remove the input or fix the source first",
                vin_mv,
                board::VIN_MAX_MV
            ),
            Verdict::Skip => {}
        }
    }

    /// The output node should be quiet with the stage parked. Anything left over
    /// is stale charge from a previous session or a back-feed; the sequencer
    /// already waits it out, so this is informational.
    pub fn output_parked(&mut self, vout_mv: u32) {
        let v = if vout_mv <= 5_000 {
            Verdict::Pass
        } else {
            Verdict::Warn
        };
        self.tally(v);
        match v {
            Verdict::Pass => defmt::info!(
                "preflight: [ ok ] output node parked at {} mV with the stage off",
                vout_mv
            ),
            _ => defmt::warn!(
                "preflight: [warn] output node holds {} mV with the stage off (stale charge or an external source). The sequencer will hold LOADEN low and wait for it to bleed before ramping (or latch OUT TOO HIGH if it never does)",
                vout_mv
            ),
        }
        // Note the parked-DAC offset explicitly: with the chip disabled and the
        // CV DAC parked at 3.3 V, R36/R19 inject a little current into the output
        // node, which settles around 1.3 V through the sense divider. Seeing ~1 V
        // there is normal, not a bleed fault.
        if (300..3_000).contains(&vout_mv) {
            defmt::info!(
                "preflight: (the {} mV is the parked CV DAC feeding R36/R19 into the unloaded output node - expected with the stage off, not a fault)",
                vout_mv
            );
        }
    }

    /// Display presence: not a safety item, but without it the UI cannot show
    /// any fault or setpoint.
    pub fn display(&mut self, present: bool) {
        self.tally(if present { Verdict::Pass } else { Verdict::Warn });
        if present {
            defmt::info!("preflight: [ ok ] SSD1306 display answering on the UI bus");
        } else {
            defmt::warn!(
                "preflight: [warn] no SSD1306 on the UI bus - the UI is unavailable (no setpoint, no fault display). Check J5, the R88/R89 pull-ups and the module orientation"
            );
        }
    }

    /// `BOOT0` option bytes, needed for the USB-DFU reflash path on rev3.
    pub fn option_bytes(&mut self, opt: Option<(bool, bool)>) {
        match opt {
            Some((n_swboot0, n_boot1)) if n_swboot0 && n_boot1 => {
                self.tally(Verdict::Pass);
                defmt::info!(
                    "preflight: [ ok ] option bytes: nSWBOOT0=1 nBOOT1=1 (BOOT0 follows PB8 - USB-DFU strap works)"
                );
            }
            Some((n_swboot0, n_boot1)) => {
                self.tally(Verdict::Warn);
                defmt::warn!(
                    "preflight: [warn] option bytes not set for the BOOT0 strap: nSWBOOT0={} nBOOT1={} (need 1/1, FLASH_OPTR 0xFFEFF8AA). USB-DFU entry will not hold; program the option bytes once via SWD",
                    n_swboot0 as u8,
                    n_boot1 as u8
                );
            }
            None => {
                self.tally(Verdict::Warn);
                defmt::warn!(
                    "preflight: [warn] option bytes could not be read - check FLASH_OPTR (0xFFEFF8AA) manually with STM32CubeProgrammer if USB-DFU is needed"
                );
            }
        }
    }

    // -----------------------------------------------------------------
    // Summary
    // -----------------------------------------------------------------

    /// Print the tally and the verdict. Returns true when the converter may be
    /// enabled (see [`Self::may_enable`]).
    pub fn finish(&self) -> bool {
        let ok = self.may_enable();
        let passed = self.checks - self.warned - self.failed - self.skipped;
        if ok && self.failed == 0 {
            defmt::info!(
                "preflight: PASS - {}/{} checks, {} not applicable, {} warning(s); converter enable ARMED",
                passed,
                self.checks,
                self.skipped,
                self.warned
            );
        } else if ok {
            defmt::warn!(
                "preflight: {} FAILURE(s) but board::PREFLIGHT_BLOCK_ENABLE = false - enabling anyway (bench override), {}/{} other checks passed",
                self.failed,
                passed,
                self.checks
            );
        } else {
            defmt::error!(
                "preflight: FAIL - {}/{} checks failed, {} warning(s). Converter enable BLOCKED until the [FAIL] items above are fixed (override only for a supervised bench run: board::PREFLIGHT_BLOCK_ENABLE = false)",
                self.failed,
                self.checks,
                self.warned
            );
        }
        ok
    }
}

impl Default for Preflight {
    fn default() -> Self {
        Self::new(false)
    }
}
