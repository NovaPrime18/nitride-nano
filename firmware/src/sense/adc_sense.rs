use embassy_stm32::adc::Adc;
use embassy_stm32::Peri;

use crate::board;
use crate::state::Telemetry;

pub struct AdcSense;

impl AdcSense {
    pub fn new() -> Self {
        Self
    }

    pub fn sample(
        &mut self,
        adc1: &mut Adc<'_, embassy_stm32::peripherals::ADC1>,
        adc2: &mut Adc<'_, embassy_stm32::peripherals::ADC2>,
        adc5: &mut Adc<'_, embassy_stm32::peripherals::ADC5>,
        vout: &mut Peri<'_, embassy_stm32::peripherals::PA0>,
        isense: &mut Peri<'_, embassy_stm32::peripherals::PA3>,
        vbus: &mut Peri<'_, embassy_stm32::peripherals::PA7>,
        temp_conv: &mut Peri<'_, embassy_stm32::peripherals::PA1>,
        temp_in: &mut Peri<'_, embassy_stm32::peripherals::PA9>,
    ) -> Telemetry {
        let vout_raw = adc1.blocking_read(vout) as u32;
        let i_raw = adc1.blocking_read(isense) as u32;
        let vbus_raw = adc2.blocking_read(vbus) as u32;
        let t_conv_raw = adc1.blocking_read(temp_conv) as u32;
        let t_in_raw = adc5.blocking_read(temp_in) as u32;

        let vout_mv = scale(vout_raw, board::VOUT_SENSE_NUM);
        let vbus_mv = scale(vbus_raw, board::VBUS_SENSE_NUM);
        let iout_ma = if board::ISENSE_MV_PER_A > 0 {
            ((i_raw as u64 * board::ADC_VREF_MV as u64 * 10) / (4096 * board::ISENSE_MV_PER_A as u64)) as u32
        } else {
            0
        };

        let pout_mw = vout_mv.saturating_mul(iout_ma) / 1000;

        Telemetry {
            vin_mv: vbus_mv,
            vout_mv,
            iout_ma,
            pout_mw,
            temp_conv_c: ntc_c(t_conv_raw),
            temp_input_c: ntc_c(t_in_raw),
        }
    }
}

fn scale(raw: u32, full_scale_phys: u32) -> u32 {
    raw.saturating_mul(full_scale_phys) / 4096
}

fn ntc_c(raw: u32) -> i32 {
    let v = (raw as f32 * board::ADC_VREF_MV as f32) / 4096.0 / 1000.0;
    // Match PD240W example: return -273.15 (absolute zero) as error indicator
    if v <= 0.01 || v >= board::ADC_VREF_MV as f32 / 1000.0 {
        return -273;
    }
    let r = board::NTC_PULLUP_OHM * v / (3.3 - v);
    let t0_kelvin = 298.15f32; // 25°C in Kelvin
    let r0 = board::NTC_R25_OHM;
    let beta = board::NTC_BETA;
    // Beta parameter equation (simplified Steinhart-Hart)
    // T = 1 / (1/T0 + (1/Beta) * ln(R/R0))
    let inv_t = 1.0 / t0_kelvin + libm::logf(r / r0) / beta;
    let t_kelvin = 1.0 / inv_t;
    (t_kelvin - 273.15) as i32
}

impl Default for AdcSense {
    fn default() -> Self {
        Self::new()
    }
}
