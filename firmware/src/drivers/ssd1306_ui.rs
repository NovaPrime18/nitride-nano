//! Display layout for the dual-colour 128×64 SSD1306.
//!
//! Colour zones are hardware-fixed and cannot be changed in software:
//!   Yellow  →  rows  0..16  (16 px)
//!   Blue    →  rows 16..64  (48 px)
//!
//! Screen layout:
//!
//!   ┌──────────────────────────────────────────┐  row 0
//!   │  nitride-nano               CV  ON        │  ← yellow header
//!   ├──────────────────────────────────────────┤  row 16  (separator line)
//!   │  Vin    12.000 V  [████████░░░░░]         │
//!   │  Vout    5.000 V  [████░░░░░░░░░]         │  ← telemetry + inline bars
//!   │  Iout    0.500 A  [█░░░░░░░░░░░░]         │
//!   │  Pout    2.500 W  [██░░░░░░░░░░░]         │
//!   │  ████████████░░░░░░░░░░░░░░░░░░░░░░░░░   │  ← power bar
//!   │  > MAIN                  SET  5.000 V     │  ← status / setpoint
//!   └──────────────────────────────────────────┘  row 63
//!
//! Each telemetry row now contains a 6-px-tall bordered bar graph in the
//! 47-px region to the right of the unit symbol (cols 81–127).  The bar
//! spans from the board's configured minimum to its maximum for that
//! measurement channel.
//!
//! NOTE: uses `draw_line(x0, y0, x1, y1)` for horizontal and vertical rules.

use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;
use embassy_time::{Duration, Timer};

use crate::board::{
    IOUT_MAX_MA,
    POWER_MAX_MW,
    SSD1306_ADDR,
    VBUS_SENSE_NUM, // Vin full-scale (mV at ADC ceiling)
    VOUT_MAX_MV,
    VOUT_MIN_MV,
};
use crate::drivers::ssd1306::Ssd1306;
use crate::state::{AppState, MenuScreen, SupplyMode};

// ── Layout constants ──────────────────────────────────────────────────────────

/// Pixel width of one character cell (6 px for the typical 5×7 font + 1 gap).
const FONT_W: u8 = 6;

/// Full display width in pixels.
const DISPLAY_W: u8 = 128;

// Row y-coordinates
const ROW_HEADER: u8 = 4; // vertically centred in the 16-px yellow zone
const ROW_DIVIDER: u8 = 16; // separator line at colour boundary (first blue row)
const ROW_VIN: u8 = 18;
const ROW_VOUT: u8 = 28;
const ROW_IOUT: u8 = 38;
const ROW_POUT: u8 = 48;
const ROW_POWER_BAR: u8 = 56; // 1-px power bar between telemetry and status
const ROW_STATUS: u8 = 57; // bottom of blue zone

// Column x-coordinates for measurement rows
//   "Iout   0.500 A"
//    x=0    right  x=73
const COL_LABEL: u8 = 0;
const COL_VALUE_RIGHT: u8 = 70; // right edge of the right-justified value field
const COL_UNIT: u8 = 73;

// Inline bar graph geometry
//   Starts 2 px after the unit character ends (col 73 + 6 + 2 = 81).
//   Ends 1 px from the right edge (col 126), leaving a 1-px margin.
//   Inner fill width  = BAR_INNER_W  (44 px of usable fill space).
const COL_BAR_LEFT: u8 = COL_UNIT + FONT_W + 2; // 81  – left border pixel
const COL_BAR_RIGHT: u8 = DISPLAY_W - 2; // 126 – right border pixel
const BAR_INNER_W: u8 = COL_BAR_RIGHT - COL_BAR_LEFT - 1; // 44 px (excl. borders)

// Bar height: 6 px tall, inset 1 px from the top of the 8-px character cell.
//   top border  = y + 1
//   fill rows   = y + 2 … y + 5   (4 px)
//   bot border  = y + 6
const BAR_OFFSET_TOP: u8 = 1;
const BAR_OFFSET_BOT: u8 = 6;

// Header badge positions (right side of yellow zone)
const COL_MODE: u8 = 92;
const COL_ENABLE: u8 = 110;

// ── Measurement display ranges ────────────────────────────────────────────────
//
// Each tuple is (min_milliunit, max_milliunit) used to scale the inline bar
// graph.  All bounds are sourced directly from `board.rs`; no magic numbers
// live here.

/// Vin bar spans 0 V → VBUS_SENSE_NUM (the ADC full-scale input voltage).
const VIN_RANGE: (u32, u32) = (0, VBUS_SENSE_NUM);
/// Vout bar spans VOUT_MIN_MV → VOUT_MAX_MV.
const VOUT_RANGE: (u32, u32) = (VOUT_MIN_MV, VOUT_MAX_MV);
/// Iout bar spans 0 A → IOUT_MAX_MA.
const IOUT_RANGE: (u32, u32) = (0, IOUT_MAX_MA);
/// Pout bar spans 0 W → POWER_MAX_MW.
const POUT_RANGE: (u32, u32) = (0, POWER_MAX_MW);

const SSD1306_POWER_ON_DELAY_MS: u64 = 50;

// ── Public type ───────────────────────────────────────────────────────────────

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
        Timer::after(Duration::from_millis(SSD1306_POWER_ON_DELAY_MS)).await;
        self.display.init(i2c).await
    }

    pub async fn draw_power_screen(
        &mut self,
        i2c: &mut I2c<'_, Async, Master>,
        app: &AppState,
    ) -> Result<(), ()> {
        self.display.clear();
        self.draw_header(app);
        self.draw_telemetry(app);
        self.draw_power_bar(app);
        self.draw_status_bar(app);
        self.display.flush(i2c).await
    }

    // ── Private drawing helpers ───────────────────────────────────────────────

    /// Yellow zone: device name on the left, mode and enable badges on the right.
    fn draw_header(&mut self, app: &AppState) {
        self.display.draw_str(COL_LABEL, ROW_HEADER, "nitride-nano");

        let mode = match app.supply.mode {
            SupplyMode::Cv => "CV",
            SupplyMode::Cc => "CC",
            SupplyMode::Off => "--",
        };
        self.display.draw_str(COL_MODE, ROW_HEADER, mode);
        self.display.draw_str(
            COL_ENABLE,
            ROW_HEADER,
            if app.supply.enabled { "ON" } else { "OFF" },
        );

        // Separator line marks the hardware colour boundary
        self.display
            .draw_line(0, ROW_DIVIDER, DISPLAY_W - 1, ROW_DIVIDER);
    }

    /// Four telemetry rows, each with label, right-justified decimal value,
    /// unit symbol, and an inline bar graph spanning the channel's full range.
    fn draw_telemetry(&mut self, app: &AppState) {
        draw_row(
            &mut self.display,
            ROW_VIN,
            "Vin ",
            app.telemetry.vin_mv,
            Unit::Voltage,
            VIN_RANGE,
        );
        draw_row(
            &mut self.display,
            ROW_VOUT,
            "Vout",
            app.telemetry.vout_mv,
            Unit::Voltage,
            VOUT_RANGE,
        );
        draw_row(
            &mut self.display,
            ROW_IOUT,
            "Iout",
            app.telemetry.iout_ma,
            Unit::Current,
            IOUT_RANGE,
        );
        draw_row(
            &mut self.display,
            ROW_POUT,
            "Pout",
            app.telemetry.pout_mw,
            Unit::Power,
            POUT_RANGE,
        );
    }

    /// Thin bar showing output power relative to the configured v_set × i_set ceiling.
    fn draw_power_bar(&mut self, app: &AppState) {
        let max_mw = ((app.supply.v_set_mv as u64 * app.supply.i_set_ma as u64) / 1_000).max(1);
        let bar_len = ((app.telemetry.pout_mw as u64 * DISPLAY_W as u64) / max_mw)
            .min(DISPLAY_W as u64) as u8;

        if bar_len > 0 {
            self.display
                .draw_line(0, ROW_POWER_BAR, bar_len - 1, ROW_POWER_BAR);
        }
    }

    /// Bottom row: active screen name on the left, setpoint on the right when editing.
    fn draw_status_bar(&mut self, app: &AppState) {
        let tag = match app.ui.screen {
            MenuScreen::Main => "MAIN",
            MenuScreen::CvSetpoint => "V-SET",
            MenuScreen::CcLimit => "I-LIM",
            MenuScreen::Enable => "ENBL",
            MenuScreen::PdProfile => "PD",
            MenuScreen::Settings => "CFG",
            MenuScreen::EepromFlash => "EE",
        };

        self.display.draw_str(0, ROW_STATUS, ">");
        self.display.draw_str(FONT_W, ROW_STATUS, tag);

        match app.ui.screen {
            MenuScreen::CvSetpoint => {
                draw_setpoint_right(&mut self.display, app.supply.v_set_mv, Unit::Voltage)
            }
            MenuScreen::CcLimit => {
                draw_setpoint_right(&mut self.display, app.supply.i_set_ma, Unit::Current)
            }
            _ => {}
        }
    }

    pub async fn draw_eeprom_screen(
        &mut self,
        i2c: &mut I2c<'_, Async, Master>,
        title: &str,
        message: &str,
        progress_percent: u8,
    ) -> Result<(), ()> {
        self.display.clear();
        self.display.draw_str(0, ROW_HEADER, "nitride-nano");
        self.display.draw_str(92, ROW_HEADER, "EE");
        self.display
            .draw_line(0, ROW_DIVIDER, DISPLAY_W - 1, ROW_DIVIDER);

        self.display.draw_str(0, 22, title);
        self.display.draw_str(0, 34, message);
        draw_percent_bar(&mut self.display, 48, progress_percent);

        let mut pct_buf = [0u8; 4];
        let pct = fmt_percent(&mut pct_buf, progress_percent);
        let x = DISPLAY_W.saturating_sub((pct.len() as u8 + 1) * FONT_W);
        self.display.draw_str(x, ROW_STATUS, pct);
        self.display
            .draw_str(x + pct.len() as u8 * FONT_W, ROW_STATUS, "%");

        self.display.flush(i2c).await
    }
}

impl Default for Ssd1306Ui {
    fn default() -> Self {
        Self::new()
    }
}

// ── Unit type ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Unit {
    Voltage,
    Current,
    Power,
}

impl Unit {
    fn symbol(self) -> &'static str {
        match self {
            Unit::Voltage => "V",
            Unit::Current => "A",
            Unit::Power => "W",
        }
    }
}

// ── Drawing helpers ───────────────────────────────────────────────────────────

/// Draw one labelled measurement row with a right-justified value column and
/// an inline bar graph.
///
/// ```text
/// "Vout   5.000 V  [████░░░░░░░░░]"
///  ^      ^     ^   ^            ^
///  label  val  unit bar_left  bar_right
/// ```
///
/// `range` is `(min_milliunit, max_milliunit)` sourced from `board.rs`.
fn draw_row(d: &mut Ssd1306, y: u8, label: &str, millivalue: u32, unit: Unit, range: (u32, u32)) {
    // ── Text ──────────────────────────────────────────────────────────────────
    d.draw_str(COL_LABEL, y, label);

    let mut buf = [0u8; 8];
    let s = fmt_decimal(&mut buf, millivalue);

    // Right-justify against COL_VALUE_RIGHT
    let x = COL_VALUE_RIGHT.saturating_sub(s.len() as u8 * FONT_W);
    d.draw_str(x, y, s);

    d.draw_str(COL_UNIT, y, unit.symbol());

    // ── Bar graph ─────────────────────────────────────────────────────────────
    draw_bar(d, y, millivalue, range);
}

/// Draw a bordered horizontal bar graph for a single measurement row.
///
/// The bar occupies columns `COL_BAR_LEFT..=COL_BAR_RIGHT` and is inset
/// vertically within the character cell:
///
/// ```text
/// y+1  ┌─────────────────────────────┐   ← top border (1 px)
/// y+2  │ ████████████░░░░░░░░░░░░░░░ │   ┐
/// y+3  │ ████████████░░░░░░░░░░░░░░░ │   │ fill (4 px)
/// y+4  │ ████████████░░░░░░░░░░░░░░░ │   │
/// y+5  │ ████████████░░░░░░░░░░░░░░░ │   ┘
/// y+6  └─────────────────────────────┘   ← bottom border (1 px)
/// ```
///
/// `range` is `(min_milliunit, max_milliunit)`.  Values outside the range are
/// clamped so the bar always stays within its borders.
fn draw_bar(d: &mut Ssd1306, y: u8, millivalue: u32, range: (u32, u32)) {
    let (min_mv, max_mv) = range;
    let top = y + BAR_OFFSET_TOP;
    let bot = y + BAR_OFFSET_BOT;

    // Outline rectangle (4 lines)
    d.draw_line(COL_BAR_LEFT, top, COL_BAR_RIGHT, top); // top border
    d.draw_line(COL_BAR_LEFT, bot, COL_BAR_RIGHT, bot); // bottom border
    d.draw_line(COL_BAR_LEFT, top, COL_BAR_LEFT, bot); // left border
    d.draw_line(COL_BAR_RIGHT, top, COL_BAR_RIGHT, bot); // right border

    // Filled portion: proportional to (value − min) / (max − min)
    let span = max_mv.saturating_sub(min_mv).max(1);
    let clamped = millivalue.clamp(min_mv, max_mv) - min_mv;
    let fill_w = ((clamped as u64 * BAR_INNER_W as u64) / span as u64) as u8;

    if fill_w > 0 {
        let fill_right = COL_BAR_LEFT + fill_w; // still ≤ COL_BAR_RIGHT − 1
                                                // Fill all interior rows (y+2 … y+5)
        let mut row = top + 1;
        while row < bot {
            d.draw_line(COL_BAR_LEFT + 1, row, fill_right, row);
            row += 1;
        }
    }
}

fn draw_percent_bar(d: &mut Ssd1306, y: u8, percent: u8) {
    let clamped = percent.min(100);
    d.draw_line(0, y, DISPLAY_W - 1, y);
    d.draw_line(0, y + 7, DISPLAY_W - 1, y + 7);
    d.draw_line(0, y, 0, y + 7);
    d.draw_line(DISPLAY_W - 1, y, DISPLAY_W - 1, y + 7);

    let fill_w = ((clamped as u16 * (DISPLAY_W - 2) as u16) / 100) as u8;
    if fill_w > 0 {
        for row in y + 1..y + 7 {
            d.draw_line(1, row, fill_w, row);
        }
    }
}

/// Draw a setpoint reading right-aligned on the status bar.
///
/// Example: `"SET  5.000 V"` flush with the right edge.
fn draw_setpoint_right(d: &mut Ssd1306, millivalue: u32, unit: Unit) {
    const PREFIX: &str = "SET ";
    let mut buf = [0u8; 8];
    let val = fmt_decimal(&mut buf, millivalue);
    let sym = unit.symbol();

    let total_w = (PREFIX.len() + val.len() + sym.len()) as u8 * FONT_W;
    let x = DISPLAY_W.saturating_sub(total_w);

    d.draw_str(x, ROW_STATUS, PREFIX);
    d.draw_str(x + PREFIX.len() as u8 * FONT_W, ROW_STATUS, val);
    d.draw_str(
        x + (PREFIX.len() + val.len()) as u8 * FONT_W,
        ROW_STATUS,
        sym,
    );
}

// ── Number formatting ─────────────────────────────────────────────────────────

/// Convert a milli-unit value to compact decimal notation.
///
/// ```text
/// 12_000  →  "12.000"
///    500  →   "0.500"
///    999  →   "0.999"
/// 99_999  →  "99.999"
/// ```
fn fmt_decimal(buf: &mut [u8; 8], millivalue: u32) -> &str {
    let int_part = millivalue / 1000;
    let frac_part = millivalue % 1000;
    let mut i = 0usize;

    // Integer digits – no leading zeros, but always at least one digit
    if int_part == 0 {
        buf[i] = b'0';
        i += 1;
    } else {
        // Collect digits right-to-left in a scratch buffer
        let mut tmp = [0u8; 4];
        let mut ti = tmp.len();
        let mut n = int_part;
        while n > 0 && ti > 0 {
            ti -= 1;
            tmp[ti] = b'0' + (n % 10) as u8;
            n /= 10;
        }
        for &byte in &tmp[ti..] {
            buf[i] = byte;
            i += 1;
        }
    }

    buf[i] = b'.';
    i += 1;

    // Three fractional digits, always zero-padded
    buf[i] = b'0' + (frac_part / 100) as u8;
    i += 1;
    buf[i] = b'0' + ((frac_part / 10) % 10) as u8;
    i += 1;
    buf[i] = b'0' + (frac_part % 10) as u8;
    i += 1;

    core::str::from_utf8(&buf[..i]).unwrap_or("?.???")
}

fn fmt_percent(buf: &mut [u8; 4], percent: u8) -> &str {
    let percent = percent.min(100);
    let i = if percent == 100 {
        buf[0] = b'1';
        buf[1] = b'0';
        buf[2] = b'0';
        3
    } else if percent >= 10 {
        buf[0] = b'0' + percent / 10;
        buf[1] = b'0' + percent % 10;
        2
    } else {
        buf[0] = b'0' + percent;
        1
    };

    core::str::from_utf8(&buf[..i]).unwrap_or("?")
}
