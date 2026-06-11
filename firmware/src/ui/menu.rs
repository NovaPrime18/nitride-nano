use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;

use crate::drivers::ssd1306_ui::Ssd1306Ui;
use crate::state::{AppState, Fault, MenuScreen, SupplyMode};
use crate::ui::input::InputEvent;

pub struct UiTask;

impl UiTask {
    pub async fn draw(ui: &mut Ssd1306Ui, i2c: &mut I2c<'_, Async, Master>, app: &AppState) {
        let _ = ui.draw_power_screen(i2c, app).await;
    }
}

pub fn apply_input(app: &mut AppState, ev: InputEvent) {
    let step_coarse_v: u32 = 100;
    let step_fine_v: u32 = 10;
    let step_coarse_i: u32 = 100;
    let step_fine_i: u32 = 10;

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
                let step = step_fine_v;
                if d > 0 {
                    app.supply.v_set_mv = (app.supply.v_set_mv + step).min(60_000);
                } else {
                    app.supply.v_set_mv = app.supply.v_set_mv.saturating_sub(step);
                }
            }
        },
        // Inside menu.rs -> apply_input -> MenuScreen::CvSetpoint
        MenuScreen::CvSetpoint => {
            adjust_voltage(app, ev, step_coarse_v, step_fine_v);
            if ev == InputEvent::Btn3 {
                app.ui.screen = MenuScreen::CcLimit; // Move to the next setting
            }
        }
        MenuScreen::CcLimit => {
            adjust_current(app, ev, step_coarse_i, step_fine_i);
            if ev == InputEvent::Btn3 {
                app.ui.screen = MenuScreen::Enable; // Move to the next setting
            }
        }
        MenuScreen::Enable => match ev {
            InputEvent::Btn1 | InputEvent::EncBtn => {
                app.supply.enabled = true;
            }
            InputEvent::Btn2 => {
                app.supply.enabled = false;
            }
            InputEvent::Btn3 => {
                app.ui.screen = MenuScreen::PdProfile;
            }
            _ => {}
        },
        MenuScreen::PdProfile => match ev {
            InputEvent::EncTurn(d) => {
                if app.pd_cap_count > 0 {
                    if d > 0 {
                        app.ui.pd_profile_index = (app.ui.pd_profile_index + 1) % app.pd_cap_count;
                    } else if d < 0 {
                        app.ui.pd_profile_index = app.ui.pd_profile_index.saturating_sub(1);
                    }
                }
            }
            InputEvent::Btn3 => {
                app.ui.screen = MenuScreen::Settings;
            }
            InputEvent::EncBtn => {
                app.ui.screen = MenuScreen::Main;
            }
            _ => {}
        },
        MenuScreen::Settings => match ev {
            InputEvent::Btn1 | InputEvent::EncBtn => {
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

fn adjust_voltage(app: &mut AppState, ev: InputEvent, coarse: u32, fine: u32) {
    match ev {
        InputEvent::Btn1 => {
            app.supply.v_set_mv = (app.supply.v_set_mv + coarse).min(60_000);
        }
        InputEvent::Btn2 => {
            app.supply.v_set_mv = app.supply.v_set_mv.saturating_sub(coarse);
        }
        InputEvent::EncTurn(d) => {
            if d > 0 {
                app.supply.v_set_mv = (app.supply.v_set_mv + fine).min(60_000);
            } else {
                app.supply.v_set_mv = app.supply.v_set_mv.saturating_sub(fine);
            }
        }
        _ => {}
    }
}

fn adjust_current(app: &mut AppState, ev: InputEvent, coarse: u32, fine: u32) {
    match ev {
        InputEvent::Btn1 => {
            app.supply.i_set_ma = (app.supply.i_set_ma + coarse).min(20_000);
        }
        InputEvent::Btn2 => {
            app.supply.i_set_ma = app.supply.i_set_ma.saturating_sub(coarse);
        }
        InputEvent::EncTurn(d) => {
            if d > 0 {
                app.supply.i_set_ma = (app.supply.i_set_ma + fine).min(20_000);
            } else {
                app.supply.i_set_ma = app.supply.i_set_ma.saturating_sub(fine);
            }
        }
        _ => {}
    }
}
