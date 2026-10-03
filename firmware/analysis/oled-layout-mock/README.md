# OLED main-screen layout mock

Design/verification aid for the 128×64 SSD1306 power screen. Not part of the
firmware build.

- `font.py` — parses the real 5×7 glyph table out of
  `src/drivers/ssd1306.rs` and re-implements the framebuffer primitives, so a
  render here matches the panel bit-for-bit.
- `candidates.py` — renders the layouts that were considered (A is the one
  implemented in `src/drivers/ssd1306_ui.rs`). `main_screen.png` is the
  implemented layout at low load, 240 W, and with no PD source;
  `comparison.png` shows the alternatives.
- `verify_geometry.py` — reads the layout constants straight out of
  `src/drivers/ssd1306_ui.rs` and asserts the geometry invariants (rows do not
  overlap, no column can be overrun by its widest value, bars stay on-panel)
  plus that the shipped constants still match the rendered mock.

```
python3 verify_geometry.py     # geometry invariants against the Rust source
python3 candidates.py A        # ASCII dump of the implemented layout
```
