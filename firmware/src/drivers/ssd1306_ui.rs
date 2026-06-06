use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;

use crate::board::SSD1306_ADDR;
use crate::drivers::ssd1306::Ssd1306;
use crate::state::{AppState, MenuScreen, SupplyMode};

pub struct Ssd1306Ui {
    display: Ssd1306,
}

impl Ssd1306Ui {
    pub fn new() -> Self {
        Self {
            display: Ssd1306::new(SSD1306_ADDR),
        }
    }

    pub async fn init(&mut self, i2c: &mut I2c<'_, Async, Master>) -> Result<(), ()> {
        self.display.init(i2c).await
    }

    pub async fn draw_power_screen(
        &mut self,
        i2c: &mut I2c<'_, Async, Master>,
        app: &AppState,
    ) -> Result<(), ()> {
        self.display.clear();
        self.display.draw_str(0, 0, "nitride-nano");
        draw_field(&mut self.display, 0, 12, "Vin ", app.telemetry.vin_mv, "mV");
        draw_field(&mut self.display, 0, 22, "Vout", app.telemetry.vout_mv, "mV");
        draw_field(&mut self.display, 0, 32, "Iout", app.telemetry.iout_ma, "mA");
        draw_field(&mut self.display, 0, 42, "P   ", app.telemetry.pout_mw, "mW");

        let mode = match app.supply.mode {
            SupplyMode::Cv => "CV",
            SupplyMode::Cc => "CC",
            SupplyMode::Off => "OFF",
        };
        self.display.draw_str(90, 12, mode);
        let en = if app.supply.enabled { "ON " } else { "OFF" };
        self.display.draw_str(90, 22, en);

        let screen = match app.ui.screen {
            MenuScreen::Main => "MAIN",
            MenuScreen::CvSetpoint => "CV SET",
            MenuScreen::CcLimit => "CC LIM",
            MenuScreen::Enable => "ENABLE",
            MenuScreen::PdProfile => "PD",
            MenuScreen::Settings => "CFG",
        };
        self.display.draw_str(0, 54, screen);

        if app.ui.screen == MenuScreen::CvSetpoint {
            draw_field(&mut self.display, 64, 42, "Set", app.supply.v_set_mv, "mV");
        }
        if app.ui.screen == MenuScreen::CcLimit {
            draw_field(&mut self.display, 64, 42, "Set", app.supply.i_set_ma, "mA");
        }

        self.display.flush(i2c).await
    }
}

fn draw_field(d: &mut Ssd1306, x: u8, y: u8, label: &str, value: u32, unit: &str) {
    d.draw_str(x, y, label);
    let mut buf = [0u8; 8];
    fmt_u32(&mut buf, value);
    let s = core::str::from_utf8(&buf).unwrap_or("?");
    d.draw_str(x + 24, y, s);
    d.draw_str(x + 54, y, unit);
}

fn fmt_u32(buf: &mut [u8; 8], mut v: u32) -> &str {
    if v == 0 {
        buf[0] = b'0';
        return core::str::from_utf8(&buf[..1]).unwrap();
    }
    let mut i = 7usize;
    while v > 0 && i > 0 {
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i -= 1;
    }
    core::str::from_utf8(&buf[i + 1..]).unwrap_or("?")
}

impl Default for Ssd1306Ui {
    fn default() -> Self {
        Self::new()
    }
}
