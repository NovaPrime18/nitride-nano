//! Minimal SSD1306 128x64 I2C driver.

use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;

const CMD: u8 = 0x00;
const DATA: u8 = 0x40;

pub struct Ssd1306 {
    addr: u8,
    fb: [u8; 1024],
}

impl Ssd1306 {
    pub fn new(addr: u8) -> Self {
        Self {
            addr,
            fb: [0; 1024],
        }
    }

    pub async fn init(&mut self, i2c: &mut I2c<'_, Async, Master>) -> Result<(), ()> {
        let cmds: &[u8] = &[
            0xAE, 0xD5, 0x80, 0xA8, 0x3F, 0xD3, 0x00, 0x40, 0x8D, 0x14, 0x20, 0x00, 0xA1,
            0xC8, 0xDA, 0x12, 0x81, 0xCF, 0xD9, 0xF1, 0xDB, 0x40, 0xA4, 0xA6, 0xAF,
        ];
        for &c in cmds {
            self.cmd(i2c, c).await?;
        }
        self.clear();
        Ok(())
    }

    async fn cmd(&self, i2c: &mut I2c<'_, Async, Master>, c: u8) -> Result<(), ()> {
        i2c.write(self.addr, &[CMD, c]).await.map_err(|_| ())
    }

    pub fn clear(&mut self) {
        self.fb.fill(0);
    }

    pub fn set_pixel(&mut self, x: u8, y: u8, on: bool) {
        if x >= 128 || y >= 64 {
            return;
        }
        let idx = (x as usize) + (y as usize / 8) * 128;
        let bit = 1 << (y % 8);
        if on {
            self.fb[idx] |= bit;
        } else {
            self.fb[idx] &= !bit;
        }
    }

    pub fn draw_char(&mut self, x: u8, y: u8, ch: u8) {
        let glyph = font5x7(ch);
        for (col, col_bits) in glyph.iter().enumerate() {
            for row in 0..7u8 {
                let on = (col_bits >> row) & 1 != 0;
                self.set_pixel(x + col as u8, y + row, on);
            }
        }
    }

    pub fn draw_str(&mut self, mut x: u8, y: u8, s: &str) {
        for b in s.bytes() {
            if x > 122 {
                break;
            }
            self.draw_char(x, y, b);
            x += 6;
        }
    }

    pub fn draw_line(&mut self, x0: u8, y0: u8, x1: u8, y1: u8) {
        let mut x = x0 as i16;
        let mut y = y0 as i16;
        let dx = (x1 as i16 - x).abs();
        let dy = (y1 as i16 - y).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx - dy;
        loop {
            if x >= 0 && y >= 0 {
                self.set_pixel(x as u8, y as u8, true);
            }
            if x == x1 as i16 && y == y1 as i16 {
                break;
            }
            let e2 = 2 * err;
            if e2 > -dy {
                err -= dy;
                x += sx;
            }
            if e2 < dx {
                err += dx;
                y += sy;
            }
        }
    }

    pub async fn flush(&self, i2c: &mut I2c<'_, Async, Master>) -> Result<(), ()> {
        self.cmd(i2c, 0x21).await?;
        self.cmd(i2c, 0x00).await?;
        self.cmd(i2c, 0x7F).await?;
        self.cmd(i2c, 0x22).await?;
        self.cmd(i2c, 0x00).await?;
        self.cmd(i2c, 0x07).await?;
        for chunk in self.fb.chunks(16) {
            let mut buf = [0u8; 17];
            buf[0] = DATA;
            buf[1..=chunk.len()].copy_from_slice(chunk);
            i2c.write(self.addr, &buf[..chunk.len() + 1]).await.map_err(|_| ())?;
        }
        Ok(())
    }
}

fn font5x7(ch: u8) -> [u8; 5] {
    match ch {
        b'0' => [0x3E, 0x51, 0x49, 0x45, 0x3E],
        b'1' => [0x00, 0x42, 0x7F, 0x40, 0x00],
        b'2' => [0x62, 0x51, 0x49, 0x49, 0x46],
        b'3' => [0x22, 0x41, 0x49, 0x49, 0x36],
        b'4' => [0x18, 0x14, 0x12, 0x7F, 0x10],
        b'5' => [0x27, 0x45, 0x45, 0x45, 0x39],
        b'6' => [0x3C, 0x4A, 0x49, 0x49, 0x30],
        b'7' => [0x01, 0x71, 0x09, 0x05, 0x03],
        b'8' => [0x36, 0x49, 0x49, 0x49, 0x36],
        b'9' => [0x06, 0x49, 0x49, 0x29, 0x1E],
        
        // --- Replace the A..=Z range with this block ---
        b'A' => [0x7E, 0x11, 0x11, 0x11, 0x7E],
        b'B' => [0x7F, 0x49, 0x49, 0x49, 0x36],
        b'C' => [0x3E, 0x41, 0x41, 0x41, 0x22],
        b'D' => [0x7F, 0x41, 0x41, 0x22, 0x1C],
        b'E' => [0x7F, 0x49, 0x49, 0x49, 0x41],
        b'F' => [0x7F, 0x09, 0x09, 0x09, 0x01],
        b'G' => [0x3E, 0x41, 0x49, 0x49, 0x7A],
        b'H' => [0x7F, 0x08, 0x08, 0x08, 0x7F],
        b'I' => [0x00, 0x41, 0x7F, 0x41, 0x00],
        b'J' => [0x20, 0x40, 0x41, 0x3F, 0x01],
        b'K' => [0x7F, 0x08, 0x14, 0x22, 0x41],
        b'L' => [0x7F, 0x40, 0x40, 0x40, 0x40],
        b'M' => [0x7F, 0x02, 0x0C, 0x02, 0x7F],
        b'N' => [0x7F, 0x04, 0x08, 0x10, 0x7F],
        b'O' => [0x3E, 0x41, 0x41, 0x41, 0x3E],
        b'P' => [0x7F, 0x09, 0x09, 0x09, 0x06],
        b'Q' => [0x3E, 0x41, 0x51, 0x21, 0x5E],
        b'R' => [0x7F, 0x09, 0x19, 0x29, 0x46],
        b'S' => [0x46, 0x49, 0x49, 0x49, 0x31],
        b'T' => [0x01, 0x01, 0x7F, 0x01, 0x01],
        b'U' => [0x3F, 0x40, 0x40, 0x40, 0x3F],
        b'V' => [0x1F, 0x20, 0x40, 0x20, 0x1F],
        b'W' => [0x3F, 0x40, 0x38, 0x40, 0x3F],
        b'X' => [0x63, 0x14, 0x08, 0x14, 0x63],
        b'Y' => [0x07, 0x08, 0x70, 0x08, 0x07],
        b'Z' => [0x61, 0x51, 0x49, 0x45, 0x43],
        // -----------------------------------------------

        b'a'..=b'z' => font5x7(ch - 32),
        b' ' => [0; 5],
        b'.' => [0x60, 0x60, 0x00, 0x00, 0x00],
        b':' => [0x00, 0x36, 0x36, 0x00, 0x00],
        b'-' => [0x08, 0x08, 0x08, 0x08, 0x08],
        b'>' => [0x00, 0x41, 0x22, 0x14, 0x08],
        _ => [0x7F, 0x41, 0x41, 0x41, 0x7F], // Fallback block character
    }
}
