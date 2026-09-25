//! Embassy task driving the OLED at [`crate::board::UI_REFRESH_MS`] intervals.

use embassy_time::{Duration, Instant, Timer};

use crate::board;
use crate::drivers::ssd1306_ui::Ssd1306Ui;
use crate::runtime::{AppStateMutex, I2cBusMutex};
use crate::state::MenuScreen;

/// Render loop: clones the shared state, then renders the active screen.
///
/// The `APP_STATE` lock is dropped before `I2C_UI_BUS` is taken
/// (snapshot-then-render), which keeps the critical section short and follows
/// the crate-wide lock order documented in [`crate::runtime`]. Draw errors are
/// swallowed — a NACKed frame just shows up as a one-frame glitch and the next
/// tick retries.
#[embassy_executor::task]
pub async fn ui_task(app_state: &'static AppStateMutex, i2c_bus: &'static I2cBusMutex) {
    let mut ui = Ssd1306Ui::new();
    let period = Duration::from_millis(board::UI_REFRESH_MS);

    // First init attempt. A missing or slow panel must not panic the power-supply
    // firmware, so log the failure and keep retrying in the loop below.
    let mut display_ready = {
        let mut i2c = i2c_bus.lock().await;
        ui.init(&mut i2c).await.is_ok()
    };
    if display_ready {
        defmt::info!("Display init OK");
    } else {
        defmt::error!("SSD1306 init failed; running headless, will retry");
    }

    let mut next = Instant::now();
    let mut next_init = Instant::now() + Duration::from_secs(1);
    // Resync the panel on the very first paint (covers soft-reset cursor garbage
    // when the panel wasn't power-cycled) and again on every screen change, so
    // no stale pixels from the previous layout survive into the next one.
    let mut last_screen: Option<MenuScreen> = None;
    loop {
        Timer::at(next).await;
        next += period;

        if !display_ready {
            // Retry at most once a second: `init` blocks ~50 ms on the panel
            // power-on delay, and a dead panel must not dominate the UI task.
            if Instant::now() >= next_init {
                next_init = Instant::now() + Duration::from_secs(1);
                let mut i2c = i2c_bus.lock().await;
                display_ready = ui.init(&mut i2c).await.is_ok();
                if display_ready {
                    defmt::info!("Display init OK");
                }
                // Don't try to catch up on the refresh cadence after a long init.
                next = Instant::now() + period;
            }
            continue;
        }

        let app = app_state.lock().await.clone();
        if last_screen != Some(app.ui.screen) {
            ui.invalidate();
            last_screen = Some(app.ui.screen);
        }
        let mut i2c = i2c_bus.lock().await;

        match app.ui.screen {
            MenuScreen::EepromFlash => {
                ui.draw_eeprom_screen(
                    &mut i2c,
                    &app,
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
            MenuScreen::Settings => {
                ui.draw_cfg_screen(&mut i2c, &app).await.ok();
            }
            _ => {
                ui.draw_power_screen(&mut i2c, &app).await.ok();
            }
        }
    }
}
