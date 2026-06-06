use embassy_stm32::gpio::Input;
use embassy_time::{Duration, Instant};

use crate::board;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Btn1,
    Btn2,
    Btn3,
    EncBtn,
    EncTurn(i16),
}

pub struct InputHandler {
    pub last_event: Option<InputEvent>,
    pub encoder_delta: i16,
    debounce: [Instant; 4],
    held: [bool; 4],
}

impl InputHandler {
    pub fn new() -> Self {
        Self {
            last_event: None,
            encoder_delta: 0,
            debounce: [Instant::now(); 4],
            held: [false; 4],
        }
    }

    pub fn poll(
        &mut self,
        btn1: &Input<'_>,
        btn2: &Input<'_>,
        btn3: &Input<'_>,
        enc_btn: &Input<'_>,
        enc_delta: i16,
    ) {
        self.encoder_delta = enc_delta;
        if enc_delta != 0 {
            self.last_event = Some(InputEvent::EncTurn(enc_delta));
        }
        self.check_button(0, btn1.is_low(), InputEvent::Btn1);
        self.check_button(1, btn2.is_low(), InputEvent::Btn2);
        self.check_button(2, btn3.is_low(), InputEvent::Btn3);
        self.check_button(3, enc_btn.is_low(), InputEvent::EncBtn);
    }

    fn check_button(&mut self, idx: usize, pressed: bool, ev: InputEvent) {
        let now = Instant::now();
        if pressed && !self.held[idx] {
            if now.duration_since(self.debounce[idx]) >= Duration::from_millis(board::DEBOUNCE_MS)
            {
                self.last_event = Some(ev);
                self.held[idx] = true;
                self.debounce[idx] = now;
            }
        } else if !pressed {
            self.held[idx] = false;
        }
    }

    pub fn clear_event(&mut self) {
        self.last_event = None;
        self.encoder_delta = 0;
    }
}

impl Default for InputHandler {
    fn default() -> Self {
        Self::new()
    }
}
