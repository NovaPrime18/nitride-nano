//! Input-to-output efficiency, shared by the OLED readout and the sweep
//! diagnostics.
//!
//! Efficiency is `pout / pin`, where `pout` is the MCU's ADC output power and
//! `pin` the INA228 input-bus power. Both are already smoothed by their own
//! filters (median-of-3 + EMA on the ADC; 16× averaging inside the INA228), but
//! a *single* ratio of two independently filtered channels still amplifies
//! their residual jitter. [`EfficiencyAccumulator`] therefore integrates both
//! powers over a window and divides once — the ratio of the means, not the mean
//! of the ratios — which is the physically meaningful windowed efficiency.
//!
//! All arithmetic is integer-only; the unit is tenths of a percent (875 =
//! 87.5 %) to match the OLED's one-decimal display without any float
//! formatting in the log path.

/// Unity efficiency in tenths of a percent.
pub const ETA_UNITY_TENTHS: u32 = 1000;

/// Input-to-output efficiency in tenths of a percent, or `None` when it is
/// undefined.
///
/// A non-positive input power makes the ratio meaningless (no load, or the
/// INA228 is not reporting), so that case returns `None` rather than a bogus
/// number. Values at or above [`ETA_UNITY_TENTHS`] are returned unchanged: a
/// reading above unity is measurement error, and the caller decides whether to
/// clamp, flag or discard it.
pub fn tenths_pct(pin_mw: i32, pout_mw: u32) -> Option<u32> {
    if pin_mw <= 0 {
        return None;
    }
    (pout_mw as u64 * 1000 / pin_mw as u64).try_into().ok()
}

/// Windowed accumulator for one efficiency measurement point.
///
/// Samples are only integrated while the input monitor is responding and the
/// input power is positive; everything else (no PD contract, INA228 absent or
/// NACKing, output parked) is silently dropped so it cannot poison the mean.
/// The power sums are `u64` and saturating: a full 2 s dwell at the 240 W design
/// ceiling sums to ~4.8e8 mW·ms for each channel, far inside range.
#[derive(Clone, Copy, Debug)]
pub struct EfficiencyAccumulator {
    pin_mw_sum: u64,
    pout_mw_sum: u64,
    samples: u16,
}

impl EfficiencyAccumulator {
    pub const fn new() -> Self {
        Self {
            pin_mw_sum: 0,
            pout_mw_sum: 0,
            samples: 0,
        }
    }

    /// Drop every sample collected so far, ready for the next measurement point.
    pub fn reset(&mut self) {
        self.pin_mw_sum = 0;
        self.pout_mw_sum = 0;
        self.samples = 0;
    }

    /// Integrate one telemetry sample. Returns `true` when the sample was
    /// accepted. Rejected samples (stale/absent input monitor, non-positive
    /// input power) do not advance [`Self::samples`].
    pub fn push(&mut self, pin_mw: i32, pout_mw: u32, ina_ok: bool) -> bool {
        if !ina_ok || pin_mw <= 0 {
            return false;
        }
        self.pin_mw_sum = self.pin_mw_sum.saturating_add(pin_mw as u64);
        self.pout_mw_sum = self.pout_mw_sum.saturating_add(pout_mw as u64);
        self.samples = self.samples.saturating_add(1);
        true
    }

    /// Number of accepted samples in the window.
    pub fn samples(&self) -> u16 {
        self.samples
    }

    /// Mean input power over the window, in milliwatts (0 when empty).
    pub fn mean_pin_mw(&self) -> u32 {
        if self.samples == 0 {
            return 0;
        }
        (self.pin_mw_sum / self.samples as u64) as u32
    }

    /// Mean output power over the window, in milliwatts (0 when empty).
    pub fn mean_pout_mw(&self) -> u32 {
        if self.samples == 0 {
            return 0;
        }
        (self.pout_mw_sum / self.samples as u64) as u32
    }

    /// Windowed efficiency in tenths of a percent, or `None` with no accepted
    /// samples.
    pub fn tenths_pct(&self) -> Option<u32> {
        if self.samples == 0 {
            return None;
        }
        tenths_pct(self.mean_pin_mw() as i32, self.mean_pout_mw())
    }
}

impl Default for EfficiencyAccumulator {
    fn default() -> Self {
        Self::new()
    }
}
