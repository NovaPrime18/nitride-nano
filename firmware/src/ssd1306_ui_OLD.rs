// ============================================================================
// LEGACY — SUPERSEDED, DO NOT USE.
//
// This file targets the old `ssd1306` + `embedded-graphics` crates and is NOT
// part of the build (not declared in `lib.rs`). The current UI is implemented
// with the hand-rolled framebuffer driver in `drivers/ssd1306.rs` and the
// screen layout in `drivers/ssd1306_ui.rs`. Kept in the tree for historical
// reference only; it is not expected to compile.
// ============================================================================

//! SSD1306 UI module for displaying status messages on the OLED display.

use embedded_graphics::{
    prelude::*,
    pixelcolor::BinaryColor,
    primitives::{Line, Rectangle},
    mono_font::{ascii::FONT_6X8, MonoTextStyleBuilder},
};
use ssd1306::{prelude::*, I2CDisplay};

/// Displays a message on the SSD1306 OLED display.
///
/// # Arguments
/// * `display` - A mutable reference to the SSD1306 display instance.
/// * `message` - The message to display.
pub fn show_message(display: &mut I2CDisplay<_, ssd1306::prelude::I2CInterface<_>>, message: &str) {
    println!("Displaying message: {}", message);
    let style = MonoTextStyleBuilder::new()
        .font(&FONT_6X8)
        .text_color(BinaryColor::On)
        .build();

    display.clear().unwrap();
    Text::with_baseline(message, Point::new(0, 12), style, Baseline::Top).draw(display).unwrap();
    display.flush().unwrap();
}