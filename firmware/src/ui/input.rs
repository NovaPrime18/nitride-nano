//! Debounced button/encoder sampling, producing a single [`InputEvent`] per poll.

use embassy_stm32::gpio::Input;
use embassy_time::{Duration, Instant};

use crate::board;

/// A single user interaction, consumed by [`crate::ui::menu::apply_input`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Btn1,
    Btn2,
    Btn3,
    EncBtn,
    /// Encoder rotation in detents since the last poll (sign = direction).
    EncTurn(i16),
}

/// Polls the three buttons, the encoder push-button, and the quadrature encoder.
///
/// Only one event is kept per poll cycle (`last_event`); the main loop drains it
/// every [`crate::board::INPUT_POLL_MS`] and calls [`InputHandler::clear_event`],
/// so nothing queues up while the UI is busy.
pub struct InputHandler {
    pub last_event: Option<InputEvent>,
    // TODO(dead-code): stored but never read — the encoder delta is delivered to
    // consumers inside the `InputEvent::EncTurn` payload instead.
    // pub encoder_delta: i16,
    debounce: [Instant; 4],
    held: [bool; 4],
}

impl InputHandler {
    pub fn new() -> Self {
        Self {
            last_event: None,
            // encoder_delta: 0,
            debounce: [Instant::now(); 4],
            held: [false; 4],
        }
    }

    /// Sample all inputs. `enc_delta` is the quadrature count change measured by
    /// the caller since the previous poll (already wrapping-adjusted).
    pub fn poll(
        &mut self,
        btn1: &Input<'_>,
        btn2: &Input<'_>,
        btn3: &Input<'_>,
        enc_btn: &Input<'_>,
        enc_delta: i16,
    ) {
        if enc_delta != 0 {
            self.last_event = Some(InputEvent::EncTurn(enc_delta));
        }
        self.check_button(0, btn1.is_low(), InputEvent::Btn1);
        self.check_button(1, btn2.is_low(), InputEvent::Btn2);
        self.check_button(2, btn3.is_low(), InputEvent::Btn3);
        self.check_button(3, enc_btn.is_low(), InputEvent::EncBtn);
    }

    /// Edge-detect with debounce: an event fires once per press, only after the
    /// line has been released (and settled) for at least `DEBOUNCE_MS`.
    fn check_button(&mut self, idx: usize, pressed: bool, ev: InputEvent) {
        let now = Instant::now();
        if pressed && !self.held[idx] {
            if now.duration_since(self.debounce[idx]) >= Duration::from_millis(board::DEBOUNCE_MS) {
                self.last_event = Some(ev);
                self.held[idx] = true;
                self.debounce[idx] = now;
            }
        } else if !pressed {
            if self.held[idx] {
                // Restart the debounce window on release so a switch that
                // bounce-retriggers within DEBOUNCE_MS can't double-fire.
                self.debounce[idx] = now;
            }
            self.held[idx] = false;
        }
    }

    /// Discard the pending event after the main loop has consumed it.
    pub fn clear_event(&mut self) {
        self.last_event = None;
    }
}

impl Default for InputHandler {
    fn default() -> Self {
        Self::new()
    }
}
