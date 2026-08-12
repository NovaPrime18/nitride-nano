//! ADC sampling and telemetry post-processing.
//!
//! Raw 12-bit ADC counts are scaled to physical units using the divider ratios
//! in [`crate::board`], then passed through a median-of-3 despiker and an
//! exponential moving average before being published as [`Telemetry`].

use embassy_stm32::adc::Adc;
use embassy_stm32::Peri;
use embassy_time::Instant;

use crate::board;
use crate::state::Telemetry;

/// Time constant of the telemetry low-pass filter, in milliseconds.
/// With a 2 ms ADC period, 150 ms gives ~75 samples per time constant —
/// fast enough to track real load steps, slow enough to kill
/// switching-noise jitter on the ADC readings.
const TAU_MS: f32 = 150.0;

/// Exponential moving average (EMA) low-pass filter for telemetry,
/// with median-of-3 despiking ahead of the EMA to reject single-sample
/// ADC glitches (common on switching supplies) before they enter the
/// smoothed average.
pub struct TelemetryFilter {
    tau_ms: f32,
    initialized: bool,
    last_update: Instant,

    vin: f32,
    vout: f32,
    iout: f32,

    vin_hist: [f32; 2],
    vout_hist: [f32; 2],
    iout_hist: [f32; 2],
}

impl TelemetryFilter {
    pub fn new() -> Self {
        Self::with_tau_ms(TAU_MS)
    }

    pub fn with_tau_ms(tau_ms: f32) -> Self {
        Self {
            tau_ms,
            initialized: false,
            last_update: Instant::now(),
            vin: 0.0,
            vout: 0.0,
            iout: 0.0,
            vin_hist: [0.0; 2],
            vout_hist: [0.0; 2],
            iout_hist: [0.0; 2],
        }
    }

    pub fn filter(&mut self, raw: Telemetry) -> Telemetry {
        let vin_in = median3(self.vin_hist[0], self.vin_hist[1], raw.vin_mv as f32);
        let vout_in = median3(self.vout_hist[0], self.vout_hist[1], raw.vout_mv as f32);
        let iout_in = median3(self.iout_hist[0], self.iout_hist[1], raw.iout_ma as f32);

        self.vin_hist = [self.vin_hist[1], raw.vin_mv as f32];
        self.vout_hist = [self.vout_hist[1], raw.vout_mv as f32];
        self.iout_hist = [self.iout_hist[1], raw.iout_ma as f32];

        let now = Instant::now();

        if !self.initialized {
            self.vin = vin_in;
            self.vout = vout_in;
            self.iout = iout_in;
            self.initialized = true;
        } else {
            let dt_ms = now.duration_since(self.last_update).as_micros() as f32 / 1000.0;
            // alpha derived from actual elapsed time, not an assumed sample period
            let a = 1.0 - libm::expf(-dt_ms / self.tau_ms);
            self.vin += a * (vin_in - self.vin);
            self.vout += a * (vout_in - self.vout);
            self.iout += a * (iout_in - self.iout);
        }
        self.last_update = now;

        let vout_mv = libm::roundf(self.vout) as u32;
        let iout_ma = libm::roundf(self.iout) as u32;

        Telemetry {
            vin_mv: libm::roundf(self.vin) as u32,
            vout_mv,
            iout_ma,
            pout_mw: vout_mv.saturating_mul(iout_ma) / 1000,
            temp_conv_c: raw.temp_conv_c,
            temp_input_c: raw.temp_input_c,
        }
    }
}

/// Median of three samples, computed branchlessly. A single outlier spike can
/// never win: it is always the min or the max of the three.
fn median3(a: f32, b: f32, c: f32) -> f32 {
    a.max(b).min(a.min(b).max(c))
}

impl Default for TelemetryFilter {
    fn default() -> Self {
        Self::new()
    }
}

/// Stateless sampler for the five analog channels (Vout, Isense, Vbus, two NTCs).
pub struct AdcSense;

impl AdcSense {
    pub fn new() -> Self {
        Self
    }

    /// Blocking-read all channels once and return raw (unfiltered) telemetry.
    ///
    /// Channel → peripheral mapping is fixed by the PCB: Vout/Isense/temp_conv
    /// on ADC1, Vbus on ADC2, temp_in on ADC5.
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
            let i_mv = (i_raw as u64 * board::ADC_VREF_MV as u64) / 4096;
            let i_corr = i_mv.saturating_sub(board::ISENSE_OFFSET_MV as u64);
            ((i_corr * 1000) / board::ISENSE_MV_PER_A as u64) as u32
        } else {
            0
        };

        // Instantaneous (unfiltered) power — useful for fast OCP/OPP checks
        // that shouldn't wait for the smoothed value.
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

/// Convert 12-bit ADC counts to a physical unit, given the physical value that
/// corresponds to full scale (4096 counts) after the input divider network.
fn scale(raw: u32, full_scale_phys: u32) -> u32 {
    raw.saturating_mul(full_scale_phys) / 4096
}

/// Beta-equation NTC conversion. Returns -273 °C for open/short sensor
/// readings (junction voltage at a rail), which will trip the overtemp fault
/// check only if the limit is ever set below that sentinel — today it simply
/// displays as an obviously bogus value.
fn ntc_c(raw: u32) -> i32 {
    let vref = board::ADC_VREF_MV as f32 / 1000.0;
    let v = (raw as f32 * vref) / 4096.0;
    if v <= 0.01 || v >= vref {
        return -273;
    }
    let r = board::NTC_PULLUP_OHM * v / (vref - v);
    let t0_kelvin = 298.15f32;
    let r0 = board::NTC_R25_OHM;
    let beta = board::NTC_BETA;
    let inv_t = 1.0 / t0_kelvin + libm::logf(r / r0) / beta;
    let t_kelvin = 1.0 / inv_t;
    libm::roundf(t_kelvin - 273.15) as i32
}

impl Default for AdcSense {
    fn default() -> Self {
        Self::new()
    }
}
