//! Shared resources between Embassy tasks.

use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use static_cell::StaticCell;

use crate::state::AppState;

pub type AppStateMutex = Mutex<CriticalSectionRawMutex, AppState>;
pub type I2cBusMutex = Mutex<CriticalSectionRawMutex, I2c<'static, Async, Master>>;

pub static APP_STATE: StaticCell<AppStateMutex> = StaticCell::new();
pub static I2C_BUS: StaticCell<I2cBusMutex> = StaticCell::new();
