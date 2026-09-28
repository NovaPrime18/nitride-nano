//! Output-stage supervisor: fault checks, enable gating, and CV/CC DAC updates.
//! [`SupplyController::tick`] runs every [`crate::board::SUPPLY_TICK_MS`]
//! from the main loop while holding the `APP_STATE` lock.
//!
//! # Diagnostics
//!
//! The controller emits an edge-triggered line whenever the mode, enable state,
//! latched fault, or clamped current limit changes, plus one consolidated
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
use crate::control::dac_cc::CcDac;
use crate::control::dac_cv::CvDac;
use crate::hal::converter_enable::ConverterEnable;
use crate::state::{Fault, SupplyMode};

/// Owns the CV and CC DAC state machines and applies the fault policy.
pub struct SupplyController {
    cv: CvDac,
    cc: CcDac,
    /// Timestamp of the last consolidated `supply[...]` snapshot.
    last_log: Instant,
    // Edge-detection state for the change-triggered lines.
    last_enabled: bool,
    last_mode: SupplyMode,
    last_fault: Fault,
    last_power_clamped: bool,
    last_cc_active: bool,
    last_cc_below_floor: bool,
    last_sagging: bool,
    last_deep_buck: bool,
}

impl SupplyController {
    pub fn new() -> Self {
        Self {
            cv: CvDac::new(),
            cc: CcDac::new(),
            last_log: Instant::now(),
            last_enabled: false,
            last_mode: SupplyMode::Off,
            last_fault: Fault::None,
            last_power_clamped: false,
            last_cc_active: false,
            last_cc_below_floor: false,
            last_sagging: false,
            last_deep_buck: false,
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
        }
    }

    /// One control step: latch faults, gate the converter enable line, and push
    /// new DAC codes. Any latched fault forces the output off and zeroes both
    /// DACs; the fault is cleared from the UI, not here.
    pub fn tick(
        &mut self,
        app: &mut crate::state::AppState,
        dac_cv: &mut DacCh1<'_, DAC1, Blocking>,
        dac_cc: &mut DacCh1<'_, DAC2, Blocking>,
        en: &mut ConverterEnable,
    ) {
        let tele = app.telemetry;

        // Hard overvoltage guard at 105% of the design max. The CV loop already
        // clamps setpoints to VOUT_MAX_MV, so this trips only on regulation
        // failure (runaway) — it must sit ABOVE VOUT_MAX_MV or normal
        // full-scale operation would false-trip.
        let ov_threshold = crate::board::VOUT_MAX_MV * 105 / 100;
        if tele.iout_ma > crate::board::IOUT_MAX_MA {
            app.supply.fault = Fault::OverCurrent;
            defmt::info!("Fault::OverCurrent");
        } else if tele.vout_mv > ov_threshold {
            app.supply.fault = Fault::OverVoltage;
            defmt::info!(
                "Fault::OverVoltage: {} mV > {} mV threshold",
                tele.vout_mv,
                ov_threshold
            );
        } else if tele.pout_mw > crate::board::POWER_MAX_MW {
            app.supply.fault = Fault::OverPower;
            defmt::info!("Fault::OverPower");
        } else if tele.temp_conv_c > crate::board::NTC_OVERTEMP_C
            || tele.temp_input_c > crate::board::NTC_OVERTEMP_C
        {
            app.supply.fault = Fault::OverTemp;
            defmt::info!(
                "Fault::OverTemp! conv_c: {}, input_c: {}, limit: {}",
                tele.temp_conv_c,
                tele.temp_input_c,
                crate::board::NTC_OVERTEMP_C
            );
        } else if tele.ina_ok {
            // Input-side backstop from the INA228. The current limit follows the
            // negotiated PD cap (falling back to the design max on XT90 input)
            // so it does not false-trip a high-current, low-voltage DC feed.
            let iin_limit = (app.supply.input_current_cap_ma as i32
                + crate::board::IIN_MARGIN_MA)
                .min(crate::board::IIN_MAX_MA);
            if tele.iin_ma > iin_limit {
                app.supply.fault = Fault::InputOverCurrent;
                defmt::info!(
                    "Fault::InputOverCurrent: {} mA > {} mA",
                    tele.iin_ma,
                    iin_limit
                );
            } else if tele.vin_mv > crate::board::VIN_MAX_MV {
                app.supply.fault = Fault::InputOverVoltage;
                defmt::info!(
                    "Fault::InputOverVoltage: {} mV > {} mV",
                    tele.vin_mv,
                    crate::board::VIN_MAX_MV
                );
            }
        }

        if app.supply.fault != Fault::None {
            app.supply.enabled = false;
        }

        let enabled = app.supply.enabled && app.supply.fault == Fault::None;
        en.set_enabled(enabled);

        // Current-limit budget. The output current ceiling is the user setpoint
        // (bounded by the design max) further limited by the *power* available
        // at the present output voltage. The PD contract's current is
        // deliberately NOT applied here: that is an input-bus (VIN-side) limit,
        // and the output stage is a converter, so only the contract's power
        // (`VIN x I`) is meaningful at the output. Input over-current is
        // enforced separately by the INA228 backstop above.
        let i_cap = app.supply.i_set_ma.min(crate::board::IOUT_MAX_MA);
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

        if !enabled {
            // Inverted CV DAC: park at full-scale code = minimum output. The CC
            // DAC at code 0 holds CTRL below the LT8390A latch-off (and the
            // enable line is off), so the stage is genuinely stopped.
            self.cv.code = board::DAC_MAX_CODE;
            dac_cv.set(Value::Bit12Right(self.cv.code));
            self.cc.code = 0;
            dac_cc.set(Value::Bit12Right(0));
            self.last_cc_below_floor = false;
        } else {
            match app.supply.mode {
                SupplyMode::Cv | SupplyMode::Cc => {
                    // Open-loop CV from the setpoint via the (inverted) map,
                    // with the DAC code slewed at most
                    // board::CV_SLEW_MAX_LSB_PER_TICK counts per tick so
                    // setpoint changes step gently (avoids ringing / FET
                    // heating).
                    let target = self.cv.mv_to_code(app.supply.v_set_mv);
                    self.cv.slew(target);
                    dac_cv.set(Value::Bit12Right(self.cv.code));

                    // CC limit. In CC mode the *user* limit is used; in CV mode
                    // the same limit is the safety ceiling. Both are clamped by
                    // the power budget above.
                    let cc_target = match app.supply.mode {
                        SupplyMode::Cc => app.supply.i_set_ma,
                        _ => i_cap,
                    };
                    let cc_req = cc_target.min(i_limit);
                    // The setpoint is the current the user expects to be drawn;
                    // `set_current` cancels the bench CC offset and clamps to the
                    // settable range. Warn only when the *power* budget forces
                    // the limit below that range, so it is clear the PD contract
                    // rather than the CC setting is binding.
                    let cc_eff = CcDac::clamp_current(cc_req);
                    let below_floor = cc_req < board::CC_SET_MIN_MA;
                    if below_floor && !self.last_cc_below_floor {
                        defmt::warn!(
                            "supply: power budget limits CC to {} mA, below the {} mA settable floor (R18={} mOhm); limiting at {} mA",
                            cc_req,
                            board::CC_SET_MIN_MA,
                            board::ISENSE_SHUNT_MOHM,
                            cc_eff
                        );
                    }
                    self.last_cc_below_floor = below_floor;
                    self.cc.set_current(cc_eff);
                    dac_cc.set(Value::Bit12Right(self.cc.code));

                    // NOTE: CC mode deliberately programs the CV DAC from
                    // `v_set_mv`, exactly like CV mode. The LT8390A diode-ORs its
                    // FB error amp (regulate FB to 1 V) and its ISP/ISN current
                    // error amp (regulate to the CTRL threshold) into VC, so
                    // whichever asks for the *lower* VC wins. Parking the CV DAC
                    // at minimum output — the old CC-mode behaviour — made the
                    // voltage loop win and collapsed the output instead of
                    // current-limiting it. With the normal voltage ceiling
                    // command, the chip transitions CV↔CC on its own.

                    if app.supply.mode == SupplyMode::Cv
                        && tele.iout_ma
                            > i_limit * board::IOUT_TRIP_MARGIN_PCT / 100
                    {
                        defmt::warn!(
                            "Fault::OverCurrent SOFTWARE trip: iout={} mA > i_limit={} mA ({}% margin) vout={} mV vset={} mV cc_code={} cc_est={} mA isense_biased_high?",
                            tele.iout_ma,
                            i_limit,
                            board::IOUT_TRIP_MARGIN_PCT,
                            tele.vout_mv,
                            app.supply.v_set_mv,
                            self.cc.code,
                            CcDac::current_ma_for_code(self.cc.code)
                        );
                        app.supply.fault = Fault::OverCurrent;
                    }
                }
                SupplyMode::Off => {
                    self.cv.code = board::DAC_MAX_CODE;
                    dac_cv.set(Value::Bit12Right(self.cv.code));
                    self.cc.code = 0;
                    dac_cc.set(Value::Bit12Right(0));
                    self.last_cc_below_floor = false;
                }
            }
        }

        // A converter that is enabled, has a non-trivial limit, is sitting at
        // (or above) that limit, and whose output has fallen away from the
        // setpoint is being current-limited by the hardware CC loop.
        let cc_active = enabled
            && i_limit > 0
            && tele.iout_ma * 100 >= i_limit * 90
            && tele.vout_mv < app.supply.v_set_mv * 95 / 100;
        app.supply.cc_active = cc_active;

        // Out of regulation: enabled, has a setpoint, but the output is well
        // below it. This is *not* the same as `cc_active` — it is what a
        // converter that cannot deliver the requested power looks like, and it
        // is the case the bench run hit (collapse at ~9 A while the CC limit was
        // programmed to 16 A).
        let sagging = enabled
            && app.supply.v_set_mv > 0
            && tele.vout_mv < app.supply.v_set_mv * board::VOUT_SAG_PCT / 100;

        self.log(
            app,
            enabled,
            i_cap,
            p_cap,
            i_power_cap,
            i_limit,
            power_clamped,
            cc_active,
            sagging,
        );
    }

    /// Emit the change-triggered edge lines and the periodic consolidated
    /// snapshot. Split out so `tick` reads as control flow only.
    fn log(
        &mut self,
        app: &crate::state::AppState,
        enabled: bool,
        i_cap: u32,
        p_cap: u32,
        i_power_cap: u32,
        i_limit: u32,
        power_clamped: bool,
        cc_active: bool,
        sagging: bool,
    ) {
        let tele = app.telemetry;

        if enabled != self.last_enabled
            || app.supply.mode != self.last_mode
            || app.supply.fault != self.last_fault
        {
            defmt::info!(
                "supply: state change mode={} en={} fault={} vout={} iout={} vset={} iset={}",
                Self::mode_str(app.supply.mode),
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
                CcDac::current_ma_for_code(self.cc.code)
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
                CcDac::current_ma_for_code(self.cc.code),
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
            "supply[ctl]: mode={} en={} fault={} vset={} iset={} cv_code={} map_vout={} cc_code={} ctrl_mv={} cc_est={}",
            Self::mode_str(app.supply.mode),
            enabled,
            Self::fault_str(app.supply.fault),
            app.supply.v_set_mv,
            app.supply.i_set_ma,
            cv_code,
            CvDac::code_to_vout_mv(cv_code),
            self.cc.code,
            CcDac::ctrl_mv_for_code(self.cc.code),
            CcDac::current_ma_for_code(self.cc.code)
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
            "supply[meas]: vout={} iout={} verr={} vin={} iin={} pin={} dac_nom={} dac_impl={} fb_impl={}",
            tele.vout_mv,
            tele.iout_ma,
            vout_err,
            tele.vin_mv,
            tele.iin_ma,
            tele.pin_mw,
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
