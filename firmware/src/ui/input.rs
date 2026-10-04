//! Debounced button/encoder sampling, producing a single [`InputEvent`] per poll.
//!
//! The encoder button carries two gestures: a single click (VSET mode on the
//! Main screen) and a double click (fine/coarse step toggle). To tell them apart
//! the single-click action is **deferred** by [`board::ENC_DOUBLE_CLICK_MS`]: the
//! first press arms a pending click, and it is only emitted as [`InputEvent::EncBtn`]
//! once that window expires without a second press. A second press inside the
//! window emits [`InputEvent::EncDoubleClick`] instead and cancels the pending
//! single click, so a double click can never also toggle the mode.
//!
//! The pending click lives outside `last_event` because the main loop drains and
//! clears `last_event` every [`board::INPUT_POLL_MS`]; it must survive that drain
//! for the length of the double-click window.

use embassy_stm32::gpio::Input;
use embassy_time::{Duration, Instant};

use crate::board;

/// A single user interaction, consumed by [`crate::ui::menu::apply_input`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Btn1,
    Btn2,
    Btn3,
    /// Single click of the encoder button (deferred; see the module docs).
    EncBtn,
    /// Two encoder-button clicks inside [`board::ENC_DOUBLE_CLICK_MS`].
    EncDoubleClick,
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
    /// Time the first encoder-button click of a possible double click landed.
    /// `Some` means a single click is still pending; it is emitted as
    /// [`InputEvent::EncBtn`] once the double-click window lapses without a
    /// second press. Deliberately not touched by [`InputHandler::clear_event`].
    enc_pending_single_at: Option<Instant>,
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
            enc_pending_single_at: None,
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
        let now = Instant::now();

        // A pending single click whose window has lapsed becomes the real
        // single-click event. Rotation wins this tick so a detent is never
        // dropped: the click is emitted on the following 5 ms poll instead.
        if let Some(t) = self.enc_pending_single_at {
            if now.duration_since(t) >= Duration::from_millis(board::ENC_DOUBLE_CLICK_MS) {
                if enc_delta != 0 {
                    // Rotation wins this tick: emit the detent and keep the
                    // click pending so it still fires once the knob stops.
                    self.last_event = Some(InputEvent::EncTurn(enc_delta));
                } else {
                    self.enc_pending_single_at = None;
                    self.last_event = Some(InputEvent::EncBtn);
                }
                // Returning here (rather than also sampling the buttons) keeps
                // `last_event` from being overwritten in the same tick; a
                // simultaneous button press is picked up on the next poll.
                return;
            }
        }

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
        self.check_enc_button(enc_btn.is_low());
    }

    /// Edge-detect with a **stable-press** debounce: an event fires once per
    /// press, and only after the line has been continuously asserted for
    /// `DEBOUNCE_MS`. Timing from the last release (the previous behaviour) fires
    /// on the very first sample, so a one-poll glitch counts as a real press.
    fn check_button(&mut self, idx: usize, pressed: bool, ev: InputEvent) {
        if self.debounced_press(idx, pressed) {
            self.last_event = Some(ev);
        }
    }

    /// Encoder-button variant of [`Self::check_button`]: a debounced press either
    /// completes a pending single click into a double click, or arms a new
    /// pending single click.
    ///
    /// Any pending value reaching here is necessarily inside the double-click
    /// window: the expiry branch at the top of [`Self::poll`] clears an older one
    /// before the buttons are ever sampled.
    fn check_enc_button(&mut self, pressed: bool) {
        if !self.debounced_press(3, pressed) {
            return;
        }
        if self.enc_pending_single_at.take().is_some() {
            self.last_event = Some(InputEvent::EncDoubleClick);
        } else {
            self.enc_pending_single_at = Some(Instant::now());
        }
    }

    /// Shared stable-press debounce. Returns true exactly once per physical
    /// press, on the first sample at or after `DEBOUNCE_MS`.
    fn debounced_press(&mut self, idx: usize, pressed: bool) -> bool {
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
                self.fired[idx] = true;
                return true;
            }
        } else {
            self.pressed_prev[idx] = false;
            self.fired[idx] = false;
        }
        false
    }

    /// Discard the pending event after the main loop has consumed it.
    ///
    /// This never touches `enc_pending_single_at`: the deferred single click must
    /// outlive the per-poll drain of `last_event`.
    pub fn clear_event(&mut self) {
        self.last_event = None;
    }
}

impl Default for InputHandler {
    fn default() -> Self {
        Self::new()
    }
}
