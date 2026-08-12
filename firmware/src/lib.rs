//! nitride-nano firmware library — USB-PD bench power supply on STM32G474.
//!
//! The binary in `main.rs` is a thin Embassy entry point; all logic lives here:
//!
//! - [`board`]: hardware constants, analog scaling, I2C addresses.
//! - [`state`]: shared [`state::AppState`] and its sub-structs/enums.
//! - [`runtime`]: the global mutexes (`APP_STATE`, `I2C_BUS`) shared between
//!   the main loop and the UI task.
//! - [`control`]: CV/CC DAC output stage and fault supervision.
//! - [`sense`]: ADC sampling, scaling, and telemetry filtering.
//! - [`pd`]: USB-PD contract management via the TPS26750.
//! - [`drivers`]: I2C device drivers (TPS26750, SSD1306, INA228 stub).
//! - [`ui`]: buttons/encoder input, menu model, and the OLED task.
//! - [`eeprom_loader`] / [`eeprom_workflow`]: TPS26750 full-flash EEPROM upload.
#![no_std]

pub mod board;
pub mod control;
pub mod drivers;
pub mod eeprom_loader;
pub mod eeprom_workflow;
pub mod hal;
pub mod pd;
pub mod runtime;
pub mod sense;
pub mod state;
pub mod ui;
