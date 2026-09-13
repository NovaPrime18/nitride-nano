//! INA228 polling helper: owns the driver and merges its input-side readings
//! into the shared [`crate::state::AppState`].
//!
//! The INA228 already averages 16 conversions per update, so its samples are
//! published directly instead of going through the ADC `TelemetryFilter`. It
//! overwrites `vin_mv` (input bus) and fills `iin_ma` / `pin_mw` /
//! `ina_temp_c`; on failure it leaves the last ADC-derived `vin_mv` in place
//! and only flags the input-side fields stale.

use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;
use embassy_time::{Duration, Instant};

use crate::board;
use crate::drivers::ina228::{Ina228, Ina228Reading};
use crate::state::AppState;

/// Consecutive read failures before the input telemetry is marked stale.
const FAILURES_BEFORE_STALE: u8 = 3;
/// Re-probe period while the INA228 is absent (bus scan is not free).
const ABSENT_RETRY_MS: u64 = 1_000;

/// Stateful wrapper around [`Ina228`], polled from the main loop.
pub struct InaSense {
    ina: Ina228,
    present: bool,
    failures: u8,
    last: Ina228Reading,
    next_probe: Instant,
}

impl InaSense {
    pub fn new() -> Self {
        Self {
            ina: Ina228::new(board::INA228_ADDR),
            present: false,
            failures: 0,
            last: Ina228Reading::default(),
            next_probe: Instant::now(),
        }
    }

    /// Configure and identify the device. Retries are rate-limited so an
    /// unpopulated/failed INA228 does not hammer the bus.
    pub async fn init(&mut self, i2c: &mut I2c<'_, Async, Master>) -> bool {
        if self.present {
            return true;
        }
        if Instant::now() < self.next_probe {
            return false;
        }
        self.present = self.ina.init(i2c).await;
        if !self.present {
            self.next_probe = Instant::now() + Duration::from_millis(ABSENT_RETRY_MS);
        }
        self.present
    }

    /// One polling step: refresh the input telemetry in `app`.
    pub async fn poll(&mut self, i2c: &mut I2c<'_, Async, Master>, app: &mut AppState) {
        if !self.present && !self.init(i2c).await {
            self.mark_stale(app);
            return;
        }

        match self.ina.read(i2c).await {
            Some(reading) => {
                self.failures = 0;
                self.last = reading;
                app.telemetry.vin_mv = reading.vin_mv;
                app.telemetry.iin_ma = reading.iin_ma;
                app.telemetry.pin_mw = reading.pin_mw;
                app.telemetry.ina_temp_c = reading.die_temp_c;
                app.telemetry.ina_ok = true;
            }
            None => {
                self.failures = self.failures.saturating_add(1);
                if self.failures >= FAILURES_BEFORE_STALE {
                    // Force a re-probe on the next poll so a hot-unplugged or
                    // reset chip is picked up again.
                    self.present = false;
                    self.next_probe = Instant::now();
                    self.mark_stale(app);
                }
            }
        }
    }

    /// Most recent successful reading (for diagnostics/logging).
    pub fn last(&self) -> Ina228Reading {
        self.last
    }

    fn mark_stale(&self, app: &mut AppState) {
        app.telemetry.ina_ok = false;
        app.telemetry.iin_ma = 0;
        app.telemetry.pin_mw = 0;
    }
}

impl Default for InaSense {
    fn default() -> Self {
        Self::new()
    }
}
