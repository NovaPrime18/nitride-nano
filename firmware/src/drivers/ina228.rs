//! INA228 driver stub — not wired to MCU I2C on the current PCB revision.
//!
//! Nothing in the crate references this module yet; it is a deliberate
//! placeholder for a future high-accuracy power-monitor backend (today all
//! telemetry comes from the MCU ADCs via [`crate::sense::adc_sense`]). Kept
//! compilable so the trait shape is type-checked as the rest of the code
//! evolves.

use crate::state::Telemetry;

/// Future power-monitor backend when the INA228 is routed to MCU I2C.
#[allow(async_fn_in_trait)]
pub trait PowerMonitor {
    async fn read_telemetry(&mut self) -> Result<Telemetry, ()>;
}

/// Placeholder that always reports failure, so accidental use is loud.
pub struct Ina228Stub;

impl PowerMonitor for Ina228Stub {
    async fn read_telemetry(&mut self) -> Result<Telemetry, ()> {
        Err(())
    }
}
