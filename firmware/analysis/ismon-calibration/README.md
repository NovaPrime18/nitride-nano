# ISMON (output current) calibration

The OLED `Iout` row comes from the LT8390A's ISP/ISN current monitor on PA3
(`/Converter/ISMON`), not from the INA228 (which is input-side). The chain is:

```
VOUT ── R18 (2 mΩ) ── output ── load
        │  └─ Kelvin to ISP/ISN
        └─ LT8390A:  V_ISMON = 10 · V(ISP−ISN) + V_OFFSET
                     └─ R49 10 k to GND, direct to PA3 (ADC1_IN4)
```

`V_ISMON` full scale is 1.25 V at 100 mV across R18, i.e. **20 mV/A**, and the
offset is the part-specific 0.20–0.30 V in the datasheet. The firmware therefore
has two calibration constants in `src/board.rs`:

| constant | value | meaning |
| --- | --- | --- |
| `ISENSE_MV_PER_A` | 20 | `10 × R18(2 mΩ)`, datasheet gain |
| `ISENSE_ZERO_MV` | 244 (bench) | ISMON voltage at **0 A with the output enabled** |

## Why the old boot learn did not work

The previous firmware learned the zero at boot with the converter parked
(`AdcSense::calibrate_zero`, called before the PD/output bring-up). The LT8390A
powers its ISMON buffer down with the rest of the chip while `EN/UVLO` is low, so
that sample is not the operating offset. It either fell outside the 150–350 mV
plausibility window (rejected, leaving the 250 mV datasheet seed) or captured a
slightly-off value, and either way the board read low by a fixed offset.

Because the current signal is only 20 mV/A, a **5 mV** offset error is **0.25 A**
at *every* load — which is exactly what the bench saw (see below). The learn was
removed and replaced with the explicit, bench-calibrated constant.

## Calibrating a board

1. Power the board and **enable the output with no load connected**.
2. Read the once-per-second RTT diagnostic:

   ```
   isense: raw=NNN (XXX mV) zero=NNN (YYY mV) span=N mV vout=... iout=... ...
   ```

   The `XXX mV` field is the ISMON node voltage measured with the ADC. With no
   load it is the zero, so set `board::ISENSE_ZERO_MV` to that value and rebuild.
   (A DMM on the ISMON node or across R49 gives the same number.)
3. Re-check with a known load: the OLED should now agree within a count or two
   (1 count ≈ 40 mA). If it does not, the gain is off — verify `R18` is really
   2 mΩ and +3V3 (the ADC reference) is really 3.300 V, then adjust
   `ISENSE_MV_PER_A`.

## Bench log — 2026-09-25

With `ISENSE_OFFSET_MV = 250` (the old seed), readings were low by a consistent
offset and the displayed slope was ~0.97, i.e. the gain was right and only the
zero was wrong:

| actual | displayed | error | implied zero |
| ---: | ---: | ---: | ---: |
| 0.25 A | 0.020 A | −0.230 A | 245.2 mV |
| 0.50 A | 0.340 A | −0.160 A | 246.6 mV |
| 1.00 A | 0.520 A | −0.480 A | 240.2 mV |
| 1.50 A | 1.200 A | −0.300 A | 243.8 mV |
| 2.00 A | 1.750 A | −0.250 A | 244.8 mV |
| 2.50 A | 2.200 A | −0.300 A | 243.8 mV |

`fit_ismon.py` turns a table like this into the constant to set:

```sh
python3 analysis/ismon-calibration/fit_ismon.py
python3 analysis/ismon-calibration/fit_ismon.py 0.5,0.34 2.0,1.75 2.5,2.2
```

It reconstructs the raw count behind each displayed value, divides out the
datasheet gain, and reports the mean implied zero (244 mV here) plus the
least-squares gain, so a zero error can be told apart from a gain error.
