// ============================================================================
// LEGACY — SUPERSEDED, DO NOT USE.
//
// This file targets the old `ssd1306` crate API and a pre-Embassy HAL, and is
// NOT part of the build (not declared in `lib.rs`). Display initialisation now
// lives in `drivers/ssd1306.rs` (`Ssd1306::init`) behind `drivers/ssd1306_ui.rs`
// (`Ssd1306Ui::init`). Kept in the tree for historical reference only; it is not
// expected to compile.
// ============================================================================

use embassy_stm32::i2c::{I2c, I2cConfig};
use embassy_stm32::pac;
use ssd1306::{prelude::*, I2CDisplay};

pub fn initialize_ssd1306() -> I2CDisplay<I2c<pac::I2C3>, ssd1306::prelude::I2CInterface<I2c<pac::I2C3>>> {
    let dp = pac::Peripherals::take().unwrap();
    let mut rcc = dp.RCC.constrain();

    // Configure I2C
    let i2c = I2c::i2c3(
        dp.I2C3,
        dp.PB5,  // SDA
        dp.PA8,  // SCL
        I2cConfig::default(),
        &mut rcc.apb1r1,
        &mut rcc.rcc,
    );

    let interface = ssd1306::I2CInterface::new(i2c);
    let mut display = I2CDisplay::new(interface, DisplaySize128x64, DisplayRotation::Rotate0);
display.clear().unwrap(); // Clear the display after initialization
println!("Display initialized successfully");

    // Initialize the display
    display.init().unwrap();

    display
}