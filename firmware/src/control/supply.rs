use embassy_stm32::dac::{DacCh1, Value};
use embassy_stm32::mode::Blocking;
use embassy_stm32::peripherals::{DAC1, DAC2};

use crate::control::dac_cc::CcDac;
use crate::control::dac_cv::CvDac;
use crate::hal::converter_enable::ConverterEnable;
use crate::state::{Fault, SupplyMode};

pub struct SupplyController {
    cv: CvDac,
    cc: CcDac,
}

impl SupplyController {
    pub fn new() -> Self {
        Self {
            cv: CvDac::new(),
            cc: CcDac::new(),
        }
    }

    pub fn tick(
        &mut self,
        app: &mut crate::state::AppState,
        dac_cv: &mut DacCh1<'_, DAC1, Blocking>,
        dac_cc: &mut DacCh1<'_, DAC2, Blocking>,
        en: &mut ConverterEnable,
    ) {
        let tele = app.telemetry;
        let now = embassy_time::Instant::now();

        // Hard overvoltage guard with 5% safety margin below design max.
        // This trips BEFORE VOUT_MAX_MV to catch runaway before damage.
        let ov_threshold = crate::board::VOUT_MAX_MV * 105 / 100;
        if tele.iout_ma > crate::board::IOUT_MAX_MA {
            app.supply.fault = Fault::OverCurrent;
            defmt::info!("Fault::OverCurrent");
        } else if tele.vout_mv > ov_threshold {
            app.supply.fault = Fault::OverVoltage;
            defmt::info!("Fault::OverVoltage: {} mV > {} mV threshold", tele.vout_mv, ov_threshold);
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
        }
        
        if app.supply.fault != Fault::None {
            app.supply.enabled = false;
            //app.supply.mode = SupplyMode::Off;
        }

        let enabled = app.supply.enabled && app.supply.fault == Fault::None;
        en.set_enabled(enabled);

        if !enabled {
            self.cv.code = 0;
            dac_cv.set(Value::Bit12Right(0));
            self.cc.set_current(0);
            dac_cc.set(Value::Bit12Right(0));
            app.supply.v_slewed_mv = 0;
            return;
        }

        app.supply.v_slewed_mv = self.cv.apply_slew(app.supply.v_set_mv, now);

        let i_cap = app
            .supply
            .i_set_ma
            .min(app.supply.input_current_cap_ma)
            .min(crate::board::IOUT_MAX_MA);
        let p_cap = app.supply.input_power_cap_mw.min(crate::board::POWER_MAX_MW);
        let i_power_cap = if tele.vout_mv > 0 {
            (p_cap * 1000) / tele.vout_mv
        } else {
            i_cap
        };
        let i_limit = i_cap.min(i_power_cap);

        match app.supply.mode {
            SupplyMode::Cv => {
                self.cv
                    .update_closed_loop(app.supply.v_slewed_mv, tele.vout_mv);
                dac_cv.set(Value::Bit12Right(self.cv.code));
                self.cc.set_current(i_limit);
                dac_cc.set(Value::Bit12Right(self.cc.code));
                if tele.iout_ma > i_limit {
                    app.supply.fault = Fault::OverCurrent;
                }
            }
            SupplyMode::Cc => {
                // Disable CV DAC in CC mode to prevent it from overriding the CC controller.
                // The CV DAC must NOT be driven from measured Vout here — if Vout spikes,
                // a high DAC code would tell the converter to drive even harder, causing runaway.
                self.cv.code = 0;
                dac_cv.set(Value::Bit12Right(0));

                self.cc.set_current(app.supply.i_set_ma.min(i_limit));
                dac_cc.set(Value::Bit12Right(self.cc.code));
            }
            SupplyMode::Off => {
                self.cv.code = 0;
                dac_cv.set(Value::Bit12Right(0));
                self.cc.set_current(0);
                dac_cc.set(Value::Bit12Right(0));
            }
        }
    }
}

impl Default for SupplyController {
    fn default() -> Self {
        Self::new()
    }
}
