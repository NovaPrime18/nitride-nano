#!/usr/bin/env python3
"""Fit the nitride-nano LT8390A ISMON (output-current) calibration.

The firmware (src/board.rs, src/sense/adc_sense.rs) converts each raw PA3 sample
with:

    zero_counts = ISENSE_ZERO_MV * 4096 // VREF_MV          # integer
    iout_mA     = (raw - zero_counts) * VREF_MV * 1000 // (4096 * ISENSE_MV_PER_A)

The LT8390A drives  V_ISMON = 10 * V(ISP-ISN) + V_OFFSET  and R18 = 2 mOhm, so
the gain is the datasheet 20 mV/A. The offset is part specific (0.20-0.30 V), so
the per-board unknown is the zero. This script fits it from bench pairs of
(actual drawn, value on the OLED) in amps, using the *current* firmware
constants, and prints the value to put in `board::ISENSE_ZERO_MV`.

    python3 fit_ismon.py                      # the 2026-09-25 bench run
    python3 fit_ismon.py 0.5,0.34 2,1.75 ...  # your own pairs
"""

import sys

# (actual drawn, value on the OLED), amps, captured 2026-09-25
BENCH = [
    (0.25, 0.020),
    (0.50, 0.340),
    (1.00, 0.520),
    (1.50, 1.200),
    (2.00, 1.750),
    (2.50, 2.200),
]

VREF_MV = 3300          # board::ADC_VREF_MV
MV_PER_A = 20           # board::ISENSE_MV_PER_A  (= 10 * R18 2 mOhm)
ZERO_MV = 250           # constants currently flashed into the board
COUNT_MV = VREF_MV / 4096.0


def zero_counts(zero_mv):
    """Mirror the firmware's integer conversion."""
    return zero_mv * 4096 // VREF_MV


def displayed_a(raw, zero_mv):
    return (raw - zero_counts(zero_mv)) * COUNT_MV / MV_PER_A


def lsq(xs, ys):
    n = len(xs)
    mx, my = sum(xs) / n, sum(ys) / n
    sxx = sum((x - mx) ** 2 for x in xs)
    sxy = sum((x - mx) * (y - my) for x, y in zip(xs, ys))
    slope = sxy / sxx
    return slope, my - slope * mx


def main(pairs):
    zc = zero_counts(ZERO_MV)
    print(f"firmware: VREF={VREF_MV} mV, {MV_PER_A} mV/A, zero={ZERO_MV} mV "
          f"({zc} counts, {zc * COUNT_MV:.1f} mV)")
    print(f"1 ADC count = {COUNT_MV:.4f} mV = {COUNT_MV / MV_PER_A * 1000:.1f} mA\n")

    hdr = (f"{'actual A':>9} {'displayed A':>12} {'err A':>8} "
           f"{'raw*':>7} {'ideal raw':>10} {'implied zero mV':>16}")
    print(hdr)
    print("-" * len(hdr))

    xs, ys, implied, raws = [], [], [], []
    for actual, shown in pairs:
        raw = zc + shown * MV_PER_A / COUNT_MV
        ideal_raw = actual * MV_PER_A / COUNT_MV
        z_new = (raw - ideal_raw) * COUNT_MV
        implied.append(z_new)
        raws.append(raw)
        xs.append(actual)
        ys.append(shown)
        print(f"{actual:9.3f} {shown:12.3f} {shown - actual:8.3f} "
              f"{raw:7.2f} {zc + ideal_raw:10.2f} {z_new:16.1f}")

    mean_zero = sum(implied) / len(implied)
    slope, intercept = lsq(xs, ys)
    print(f"\nimplied zero: mean {mean_zero:.1f} mV, spread "
          f"{min(implied):.1f}-{max(implied):.1f} mV")
    print(f"least squares: displayed = {slope:.4f}*actual {intercept:+.4f} "
          f"(gain {slope * MV_PER_A:.2f} mV/A, zero {ZERO_MV - intercept * MV_PER_A:.1f} mV)")
    print(f"\n=> set board::ISENSE_ZERO_MV = {round(mean_zero)}")

    print(f"\npredicted OLED reading with ISENSE_ZERO_MV = {round(mean_zero)}:")
    for actual, raw in zip(xs, raws):
        print(f"  {actual:4.2f} A drawn -> {displayed_a(raw, mean_zero):.3f} A")
    print("\nKeep ISENSE_MV_PER_A = 20 unless the least-squares gain is far off;")
    print("a consistent offset (not a slope) is the usual LT8390A part spread.")


if __name__ == "__main__":
    pts = [tuple(float(v) for v in a.split(",")) for a in sys.argv[1:]] or BENCH
    main(pts)
