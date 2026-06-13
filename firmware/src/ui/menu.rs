use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;

use crate::drivers::ssd1306_ui::Ssd1306Ui;
use crate::state::{AppState, Fault, MenuScreen, PD_PRESET_VOLTAGES_MV, StepMode, SupplyMode};
use crate::ui::input::InputEvent;

pub struct UiTask;

impl UiTask {
    pub async fn draw(ui: &mut Ssd1306Ui, i2c: &mut I2c<'_, Async, Master>, app: &AppState) {
        let _ = ui.draw_power_screen(i2c, app).await;
    }
}

pub fn apply_input(app: &mut AppState, ev: InputEvent) {
    match app.ui.screen {
        MenuScreen::Main => match ev {
            InputEvent::Btn3 => {
                app.ui.screen = MenuScreen::CvSetpoint;
            }
            InputEvent::Btn1 => {
                app.supply.mode = SupplyMode::Cv;
            }
            InputEvent::Btn2 => {
                app.supply.mode = SupplyMode::Cc;
            }
            InputEvent::EncBtn => {
                if app.supply.fault != Fault::None {
                    app.supply.fault = Fault::None; // Clear the fault state
                    app.supply.enabled = !app.supply.enabled; // Normal toggle
                } else {
                    app.supply.enabled = !app.supply.enabled; // Normal toggle
                }
            }
            InputEvent::EncTurn(d) => {
                if d > 0 {
                    app.supply.v_set_mv = (app.supply.v_set_mv + 10).min(60_000);
                } else {
                    app.supply.v_set_mv = app.supply.v_set_mv.saturating_sub(10);
                }
            }
        },
        // Inside menu.rs -> apply_input -> MenuScreen::CvSetpoint
        MenuScreen::CvSetpoint => {
            let step = match app.ui.encoder_step_mode {
                StepMode::Fine => 100u32,    // 100 mV per click
                StepMode::Coarse => 1_000u32, // 1 V per click
            };
            adjust_voltage_dynamic(app, ev, step);
            if ev == InputEvent::EncBtn {
                app.ui.encoder_step_mode = match app.ui.encoder_step_mode {
                    StepMode::Fine => StepMode::Coarse,
                    StepMode::Coarse => StepMode::Fine,
                };
            }
            if ev == InputEvent::Btn3 {
                app.ui.encoder_step_mode = StepMode::Fine; // Reset on exit
                app.ui.screen = MenuScreen::CcLimit; // Move to the next setting
            }
        }
        MenuScreen::CcLimit => {
            let step = match app.ui.encoder_step_mode {
                StepMode::Fine => 100u32,    // 100 mA per click
                StepMode::Coarse => 1_000u32, // 1 A per click
            };
            adjust_current_dynamic(app, ev, step);
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
                if d > 0 {
                    app.ui.pd_profile_index = (app.ui.pd_profile_index + 1) % PD_PRESET_VOLTAGES_MV.len() as u8;
                } else if d < 0 {
                    app.ui.pd_profile_index = if app.ui.pd_profile_index > 0 {
                        app.ui.pd_profile_index - 1
                    } else {
                        (PD_PRESET_VOLTAGES_MV.len() - 1) as u8
                    };
                }
            }
            InputEvent::Btn3 => {
                app.ui.screen = MenuScreen::Settings;
            }
            InputEvent::EncBtn => {
                // Confirm: request the selected PD contract via I2C
                app.ui.screen = MenuScreen::Main;
            }
            _ => {}
        },
        MenuScreen::Settings => match ev {
            InputEvent::Btn1 | InputEvent::EncBtn => {
                app.ui.screen = MenuScreen::PdContract;
            }
            InputEvent::Btn2 => {
                app.ui.screen = MenuScreen::EepromFlash;
            }
            InputEvent::Btn3 => {
                app.ui.screen = MenuScreen::Main;
            }
            _ => {}
        },
        MenuScreen::EepromFlash => match ev {
            InputEvent::Btn2 | InputEvent::Btn3 => {
                app.ui.screen = MenuScreen::Main;
            }
            _ => {}
        },
    }
}

fn adjust_voltage_dynamic(app: &mut AppState, ev: InputEvent, step: u32) {
    match ev {
        InputEvent::EncTurn(d) => {
            if d > 0 {
                app.supply.v_set_mv = (app.supply.v_set_mv + step).min(60_000);
            } else {
                app.supply.v_set_mv = app.supply.v_set_mv.saturating_sub(step);
            }
        }
        _ => {}
    }
}

fn adjust_current_dynamic(app: &mut AppState, ev: InputEvent, step: u32) {
    match ev {
        InputEvent::EncTurn(d) => {
            if d > 0 {
                app.supply.i_set_ma = (app.supply.i_set_ma + step).min(20_000);
            } else {
                app.supply.i_set_ma = app.supply.i_set_ma.saturating_sub(step);
            }
        }
        _ => {}
    }
}
