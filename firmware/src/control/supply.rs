//! Output-stage supervisor: fault checks, the rev3 three-pin power-up sequence,
//! and CV/CC DAC updates. [`SupplyController::tick`] runs every
//! [`crate::board::SUPPLY_TICK_MS`] from the main loop while holding the
//! `APP_STATE` lock.
//!
//! # Power-up ordering
//!
//! rev3 replaced the single `Conv-Disable` line with three pins, and the order is
//! safety-critical. [`crate::control::sequencer`] owns the decision logic (which
//! stage we are in, and what each pin should be); this file owns the *application*
//! order within one tick, which is why the DACs are programmed from the stage
//! **before** the GPIOs move:
//!
//! 1. latch faults;
//! 2. compute `want_on`;
//! 3. step the sequencer;
//! 4. program CV/CC from the resulting stage — the CC DAC must already be at or
//!    above the cold-start floor when `CNV-EN-LOADEN` is raised, or the LT8390A
//!    never leaves its `UP/PRE` state;
//! 5. drive the three power-path pins;
//! 6. latch any fault the sequencer raised.
//!
//! # Diagnostics
//!
//! The controller emits an edge-triggered line whenever the mode, stage, enable
//! state, latched fault, or clamped current limit changes, plus one consolidated
//! snapshot per [`board::SUPPLY_LOG_MS`] split into three greppable lines:
//!
//! * `supply[ctl]:` — mode/enable/fault, setpoints, CV & CC DAC codes, and what
//!   those codes actually command (mapped Vout, CTRL voltage, CC estimate).
//! * `supply[lim]:` — the current-limit breakdown (`i_cap`, input `p_cap`, the
//!   power-derived `i_power_cap`, the winning `i_limit`) and the two flags that
//!   matter: `power_clamped` and `cc_active`.
//! * `supply[meas]:` — measured Vout/Iout (and the error vs setpoint), input
//!   telemetry, and the *nominal* vs *implied* CV DAC pin voltage.
//!
//! `dac_nom` vs `dac_impl` is the deliberate setpoint-accuracy probe: `dac_nom`
//! is `VREF·code/4095`, while `dac_impl` is what the measured output says the
//! DAC pin must be (assuming FB = 1 V and the nominal R19/R20/R36). If they
//! disagree by tens of mV, the DAC pin is not at the voltage the map assumes —
//! a DAC/reference problem, not a firmware map problem.

use embassy_stm32::dac::{DacCh1, Value};
use embassy_stm32::mode::Blocking;
use embassy_stm32::peripherals::{DAC1, DAC2};
use embassy_time::Instant;

use crate::board;
use crate::board::CcRange;
use crate::control::dac_cc::CcDac;
use crate::control::dac_cv::CvDac;
use crate::control::sequencer::{Inputs, Sequencer, Stage};
use crate::hal::converter_enable::ConverterEnable;
use crate::hal::high_gain::HighGain;
use crate::hal::load_enable::LoadEnable;
use crate::hal::output_switch::OutputSwitch;
use crate::state::{Fault, SupplyMode, SweepPhase};

/// Owns the CV and CC DAC state machines, the rev3 power-up sequencer, and the
/// fault policy.
pub struct SupplyController {
    cv: CvDac,
    cc: CcDac,
    seq: Sequencer,
    /// True when the boot-time board detect confirmed the rev3 power path
    /// (see `main.rs`). While false, `want_on` is forced low, so the sequencer
    /// never leaves [`Stage::Off`] and both DACs stay parked — a rev2 board
    /// (whose converter-disable line the firmware does not drive, and whose
    /// load switch is chip-driven `LOADTG`) can never be blind-enabled into a
    /// load by a stray button press.
    rev3_power_path: bool,
    /// True when the boot self-test (`src/preflight.rs`) reported no `[FAIL]`.
    /// While false the converter enable is refused and `Fault::PreflightFailed`
    /// is latched, so an enable attempt is visibly refused rather than ignored.
    preflight_ok: bool,
    /// Edge state for the preflight refusal log.
    preflight_refused: bool,
    /// Edge state for the single refusal log while the gate blocks an enable.
    gate_refused: bool,
    /// When the output-current chain first looked implausible. The watchdog
    /// latches [`Fault::SenseOpen`] only after
    /// [`crate::board::SENSE_OPEN_CONFIRM_MS`] of a persistent mismatch, so a
    /// load's soft-start or an INA228 sample-latency gap cannot trip it.
    sense_open_since: Option<Instant>,
    /// Last tick the implausible condition was actually seen.
    ///
    /// The confirmation window is only restarted once the condition has been
    /// absent for a *full* [`crate::board::SENSE_OPEN_CONFIRM_MS`]: the input
    /// monitor is polled and averaged on the PD bus, and its reading can drop out
    /// for a second at a time (the bench log shows `pin` alternating between
    /// ~46 W and ~0.4 W), so a single quiet tick must not reset the timer.
    sense_last_suspect: Option<Instant>,
    // -----------------------------------------------------------------
    // CV metrology servo (see board::CV_SERVO_* and
    // analysis/CV_CALIBRATION_PLAN.md phase 2)
    // -----------------------------------------------------------------
    /// Accumulated CV DAC trim, in codes. Sign convention: `target =
    /// map_code − trim`, so a *positive* trim raises the output (it was
    /// measured low). Bounded to ±[`board::CV_SERVO_MAX_TRIM_LSB`].
    cv_trim_lsb: i16,
    /// When the servo last changed the trim (rate limit).
    cv_trim_last_step: Instant,
    /// Setpoint the trim was last valid for; a knob turn invalidates it.
    cv_trim_vset: u32,
    /// CV DAC target computed last tick, to detect slew-settled.
    cv_last_target: u16,
    /// True while the servo is actively stepping (for the `settled` edge log).
    cv_servo_stepping: bool,
    /// Steps since the last servo log line (throttle: first + every 8th).
    cv_servo_step_count: u32,
    /// Edge latch for the one-time trim-clamp warning.
    cv_trim_clamp_logged: bool,
    /// Last (range, setpoint-wants) pair used for the deferred-range-change warning,
    /// so the log only fires on an edge instead of every tick.
    last_range_deferred: bool,
    /// Timestamp of the last consolidated `supply[...]` snapshot.
    last_log: Instant,
    // Edge-detection state for the change-triggered lines.
    last_enabled: bool,
    last_mode: SupplyMode,
    last_fault: Fault,
    last_stage: Stage,
    last_power_clamped: bool,
    last_cc_active: bool,
    last_cc_below_floor: bool,
    last_sagging: bool,
    last_deep_buck: bool,
    last_startup_clamped: bool,
}

impl SupplyController {
    /// Build with both enable gates open — the normal case on a healthy board.
    pub fn new() -> Self {
        Self::with_gates(true, true)
    }

    /// Build with explicit gate verdicts: `rev3_power_path` from the board
    /// detect and `preflight_ok` from the boot self-test. Either being `false`
    /// parks the whole output path and refuses enables; see the field docs.
    pub fn with_gates(rev3_power_path: bool, preflight_ok: bool) -> Self {
        Self {
            cv: CvDac::new(),
            cc: CcDac::new(),
            seq: Sequencer::new(),
            rev3_power_path,
            preflight_ok,
            preflight_refused: false,
            gate_refused: false,
            sense_open_since: None,
            sense_last_suspect: None,
            cv_trim_lsb: 0,
            cv_trim_last_step: Instant::now(),
            cv_trim_vset: 0,
            cv_last_target: board::DAC_MAX_CODE,
            cv_servo_stepping: false,
            cv_servo_step_count: 0,
            cv_trim_clamp_logged: false,
            last_range_deferred: false,
            last_log: Instant::now(),
            last_enabled: false,
            last_mode: SupplyMode::Off,
            last_fault: Fault::None,
            last_stage: Stage::Off,
            last_power_clamped: false,
            last_cc_active: false,
            last_cc_below_floor: false,
            last_sagging: false,
            last_deep_buck: false,
            last_startup_clamped: false,
        }
    }

    fn mode_str(mode: SupplyMode) -> &'static str {
        match mode {
            SupplyMode::Off => "OFF",
            SupplyMode::Cv => "CV",
            SupplyMode::Cc => "CC",
        }
    }

    fn fault_str(fault: Fault) -> &'static str {
        match fault {
            Fault::None => "none",
            Fault::OverCurrent => "OVERCURRENT",
            Fault::OverVoltage => "OVERVOLTAGE",
            Fault::OverPower => "OVERPOWER",
            Fault::OverTemp => "OVERTEMP",
            Fault::InputOverCurrent => "IN_OCP",
            Fault::InputOverVoltage => "IN_OVP",
            Fault::OutputNotReached => "OUTPUT_NOT_REACHED",
            Fault::OutputSwitchFailed => "OUTPUT_SWITCH_FAILED",
            Fault::OutputAboveSetpoint => "OUTPUT_ABOVE_SETPOINT",
            Fault::PreflightFailed => "PREFLIGHT_FAILED",
            Fault::SenseOpen => "SENSE_OPEN",
        }
    }

    /// One control step: latch faults, run the three-pin power-up sequence, and
    /// push new DAC codes. Any latched fault forces the output off and zeroes both
    /// DACs; the fault is cleared from the UI, not here.
    ///
    /// The power-path handles are separate types on purpose: `en` and `load_en`
    /// are both active-high inputs to the same LT8390A, and swapping them would
    /// silently invert the sequence — exactly the class of bug a pair of bare
    /// `bool`s would invite.
    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        app: &mut crate::state::AppState,
        dac_cv: &mut DacCh1<'_, DAC1, Blocking>,
        dac_cc: &mut DacCh1<'_, DAC2, Blocking>,
        en: &mut ConverterEnable,
        load_en: &mut LoadEnable,
        pwr_g: &mut OutputSwitch,
        high_gain: &mut HighGain,
    ) {
        let tele = app.telemetry;

        // Output-side supervision is judged only where its signals mean
        // something:
        //
        // * **Current and power** come from the LT8390A ISMON pin, which senses
        //   `ISP/ISN` across the output-return shunt R96, plus the output INA228
        //   (U12) on the same taps. `Q5` is in the **return** path, so while it
        //   is open the return node floats up to `VOUT` through any attached load
        //   and ISMON is meaningless. The checks are therefore gated on the load
        //   path actually being closed (`SwitchOn`/`Running`), which is also the
        //   only time a limit violation can deliver current anywhere. The
        //   sequencer additionally keeps `PWR_G` closed whenever the output is
        //   above `board::OUTPUT_SWITCH_SAFE_VOUT_MV` (rev3 safety net).
        // * **Overvoltage** is the converter-side node (`Vsense_REG_OUT`), real
        //   whenever the user has the output enabled, so it is judged from the
        //   moment of enable onward.
        // * **Temperature** and the **input-side** INA228 backstop are always
        //   valid (the NTCs sit on the always-on 3V3 rail, and the input checks
        //   already require `ina_ok`).
        //
        // A fault is latched on *transition* only, so a condition that persists
        // logs once instead of every 1 ms tick (a floating ISMON used to fill
        // RTT at 1 kHz).
        let out_current_valid = matches!(app.supply.stage, Stage::SwitchOn | Stage::Running);
        let out_voltage_valid = app.supply.enabled;
        let ov_threshold = crate::board::VOUT_MAX_MV * 105 / 100;

        // Best available output-current magnitude. The two paths are independent
        // consumers of the R96 taps: ISMON goes through U7/U14, U12 taps the
        // shunt directly. Taking the larger keeps the over-current/over-power
        // backstops alive when either one goes blind (rev3 bring-up: a dead range
        // mux left ISMON at 0 A while the stage delivered ~45 W into a load).
        let i_u12_ma = if tele.out_ina.ok {
            tele.out_ina.current_ma.unsigned_abs()
        } else {
            0
        };
        let i_best_ma = tele.iout_ma.max(i_u12_ma);
        let pout_best_mw = (tele.vout_mv as u64 * i_best_ma as u64 / 1_000) as u32;

        // Sense-chain plausibility (rev3 safety net, see `board::SENSE_OPEN_*`).
        //
        // Two ways the output-current chain can lie:
        //   * the converter is delivering meaningful power (input power, from a
        //     different shunt) yet neither current path sees it — open taps /
        //     shorted C71, or both readers dead;
        //   * U12 sees the load current but ISMON does not — the U7 mux / U14 /
        //     ISMON path is dead while the taps are fine.
        //
        // Either way the hardware CC loop (which reads ISP/ISN through the same
        // mux) is no longer limiting anything, so this latches a fault rather
        // than warning: a 30 V / 5 A stage with no current sense must not run.
        let expected_iout_ma = if tele.ina_ok && tele.vout_mv > 0 && tele.pin_mw > 0 {
            (tele.pin_mw as u64 * 1_000 / tele.vout_mv as u64) as u32
        } else {
            0
        };
        let blind_by_power = tele.ina_ok
            && tele.pin_mw > board::SENSE_OPEN_MIN_PIN_MW
            && (i_best_ma as u64) * 100
                < expected_iout_ma as u64 * board::SENSE_OPEN_RATIO_PCT as u64;
        let ismon_dead = tele.out_ina.ok
            && i_u12_ma >= board::SENSE_OPEN_U12_MIN_MA
            && (tele.iout_ma as u64) * 4 < i_u12_ma as u64;
        let sense_suspect = out_current_valid && (blind_by_power || ismon_dead);
        let mut sense_confirmed = false;
        if sense_suspect {
            let now = Instant::now();
            if self.sense_open_since.is_none() {
                self.sense_open_since = Some(now);
                defmt::warn!(
                    "sense: output-current chain implausible - ISMON={} mA, U12={} mA, but pin={} mW at vout={} mV (expected ~{} mA from the input monitor); vin={} mV iin={} mA. Confirming for {} ms before latching SENSE OPEN",
                    tele.iout_ma,
                    i_u12_ma,
                    tele.pin_mw,
                    tele.vout_mv,
                    expected_iout_ma,
                    tele.vin_mv,
                    tele.iin_ma,
                    board::SENSE_OPEN_CONFIRM_MS
                );
            }
            self.sense_last_suspect = Some(now);
            sense_confirmed = now
                .duration_since(self.sense_open_since.unwrap_or(now))
                .as_millis()
                >= board::SENSE_OPEN_CONFIRM_MS;
        } else if self.sense_last_suspect.is_none_or(|t| {
            Instant::now().duration_since(t).as_millis() >= board::SENSE_OPEN_CONFIRM_MS
        }) {
            // Only restart the window once the condition has been gone for a full
            // confirm period (see `sense_last_suspect`).
            self.sense_open_since = None;
            self.sense_last_suspect = None;
        }

        let mut detected = Fault::None;
        if out_current_valid && i_best_ma > crate::board::IOUT_MAX_MA {
            detected = Fault::OverCurrent;
        } else if out_voltage_valid && tele.vout_mv > ov_threshold {
            detected = Fault::OverVoltage;
        } else if out_current_valid && pout_best_mw > crate::board::POWER_MAX_MW {
            detected = Fault::OverPower;
        } else if crate::board::NTC_SENSE_ENABLE
            && (tele.temp_conv_c > crate::board::NTC_OVERTEMP_C
                || tele.temp_input_c > crate::board::NTC_OVERTEMP_C)
        {
            detected = Fault::OverTemp;
        } else if tele.ina_ok {
            // Input-side backstop from the INA228. The current limit follows the
            // negotiated PD cap (falling back to the design max on XT90 input)
            // so it does not false-trip a high-current, low-voltage DC feed.
            let iin_limit = (app.supply.input_current_cap_ma as i32
                + crate::board::IIN_MARGIN_MA)
                .min(crate::board::IIN_MAX_MA);
            if tele.iin_ma > iin_limit {
                detected = Fault::InputOverCurrent;
            } else if tele.vin_mv > crate::board::VIN_MAX_MV {
                detected = Fault::InputOverVoltage;
            }
        }
        // The sense-chain watchdog is the *lowest* priority: it only speaks up
        // when no genuine over-current/over-voltage/over-temperature/input
        // condition explains what the monitors are seeing (an input over-current,
        // for instance, also makes the output current look implausibly small).
        if detected == Fault::None && sense_confirmed {
            detected = Fault::SenseOpen;
        }
        if detected != Fault::None && detected != app.supply.fault {
            app.supply.fault = detected;
            match detected {
                Fault::OverCurrent => defmt::info!(
                    "Fault::OverCurrent: iout={} mA (ISMON {} / U12 {}) > {} mA",
                    i_best_ma,
                    tele.iout_ma,
                    i_u12_ma,
                    crate::board::IOUT_MAX_MA
                ),
                Fault::OverVoltage => defmt::info!(
                    "Fault::OverVoltage: {} mV > {} mV threshold",
                    tele.vout_mv,
                    ov_threshold
                ),
                Fault::OverPower => defmt::info!(
                    "Fault::OverPower: pout={} mW (ISMON path {}) > {} mW",
                    pout_best_mw,
                    tele.pout_mw,
                    crate::board::POWER_MAX_MW
                ),
                Fault::OverTemp => defmt::info!(
                    "Fault::OverTemp! conv_c: {}, input_c: {}, limit: {}",
                    tele.temp_conv_c,
                    tele.temp_input_c,
                    crate::board::NTC_OVERTEMP_C
                ),
                Fault::InputOverCurrent => defmt::info!(
                    "Fault::InputOverCurrent: {} mA > {} mA",
                    tele.iin_ma,
                    (app.supply.input_current_cap_ma as i32 + crate::board::IIN_MARGIN_MA)
                        .min(crate::board::IIN_MAX_MA)
                ),
                Fault::InputOverVoltage => defmt::info!(
                    "Fault::InputOverVoltage: {} mV > {} mV",
                    tele.vin_mv,
                    crate::board::VIN_MAX_MV
                ),
                Fault::SenseOpen => defmt::error!(
                    "Fault::SenseOpen: the output-current chain is blind - ISMON={} mA, U12={} mA, but pin={} mW at vout={} mV (expected ~{} mA). The CC loop AND the software over-current trip cannot see the load current, so the stage is switched off. Check the U7 range mux (S1A/S2A pins 16/3 on SENSE_P/SENSE_N), R83/R98, C71 and the R96 taps before re-enabling.",
                    tele.iout_ma,
                    i_u12_ma,
                    tele.pin_mw,
                    tele.vout_mv,
                    expected_iout_ma
                ),
                _ => {}
            }
        }

        if app.supply.fault != Fault::None {
            app.supply.enabled = false;
        }

        // Enable gates. On anything the boot detect did not confirm as rev3, or
        // where the boot self-test reported a [FAIL], the output stage is never
        // commanded on: the DACs stay parked (0 V CV target, CTRL at latch-off)
        // and the sequencer sits in `Off`, so no stray button press can ramp a
        // board the firmware cannot supervise.
        if app.supply.enabled {
            if !self.rev3_power_path {
                if !self.gate_refused {
                    self.gate_refused = true;
                    defmt::warn!(
                        "supply: output enable REFUSED - rev3 power path not detected at boot (rev2 board, or rev3 where neither the PD bus nor the OLED answered); sequencer is parked and the DACs stay at 0"
                    );
                }
                app.supply.enabled = false;
            } else if !self.preflight_ok {
                if !self.preflight_refused {
                    self.preflight_refused = true;
                    defmt::warn!(
                        "supply: output enable REFUSED - the boot self-test reported [FAIL] item(s); see the `preflight:` lines above. Fix the hardware, or accept it consciously with board::PREFLIGHT_BLOCK_ENABLE = false"
                    );
                }
                app.supply.enabled = false;
                app.supply.fault = Fault::PreflightFailed;
            }
        } else {
            self.gate_refused = false;
            self.preflight_refused = false;
        }

        let want_on = app.supply.enabled && app.supply.fault == Fault::None;

        // Current-limit budget. The output current ceiling is the user setpoint
        // (bounded by the design max) further limited by the *power* available
        // at the present output voltage. The PD contract's current is
        // deliberately NOT applied here: that is an input-bus (VIN-side) limit,
        // and the output stage is a converter, so only the contract's power
        // (`VIN x I`) is meaningful at the output. Input over-current is
        // enforced separately by the INA228 backstop above.
        // The range the setpoint asks for (overridden by
        // `board::FORCE_CC_RANGE`). Resolved before the budget because a forced
        // range also caps the settable current, and the ceiling below must not
        // claim a limit the CC DAC will silently clamp.
        let wanted_range = CcRange::for_setpoint(app.supply.i_set_ma);
        let i_cap = app
            .supply
            .i_set_ma
            .min(crate::board::IOUT_MAX_MA)
            .min(wanted_range.set_max_ma());
        let p_cap = app
            .supply
            .input_power_cap_mw
            .min(crate::board::POWER_MAX_MW);
        let i_power_cap = if tele.vout_mv > 0 {
            (p_cap * 1000) / tele.vout_mv
        } else {
            i_cap
        };
        let i_limit = i_cap.min(i_power_cap);
        // True when the *power* cap, not the user setpoint, is the binding limit.
        // This is the first thing to check when the converter current-limits well
        // below the number on the CC screen.
        let power_clamped = i_power_cap < i_cap;

        // Step the three-pin power-up sequence. It reads the *fast* ADC output
        // voltage, not the INA228's, so a 1 ms tick can act on it.
        //
        // `cc_limited` lets the sequencer accept a CC-limited output as a
        // completed ramp (the load switch is closed before the ramp on rev3, so a
        // load that draws the limit holds the output below the setpoint on
        // purpose). `load_current_ma` is the best available current, used only to
        // decide whether releasing the load switch during a disable is safe.
        let cc_limited =
            i_limit > 0 && (i_best_ma as u64) * 100 >= (i_limit as u64) * 95;
        let out = self.seq.step(
            Instant::now(),
            Inputs {
                want_on,
                v_set_mv: app.supply.v_set_mv,
                vout_adc_mv: tele.vout_mv,
                // Previous tick's slew verdict (the DAC section below runs after the
                // sequencer step); a 1 ms lag is irrelevant to a ramp budget.
                cv_slew_settled: self.cv.code == self.cv_last_target,
                out_ina_ok: tele.out_ina.ok,
                // U12 is only expected where the PD bus actually lives (USB-C VBUS
                // present); an XT90/DC feed has no PD bus by design.
                out_ina_expected: app.pd_bus_alive(),
                out_ina_current_ma: tele.out_ina.current_ma,
                load_current_ma: i_best_ma,
                cc_limited,
            },
        );
        if let Some(f) = out.fault {
            if f == Fault::OutputSwitchFailed {
                // The sequencer's own line says *why* the close was not
                // confirmed; this one adds the hardware context that separates
                // the two indistinguishable causes:
                //   * firmware/GPIO: was `PWR_G` actually driven and does the pad
                //     read back high?
                //   * power path: is anything being delivered at all (`iin`,
                //     `pin`)? With a load attached and `PWR_G` high, a dead
                //     `Q12`/`Q7`/`R58`/`Q5` chain shows 0 mA everywhere, while a
                //     bench run with no load shows the same thing by design.
                let (pb1, pb2, pb3, pb6) = board::power_path_pin_levels();
                defmt::error!(
                    "output: switch context: PB2 PWR_G reads {} (1 = load switch commanded closed), PB1 disable={} PB3 LOADEN={} PB6 x10={}; converter-side vout={} mV vs U12 bus={} mV; iout ADC={} mA U12={} mA; vin={} mV iin={} mA pin={} mW. If a load was attached and `iin`/`pin` are near zero, this is the Q12/Q7/R58/Q5 gate chain; closure cannot be proven by current with nothing attached, so leave board::OUTPUT_SWITCH_REQUIRE_LOAD = false (its default) for a no-load bench",
                    pb2 as u8,
                    pb1 as u8,
                    pb3 as u8,
                    pb6 as u8,
                    tele.vout_mv,
                    tele.out_ina.bus_mv,
                    tele.iout_ma,
                    tele.out_ina.current_ma,
                    tele.vin_mv,
                    tele.iin_ma,
                    tele.pin_mw
                );
            }
            app.supply.fault = f;
            app.supply.enabled = false;
        }
        app.supply.stage = out.stage;

        // CC sense range. The setpoint decides which range it wants, but the pin
        // may only move while the stage is parked: a range change is a ×10 step in
        // the CC loop gain (and a ~200 mV common-mode step at the chip's ISP/ISN
        // pair), so switching it while regulating would kick the loop. The ×1
        // window (2.5–50 A) still covers every setpoint above 2.5 A, so deferring
        // is bounded and safe — only a sub-2.5 A setpoint genuinely needs ×10.
        if out.stage == Stage::Off {
            high_gain.set_range(wanted_range);
        } else if high_gain.range() != wanted_range {
            if !self.last_range_deferred {
                defmt::warn!(
                    "output: HIGH-GAIN range change deferred to the next disable: iset={} mA wants {:?} but stage={} is running {:?}",
                    app.supply.i_set_ma,
                    wanted_range,
                    out.stage.label(),
                    high_gain.range()
                );
            }
            self.last_range_deferred = true;
        }
        if high_gain.range() == wanted_range {
            self.last_range_deferred = false;
        }
        let range = high_gain.range();
        app.supply.cc_range = range;

        let enabled = !out.conv_disable;

        if !enabled {
            // Inverted CV DAC: park at full-scale code = minimum output. The CC
            // DAC at code 0 holds CTRL below the LT8390A latch-off (and the
            // enable line is off), so the stage is genuinely stopped.
            self.cv.code = board::DAC_MAX_CODE;
            dac_cv.set(Value::Bit12Right(self.cv.code));
            self.cc.code = 0;
            dac_cc.set(Value::Bit12Right(0));
            app.supply.dac_cv_code = self.cv.code;
            app.supply.dac_cc_code = 0;
            self.last_cc_below_floor = false;
            self.reset_cv_trim(app.supply.v_set_mv);
        } else {
            match app.supply.mode {
                SupplyMode::Cv | SupplyMode::Cc => {
                    // CV metrology servo first: it decides this tick's trim from
                    // the *settled* state, then the trim folds into the slew
                    // target below (it never steps the DAC around the limiter).
                    self.cv_servo_tick(app, out.stage);

                    // Open-loop CV from the setpoint via the (inverted) map,
                    // with the DAC code slewed at most
                    // board::CV_SLEW_MAX_LSB_PER_TICK counts per tick so
                    // setpoint changes step gently (avoids ringing / FET
                    // heating). The servo trim is a code-domain correction on
                    // top of the map: `target = map − trim`, so a positive
                    // trim raises the output.
                    let map_code = self.cv.mv_to_code(app.supply.v_set_mv);
                    let target = (map_code as i32 - self.cv_trim_lsb as i32)
                        .clamp(0, board::DAC_MAX_CODE as i32) as u16;
                    self.cv_last_target = target;
                    self.cv.slew(target);
                    dac_cv.set(Value::Bit12Right(self.cv.code));
                    app.supply.dac_cv_code = self.cv.code;

                    // CC limit. In CC mode the *user* limit is used; in CV mode
                    // the same limit is the safety ceiling. Both are clamped by
                    // the power budget above.
                    let cc_target = match app.supply.mode {
                        SupplyMode::Cc => app.supply.i_set_ma,
                        _ => i_cap,
                    };
                    let mut cc_req = cc_target.min(i_limit);
                    // Every stage except `Running` is a start-up stage: the
                    // LT8390A does not leave its `UP/PRE` state until CTRL is
                    // above its rising latch-off (~0.325 V), so the command is
                    // floored at the datasheet cold-start current for as long as
                    // the sequencer asks. This is a deliberate, logged overshoot
                    // of the user setpoint, bounded by the ramp window.
                    let startup_clamped =
                        out.cc_startup_clamp && cc_req < range.start_min_ma();
                    if startup_clamped {
                        cc_req = range.start_min_ma();
                    }
                    if startup_clamped && !self.last_startup_clamped {
                        defmt::info!(
                            "output: start-up CC floored at the {} mA cold-start value (CTRL >= {} mV, range {:?}) for stage={} so LOADEN can take effect",
                            range.start_min_ma(),
                            board::LT8390_CTRL_START_MV,
                            range,
                            out.stage.label()
                        );
                    }
                    self.last_startup_clamped = startup_clamped;

                    // The setpoint is the current the user expects to be drawn;
                    // `set_current` cancels the bench CC offset and clamps to the
                    // settable range. Warn only when the *power* budget forces
                    // the limit below that range, so it is clear the PD contract
                    // rather than the CC setting is binding.
                    let cc_eff = CcDac::clamp_current(cc_req, range);
                    let below_floor = cc_req < range.set_min_ma();
                    if below_floor && !self.last_cc_below_floor {
                        defmt::warn!(
                            "supply: power budget limits CC to {} mA, below the {} mA settable floor for range {:?} (R_eff={} mOhm); limiting at {} mA",
                            cc_req,
                            range.set_min_ma(),
                            range,
                            range.shunt_mohm(),
                            cc_eff
                        );
                    }
                    self.last_cc_below_floor = below_floor;
                    self.cc.set_current(cc_eff, range);
                    dac_cc.set(Value::Bit12Right(self.cc.code));
                    app.supply.dac_cc_code = self.cc.code;

                    // NOTE: CC mode deliberately programs the CV DAC from
                    // `v_set_mv`, exactly like CV mode. The LT8390A diode-ORs its
                    // FB error amp (regulate FB to 1 V) and its ISP/ISN current
                    // error amp (regulate to the CTRL threshold) into VC, so
                    // whichever asks for the *lower* VC wins. Parking the CV DAC
                    // at minimum output — the old CC-mode behaviour — made the
                    // voltage loop win and collapsed the output instead of
                    // current-limiting it. With the normal voltage ceiling
                    // command, the chip transitions CV↔CC on its own.

                    // The trip only means something with the load path closed:
                    // ISMON senses the return shunt behind the open Q5 until
                    // `SwitchOn`, and floats then (see the fault-gate note at
                    // the top of `tick`). Use the best of ISMON/U12: if the mux
                    // path is dead this trip is the only over-current backstop
                    // left, and U12 still sees the load.
                    if app.supply.mode == SupplyMode::Cv
                        && matches!(out.stage, Stage::SwitchOn | Stage::Running)
                        && i_best_ma > i_limit * board::IOUT_TRIP_MARGIN_PCT / 100
                    {
                        defmt::warn!(
                            "Fault::OverCurrent SOFTWARE trip: iout={} mA (ISMON {} / U12 {}) > i_limit={} mA ({}% margin) vout={} mV vset={} mV cc_code={} cc_est={} mA isense_biased_high?",
                            i_best_ma,
                            tele.iout_ma,
                            i_u12_ma,
                            i_limit,
                            board::IOUT_TRIP_MARGIN_PCT,
                            tele.vout_mv,
                            app.supply.v_set_mv,
                            self.cc.code,
                            CcDac::current_ma_for_code(self.cc.code, range)
                        );
                        app.supply.fault = Fault::OverCurrent;
                    }
                }
                SupplyMode::Off => {
                    self.cv.code = board::DAC_MAX_CODE;
                    dac_cv.set(Value::Bit12Right(self.cv.code));
                    self.cc.code = 0;
                    dac_cc.set(Value::Bit12Right(0));
                    app.supply.dac_cv_code = self.cv.code;
                    app.supply.dac_cc_code = 0;
                    self.last_cc_below_floor = false;
                    self.last_startup_clamped = false;
                    self.reset_cv_trim(app.supply.v_set_mv);
                }
            }
        }

        // Apply the power-path pins. Deliberately *after* the DACs: on the way up
        // the CC command must already be legal when `CNV-EN-LOADEN` rises, and on
        // the way down the DACs are parked before the chip is shut off.
        en.set_enabled(!out.conv_disable);
        load_en.set_enabled(out.load_enable);
        pwr_g.set_closed(out.pwr_g);

        // A converter that is enabled, has a non-trivial limit, is sitting at
        // (or above) that limit, and whose output has fallen away from the
        // setpoint is being current-limited by the hardware CC loop.
        // A converter that is enabled, has a non-trivial limit, is sitting at
        // (or above) that limit, and whose output has fallen away from the
        // setpoint is being current-limited by the hardware CC loop. Gated on
        // the load path being closed: the return shunt carries no meaningful
        // current until `PWR_G` does.
        let cc_active = enabled
            && matches!(out.stage, Stage::SwitchOn | Stage::Running)
            && i_limit > 0
            && i_best_ma * 100 >= i_limit * 90
            && tele.vout_mv < app.supply.v_set_mv * 95 / 100;
        app.supply.cc_active = cc_active;

        // Out of regulation: enabled, has a setpoint, but the output is well
        // below it. This is *not* the same as `cc_active` — it is what a
        // converter that cannot deliver the requested power looks like, and it
        // is the case the bench run hit (collapse at ~9 A while the CC limit was
        // programmed to 16 A).
        //
        // Gated on the stages where the output is *supposed* to be at the
        // setpoint: during `Precharge`/`Ramping` a low output is the ramp doing
        // its job, and during `Disabling` it is the node bleeding down, so
        // judging those produced a warning that read like a fault on every
        // enable (``OUTPUT OUT OF REGULATION ... at iout=0 mA`` mid-ramp, and
        // again after the switch opened).
        let sagging = enabled
            && matches!(out.stage, Stage::SwitchOn | Stage::Running)
            && app.supply.v_set_mv > 0
            && tele.vout_mv < app.supply.v_set_mv * board::VOUT_SAG_PCT / 100;

        self.log(
            app,
            enabled,
            out.stage,
            i_cap,
            p_cap,
            i_power_cap,
            i_limit,
            power_clamped,
            cc_active,
            sagging,
        );
    }

    // -----------------------------------------------------------------
    // CV metrology servo (Phase 2 of analysis/CV_CALIBRATION_PLAN.md)
    // -----------------------------------------------------------------

    /// Zero the servo trim and re-anchor it to `vset`.
    ///
    /// Called on every servo-invalidating event (disable, mode change,
    /// setpoint change, sweep start, fault): the static map — analytic or
    /// fitted — is always the correct *starting point* at a new setpoint, and
    /// a trim earned at one voltage would over-correct at another (a B-scale
    /// error is 3.5 LSB at 20 V but only 0.9 LSB at 5 V).
    fn reset_cv_trim(&mut self, vset_mv: u32) {
        self.cv_trim_lsb = 0;
        self.cv_servo_stepping = false;
        self.cv_servo_step_count = 0;
        self.cv_trim_clamp_logged = false;
        self.cv_trim_vset = vset_mv;
    }

    /// CV metrology servo: nudge the CV DAC target by ±1 code every
    /// [`board::CV_SERVO_PERIOD_MS`] so the *measured* output (U12, the ±0.05 %
    /// monitor on the load side of F2) matches the setpoint.
    ///
    /// This is the slow outer metrology loop around the chip's fast FB loop
    /// (`VOUT = A − B·code` in `control::dac_cv`): the fit removes the big
    /// systematic map errors, and this removes what a fit cannot — DAC INL and
    /// temperature drift — plus the load-path (bead + F2) drop, because U12's
    /// VBUS pin sits at the output terminals.
    ///
    /// It only ever *steps* under all of these, and every break resets the
    /// trim to the static map (see [`Self::reset_cv_trim`]):
    ///
    /// * steady CV: [`Stage::Running`], `mode == Cv`, not current-limiting
    ///   (`cc_active` — VOUT below setpoint is *correct* in CC, integrating
    ///   against it would wind up), not sagging, no sweep in flight;
    /// * the output INA228 is answering (its reading is the servo's only
    ///   input; without it the servo freezes on the last code, which the
    ///   static map continues to hold);
    /// * the CV slew has settled (`cv.code == cv_last_target`) — never trim
    ///   mid-ramp;
    /// * the rate limiter: one code step per period, at most 28.8 mV each.
    ///
    /// Authority is clamped to [`board::CV_SERVO_MAX_TRIM_LSB`] (±1.84 V),
    /// far below the 105 % overvoltage guard, and the step direction follows
    /// the inverted map: output measured low → trim up (code down).
    fn cv_servo_tick(&mut self, app: &crate::state::AppState, stage: Stage) {
        let tele = app.telemetry;
        let vset = app.supply.v_set_mv;

        // `cc_active` is published later in this same tick; the value read here
        // is last tick's verdict, which is perfect for a 500 ms-period servo.
        let servo_ready = board::CV_SERVO_ENABLE
            && vset > 0
            && app.supply.mode == SupplyMode::Cv
            && stage == Stage::Running
            && tele.out_ina.ok
            && !app.supply.cc_active
            && app.sweep.phase != SweepPhase::Running
            && tele.vout_mv as u64 * 100 >= vset as u64 * board::VOUT_SAG_PCT as u64
            && self.cv.code == self.cv_last_target;

        if !servo_ready || vset != self.cv_trim_vset {
            if self.cv_trim_lsb != 0 {
                defmt::info!(
                    "cv servo: trim reset to the map (trim={} lsb; setpoint moved or CV not steady)",
                    self.cv_trim_lsb
                );
            }
            self.reset_cv_trim(vset);
            return;
        }

        let now = Instant::now();
        if now.duration_since(self.cv_trim_last_step).as_millis() < board::CV_SERVO_PERIOD_MS {
            return;
        }

        let err_mv = vset as i64 - tele.out_ina.bus_mv as i64;
        if err_mv.unsigned_abs() <= board::CV_SERVO_DEADBAND_MV as u64 {
            if self.cv_servo_stepping {
                self.cv_servo_stepping = false;
                defmt::info!(
                    "cv servo: settled, err={} mV, trim={} lsb (vset={} vout_ina={})",
                    err_mv,
                    self.cv_trim_lsb,
                    vset,
                    tele.out_ina.bus_mv
                );
            }
            self.cv_servo_step_count = 0;
            self.cv_trim_clamp_logged = false;
            return;
        }

        // One code step toward the error: output low (err > 0) wants the trim
        // up, which lowers the code and raises the output on this inverted map.
        let dir: i16 = if err_mv > 0 { 1 } else { -1 };
        let wanted = self.cv_trim_lsb + dir;
        let clamped =
            wanted.clamp(-board::CV_SERVO_MAX_TRIM_LSB, board::CV_SERVO_MAX_TRIM_LSB);
        if clamped != wanted && !self.cv_trim_clamp_logged {
            self.cv_trim_clamp_logged = true;
            defmt::warn!(
                "cv servo: TRIM CLAMPED at {} lsb ({} lsb = ~{} mV of authority) - the static map is off by more than the servo can correct; run the CV calibration (analysis/cv_cal/fit.py)",
                board::CV_SERVO_MAX_TRIM_LSB,
                board::CV_SERVO_MAX_TRIM_LSB,
                board::CV_SERVO_MAX_TRIM_LSB as u32 * board::CV_CAL_B_UV_PER_LSB / 1000
            );
        }
        self.cv_trim_lsb = clamped;
        self.cv_trim_last_step = now;
        self.cv_servo_stepping = true;
        self.cv_servo_step_count = self.cv_servo_step_count.saturating_add(1);
        // Throttle the step lines: the first of a burst, then every 8th, so a
        // long settle does not flood RTT but its progress stays visible.
        if self.cv_servo_step_count == 1 || self.cv_servo_step_count.is_multiple_of(8) {
            defmt::info!(
                "cv servo: err={} mV -> trim={} lsb (vset={} vout_ina={})",
                err_mv,
                self.cv_trim_lsb,
                vset,
                tele.out_ina.bus_mv
            );
        }
    }

    /// Emit the change-triggered edge lines and the periodic consolidated
    /// snapshot. Split out so `tick` reads as control flow only.
    #[allow(clippy::too_many_arguments)]
    fn log(
        &mut self,
        app: &crate::state::AppState,
        enabled: bool,
        stage: Stage,
        i_cap: u32,
        p_cap: u32,
        i_power_cap: u32,
        i_limit: u32,
        power_clamped: bool,
        cc_active: bool,
        sagging: bool,
    ) {
        let tele = app.telemetry;
        let range = app.supply.cc_range;

        if enabled != self.last_enabled
            || app.supply.mode != self.last_mode
            || app.supply.fault != self.last_fault
            || stage != self.last_stage
        {
            defmt::info!(
                "supply: state change mode={} stage={} range={:?} en={} fault={} vout={} iout={} vset={} iset={}",
                Self::mode_str(app.supply.mode),
                stage.label(),
                range,
                enabled,
                Self::fault_str(app.supply.fault),
                tele.vout_mv,
                tele.iout_ma,
                app.supply.v_set_mv,
                app.supply.i_set_ma
            );
        }

        if power_clamped && !self.last_power_clamped {
            defmt::warn!(
                "supply: OUTPUT CURRENT LIMITED BY INPUT POWER CAP: iset={} mA clamped to i_limit={} mA (p_cap={} mW / vout={} mV = {} mA). Contract vin={} mV iin={} mA.",
                app.supply.i_set_ma,
                i_limit,
                p_cap,
                tele.vout_mv,
                i_power_cap,
                tele.vin_mv,
                tele.iin_ma
            );
        }

        if cc_active && !self.last_cc_active {
            defmt::info!(
                "supply: hardware CC transition: vout={} mV below vset={} mV at iout={} mA (i_limit={} mA, cc_code={} ctrl_mv={} cc_est={} mA)",
                tele.vout_mv,
                app.supply.v_set_mv,
                tele.iout_ma,
                i_limit,
                self.cc.code,
                CcDac::ctrl_mv_for_code(self.cc.code),
                CcDac::current_ma_for_code(self.cc.code, range)
            );
        }

        if sagging && !self.last_sagging {
            defmt::warn!(
                "supply: OUTPUT OUT OF REGULATION: vout={} mV < {}% of vset={} mV at iout={} mA; fb_impl={} mV (ref 1000, <1000 = chip cannot deliver); i_limit={} mA cc_est={} mA ctrl_mv={} dac_nom={} dac_impl={}; vin={} mV iin={} mA pin={} mW vin/vset={}%",
                tele.vout_mv,
                board::VOUT_SAG_PCT,
                app.supply.v_set_mv,
                tele.iout_ma,
                CvDac::implied_fb_mv(tele.vout_mv, self.cv.code),
                i_limit,
                CcDac::current_ma_for_code(self.cc.code, range),
                CcDac::ctrl_mv_for_code(self.cc.code),
                CvDac::nominal_dac_mv(self.cv.code),
                CvDac::implied_dac_mv(tele.vout_mv),
                tele.vin_mv,
                tele.iin_ma,
                tele.pin_mw,
                tele.vin_mv.saturating_mul(100) / app.supply.v_set_mv.max(1)
            );
        }

        // Advisory, not a fault: a high step-down ratio is the corner where this
        // board's power stage gives up (see `board::DEEP_BUCK_WARN_RATIO_PCT`).
        let vin_over_vset_pct = if app.supply.v_set_mv > 0 {
            tele.vin_mv.saturating_mul(100) / app.supply.v_set_mv
        } else {
            0
        };
        let deep_buck = enabled && vin_over_vset_pct >= board::DEEP_BUCK_WARN_RATIO_PCT;
        if deep_buck && !self.last_deep_buck {
            defmt::warn!(
                "supply: DEEP-BUCK OPERATING POINT: vin={} mV / vset={} mV = {}% >= {}%; this board loses regulation above ~70 W here — prefer a lower PD rail (Auto-tracking picks the gentlest step-down)",
                tele.vin_mv,
                app.supply.v_set_mv,
                vin_over_vset_pct,
                board::DEEP_BUCK_WARN_RATIO_PCT
            );
        }

        self.last_enabled = enabled;
        self.last_mode = app.supply.mode;
        self.last_fault = app.supply.fault;
        self.last_stage = stage;
        self.last_power_clamped = power_clamped;
        self.last_cc_active = cc_active;
        self.last_sagging = sagging;
        self.last_deep_buck = deep_buck;

        let now = Instant::now();
        let period = if sagging {
            board::SUPPLY_LOG_SAG_MS
        } else {
            board::SUPPLY_LOG_MS
        };
        if now.duration_since(self.last_log).as_millis() < period {
            return;
        }
        self.last_log = now;

        let vout_err = tele.vout_mv as i64 - app.supply.v_set_mv as i64;
        let cv_code = self.cv.code;
        defmt::info!(
            "supply[ctl]: mode={} stage={} range={:?} en={} fault={} vset={} iset={} cv_code={} map_vout={} trim={} cc_code={} ctrl_mv={} cc_est={}",
            Self::mode_str(app.supply.mode),
            stage.label(),
            range,
            enabled,
            Self::fault_str(app.supply.fault),
            app.supply.v_set_mv,
            app.supply.i_set_ma,
            cv_code,
            CvDac::code_to_vout_mv(cv_code),
            self.cv_trim_lsb,
            self.cc.code,
            CcDac::ctrl_mv_for_code(self.cc.code),
            CcDac::current_ma_for_code(self.cc.code, range)
        );
        defmt::info!(
            "supply[lim]: i_cap={} p_cap={} i_power_cap={} i_limit={} power_clamped={} cc_active={}",
            i_cap,
            p_cap,
            i_power_cap,
            i_limit,
            power_clamped,
            cc_active
        );
        defmt::info!(
            "supply[meas]: vout={} iout={} verr={} vin={} iin={} pin={} ina_out_ok={} ina_out_vout={} ina_out_iout={} dac_nom={} dac_impl={} fb_impl={}",
            tele.vout_mv,
            tele.iout_ma,
            vout_err,
            tele.vin_mv,
            tele.iin_ma,
            tele.pin_mw,
            tele.out_ina.ok,
            tele.out_ina.bus_mv,
            tele.out_ina.current_ma,
            CvDac::nominal_dac_mv(cv_code),
            CvDac::implied_dac_mv(tele.vout_mv),
            CvDac::implied_fb_mv(tele.vout_mv, cv_code)
        );
    }
}

impl Default for SupplyController {
    fn default() -> Self {
        Self::new()
    }
}
