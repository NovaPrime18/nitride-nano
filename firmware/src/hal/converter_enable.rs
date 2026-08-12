//! Converter run/disable GPIO (LT8390 RUN pin) with polarity handling.

use embassy_stm32::gpio::{Level, Output};

/// LT8390 / converter run-disable line on PA11.
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
