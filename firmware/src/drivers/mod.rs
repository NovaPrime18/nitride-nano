//! I2C device drivers and the OLED screen layout.
//!
//! Two independent buses: the TPS26750 USB-PD controller and INA228 input
//! power monitor live on I2C1, while the SSD1306 and the TPS config EEPROM
//! live on I2C3.

pub mod ina228;
pub mod ssd1306;
pub mod ssd1306_init;
pub mod ssd1306_ui;
pub mod tps26750;
