//! INA228 polling helper: one stateful wrapper per device, polled from the main
//! loop, with the merge into [`crate::state::AppState`] left to the caller.
//!
//! rev3 fits two of these on the PD bus (see [`crate::drivers::ina228`]):
//!
//! * the **input** monitor U9 (`0x40`, R60 = 2 mΩ), which owns `vin_mv`,
//!   `iin_ma`, `pin_mw` and `ina_temp_c`;
//! * the **output** monitor U12 (`0x41`, R96 = 2 mΩ), which owns
//!   [`crate::state::Telemetry::out_ina`].
//!
//! Splitting `poll` (device state machine) from the merge keeps one copy of the
//! presence watchdog and stale-detection logic serving both instances. The
//! INA228 already averages 16 conversions per update, so its samples are
//! published directly instead of going through the ADC `TelemetryFilter`.

use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;
use embassy_time::{Duration, Instant};

use crate::drivers::ina228::{Ina228, Ina228Reading};

/// Consecutive read failures before the device is declared stale.
const FAILURES_BEFORE_STALE: u8 = 3;
/// Re-probe period while the device is absent (bus scan is not free).
const ABSENT_RETRY_MS: u64 = 1_000;

/// Stateful wrapper around one [`Ina228`], polled from the main loop.
pub struct InaSense {
    ina: Ina228,
    present: bool,
    failures: u8,
    last: Ina228Reading,
    next_probe: Instant,
}

impl InaSense {
    /// Bind to the INA228 at `addr` measuring a shunt of `shunt_mohm`.
    pub fn new(addr: u8, shunt_mohm: u32) -> Self {
        Self {
            ina: Ina228::new(addr, shunt_mohm),
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

    /// One polling step. Returns the newest reading, or `None` when the device is
    /// absent or this attempt failed.
    ///
    /// After [`FAILURES_BEFORE_STALE`] consecutive failures the device is
    /// declared absent and re-probed, so a hot-unplugged or reset chip is picked
    /// up again without a power cycle.
    pub async fn poll(&mut self, i2c: &mut I2c<'_, Async, Master>) -> Option<Ina228Reading> {
        if !self.present && !self.init(i2c).await {
            return None;
        }

        match self.ina.read(i2c).await {
            Some(reading) => {
                self.failures = 0;
                self.last = reading;
                Some(reading)
            }
            None => {
                self.failures = self.failures.saturating_add(1);
                if self.failures >= FAILURES_BEFORE_STALE {
                    // Force a re-probe on the next poll so a hot-unplugged or
                    // reset chip is picked up again.
                    self.present = false;
                    self.next_probe = Instant::now();
                }
                None
            }
        }
    }

    /// True while the most recent reads are succeeding.
    pub fn is_present(&self) -> bool {
        self.present
    }

    /// Most recent successful reading (for diagnostics/logging).
    pub fn last(&self) -> Ina228Reading {
        self.last
    }
}

impl Default for InaSense {
    fn default() -> Self {
        Self::new(crate::board::INA228_IN_ADDR, crate::board::INA228_IN_SHUNT_MOHM)
    }
}
