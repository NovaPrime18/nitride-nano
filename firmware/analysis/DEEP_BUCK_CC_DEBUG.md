# Deep-buck / CC regulation — debug notes (2026-09-27)

Bench case: **Vset = 13 V, Ilimit = 15 A, load = 2.6 A.**
Observed: OLED `Iout` ≈ 3.23 A (0.63 A high), output ≈ 1.5 V above
setpoint, and at higher load the output “goes into CC” and collapses to ~5 V
even though the programmed limit is 15 A and the input has headroom. The LT8390A
FB pin measured a clean 1.0 V.

This file records what the firmware was actually doing, what was fixed, and how
to read the new console output. Hardware-side notes are flagged as **verify** —
the PCB files are mid-revision, so treat the connectivity below as “what the
committed rev2 files say”.

---

## TL;DR

Four independent firmware effects stack up, and any one of them can look like
“the chip went into CC and the voltage collapsed”:

1. **The CC DAC mapping ignored the CTRL summing network** (R48/R5/R4) — every
   programmed limit came out ~14 % low (15 A → ~13.6 A). *Fixed.*
2. **`SupplyMode::Cc` parked the CV DAC at minimum output.** The LT8390A ORs its
   voltage and current error amps, so the voltage loop won and collapsed the
   output instead of current-limiting it. *Fixed.*
3. **The output current was silently clamped by the input-power budget**
   (`i_power_cap = input_power_cap_mw / vout`), which can be a few amps when the
   PD contract power is low or stale. *Now logged loudly when it binds.*
4. **The software over-current trip used the already-high-biased ISMON at 100 %
   of the limit**, so a load sitting at the limit could false-trip and park the
   output. *Now 115 % + a full-context trip log.*

Plus: the ISMON zero constant was ~4 mV low on this board (see the second-run
update at the end — the no-load measurement is 248 mV), and the
`dac_nom` vs `dac_impl`/`fb_impl` logs tell you whether a setpoint error is the
DAC pin, the feedback network, or the converter failing to regulate.

---

## 1. The CC DAC never drove CTRL directly

**Connectivity (committed rev2 PCB):**

```
PA6 / CC_Set ── R48 (10 k) ──┬── LT8390A CTRL (U1.10)
                             │
LT8390A VREF (2.000 V) ── R5 (357 k)
                             │
                            R4 (75 k)
                             │
                            GND
```

The DAC does **not** sit on CTRL. `control/dac_cc.rs` used to assume
`V_CTRL = 0.02·I + 0.25` (i.e. the DAC voltage *is* the CTRL voltage), which is
only true with no divider. The real network is

```
V_CTRL = k·V_DAC + V_ctrl0,   k = 0.861,  V_ctrl0 = 48 mV
```

so the old code produced `I_actual ≈ 0.861·I_set + 0.68 A`:

| requested | old code | old actual limit | new code | new actual limit |
| ---: | ---: | ---: | ---: | ---: |
| 2.6 A | 374 | 2.89 A | 366 | 2.60 A |
| 15 A | 682 | **13.6 A** | 723 | **15.0 A** |

With the DAC high-Z (MCU in reset) the divider gives `V_CTRL = 347 mV`, i.e. a
**4.9 A** fail-safe limit — that is what R5 = 357 k was changed from 124 k for
(commit `1daf014`), and it is exactly why a stuck/floating CC node looks like a
~2–5 A “mystery CC limit”.

`dac_cc.rs` now inverts the network, and `board.rs` carries the divider
constants (`CTRL_SERIES_OHM`, `CTRL_VREF_OHM`, `CTRL_GND_OHM`, `LT8390_VREF_MV`,
`ISENSE_SHUNT_MOHM`). **If rev3 changes the divider, update those constants or
every CC limit is wrong again.**

CTRL transfer used (datasheet: `V(ISP−ISN) = (V_CTRL − 0.25 V)·0.1` for
0.3 V ≤ V_CTRL ≤ 1.15 V, saturating at 100 mV; R18 = 2 mΩ):

```
I_LIMIT(mA) = (V_CTRL(mV) − 250) · 100 / R18_mΩ
```

## 2. CC mode collapsed the output by fighting the hardware

The LT8390A diodes-ORs the FB error amp (regulate FB to 1 V) and the ISP/ISN
current error amp (regulate to the CTRL threshold) into VC; the **lower** of the
two wins. `SupplyMode::Cc` used to park the CV DAC at code 4095 — minimum output
on the inverted map — while setting the CC limit. The voltage loop therefore
demanded a *lower* VC than the current loop, so the output collapsed to the
minimum instead of limiting current.

`SupplyMode::Cc` now programs the CV DAC from `v_set_mv` exactly like CV mode, so
the chip is free to transition CV↔CC itself. In practice **CV and CC modes are
now functionally identical** (both are “CV with a CC ceiling”); the LT8390A
decides the active loop. The OLED badge now shows the *detected* loop
(`cc_active`), not the user’s mode selection.

`Btn2` on the Main screen still selects the mode; if you were pressing it to try
to “get to CC”, that alone could reproduce the collapse on the old firmware.

## 3. Output current clamped by the input-power budget

`control/supply.rs` limits output current to

```
i_limit = min(i_set, input_power_cap_mw · 1000 / vout_measured)
```

`input_power_cap_mw` is refreshed from the active PD contract
(`pd/manager.rs`: `v_mv · i_ma / 1000`). A low or stale contract (e.g. 40 W)
therefore caps the output at `40 W / 13 V ≈ 3 A` **regardless of the 15 A on the
CC screen**, and the converter then current-limits and the voltage sags — exactly
“plenty of input power, but it collapses”. A `supply: OUTPUT CURRENT LIMITED BY
INPUT POWER CAP` warning is now emitted the moment this becomes the binding
limit, with the contract numbers.

If you hit that warning but the source really can deliver more, the problem is
the negotiated contract / auto-track rail, not the converter.

## 4. Software OC trip

`SupplyMode::Cv` latched `Fault::OverCurrent` the instant the *filtered* ISMON
reading exceeded `i_limit`. With the ISMON reading ~0.6 A high and `i_limit`
clamped by item 3, a normal 2.6 A load could trip and park the output. The trip
now needs `iout > i_limit · 115 %` (`board::IOUT_TRIP_MARGIN_PCT`) and logs the
full context (measured current, limit, Vout, Vset, CC code and its estimated
limit) so a real over-current is distinguishable from measurement bias.

## 5. ISMON: the high reading is a zero offset

Firmware chain: `ISENSE_ZERO_MV` → `zero_raw = ISENSE_ZERO_MV·4096/3300`,
`iout_mA = (raw − zero)·3300·1000/(4096·20)`, i.e. **1 count ≈ 0.806 mV ≈ 40 mA**.

The first report's 3.23 A reading at 2.6 A implies `raw ≈ 382` → node ≈
**307.9 mV**; subtracting the `2.6 A · 20 mV/A = 52 mV` signal gives an implied
zero of **256 mV**. The second run's *no-load* measurement (output on, 0 A) read
raw 307–308 = **247–248 mV**, so the fitted constant is now **248 mV**:

```
zero 244 mV → ~0.24 A at no load (first-run constant)
zero 248 mV → ~0.00 A at no load (fitted from the 2026-09-27 run)
zero 256 mV → the *loaded* inference, ~10 mV above the no-load point
```

The gap between the 248 mV no-load zero and the 256 mV loaded inference is the
**intermittency**: the LT8390A ISMON offset is only specified 0.20–0.30 V (a
±2.5 A spread), is part-specific, and moves with temperature/operating point, so
a single fixed constant cannot track it across all conditions.

Recalibrate with the output **enabled and no load** (the buffer is powered down
when EN/UVLO is low): read the new `isense: raw=… (XXX mV)` line and set
`ISENSE_ZERO_MV` to `XXX`.

The new `isense` line also prints `raw_i` (single sample) next to `filt_i` (the
median+EMA value). If `raw_i` jumps around while `filt_i` is steady, the error is
switching ripple on a single sample, not a scaling error.

## 6. The 1.5 V setpoint error

FB is at a clean 1.0 V, so with the nominal R19/R20/R36 the only free variable is
the **DAC pin voltage**: +1.5 V at the output needs the DAC pin ~42 mV *below*
`VREF·code/4095`. The new `supply[meas]` line prints both:

* `dac_nom` — `VREF·code/4095` (what the firmware assumes), and
* `dac_impl` — the DAC-pin voltage implied by the *measured* Vout, assuming
  FB = 1 V and the nominal divider.

For 13 V / code 2065 → `dac_nom ≈ 1664 mV`; a 14.5 V output implies
`dac_impl ≈ 1621 mV`. A persistent 40+ mV gap means the DAC pin/reference is not
what the map assumes (check +3V3 / VREF+ with a DMM, and the DAC channel is
configured `NormalExternalBuffered` in embassy, so it is not a loading problem).
If `dac_nom ≈ dac_impl` but Vout ≠ setpoint, the feedback resistor values on the
board differ from `board::CV_FB_*`.

## 7. Verify: converter-disable pin (PA11 vs PB1)

The firmware drives **PA11** for the converter disable (`hal/converter_enable.rs`,
`main.rs`). The committed rev2 PCB routes the Q13 gate / LT8390A `EN/UVLO`
disable to **PB1 (U2 pin 18) → R78 (10 Ω) → Q13 gate**, with R79 (1 k) pulling
that gate to +3V3. PA11 in that same file is `USB_D_N` → TPD4S480 (U3.14).

**Verify which pin your board’s Q13 gate actually follows.** If it is PB1, then
`en.set_enabled(false)` is a no-op and all software shutdown relies on parking
the DACs (CV → min, CC → code 0, which holds CTRL below the 300 mV latch-off).
That still stops the stage, but the hardware disable path is not being driven.

---

## New console output

One-time at boot:

```
boot: conv-disable pin PA11 (active_high=…), CV dac PA4, CC dac PA6
boot: cv map: code0 -> 72400 mV, code4095 -> … mV (VREF 3300 mV, R19 … R20 … R36 …)
boot: cc ctrl net R48=10000 R5=357000 R4=75000 VREF=2000 mV: 15A -> code 723 (ctrl 550 mV, est 15000 mA); code0 ctrl 48 mV (latchoff 300 mV)
boot: iout software trip margin 115%; supply snapshot every 1000 ms
```

Once per second from the supply tick (grep `supply[`):

```
supply[ctl]:  mode=… en=… fault=… vset=… iset=… cv_code=… map_vout=… cc_code=… ctrl_mv=… cc_est=…
supply[lim]:  i_cap=… p_cap=… i_power_cap=… i_limit=… power_clamped=… cc_active=…
supply[meas]: vout=… iout=… verr=… vin=… iin=… pin=… dac_nom=… dac_impl=…
```

Edge-triggered:

```
supply: state change mode=… en=… fault=… …
supply: OUTPUT CURRENT LIMITED BY INPUT POWER CAP: iset=… clamped to i_limit=… (p_cap=… / vout=…)
supply: hardware CC transition: vout=… below vset=… at iout=… (i_limit=… cc_code=… ctrl_mv=… cc_est=…)
Fault::OverCurrent SOFTWARE trip: iout=… > i_limit=… (115% margin) …
Fault::… (existing fault lines, unchanged)
```

Once per second from the ADC loop (extended):

```
isense: raw=… (… mV) zero=… (… mV) span=… mV raw_i=… mA filt_i=… mA vout=… mV vset=… mV iset=… mA vin=… mV iin=… mA
```

### How to read the collapse

| What you see | Meaning |
| --- | --- |
| `supply: OUTPUT CURRENT LIMITED BY INPUT POWER CAP` | firmware clamped `i_set` by the PD contract power — not the LT8390A |
| `supply[ctl]: … mode=CC` on old firmware | the CC-mode CV park collapsed the output (fixed) |
| `Fault::OverCurrent SOFTWARE trip` | ISMON bias + tight limit false-tripped it |
| `supply: hardware CC transition` | the chip really is current-limiting; check `cc_est` vs `i_set` |
| `vin` ≈ 4400 mV in `supply[meas]` | the documented PD power-path dropout (`analysis/PD_ROOT_CAUSE.md`), not the converter |

---

## Bench procedure

1. **Recalibrate ISMON**: output enabled, no load, set `ISENSE_ZERO_MV` from the
   `isense` line’s `XXX mV`. Re-check at a known load.
2. Note the four `boot:` lines; confirm the CC `15A -> code … ctrl … mV` matches
   the assembled CTRL divider.
3. Enable at 13 V / 15 A, no load. Compare `dac_nom` vs `dac_impl`; DMM +3V3 and
   the ISMON/CTRL nodes.
4. Step the load up. Watch for `power_clamped=1`, `cc_active=1`, the CC-warning
   and the OC trip; check `vin`/`iin` against the contract.
5. Press BTN2 (CC mode) and confirm the output now holds its voltage ceiling
   while the badge follows the detected loop.

---

## Update — second bench run (2026-09-27, firmware with the fixes above)

Log: `Vset=13 V`, `Iset=16 A`, PD negotiated **48 V / 5 A EPR** (EPR contract,
`p_cap = 240 W`). The output enabled cleanly and regulated at no load. This run
changes the conclusion: **the firmware is not the limiter this time.**

What the log shows:

* **CC path is correct and not reached.** `cc_code=752 ctrl_mv=570
  cc_est=16000` — the corrected divider inversion is programming exactly 16 A,
  and the collapse happened at `iout ≈ 9 A`, i.e. *below* the limit.
  `cc_active=false` throughout, so the OLED badge correctly showed CV.
* **Power cap is not binding.** `p_cap=240000`, `i_power_cap` 17–45 A,
  `power_clamped=false`. (The 17 k–45 k swings are just `240 W / vout`.)
* **CV at no load is accurate.** 13.25 V measured for a 13.00 V setpoint;
  `dac_nom=1664` vs `dac_impl≈1656` (8 mV). The old +1.5 V is not reproducing at
  no load.
* **The converter is saturating, not current-limiting.** During the collapse
  (`vout≈5.37 V, iout≈9 A, vin≈47.8 V, iin≈1.05 A`), the output power is
  ≈48 W and the input ≈50 W. Computing FB from the measured output and the
  commanded code gives **fb_impl ≈ 895 mV, far below the 1 V reference** — the
  LT8390A is asking for more and cannot deliver it. A current limit would show
  FB ≈ 1 V with the output dragged by the load; this does not.
* The input current ≈1.05 A is *expected*: in buck, `Iin ≈ Iout·D = 9 A × 0.11`.
  It is not a hidden 1 A input limit.
* **ISMON zero is measured.** With the output on and no load PA3 read raw
  307–308 = **247–248 mV**, so `ISENSE_ZERO_MV` is now **248** (was 244); the
  244 constant showed ~200–250 mA at true zero.
* Load operating points were ≈14.0 V/3.3 A and ≈5.4 V/9 A — both ≈47 W, which
  is the signature of a **constant-power** load, not the CC 2.6 A the load was
  set to. Confirm the electronic load's mode.

So the collapse is a *converter* limit at ~50 W / ~9 A in deep buck
(48 V → 5.4 V, D ≈ 0.11), and the new firmware now says so explicitly:

```
supply: OUTPUT OUT OF REGULATION: vout=… mV < 85% of vset=… mV at iout=… mA;
  fb_impl=… mV (ref 1000, <1000 = chip cannot deliver); i_limit=… cc_est=…;
  ctrl_mv=… dac_nom=… dac_impl=…; vin=… iin=… pin=…
```

### Next measurements (hardware, in order)

1. **Electronic load mode.** Set it to a *verified* CC value (or a power
   resistor). The 14 V/3.3 A ↔ 5.4 V/9 A bistability is the classic
   constant-power-load signature and may be the whole story at the load.
2. **Scope FB during the collapse.** Expect ≈0.9 V (saturated) per the log. If
   it is ≈1 V, the output is being dragged by the load and the converter is
   fine.
3. **DMM the output under load.** If the DMM reads ~13 V while the OLED says
   ~14 V, the +1 V is ADC/ground-referred telemetry error, not regulation.
4. **DMM/scope the CTRL pin** during the collapse. The firmware intends 570 mV;
   if it measures ≈430 mV the real output current limit is ≈9 A and the CTRL
   divider/DAC path is still off. `TP13` is on `CC_Set`; the CTRL side is
   R48 pad 2.
5. **Check the deep-buck current sense.** The inductor peak limit is 50 mV /
   R9(2 mΩ) = 25 A, so it is *not* the intended limiter, but at D ≈ 0.11 the
   on-time is only ~280 ns and the LSP/LSN leading-edge spike can false-trip the
   peak-current comparator. Scope `SW1`/`SW2` and LSP–LSN across C25 (with
   R14/R15 = 47 Ω, τ ≈ 47 ns) and try a larger filter if the spike dominates.
6. **Retest at a lower input rail** (e.g. 24 V). If the collapse point moves,
   it is a duty-cycle/deep-buck sensing effect rather than a hard current limit.

---

## Update 2 — third bench run (2026-09-27): 48 V vs 28 V, and the verdict

Two runs, same firmware, same 12 V setpoint, load verified in **CC**:

| input | result |
| --- | --- |
| **48 V EPR** (VIN/VOUT ≈ 4.0) | held to ~5 A / ~68 W, then sagged and "chittered"; CC limit 11 A never reached |
| **28 V** (VIN/VOUT ≈ 2.3) | reached the programmed **10 A CC limit** (`hardware CC transition … iout=9974 mA, cc_est=10000 mA`) at ~115 W |

### What this proves

* **The firmware CV/CC path is now correct and validated.** At 28 V the
  converter current-limited at 9974 mA against a 10 000 mA command — the
  corrected CTRL-network mapping in `dac_cc.rs` is right to well under 1 %.
  `fb_impl` sat at 1003–1005 mV (the chip regulating) right up to the limit.
* **The power stage, FETs, inductor, output shunt and the whole sense chain are
  healthy** — they deliver 115 W at 28 V.
* **The failure is purely the high-step-down corner.** It moves with input
  voltage, exactly as a duty-cycle / switching-frequency / current-sense
  limitation does, not with the setpoint or the limit.

### Why 48 V→12 V is the weak corner

* **R1 = 147 k ⇒ fOSC = 1.0 MHz** ([LT8390A datasheet Table 1](https://www.farnell.com/datasheets/2311639.pdf)).
* At 1 MHz the on-time at 48 V→12 V (D≈0.25) is ~250 ns; the LT8390A's
  TG1 minimum duty in buck region is 10 % (~100 ns), so the duty itself is
  legal, but the margin for current-sense leading-edge blanking / filter delay
  is small.
* L = 2.2 µH at 1 MHz gives ΔIL ≈ 4.1 A pk-pk at 48 V→12 V (vs ≈3.1 A at
  28 V→12 V), so the LSP/LSN sensed ramp is small (≈8 mV) relative to a
  high-VIN switching spike, and R14/R15/C25 = 47 Ω/47 Ω/1 nF is only τ≈47 ns.
* The datasheet's own product table rates the 2 MHz **LT8390A** at
  **"50 W+"**, where the 650 kHz **LT8390** is **"450 W+"** — i.e. this part is
  optimised for small size at moderate power, and the board is pushing it.

### Fixes, cheapest first

1. **Use the lowest clean PD rail, not 48 V.** For a 12 V output anything
   ≥16.2 V is a clean buck; ~20 V (`VIN/VOUT ≈ 1.67`) is the sweet spot and was
   available in both runs. `control::auto_track` already implements exactly this
   ("gentlest step-down" in Efficiency policy) — the 48 V runs happened because
   the controller was left on its boot-negotiated 48 V rail. **Switching the PD
   screen to Auto, or just selecting the 20 V preset, is a one-button test that
   should recover full current.** The new `supply: DEEP-BUCK OPERATING POINT`
   warning tells you when you are in the marginal corner.
2. **Hardware, only if 48 V→12 V is a requirement:** increase the LSP/LSN filter
   (R14/R15 → 100 Ω and/or C25 → 2.2 nF), increase L (2.2 → 3.3–4.7 µH), and/or
   drop fOSC to 0.6–0.8 MHz (RT 267 k / 191 k) for more on-time margin. Scope
   SW1/SW2 and LSP−LSN first to confirm the leading-edge spike.

### The residual output error

* **Fixed +0.4 V**: the no-load output was 12.4 V (DMM-verified) for a 12.000 V
  setpoint, `fb_impl` ≈ 1003 mV. That is a fixed open-loop calibration error in
  the DAC/reference/FB-divider chain (a few mV at the DAC pin ≈ 0.4 V at the
  output). It is trimmable in firmware with a CV gain/offset calibration.
* **Load-dependent ~+0.6 V at 48 V** (13.0 V at 5 A) vs **~0.05 V at 28 V**
  (12.45 V at 4 A). Since a fixed DAC and a fixed FB network cannot produce a
  load-dependent output, either the FB regulation point shifts in the marginal
  48 V corner (likely, given `fb_impl` rose 1003 → 1014) or the ADC reading
  rises under load. **DMM the output at the load terminals while loaded** to
  settle it: if the DMM says 12.4 V, it is telemetry; if it says 13.0 V, it is
  the 48 V operating corner again.

### Recommended next tests (in order)

1. **20 V rail, 12 V out, load to 10 A** — confirms the fix and that the CC
   limit holds. Highest information per minute.
2. **Scope SW1/SW2 + LSP−LSN at 48 V near the collapse** — pulse-skipping,
   minimum-on-time, subharmonic oscillation or leading-edge false-trips.
3. **DMM the output under load** — resolves the +1 V.
4. No need to re-test the CTRL pin: the 28 V CC transition at 9974 mA already
   validated it.
