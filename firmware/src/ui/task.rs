//! Embassy task driving the OLED at [`crate::board::UI_REFRESH_MS`] intervals.

use embassy_time::{Duration, Instant, Timer};

use crate::board;
use crate::drivers::ssd1306_ui::Ssd1306Ui;
use crate::runtime::{AppStateMutex, I2cBusMutex};
use crate::state::MenuScreen;

/// Render loop: clones the shared state, then renders the active screen.
///
/// The `APP_STATE` lock is dropped before `I2C_BUS` is taken (snapshot-then-render),
/// which keeps the critical section short and follows the crate-wide lock order
/// documented in [`crate::runtime`]. Draw errors are swallowed — a NACKed frame
/// just shows up as a one-frame glitch and the next tick retries.
#[embassy_executor::task]
pub async fn ui_task(app_state: &'static AppStateMutex, i2c_bus: &'static I2cBusMutex) {
    let mut ui = Ssd1306Ui::new();
    {
        let mut i2c = i2c_bus.lock().await;
        ui.init(&mut i2c).await.expect("SSD1306 init");
    }
    defmt::info!("Display init OK");

    let period = Duration::from_millis(board::UI_REFRESH_MS);
    let mut next = Instant::now();
    loop {
        Timer::at(next).await;
        next += period;

        let app = app_state.lock().await.clone();
        let mut i2c = i2c_bus.lock().await;

        match app.ui.screen {
            MenuScreen::EepromFlash => {
                ui.draw_eeprom_screen(
                    &mut i2c,
                    app.eeprom_ui.title,
                    app.eeprom_ui.message,
                    app.eeprom_ui.progress_percent,
                )
                .await
                .ok();
            }
            MenuScreen::PdContract => {
                ui.draw_pd_contract_screen(&mut i2c, &app).await.ok();
            }
            _ => {
                ui.draw_power_screen(&mut i2c, &app).await.ok();
            }
        }
    }
}
