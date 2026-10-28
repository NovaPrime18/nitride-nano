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
//!
//! # Buttons that need an analog level
//!
//! `BTN2`/`BTN3` are ordinary active-low GPIOs, but two buttons on rev3 are not
//! trustworthy as logic levels and are read by the **ADC** instead, thresholded
//! by [`AnalogButton`] with hysteresis:
//!
//! * `ENC_BUTT` (PB0, `ADC1_IN15`) — a rev3 board pulled it down to only ~2.4 V
//!   when pressed, and no GPIO calls that a logic low (`V_IL` ≈ 0.3 · VDD ≈ 1 V).
//!   Thresholds: [`board::ENC_BTN_PRESSED_MV`] / [`board::ENC_BTN_RELEASED_MV`].
//! * `BUTT_1` (PB9) — *only after a rework*, see [`board::BTN1_ANALOG`]: PB9 has no
//!   analog input, so the read must be moved to `PA2` (`ADC1_IN3`) first.
//!   Thresholds: [`board::BTN1_PRESSED_MV`] / [`board::BTN1_RELEASED_MV`].
//!
//! The caller samples the pin and passes pressed/released into [`InputHandler`],
//! which keeps the debouncing, the single/double click logic and the event
//! routing identical for both kinds of input.

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
    /// A button is only armed once its line has been seen *released* at least
    /// once. A line that is already low when the firmware starts (a stuck or
    /// mid-level pin, a key held through a reset) therefore cannot fire on the
    /// first poll — that is what made a stuck `BUTT_1` flip the supply mode at
    /// every boot instead of doing nothing.
    armed: [bool; 4],
}

impl InputHandler {
    pub fn new() -> Self {
        Self {
            last_event: None,
            enc_pending_single_at: None,
            press_at: [Instant::now(); 4],
            pressed_prev: [false; 4],
            fired: [false; 4],
            armed: [false; 4],
        }
    }

    /// Sample all inputs.
    ///
    /// `btn1_pressed` and `enc_btn_pressed` are the two buttons the caller may
    /// have sampled analogically ([`board::BTN1_ANALOG`] and the encoder switch —
    /// see the module docs); `btn2`/`btn3` are plain active-low GPIOs. `enc_delta`
    /// is the quadrature count change measured by the caller since the previous
    /// poll (already wrapping-adjusted).
    pub fn poll(
        &mut self,
        btn1_pressed: bool,
        btn2: &Input<'_>,
        btn3: &Input<'_>,
        enc_btn_pressed: bool,
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

        self.check_button(0, btn1_pressed, InputEvent::Btn1);
        self.check_button(1, btn2.is_low(), InputEvent::Btn2);
        self.check_button(2, btn3.is_low(), InputEvent::Btn3);
        self.check_enc_button(enc_btn_pressed);
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
        // Ignore a line that is already asserted when the firmware starts until it
        // has been seen released once (see `armed`).
        if !self.armed[idx] {
            if pressed {
                return false;
            }
            self.armed[idx] = true;
        }
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

/// Analog sampler for a button whose pressed level is not a logic low.
///
/// A GPIO cannot be used for these: `ENC_BUTT` has been measured at ~2.4 V pressed
/// and `BUTT_1` at ~0.8 V *released* (both on rev3), far above a logic low (~1 V).
/// This converts an ADC reading to a pressed/released verdict with hysteresis, so
/// the stable-press debounce in [`InputHandler`] is unchanged.
///
/// One instance per button, each with its own thresholds — see
/// [`board::ENC_BTN_PRESSED_MV`] and [`board::BTN1_PRESSED_MV`].
pub struct AnalogButton {
    pressed_mv: u32,
    released_mv: u32,
    pressed: bool,
    last_mv: u32,
}

impl AnalogButton {
    /// `pressed_mv` is the level at or below which the button counts as pressed,
    /// `released_mv` the level at or above which it counts as released (must be
    /// the larger of the two; the band between them is hysteresis).
    pub const fn new(pressed_mv: u32, released_mv: u32) -> Self {
        Self {
            pressed_mv,
            released_mv,
            pressed: false,
            last_mv: 0,
        }
    }

    /// Feed one ADC sample (millivolts) and return the pressed state.
    ///
    /// * `<= pressed_mv` → pressed
    /// * `>= released_mv` → released
    /// * in between → the previous state is kept (hysteresis)
    ///
    /// The released level is the 3.3 V rail through the button's external pull-up;
    /// in analog mode the MCU's internal pull-up is off, so that resistor is what
    /// holds the line up. A reading of 0 mV therefore means "pressed with a healthy
    /// switch", not "pin floating".
    pub fn sample(&mut self, mv: u32) -> bool {
        if mv <= self.pressed_mv {
            self.pressed = true;
        } else if mv >= self.released_mv {
            self.pressed = false;
        }
        self.last_mv = mv;
        self.pressed
    }

    /// Most recent sample in millivolts, for the movement diagnostics.
    pub fn last_mv(&self) -> u32 {
        self.last_mv
    }

    /// Thresholds this instance compares against, for the movement log.
    pub fn thresholds_mv(&self) -> (u32, u32) {
        (self.pressed_mv, self.released_mv)
    }
}

impl Default for InputHandler {
    fn default() -> Self {
        Self::new()
    }
}
