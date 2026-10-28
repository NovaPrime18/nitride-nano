//! Embassy task driving the OLED at [`crate::board::UI_REFRESH_MS`] intervals.

use embassy_time::{Duration, Instant, Timer};

use crate::board;
use crate::drivers::ssd1306_ui::Ssd1306Ui;
use crate::runtime::{AppStateMutex, I2cBusMutex};
use crate::state::MenuScreen;

/// First delay between panel init attempts while the display is not answering.
///
/// Short, because the usual reason for a failed init is a panel that was not
/// ready yet (see [`ui_task`]); it doubles on every failure up to
/// [`RETRY_MAX_MS`] so a genuinely absent panel cannot cost a blocking I2C
/// timeout on every refresh tick.
const RETRY_FIRST_MS: u64 = 500;
const RETRY_MAX_MS: u64 = 5_000;

/// Extra init rounds issued after a *successful* init, and the spacing between
/// them.
///
/// An SSD1306 that is still inside its power-on reset ACKs its address but
/// ignores the commands, so `init` reports success on a panel that stays dark.
/// Re-sending the sequence once the panel is really up is the only cure, and at
/// these two points (0.5 s and 2 s after the first init) it is invisible: the
/// sequence ends with display-on and the frame is repainted immediately after.
const WAKEUP_ROUNDS: u8 = 2;
const WAKEUP_INTERVAL_MS: u64 = 1_500;

/// Render loop: clones the shared state, then renders the active screen.
///
/// The `APP_STATE` lock is dropped before `I2C_UI_BUS` is taken
/// (snapshot-then-render), which keeps the critical section short and follows
/// the crate-wide lock order documented in [`crate::runtime`]. Draw errors are
/// not swallowed: a frame that did not make it out means the panel or the bus
/// is unhappy, so the loop drops back to the init/retry path and repaints the
/// whole screen once it is back, instead of leaving a blank panel lit forever.
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
    let mut retry_ms = RETRY_FIRST_MS;
    let mut next_init = Instant::now() + Duration::from_millis(retry_ms);
    // Post-init wake-up schedule (see `WAKEUP_ROUNDS`); only runs while the
    // panel is answering.
    let mut wakeups_left = WAKEUP_ROUNDS;
    let mut next_wakeup = Instant::now() + Duration::from_millis(RETRY_FIRST_MS);
    // Resync the panel on the very first paint (covers soft-reset cursor garbage
    // when the panel wasn't power-cycled) and again on every screen change, so
    // no stale pixels from the previous layout survive into the next one.
    let mut last_screen: Option<MenuScreen> = None;

    loop {
        Timer::at(next).await;
        next += period;

        if !display_ready {
            // Retry with a backoff: the very first attempts are close together
            // (a panel that missed its power-on window only needs a second
            // chance), later ones are sparse, because each failed attempt can
            // block for the UI bus timeout and a dead panel must not dominate
            // the UI task.
            if Instant::now() >= next_init {
                let mut i2c = i2c_bus.lock().await;
                display_ready = ui.init(&mut i2c).await.is_ok();
                if display_ready {
                    defmt::info!("Display init OK");
                    // A recovered panel that hiccups later should come back
                    // quickly, so the backoff restarts from the bottom.
                    retry_ms = RETRY_FIRST_MS;
                    // The panel only just started answering: give it the same
                    // wake-up rounds as a fresh boot.
                    wakeups_left = WAKEUP_ROUNDS;
                    next_wakeup = Instant::now() + Duration::from_millis(WAKEUP_INTERVAL_MS);
                } else {
                    retry_ms = (retry_ms * 2).min(RETRY_MAX_MS);
                }
                next_init = Instant::now() + Duration::from_millis(retry_ms);
                // Don't try to catch up on the refresh cadence after a long init.
                next = Instant::now() + period;
            }
            continue;
        }

        if wakeups_left > 0 && Instant::now() >= next_wakeup {
            wakeups_left -= 1;
            next_wakeup = Instant::now() + Duration::from_millis(WAKEUP_INTERVAL_MS);
            let mut i2c = i2c_bus.lock().await;
            if ui.init(&mut i2c).await.is_ok() {
                defmt::info!(
                    "Display re-synced ({}/{} power-on wake-up)",
                    WAKEUP_ROUNDS - wakeups_left,
                    WAKEUP_ROUNDS
                );
            } else {
                // Lost the panel mid-flight: fall back to the retry path, which
                // will bring it back and repaint from scratch.
                display_ready = false;
                retry_ms = RETRY_FIRST_MS;
                next_init = Instant::now() + Duration::from_millis(retry_ms);
                continue;
            }
            // Repaint everything: the init sequence reset the panel's cursor.
            last_screen = None;
        }

        let app = app_state.lock().await.clone();
        if last_screen != Some(app.ui.screen) {
            ui.invalidate();
            last_screen = Some(app.ui.screen);
        }
        let mut i2c = i2c_bus.lock().await;

        let result = match app.ui.screen {
            MenuScreen::EepromFlash => {
                ui.draw_eeprom_screen(
                    &mut i2c,
                    &app,
                    app.eeprom_ui.title,
                    app.eeprom_ui.message,
                    app.eeprom_ui.progress_percent,
                )
                .await
            }
            MenuScreen::PdContract => ui.draw_pd_contract_screen(&mut i2c, &app).await,
            MenuScreen::Settings => ui.draw_cfg_screen(&mut i2c, &app).await,
            _ => ui.draw_power_screen(&mut i2c, &app).await,
        };

        if result.is_err() {
            // The panel stopped taking frames (unplugged, wedged, or a bus
            // timeout truncated the flush). Re-initialise and repaint rather
            // than keep pushing frames at a panel that is not listening. The
            // backoff escalates, so a panel that ACKs but never takes frames
            // cannot turn this into a re-init loop.
            defmt::warn!("Display frame failed; re-initialising the panel");
            display_ready = false;
            retry_ms = (retry_ms * 2).min(RETRY_MAX_MS);
            next_init = Instant::now() + Duration::from_millis(retry_ms);
            last_screen = None;
        }
    }
}
