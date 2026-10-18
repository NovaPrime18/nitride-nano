//! rev3 `CNV-EN-LOADEN` GPIO (LT8390A `LOADEN`, `PB3`) with polarity handling.

use embassy_stm32::gpio::{Level, Output};

/// LT8390A `LOADEN` line on rev3 `PB3`.
///
/// This is the pin the output-stage start-up sequence hangs on, and its two
/// states are both load-bearing (LT8390A datasheet, Rev. C):
///
/// * **low** — `TG1`/`TG2` off and **`BG1`/`BG2` on**, so the four-switch
///   bridge's bottom gates short the output to ground; `VC` is disconnected from
///   its internal loads (which also parks the CC loop where it left off) and
///   `LOADTG` is off. The output is held at a defined 0 V.
/// * **high** — the part is allowed to run: once `CTRL` is above its rising
///   latch-off (~0.325 V), the start-up state machine reaches `UP/PRE` and
///   soft-start begins.
///
/// The wrapper stays polarity-free on purpose — the *sequence* is the caller's
/// business, not this type's — but it is a distinct type from the converter
/// `EN/UVLO` line so the two can never be swapped by a copy-paste.
pub struct LoadEnable {
    pin: Output<'static>,
}

impl LoadEnable {
    pub fn new(pin: Output<'static>) -> Self {
        Self { pin }
    }

    /// Raise (`true`) or drop (`false`) `CNV-EN-LOADEN`. Active high.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.pin.set_level(if enabled {
            Level::High
        } else {
            Level::Low
        });
    }
}
