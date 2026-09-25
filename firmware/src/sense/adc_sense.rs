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
            // Input-side fields are owned by the INA228 poll; the caller copies
            // the previous values back over these placeholders.
            iin_ma: 0,
            pin_mw: 0,
            ina_temp_c: 25,
            ina_ok: false,
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

/// Number of raw PA3 samples averaged when learning the ISMON zero-current
/// level. Each sample is ~15 µs of ADC time, so 64 of them average out the
/// ±1-count dither without a noticeable boot delay.
const ISENSE_ZERO_SAMPLES: u32 = 64;

/// Sampler for the five analog channels (Vout, Isense, Vbus, two NTCs).
pub struct AdcSense {
    /// Raw PA3 count measured with the output stage parked (no load current).
    ///
    /// The LT8390A drives ISMON to its offset when no current flows, but that
    /// offset is only specified to 0.20–0.30 V while the current signal is
    /// 20 mV/A. Subtracting a fixed constant therefore swamps the low-current
    /// range, so the zero is measured at boot and subtracted in raw counts
    /// (which also cancels the ADC's own offset). Seeded with the
    /// datasheet-typical offset so pre-calibration samples stay sane.
    i_zero_raw: u32,
    /// Most recent raw PA3 count, kept for bring-up diagnostics. Together with
    /// [`Self::zero_raw`] this shows how far the ISMON node moves with load,
    /// independently of the scaling constants.
    last_i_raw: u32,
}

impl AdcSense {
    pub fn new() -> Self {
        Self {
            i_zero_raw: board::ISENSE_OFFSET_MV * 4096 / board::ADC_VREF_MV,
            last_i_raw: 0,
        }
    }

    /// Raw ISMON count learned at zero current (the runtime calibration).
    pub fn zero_raw(&self) -> u32 {
        self.i_zero_raw
    }

    /// Most recent raw ISMON count.
    pub fn last_i_raw(&self) -> u32 {
        self.last_i_raw
    }

    /// Learn the ISMON zero-current level from PA3.
    ///
    /// Call only while the output stage is disabled and no load current flows:
    /// the LT8390A then drives ISMON to its offset. Returns the averaged raw
    /// count that will be subtracted from every later sample (also stored).
    pub fn calibrate_zero(
        &mut self,
        adc1: &mut Adc<'_, embassy_stm32::peripherals::ADC1>,
        isense: &mut Peri<'_, embassy_stm32::peripherals::PA3>,
    ) -> u32 {
        let mut sum: u64 = 0;
        for _ in 0..ISENSE_ZERO_SAMPLES {
            sum += adc1.blocking_read(isense) as u64;
        }
        let zero = (sum / ISENSE_ZERO_SAMPLES as u64) as u32;

        // Plausibility window: the specified offset spread is 200–300 mV, so a
        // count outside a generous 150–350 mV band means ISMON is unpowered or
        // shorted (or the rail is not ready yet). Keep the seed in that case.
        let lo = 150 * 4096 / board::ADC_VREF_MV;
        let hi = 350 * 4096 / board::ADC_VREF_MV;
        if zero >= lo && zero <= hi {
            self.i_zero_raw = zero;
        }
        self.i_zero_raw
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
        self.last_i_raw = i_raw;
        let vbus_raw = adc2.blocking_read(vbus) as u32;
        let t_conv_raw = adc1.blocking_read(temp_conv) as u32;
        let t_in_raw = adc5.blocking_read(temp_in) as u32;

        let vout_mv = scale(vout_raw, board::VOUT_SENSE_NUM);
        let vbus_mv = scale(vbus_raw, board::VBUS_SENSE_NUM);
        let iout_ma = if board::ISENSE_MV_PER_A > 0 {
            // ISMON = 10·V(ISP−ISN) + offset. Subtract the measured zero-current
            // count (cancelling both the part's offset and the ADC's own offset)
            // before scaling by the datasheet gain. One count is ~0.806 mV, so
            // keeping the arithmetic in counts preserves the full resolution.
            let d = i_raw.saturating_sub(self.i_zero_raw) as u64;
            ((d * board::ADC_VREF_MV as u64 * 1000) / (4096 * board::ISENSE_MV_PER_A as u64)) as u32
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
            // Filled in by the INA228 poll.
            iin_ma: 0,
            pin_mw: 0,
            ina_temp_c: 25,
            ina_ok: false,
        }
    }
}

/// Convert 12-bit ADC counts to a physical unit, given the physical value that
/// corresponds to full scale (4096 counts) after the input divider network.
fn scale(raw: u32, full_scale_phys: u32) -> u32 {
    raw.saturating_mul(full_scale_phys) / 4096
}

/// Temperature reported for a failed (open or shorted) thermistor. Deliberately
/// above [`board::NTC_OVERTEMP_C`] so a broken sensor latches the over-temperature
/// fault and parks the output instead of silently reading as a very cold junction.
const NTC_INVALID_C: i32 = 1000;

/// Raw-count window outside which the divider cannot be reporting a real NTC
/// temperature. An open thermistor pulls the junction to the 3.3 V rail
/// (raw ≈ 4095) and a shorted one to GND (raw ≈ 0); the valid range for roughly
/// −40…+150 °C is only ~0x00A6…0x0FD1, so these bounds are deliberately generous.
const NTC_RAW_MIN: u32 = 16;
const NTC_RAW_MAX: u32 = 4080;

/// Beta-equation NTC conversion. Readings at either rail are treated as a failed
/// sensor and reported as [`NTC_INVALID_C`] (fail-safe: an open NTC used to read
/// as ~−83 °C and a shorted one as −273 °C, so an overtemp check could never see
/// them).
fn ntc_c(raw: u32) -> i32 {
    if raw <= NTC_RAW_MIN || raw >= NTC_RAW_MAX {
        return NTC_INVALID_C;
    }
    let vref = board::ADC_VREF_MV as f32 / 1000.0;
    let v = (raw as f32 * vref) / 4096.0;
    if v <= 0.01 || v >= vref {
        return NTC_INVALID_C;
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
