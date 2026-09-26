//! Status LED (D31 on PA15): a very dim heartbeat while the supply is healthy,
//! and a blink code when it is not.
//!
//! # Hardware
//!
//! `PA15 → D31 (anode → cathode) → R81 → GND`, so PA15 **high** lights it. PA15
//! is `TIM2_CH1`, and TIM2 is free (TIM4 is the encoder QEI, TIM15 is the
//! Embassy time driver), so the LED runs off hardware PWM. A pattern therefore
//! only sets the on/off *envelope* at [`board::LED_TICK_MS`] resolution while
//! the PWM duty sets the brightness — the heartbeat stays dim and rock-steady
//! even when a blocking I2C timeout stalls the executor, which a bit-banged
//! software PWM could not manage.
//!
//! # Blink code table
//!
//! A code is a burst of `N` full-brightness flashes, then a long dark pause,
//! repeating until the condition clears. `N` maps to a condition:
//!
//! | `N` | Condition    | Source                                    |
//! |-----|--------------|-------------------------------------------|
//! | 1   | OVERCURRENT  | `Fault::OverCurrent`                      |
//! | 2   | OVERVOLTAGE  | `Fault::OverVoltage`                      |
//! | 3   | OVERPOWER    | `Fault::OverPower`                        |
//! | 4   | OVERTEMP     | `Fault::OverTemp`                         |
//! | 5   | IN OCP       | `Fault::InputOverCurrent`                 |
//! | 6   | IN OVP       | `Fault::InputOverVoltage`                 |
//! | 7   | INA228 FAULT | `!telemetry.ina_ok`                       |
//! | 8   | PD CTRL LOST | `!pd_present`                             |
//! | 9   | PD NO RAIL   | `pd_control.error == PdAutoError::NoRail` |
//! | 10  | EEPROM FAULT | EEPROM workflow error                     |
//!
//! Non-code states:
//!
//! * **Heartbeat** — no latched fault and every fitted subsystem is happy: two
//!   3 %-duty pulses ("thump-thump") about once every two seconds.
//! * **Activity** — an EEPROM write/verify is in progress: a single medium-duty
//!   pulse roughly twice a second.
//!
//! The latch is the *top* priority: a protection fault always shows its code,
//! even while the EEPROM screen is open. The subsystem codes (7–9) are held back
//! for [`board::LED_SUBSYSTEM_GRACE_MS`] after boot, and can be disabled
//! entirely with [`board::LED_SUBSYSTEM_CODES`], because the INA228 and TPS26750
//! are probed asynchronously and may legitimately not have answered yet.

use embassy_stm32::peripherals::TIM2;
use embassy_stm32::timer::simple_pwm::SimplePwmChannel;
use embassy_time::{Duration, Instant, Timer};

use crate::board;
use crate::runtime::AppStateMutex;
use crate::state::{AppState, Fault, MenuScreen, PdAutoError};

/// On time of one fault-code flash.
const CODE_ON_MS: u32 = 140;
/// Dark time between flashes inside a code burst.
const CODE_GAP_MS: u32 = 160;
/// Dark pause after a code burst, before it repeats.
const CODE_PAUSE_MS: u32 = 1_400;

/// Healthy heartbeat: two short, very dim pulses then a long quiet gap.
const HEARTBEAT: BlinkPattern = BlinkPattern {
    flashes: 2,
    on_ms: 70,
    gap_ms: 90,
    pause_ms: 1_900,
    duty_pct: board::LED_HEARTBEAT_DUTY_PCT,
};

/// EEPROM write/verify activity: one medium-duty pulse, ~2 Hz.
const ACTIVITY: BlinkPattern = BlinkPattern {
    flashes: 1,
    on_ms: 120,
    gap_ms: 0,
    pause_ms: 900,
    duty_pct: board::LED_ACTIVITY_DUTY_PCT,
};

/// What the LED should currently be saying. Derived from an `AppState` snapshot
/// plus a little boot-time context; see [`LedStatus::classify`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LedStatus {
    /// Everything fitted is present and no fault is latched.
    Heartbeat,
    /// The EEPROM flash workflow is writing or verifying.
    EepromBusy,
    /// A protection fault is latched.
    Fault(Fault),
    /// The INA228 input monitor is not answering.
    InaMissing,
    /// The TPS26750 PD controller is not answering.
    PdControllerLost,
    /// Source capabilities were read but none can serve the request.
    PdNoRail,
    /// The last EEPROM flash attempt failed.
    EepromError,
}

impl LedStatus {
    /// Pick the highest-priority indication for the current state.
    ///
    /// `subsystems_ready` is false for the first moments after boot (and when
    /// [`board::LED_SUBSYSTEM_CODES`] is off), which suppresses the "missing
    /// part" codes so a healthy power-up does not flash them.
    pub fn classify(app: &AppState, subsystems_ready: bool) -> Self {
        // Safety first: a latched protection fault always wins.
        if app.supply.fault != Fault::None {
            return LedStatus::Fault(app.supply.fault);
        }

        // EEPROM status only means anything while its screen is open; navigating
        // away stops the workflow being stepped, so the snapshot would otherwise
        // sit in `Flashing` forever.
        if app.ui.screen == MenuScreen::EepromFlash {
            if app.eeprom_ui.failed {
                return LedStatus::EepromError;
            }
            if app.eeprom_ui.busy {
                return LedStatus::EepromBusy;
            }
        }

        if subsystems_ready {
            if !app.telemetry.ina_ok {
                return LedStatus::InaMissing;
            }
            if !app.pd_present {
                return LedStatus::PdControllerLost;
            }
            if app.pd_control.error == PdAutoError::NoRail {
                return LedStatus::PdNoRail;
            }
        }

        LedStatus::Heartbeat
    }

    /// Blink code, or 0 for the non-code (heartbeat/activity) states.
    pub fn code(self) -> u8 {
        match self {
            LedStatus::Heartbeat | LedStatus::EepromBusy => 0,
            LedStatus::Fault(f) => fault_code(f),
            LedStatus::InaMissing => 7,
            LedStatus::PdControllerLost => 8,
            LedStatus::PdNoRail => 9,
            LedStatus::EepromError => 10,
        }
    }

    /// Human-readable name for the RTT log line that accompanies each change.
    pub fn label(self) -> &'static str {
        match self {
            LedStatus::Heartbeat => "heartbeat",
            LedStatus::EepromBusy => "EEPROM busy",
            LedStatus::Fault(Fault::OverCurrent) => "OVERCURRENT",
            LedStatus::Fault(Fault::OverVoltage) => "OVERVOLTAGE",
            LedStatus::Fault(Fault::OverPower) => "OVERPOWER",
            LedStatus::Fault(Fault::OverTemp) => "OVERTEMP",
            LedStatus::Fault(Fault::InputOverCurrent) => "IN OCP",
            LedStatus::Fault(Fault::InputOverVoltage) => "IN OVP",
            // `classify` never produces `Fault(None)`.
            LedStatus::Fault(Fault::None) => "no fault",
            LedStatus::InaMissing => "INA228 FAULT",
            LedStatus::PdControllerLost => "PD CTRL LOST",
            LedStatus::PdNoRail => "PD NO RAIL",
            LedStatus::EepromError => "EEPROM FAULT",
        }
    }

    /// The envelope this status plays.
    pub fn pattern(self) -> BlinkPattern {
        match self {
            LedStatus::Heartbeat => HEARTBEAT,
            LedStatus::EepromBusy => ACTIVITY,
            LedStatus::Fault(f) => BlinkPattern::code(fault_code(f), board::LED_FAULT_DUTY_PCT),
            LedStatus::InaMissing => BlinkPattern::code(7, board::LED_FAULT_DUTY_PCT),
            LedStatus::PdControllerLost => BlinkPattern::code(8, board::LED_FAULT_DUTY_PCT),
            LedStatus::PdNoRail => BlinkPattern::code(9, board::LED_FAULT_DUTY_PCT),
            LedStatus::EepromError => BlinkPattern::code(10, board::LED_FAULT_DUTY_PCT),
        }
    }
}

/// Map a latched protection fault to its blink-code flash count.
fn fault_code(fault: Fault) -> u8 {
    match fault {
        Fault::OverCurrent => 1,
        Fault::OverVoltage => 2,
        Fault::OverPower => 3,
        Fault::OverTemp => 4,
        Fault::InputOverCurrent => 5,
        Fault::InputOverVoltage => 6,
        // Not reachable through `classify`, which tests for `None` first.
        Fault::None => 0,
    }
}

/// A repeating LED envelope: `flashes` lit pulses of `on_ms` at `duty_pct`,
/// separated by `gap_ms` of dark, then `pause_ms` of dark before it repeats.
/// `flashes == 0` keeps the LED off.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BlinkPattern {
    pub flashes: u8,
    pub on_ms: u32,
    pub gap_ms: u32,
    pub pause_ms: u32,
    pub duty_pct: u8,
}

impl BlinkPattern {
    /// LED held off.
    pub const OFF: Self = Self {
        flashes: 0,
        on_ms: 0,
        gap_ms: 0,
        pause_ms: 0,
        duty_pct: 0,
    };

    /// A standard fault-code burst of `flashes` pulses.
    const fn code(flashes: u8, duty_pct: u8) -> Self {
        Self {
            flashes,
            on_ms: CODE_ON_MS,
            gap_ms: CODE_GAP_MS,
            pause_ms: CODE_PAUSE_MS,
            duty_pct,
        }
    }
}

/// Phase inside one burst.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// A pulse is lit.
    On,
    /// Dark gap between pulses of the same burst.
    Gap,
    /// Dark pause after the last pulse.
    Pause,
}

/// Non-blocking envelope generator: feed it the desired pattern every tick and
/// it returns the duty to apply. It only needs the elapsed tick length, so the
/// task can be late without desynchronising the pattern.
pub struct LedEngine {
    pattern: BlinkPattern,
    phase: Phase,
    /// Pulses completed in the current burst.
    pulses: u8,
    /// Milliseconds left in the current phase.
    remaining_ms: u32,
}

impl LedEngine {
    pub const fn new() -> Self {
        Self {
            pattern: BlinkPattern::OFF,
            phase: Phase::Pause,
            pulses: 0,
            remaining_ms: 0,
        }
    }

    /// Advance by `tick_ms` toward `pattern` and return the duty (percent) for
    /// the next tick. A change of pattern restarts the burst immediately.
    pub fn step(&mut self, pattern: BlinkPattern, tick_ms: u32) -> u8 {
        if pattern.flashes == 0 {
            self.pattern = pattern;
            self.phase = Phase::Pause;
            self.pulses = 0;
            self.remaining_ms = u32::MAX;
            return 0;
        }

        if pattern != self.pattern {
            self.pattern = pattern;
            self.phase = Phase::On;
            self.pulses = 0;
            self.remaining_ms = pattern.on_ms;
        }

        // Consume the tick, crossing phase boundaries as needed. The guard stops
        // a zero-length phase from spinning (and the tick is far shorter than
        // any phase anyway).
        self.remaining_ms = self.remaining_ms.saturating_sub(tick_ms);
        let mut guard = 0;
        while self.remaining_ms == 0 && guard < 8 {
            self.advance();
            guard += 1;
        }
        self.duty()
    }

    /// Move to the next phase, saturating the pulse count at the burst length.
    fn advance(&mut self) {
        match self.phase {
            Phase::On => {
                self.pulses = self.pulses.saturating_add(1);
                if self.pulses >= self.pattern.flashes {
                    self.phase = Phase::Pause;
                    self.remaining_ms = self.pattern.pause_ms;
                } else {
                    self.phase = Phase::Gap;
                    self.remaining_ms = self.pattern.gap_ms;
                }
            }
            Phase::Gap => {
                self.phase = Phase::On;
                self.remaining_ms = self.pattern.on_ms;
            }
            Phase::Pause => {
                self.pulses = 0;
                self.phase = Phase::On;
                self.remaining_ms = self.pattern.on_ms;
            }
        }
    }

    fn duty(&self) -> u8 {
        match self.phase {
            Phase::On => self.pattern.duty_pct,
            Phase::Gap | Phase::Pause => 0,
        }
    }
}

impl Default for LedEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Embassy task driving the status LED.
///
/// Snapshots `APP_STATE` every [`board::LED_TICK_MS`] and never holds the lock
/// (snapshot-then-drive), matching the crate-wide lock order in
/// [`crate::runtime`]. Any status change is logged on RTT so the blink code has
/// a matching human-readable message.
#[embassy_executor::task]
pub async fn led_task(app_state: &'static AppStateMutex, mut pwm: SimplePwmChannel<'static, TIM2>) {
    let tick = Duration::from_millis(board::LED_TICK_MS);
    let tick_ms = board::LED_TICK_MS as u32;

    pwm.set_duty_cycle_fully_off();
    pwm.enable();

    let boot = Instant::now();
    let mut engine = LedEngine::new();
    let mut last: Option<LedStatus> = None;
    let mut next = Instant::now();

    loop {
        Timer::at(next).await;
        next += tick;

        let ready = board::LED_SUBSYSTEM_CODES
            && boot.elapsed() >= Duration::from_millis(board::LED_SUBSYSTEM_GRACE_MS);
        let status = {
            let app = app_state.lock().await;
            LedStatus::classify(&app, ready)
        };

        if Some(status) != last {
            let code = status.code();
            if code == 0 {
                defmt::info!("LED: {} (duty {}%)", status.label(), status.pattern().duty_pct);
            } else {
                defmt::warn!("LED: {} (code {})", status.label(), code);
            }
            last = Some(status);
        }

        pwm.set_duty_cycle_percent(engine.step(status.pattern(), tick_ms));
    }
}
