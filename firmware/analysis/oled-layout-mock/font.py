"""Extract the real 5x7 font from the firmware's ssd1306.rs and provide a
bit-accurate software renderer for the 128x64 panel."""
import re

SRC = "/home/tj/nitride-nano/firmware/src/drivers/ssd1306.rs"


def load_font():
    text = open(SRC).read()
    glyphs = {}
    for m in re.finditer(r"b'([^'])' => \[([^\]]*)\]", text):
        ch = m.group(1)
        vals = [int(v, 16) for v in re.findall(r"0x[0-9A-Fa-f]+", m.group(2))]
        if len(vals) == 5:
            glyphs[ch] = vals
    if "b' ' => [0; 5]" in text:
        glyphs[" "] = [0, 0, 0, 0, 0]
    for c in "abcdefghijklmnopqrstuvwxyz":
        if c.upper() in glyphs:
            glyphs[c] = glyphs[c.upper()]
    return glyphs


FONT = load_font()
FALLBACK = [0x7F, 0x41, 0x41, 0x41, 0x7F]


class Panel:
    def __init__(self, w=128, h=64):
        self.w, self.h = w, h
        self.fb = bytearray(w * h)

    def set_pixel(self, x, y, on):
        if x < 0 or y < 0 or x >= self.w or y >= self.h:
            return
        self.fb[x + y * self.w] = 1 if on else 0

    def fill_rect(self, x, y, w, h):
        for px in range(x, min(x + w, self.w)):
            for py in range(y, min(y + h, self.h)):
                self.set_pixel(px, py, False)

    def draw_char(self, x, y, ch):
        g = FONT.get(ch, FALLBACK)
        for col, bits in enumerate(g):
            for row in range(7):
                self.set_pixel(x + col, y + row, (bits >> row) & 1)

    def draw_char_inverted(self, x, y, ch):
        g = FONT.get(ch, FALLBACK)
        for col, bits in enumerate(g):
            for row in range(7):
                self.set_pixel(x + col, y + row, not ((bits >> row) & 1))

    def draw_str(self, x, y, s):
        for b in s:
            if x > 122:
                break
            self.draw_char(x, y, b)
            x += 6

    def draw_str_inverted(self, x, y, s):
        for b in s:
            if x > 122:
                break
            self.draw_char_inverted(x, y, b)
            x += 6

    def draw_line(self, x0, y0, x1, y1):
        x, y = x0, y0
        dx, dy = abs(x1 - x0), abs(y1 - y0)
        sx = 1 if x0 < x1 else -1
        sy = 1 if y0 < y1 else -1
        err = dx - dy
        while True:
            if x >= 0 and y >= 0:
                self.set_pixel(x, y, True)
            if x == x1 and y == y1:
                break
            e2 = 2 * err
            if e2 > -dy:
                err -= dy
                x += sx
            if e2 < dx:
                err += dx
                y += sy

    def bar(self, left, right, y, bot_off=5):
        """Bordered inline bar matching draw_bar(): top = y+1, bottom = y+bot_off."""
        top, bot = y + 1, y + bot_off
        self.draw_line(left, top, right, top)
        self.draw_line(left, bot, right, bot)
        self.draw_line(left, top, left, bot)
        self.draw_line(right, top, right, bot)

    def fill_bar(self, left, right, y, frac, bot_off=5):
        top, bot = y + 1, y + bot_off
        inner = right - left - 1
        w = int(frac * inner)
        if w <= 0:
            return
        for row in range(top + 1, bot):
            self.draw_line(left + 1, row, left + w, row)

    def ascii(self, ruler=True):
        out = []
        if ruler:
            tens = "".join(str((i // 10) % 10) if i % 10 == 0 else " " for i in range(self.w))
            ones = "".join(str(i % 10) for i in range(self.w))
            out.append("    " + tens)
            out.append("    " + ones)
        for y in range(self.h):
            row = "".join("#" if self.fb[x + y * self.w] else "." for x in range(self.w))
            tag = {0: " <- yellow top", 15: " <- yellow bottom", 16: " <- blue starts",
                   63: " <- bottom"}.get(y, "")
            out.append(f"{y:3d} {row}{tag}")
        return "\n".join(out)


def fmt_decimal(milli):
    milli = min(milli, 9_999_999)
    return f"{milli // 1000}.{milli % 1000:03d}"
