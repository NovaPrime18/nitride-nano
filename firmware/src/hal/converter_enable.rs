use embassy_stm32::gpio::{Level, Output};

/// LT8390 / converter run-disable line on PA11.
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
