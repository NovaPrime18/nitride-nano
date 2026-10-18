//! rev3 `High-Gain` GPIO — the CC sense-range select (`PB6`) with polarity handling.

use embassy_stm32::gpio::{Level, Output};

use crate::board::CcRange;

/// CC sense-range select on rev3 `PB6`.
///
/// `PB6` drives the `SEL` pin of the U7 TMUX1574, which sits between the output
/// shunt R96 and the LT8390A's `ISP/ISN` pair:
///
/// * **low** → the chip senses the shunt directly, `R_eff` = 2 mΩ ([`CcRange::LowGain`], 2.5–50 A).
/// * **high** → the chip senses it through the U14 INA241 ×10 amplifier,
///   `R_eff` = 20 mΩ ([`CcRange::HighGain`], 0.25–5 A, ten times finer resolution).
///
/// The mux switches the `VC` compensation network on its third channel at the
/// same time, and the net has no external pull resistor, so firmware drives it
/// explicitly from boot. Because a range change is a ×10 step in CC loop gain,
/// callers must only move it while the output stage is parked — see
/// `control::supply`.
pub struct HighGain {
    pin: Output<'static>,
    range: CcRange,
}

impl HighGain {
    pub fn new(pin: Output<'static>, range: CcRange) -> Self {
        Self { pin, range }
    }

    /// Currently selected range.
    pub fn range(&self) -> CcRange {
        self.range
    }

    /// Select a sense range and drive the pin to match. No-op when already
    /// selected, so the pin does not glitch on every call.
    pub fn set_range(&mut self, range: CcRange) {
        if range == self.range {
            return;
        }
        self.pin.set_level(match range {
            CcRange::LowGain => Level::Low,
            CcRange::HighGain => Level::High,
        });
        self.range = range;
    }
}
