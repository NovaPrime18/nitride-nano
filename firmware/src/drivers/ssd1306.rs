//! Minimal SSD1306 128x64 I2C driver with partial refresh support.

use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;

const CMD: u8 = 0x00;
const DATA: u8 = 0x40;

/// Number of pages in the SSD1306 framebuffer (64 rows / 8 pixels per page).
const NUM_PAGES: u8 = 8;

/// Bitmask with all page bits set — used to mark every page dirty after a clear.
const DIRTY_ALL: u8 = 0xFF;

/// Framebuffer-backed SSD1306 handle. Drawing primitives only touch the 1 KiB
/// RAM buffer and record which pages changed; nothing reaches the panel until
/// [`Ssd1306::flush_partial`] pushes the dirty pages over I2C.
pub struct Ssd1306 {
    addr: u8,
    fb: [u8; 1024],
    /// Bitmap of pages that have been modified since the last flush.
    /// Bit `n` corresponds to page `n` (rows `[n*8 .. (n+1)*8 - 1]`).
    dirty_pages: u8,
}

impl Ssd1306 {
    /// Create a handle for the panel at 7-bit I2C address `addr`
    /// (typically [`crate::board::SSD1306_ADDR`]).
    pub fn new(addr: u8) -> Self {
        Self {
            addr,
            fb: [0; 1024],
            // All pages start "dirty" so the first flush sends the entire framebuffer.
            dirty_pages: DIRTY_ALL,
        }
    }

    /// Send the SSD1306 power-on command sequence and clear the framebuffer.
    ///
    /// The command bytes are the datasheet-recommended sequence for a 128×64
    /// panel with charge pump enabled — do not reorder or alter them.
    pub async fn init(&mut self, i2c: &mut I2c<'_, Async, Master>) -> Result<(), ()> {
        let cmds: &[u8] = &[
            0xAE, 0xD5, 0x80, 0xA8, 0x3F, 0xD3, 0x00, 0x40, 0x8D, 0x14, 0x20, 0x00, 0xA1, 0xC8,
            0xDA, 0x12, 0x81, 0xCF, 0xD9, 0xF1, 0xDB, 0x40, 0xA4, 0xA6, 0xAF,
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

    /// Zero the framebuffer and mark every page as dirty so the next flush
    /// sends all 1024 bytes to the hardware (cleans up any stale pixels).
    pub fn clear(&mut self) {
        self.fb.fill(0);
        self.dirty_pages = DIRTY_ALL;
    }

    /// Mark a page as dirty.  Called by `set_pixel`, `draw_char`, etc.
    #[inline(always)]
    fn mark_page_dirty(&mut self, page: u8) {
        if page < NUM_PAGES {
            self.dirty_pages |= 1 << page;
        }
    }

    /// Set or clear one pixel. Out-of-bounds coordinates are silently ignored so
    /// drawing helpers don't need their own clipping.
    pub fn set_pixel(&mut self, x: u8, y: u8, on: bool) {
        if x >= 128 || y >= 64 {
            return;
        }
        let page = y / 8;
        self.mark_page_dirty(page);

        let idx = (x as usize) + page as usize * 128;
        let bit = 1 << (y % 8);
        if on {
            self.fb[idx] |= bit;
        } else {
            self.fb[idx] &= !bit;
        }
    }

    /// Fill a rectangular region with black pixels and mark all affected pages dirty.
    pub fn fill_rect(&mut self, x: u8, y: u8, w: u8, h: u8) {
        let x_end = (x as u16 + w as u16).saturating_sub(1).min(127);
        let y_end = (y as u16 + h as u16).saturating_sub(1).min(63);
        for px in x as u8..=x_end as u8 {
            for py in y as u8..=y_end as u8 {
                self.set_pixel(px, py, false);
            }
        }
    }

    /// Draw one 5×7 glyph with its top-left corner at (`x`, `y`).
    pub fn draw_char(&mut self, x: u8, y: u8, ch: u8) {
        let glyph = font5x7(ch);
        for (col, col_bits) in glyph.iter().enumerate() {
            for row in 0..7u8 {
                let on = (col_bits >> row) & 1 != 0;
                self.set_pixel(x + col as u8, y + row, on);
            }
        }
    }

    /// Draw a string left-to-right, stopping before glyphs would cross the
    /// right edge of the panel.
    pub fn draw_str(&mut self, mut x: u8, y: u8, s: &str) {
        for b in s.bytes() {
            if x > 122 {
                break;
            }
            self.draw_char(x, y, b);
            x += 6;
        }
    }

    /// Bresenham line between two points, always drawn with "on" pixels.
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

    /// Send only the dirty (changed) page ranges to the display hardware.
    ///
    /// Pages are grouped into contiguous runs so that a single I2C command
    /// sequence covers each run instead of issuing one per page.  After
    /// flushing, all dirty bits are cleared — subsequent draws will only
    /// re-send pages they actually modified.
    pub async fn flush_partial(&mut self, i2c: &mut I2c<'_, Async, Master>) -> Result<(), ()> {
        let mut page = 0u8;
        while page < NUM_PAGES {
            if self.dirty_pages & (1 << page) != 0 {
                // Found a dirty page — collect the contiguous range starting here.
                let start = page;
                while page < NUM_PAGES && self.dirty_pages & (1 << page) != 0 {
                    page += 1;
                }
                let end = page - 1;

                // Send this run: set column/page address, then write the bytes.
                let offset = start as usize * 128;
                let len = (end - start + 1) as usize * 128;

                self.cmd(i2c, 0x22).await?; // page-addr-set
                self.cmd(i2c, start).await?;
                self.cmd(i2c, end).await?;

                let fb_chunk = &self.fb[offset..offset + len];
                for chunk in fb_chunk.chunks(16) {
                    let mut buf = [0u8; 17];
                    buf[0] = DATA;
                    buf[1..=chunk.len()].copy_from_slice(chunk);
                    i2c.write(self.addr, &buf[..chunk.len() + 1])
                        .await
                        .map_err(|_| ())?;
                }
            } else {
                page += 1;
            }
        }

        // All dirty pages have been sent — reset the bitmap.
        self.dirty_pages = 0;
        Ok(())
    }

    // TODO(dead-code): full-screen flush (legacy). Sends every page unconditionally and
    // clears all dirty bits. Superseded by `flush_partial`, which already covers the
    // full-screen case because `clear()` marks every page dirty. No call sites remain.
    // /// Full-screen flush (legacy). Sends every page unconditionally and clears all dirty bits.
    // /// Prefer `flush_partial` for incremental updates.
    // pub async fn flush(&mut self, i2c: &mut I2c<'_, Async, Master>) -> Result<(), ()> {
    //     self.cmd(i2c, 0x21).await?; // col-addr-set (auto)
    //     self.cmd(i2c, 0x00).await?;
    //     self.cmd(i2c, 0x7F).await?;
    //     self.cmd(i2c, 0x22).await?; // page-addr-set
    //     self.cmd(i2c, 0x00).await?;
    //     self.cmd(i2c, 0x07).await?;
    //     for chunk in self.fb.chunks(16) {
    //         let mut buf = [0u8; 17];
    //         buf[0] = DATA;
    //         buf[1..=chunk.len()].copy_from_slice(chunk);
    //         i2c.write(self.addr, &buf[..chunk.len() + 1]).await.map_err(|_| ())?;
    //     }
    //     self.dirty_pages = 0;
    //     Ok(())
    // }

    // TODO(dead-code): dirty-bitmap query — the UI flushes unconditionally on every
    // refresh tick, so nothing needs to ask whether a flush is required.
    // /// Returns true if any page has been modified since the last flush.
    // #[inline(always)]
    // pub fn is_dirty(&self) -> bool {
    //     self.dirty_pages != 0
    // }
}

/// 5×7 bitmap font lookup. Glyphs are stored column-major, LSB = top pixel.
/// Lowercase letters alias the uppercase glyphs; unknown bytes render as a box.
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
