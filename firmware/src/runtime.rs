//! Shared resources between Embassy tasks.
//!
//! Lock-ordering contract (load-bearing, do not change):
//! `APP_STATE` is always taken **before** `I2C_BUS` when both are needed
//! (e.g. the PD poll and the UI task's snapshot-then-render). Holding them in
//! the opposite order in any future code would deadlock against the UI task.

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
/// Sole owner of the I2C3 peripheral, shared by the PD manager, OLED, and
/// EEPROM loader (all on the same bus).
pub static I2C_BUS: StaticCell<I2cBusMutex> = StaticCell::new();
