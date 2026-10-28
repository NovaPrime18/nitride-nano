//! rev3 `CNV-EN-LOADEN` GPIO (LT8390A `LOADEN`, `PB3`) with polarity handling.

use embassy_stm32::gpio::{Level, Output};

/// LT8390A `LOADEN` line on rev3 `PB3`.
///
/// This is the pin the output-stage start-up sequence hangs on, and its two
/// states are both load-bearing (LT8390A datasheet, Rev. C):
///
/// * **low** — `TG1`/`TG2` off and **`BG1`/`BG2` on**, so the bridge is parked
///   with the inductor side clamped to ground and the **output node isolated**
///   (`TG2` is the `SW2`→`VOUT` switch); `VC` is disconnected from its internal
///   loads (which also parks the CC loop where it left off) and `LOADTG` is off.
/// * **high** — the part is allowed to run: once `CTRL` is above its rising
///   latch-off (~0.325 V), the start-up state machine reaches `UP/PRE` and
///   soft-start begins.
///
/// The wrapper stays polarity-free on purpose — the *sequence* is the caller's
/// business, not this type's — but it is a distinct type from the converter
/// `EN/UVLO` line so the two can never be swapped by a copy-paste.
///
/// [`LoadEnable::parked`] builds one that never drives its pin — see the
/// module-level note in `main.rs` on the rev2/rev3 cross-compatibility gate.
pub struct LoadEnable {
    pin: Option<Output<'static>>,
}

impl LoadEnable {
    pub fn new(pin: Output<'static>) -> Self {
        Self { pin: Some(pin) }
    }

    /// A load-enable that drives nothing.
    ///
    /// On a board the rev3-detect did not confirm, `PB3` is left in its reset
    /// floating-input state. On rev2 `PB3` was `SWITCH_EN_MANUAL` (an input to
    /// the old `U7` AND gate, since removed) and is safe to float; on rev3 the
    /// sequencer cannot start without it, so parking it parks the output.
    pub fn parked() -> Self {
        Self { pin: None }
    }

    /// Raise (`true`) or drop (`false`) `CNV-EN-LOADEN`. Active high. A no-op
    /// on a parked instance.
    pub fn set_enabled(&mut self, enabled: bool) {
        let Some(pin) = self.pin.as_mut() else {
            return;
        };
        pin.set_level(if enabled {
            Level::High
        } else {
            Level::Low
        });
    }
}