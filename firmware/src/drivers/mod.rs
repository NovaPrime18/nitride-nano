//! I2C device drivers and the OLED screen layout.
//!
//! Two independent buses: the TPS26750 USB-PD controller and INA228 input
//! power monitor live on I2C1, while the SSD1306 and the TPS config EEPROM
//! live on I2C3.

use core::sync::atomic::{AtomicU32, Ordering};

pub mod ina228;
pub mod ssd1306;
pub mod ssd1306_init;
pub mod ssd1306_ui;
pub mod tps26750;

/// Cumulative I2C1 (TPS26750 + INA228) transaction failures since boot.
pub static I2C_PD_BUS_ERRORS: AtomicU32 = AtomicU32::new(0);

/// Cumulative I2C3 (OLED + EEPROM) transaction failures since boot.
pub static I2C_UI_BUS_ERRORS: AtomicU32 = AtomicU32::new(0);

/// Map an embassy I2C error to a short static label for defmt logging.
pub fn i2c_error_label(e: embassy_stm32::i2c::Error) -> &'static str {
    use embassy_stm32::i2c::Error::*;
    match e {
        Bus => "bus",
        Arbitration => "arbitration",
        Nack => "nack",
        Timeout => "timeout",
        Crc => "crc",
        Overrun => "overrun",
        ZeroLengthTransfer => "zero-length",
    }
}

/// Count one I2C transaction failure and log it (throttled after the first few
/// so a wedged bus cannot flood blocking RTT). Returns the running count.
pub fn note_i2c_error(counter: &AtomicU32, bus: &'static str, e: embassy_stm32::i2c::Error) -> u32 {
    let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 8 || n % 64 == 0 {
        defmt::warn!("I2C {} error #{}: {}", bus, n, i2c_error_label(e));
    }
    n
}
