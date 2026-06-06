//! INA228 driver stub — not wired to MCU I2C on current PCB revision.

use crate::state::Telemetry;

/// Future power-monitor backend when INA228 is routed to MCU I2CC.
#[allow(async_fn_in_trait)]
pub trait PowerMonitor {
    async fn read_telemetry(&mut self) -> Result<Telemetry, ()>;
}

pub struct Ina228Stub;

impl PowerMonitor for Ina228Stub {
    async fn read_telemetry(&mut self) -> Result<Telemetry, ()> {
        Err(())
    }
}
