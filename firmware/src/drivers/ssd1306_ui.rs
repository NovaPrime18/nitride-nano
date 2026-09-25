//! Display layout for the dual-colour 128×64 SSD1306.
//!
//! Colour zones are hardware-fixed and cannot be changed in software:
//!   Yellow  →  rows  0..16  (16 px)
//!   Blue    →  rows 16..64  (48 px)
//!
//! Screen layout:
//!
//!   ┌──────────────────────────────────────────┐  row 0
//!   │  T1:30.0 T2:35.0            CV  ON        │  ← yellow header
//!   ├──────────────────────────────────────────┤  row 16  (separator line)
//!   │  Vin   12.000 V   Iin   0.500 A           │  row 18  input values
//!   │  [████████░░░░]   [██░░░░░░░░░░]          │  row 26  input bars
//!   │  Vout         Iout         Pout           │  row 32  output headers
//!   │  5.000 V      0.500 A      2.500 W        │  row 40  output values
//!   │  [██░░░░░]    [█░░░░░░]    [██░░░░░]      │  row 48  output bars
//!   │  ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓  │  row 54  ceiling bar
//!   │  >MAIN             Eff 87.5% [███░░░░]    │  row 56  status / eff.
//!   └──────────────────────────────────────────┘  row 63
//!
//! The blue zone is 48 px (rows 16..63) and the 5-px bar is what makes the
//! content fit: every element gets a 1-px gap, and both the row under the
//! divider and the panel's last row stay clear.  Inputs sit in two 60-px fields
//! on one line; outputs form three 42-px columns whose reading is
//! right-justified directly under its header, so the digits line up in columns.
//! Every reading keeps a bordered bar graph, scaled from the board's configured
//! range for that channel (`board.rs`) — no magic bounds live here.  The Iin
//! field doubles as the input-monitor status field (`NO PD` / `INA!`), and the
//! efficiency readout shares the bottom line with the screen tag.
//!
//! Two other fullscreen layouts share the panel:
//! - the CFG (Settings) list — header + up to [`CFG_VISIBLE_ROWS`] option rows,
//!   the selected row filled with inverted text and a right-edge scrollbar when
//!   the list overflows; and
//! - the PD contract grid and EEPROM progress screens (below).
//!
//! The CFG "Output V sweep" has no screen of its own: while it is armed,
//! running, or done, the power screen's bottom line is replaced by a sweep
//! status line (`>SWEEP` plus the confirm prompt / point progress / `DONE`),
//! which also displaces the efficiency readout.
//!
//! NOTE: uses `draw_line(x0, y0, x1, y1)` for horizontal and vertical rules.

use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;
use embassy_time::{Duration, Timer};

use crate::board::{
    IOUT_MAX_MA,
    POWER_MAX_MW,
    SSD1306_ADDR,
    SWEEP_POINTS,
    VBUS_SENSE_NUM, // Vin full-scale (mV at ADC ceiling)
    VOUT_MAX_MV,
    VOUT_MIN_MV,
};
use crate::drivers::ssd1306::Ssd1306;
use crate::pd::auto_track::nearest_preset_index;
use crate::state::{
    AppState, MenuScreen, PdAutoError, PdMode, RailRegion, StepMode, SupplyMode,
    SweepPhase, CFG_ITEMS, CFG_VISIBLE_ROWS, PD_PRESET_VOLTAGES_MV,
};

// ── Layout constants ──────────────────────────────────────────────────────────

/// Pixel width of one character cell (6 px for the typical 5×7 font + 1 gap).
const FONT_W: u8 = 6;

/// Full display width in pixels.
const DISPLAY_W: u8 = 128;

// Row y-coordinates.  The blue zone is 48 px (rows 16..63).  A 5-px bar is
// what lets the content plus a 1-px gap around every element fit, keeping both
// the row under the divider (17) and the panel's last row (63) clear.
const ROW_HEADER: u8 = 4; // vertically centred in the 16-px yellow zone
const ROW_DIVIDER: u8 = 16; // separator line at colour boundary (first blue row)
const ROW_IN_VALUES: u8 = 18; // Vin | Iin readings, side by side
const ROW_IN_BARS: u8 = 25; // bars occupy rows 26..30
const ROW_OUT_LABELS: u8 = 32; // Vout | Iout | Pout column headers
const ROW_OUT_VALUES: u8 = 40; // output readings, under their headers
const ROW_OUT_BARS: u8 = 47; // bars occupy rows 48..52
const ROW_POWER_BAR: u8 = 54; // 1-px setpoint-ceiling bar
const ROW_STATUS: u8 = 56; // screen tag + setpoint / sweep / efficiency

// Input row: two equal 60-px fields, each laid out as "label value unit" with
// the value right-justified so the decimal points stay put.
//   "Vin 12.000 V"    label 0,  value ends at 53, unit at 54
//   "Iin  0.500 A"    label 66, value ends at 119, unit at 120
const COL_IN_LABEL: u8 = 0;
const COL_IN_VALUE_RIGHT: u8 = 54; // Vin value is right-justified to here
const COL_IN2_LABEL: u8 = 66;
const COL_IN2_VALUE_RIGHT: u8 = 120; // Iin value is right-justified to here
const IN_VALUE_CLEAR_X: u8 = 18; // Vin value + unit field, cleared each frame
const IN_VALUE_CLEAR_W: u8 = 42; // 18..59
const IN2_VALUE_CLEAR_X: u8 = 84; // Iin value + unit field (also `NO PD`/`INA!`)
const IN2_VALUE_CLEAR_W: u8 = 42; // 84..125

// Input bars: one 60-px bar under each field.
const IN_BAR0_LEFT: u8 = 0;
const IN_BAR0_RIGHT: u8 = 59;
const IN_BAR1_LEFT: u8 = 66;
const IN_BAR1_RIGHT: u8 = 125;

// Output group: three ~42-px columns.  The header sits at the column's left
// edge on ROW_OUT_LABELS; the value is right-justified to the column's right
// edge on ROW_OUT_VALUES; the bar spans the column on ROW_OUT_BARS.  The 42-px
// width is 7 character cells, which is exactly "value + unit" at the widest
// formatting the channels can produce.
const OUT_COL0_LEFT: u8 = 0;
const OUT_COL0_RIGHT: u8 = 42; // value right edge (exclusive)
const OUT_COL1_LEFT: u8 = 43;
const OUT_COL1_RIGHT: u8 = 85;
const OUT_COL2_LEFT: u8 = 86;
const OUT_COL2_RIGHT: u8 = 128;
const OUT_BAR0_LEFT: u8 = 0;
const OUT_BAR0_RIGHT: u8 = 41;
const OUT_BAR1_LEFT: u8 = 43;
const OUT_BAR1_RIGHT: u8 = 84;
const OUT_BAR2_LEFT: u8 = 86;
const OUT_BAR2_RIGHT: u8 = 127;

// Efficiency readout, right side of the status line: "Eff 87.5%" is
// right-justified to col 89 and followed by a wide 0–100 % bar.
const EFF_LABEL: &str = "Eff ";
const COL_EFF_RIGHT: u8 = 89;
const EFF_BAR_LEFT: u8 = 92;
const EFF_BAR_RIGHT: u8 = 127;
/// Efficiency bar span, in whole percent.
const EFF_RANGE: (u32, u32) = (0, 100);

// Bar height: 5 px tall, inset 1 px from the top of the 8-px character cell so
// the row above the bar and the row below it stay clear.
//   top border  = y + 1
//   fill rows   = y + 2 … y + 4   (3 px)
//   bot border  = y + 5
const BAR_OFFSET_TOP: u8 = 1;
const BAR_OFFSET_BOT: u8 = 5;

// Header badge positions (right side of yellow zone)
const COL_MODE: u8 = 92;
const COL_ENABLE: u8 = 110;

// Label column for the header's left-hand content (temperatures / fault text).
const COL_LABEL: u8 = 0;

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
/// Iin bar uses the same full-scale current as the output stage's INA228.
const IIN_RANGE: (u32, u32) = (0, IOUT_MAX_MA);

const SSD1306_POWER_ON_DELAY_MS: u64 = 50;

// ── Public type ───────────────────────────────────────────────────────────────

/// Screen composer for the bench supply UI. Owns the framebuffer driver and
/// knows the pixel layout of each screen; all methods end with a partial
/// flush, so callers never deal with the dirty-page machinery.
pub struct Ssd1306Ui {
    display: Ssd1306,
}

impl Ssd1306Ui {
    pub fn new() -> Self {
        Self {
            display: Ssd1306::new(SSD1306_ADDR),
        }
    }

    /// Power-on delay (panel VDD stabilization), then the SSD1306 init sequence.
    pub async fn init(&mut self, i2c: &mut I2c<'_, Async, Master>) -> Result<(), ()> {
        Timer::after(Duration::from_millis(SSD1306_POWER_ON_DELAY_MS)).await;
        self.display.init(i2c).await
    }

    /// Force the panel to fully repaint on the next flush.
    ///
    /// Zeros the model framebuffer and marks every page dirty, so the following
    /// [`Ssd1306::flush_partial`] sends all 1024 bytes (equivalent to a full,
    /// column-anchored repaint). Call this on screen transitions and once after
    /// init to re-sync the physical panel with the framebuffer, guaranteeing no
    /// stale pixels or leftover-cursor misalignment survive between layouts.
    pub fn invalidate(&mut self) {
        self.display.clear();
    }

    /// Redraw the power screen, reusing existing framebuffer content where possible.
    ///
    /// Unlike `clear()`-based redraws this only touches pages that actually changed,
    /// so typical telemetry updates send a fraction of the 1024-byte framebuffer over I2C.
    pub async fn draw_power_screen(
        &mut self,
        i2c: &mut I2c<'_, Async, Master>,
        app: &AppState,
    ) -> Result<(), ()> {
        // No clear() — only dirty pages are flushed at the end.
        self.draw_header(app);
        self.draw_telemetry(app);
        self.draw_power_bar(app);
        self.draw_status_bar(app);
        self.display.flush_partial(i2c).await
    }

    // ── Private drawing helpers ───────────────────────────────────────────────

    /// Yellow zone: device name (or step mode when editing) on the left,
    /// mode and enable badges on the right.
    fn draw_header(&mut self, app: &AppState) {
        // Clear any stale pixels in the header region before redrawing.
        // This is needed because partial refresh only sends dirty pages to hardware,
        // so text shorter than previous content (e.g. "OFF" → "ON") would leave ghost pixels.
        self.display.fill_rect(0, 0, DISPLAY_W, ROW_DIVIDER);

        // Show "Fine" or "Coarse" when editing CV/CC setpoints, otherwise temperature values
        match app.ui.screen {
            MenuScreen::CvSetpoint | MenuScreen::CcLimit => {
                let mode_text = match app.ui.encoder_step_mode {
                    StepMode::Fine => "Fine",
                    StepMode::Coarse => "Coarse",
                };
                self.display.draw_str(COL_LABEL, ROW_HEADER, mode_text);
            }
            _ => {
                self.draw_header_label_or_temps(app);
            }
        }

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

    /// The two telemetry groups: `Vin | Iin` on one line with a bar each, then
    /// the `Vout | Iout | Pout` triple as header/value/bar columns.
    fn draw_telemetry(&mut self, app: &AppState) {
        self.draw_input_values(app);
        self.draw_input_bars(app);
        self.draw_output_header();
        self.draw_output_values(app);
        self.draw_output_bars(app);
    }

    /// Input readings side by side, each with its unit.  The Iin field is also
    /// where a missing input monitor or PD source is reported.
    fn draw_input_values(&mut self, app: &AppState) {
        // Vin — the INA228 when present, otherwise the ADC's Vbus fallback.
        self.display.draw_str(COL_IN_LABEL, ROW_IN_VALUES, "Vin");
        self.display
            .fill_rect(IN_VALUE_CLEAR_X, ROW_IN_VALUES, IN_VALUE_CLEAR_W, 8);
        let mut vin_buf = [0u8; 8];
        let vin = fmt_value(&mut vin_buf, app.telemetry.vin_mv);
        self.display.draw_str(
            COL_IN_VALUE_RIGHT.saturating_sub(vin.len() as u8 * FONT_W),
            ROW_IN_VALUES,
            vin,
        );
        self.display.draw_str(COL_IN_VALUE_RIGHT, ROW_IN_VALUES, "V");

        // Iin — the field is cleared first so `NO PD`/`INA!` cannot ghost.
        self.display.draw_str(COL_IN2_LABEL, ROW_IN_VALUES, "Iin");
        self.display
            .fill_rect(IN2_VALUE_CLEAR_X, ROW_IN_VALUES, IN2_VALUE_CLEAR_W, 8);

        // No source capabilities were read: there is no PD source on the bus
        // (dead bus, no cable, or a non-PD input). Say so plainly.
        if app.pd_control.error == PdAutoError::NoCable {
            self.display
                .draw_str(IN2_VALUE_CLEAR_X, ROW_IN_VALUES, "NO PD");
            return;
        }
        if !app.telemetry.ina_ok {
            self.display
                .draw_str(IN2_VALUE_CLEAR_X, ROW_IN_VALUES, "INA!");
            return;
        }

        let mut iin_buf = [0u8; 8];
        let iin = fmt_signed_value(&mut iin_buf, app.telemetry.iin_ma);
        self.display.draw_str(
            COL_IN2_VALUE_RIGHT.saturating_sub(iin.len() as u8 * FONT_W),
            ROW_IN_VALUES,
            iin,
        );
        self.display.draw_str(COL_IN2_VALUE_RIGHT, ROW_IN_VALUES, "A");
    }

    /// One bar under each input field, spanning the channel's full range.  The
    /// Iin bar is empty whenever the reading is unavailable.
    fn draw_input_bars(&mut self, app: &AppState) {
        let iin_ok = app.telemetry.ina_ok && app.pd_control.error != PdAutoError::NoCable;
        let iin_ma = if iin_ok {
            app.telemetry.iin_ma.max(0) as u32
        } else {
            0
        };

        draw_bar(
            &mut self.display,
            IN_BAR0_LEFT,
            IN_BAR0_RIGHT,
            ROW_IN_BARS,
            app.telemetry.vin_mv,
            VIN_RANGE,
        );
        draw_bar(
            &mut self.display,
            IN_BAR1_LEFT,
            IN_BAR1_RIGHT,
            ROW_IN_BARS,
            iin_ma,
            IIN_RANGE,
        );
    }

    /// Column headers for the output triple.
    fn draw_output_header(&mut self) {
        self.display.draw_str(OUT_COL0_LEFT, ROW_OUT_LABELS, "Vout");
        self.display.draw_str(OUT_COL1_LEFT, ROW_OUT_LABELS, "Iout");
        self.display.draw_str(OUT_COL2_LEFT, ROW_OUT_LABELS, "Pout");
    }

    /// Output readings right-justified directly beneath their headers.
    fn draw_output_values(&mut self, app: &AppState) {
        draw_column_value(
            &mut self.display,
            OUT_COL0_LEFT,
            OUT_COL0_RIGHT,
            ROW_OUT_VALUES,
            app.telemetry.vout_mv,
            "V",
        );
        draw_column_value(
            &mut self.display,
            OUT_COL1_LEFT,
            OUT_COL1_RIGHT,
            ROW_OUT_VALUES,
            app.telemetry.iout_ma,
            "A",
        );
        draw_column_value(
            &mut self.display,
            OUT_COL2_LEFT,
            OUT_COL2_RIGHT,
            ROW_OUT_VALUES,
            app.telemetry.pout_mw,
            "W",
        );
    }

    /// One bar per output channel, spanning each channel's configured range.
    fn draw_output_bars(&mut self, app: &AppState) {
        draw_bar(
            &mut self.display,
            OUT_BAR0_LEFT,
            OUT_BAR0_RIGHT,
            ROW_OUT_BARS,
            app.telemetry.vout_mv,
            VOUT_RANGE,
        );
        draw_bar(
            &mut self.display,
            OUT_BAR1_LEFT,
            OUT_BAR1_RIGHT,
            ROW_OUT_BARS,
            app.telemetry.iout_ma,
            IOUT_RANGE,
        );
        draw_bar(
            &mut self.display,
            OUT_BAR2_LEFT,
            OUT_BAR2_RIGHT,
            ROW_OUT_BARS,
            app.telemetry.pout_mw,
            POUT_RANGE,
        );
    }

    /// Thin bar showing output power relative to the configured v_set × i_set ceiling.
    fn draw_power_bar(&mut self, app: &AppState) {
        let max_mw = ((app.supply.v_set_mv as u64 * app.supply.i_set_ma as u64) / 1_000).max(1);
        let bar_len = ((app.telemetry.pout_mw as u64 * DISPLAY_W as u64) / max_mw)
            .min(DISPLAY_W as u64) as u8;

        // Clear the whole row first so a shrinking bar doesn't leave its old
        // length lit.
        self.display.fill_rect(0, ROW_POWER_BAR, DISPLAY_W, 1);

        if bar_len > 0 {
            self.display
                .draw_line(0, ROW_POWER_BAR, bar_len - 1, ROW_POWER_BAR);
        }
    }

    /// Bottom row: active screen name on the left, then the setpoint (when
    /// editing) or the efficiency readout (main screen) on the right.
    fn draw_status_bar(&mut self, app: &AppState) {
        // A CFG sweep owns the whole bottom line while armed/running/done.
        if app.sweep.phase != SweepPhase::Off {
            self.draw_sweep_status(app);
            return;
        }

        // Clear the whole line: the tag, the setpoint, the efficiency readout
        // and the transient sweep text all differ in length, and this row is
        // not otherwise redrawn, so leftover pixels would ghost.
        self.display.fill_rect(0, ROW_STATUS, DISPLAY_W, 8);

        let auto = app.pd_control.mode == PdMode::Auto;
        let tag = match app.ui.screen {
            MenuScreen::Main => {
                if auto {
                    "AUTO"
                } else {
                    "MAIN"
                }
            }
            MenuScreen::CvSetpoint => "V-SET",
            MenuScreen::CcLimit => "I-LIM",
            MenuScreen::PdContract => "PD",
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
            MenuScreen::Main => self.draw_efficiency(app),
            _ => {}
        }
    }

    /// Efficiency readout on the main screen's status line: the input-to-output
    /// percentage, right-justified next to the tag, followed by a 0–100 % bar.
    ///
    /// `pin_mw` is the INA228's input power and `pout_mw` the ADC's output
    /// power, so the ratio is only meaningful while the monitor is responding.
    fn draw_efficiency(&mut self, app: &AppState) {
        let pin_mw = app.telemetry.pin_mw;
        let pout_mw = app.telemetry.pout_mw;

        let mut buf = [0u8; 8];
        let pct = fmt_efficiency(&mut buf, pin_mw, pout_mw);
        let text_w = (EFF_LABEL.len() + pct.len()) as u8 * FONT_W;
        let x = COL_EFF_RIGHT.saturating_sub(text_w);
        self.display.draw_str(x, ROW_STATUS, EFF_LABEL);
        self.display
            .draw_str(x + EFF_LABEL.len() as u8 * FONT_W, ROW_STATUS, pct);

        // Bar caps at 100 % so a measurement-error reading above unity still
        // shows a full bar next to its `>100%` label.
        let bar_pct = if pin_mw > 0 {
            let pin = pin_mw as i64;
            let pout = pout_mw as i64;
            (pout.clamp(0, pin) * 100 / pin) as u32
        } else {
            0
        };
        draw_bar(
            &mut self.display,
            EFF_BAR_LEFT,
            EFF_BAR_RIGHT,
            ROW_STATUS,
            bar_pct,
            EFF_RANGE,
        );
    }

    /// Bottom line while the CFG "Output V sweep" is armed, running, or done.
    ///
    /// Clears the entire row first so the previous `MAIN`/`AUTO` tag and the
    /// efficiency readout cannot ghost through underneath it.
    fn draw_sweep_status(&mut self, app: &AppState) {
        self.display.fill_rect(0, ROW_STATUS, DISPLAY_W, 8);
        self.display.draw_str(0, ROW_STATUS, ">SWEEP");

        match app.sweep.phase {
            SweepPhase::Armed => {
                draw_str_right(&mut self.display, ROW_STATUS, "ENC TO START");
            }
            SweepPhase::Running => {
                let mut idx_buf = [0u8; 4];
                let idx = fmt_u8(&mut idx_buf, app.sweep.index + 1);
                let mut pct_buf = [0u8; 4];
                let pct = fmt_percent(
                    &mut pct_buf,
                    (((app.sweep.index as u16) + 1) * 100 / SWEEP_POINTS as u16) as u8,
                );

                // Right-justify "n N%" as one unit so the numbers do not jitter.
                let x0 = DISPLAY_W
                    .saturating_sub((idx.len() + 1 + pct.len() + 1) as u8 * FONT_W);
                self.display.draw_str(x0, ROW_STATUS, idx);
                let x = x0 + idx.len() as u8 * FONT_W;
                self.display.draw_str(x, ROW_STATUS, " ");
                let x = x + FONT_W;
                self.display.draw_str(x, ROW_STATUS, pct);
                self.display
                    .draw_str(x + pct.len() as u8 * FONT_W, ROW_STATUS, "%");
            }
            SweepPhase::Done => {
                draw_str_right(&mut self.display, ROW_STATUS, "DONE");
            }
            SweepPhase::Off => {}
        }
    }

    /// EEPROM flashing progress screen (title, message, percent bar).
    pub async fn draw_eeprom_screen(
        &mut self,
        i2c: &mut I2c<'_, Async, Master>,
        app: &AppState,
        title: &str,
        message: &str,
        progress_percent: u8,
    ) -> Result<(), ()> {
        // Full redraw — completely different layout from the power screen.
        self.display.clear();
        self.draw_temp_header(app, "EE");

        self.display.draw_str(0, 22, title);
        self.display.draw_str(0, 34, message);
        // 8-px bar at rows 47..54, keeping row 55 clear above the percentage
        // text on the status row.
        draw_percent_bar(&mut self.display, 47, progress_percent);

        let mut pct_buf = [0u8; 4];
        let pct = fmt_percent(&mut pct_buf, progress_percent);
        let x = DISPLAY_W.saturating_sub((pct.len() as u8 + 1) * FONT_W);
        self.display.draw_str(x, ROW_STATUS, pct);
        self.display
            .draw_str(x + pct.len() as u8 * FONT_W, ROW_STATUS, "%");

        // clear() marks everything dirty → flush_partial sends all pages (equivalent to full flush).
        self.display.flush_partial(i2c).await
    }

    /// Fullscreen CFG (Settings) list: header, then the scrollable option rows
    /// with the highlighted entry filled and inverted.
    pub async fn draw_cfg_screen(
        &mut self,
        i2c: &mut I2c<'_, Async, Master>,
        app: &AppState,
    ) -> Result<(), ()> {
        // Full redraw — different layout from the power screen.
        self.display.clear();
        self.draw_temp_header_for_screen(app, "CFG");
        self.draw_cfg_list(app);
        self.display.flush_partial(i2c).await
    }

    // ── CFG screen private helpers ──────────────────────────────────────────

    /// CFG list rows. Four 11-px rows fit between the header divider (row 16)
    /// and the panel bottom; the viewport scrolls once the list grows past
    /// [`CFG_VISIBLE_ROWS`].
    fn draw_cfg_list(&mut self, app: &AppState) {
        const ROW0: u8 = 20;
        const PITCH: u8 = 11;

        let total = CFG_ITEMS.len() as u8;
        let scroll = app.ui.cfg_scroll.min(total.saturating_sub(1));
        let show_scrollbar = total > CFG_VISIBLE_ROWS;
        // Keep the right margin clear for the scrollbar when one is shown.
        let row_w = if show_scrollbar {
            DISPLAY_W - 4
        } else {
            DISPLAY_W
        };

        for row in 0..CFG_VISIBLE_ROWS {
            let idx = scroll.saturating_add(row);
            if idx >= total {
                break;
            }
            let y = ROW0 + row * PITCH;
            let label = CFG_ITEMS[idx as usize].label();

            if idx == app.ui.cfg_index {
                // Filled row + inverted glyphs, matching the PD grid's selection.
                self.display.fill_rect(0, y - 1, row_w, 10);
                self.display.draw_str_inverted(2, y, label);
            } else {
                self.display.draw_str(2, y, label);
            }
        }

        if show_scrollbar {
            self.draw_scrollbar(total, scroll);
        }
    }

    /// Thin proportional scrollbar on the right edge, drawn only when the list
    /// overflows the viewport.
    fn draw_scrollbar(&mut self, total: u8, scroll: u8) {
        const COL: u8 = 126;
        const TOP: u8 = 18;
        const BOT: u8 = 62;

        self.display.draw_line(COL, TOP, COL, BOT);

        let track_h = (BOT - TOP + 1) as u16;
        let thumb_h = (track_h * CFG_VISIBLE_ROWS as u16 / total as u16).max(2);
        let max_scroll = total.saturating_sub(CFG_VISIBLE_ROWS).max(1) as u16;
        let max_off = track_h.saturating_sub(thumb_h);
        let off = (max_off * scroll as u16 / max_scroll) as u8;
        let thumb_top = TOP + off;
        let thumb_bot = (thumb_top + thumb_h as u8 - 1).min(BOT);
        self.display
            .draw_line(COL + 1, thumb_top, COL + 1, thumb_bot);
    }

    /// Fullscreen PD contract selection screen with 2×3 grid.
    pub async fn draw_pd_contract_screen(
        &mut self,
        i2c: &mut I2c<'_, Async, Master>,
        app: &AppState,
    ) -> Result<(), ()> {
        // Full redraw — completely different layout from the power screen.
        self.display.clear();
        self.draw_pd_header(app);
        self.draw_pd_grid(app);
        self.draw_pd_footer(app);
        self.display.flush_partial(i2c).await
    }

    // ── PD screen private helpers ───────────────────────────────────────────

    fn draw_pd_header(&mut self, app: &AppState) {
        self.draw_temp_header_for_screen(app, "PD");
    }

    /// Draw 2×3 grid of preset voltage buttons with bordered boxes.
    fn draw_pd_grid(&mut self, app: &AppState) {
        // Grid layout: 3 columns × 2 rows
        // Each box: ~22px wide × 10px tall (bordered)
        const BOX_W: u8 = 22;
        const BOX_H: u8 = 10;
        const GAP_X: u8 = 10;
        const GAP_Y: u8 = 14;
        const START_X: u8 = 14; // center the 3 boxes
        const START_Y_ROW0: u8 = 22;
        const START_Y_ROW1: u8 = START_Y_ROW0 + GAP_Y;

        // In manual mode the cursor selects a preset; in Auto the highlighted
        // cell tracks the rail the chooser derived from the output setpoint.
        let selected: Option<usize> = match app.pd_control.mode {
            PdMode::Auto if app.pd_control.target_mv > 0 => {
                Some(nearest_preset_index(app.pd_control.target_mv) as usize)
            }
            PdMode::Auto => None,
            PdMode::Manual => Some(app.ui.pd_profile_index as usize),
        };

        for (idx, _volt_mv) in PD_PRESET_VOLTAGES_MV.iter().enumerate() {
            let row = idx / 3;
            let col = idx % 3;
            let x = START_X + (col as u8 * (BOX_W + GAP_X));
            let y = if row == 0 { START_Y_ROW0 } else { START_Y_ROW1 };
            let is_selected = selected == Some(idx);

            // Draw bordered box
            self.display.draw_line(x, y, x + BOX_W - 1, y); // top
            self.display
                .draw_line(x, y + BOX_H - 1, x + BOX_W - 1, y + BOX_H - 1); // bottom
            self.display.draw_line(x, y, x, y + BOX_H - 1); // left
            self.display
                .draw_line(x + BOX_W - 1, y, x + BOX_W - 1, y + BOX_H - 1); // right

            // Fill selected box with white pixels
            if is_selected {
                let mut ry = y + 1;
                while ry < y + BOX_H - 1 {
                    self.display.draw_line(x + 1, ry, x + BOX_W - 2, ry);
                    ry += 1;
                }
            }

            // Draw label centered in box. The selected box is filled, so its
            // label must be drawn inverted to stay readable.
            let label = PRESET_LABELS[idx];
            let label_w = (label.len() as u8) * FONT_W;
            let lx = x + (BOX_W - label_w) / 2;
            let ly = y + (BOX_H - 8) / 2; // 8 = font height
            if is_selected {
                self.display.draw_str_inverted(lx, ly, label);
            } else {
                self.display.draw_str(lx, ly, label);
            }
        }
    }

    fn draw_pd_footer(&mut self, app: &AppState) {
        // Row 48: measured input bus. The INA228 is the accurate source; the
        // ADC fallback is only flagged as missing when the INA228 is absent.
        let mut vin_buf = [0u8; 8];
        let vin = fmt_decimal(&mut vin_buf, app.telemetry.vin_mv);
        self.display.draw_str(0, 48, "Vin");
        self.display.draw_str(3 * FONT_W, 48, vin);
        self.display
            .draw_str(3 * FONT_W + vin.len() as u8 * FONT_W, 48, "V");

        if app.telemetry.ina_ok {
            let mut iin_buf = [0u8; 8];
            let iin = fmt_decimal(&mut iin_buf, app.telemetry.iin_ma.unsigned_abs());
            self.display.draw_str(68, 48, "Iin");
            self.display.draw_str(68 + 3 * FONT_W, 48, iin);
            self.display
                .draw_str(68 + 3 * FONT_W + iin.len() as u8 * FONT_W, 48, "A");
        } else {
            self.display.draw_str(68, 48, "INA!");
        }

        // Row 57: manual/auto mode, chosen rail, region and (auto) policy.
        let ctrl = &app.pd_control;
        self.display.draw_str(0, 57, match ctrl.mode {
            PdMode::Auto => "AUTO",
            PdMode::Manual => "MAN",
        });

        match ctrl.error {
            PdAutoError::NoCable => {
                self.display.draw_str(30, 57, "NO PD");
                return;
            }
            PdAutoError::NoRail => {
                self.display.draw_str(30, 57, "NO RAIL");
                return;
            }
            PdAutoError::None => {}
        }

        if ctrl.target_mv == 0 {
            return;
        }
        let mut rail_buf = [0u8; 6];
        let rail = fmt_int_volts(&mut rail_buf, ctrl.target_mv);
        self.display.draw_str(30, 57, rail);
        let region_x = 30 + rail.len() as u8 * FONT_W + FONT_W;
        let region = match ctrl.region {
            RailRegion::Buck => "BUCK",
            RailRegion::Boost => "BOOST",
            RailRegion::FallbackPower => "PWR",
            RailRegion::Unavailable => "",
        };
        self.display.draw_str(region_x, 57, region);

        match ctrl.mode {
            PdMode::Auto => {
                let policy = match ctrl.policy {
                    crate::state::AutoPolicy::Efficiency => "EFF",
                    crate::state::AutoPolicy::Power => "PWR",
                };
                self.display
                    .draw_str(DISPLAY_W - 3 * FONT_W, 57, policy);
            }
            // BTN2 in Manual requests the highest rail the source offers.
            PdMode::Manual if ctrl.max_request => {
                self.display
                    .draw_str(DISPLAY_W - 3 * FONT_W, 57, "MAX");
            }
            PdMode::Manual => {}
        }
    }

    /// Transient confirmation screen shown after a PD contract request.
    pub async fn draw_pd_contract_result(
        &mut self,
        i2c: &mut I2c<'_, Async, Master>,
        app: &AppState,
        title: &str,
        message: &str,
    ) -> Result<(), ()> {
        // Full redraw — completely different layout from the power screen.
        self.display.clear();
        self.draw_temp_header(app, "PD");

        self.display.draw_str(0, 22, title);
        self.display.draw_str(0, 34, message);

        self.display.draw_str(0, ROW_STATUS, "ENC:OK  BTN3:BACK");
        // clear() marks everything dirty → flush_partial sends all pages (equivalent to full flush).
        self.display.flush_partial(i2c).await
    }

    // ── Temperature header helpers ───────────────────────────────────────────

    /// Draw the header's left-side content: the latched fault label when one is
    /// active, otherwise the temperature badges.
    fn draw_header_label_or_temps(&mut self, app: &AppState) {
        if let Some(label) = app.supply.fault.label() {
            self.display.draw_str(COL_LABEL, ROW_HEADER, label);
        } else {
            let mut buf = [0u8; 20];
            let text = fmt_temps(
                &mut buf,
                app.telemetry.temp_conv_c,
                app.telemetry.temp_input_c,
            );
            self.display.draw_str(COL_LABEL, ROW_HEADER, text);
        }
    }

    /// Draw temperature values in the header zone (optionally a right-justified
    /// badge label) — or the latched fault label in place of temperatures.
    fn draw_temp_header(&mut self, app: &AppState, badge: &str) {
        self.draw_header_label_or_temps(app);
        if !badge.is_empty() {
            self.display.draw_str(92, ROW_HEADER, badge);
        }
    }

    /// Draw temperature values from AppState with a given badge label.
    fn draw_temp_header_for_screen(&mut self, app: &AppState, badge: &str) {
        self.draw_header_label_or_temps(app);
        self.display.draw_str(92, ROW_HEADER, badge);
        self.display
            .draw_line(0, ROW_DIVIDER, DISPLAY_W - 1, ROW_DIVIDER);
    }
}

impl Default for Ssd1306Ui {
    fn default() -> Self {
        Self::new()
    }
}
// ── Unit type ─────────────────────────────────────────────────────────────────

/// Unit suffix for the status line's setpoint readout.  Measurement values in
/// the telemetry grid carry their unit as a string literal instead.
#[derive(Clone, Copy)]
enum Unit {
    Voltage,
    Current,
}

impl Unit {
    fn symbol(self) -> &'static str {
        match self {
            Unit::Voltage => "V",
            Unit::Current => "A",
        }
    }
}

// ── Drawing helpers ───────────────────────────────────────────────────────────

/// Draw one measurement column: clear the column's value field, then draw
/// `value` + unit flush with the column's right edge so readings line up under
/// their headers and never overrun into the neighbouring column.
///
/// ```text
/// "Vout"        header, drawn separately on ROW_OUT_LABELS at x0
/// "  5.000 V"   this function, on ROW_OUT_VALUES, right-justified to `right`
/// ```
fn draw_column_value(
    d: &mut Ssd1306,
    x0: u8,
    right: u8,
    y: u8,
    millivalue: u32,
    unit: &str,
) {
    // Clear the field first: readings are right-justified, so a shorter string
    // would otherwise leave the previous leftmost digit lit (partial refresh
    // only pushes pages whose contents changed).
    d.fill_rect(x0, y, right.saturating_sub(x0), 8);

    let mut buf = [0u8; 8];
    let value = fmt_value(&mut buf, millivalue);
    let total_w = (value.len() + unit.len()) as u8 * FONT_W;
    let x = right.saturating_sub(total_w);
    d.draw_str(x, y, value);
    d.draw_str(x + value.len() as u8 * FONT_W, y, unit);
}

/// Draw a bordered horizontal bar graph spanning `left..=right` (inclusive
/// border columns), inset vertically within the 8-px character cell:
///
/// ```text
/// y+1  ┌─────────────────────────────┐   ← top border (1 px)
/// y+2  │ ████████████░░░░░░░░░░░░░░░ │   ┐
/// y+3  │ ████████████░░░░░░░░░░░░░░░ │   │ fill (3 px)
/// y+4  │ ████████████░░░░░░░░░░░░░░░ │   ┘
/// y+5  └─────────────────────────────┘   ← bottom border (1 px)
/// ```
///
/// `range` is `(min_value, max_value)` in the same unit as `millivalue`.
/// Values outside the range are clamped so the bar always stays within its
/// borders.
fn draw_bar(d: &mut Ssd1306, left: u8, right: u8, y: u8, millivalue: u32, range: (u32, u32)) {
    let (min_v, max_v) = range;
    let top = y + BAR_OFFSET_TOP;
    let bot = y + BAR_OFFSET_BOT;
    let inner_w = right.saturating_sub(left).saturating_sub(1); // usable fill columns

    // Outline rectangle (4 lines)
    d.draw_line(left, top, right, top); // top border
    d.draw_line(left, bot, right, bot); // bottom border
    d.draw_line(left, top, left, bot); // left border
    d.draw_line(right, top, right, bot); // right border

    // Clear the interior before filling: `draw_line` only ever sets pixels, so
    // a falling reading would otherwise leave the previous, longer bar lit.
    let mut clear_row = top + 1;
    while clear_row < bot {
        let mut cx = left + 1;
        while cx < right {
            d.set_pixel(cx, clear_row, false);
            cx += 1;
        }
        clear_row += 1;
    }

    // Filled portion: proportional to (value − min) / (max − min)
    let span = max_v.saturating_sub(min_v).max(1);
    let clamped = millivalue.clamp(min_v, max_v) - min_v;
    let fill_w = ((clamped as u64 * inner_w as u64) / span as u64) as u8;

    if fill_w > 0 {
        let fill_right = left + fill_w; // still ≤ right − 1
                                        // Fill all interior rows (y+2 … y+5)
        let mut row = top + 1;
        while row < bot {
            d.draw_line(left + 1, row, fill_right, row);
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

/// Draw a short string flush with the right edge of a row.
fn draw_str_right(d: &mut Ssd1306, y: u8, s: &str) {
    let x = DISPLAY_W.saturating_sub(s.len() as u8 * FONT_W);
    d.draw_str(x, y, s);
}

/// Draw a setpoint reading right-aligned on the status bar.
///
/// Example: `"SET  5.000 V"` flush with the right edge.
fn draw_setpoint_right(d: &mut Ssd1306, millivalue: u32, unit: Unit) {
    const PREFIX: &str = "SET ";
    // Widest possible "SET " + value + unit field (12 chars).
    const FIELD_W: u8 = 12 * FONT_W;
    let mut buf = [0u8; 8];
    let val = fmt_decimal(&mut buf, millivalue);
    let sym = unit.symbol();

    let total_w = (PREFIX.len() + val.len() + sym.len()) as u8 * FONT_W;
    let x = DISPLAY_W.saturating_sub(total_w);

    // Clear the fixed right-hand field first so a shrinking setpoint can't leave
    // parts of the previous one lit.
    d.fill_rect(DISPLAY_W - FIELD_W, ROW_STATUS, FIELD_W, 8);

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
    // The 8-byte buffer holds at most "9999.999" (4 int digits + '.' + 3 frac).
    // Saturate a larger reading (only reachable during a fault) rather than
    // dropping leading digits or writing past the buffer.
    let millivalue = millivalue.min(9_999_999);
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

/// Write `value` as decimal digits with no leading zeros (at least one digit).
/// Returns the next free index in `buf`.
fn write_uint(buf: &mut [u8], mut i: usize, value: u32) -> usize {
    if value == 0 {
        buf[i] = b'0';
        return i + 1;
    }
    let mut tmp = [0u8; 10];
    let mut ti = tmp.len();
    let mut n = value;
    while n > 0 && ti > 0 {
        ti -= 1;
        tmp[ti] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    for &byte in &tmp[ti..] {
        buf[i] = byte;
        i += 1;
    }
    i
}

/// Format a measurement to at most six characters, buying the extra integer
/// digits by dropping fractional ones.
///
/// The width budget is what the output columns provide — 42 px is seven
/// character cells, i.e. the value plus its unit symbol — so a 240 W reading
/// must render as `240.00` rather than `240.000` to stay inside its column.
///
/// ```text
/// 12_000   →  "12.000"
/// 99_999   →  "99.999"
/// 100_000  →  "100.00"
/// 240_000  →  "240.00"
/// 9_999_999 → "9999.9"
/// ```
fn fmt_value(buf: &mut [u8; 8], millivalue: u32) -> &str {
    let value = millivalue.min(9_999_999);
    if value < 100_000 {
        // Up to "99.999".
        return fmt_decimal(buf, value);
    }

    let mut i = write_uint(buf, 0, value / 1000);
    buf[i] = b'.';
    i += 1;
    let frac = value % 1000;
    if value < 1_000_000 {
        // Up to "999.99".
        buf[i] = b'0' + (frac / 100) as u8;
        i += 1;
        buf[i] = b'0' + ((frac / 10) % 10) as u8;
        i += 1;
    } else {
        // Up to "9999.9".
        buf[i] = b'0' + (frac / 100) as u8;
        i += 1;
    }

    core::str::from_utf8(&buf[..i]).unwrap_or("?.??")
}

/// Format a signed current reading to at most six characters.
///
/// A negative reading spends one character on the sign, so it drops a decimal
/// to stay within the same budget.
///
/// ```text
///     500  →  "0.500"
///    -500  →  "-0.50"
/// -20_000  →  "-20.00"
/// ```
fn fmt_signed_value(buf: &mut [u8; 8], milliunit: i32) -> &str {
    if milliunit >= 0 {
        return fmt_value(buf, milliunit as u32);
    }

    let magnitude = milliunit.unsigned_abs().min(9_999_999);
    let mut i = 0usize;
    buf[i] = b'-';
    i += 1;

    let frac = magnitude % 1000;
    i = write_uint(buf, i, magnitude / 1000);
    if magnitude < 1_000_000 {
        buf[i] = b'.';
        i += 1;
        buf[i] = b'0' + (frac / 100) as u8;
        i += 1;
        if magnitude < 100_000 {
            buf[i] = b'0' + ((frac / 10) % 10) as u8;
            i += 1;
        }
    }

    core::str::from_utf8(&buf[..i]).unwrap_or("-.--")
}

/// Format the input-to-output efficiency as a percentage.
///
/// `pin_mw` is the INA228 input power (signed) and `pout_mw` the ADC output
/// power. A non-positive input power means the ratio is undefined, and a reading
/// above unity is measurement error rather than a real result, so both are
/// called out instead of printing a misleading number.
///
/// The ratio itself comes from [`crate::sense::efficiency::tenths_pct`], the
/// same helper the sweep diagnostics use, so the console and the panel can
/// never disagree about what "efficiency" means.
///
/// ```text
///  (12_000,  10_500)  →  "87.5%"
///  (12_000,  12_060)  →  ">100%"
///  (      0,      0)  →  "--%"
/// ```
fn fmt_efficiency(buf: &mut [u8; 8], pin_mw: i32, pout_mw: u32) -> &str {
    let tenths = match crate::sense::efficiency::tenths_pct(pin_mw, pout_mw) {
        None => return "--%",
        Some(t) if t >= crate::sense::efficiency::ETA_UNITY_TENTHS => return ">100%",
        // `tenths_pct` cannot exceed unity here, so it always fits the buffer.
        Some(t) => t,
    };

    let mut i = write_uint(buf, 0, tenths / 10);
    buf[i] = b'.';
    i += 1;
    buf[i] = b'0' + (tenths % 10) as u8;
    i += 1;
    buf[i] = b'%';
    i += 1;

    core::str::from_utf8(&buf[..i]).unwrap_or("?.?%")
}

/// Format a millivolt value as a short integer-with-unit label, e.g.
/// `36000 → "36V"`, `5000 → "5V"`.
fn fmt_int_volts(buf: &mut [u8; 6], mv: u32) -> &str {
    let v = mv / 1000;
    let mut i = 0usize;
    if v >= 100 {
        buf[i] = b'0' + (v / 100) as u8;
        i += 1;
    }
    if v >= 10 {
        buf[i] = b'0' + ((v / 10) % 10) as u8;
        i += 1;
    }
    buf[i] = b'0' + (v % 10) as u8;
    i += 1;
    buf[i] = b'V';
    i += 1;
    core::str::from_utf8(&buf[..i]).unwrap_or("?V")
}

/// Format an unsigned byte with no padding (0–255).
fn fmt_u8(buf: &mut [u8; 4], value: u8) -> &str {
    let i = if value >= 100 {
        buf[0] = b'0' + value / 100;
        buf[1] = b'0' + (value / 10) % 10;
        buf[2] = b'0' + value % 10;
        3
    } else if value >= 10 {
        buf[0] = b'0' + value / 10;
        buf[1] = b'0' + value % 10;
        2
    } else {
        buf[0] = b'0' + value;
        1
    };

    core::str::from_utf8(&buf[..i]).unwrap_or("?")
}

/// Format a clamped 0–100 percentage without a '%' suffix.
fn fmt_percent(buf: &mut [u8; 4], percent: u8) -> &str {    let percent = percent.min(100);
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

/// Format both temperatures as "T1:XX.0 T2:XX.0".
/// `temp_c` values are in degrees Celsius (i32).
fn fmt_temps(buf: &mut [u8; 20], t1: i32, t2: i32) -> &str {
    let mut i = 0usize;

    // "T1:" prefix
    buf[i] = b'T';
    i += 1;
    buf[i] = b'1';
    i += 1;
    buf[i] = b':';
    i += 1;

    // First temperature: handle negative values
    if t1 < 0 {
        buf[i] = b'-';
        i += 1;
    }
    // Cap the magnitude at 3 digits: this keeps the fixed 20-byte buffer from
    // overflowing for pathological readings (valid NTC temperatures are far
    // below 1000 °C; the failed-sensor sentinel is rendered by the fault label).
    let int1 = t1.unsigned_abs().min(999);
    // Integer part (no leading zeros, at least one digit)
    if int1 == 0 {
        buf[i] = b'0';
        i += 1;
    } else {
        let mut tmp = [0u8; 4];
        let mut ti = tmp.len();
        let mut n = int1;
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
    buf[i] = b'0';
    i += 1;

    // Space separator
    buf[i] = b' ';
    i += 1;

    // "T2:" prefix
    buf[i] = b'T';
    i += 1;
    buf[i] = b'2';
    i += 1;
    buf[i] = b':';
    i += 1;

    // Second temperature
    if t2 < 0 {
        buf[i] = b'-';
        i += 1;
    }
    let int2 = t2.unsigned_abs().min(999);
    if int2 == 0 {
        buf[i] = b'0';
        i += 1;
    } else {
        let mut tmp = [0u8; 4];
        let mut ti = tmp.len();
        let mut n = int2;
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
    buf[i] = b'0';
    i += 1;

    core::str::from_utf8(&buf[..i]).unwrap_or("T1:?.? T2:?.?")
}

/// Preset voltage labels for the PD screen.
const PRESET_LABELS: [&str; 6] = ["12V", "15V", "20V", "28V", "36V", "48V"];
