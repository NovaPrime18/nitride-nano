//! rev3 `High-Gain` GPIO — the CC sense-range select (`PB6`) with polarity handling.

use embassy_stm32::gpio::{Level, Output};

use crate::board::CcRange;

/// CC sense-range select on rev3 `PB6`.
///
/// `PB6` drives the `SEL` pin of the U7 TMUX1574, which sits between the output
/// shunt R96 and the LT8390A's `ISP/ISN` pair:
///
/// * **low** → the chip senses the shunt directly, `R_eff` = 2 mΩ ([`CcRange::LowGain`], 2.5–50 A).
/// * **high** → the chip senses it through the U14 INA241A1 ×10 amplifier,
///   `R_eff` = 20 mΩ ([`CcRange::HighGain`], 0.25–5 A, ten times finer resolution).
///
/// The mux switches the `VC` compensation network on its third channel at the
/// same time. Its `SEL` input has an internal 6 MΩ pull-down, so a floating pin
/// powers up in the [`CcRange::LowGain`] state (the widest window) — but the
/// firmware still drives the pin explicitly, because a range change is a ×10
/// step in CC loop gain and callers must only move it while the output stage is
/// parked — see `control::supply`.
///
/// # Why [`HighGain::parked`] exists
///
/// On **rev2**, `PB6` is the encoder **A** signal (10 kΩ pull-up, switch contact
/// to GND). Driving it as the mux select would put a push-pull STM32 output
/// directly across a mechanical contact that closes ~half the detent positions —
/// a hard short at ~4× the pin's absolute-maximum source current whenever the
/// knob stops on a closed phase. The rev3-detect gate in `main.rs` therefore
/// builds a *parked* (never-driven) instance on any board that did not answer
/// the rev3 signature, which also covers a rev3 board whose PD-bus monitors
/// are absent (the mux's own SEL pull-down keeps it at ×1 there).
pub struct HighGain {
    pin: Option<Output<'static>>,
    range: CcRange,
}

impl HighGain {
    pub fn new(pin: Output<'static>, range: CcRange) -> Self {
        Self {
            pin: Some(pin),
            range,
        }
    }

    /// A range select that drives nothing. `set_range` still tracks the
    /// requested range in software (so logging and the CC map stay consistent),
    /// but the pin is never touched: `PB6` stays in its reset floating-input
    /// state, and the TMUX1574's internal `SEL` pull-down selects `LowGain` on
    /// rev3.
    pub fn parked() -> Self {
        Self {
            pin: None,
            range: CcRange::LowGain,
        }
    }

    /// Currently selected range.
    pub fn range(&self) -> CcRange {
        self.range
    }

    /// Select a sense range and drive the pin to match. No-op when already
    /// selected, so the pin does not glitch on every call.
    ///
    /// On a **parked** instance this is a complete no-op, including the software
    /// view: the hardware is at `LowGain` (the TMUX1574's internal `SEL`
    /// pull-down), and letting the software view diverge from that would make
    /// every CC conversion — including the current-limit backstop — wrong by a
    /// factor of ten. The conservative `LowGain` window is the safest range for
    /// a board the sequencer cannot verify anyway.
    pub fn set_range(&mut self, range: CcRange) {
        if range == self.range || self.pin.is_none() {
            return;
        }
        if let Some(pin) = self.pin.as_mut() {
            pin.set_level(match range {
                CcRange::LowGain => Level::Low,
                CcRange::HighGain => Level::High,
            });
        }
        self.range = range;
    }
}