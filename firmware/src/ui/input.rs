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
    /// Time the current press was first observed. A button only fires once the
    /// press has been continuously present for `DEBOUNCE_MS`.
    press_at: [Instant; 4],
    /// Previous sample's pressed state, for press-edge detection.
    pressed_prev: [bool; 4],
    /// An event has already fired for the current press.
    fired: [bool; 4],
}

impl InputHandler {
    pub fn new() -> Self {
        Self {
            last_event: None,
            // encoder_delta: 0,
            press_at: [Instant::now(); 4],
            pressed_prev: [false; 4],
            fired: [false; 4],
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
            // Do NOT sample the buttons in the same tick: a rotation must never
            // be delivered as a button press. On the PD screen `EncBtn` confirms
            // and requests the highlighted preset, so a turn that also read the
            // encoder switch as closed would renegotiate instead of scroll (and
            // scrolling past 48 V wraps the highlight to 12 V). A genuine press
            // is simply picked up on the next 5 ms poll.
            return;
        }
        self.check_button(0, btn1.is_low(), InputEvent::Btn1);
        self.check_button(1, btn2.is_low(), InputEvent::Btn2);
        self.check_button(2, btn3.is_low(), InputEvent::Btn3);
        self.check_button(3, enc_btn.is_low(), InputEvent::EncBtn);
    }

    /// Edge-detect with a **stable-press** debounce: an event fires once per
    /// press, and only after the line has been continuously asserted for
    /// `DEBOUNCE_MS`. Timing from the last release (the previous behaviour) fires
    /// on the very first sample, so a one-poll glitch counts as a real press.
    fn check_button(&mut self, idx: usize, pressed: bool, ev: InputEvent) {
        let now = Instant::now();
        if pressed {
            if !self.pressed_prev[idx] {
                self.press_at[idx] = now;
                self.pressed_prev[idx] = true;
            }
            if !self.fired[idx]
                && now.duration_since(self.press_at[idx])
                    >= Duration::from_millis(board::DEBOUNCE_MS)
            {
                self.last_event = Some(ev);
                self.fired[idx] = true;
            }
        } else {
            self.pressed_prev[idx] = false;
            self.fired[idx] = false;
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
