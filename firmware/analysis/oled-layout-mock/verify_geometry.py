"""Parse the layout constants straight out of ssd1306_ui.rs and assert the
geometry invariants the main screen depends on.

This checks the shipped source, not a transcription: every assertion below is
run against numbers read from the Rust file.
"""
import re

SRC = "/home/tj/nitride-nano/firmware/src/drivers/ssd1306_ui.rs"
FONT_W = 6

text = open(SRC).read()

# ── pull out the u8 layout constants, resolving references between them ───────
consts = {}
for m in re.finditer(r"const (\w+): u8 = ([^;]+);", text):
    name, expr = m.group(1), m.group(2).strip()
    try:
        consts[name] = int(eval(expr, {"__builtins__": {}}, dict(consts)))
    except Exception:
        pass  # depends on a board:: constant; not layout geometry

BOARD = {
    "VBUS_SENSE_NUM": 69_600,
    "VOUT_MIN_MV": 0,
    "VOUT_MAX_MV": 60_000,
    "IOUT_MAX_MA": 20_000,
    "POWER_MAX_MW": 240_000,
}

ranges = {}
for m in re.finditer(r"const (\w+): \(u32, u32\) = ([^;]+);", text):
    name, expr = m.group(1), m.group(2).strip()
    try:
        ranges[name] = eval(expr, {"__builtins__": {}}, {**BOARD, **consts})
    except Exception:
        pass

eff_label = re.search(r'const EFF_LABEL: &str = "([^"]*)";', text).group(1)

fails = []


def need(cond, msg):
    print(("ok   " if cond else "FAIL ") + msg)
    if not cond:
        fails.append(msg)


def c(name):
    assert name in consts, f"constant {name} not parsed"
    return consts[name]


print(f"parsed {len(consts)} u8 constants from ssd1306_ui.rs")
print(f"EFF_LABEL = {eff_label!r}\n")

# ── vertical: six 8-px text rows inside the 48-px blue zone ──────────────────
print("-- vertical budget (text glyphs occupy y..y+6, bars y+1..y+BAR_OFFSET_BOT) --")
rows = [
    ("in values", c("ROW_IN_VALUES")),
    ("in bars", c("ROW_IN_BARS")),
    ("out labels", c("ROW_OUT_LABELS")),
    ("out values", c("ROW_OUT_VALUES")),
    ("out bars", c("ROW_OUT_BARS")),
]
for i in range(len(rows) - 1):
    (n0, y0), (n1, y1) = rows[i], rows[i + 1]
    bar_bot = y0 + c("BAR_OFFSET_BOT")
    # previous row's bar bottom must leave at least one blank row before the
    # next element's top glyph row
    need(bar_bot < y1, f"{n0} bar (ends {bar_bot}) leaves a gap before {n1} (starts {y1})")

need(c("ROW_IN_VALUES") >= c("ROW_DIVIDER") + 2,
     f"first text row {c('ROW_IN_VALUES')} leaves a blank row under the divider "
     f"{c('ROW_DIVIDER')}")

bar_bot = c("ROW_OUT_BARS") + c("BAR_OFFSET_BOT")
need(bar_bot < c("ROW_POWER_BAR"),
     f"out bars (end {bar_bot}) clear the ceiling bar {c('ROW_POWER_BAR')}")
need(c("ROW_POWER_BAR") + 1 < c("ROW_STATUS"),
     f"ceiling bar {c('ROW_POWER_BAR')} leaves a gap above the status row "
     f"{c('ROW_STATUS')}")
need(c("ROW_STATUS") + 6 <= 62,
     f"status glyphs (end {c('ROW_STATUS')+6}) leave the last panel row clear")
need(c("EFF_BAR_LEFT") > 0 and c("EFF_BAR_RIGHT") == 127,
     "efficiency bar spans to the right edge")
# the efficiency bar sits on the status row and must stay off the panel edge
eff_bot = c("ROW_STATUS") + c("BAR_OFFSET_BOT")
need(eff_bot <= 62,
     f"efficiency bar bottom row {eff_bot} leaves the panel edge clear")

# ── horizontal: input row ────────────────────────────────────────────────────
print("\n-- input row fields --")
IN_VALUE_MAX = 6            # fmt_value worst case
IN_UNIT_W = len("V") * FONT_W

vin_label_end = c("COL_IN_LABEL") + len("Vin") * FONT_W - 1
vin_val_x = c("COL_IN_VALUE_RIGHT") - IN_VALUE_MAX * FONT_W
need(vin_val_x > vin_label_end,
     f"Vin value (starts {vin_val_x}) clears its label (ends {vin_label_end})")
vin_unit_end = c("COL_IN_VALUE_RIGHT") + FONT_W - 1
iin_label_x = c("COL_IN2_LABEL")
need(vin_unit_end < iin_label_x,
     f"Vin field (ends {vin_unit_end}) clears the Iin label ({iin_label_x})")

iin_label_end = c("COL_IN2_LABEL") + len("Iin") * FONT_W - 1
iin_val_x = c("COL_IN2_VALUE_RIGHT") - IN_VALUE_MAX * FONT_W
need(iin_val_x > iin_label_end,
     f"Iin value (starts {iin_val_x}) clears its label (ends {iin_label_end})")
iin_unit_end = c("COL_IN2_VALUE_RIGHT") + FONT_W - 1
need(iin_unit_end <= 127, f"Iin field (ends {iin_unit_end}) fits the panel")

# clear fields must cover the widest content they erase
need(c("IN_VALUE_CLEAR_X") <= vin_val_x and
     c("IN_VALUE_CLEAR_X") + c("IN_VALUE_CLEAR_W") - 1 >= vin_unit_end,
     "Vin clear field covers value + unit")
need(c("IN2_VALUE_CLEAR_X") <= iin_val_x and
     c("IN2_VALUE_CLEAR_X") + c("IN2_VALUE_CLEAR_W") - 1 >= iin_unit_end,
     "Iin clear field covers value + unit")
need(c("IN2_VALUE_CLEAR_X") == c("COL_IN2_LABEL") + len("Iin") * FONT_W,
     "'NO PD'/'INA!' starts exactly after the Iin label")
need(c("IN2_VALUE_CLEAR_X") + len("NO PD") * FONT_W - 1 <= 127,
     "'NO PD' fits the panel")

# input bars must not overlap
need(c("IN_BAR0_RIGHT") < c("IN_BAR1_LEFT"),
     f"input bars separated ({c('IN_BAR0_RIGHT')} < {c('IN_BAR1_LEFT')})")
need(c("IN_BAR1_RIGHT") <= 127, "input bar 2 fits the panel")
need(c("OUT_BAR2_RIGHT") <= 127, "output bar 3 fits the panel")

# ── horizontal: output columns ───────────────────────────────────────────────
print("\n-- output columns --")
OUT_VALUE_MAX = 7  # value + unit
cols = [
    ("Vout", c("OUT_COL0_LEFT"), c("OUT_COL0_RIGHT")),
    ("Iout", c("OUT_COL1_LEFT"), c("OUT_COL1_RIGHT")),
    ("Pout", c("OUT_COL2_LEFT"), c("OUT_COL2_RIGHT")),
]
for i, (name, left, right) in enumerate(cols):
    width = right - left
    need(width >= OUT_VALUE_MAX * FONT_W,
         f"{name} column {left}..{right-1} fits {OUT_VALUE_MAX} cells "
         f"({OUT_VALUE_MAX*FONT_W}px)")
    label_w = len(name) * FONT_W
    need(label_w <= width, f"{name} label fits its column")
    if i:
        prev_left, prev_right = cols[i - 1][1], cols[i - 1][2]
        need(prev_right <= left, f"{name} column starts after the previous one")
        # widest previous value must not reach into this column
        prev_val_x = prev_right - OUT_VALUE_MAX * FONT_W
        need(prev_val_x + OUT_VALUE_MAX * FONT_W <= left,
             f"previous {OUT_VALUE_MAX}-cell value ends at or before {left}")

bars = [(c("OUT_BAR0_LEFT"), c("OUT_BAR0_RIGHT")),
        (c("OUT_BAR1_LEFT"), c("OUT_BAR1_RIGHT")),
        (c("OUT_BAR2_LEFT"), c("OUT_BAR2_RIGHT"))]
for i in range(len(bars) - 1):
    need(bars[i][1] < bars[i + 1][0],
         f"output bars separated ({bars[i][1]} < {bars[i+1][0]})")

# ── horizontal: status line ──────────────────────────────────────────────────
print("\n-- status line --")
TAG_MAX = len(">AUTO")            # longest tag drawn by the main screen
tag_end = TAG_MAX * FONT_W - 1
eff_text_max = len(eff_label) + len("99.9%")
eff_x = c("COL_EFF_RIGHT") - eff_text_max * FONT_W
need(tag_end < eff_x,
     f"tag (ends {tag_end}) clears the efficiency text (starts {eff_x})")
need(c("COL_EFF_RIGHT") < c("EFF_BAR_LEFT"),
     f"efficiency text field ends {c('COL_EFF_RIGHT')} before the bar {c('EFF_BAR_LEFT')}")
need(c("EFF_BAR_RIGHT") - c("EFF_BAR_LEFT") + 1 >= 30,
     f"efficiency bar is {c('EFF_BAR_RIGHT')-c('EFF_BAR_LEFT')+1}px wide")

# ── ranges ───────────────────────────────────────────────────────────────────
print("\n-- bar ranges --")
for name in ("VIN_RANGE", "VOUT_RANGE", "IOUT_RANGE", "IIN_RANGE", "POUT_RANGE", "EFF_RANGE"):
    need(name in ranges, f"{name} parsed as a (min, max) pair")
if "EFF_RANGE" in ranges:
    need(ranges["EFF_RANGE"] == (0, 100), f"EFF_RANGE is {ranges['EFF_RANGE']}")
# Each bar must span the channel's full board range.
need(ranges.get("VIN_RANGE") == (0, BOARD["VBUS_SENSE_NUM"]),
     f"VIN_RANGE {ranges.get('VIN_RANGE')}")
need(ranges.get("VOUT_RANGE") == (BOARD["VOUT_MIN_MV"], BOARD["VOUT_MAX_MV"]),
     f"VOUT_RANGE {ranges.get('VOUT_RANGE')}")
need(ranges.get("IOUT_RANGE") == (0, BOARD["IOUT_MAX_MA"]),
     f"IOUT_RANGE {ranges.get('IOUT_RANGE')}")
need(ranges.get("IIN_RANGE") == (0, BOARD["IOUT_MAX_MA"]),
     f"IIN_RANGE {ranges.get('IIN_RANGE')}")
need(ranges.get("POUT_RANGE") == (0, BOARD["POWER_MAX_MW"]),
     f"POUT_RANGE {ranges.get('POUT_RANGE')}")

print("\n-- mock render matches shipped constants --")
# candidates.py draws candidate A with these literals; if the Rust constants
# ever drift from them, the mock I reviewed stops representing the firmware.
for name, want in [
    ("ROW_IN_VALUES", 18), ("ROW_IN_BARS", 25), ("ROW_OUT_LABELS", 32),
    ("ROW_OUT_VALUES", 40), ("ROW_OUT_BARS", 47), ("ROW_POWER_BAR", 54),
    ("ROW_STATUS", 56),
    ("BAR_OFFSET_TOP", 1), ("BAR_OFFSET_BOT", 5),
    ("COL_IN_VALUE_RIGHT", 54), ("COL_IN2_LABEL", 66),
    ("COL_IN2_VALUE_RIGHT", 120),
    ("IN_VALUE_CLEAR_X", 18), ("IN_VALUE_CLEAR_W", 42),
    ("IN2_VALUE_CLEAR_X", 84), ("IN2_VALUE_CLEAR_W", 42),
    ("IN_BAR0_LEFT", 0), ("IN_BAR0_RIGHT", 59),
    ("IN_BAR1_LEFT", 66), ("IN_BAR1_RIGHT", 125),
    ("OUT_COL0_LEFT", 0), ("OUT_COL0_RIGHT", 42),
    ("OUT_COL1_LEFT", 43), ("OUT_COL1_RIGHT", 85),
    ("OUT_COL2_LEFT", 86), ("OUT_COL2_RIGHT", 128),
    ("OUT_BAR0_LEFT", 0), ("OUT_BAR0_RIGHT", 41),
    ("OUT_BAR1_LEFT", 43), ("OUT_BAR1_RIGHT", 84),
    ("OUT_BAR2_LEFT", 86), ("OUT_BAR2_RIGHT", 127),
    ("COL_EFF_RIGHT", 89), ("EFF_BAR_LEFT", 92), ("EFF_BAR_RIGHT", 127),
]:
    need(c(name) == want, f"{name} == {want} (rendered geometry)")

print()
if fails:
    print(f"{len(fails)} CHECK(S) FAILED")
    raise SystemExit(1)
print("ALL GEOMETRY CHECKS PASSED")
