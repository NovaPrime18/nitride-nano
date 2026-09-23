#!/usr/bin/env python3
"""
DC I^2R plane-loss analysis for the nitride-nano buck/boost power pours.

Method
------
For each target net, all copper of that net is extracted from the KiCad board
(filled zone polygons, tracks, pads, via pads), rasterised onto a uniform grid
and turned into a resistor network:

  in-plane      R = Rs per square between adjacent conductor cells, Rs = rho/t
  out-of-plane  each via is a barrel resistor coupling every layer it passes,
                including layers where the net has no pour
  terminals     each terminal group is shorted to a bus node with a stiff
                conductance; a 1 A current is injected bus-to-bus and the
                resulting dV is the effective resistance of that net.

The system is linear, so each net is solved once (I = 1 A) and every operating
point is then P = I^2 * R analytically.  Power is taken as
P = sum_edges g*dV^2 and cross-checked against P = I*dV.

Run with the venv:  .venv/bin/python plane_dc_loss.py --help
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import subprocess
import sys
import tempfile
import time
from collections import OrderedDict, defaultdict

import numpy as np
import scipy.sparse as sp
import scipy.sparse.linalg as spla
from scipy.sparse.csgraph import connected_components
from shapely import contains_xy
from shapely.geometry import LineString, Point, Polygon
from shapely.ops import unary_union

import pcbnew

# --------------------------------------------------------------------------
# Constants
# --------------------------------------------------------------------------
RHO20 = 1.72e-8            # ohm*m, copper at 20 C
ALPHA_CU = 0.00393         # 1/K
BOARD_THICKNESS = 1.6e-3
VIA_DRILL = 0.3e-3
VIA_PAD = 0.6e-3
VIA_PLATING = 25e-6
N_LAYER_GAPS = 5

STACK = ["F.Cu", "In1.Cu", "In2.Cu", "In3.Cu", "In4.Cu", "B.Cu"]
LAYER_ENUM = {
    "F.Cu": pcbnew.F_Cu, "In1.Cu": pcbnew.In1_Cu, "In2.Cu": pcbnew.In2_Cu,
    "In3.Cu": pcbnew.In3_Cu, "In4.Cu": pcbnew.In4_Cu, "B.Cu": pcbnew.B_Cu,
}
# board.GetLayerName() returns the *user* name (In1 -> "PWR.Cu", In2 -> "GND.Cu"),
# so map the enum back to the canonical stack name ourselves.
NAME_BY_ENUM = {v: k for k, v in LAYER_ENUM.items()}
THICKNESS_DEFAULT = 70e-6   # 2 oz, as ordered on all six layers

# --------------------------------------------------------------------------
# Nets under test.  cases = (current_key, [+]group, [-]group);
# a group is a list of (reference, pad number).
# --------------------------------------------------------------------------
NETS = OrderedDict([
    ("VPP", dict(path="XT90", cases=[("Iin", [("J6", "1")], [("F1", "1")])])),
    ("+VBUS", dict(path="USB", cases=[("Iin", [("FB9", "2")], [("Q10", "5")])])),
    ("/Power-Sense/+VBUS_SENSED", dict(cases=[
        ("Iin", [("F1", "2"), ("Q10", "6")], [("R60", "1")])])),    ("/Converter/PWR_UNREG_IN", dict(cases=[
        ("Iin", [("R60", "2")], [("FB1", "1"), ("FB2", "1"), ("FB3", "1"), ("FB4", "1")])])),
    ("Net-(U1-VIN)", dict(cases=[
        ("Iin", [("FB1", "2"), ("FB2", "2"), ("FB3", "2"), ("FB4", "2")],
                [("Q1", "3"), ("Q1", "5")])])),
    ("Net-(D1-A)", dict(cases=[
        ("IL", [("Q1", "2"), ("Q1", "4"), ("Q2", "3"), ("Q2", "5")], [("R9", "1")])])),
    ("Net-(L1-Pad1)", dict(cases=[("IL", [("R9", "2")], [("L1", "1")])])),
    ("Net-(D2-A)", dict(cases=[
        ("IL", [("L1", "2")], [("Q3", "3"), ("Q3", "5"), ("Q4", "2"), ("Q4", "4")])])),
    ("Net-(D3-K)", dict(cases=[("Iout", [("Q4", "3"), ("Q4", "5")], [("R18", "1")])])),
    ("Net-(D20-A)", dict(cases=[("Iout", [("R18", "2")], [("Q5", "S"), ("Q12", "S")])])),
    ("Net-(FB5-Pad1)", dict(cases=[
        ("Iout", [("Q5", "D"), ("Q12", "D")],
                 [("FB5", "1"), ("FB6", "1"), ("FB7", "1"), ("FB8", "1")])])),
    ("/Converter/PWR_REG_OUT", dict(cases=[
        ("Iout", [("FB5", "2"), ("FB6", "2"), ("FB7", "2"), ("FB8", "2")], [("J3", "2")])])),
    ("GND", dict(pitch_scale=2.5, cases=[
        ("Iin", [("Q2", "2"), ("Q2", "4")], [("J6", "2"), ("J6", "3"), ("J6", "4")]),
        ("Iout", [("Q3", "2"), ("Q3", "4")], [("J3", "1"), ("J3", "3"), ("J3", "4")]),
    ])),
])

SCENARIOS = [
    dict(name="S1 buck 48->5V 20A", vin=48, vout=5.0, iout=20.0, eta=0.95, input="XT90"),
    dict(name="S2 buck 20->5V 20A", vin=20, vout=5.0, iout=20.0, eta=0.95, input="XT90"),
    dict(name="S3 buck 48->18V 18A", vin=48, vout=18.0, iout=18.0, eta=0.95, input="XT90"),
    dict(name="S4 buck 48->14.5V 14.5A", vin=48, vout=14.5, iout=14.5, eta=0.95, input="XT90"),
    dict(name="S5 boost 12->48V 5A", vin=12, vout=48.0, iout=5.0, eta=0.95, input="XT90"),
    dict(name="S6 boost 20->48V 5A", vin=20, vout=48.0, iout=5.0, eta=0.95, input="XT90"),
    dict(name="S7 boost 24->60V 4A", vin=24, vout=60.0, iout=4.0, eta=0.95, input="XT90"),
    dict(name="S8 4-switch 24->24V 10A", vin=24, vout=24.0, iout=10.0, eta=0.95,
         input="XT90", note="transition band, IL up to ~2x Iout (worst case)"),
    dict(name="S9 USB-C 48->24V 5A", vin=48, vout=24.0, iout=5.0, eta=0.95,
         input="USB", note="USB-C input path (through +VBUS)"),
]


def scenario_currents(sc):
    iin = sc["vout"] * sc["iout"] / (sc["vin"] * sc["eta"])
    if sc["vout"] < sc["vin"]:
        il = sc["iout"]
    elif sc["vout"] > sc["vin"]:
        il = iin
    else:
        il = 2.0 * sc["iout"]
    return dict(Iin=iin, Iout=sc["iout"], IL=il)


# --------------------------------------------------------------------------
# Board loading
# --------------------------------------------------------------------------
def load_board(args):
    path = args.board
    if args.git_rev:
        out = subprocess.run(
            ["git", "-C", args.repo, "show", f"{args.git_rev}:{args.rev_path}"],
            check=True, stdout=subprocess.PIPE)
        fd, tmp = tempfile.mkstemp(suffix=".kicad_pcb", prefix="plane-loss-rev-")
        with os.fdopen(fd, "wb") as fh:
            fh.write(out.stdout)
        path = tmp
        print(f"[board] {args.git_rev}:{args.rev_path} -> {tmp} "
              f"({len(out.stdout)/1e6:.1f} MB)")
    board = pcbnew.LoadBoard(path)
    if board is None:
        raise SystemExit(f"failed to load board: {path}")
    return board, path


# --------------------------------------------------------------------------
# Geometry extraction
# --------------------------------------------------------------------------
def _chain_pts(ch):
    return [(ch.CPoint(k).x / 1e6, ch.CPoint(k).y / 1e6) for k in range(ch.PointCount())]


def polyset_to_geoms(ps):
    out = []
    for i in range(ps.OutlineCount()):
        outer = _chain_pts(ps.Outline(i))
        if len(outer) < 3:
            continue
        holes = []
        for j in range(ps.HoleCount(i)):
            h = _chain_pts(ps.Hole(i, j))
            if len(h) >= 3:
                holes.append(h)
        try:
            g = Polygon(outer, holes)
        except Exception:
            continue
        if not g.is_valid:
            g = g.buffer(0)
        if not g.is_empty:
            out.append(g)
    return out


def pad_geoms(pad, layer):
    ps = pcbnew.SHAPE_POLY_SET()
    try:
        pad.TransformShapeToPolygon(ps, layer, 0, 5000, pcbnew.ERROR_INSIDE)
    except Exception:
        return []
    return polyset_to_geoms(ps)


def _sample_arc(s, m, e, n=24):
    ax, ay = s.x / 1e6, s.y / 1e6
    bx, by = m.x / 1e6, m.y / 1e6
    cx, cy = e.x / 1e6, e.y / 1e6
    d = 2 * (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by))
    if abs(d) < 1e-12:
        return [(ax, ay), (cx, cy)]
    ux = ((ax**2 + ay**2) * (by - cy) + (bx**2 + by**2) * (cy - ay)
          + (cx**2 + cy**2) * (ay - by)) / d
    uy = ((ax**2 + ay**2) * (cx - bx) + (bx**2 + by**2) * (ax - cx)
          + (cx**2 + cy**2) * (bx - ax)) / d
    r = math.hypot(ax - ux, ay - uy)
    a0 = math.atan2(ay - uy, ax - ux)
    a2 = math.atan2(cy - uy, cx - ux)

    def norm(a):
        while a < 0:
            a += 2 * math.pi
        return a

    n0, n2 = norm(a0), norm(a2)
    sweep = norm(n2 - n0)
    return [(ux + r * math.cos(n0 + sweep * k / n),
             uy + r * math.sin(n0 + sweep * k / n)) for k in range(n + 1)]


class NetGeometry:
    def __init__(self):
        self.zone = {l: [] for l in STACK}
        self.extra = {l: [] for l in STACK}
        self.pads = defaultdict(dict)
        self.vias = []
        self.arcs = 0

    def copper(self, l):
        return self.zone[l] + self.extra[l]

    def bounds(self):
        gs = [g for l in STACK for g in self.copper(l)]
        return None if not gs else unary_union(gs).bounds

    def layer_area(self, l):
        return 0.0 if not self.copper(l) else unary_union(self.copper(l)).area

    def zone_area(self, l):
        return 0.0 if not self.zone[l] else unary_union(self.zone[l]).area


def extract_net(board, netname):
    ng = NetGeometry()
    for z in board.Zones():
        if str(z.GetNetname()) != netname:
            continue
        for lay in z.GetLayerSet().Seq():
            if not z.HasFilledPolysForLayer(lay):
                continue
            name = NAME_BY_ENUM.get(lay)
            if name in ng.zone:
                ng.zone[name].extend(polyset_to_geoms(z.GetFilledPolysList(lay)))

    for t in board.GetTracks():
        if str(t.GetNetname()) != netname:
            continue
        tt = t.Type()
        if tt == pcbnew.PCB_VIA_T:
            pos = t.GetPosition()
            span = [i for i, l in enumerate(STACK)
                    if t.GetLayerSet().Contains(LAYER_ENUM[l])]
            ng.vias.append((pos.x / 1e6, pos.y / 1e6, span))
            disc = Point(pos.x / 1e6, pos.y / 1e6).buffer(VIA_PAD / 2e3, resolution=16)
            for i in span:
                ng.extra[STACK[i]].append(disc)
        elif tt in (pcbnew.PCB_TRACE_T, pcbnew.PCB_ARC_T):
            name = NAME_BY_ENUM.get(t.GetLayer())
            if name not in ng.extra:
                continue
            w = t.GetWidth() / 1e6
            s, e = t.GetStart(), t.GetEnd()
            if tt == pcbnew.PCB_TRACE_T:
                pts = [(s.x / 1e6, s.y / 1e6), (e.x / 1e6, e.y / 1e6)]
            else:
                pts = _sample_arc(s, t.GetMid(), e)
                ng.arcs += 1
            ng.extra[name].append(LineString(pts).buffer(w / 2, cap_style=2))

    for fp in board.GetFootprints():
        ref = str(fp.GetReference())
        for p in fp.Pads():
            if str(p.GetNetname()) != netname:
                continue
            pn = str(p.GetNumber())
            for l in STACK:
                if not p.IsOnLayer(LAYER_ENUM[l]):
                    continue
                gs = pad_geoms(p, LAYER_ENUM[l])
                if gs:
                    ng.pads[(ref, pn)].setdefault(l, []).extend(gs)
                    ng.extra[l].extend(gs)
    return ng


# --------------------------------------------------------------------------
# Rasterisation
# --------------------------------------------------------------------------
def rasterize(geom, bounds, pitch, nsub):
    minx, miny, maxx, maxy = bounds
    nx = int(math.ceil((maxx - minx) / pitch)) + 2
    ny = int(math.ceil((maxy - miny) / pitch)) + 2
    cov = np.zeros((nx, ny), dtype=np.float32)
    if geom is None or geom.is_empty:
        return cov, nx, ny, minx, miny
    sub = (np.arange(nsub) + 0.5) / nsub
    xs = minx + (np.arange(nx)[:, None] + sub[None, :]) * pitch
    ys = miny + (np.arange(ny)[:, None] + sub[None, :]) * pitch
    for a in range(nsub):
        for b in range(nsub):
            Xg, Yg = np.meshgrid(xs[:, a], ys[:, b], indexing="ij")
            cov += contains_xy(geom, Xg, Yg)
    cov /= float(nsub * nsub)
    return cov, nx, ny, minx, miny


class Grid:
    def __init__(self, ng, pitch, nsub=3, cov_min=0.05, pad_sub=3):
        self.pitch = pitch
        self.cov_min = cov_min
        self._ng = ng
        self._pad_sub = pad_sub
        bounds = ng.bounds()
        if bounds is None:
            raise ValueError("net has no copper geometry")
        self.bounds = bounds
        self.cov, self.zcov, self.ids, self.dims = {}, {}, {}, {}
        lay, zfrac = [], []
        offset = 0
        for li, l in enumerate(STACK):
            cond = unary_union(ng.copper(l)) if ng.copper(l) else None
            zone = unary_union(ng.zone[l]) if ng.zone[l] else None
            c_cov, nx, ny, mx, my = rasterize(cond, bounds, pitch, nsub)
            z_cov, _, _, _, _ = rasterize(zone, bounds, pitch, nsub)
            ids = np.full((nx, ny), -1, dtype=np.int64)
            m = c_cov > cov_min
            ids[m] = np.arange(offset, offset + int(m.sum()))
            offset += int(m.sum())
            self.cov[l], self.zcov[l], self.ids[l] = c_cov, z_cov, ids
            self.dims[l] = (nx, ny, mx, my)
            lay.extend([li] * int(m.sum()))
            zf = np.zeros_like(c_cov)
            nz = c_cov > 1e-6
            zf[nz] = z_cov[nz] / c_cov[nz]
            zfrac.extend(zf[m].tolist())
        self.n = offset
        self.cell_layer = np.asarray(lay, dtype=np.int32)
        self.cell_zone = np.asarray(zfrac, dtype=np.float32)

    def total_cell_area(self):
        return sum(float((self.ids[l] >= 0).sum()) for l in STACK) * self.pitch ** 2

    def terminal_cells(self, group):
        out = set()
        sub = (np.arange(self._pad_sub) + 0.5) / self._pad_sub
        for key in group:
            per_layer = self._ng.pads.get(key)
            if not per_layer:
                raise KeyError(f"no pad geometry for {key[0]}.{key[1]}")
            for l, gs in per_layer.items():
                g = unary_union(gs)
                nx, ny, mx, my = self.dims[l]
                ids = self.ids[l]
                hit = np.zeros((nx, ny), dtype=bool)
                xs = mx + (np.arange(nx)[:, None] + sub[None, :]) * self.pitch
                ys = my + (np.arange(ny)[:, None] + sub[None, :]) * self.pitch
                for a in range(self._pad_sub):
                    for b in range(self._pad_sub):
                        Xg, Yg = np.meshgrid(xs[:, a], ys[:, b], indexing="ij")
                        hit |= contains_xy(g, Xg, Yg)
                m = hit & (ids >= 0)
                for i, j in zip(*np.where(m)):
                    out.add(int(ids[i, j]))
        return sorted(out)


# --------------------------------------------------------------------------
# Resistor network
# --------------------------------------------------------------------------
class Edges:
    def __init__(self):
        self.i, self.j, self.g, self.lay, self.kind = [], [], [], [], []

    def append(self, i, j, g, lay, kind):
        self.i.append(i); self.j.append(j); self.g.append(g)
        self.lay.append(lay); self.kind.append(kind)

    def finalize(self):
        cat = lambda a, d: (np.concatenate(a) if len(a) else np.zeros(0, d))
        self.i = cat(self.i, np.int64); self.j = cat(self.j, np.int64)
        self.g = cat(self.g, np.float64); self.lay = cat(self.lay, np.int32)
        self.kind = cat(self.kind, np.int8)
        return self

    def subset(self, mask):
        e = Edges()
        e.i, e.j, e.g = self.i[mask], self.j[mask], self.g[mask]
        e.lay, e.kind = self.lay[mask], self.kind[mask]
        return e


def build_edges(grid, ng, rs, rho):
    inv_rs = 1.0 / rs
    E = Edges()
    for li, l in enumerate(STACK):
        ids, c_cov = grid.ids[l], grid.cov[l]
        for axis in (0, 1):
            if axis == 0:
                a, b, ca, cb = ids[:-1, :], ids[1:, :], c_cov[:-1, :], c_cov[1:, :]
            else:
                a, b, ca, cb = ids[:, :-1], ids[:, 1:], c_cov[:, :-1], c_cov[:, 1:]
            m = (a >= 0) & (b >= 0)
            if not m.any():
                continue
            g = inv_rs * np.minimum(ca[m], cb[m]).astype(np.float64)
            k = int(m.sum())
            E.append(a[m].ravel().astype(np.int64), b[m].ravel().astype(np.int64),
                     g.ravel(), np.full(k, li, np.int32), np.zeros(k, np.int8))

    via_area = math.pi / 4 * ((VIA_DRILL + 2 * VIA_PLATING) ** 2 - VIA_DRILL ** 2)
    via_pairs = 0
    for (vx, vy, span) in ng.vias:
        present = []
        for si in span:
            l = STACK[si]
            nx, ny, mx, my = grid.dims[l]
            ii = int(round((vx - mx) / grid.pitch))
            jj = int(round((vy - my) / grid.pitch))
            nid = -1
            if 0 <= ii < nx and 0 <= jj < ny:
                nid = int(grid.ids[l][ii, jj])
            if nid < 0:
                for rad in (1, 2, 3):
                    hit = False
                    for di in range(-rad, rad + 1):
                        for dj in range(-rad, rad + 1):
                            a, b = ii + di, jj + dj
                            if 0 <= a < nx and 0 <= b < ny and grid.ids[l][a, b] >= 0:
                                nid = int(grid.ids[l][a, b]); hit = True; break
                        if hit:
                            break
                    if nid >= 0:
                        break
            if nid >= 0:
                present.append((si, nid))
        for (s1, n1), (s2, n2) in zip(present, present[1:]):
            length = (s2 - s1) * BOARD_THICKNESS / N_LAYER_GAPS
            E.append(np.array([n1]), np.array([n2]),
                     np.array([via_area / (rho * length)]),
                     np.array([-1], np.int32), np.array([1], np.int8))
            via_pairs += 1
    E.finalize()
    E.via_pairs = via_pairs
    return E


def assemble(N, E, bus_i, bus_j, bus_g):
    if len(E.i):
        rows = np.concatenate([E.i, E.j, bus_i, bus_j])
        cols = np.concatenate([E.j, E.i, bus_j, bus_i])
        vals = -np.concatenate([E.g, E.g, bus_g, bus_g])
        diag = (np.bincount(E.i, weights=E.g, minlength=N)
                + np.bincount(E.j, weights=E.g, minlength=N)
                + np.bincount(bus_i, weights=bus_g, minlength=N)
                + np.bincount(bus_j, weights=bus_g, minlength=N))
    else:
        rows, cols = bus_i, bus_j
        vals = -bus_g
        diag = (np.bincount(bus_i, weights=bus_g, minlength=N)
                + np.bincount(bus_j, weights=bus_g, minlength=N))
    L = sp.coo_matrix((vals, (rows, cols)), shape=(N, N)).tocsr()
    return (L + sp.diags(diag)).tocsr()


def solve_reference(L, ref, inject_node, I, direct_max=350_000):
    N = L.shape[0]
    keep = np.ones(N, dtype=bool)
    keep[ref] = False
    A = L[keep][:, keep].tocsc()
    rhs = np.zeros(N)
    rhs[inject_node] += I
    rhs = rhs[keep]
    t0 = time.time()
    method = "direct"
    if A.shape[0] <= direct_max:
        V = spla.spsolve(A, rhs)
        if not np.all(np.isfinite(V)):
            raise RuntimeError("direct solve produced non-finite values")
    else:
        diag = A.diagonal()
        M = sp.diags(1.0 / np.where(np.abs(diag) > 1e-30, diag, 1.0))
        V, info = spla.cg(A, rhs, rtol=1e-11, atol=0.0, maxiter=100000, M=M)
        method = "cg" if info == 0 else f"cg(info={info})"
    Vf = np.zeros(N)
    Vf[keep] = V
    return Vf, method, time.time() - t0


def current_density(grid, E, V, layer):
    """Per-cell |J| in A/m^2 for one layer at the solved potentials."""
    li = STACK.index(layer)
    ids = grid.ids[layer]
    ny = ids.shape[1]
    sel = (E.kind == 0) & (E.lay == li)
    if not sel.any():
        return np.zeros(ids.shape, dtype=np.float32)
    rev = np.full(grid.n, -1, dtype=np.int64)
    m = ids.ravel() >= 0
    rev[ids.ravel()[m]] = np.where(m)[0]
    ei, ej = E.i[sel], E.j[sel]
    cur = E.g[sel] * (V[ei] - V[ej])          # amps from i to j
    fi, fj = rev[ei], rev[ej]
    xi, yi = np.divmod(fi, ny)
    xj, yj = np.divmod(fj, ny)
    dx = np.sign(xj - xi).astype(np.float64)
    dy = np.sign(yj - yi).astype(np.float64)
    c = cur / grid.pitch
    jx = np.zeros(ids.size)
    jy = np.zeros(ids.size)
    np.add.at(jx, fi, -c * dx)
    np.add.at(jx, fj, c * dx)
    np.add.at(jy, fi, -c * dy)
    np.add.at(jy, fj, c * dy)
    return np.hypot(jx, jy).reshape(ids.shape)


def centroid_of_cells(grid, cells):
    if not cells:
        return None
    want = np.asarray(sorted(set(cells)), dtype=np.int64)
    xs, ys = [], []
    for l in STACK:
        ids = grid.ids[l]
        nx, ny, mx, my = grid.dims[l]
        flat = ids.ravel()
        nodes = flat[flat >= 0]
        hit = np.isin(nodes, want)
        if not hit.any():
            continue
        idx = np.where(flat >= 0)[0][hit]
        ii, jj = np.divmod(idx, ny)
        xs.append(mx + (ii + 0.5) * grid.pitch)
        ys.append(my + (jj + 0.5) * grid.pitch)
    if not xs:
        return None
    return (float(np.concatenate(xs).mean()), float(np.concatenate(ys).mean()))


# --------------------------------------------------------------------------
# Per-net solve
# --------------------------------------------------------------------------
def analyze_net(board, netname, spec, args, thickness, temp_c, pitch):
    ng = extract_net(board, netname)
    if ng.bounds() is None:
        return dict(net=netname, error="no copper geometry")
    grid = Grid(ng, pitch, nsub=args.subcell, cov_min=args.cov_min)
    rho = RHO20 * (1.0 + ALPHA_CU * (temp_c - 20.0))
    rs = rho / thickness
    E = build_edges(grid, ng, rs, rho)

    groups = []
    for (key, gp, gn) in spec["cases"]:
        cp = grid.terminal_cells(gp)
        cn = grid.terminal_cells(gn)
        if not cp or not cn:
            return dict(net=netname,
                        error=f"empty terminal cells for {key} ({len(cp)}/{len(cn)})")
        groups.append(dict(key=key, plus=cp, minus=cn,
                           plus_spec=gp, minus_spec=gn))

    ngroups = len(groups)
    N = grid.n + 2 * ngroups
    bus_p = [grid.n + 2 * k for k in range(ngroups)]
    bus_m = [grid.n + 2 * k + 1 for k in range(ngroups)]
    bus_g = args.bus_g

    bi, bj, bg = [], [], []
    for g, bp, bm in zip(groups, bus_p, bus_m):
        for c in g["plus"]:
            bi.append(c); bj.append(bp); bg.append(bus_g)
        for c in g["minus"]:
            bi.append(c); bj.append(bm); bg.append(bus_g)
    bi = np.asarray(bi, np.int64); bj = np.asarray(bj, np.int64)
    bg = np.asarray(bg, np.float64)

    # connectivity
    rows = np.concatenate([E.i, E.j]); cols = np.concatenate([E.j, E.i])
    Adj = sp.coo_matrix((np.ones(rows.size), (rows, cols)), shape=(N, N)).tocsr()
    if len(bi):
        Adj = Adj + sp.coo_matrix((np.ones(bi.size * 2),
                                  (np.concatenate([bi, bj]), np.concatenate([bj, bi]))),
                                  shape=(N, N)).tocsr()
    ncomp, labels = connected_components(Adj, directed=False)
    bus_nodes = bus_p + bus_m
    bus_labels = set(int(labels[b]) for b in bus_nodes)
    keep = np.isin(labels, list(bus_labels))
    floating_cells = int((~keep[:grid.n]).sum())
    floating_area = floating_cells * pitch ** 2

    emask = keep[E.i] & keep[E.j]
    E2 = E.subset(emask)
    L = assemble(N, E2, bi, bj, bg)

    by_layer = defaultdict(float)
    by_type = defaultdict(float)
    cases_out = []
    maps = {}
    methods = set()
    t_solve = 0.0
    p_sum = 0.0
    for k, g in enumerate(groups):
        if int(labels[bus_p[k]]) != int(labels[bus_m[k]]):
            cases_out.append(dict(key=g["key"], error="open circuit"))
            continue
        V, method, dt = solve_reference(L, bus_p[k], bus_m[k], 1.0)
        methods.add(method); t_solve += dt
        dv = float(V[bus_m[k]] - V[bus_p[k]])
        r_eff = dv / 1.0
        dve = V[E2.i] - V[E2.j]
        p_edge = E2.g * dve * dve
        p_edges = float(p_edge.sum())
        cases_out.append(dict(key=g["key"], r_eff=r_eff, p_at_1A=p_edges,
                              dv=dv, method=method, seconds=dt,
                              plus=g["plus_spec"], minus=g["minus_spec"],
                              plus_centroid=centroid_of_cells(grid, g["plus"]),
                              minus_centroid=centroid_of_cells(grid, g["minus"])))
        p_sum += p_edges
        for li in range(len(STACK)):
            m = (E2.kind == 0) & (E2.lay == li)
            if m.any():
                by_layer[STACK[li]] += float(p_edge[m].sum())
        is_plane = E2.kind == 0
        by_layer["via"] += float(p_edge[~is_plane].sum())
        zf = np.minimum(grid.cell_zone[E2.i], grid.cell_zone[E2.j])
        zone_mask = is_plane & (zf > 0.9)
        by_type["zone"] += float(p_edge[zone_mask].sum())
        by_type["track+pad"] += float(p_edge[is_plane & ~zone_mask].sum())
        by_type["via"] += float(p_edge[~is_plane].sum())

        if k == 0:
            for l in STACK:
                maps[l] = current_density(grid, E2, V, l)

    return dict(
        net=netname, path=spec.get("path"), pitch=pitch, rs=rs,
        thickness=thickness, temp_c=temp_c,
        nodes=grid.n, kept_nodes=int(keep[:grid.n].sum()),
        via_pairs=getattr(E, "via_pairs", 0), via_count=len(ng.vias),
        arcs=ng.arcs, floating_cells=floating_cells, floating_area=floating_area,
        layer_area={l: round(ng.layer_area(l), 3) for l in STACK if ng.layer_area(l) > 0},
        zone_area={l: round(ng.zone_area(l), 3) for l in STACK if ng.zone_area(l) > 0},
        cell_area=grid.total_cell_area(),
        cases=cases_out, by_layer=dict(by_layer), by_type=dict(by_type),
        methods=sorted(methods), solve_seconds=t_solve,
        maps=maps, grid=grid,
    )


# --------------------------------------------------------------------------
# Validation helpers
# --------------------------------------------------------------------------
def bar_model(result):
    """Crude R = Rs * l^2 / A, all layers in parallel (order-of-magnitude check)."""
    cases = result.get("cases") or []
    if not cases or "error" in cases[0]:
        return None
    c0 = cases[0]
    p1, p2 = c0.get("plus_centroid"), c0.get("minus_centroid")
    if not p1 or not p2:
        return None
    dist = math.dist(p1, p2)
    area = sum(result["layer_area"].values())
    if area <= 0:
        return None
    return result["rs"] * dist * dist / area


# --------------------------------------------------------------------------
# Reporting
# --------------------------------------------------------------------------
def net_active(result, scenario):
    """VPP only conducts on the XT90 path, +VBUS only on the USB-C path."""
    p = result.get("path")
    return (not p) or (p == scenario.get("input", "XT90"))


def build_report(results, scenarios, args, board_path, meta):
    lines = []
    A = lines.append
    A("# Plane DC I²R loss — nitride-nano buck/boost power pours")
    A("")
    A(f"- Board: `{meta.get('board_label', board_path)}`")
    A(f"- Copper: {meta['thickness']*1e6:.0f} µm on all {len(STACK)} layers "
      f"({meta['thickness']*1e6/35:.0f} oz), ρ = {RHO20:.3g} Ω·m at 20 °C, "
      f"T = {meta['temp_c']:.0f} °C")
    A(f"- Sheet resistance Rs = {meta['rs']*1e3:.4f} mΩ/square")
    A(f"- Grid pitch: {args.pitch:.3f} mm base × {args.subcell}×{args.subcell} "
      f"subcell coverage; GND at ×{NETS['GND'].get('pitch_scale',1.0):.1f}")
    A("")
    A("Method: every copper item of a net (zone fills, tracks, pads, via pads) is "
      "rasterised, made into a sheet-resistance network with via barrels coupling "
      "the layers, and solved for a 1 A injection between terminal buses; "
      "P = Σ g·ΔV².  Nets are linear, so each operating point is P = I²R.")
    A("Excluded: component resistance (FETs, shunts, inductor, ferrites, fuse), "
      "AC/skin effects, and the logic rails.")
    A("")
    A("## Effective resistance per net (I = 1 A solve)")
    A("")
    A("| Net | layer area (mm²) | nodes | vias | floating (mm²) | case | R_eff (mΩ) | solved |")
    A("|---|---|---|---|---|---|---|---|")
    for r in results:
        if r.get("error"):
            A(f"| `{r['net']}` | — | — | — | — | — | **{r['error']}** | — |")
            continue
        area = sum(r["layer_area"].values())
        for c in r["cases"]:
            rc = "—" if "error" in c else f"{c['r_eff']*1e3:.4f}"
            A(f"| `{r['net']}` | {area:.1f} | {r['nodes']} | {r['via_count']} | "
              f"{r['floating_area']:.2f} | {c['key']} | {rc} | {c.get('method','')} |")
    A("")
    A("## Loss per operating point")
    A("")
    hdr = "| Scenario | Vin→Vout | Iout | Iin | IL | " + \
          " | ".join(f"`{r['net']}`" for r in results if not r.get("error")) + " | total |"
    A(hdr)
    A("|" + "---|" * (6 + sum(1 for r in results if not r.get("error"))))
    for sc in scenarios:
        cur = scenario_currents(sc)
        cells = []
        tot = 0.0
        for r in results:
            if r.get("error"):
                continue
            p = 0.0
            if net_active(r, sc):
                for c in r["cases"]:
                    if "error" in c:
                        continue
                    p += cur[c["key"]] ** 2 * c["r_eff"]
            tot += p
            cells.append(f"{p*1e3:.1f}")
        A(f"| {sc['name']} | {sc['vin']}→{sc['vout']} V | {sc['iout']} A | "
          f"{cur['Iin']:.2f} A | {cur['IL']:.2f} A | " +
          " | ".join(cells) + f" | **{tot:.3f} W** |")
    A("")
    A("_All loss figures are milliwatts except the total, which is watts._")
    A("VPP only conducts on the XT90 input path and +VBUS only on the USB-C path.")
    A("")
    # reference operating point = the scenario with the largest total plane loss
    def total_of(sc, res):
        cur = scenario_currents(sc)
        return sum(cur[c["key"]] ** 2 * c["r_eff"]
                   for r in res if not r.get("error") and net_active(r, sc)
                   for c in r["cases"] if "error" not in c)
    ref = max(scenarios, key=lambda s: total_of(s, results)) if scenarios else None
    ref_cur = scenario_currents(ref) if ref else {}
    A(f"## Breakdown by layer and copper type — {ref['name'] if ref else 'n/a'}")
    A("")
    A("Loss in mW at that operating point's own current through each net.")
    A("")
    A("| Net | I (A) | " + " | ".join(STACK) + " | via | zone | track+pad | total |")
    A("|" + "---|" * (len(STACK) + 5))
    for r in results:
        if r.get("error") or not r["cases"]:
            continue
        active = net_active(r, ref) if ref else True
        scale = 0.0
        cur_txt = "— (not on this input path)"
        if active:
            scale = sum(ref_cur.get(c["key"], 0.0) ** 2
                        for c in r["cases"] if "error" not in c)
            cur_txt = "/".join(f"{ref_cur.get(c['key'],0):.2f}"
                               for c in r["cases"] if "error" not in c)
        lay, typ = r["by_layer"], r["by_type"]
        cells = [f"`{r['net']}`", cur_txt]
        tot = 0.0
        for l in STACK:
            v = lay.get(l, 0.0) * scale * 1e3
            tot += v
            cells.append(f"{v:.2f}")
        vias = lay.get("via", 0.0) * scale * 1e3
        zz = typ.get("zone", 0.0) * scale * 1e3
        tp = typ.get("track+pad", 0.0) * scale * 1e3
        tot += vias
        cells += [f"{vias:.2f}", f"{zz:.2f}", f"{tp:.2f}", f"**{tot:.2f}**"]
        A("| " + " | ".join(cells) + " |")
    A("")
    if ref:
        A("## Headline")
        A("")
        # FET losses recorded in PCB/losses.txt for matching operating points
        fet = {"S3 buck 48->18V 18A": 8.91, "S4 buck 48->14.5V 14.5A": 9.61}
        A("| Scenario | plane copper loss | recorded FET loss | plane as % of FET |")
        A("|---|---|---|---|")
        worst = (None, 0.0)
        for sc in scenarios:
            t = total_of(sc, results)
            if t > worst[1]:
                worst = (sc, t)
            f = fet.get(sc["name"])
            A(f"| {sc['name']} | **{t*1e3:.0f} mW** | "
              + (f"{f:.2f} W | {100*t/f:.1f} % |" if f else "— | — |"))
        A("")
        A(f"Highest plane loss: **{worst[1]*1e3:.0f} mW** at {worst[0]['name']}; "
          f"lowest {min(total_of(s, results) for s in scenarios)*1e3:.0f} mW. "
          "Copper-only: excludes R9/R18 (2 mΩ), R60 (8 mΩ), FET Rds(on), "
          "inductor DCR, ferrite and fuse resistance.")
        A("")

    A("## Validation")
    A("")
    A("| Net | R_eff (mΩ) | bar model (mΩ) | ratio | energy check |")
    A("|---|---|---|---|---|")
    for r in results:
        if r.get("error") or not r["cases"] or "error" in r["cases"][0]:
            continue
        bm = r.get("bar_model")
        rr = r["cases"][0]["r_eff"]
        ratio = f"{rr/bm:.2f}" if bm else "—"
        bmv = f"{bm*1e3:.4f}" if bm else "—"
        chk = "ok" if abs(r["cases"][0]["p_at_1A"] - rr) <= 1e-6 + 1e-3 * max(rr, 1e-12) else "MISMATCH"
        A(f"| `{r['net']}` | {rr*1e3:.4f} | {bmv} | {ratio} | {chk} |")
    A("")
    return "\n".join(lines)


def write_maps(results, outdir):
    os.makedirs(outdir, exist_ok=True)
    wrote = []
    for r in results:
        if r.get("error") or not r.get("maps"):
            continue
        for l, jmag in r["maps"].items():
            if jmag is None or not np.any(jmag > 0):
                continue
            path = os.path.join(outdir, f"{_slug(r['net'])}-{_slug(l)}.svg")
            _svg_heatmap(path, jmag, r["grid"].dims[l], f"{r['net']} {l}")
            wrote.append(path)
    return wrote


def _slug(s):
    return re.sub(r"[^A-Za-z0-9._-]+", "_", s)


def _svg_heatmap(path, jmag, dims, title, px=900):
    nx, ny, mx, my = dims
    step = max(1, int(math.ceil(max(nx, ny) / px)))
    # max-pool
    h = (nx // step) * step
    w = (ny // step) * step
    a = jmag[:h, :w].reshape(h // step, step, w // step, step).max(axis=(1, 3))
    vmax = float(a.max()) if a.size else 0.0
    if vmax <= 0:
        return
    lv = np.log10(np.maximum(a, vmax * 1e-4))
    lo, hi = math.log10(vmax * 1e-4), math.log10(vmax)
    cell = 4
    scale = 2.0
    W, H = a.shape[1] * cell, a.shape[0] * cell
    parts = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W*scale:.0f}" '
             f'height="{H*scale:.0f}" viewBox="0 0 {W} {H}">',
             f'<rect width="{W}" height="{H}" fill="#101010"/>']
    for i in range(a.shape[0]):
        for j in range(a.shape[1]):
            t = (lv[i, j] - lo) / (hi - lo + 1e-12)
            t = min(max(t, 0.0), 1.0)
            r = int(255 * min(1.0, max(0.0, 1.5 * t - 0.5)))
            g = int(255 * min(1.0, max(0.0, 1.5 - abs(3.0 * t - 1.5))))
            b = int(255 * min(1.0, max(0.0, 1.0 - 1.5 * t)))
            parts.append(f'<rect x="{j*cell}" y="{i*cell}" width="{cell}" '
                         f'height="{cell}" fill="rgb({r},{g},{b})"/>')
    parts.append(f'<text x="4" y="14" fill="#fff" font-size="12" '
                 f'font-family="monospace">{title}  log-scale, max={vmax:.3g} A/m²</text>')
    parts.append("</svg>")
    with open(path, "w") as fh:
        fh.write("\n".join(parts))


# --------------------------------------------------------------------------
# main
# --------------------------------------------------------------------------
def parse_args(argv=None):
    here = os.path.dirname(os.path.abspath(__file__))
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--board", default="/home/tj/nitride-nano/PCB/nitride-nano.kicad_pcb")
    p.add_argument("--git-rev", default="d705578",
                   help="materialise this git revision instead of --board "
                        "(default: the fabricated 'Rev2 ordered' revision)")
    p.add_argument("--repo", default="/home/tj/nitride-nano")
    p.add_argument("--rev-path", default="PCB/nitride-nano.kicad_pcb")
    p.add_argument("--no-git-rev", action="store_true",
                   help="use --board as-is instead of a git revision")
    p.add_argument("--nets", default="", help="comma-separated subset of nets")
    p.add_argument("--scenario", default="", help="substring filter on scenario names")
    p.add_argument("--pitch", type=float, default=0.1, help="base grid pitch in mm")
    p.add_argument("--subcell", type=int, default=3)
    p.add_argument("--cov-min", type=float, default=0.05)
    p.add_argument("--thickness", type=float, default=THICKNESS_DEFAULT * 1e6,
                   help="copper thickness in µm")
    p.add_argument("--temp", type=float, default=20.0, help="copper temperature in C")
    p.add_argument("--bus-g", type=float, default=1.0e6)
    p.add_argument("--outdir", default=here)
    p.add_argument("--json", default="")
    p.add_argument("--maps", action="store_true")
    p.add_argument("--list-nets", action="store_true")
    p.add_argument("--selftest", action="store_true")
    p.add_argument("--converge", default="", help="net name for a pitch convergence study")
    return p.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    if args.no_git_rev:
        args.git_rev = ""
    board, board_path = load_board(args)
    thickness = args.thickness * 1e-6

    if args.list_nets:
        print(f"{'net':<28} " + " ".join(f"{l:>8}" for l in STACK) + "   total")
        for netname in NETS:
            ng = extract_net(board, netname)
            ar = [ng.layer_area(l) for l in STACK]
            print(f"{netname:<28} " + " ".join(f"{a:8.2f}" for a in ar)
                  + f"  {sum(ar):8.2f}")
        return 0

    if args.converge:
        netname = args.converge
        spec = NETS[netname]
        ps = spec.get("pitch_scale", 1.0)
        rows = []
        for mult in (2.0, 1.5, 1.0, 0.75, 0.5):
            pitch = args.pitch * mult * ps
            r = analyze_net(board, netname, spec, args, thickness, args.temp, pitch)
            if r.get("error"):
                print("  error:", r["error"]); continue
            c = r["cases"][0]
            rows.append((pitch, r["nodes"], c["r_eff"], c["method"]))
            print(f"  pitch={pitch:.4f} mm nodes={r['nodes']:7d} "
                  f"R_eff={c['r_eff']*1e3:.5f} mΩ ({c['method']})")
        if len(rows) >= 2:
            fine = rows[-1][2]
            for pitch, nodes, rr, m in rows:
                print(f"    pitch {pitch:.4f}: R={rr*1e3:.5f} mΩ  "
                      f"deviation vs finest = {100*(rr-fine)/fine:+.2f}%")
        return 0

    selected = [n.strip() for n in args.nets.split(",") if n.strip()] or list(NETS)
    scenarios = [s for s in SCENARIOS
                 if not args.scenario or args.scenario.lower() in s["name"].lower()]

    results = []
    for netname in selected:
        spec = NETS[netname]
        pitch = args.pitch * spec.get("pitch_scale", 1.0)
        t0 = time.time()
        r = analyze_net(board, netname, spec, args, thickness, args.temp, pitch)
        r["bar_model"] = (None if r.get("error") or len(r.get("cases", [])) > 1
                          else bar_model(r))
        results.append(r)
        if r.get("error"):
            print(f"[{netname}] ERROR {r['error']}")
        else:
            rr = ", ".join(f"{c['key']}={c['r_eff']*1e3:.4f} mΩ"
                           for c in r["cases"] if "error" not in c)
            print(f"[{netname}] nodes={r['nodes']} vias={r['via_count']} "
                  f"pitch={pitch:.3f} {rr}  ({time.time()-t0:.1f}s)")

    meta = dict(thickness=thickness, temp_c=args.temp,
                rs=(RHO20 * (1 + ALPHA_CU * (args.temp - 20)) / thickness),
                board_label=(f"{args.git_rev}:{args.rev_path}" if args.git_rev
                             else args.board))
    report = build_report(results, scenarios, args, board_path, meta)
    os.makedirs(args.outdir, exist_ok=True)
    rp = os.path.join(args.outdir, "report.md")
    with open(rp, "w") as fh:
        fh.write(report)
    print(f"[report] {rp}")

    if args.maps:
        maps = write_maps(results, os.path.join(args.outdir, "maps"))
        print(f"[maps] wrote {len(maps)} SVG maps")
    if args.json:
        payload = dict(meta=meta, board=board_path, scenarios=scenarios,
                       results=[{k: v for k, v in r.items() if k != "grid"}
                                for r in results])
        with open(args.json, "w") as fh:
            json.dump(payload, fh, indent=1, default=str)
        print(f"[json] {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
