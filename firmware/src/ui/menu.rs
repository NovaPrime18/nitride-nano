//! Menu model: maps [`InputEvent`]s to [`AppState`] mutations per screen.
//!
//! Navigation: Main → CcLimit → PdContract → Settings, then BTN3 unwinds back to
//! Main. Settings (`CFG`) is a fullscreen scrollable list of [`CFG_ITEMS`]:
//! encode-turn moves the highlight, the encoder button (or BTN1) activates the
//! entry, BTN3 returns to Main. The list opens the EEPROM flash screen or arms
//! the output-voltage sweep.
//!
//! Main-screen controls:
//! - **BTN1** cycles the selected regulation mode (CV ↔ CC).
//! - **BTN2** enables/disables the converter; while a fault is latched the first
//!   press clears the latch and keeps the output off, so recovery stays a
//!   deliberate two-step action.
//! - **BTN3** moves on to the I-LIM screen and clears VSET mode.
//! - **Encoder single click** toggles VSET mode: while on, the status line reads
//!   `>V-SET` + `v_set`, the header shows Fine/Coarse, and encoder rotation trims
//!   the setpoint with the active step. Rotation is ignored outside VSET mode.
//! - **Encoder double click** toggles Fine/Coarse while VSET mode is on. The
//!   single click is deferred by [`crate::board::ENC_DOUBLE_CLICK_MS`] so the two
//!   gestures never overlap (see [`crate::ui::input`]).
//!
//! The sweep is a mode of the Main screen: selecting it returns to Main in
//! [`SweepPhase::Armed`] and the bottom line prompts for the encoder button.
//! While armed/running the Main screen's input is owned by the sweep: the encoder
//! single click starts it, and any other button press (including a double click)
//! cancels and parks the output. [`SweepPhase::Done`] is dismissed by any button.
//! Encoder rotation is ignored in every non-`Off` sweep phase.
//!
//! On the PD screen, BTN1 toggles Auto-tracking PD, BTN2 switches its
//! efficiency/power policy, the encoder steps presets in manual mode, and the
//! encoder button confirms.

use crate::board;
use crate::state::{
    AppState, AutoPolicy, CfgItem, Fault, MenuScreen, PdMode, StepMode, SupplyMode, SweepPhase,
    CFG_ITEMS, CFG_VISIBLE_ROWS, PD_PRESET_VOLTAGES_MV,
};
use crate::ui::input::InputEvent;

/// Dispatch one input event according to the active screen.
pub fn apply_input(app: &mut AppState, ev: InputEvent) {
    match app.ui.screen {
        MenuScreen::Main => {
            // A pending/running/finished sweep owns the Main screen's input.
            if app.sweep.phase != SweepPhase::Off {
                handle_sweep_input(app, ev);
                return;
            }
            match ev {
                InputEvent::Btn3 => {
                    // Move on to I-LIM; leaving Main always clears VSET mode and
                    // restores the fine step, exactly as the old V-SET screen did.
                    app.ui.vset_mode = false;
                    app.ui.encoder_step_mode = StepMode::Fine;
                    app.ui.screen = MenuScreen::CcLimit;
                }
                InputEvent::Btn1 => {
                    // BTN1 toggles the selected regulation mode; the badge then
                    // shows CV/CC even while the output is off so the choice is
                    // visible before enabling.
                    app.supply.mode = match app.supply.mode {
                        SupplyMode::Cv => SupplyMode::Cc,
                        _ => SupplyMode::Cv,
                    };
                }
                InputEvent::Btn2 => {
                    if app.supply.fault != Fault::None {
                        // Clear the latch only. The output was forced off when the
                        // fault tripped; keep it off so re-enabling is a separate,
                        // deliberate press instead of a side effect of clearing
                        // the fault.
                        app.supply.fault = Fault::None;
                        app.supply.enabled = false;
                    } else {
                        app.supply.enabled = !app.supply.enabled;
                    }
                }
                InputEvent::EncBtn => {
                    // Single click: toggle VSET mode. Turning it off restores the
                    // fine step, so re-entering always starts precise.
                    app.ui.vset_mode = !app.ui.vset_mode;
                    if !app.ui.vset_mode {
                        app.ui.encoder_step_mode = StepMode::Fine;
                    }
                }
                InputEvent::EncDoubleClick => {
                    // Only meaningful while editing; ignored on the bare Main
                    // screen.
                    if app.ui.vset_mode {
                        app.ui.encoder_step_mode = match app.ui.encoder_step_mode {
                            StepMode::Fine => StepMode::Coarse,
                            StepMode::Coarse => StepMode::Fine,
                        };
                    }
                }
                InputEvent::EncTurn(d) => {
                    // Only VSET mode trims the setpoint; a stray turn on the bare
                    // Main screen must not perturb an applied output.
                    if app.ui.vset_mode && d != 0 {
                        let step = match app.ui.encoder_step_mode {
                            StepMode::Fine => 100u32,     // 100 mV per click
                            StepMode::Coarse => 1_000u32, // 1 V per click
                        };
                        if d > 0 {
                            app.supply.v_set_mv =
                                (app.supply.v_set_mv + step).min(board::VOUT_MAX_MV);
                        } else {
                            app.supply.v_set_mv = app.supply.v_set_mv.saturating_sub(step);
                        }
                    }
                }
            }
        }
        MenuScreen::CcLimit => {
            let step = match app.ui.encoder_step_mode {
                StepMode::Fine => 100u32,     // 100 mA per click
                StepMode::Coarse => 1_000u32, // 1 A per click
            };
            adjust_current_dynamic(app, ev, step);
            if ev == InputEvent::Btn2 {
                // Output on/off without leaving the screen (see Main's BTN2).
                app.supply.enabled = !app.supply.enabled;
            }
            if ev == InputEvent::EncBtn {
                app.ui.encoder_step_mode = match app.ui.encoder_step_mode {
                    StepMode::Fine => StepMode::Coarse,
                    StepMode::Coarse => StepMode::Fine,
                };
            }
            if ev == InputEvent::Btn3 {
                app.ui.encoder_step_mode = StepMode::Fine; // Reset on exit
                app.ui.screen = MenuScreen::PdContract; // Move to the next setting
            }
        }
        MenuScreen::PdContract => match ev {
            InputEvent::EncTurn(d) => {
                // Only manual mode steps through the preset grid; in Auto the
                // rail is derived from the output setpoint. `d` is in detents,
                // so a fast flick moves several cells and wraps. Moving off MAX
                // leaves MAX mode.
                if app.pd_control.mode == PdMode::Manual && d != 0 {
                    let n = PD_PRESET_VOLTAGES_MV.len() as i32;
                    let next = (app.ui.pd_profile_index as i32 + d as i32).rem_euclid(n);
                    app.ui.pd_profile_index = next as u8;
                    app.pd_control.max_request = false;
                }
            }
            InputEvent::Btn1 => {
                // Toggle Auto-tracking PD on/off (leaving Manual drops MAX).
                app.pd_control.mode = match app.pd_control.mode {
                    PdMode::Manual => PdMode::Auto,
                    PdMode::Auto => PdMode::Manual,
                };
                app.pd_control.max_request = false;
                app.pd_control.renegotiate_request = true;
            }
            InputEvent::Btn2 => {
                match app.pd_control.mode {
                    // Manual: MAX — request the highest rail the source offers
                    // (highest EPR PDO when EPR is available, else the highest
                    // SPR PDO).
                    PdMode::Manual => {
                        app.pd_control.max_request = true;
                        app.pd_control.renegotiate_request = true;
                    }
                    // Auto: flip the efficiency/power policy.
                    PdMode::Auto => {
                        app.pd_control.policy = match app.pd_control.policy {
                            AutoPolicy::Efficiency => AutoPolicy::Power,
                            AutoPolicy::Power => AutoPolicy::Efficiency,
                        };
                        app.pd_control.renegotiate_request = true;
                    }
                }
            }
            InputEvent::Btn3 => {
                app.ui.screen = MenuScreen::Settings;
            }
            InputEvent::EncBtn => {
                // Confirm: request the selected preset (manual) or re-evaluate
                // the derived rail (auto).
                app.pd_control.renegotiate_request = true;
            }
            // The double click is a Main-screen gesture only.
            InputEvent::EncDoubleClick => {}
        },
        MenuScreen::Settings => match ev {
            InputEvent::EncTurn(d) => cfg_move_selection(app, d),
            InputEvent::EncBtn | InputEvent::Btn1 => cfg_activate(app),
            InputEvent::Btn3 => {
                app.ui.screen = MenuScreen::Main;
            }
            InputEvent::Btn2 | InputEvent::EncDoubleClick => {}
        },
        MenuScreen::EepromFlash => match ev {
            InputEvent::Btn2 | InputEvent::Btn3 => {
                app.ui.screen = MenuScreen::Main;
            }
            _ => {}
        },
    }
}

/// Encoder-turn handler for the I-LIM screen; ignores all other events.
///
/// The settable range is bounded by the LT8390A's CC window, not by the ADC:
/// below [`board::CC_SET_MIN_MA`] the chip cannot limit current (it stops
/// switching), and [`board::CC_SET_MAX_MA`] is the design maximum.
fn adjust_current_dynamic(app: &mut AppState, ev: InputEvent, step: u32) {
    match ev {
        InputEvent::EncTurn(d) => {
            if d > 0 {
                app.supply.i_set_ma = (app.supply.i_set_ma + step).min(board::CC_SET_MAX_MA);
            } else {
                app.supply.i_set_ma = app.supply.i_set_ma.saturating_sub(step);
            }
            app.supply.i_set_ma = app
                .supply
                .i_set_ma
                .max(board::CC_SET_MIN_MA)
                .min(board::CC_SET_MAX_MA);
        }
        _ => {}
    }
}

/// Main-screen input while a CFG sweep is armed, running, or finished.
///
/// The encoder button on [`SweepPhase::Armed`] starts the sweep (the controller
/// consumes `start_request` on the next supply tick). Any other button press
/// cancels an armed/running sweep and parks the output, or dismisses the
/// completed state. Encoder rotation is ignored so a stray turn cannot perturb
/// an armed or running sweep.
fn handle_sweep_input(app: &mut AppState, ev: InputEvent) {
    match (app.sweep.phase, ev) {
        (SweepPhase::Armed, InputEvent::EncBtn) => {
            app.sweep.start_request = true;
        }
        (_, InputEvent::EncTurn(_)) => {}
        (SweepPhase::Running, _) | (SweepPhase::Armed, _) => {
            app.sweep.phase = SweepPhase::Off;
            app.sweep.start_request = false;
            app.supply.enabled = false;
        }
        (SweepPhase::Done, _) => {
            app.sweep.phase = SweepPhase::Off;
        }
        // The caller only routes here when the phase is not `Off`.
        (SweepPhase::Off, _) => {}
    }
}

/// Move the CFG list highlight by one encoder detent, wrapping at both ends and
/// scrolling the viewport to keep the selection visible.
fn cfg_move_selection(app: &mut AppState, delta: i16) {
    let count = CFG_ITEMS.len() as u8;
    if count == 0 || delta == 0 {
        return;
    }

    let current = app.ui.cfg_index.min(count - 1);
    let next = if delta > 0 {
        (current + 1) % count
    } else if current == 0 {
        count - 1
    } else {
        current - 1
    };
    app.ui.cfg_index = next;

    if next < app.ui.cfg_scroll {
        app.ui.cfg_scroll = next;
    } else if next >= app.ui.cfg_scroll.saturating_add(CFG_VISIBLE_ROWS) {
        app.ui.cfg_scroll = next.saturating_sub(CFG_VISIBLE_ROWS) + 1;
    }
}

/// Activate the highlighted CFG list entry.
fn cfg_activate(app: &mut AppState) {
    let item = CFG_ITEMS
        .get(app.ui.cfg_index as usize)
        .copied()
        .unwrap_or(CFG_ITEMS[0]);

    match item {
        CfgItem::EepromWrite => {
            app.ui.screen = MenuScreen::EepromFlash;
        }
        CfgItem::OutputSweep => {
            // Hand back to Main and wait for the encoder-button confirmation.
            app.sweep.phase = SweepPhase::Armed;
            app.sweep.index = 0;
            app.sweep.start_request = false;
            app.ui.screen = MenuScreen::Main;
        }
    }
}
