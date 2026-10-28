//! Converter run/disable GPIO (LT8390 RUN pin) with polarity handling.

use embassy_stm32::gpio::{Level, Output};

/// Converter run-disable line on rev3 `PB1`.
///
/// `PB1 → R78 (10 Ω) → Q13 gate`; `Q13`'s drain pulls the LT8390A `EN/UVLO`
/// node low. rev2 drove this from PA11, which rev3 needs for `USB_DM`.
///
/// The pin is a *disable* input whose active level depends on the hardware
/// revision ([`crate::board::CONVERTER_DISABLE_ACTIVE_HIGH`]); this wrapper
/// exposes the polarity-free `set_enabled` view so callers can't get it wrong.
///
/// [`ConverterEnable::parked`] builds one that never drives its pin — see the
/// module-level note in `main.rs` on the rev2/rev3 cross-compatibility gate.
pub struct ConverterEnable {
    pin: Option<Output<'static>>,
    active_high: bool,
}

impl ConverterEnable {
    pub fn new(pin: Output<'static>) -> Self {
        Self {
            pin: Some(pin),
            active_high: crate::board::CONVERTER_DISABLE_ACTIVE_HIGH,
        }
    }

    /// A converter-enable that drives nothing.
    ///
    /// On a board the rev3-detect did not confirm (a rev2 board, or a rev3
    /// whose PD-bus monitors did not answer), `PB1` is left in its reset
    /// floating-input state. On rev2 `PB1` is unconnected, and on rev3 the
    /// hardware `R7`/`R8` EN/UVLO divider simply keeps its default state —
    /// the parked-CV DAC (0 V target) means the converter idles either way.
    pub fn parked() -> Self {
        Self {
            pin: None,
            active_high: crate::board::CONVERTER_DISABLE_ACTIVE_HIGH,
        }
    }

    /// Drive the converter on (`true`) or off (`false`), translating to the
    /// board's disable polarity. A no-op on a parked instance.
    pub fn set_enabled(&mut self, enabled: bool) {
        let Some(pin) = self.pin.as_mut() else {
            return;
        };
        let disable = !enabled;
        let level = if self.active_high == disable {
            Level::High
        } else {
            Level::Low
        };
        pin.set_level(level);
    }
}