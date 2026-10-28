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
///
/// [`OutputSwitch::parked`] builds one that never drives its pin — see the
/// module-level note in `main.rs` on the rev2/rev3 cross-compatibility gate.
pub struct OutputSwitch {
    pin: Option<Output<'static>>,
}

impl OutputSwitch {
    pub fn new(pin: Output<'static>) -> Self {
        Self { pin: Some(pin) }
    }

    /// An output switch that drives nothing.
    ///
    /// On a board the rev3-detect did not confirm, `PB2` is left in its reset
    /// floating-input state: on rev2 it is unconnected, and the `R21` 100 kΩ
    /// pulldown on rev3 keeps the load disconnected either way.
    pub fn parked() -> Self {
        Self { pin: None }
    }

    /// Close (`true`) or open (`false`) the output load switch. Active high. A
    /// no-op on a parked instance.
    pub fn set_closed(&mut self, closed: bool) {
        let Some(pin) = self.pin.as_mut() else {
            return;
        };
        pin.set_level(if closed {
            Level::High
        } else {
            Level::Low
        });
    }
}