"""Render candidate main-screen layouts for the 128x64 SSD1306.

Geometry notes
--------------
Blue zone rows 16..63 = 48 px.  With a 5-px bar the content plus a 1-px gap
around every element fits exactly, leaving the divider and the panel's last row
clear:

    16      blue divider
    17      gap
    18..24  input values
    25      gap
    26..30  input bars      (5 px)
    31      gap
    32..38  output headers
    39      gap
    40..46  output values
    47      gap
    48..52  output bars     (5 px)
    53      gap
    54      setpoint-ceiling bar
    55      gap
    56..62  status + efficiency
    63      gap

The panel is 128 px wide, 6 px per character cell -> 21 chars/row.
"""
from font import Panel
from PIL import Image, ImageDraw

FW = 6
W, H = 128, 64
ROW_DIVIDER = 16
ROW_STATUS = 56
ROW_POWER_BAR = 54
BAR_BOT_OFF = 5  # bar height: borders at y+1 and y+BAR_BOT_OFF

ROWS = (18, 25, 32, 40, 47, 56)


# ── formatting ────────────────────────────────────────────────────────────────
def fmt_auto(milli):
    """<=6 chars always: 3 decimals below 100, 2 decimals above (matches the
    existing Pout >=100 W rule)."""
    milli = min(milli, 9_999_999)
    if milli < 100_000:
        return f"{milli // 1000}.{milli % 1000:03d}"
    return f"{milli // 1000}.{(milli % 1000) // 10:02d}"


def fmt_eff(pin_mw, pout_mw):
    if pin_mw <= 0 or pout_mw < 0:
        return "--%"
    e = pout_mw * 100.0 / pin_mw
    if e > 99.95:
        return ">100%"
    return f"{e:.1f}%"


def eff_frac(pin_mw, pout_mw):
    if pin_mw <= 0:
        return 0.0
    return min(pout_mw / pin_mw, 1.0)


# ── primitives ────────────────────────────────────────────────────────────────
def header(p, tag="CV", en="ON", temps="T1:30.0 T2:35.0"):
    p.fill_rect(0, 0, W, ROW_DIVIDER)
    p.draw_str(0, 4, temps)
    p.draw_str(92, 4, tag)
    p.draw_str(110, 4, en)
    p.draw_line(0, ROW_DIVIDER, W - 1, ROW_DIVIDER)


def bar(p, left, right, y, frac):
    p.bar(left, right, y, BAR_BOT_OFF)
    p.fill_bar(left, right, y, max(0.0, min(1.0, frac)), BAR_BOT_OFF)


def rj(p, y, right, s):
    p.draw_str(right - len(s) * FW, y, s)


def status_tag(p, app):
    """Left of the status row: the active screen tag."""
    p.draw_str(0, ROW_STATUS, ">" + app["tag"])


def setpoint_right(p, s):
    """CV/CC editing screens keep the SET readout on the right of the status row."""
    rj(p, ROW_STATUS, 126, s)


# ── shared sub-blocks ─────────────────────────────────────────────────────────
def input_block(p, a):
    """Rows 1-2: Vin / Iin values on one line, one bar each beneath."""
    y, yb = ROWS[0], ROWS[1]
    p.draw_str(0, y, "Vin")
    rj(p, y, 54, fmt_auto(a["vin"]))
    p.draw_str(54, y, "V")

    p.draw_str(66, y, "Iin")
    if a["nopd"]:
        p.draw_str(84, y, "NO PD")
    elif not a["ina_ok"]:
        p.draw_str(84, y, "INA!")
    else:
        rj(p, y, 120, fmt_auto(abs(a["iin"])))
        p.draw_str(120, y, "A")

    bar(p, 0, 59, yb, a["vin"] / 69_600)
    bar(p, 66, 125, yb, (max(a["iin"], 0) / 20_000) if (a["ina_ok"] and not a["nopd"]) else 0)


def output_labels(p, y):
    for x, lbl in ((0, "Vout"), (43, "Iout"), (86, "Pout")):
        p.draw_str(x, y, lbl)


def output_values(p, a, y):
    for right, val, unit in ((42, a["vout"], "V"), (85, a["iout"], "A"), (128, a["pout"], "W")):
        s = fmt_auto(val) + unit
        p.draw_str(right - len(s) * FW, y, s)


def output_bars(p, a, y):
    bar(p, 0, 41, y, a["vout"] / 60_000)
    bar(p, 43, 84, y, a["iout"] / 20_000)
    bar(p, 86, 127, y, a["pout"] / 240_000)


def eff_widget(p, a, y, left, right, prefix=True):
    txt = ("Eff " if prefix else "") + fmt_eff(a["pin"], a["pout"])
    rj(p, y, right, txt)
    bar(p, right + 3, right + 3 + 35, y, eff_frac(a["pin"], a["pout"]))


# ── Candidate A: efficiency shares the bottom status line ─────────────────────
def candidate_a(a):
    p = Panel()
    header(p)
    input_block(p, a)
    output_labels(p, ROWS[2])
    output_values(p, a, ROWS[3])
    output_bars(p, a, ROWS[4])
    # 1-px aggregate power bar (pout vs the v_set x i_set ceiling)
    p.fill_rect(0, ROW_POWER_BAR, W, 1)
    cap = max(a["vset"] * a["iset"] // 1000, 1)
    ln = min(a["pout"] * W // cap, W)
    if ln:
        p.draw_line(0, ROW_POWER_BAR, ln - 1, ROW_POWER_BAR)
    status_tag(p, a)
    eff_widget(p, a, ROW_STATUS, left=32, right=89)
    return p


# ── Candidate C: efficiency gets its own row; status line dropped ─────────────
def candidate_c(a):
    p = Panel()
    header(p)
    input_block(p, a)
    output_labels(p, ROWS[2])
    output_values(p, a, ROWS[3])
    output_bars(p, a, ROWS[4])
    p.draw_str(0, ROW_STATUS, "Eff " + fmt_eff(a["pin"], a["pout"]))
    bar(p, 50, 127, ROW_STATUS, eff_frac(a["pin"], a["pout"]))
    return p


# ── Candidate D: efficiency own row; output bars shrink beside the labels ─────
def candidate_d(a):
    p = Panel()
    header(p)
    input_block(p, a)
    # labels with a mini bar in the space to their right
    for x, lbl in ((0, "Vout"), (43, "Iout"), (86, "Pout")):
        p.draw_str(x, ROWS[2], lbl)
    bar(p, 26, 41, ROWS[2], a["vout"] / 60_000)
    bar(p, 69, 84, ROWS[2], a["iout"] / 20_000)
    bar(p, 112, 127, ROWS[2], a["pout"] / 240_000)
    output_values(p, a, ROWS[3])
    p.draw_str(0, ROWS[4], "Eff " + fmt_eff(a["pin"], a["pout"]))
    bar(p, 50, 127, ROWS[4], eff_frac(a["pin"], a["pout"]))
    status_tag(p, a)
    return p


# ── Candidate E: compact output row -> efficiency gets its own row AND the
#    status line survives. Cost: short V/I/P labels and a 5-char value field.
def fmt_fit5(milli):
    milli = min(milli, 9_999_999)
    if milli < 10_000:
        return f"{milli // 1000}.{milli % 1000:03d}"            # 9.999
    if milli < 100_000:
        return f"{milli // 1000}.{(milli % 1000) // 10:02d}"    # 99.99
    return f"{milli // 1000}.{(milli % 1000) // 100}"           # 999.9


def candidate_e(a):
    p = Panel()
    header(p)
    input_block(p, a)
    for x, lbl, val in ((0, "V", a["vout"]), (42, "I", a["iout"]), (84, "P", a["pout"])):
        p.draw_str(x, ROWS[2], f"{lbl} {fmt_fit5(val)}")
    output_bars(p, a, ROWS[3])
    p.draw_str(0, ROWS[4], "Eff " + fmt_eff(a["pin"], a["pout"]))
    bar(p, 60, 127, ROWS[4], eff_frac(a["pin"], a["pout"]))
    p.fill_rect(0, 56, W, 1)
    cap = max(a["vset"] * a["iset"] // 1000, 1)
    ln = min(a["pout"] * W // cap, W)
    if ln:
        p.draw_line(0, 56, ln - 1, 56)
    status_tag(p, a)
    return p


CANDIDATES = {"A": candidate_a, "C": candidate_c, "D": candidate_d, "E": candidate_e}

CASES = {
    "low": dict(vin=12_000, iin=500, vout=5_000, iout=500, pout=2_500, pin=6_000,
                nopd=False, ina_ok=True, tag="MAIN", vset=5_000, iset=5_000),
    "high": dict(vin=48_000, iin=5_000, vout=36_000, iout=6_400, pout=230_400, pin=240_000,
                 nopd=False, ina_ok=True, tag="AUTO", vset=36_000, iset=6_500),
    "nopd": dict(vin=0, iin=0, vout=0, iout=0, pout=0, pin=0,
                 nopd=True, ina_ok=False, tag="MAIN", vset=5_000, iset=5_000),
}


def render_png(p, scale=3):
    img = Image.new("RGB", (W, H), (0, 0, 0))
    px = img.load()
    for y in range(H):
        for x in range(W):
            if p.fb[x + y * W]:
                px[x, y] = (255, 205, 0) if y < 16 else (120, 200, 255)
    return img.resize((W * scale, H * scale), Image.NEAREST)


def sheet():
    """One PNG per candidate, plus a labelled comparison sheet."""
    tiles = []
    for name in ("A", "C", "E"):
        for case in ("low", "high", "nopd"):
            p = CANDIDATES[name](CASES[case])
            png = render_png(p)
            png.save(f"cand{name}_{case}.png")
            tiles.append((f"{name}:{case}", png))
    cols, rows = 3, (len(tiles) + 2) // 3
    tw, th = 128 * 3, 64 * 3
    pad, label_h = 10, 18
    comp = Image.new("RGB", (cols * (tw + pad) + pad, rows * (th + pad + label_h) + pad), (25, 25, 30))
    d = ImageDraw.Draw(comp)
    for i, (name, png) in enumerate(tiles):
        cx, cy = i % cols, i // cols
        x = pad + cx * (tw + pad)
        y = pad + cy * (th + pad + label_h)
        comp.paste(png, (x, y + label_h))
        d.text((x + 2, y + 4), name, fill=(255, 255, 255))
    comp.save("comparison.png")
    return tiles


def main_screen_sheet():
    """`main_screen.png`: the implemented layout at three load conditions."""
    cases = (("low", "low load 12V -> 5V"),
             ("high", "240W load 48V -> 36V"),
             ("nopd", "no PD source"))
    tiles = [(label, render_png(candidate_a(CASES[case]))) for case, label in cases]
    tw, th, pad, lh = 128 * 3, 64 * 3, 12, 20
    img = Image.new("RGB", (len(tiles) * (tw + pad) + pad, th + pad + lh + pad), (24, 24, 28))
    d = ImageDraw.Draw(img)
    for i, (label, png) in enumerate(tiles):
        x = pad + i * (tw + pad)
        d.text((x + 2, pad), label, fill=(255, 255, 255))
        img.paste(png, (x, pad + lh))
    img.save("main_screen.png")


if __name__ == "__main__":
    import sys
    sheet()
    main_screen_sheet()
    which = sys.argv[1:]
    for name in (which or ["A"]):
        for case in ("high", "nopd"):
            print(f"===== CANDIDATE {name} / {case} =====")
            print(CANDIDATES[name](CASES[case]).ascii())
            print()
