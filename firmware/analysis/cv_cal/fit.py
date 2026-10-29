#!/usr/bin/env python3
"""Fit the CV DAC map `VOUT = A - B*code` from an output-voltage sweep log.

Phase 1 of the CV-accuracy plan (analysis/CV_CALIBRATION_PLAN.md): the firmware's
`CFG -> OUTPUT V SWEEP` logs one line per point ending in `cv_code=NNN u12=NNNN`.
This script pairs the commanded DAC code with the output INA228's (U12) bus
voltage and least-squares fits the affine map, printing `board.rs`-ready
constants plus the residual statistics (the residual is mostly the DAC's INL —
what the metrology servo, not the fit, will absorb).

Usage:
    python3 fit.py <rtt-logfile>          # or pipe: cat run.log | python3 fit.py

Notes:
  * Fit against the *last* calibration state only: do not mix sweeps from
    before and after a `CV_CAL_*` change in one input.
  * The firmware already applies `INA228_OUT_VBUS_SCALE_NUM/DEN` to the logged
    `u12` value, so the fit inherits the DMM cross-check. If you have not
    measured k yet, the map below matches U12, not your DMM.
  * Run with no load (the servo is frozen during sweeps; the load-path drop is
    handled by the servo at load).
"""

import re
import sys

# `sweep point 7/32: vset=19839 ... cv_code=2411 u12=19810`
LINE = re.compile(r"cv_code=(\d+) u12=(\d+)")

# Sanity window for the fitted result (analytic values: A ~ 72400, B ~ 28790).
A_EXPECT = (66_000, 80_000)   # mV at code 0
B_EXPECT = (26_000, 32_000)   # µV per code


def parse(lines):
    pts = []
    for line in lines:
        m = LINE.search(line)
        if not m:
            continue
        code, u12 = int(m.group(1)), int(m.group(2))
        if 0 < u12 < 70_000:  # skip absent-INA points (u12=0) and garbage
            pts.append((code, u12))
    return pts


def fit(pts):
    """Ordinary least squares on (code, mV); returns (A_mv, B_uv_per_lsb, stats)."""
    n = len(pts)
    sx = sum(c for c, _ in pts)
    sy = sum(v for _, v in pts)
    sxx = sum(c * c for c, _ in pts)
    sxy = sum(c * v for c, v in pts)
    denom = n * sxx - sx * sx
    if denom == 0:
        sys.exit("error: all points share one code — the sweep never stepped.")
    slope = (n * sxy - sx * sy) / denom          # mV per code (negative)
    intercept = (sy - slope * sx) / n            # mV at code 0
    a_mv = intercept
    b_uv = -slope * 1000.0                        # µV per code, positive

    resid = [v - (intercept + slope * c) for c, v in pts]
    mean_abs = sum(abs(r) for r in resid) / n
    rms = (sum(r * r for r in resid) / n) ** 0.5
    ss_tot = sum((v - sy / n) ** 2 for _, v in pts)
    r2 = 1.0 - sum(r * r for r in resid) / ss_tot if ss_tot else 1.0
    span = (min(c for c, _ in pts), max(c for c, _ in pts))
    return a_mv, b_uv, mean_abs, rms, max(abs(r) for r in resid), r2, n, span


def main():
    if len(sys.argv) > 1:
        with open(sys.argv[1], "r", errors="replace") as f:
            lines = f.readlines()
    else:
        lines = sys.stdin.readlines()

    pts = parse(lines)
    if len(pts) < 4:
        sys.exit(
            f"error: only {len(pts)} usable `cv_code/u12` pairs found "
            "(need >= 4; run `CFG -> OUTPUT V SWEEP` with RTT captured)"
        )

    a_mv, b_uv, mean_abs, rms, max_abs, r2, n, span = fit(pts)

    print(f"points: {n} (cv_code {span[0]}..{span[1]})")
    print(f"R^2:            {r2:.6f}")
    print(f"residual (INL): mean |r| = {mean_abs:.1f} mV, rms = {rms:.1f} mV, "
          f"max |r| = {max_abs:.1f} mV")
    print()
    print(f"analytic reference: A ~ 72,400 mV, B ~ 28,790 uV/LSB")
    if not (A_EXPECT[0] <= a_mv <= A_EXPECT[1]):
        print(f"WARNING: A = {a_mv:.0f} mV is far outside the analytic window "
              f"{A_EXPECT} — check the sweep/fit input.")
    if not (B_EXPECT[0] <= b_uv <= B_EXPECT[1]):
        print(f"WARNING: B = {b_uv:.0f} uV/LSB is far outside the analytic window "
              f"{B_EXPECT} — check the sweep/fit input.")
    print()
    print("Paste into src/board.rs (and set CV_CAL_VALID = true):")
    print()
    print(f"pub const CV_CAL_VALID: bool = true;")
    print(f"pub const CV_CAL_A_MV: u32 = {round(a_mv):_};")
    print(f"pub const CV_CAL_B_UV_PER_LSB: u32 = {round(b_uv):_};")
    print()
    # Handy verification numbers: what the fitted map commands at round setpoints.
    print("Verification (what this map commands, before the servo):")
    for vset in (5_000, 12_000, 20_000, 30_000, 48_000):
        code = max(0, round((a_mv - vset) * 1000.0 / b_uv))
        vout = a_mv - b_uv * code / 1000.0
        print(f"  set {vset/1000:5.1f} V -> code {code:4d} -> {vout/1000:6.3f} V "
              f"(1 LSB = {b_uv/1000:.1f} mV)")
    print()
    print("Next: rebuild + flash, then verify with the DMM at 20.000 V set —")
    print("expect within the residual bound above; the servo then closes the rest.")


if __name__ == "__main__":
    main()