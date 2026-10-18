//! rev3 `PWR_G` GPIO — the output load switch (`PB2`) with polarity handling.

use embassy_stm32::gpio::{Level, Output};

/// Output load-switch gate drive on rev3 `PB2`.
///
/// `PB2 (PWR_G) → Q12 (AO3400A, R21 100 kΩ pulldown) → Q7 (AO3401A, from +5V)
/// → R58 (27 Ω) → Q5 gate`. `Q5` is the EPC2367 low-side switch in the output
/// **return** path, between the output-return shunt R96 and ground, so closing
/// it is what actually connects the load.
///
/// It is deliberately pulled low in hardware, which makes "MCU held in reset" a
/// *disconnected load* — the load is safe before firmware runs. The sequencer
/// asserts it last, only after the converter has reached its setpoint.
pub struct OutputSwitch {
    pin: Output<'static>,
}

impl OutputSwitch {
    pub fn new(pin: Output<'static>) -> Self {
        Self { pin }
    }

    /// Close (`true`) or open (`false`) the output load switch. Active high.
    pub fn set_closed(&mut self, closed: bool) {
        self.pin.set_level(if closed {
            Level::High
        } else {
            Level::Low
        });
    }
}
