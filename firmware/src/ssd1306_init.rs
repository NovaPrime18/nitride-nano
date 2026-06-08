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

    // Initialize the display
    display.init().unwrap();

    display
}