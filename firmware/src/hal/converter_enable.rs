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
pub struct ConverterEnable {
    pin: Output<'static>,
    active_high: bool,
}

impl ConverterEnable {
    pub fn new(pin: Output<'static>) -> Self {
        Self {
            pin,
            active_high: crate::board::CONVERTER_DISABLE_ACTIVE_HIGH,
        }
    }

    /// Drive the converter on (`true`) or off (`false`), translating to the
    /// board's disable polarity.
    pub fn set_enabled(&mut self, enabled: bool) {
        let disable = !enabled;
        let level = if self.active_high == disable {
            Level::High
        } else {
            Level::Low
        };
        self.pin.set_level(level);
    }
}
