#!/usr/bin/env python3
"""
Independent geometry check for the plane-loss tool.

Rasterises a fab Gerber layer with a small RS-274X parser (a completely separate
code path from pcbnew) and compares the resulting copper area with the copper
area pcbnew reports for the same layer.  A close match validates that the
zone/track/pad extraction used by plane_dc_loss.py is faithful.

Usage:
  .venv/bin/python gerber_area_check.py \
      --gerber /home/tj/nitride-nano/PCB/production/../jlcpcb/... \
      --board  /home/tj/nitride-nano/PCB/nitride-nano.kicad_pcb \
      --layer  F.Cu
"""

from __future__ import annotations

import argparse
import math
import os
import re
import sys
import zipfile

from shapely.affinity import rotate, translate
from shapely.geometry import LineString, Point, Polygon, box
from shapely.ops import unary_union

import pcbnew

STACK = ["F.Cu", "In1.Cu", "In2.Cu", "In3.Cu", "In4.Cu", "B.Cu"]
LAYER_ENUM = {
    "F.Cu": pcbnew.F_Cu, "In1.Cu": pcbnew.In1_Cu, "In2.Cu": pcbnew.In2_Cu,
    "In3.Cu": pcbnew.In3_Cu, "In4.Cu": pcbnew.In4_Cu, "B.Cu": pcbnew.B_Cu,
}
GERBER_BY_LAYER = {
    "F.Cu": "nitride-nano-F_Cu.gtl",
    "In1.Cu": "nitride-nano-PWR_Cu.g1",
    "In2.Cu": "nitride-nano-GND_Cu.g2",
    "In3.Cu": "nitride-nano-In3_Cu.g3",
    "In4.Cu": "nitride-nano-In4_Cu.g4",
    "B.Cu": "nitride-nano-B_Cu.gbl",
}


# --------------------------------------------------------------------------
# Minimal RS-274X parser
# --------------------------------------------------------------------------
def parse_gerber(text):
    """Return (dark_geoms, clear_geoms)."""
    dark, clear = [], []
    apertures = {}
    macros = {}
    cur_ap = None
    polarity = "D"
    interp = 1
    in_region = False
    region_pts = []
    x = y = 0.0
    div = 1e6
    fmt_re = re.compile(r"%FSLAX(\d)(\d)Y(\d)(\d)\*%")
    m = fmt_re.search(text)
    if m:
        div = 10 ** int(m.group(2))

    def emit(geom):
        if geom is None or geom.is_empty:
            return
        (dark if polarity == "D" else clear).append(geom)

    def flush_region():
        nonlocal region_pts
        if len(region_pts) >= 3:
            try:
                p = Polygon(region_pts)
                if not p.is_valid:
                    p = p.buffer(0)
                emit(p)
            except Exception:
                pass
        region_pts = []

    # tokenise: extended commands (%...%) and word commands (....*)
    i = 0
    n = len(text)
    while i < n:
        while i < n and text[i] in " \t\r\n":
            i += 1
        if i >= n:
            break
        if text[i] == "%":
            j = text.find("%", i + 1)
            if j < 0:
                break
            cmd = text[i + 1:j].strip()
            i = j + 1
            if cmd.startswith("ADD"):
                mm = re.match(r"ADD(\d+)([A-Za-z_0-9.]+),?([^*]*)", cmd)
                if mm:
                    num = int(mm.group(1))
                    kind = mm.group(2)
                    params = [float(v) for v in mm.group(3).split("X") if v.strip()]
                    apertures[num] = (kind, params)
            elif cmd.startswith("AM"):
                parts = cmd.split("*")
                name = parts[0][2:]
                prims = []
                for part in parts[1:]:
                    part = part.replace("\n", "").replace("\r", "").strip().strip(",")
                    if part:
                        prims.append(part)
                macros[name] = prims
            elif cmd.startswith("LP"):
                polarity = cmd[2] if len(cmd) > 2 else "D"
            continue
        j = text.find("*", i)
        if j < 0:
            break
        word = text[i:j].strip()
        i = j + 1
        if not word:
            continue
        if word in ("G36",):
            in_region = True
            region_pts = []
            continue
        if word in ("G37",):
            in_region = False
            flush_region()
            continue
        if word in ("G01", "G1"):
            interp = 1
            continue
        if word in ("G02", "G2"):
            interp = 2
            continue
        if word in ("G03", "G3"):
            interp = 3
            continue
        if word.startswith("G04") or word in ("M02", "M00"):
            continue
        if re.fullmatch(r"D(\d+)", word):
            code = int(word[1:])
            if code >= 10:
                cur_ap = code
            continue
        # coordinate / operation word -- KiCad emits combined "G1X..Y..D01*"
        mm = re.match(
            r"(?:G0?([123]))?(?:X(-?\d+))?(?:Y(-?\d+))?"
            r"(?:I(-?\d+))?(?:J(-?\d+))?D0?([123])$", word)
        if not mm:
            continue
        if mm.group(1):
            interp = int(mm.group(1))
        nx = int(mm.group(2)) / div if mm.group(2) else x
        ny = int(mm.group(3)) / div if mm.group(3) else y
        ii = int(mm.group(4)) / div if mm.group(4) else 0.0
        jj = int(mm.group(5)) / div if mm.group(5) else 0.0
        op = int(mm.group(6))
        if in_region:
            if op == 2:
                region_pts = [(nx, ny)]
            elif op == 1:
                region_pts.append((nx, ny))
            x, y = nx, ny
            continue
        if op == 3:
            emit(_flash(cur_ap, nx, ny, apertures, macros))
        elif op == 1:
            pts = _path(x, y, nx, ny, interp, ii, jj)
            if len(pts) >= 2:
                w = _ap_width(cur_ap, apertures)
                emit(LineString(pts).buffer(max(w, 1e-4) / 2, cap_style=1))
        x, y = nx, ny
    return dark, clear


def _ap_width(cur_ap, apertures):
    if cur_ap is None or cur_ap not in apertures:
        return 0.1
    kind, params = apertures[cur_ap]
    if kind.startswith("C") and params:
        return params[0]
    if kind.startswith("R") and params:
        return min(params[0], params[1])
    if kind.startswith("O") and params:
        return min(params[0], params[1])
    return 0.1


def _flash(cur_ap, x, y, apertures, macros):
    if cur_ap is None or cur_ap not in apertures:
        return None
    kind, params = apertures[cur_ap]
    if kind in macros:
        return _macro_geom(macros[kind], params, x, y)
    if kind.startswith("C"):
        return Point(x, y).buffer(params[0] / 2, resolution=16)
    if kind.startswith("R") and len(params) >= 2:
        return box(x - params[0] / 2, y - params[1] / 2,
                   x + params[0] / 2, y + params[1] / 2)
    if kind.startswith("O") and len(params) >= 2:
        return box(x - params[0] / 2, y - params[1] / 2,
                   x + params[0] / 2, y + params[1] / 2)
    # unknown custom aperture: do not invent a size from the parameters
    return None


def _tokens(prim):
    return [t for t in prim.split(",") if t != ""]


def _eval(expr, params):
    """Evaluate a Gerber macro arithmetic expression ($n substitution, 'x' = *)."""
    e = str(expr).strip()
    if not e:
        return 0.0
    e = re.sub(r"\$(\d+)", lambda m: repr(float(params[int(m.group(1)) - 1])), e)
    e = e.replace("x", "*").replace("X", "*")
    if not re.fullmatch(r"[0-9eE+\-*/(). ]+", e):
        raise ValueError(f"unsafe macro expression: {expr!r}")
    return float(eval(e, {"__builtins__": {}}, {}))


def _macro_geom(prims, params, x, y):
    """Evaluate a KiCad aperture macro (outline / circle / centre-line)."""
    pieces = []
    for prim in prims:
        vals = _tokens(prim)
        if not vals:
            continue
        try:
            code = int(float(vals[0]))
        except ValueError:
            continue
        try:
            if code == 4 and len(vals) >= 4:
                n = int(_eval(vals[2], params))
                raw = vals[3:3 + 2 * n]
                coords = [_eval(t, params) for t in raw]
                if len(coords) < 2 * n:
                    continue
                rot = 0.0
                tail = vals[3 + 2 * n:4 + 2 * n]
                if tail:
                    rot = _eval(tail[0], params)
                pts = [(coords[2 * i], coords[2 * i + 1]) for i in range(n)]
                poly = Polygon(pts)
                if not poly.is_valid:
                    poly = poly.buffer(0)
                if rot:
                    poly = rotate(poly, rot, origin=(0, 0))
                pieces.append(translate(poly, x, y))
            elif code == 1 and len(vals) >= 5:
                d = _eval(vals[2], params)
                cx, cy = _eval(vals[3], params), _eval(vals[4], params)
                pieces.append(translate(Point(cx, cy).buffer(d / 2, resolution=16), x, y))
            elif code == 21 and len(vals) >= 8:
                w = _eval(vals[1], params)
                x0, y0 = _eval(vals[2], params), _eval(vals[3], params)
                x1, y1 = _eval(vals[4], params), _eval(vals[5], params)
                pieces.append(translate(
                    LineString([(x0, y0), (x1, y1)]).buffer(w / 2, cap_style=1), x, y))
        except Exception:
            continue
    if not pieces:
        return None
    return unary_union(pieces)


def _path(x0, y0, x1, y1, interp, ii, jj):
    if interp == 1 or (ii == 0 and jj == 0):
        return [(x0, y0), (x1, y1)]
    cx, cy = x0 + ii, y0 + jj
    r0 = math.hypot(x0 - cx, y0 - cy)
    r1 = math.hypot(x1 - cx, y1 - cy)
    r = (r0 + r1) / 2
    a0 = math.atan2(y0 - cy, x0 - cx)
    a1 = math.atan2(y1 - cy, x1 - cx)

    def norm(a):
        while a < 0:
            a += 2 * math.pi
        return a

    if interp == 3:
        sweep = norm(a1 - a0)
    else:
        sweep = -norm(a0 - a1)
    n = max(8, int(abs(sweep) * r / 0.05))
    return [(cx + r * math.cos(a0 + sweep * k / n),
             cy + r * math.sin(a0 + sweep * k / n)) for k in range(n + 1)]


def gerber_area(path):
    with open(path, "r", errors="replace") as fh:
        text = fh.read()
    dark, clear = parse_gerber(text)
    if not dark:
        return 0.0, 0, 0
    u = unary_union(dark)
    if clear:
        u = u.difference(unary_union(clear))
    return u.area, len(dark), len(clear)


# --------------------------------------------------------------------------
# pcbnew reference area for one layer (all nets)
# --------------------------------------------------------------------------
def pcbnew_layer_area(board, layer_name):
    le = LAYER_ENUM[layer_name]
    geoms = []
    for z in board.Zones():
        for lay in z.GetLayerSet().Seq():
            if lay == le and z.HasFilledPolysForLayer(lay):
                ps = z.GetFilledPolysList(lay)
                geoms.extend(_ps_geoms(ps))
    for t in board.GetTracks():
        if t.GetLayer() != le:
            continue
        if t.Type() == pcbnew.PCB_VIA_T:
            if t.GetLayerSet().Contains(le):
                p = t.GetPosition()
                geoms.append(Point(p.x / 1e6, p.y / 1e6)
                             .buffer(t.GetWidth() / 2e6, resolution=16))
        elif t.Type() in (pcbnew.PCB_TRACE_T, pcbnew.PCB_ARC_T):
            s, e = t.GetStart(), t.GetEnd()
            geoms.append(LineString([(s.x / 1e6, s.y / 1e6), (e.x / 1e6, e.y / 1e6)])
                         .buffer(t.GetWidth() / 2e6, cap_style=2))
    for fp in board.GetFootprints():
        for p in fp.Pads():
            if not p.IsOnLayer(le):
                continue
            geoms.extend(_pad_geoms(p, le))
    if not geoms:
        return 0.0
    return unary_union(geoms).area


def _ps_geoms(ps):
    out = []
    for i in range(ps.OutlineCount()):
        o = [(ps.Outline(i).CPoint(k).x / 1e6, ps.Outline(i).CPoint(k).y / 1e6)
             for k in range(ps.Outline(i).PointCount())]
        if len(o) < 3:
            continue
        hs = []
        for j in range(ps.HoleCount(i)):
            h = [(ps.Hole(i, j).CPoint(k).x / 1e6, ps.Hole(i, j).CPoint(k).y / 1e6)
                 for k in range(ps.Hole(i, j).PointCount())]
            if len(h) >= 3:
                hs.append(h)
        g = Polygon(o, hs)
        out.append(g if g.is_valid else g.buffer(0))
    return out


def _pad_geoms(pad, layer):
    ps = pcbnew.SHAPE_POLY_SET()
    try:
        pad.TransformShapeToPolygon(ps, layer, 0, 5000, pcbnew.ERROR_INSIDE)
    except Exception:
        return []
    return _ps_geoms(ps)


# --------------------------------------------------------------------------
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--zip", default="/home/tj/nitride-nano/PCB/production/nitride-nano.zip")
    ap.add_argument("--board", default="/home/tj/nitride-nano/PCB/nitride-nano.kicad_pcb")
    ap.add_argument("--layers", default="F.Cu,In1.Cu,In2.Cu,In3.Cu,In4.Cu,B.Cu")
    args = ap.parse_args()

    board = pcbnew.LoadBoard(args.board)
    if board is None:
        raise SystemExit("cannot load board")

    import tempfile
    print(f"{'layer':<8} {'gerber mm²':>12} {'pcbnew mm²':>12} {'delta':>8}")
    ok = True
    with zipfile.ZipFile(args.zip) as zf:
        names = set(zf.namelist())
        for layer in args.layers.split(","):
            layer = layer.strip()
            gname = GERBER_BY_LAYER[layer]
            if gname not in names:
                print(f"{layer:<8} {'missing in zip':>12}")
                continue
            fd, tmp = tempfile.mkstemp(suffix=".gbr")
            with os.fdopen(fd, "wb") as fh:
                fh.write(zf.read(gname))
            ga, nd, nc = gerber_area(tmp)
            pa = pcbnew_layer_area(board, layer)
            d = 100 * (ga - pa) / pa if pa else 0.0
            print(f"{layer:<8} {ga:12.2f} {pa:12.2f} {d:+7.2f}%")
            if abs(d) > 3.0:
                ok = False
    print("PASS (within 3%)" if ok else "CHECK (a layer differs by >3%)")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
