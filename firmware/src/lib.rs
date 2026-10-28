//! nitride-nano firmware library — USB-PD bench power supply on STM32G474.
//!
//! The binary in `main.rs` is a thin Embassy entry point; all logic lives here:
//!
//! - [`board`]: hardware constants, analog scaling, I2C addresses.
//! - [`state`]: shared [`state::AppState`] and its sub-structs/enums.
//! - [`runtime`]: the global mutexes (`APP_STATE`, `I2C_UI_BUS`, `I2C_PD_BUS`)
//!   shared between the main loop and the UI task.
//! - [`control`]: CV/CC DAC output stage and fault supervision.
//! - [`sense`]: ADC sampling, scaling, telemetry filtering, and the INA228 poll.
//! - [`pd`]: USB-PD contract management and Auto-tracking rail selection.
//! - [`drivers`]: I2C device drivers (TPS26750, INA228, SSD1306).
//! - [`ui`]: buttons/encoder input, menu model, and the OLED task.
//! - [`eeprom_loader`] / [`eeprom_workflow`]: TPS26750 full-flash EEPROM upload.
//!
//! Reflashing is over USB-C via the STM32G474's ROM USB DFU bootloader, entered
//! by a BOOT0 hardware strap at reset; there is no in-firmware handoff path. See
//! `PCB/ECO-rev3-usb-dfu.md`.
#![no_std]

pub mod board;
pub mod control;
pub mod drivers;
pub mod eeprom_loader;
pub mod eeprom_workflow;
pub mod hal;
pub mod pd;
pub mod preflight;
pub mod runtime;
pub mod sense;
pub mod state;
pub mod ui;
