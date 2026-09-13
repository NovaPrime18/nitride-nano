//! Shared resources between Embassy tasks.
//!
//! Lock-ordering contract (load-bearing, do not change):
//! `APP_STATE` is always taken **before** an I2C bus mutex when both are needed
//! (e.g. the PD poll and the UI task's snapshot-then-render). The two I2C buses
//! are independent and must **never** be held at the same time — PD traffic and
//! UI traffic never share a peripheral, so nesting them would only invite a
//! deadlock against the UI task.

use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use static_cell::StaticCell;

use crate::state::AppState;

pub type AppStateMutex = Mutex<CriticalSectionRawMutex, AppState>;
pub type I2cBusMutex = Mutex<CriticalSectionRawMutex, I2c<'static, Async, Master>>;

/// Global application state, initialised once in `main`.
pub static APP_STATE: StaticCell<AppStateMutex> = StaticCell::new();
/// Hardware I2C3 (`SCL=PA8`, `SDA=PB5`) — SSD1306 OLED and, when JP8/JP9 are
/// bridged, the CAT24C512 TPS26750 config EEPROM.
pub static I2C_UI_BUS: StaticCell<I2cBusMutex> = StaticCell::new();
/// Hardware I2C1 (`SCL=PB8` after the rev2 bodge, `SDA=PB7`) — TPS26750 USB-PD
/// controller and INA228 input power monitor.
pub static I2C_PD_BUS: StaticCell<I2cBusMutex> = StaticCell::new();
