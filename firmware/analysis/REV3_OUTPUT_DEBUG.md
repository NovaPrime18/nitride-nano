# Rev3 output stage — power clamp and 10× current-read error

Status: **two open hardware faults**, both in the output-current sense chain.
Firmware: HEAD (`firmware.bin`, ramp budget 2 s anchored on the CV slew,
slow 2 s disable). Board: rev3, hand-soldered, XT90 input, no USB-C (so the PD
bus — and both INA228s — are absent by design, see README "XT90 / DC input
parity").

This document compiles every test run on the output stage, what each one
proved, and what is left to do. It supersedes the scratch notes in the chat
transcripts; nothing here changes the firmware.

---

## TL;DR

1. **The converter itself works**: it ramps cleanly from park to the setpoint,
   regulates 30.04 V at light load, runs `RUN` with the load switch closed.
2. **Problem 1 — it cannot deliver power.** Above ~1 A it settles at ≈4.8 V
   (~6 W) and stays there. Input is only 10 W, so the loop is *clamping*, not
   dissipating. `FB` is railed low while `VC` sits at the **bottom** of its
   range, i.e. the **peak-current loop** is the limiter. The CC threshold is
   exonerated (CTRL measures correct), and the shunt signal at **R9** is
   perfect — so the fault is **between R9 and the chip**, and the node is
   **pressure-sensitive** (probing it changes the behaviour) ⇒ marginal joint
   at **U1 pins 2/3 (LSP/LSN)**.
3. **Problem 2 — the reported current is ~15–18× low.** Not a firmware bug:
   the firmware scales `ISMON` by the range *it selected*. In HighGain it
   assumes the ×10 INA241 path (20 mΩ), but **U14's output differential measures
   1.4 mV where 25 mV is required** — U14 is not amplifying. The chip sees
   ~1/18 of the real current and the firmware reports that faithfully.
   **LowGain uses the raw shunt, so the reading should be correct there** — that
   is the confirming test (§7.1).
4. **Safety consequence of Problem 2**: because the sense is ~18× low, the CC
   threshold (112 mV ⇒ 5.6 A nominal) corresponds to a real current far above
   the setpoint ⇒ **there is no working CC protection in HighGain**. Do not
   rely on the current limit until this is fixed; keep the bench supply
   current-limited.

---

## 0. Setup

| Item | Value |
|---|---|
| Input | XT90, bench PSU **20 V / 3 A** (verified not limiting) |
| Load | electronic load, stepped 0.1 A → **1.27 A** |
| Setpoint | Vset 30 V, Iset 5 A (defaults; the first-enable recipe of 5 V / 1 A was not applied) |
| CC range | `HighGain` (×10, R_eff = 20 mΩ), also tested `LowGain` (×1, R_eff = 2 mΩ) |
| Mode badge | `CC` — **artefact of a stuck BTN1**, not a chip state (§D) |

Enable sequence behaves exactly as designed:

```
5.626  stage=PRE          (PB1 low → chip enabled, LOADEN low, bridge parked)
5.702  stage=RMP          (LOADEN high)
6.862  output: CV code settled after 1160 ms - ramp budget starts now (2000 ms, vset=30000 mV, vout=20444 mV)
7.027  stage=SWO
7.307  output: load switch confirmed by voltage only - no output INA228 in this input mode (XT90/DC)
7.307  output: switch closed and verified: vout=29854 mV
7.307  stage=RUN          vout=30356 mV
```

---

## 1. The two sense chains (reference map)

Both current senses are **2 mΩ shunts with Kelvin taps through 27 Ω**, but they
feed different things and are **not** interchangeable. Keep the full net names
in mind — there are two different `SENSE_P`/`SENSE_N` pairs on two different
sheets.

```
(a) OUTPUT current  (what the CC loop uses)
      R96 2 mΩ  (PWR-In ↔ PWR_REG_OUT-, i.e. the output return)
        │
        ├─[R83 27 Ω]─ /Power-Sense-Out/SENSE_P ─┬─ U7 S1A (pad 16)
        │                                       └─ U14 IN+ (pad 8)
        └─[R98 27 Ω]─ /Power-Sense-Out/SENSE_N ─┬─ U7 S2A (pad 3)
                                                └─ U14 IN− (pad 1)

      U14 INA241A1 (gain 10 V/V, REF ≈ 87 mV from R102 27 Ω / R103 1 kΩ)
        └─ OUT (pad 5) ─ /Converter/IAMP-In ───── U7 S1B (pad 1)
           REF (pads 3,7) ─ /Converter/IAMP-Ref-In ─ U7 S2B (pad 4)

      U7 TMUX1574  (SEL low → A = ×1 raw shunt; SEL high → B = ×10 INA241)
        ├─ D1 (pad 2) ─ /Converter/ISP-L ─ U1 ISP   (pin 11)
        ├─ D2 (pad 5) ─ /Converter/ISN-L ─ U1 ISN   (pin 12)
        └─ D3 (pad 7) ─ U1 VC (pin 17)  ← the CC/voltage comp network, also switched
             A comp (pad 9): R16 18 k + C15 1 nF ∥ C26 100 pF
             B comp (pad 8): R101 1 k + C50 10 n ∥ C27 100 pF
      SEL = U7 pad 15 ← /Converter/High-Gain ← PB6
      ~EN = U7 pad 13 → GND (permanently enabled)   VDD = pad 14 = +3V3

(b) INDUCTOR current  (what the peak-current loop uses)
      R9 2 mΩ  (SW1 ↔ L1)  — the buck-side inductor shunt
        ├─[R14 47 Ω]─ /Converter/LSP ─ U1 LSP (pin 2) ─ C25 1 nF
        └─[R15 47 Ω]─ ...            ─ U1 LSN (pin 3) ─ C25 1 nF

(c) CC threshold
      CC DAC (PA6) ─[R48 10 k]─ U1 CTRL (pin 10) ─ R5 357 k → VREF(2 V), R4 75 k → GND
```

Firmware scale factors (`src/board.rs`):

| Range | `R_eff` assumed | `mv_per_a` = 10 mV/A × `R_eff` | `ISMON` model |
|---|---|---|---|---|
| HighGain (×10) | 20 mΩ | **200 mV/A** | `ISMON = 10·V(ISP−ISN) + ≈0.25 V` |
| LowGain (×1) | 2 mΩ | **20 mV/A** | same |

---

## 2. Measurement log

Every test run so far. "Verdict" is what it establishes, not what was hoped.

| # | Test | Measured | Expected | Verdict |
|---|---|---|---|---|
| 1 | VREF, U1 pin 9 | **2.0 V** | 2.0 V | ✔ chip powered, reference fine |
| 2 | SS, U1 pin 15 | **2.0 V** | ≈2 V after soft-start | ✔ soft-start completes |
| 3 | INTVCC, U1 pin 5 | **5 V**, always (U13 5 V buck via JP15) | 5 V | ✔ gate-drive supply fine |
| 4 | Bottom gates BG1/BG2 with LOADEN low | both rise to **5 V** | 5 V, TG1/TG2 off | ✔ gate drivers + bottom FET gates OK |
| 5 | Top gates TG1/TG2 vs GND | "moving" | switching | ✔ suggestive, not conclusive (floating nodes) |
| 6 | Reflow input GaNs Q1/Q2 | no change | — | ✘ eliminates input-FET joints as the cause |
| 7 | Reflow R14/R15 | no change | — | ✘ eliminates the LSP/LSN filter resistors |
| 8 | U7 `~EN` (pad 13) / VDD (pad 14) | **GND / +3V3** | enabled / powered | ✔ mux enabled |
| 9 | U7 SEL (pad 15) vs Iset crossing 5 A | **toggles low/high** | follows PB6 | ✔ mux control works |
| 10 | CTRL, U1 pin 10 | **1.377 V** | 1.350 V (firmware model) | ✔ CC threshold network correct |
| 11 | VC, U1 pin 17 | **0.354 V** idle → **0.645 V** loaded | railed high (~2–2.5 V) if the voltage loop were the limiter | ✘ some *current* loop holds VC down |
| 12 | FB (modelled from Vout) | **651 mV** vs 1000 mV ref | 1000 mV | ✘ voltage loop is demanding more |
| 13 | PSU input current at the sag | **0.5 A @ 20 V = 10 W** for ≈6 W out | ≈2 A for 38 W out | ✘ the loop is clamping, not a resistive loss |
| 14 | R9 drop at 1.27 A (Kelvin taps at R14/R15) | **2.56 mV ⇒ 2.02 mΩ** | 2.54 mV | ✔ inductor shunt + its taps + filters fine |
| 15 | U7 pad 1 − pad 4 (INA241 output differential) at 1.27 A | **1.4 mV** | **≈25 mV** (10 × 2.54 mV) | ✘ **U14 is not amplifying** (~1/18) |
| 16 | Probe U1 pins 2/3 (chip-side LSP/LSN) while running | regulation breaks: max **≈14 V**, `OUTPUT FAIL` | no electrical effect (10 MΩ vs 2 mΩ ∥ 1 nF) | ✘ **pressure-sensitive ⇒ marginal joint** |
| 17 | Reported Iout at 1.27 A load | **85–100 mA** | 1.27 A | ✘ ~15× low (see §4) |

Measurement-quality note: the Fluke 115 resolves 0.1 mV on its 600 mV range,
which is adequate for 2.5 mV (test 14 reproduced cleanly) but **not** for
distinguishing 1 A from 0.1 A through a 2 mΩ shunt when hand-held probes add
thermal EMF. Test 15 (25 mV vs 1.4 mV) is far above that floor and is
therefore trustworthy. Currents should be measured with the meter's **10 A
range in series**, not by mV across a shunt.

---

## 3. Problem 1 — the output collapses under load (≈6 W ceiling)

### 3.1 What happens

At 0.1 A the converter holds 30.04 V. Stepping the load to 1.27 A, the output
walks down over ~1.5 s and **settles at 4.76–4.80 V**, staying there (a stable
equilibrium, not a collapse to zero), then recovers to 30 V when the load is
removed.

### 3.2 The arithmetic

| Quantity | Value | Reading |
|---|---|---|
| Input power | 20 V × 0.5 A = **10 W** | the converter is not *trying* to deliver 38 W |
| Output power | 4.8 V × 1.27 A ≈ **6 W** | ~4 W of loss + aux — plausible, not a 32 W burn |
| FB | 651 mV vs 1000 mV ref | the voltage loop is railed, asking for more |
| VC | 0.645 V | bottom of the range — **a current loop is holding it** |
| CC threshold | (1.377 − 0.25)/10 = **112.7 mV** | ⇒ 5.6 A in HighGain / 56 A in LowGain ⇒ **cannot be limiting at 1.27 A** |

Order-of-magnitude check on the peak-current ceiling implied by VC: with the
board's control transfer (`V_CTRL ≈ 0.25 V + 10·V_sense`), `V_VC = 0.645 V`
corresponds to ≈ 40 mV of sense ⇒ **≈ 2 A of peak inductor current** on a
2 mΩ sense. At 4.8 V out of 20 V the converter is bucking, so `I_L ≈ I_out` —
i.e. the observed equilibrium sits right at that ceiling. (Treat as
order-of-magnitude; it illustrates *why* the operating point parks at ~1.27 A,
not a calibrated prediction.)

### 3.3 What is ruled out

- **CC loop** — CTRL is correct (test 10), so the threshold is 5.6 A / 56 A.
- **The output shunt and its taps** — test 14 gives 2.02 mΩ at the amplifier's
  own input.
- **The LSP/LSN filter resistors** — reflow changed nothing (test 7).
- **A resistive loss in the power path** — the input power is only 10 W
  (test 13); a hot joint would show up as input power, and as heat.
- **The input GaN FETs** — reflowed, no change (test 6).

### 3.4 Surviving hypothesis

The peak-current loop reads **R9** via **U1 pins 2/3**. The signal at the shunt
is correct (test 14) and its filters were reflowed (test 7), so the failure is
**between R9 and the chip's sense amplifier**:

> **Marginal/cold joint on U1 pins 2 and/or 3 (LSP/LSN).**

The strongest evidence is test 16: a 10 MΩ meter across a node that already has
1 nF (C25) across it and a 2 mΩ source **cannot** change the loop electrically —
so the observed change (4.8 V → 14 V ceiling, then `OUTPUT FAIL`) must be
**mechanical**: probe pressure making/breaking a connection. A partial
connection leaves the current-mode loop running on the 1 nF capacitor alone,
which is exactly the "clamps the power, holds VC low, ignores the CC setting"
signature.

Secondary candidates if continuity checks out:

- **U1 itself** (sense amplifier) — chip swap.
- **The switched VC compensation** (U7 pads 8/9, R16/C15/C26, R101/C50/C27).
  `VC` has *no* hard-wired compensation network on this board; it exists only
  through mux channel 3. A bad joint there leaves the error amplifier
  uncompensated — which can look like a low, load-dependent ceiling.

---

## 4. Problem 2 — the reported current is ~10× (really ~15–18×) low

### 4.1 Why the firmware is not at fault

The firmware never measures current directly; it converts `ISMON` with the
scale of the range **it selected**:

```
iout = (ISMON − zero) / mv_per_a          mv_per_a = 10 mV/A × R_eff
HighGain → R_eff = 20 mΩ (the ×10 path) → 200 mV/A
LowGain  → R_eff =  2 mΩ (raw shunt)    →  20 mV/A
```

The two sides agree **only if U14 really delivers ×10**. `ISMON` itself is
`10·V(ISP−ISN) + offset`, so:

- If the chip were seeing the **raw shunt** (mux on A) while the firmware
  assumed ×10, every reading would be **exactly 10× low**.
- If the chip is on the ×10 path but **U14 produces far less than ×10**, the
  reading is low by the amplifier's shortfall factor instead.

Both give the same *kind* of error, which is why the log alone could not
separate them. Test 15 does.

### 4.2 The measurement that decides it

| Quantity | Value |
|---|---|
| R96 drop at 1.27 A | 1.27 A × 2 mΩ = **2.54 mV** |
| INA241 gain (INA241A1) | **10 V/V** |
| ⇒ required output differential (U7 pad 1 − pad 4) | **≈ 25 mV** |
| **measured** | **1.4 mV** |

**⇒ U14 is not amplifying (~1/18 of the required gain).** The LT8390A therefore
sees 1.4 mV where it should see 25 mV, so `ISMON` sits near its zero-current
offset, and the firmware faithfully reports
`(10 × 1.4 mV) / 200 mV/A ≈ 70 mA` for a 1.27 A load — the 85–100 mA actually
logged, within the offset uncertainty.

The offset uncertainty is large: `ISENSE_ZERO_MV = 248` is a **rev2-era**
constant, and ±50 mV of error is ±0.25 A at 200 mV/A. That is why low-current
readings cannot be used to judge this fault — the amplifier's differential can.

### 4.3 Consequence

With the ×10 path producing ~1/18 of the intended signal, the CC threshold of
112.7 mV corresponds to a **real** current far above anything the supply can
deliver ⇒ **no functioning CC protection in HighGain.** Until U14 is fixed, do
not depend on the current limit; keep the bench supply current-limited.

Note the mux is *not* the problem: `~EN` is strapped low (test 8) and `SEL`
follows PB6 (test 9), so the switch itself and its control are correct.

---

## 5. Why these are two faults, not one

| | Problem 1 (power clamp) | Problem 2 (low reading) |
|---|---|---|
| Sense element | **R9** (inductor, SW1↔L1) | **R96** (output return) |
| Amplifier | none — direct into U1 | **U14 INA241 ×10** |
| Signal path | R14/R15 + C25 → U1 pins 2/3 | R83/R98 → mux U7 → U1 pins 11/12 |
| Affected by the CC range mux | **no** → identical in both ranges ✔ matches | **yes** → low in HighGain, should be correct in LowGain |
| Effect | caps deliverable power (~6 W) | misreports current everywhere; disables CC protection |
| Evidence | tests 11, 12, 13, 16 | tests 15, 17 |

They share a physical neighbourhood (hand-soldered UQFN-32 + SOT-23-8), which is
consistent with both being assembly problems rather than design problems — but
they must be fixed and verified separately.

---

## 6. Ruled out — do not re-test

- R9's value and joints (2.02 mΩ measured at the amplifier's input).
- R14/R15 (reflowed, no change).
- The CTRL network R48/R5/R4 and the DAC path (CTRL = 1.377 V vs 1.350 V model).
- The mux: `~EN` = GND, VDD = +3V3, SEL follows PB6.
- The input GaNs Q1/Q2 (reflowed, no change).
- The firmware's range model and scale factors (they match the hardware *if*
  the hardware works — see §4.1).
- VREF, SS, INTVCC, LOADEN/BG gate behaviour.

---

## 7. Next actions, in order

### 7.1 Confirmation of Problem 2 (5 minutes, no soldering)

Set **Iset > 5 A** (e.g. 10.7 A) so the range becomes **LowGain**, load 1.27 A,
and compare the displayed current with the Fluke on its **10 A range, in series
with the load**:

- display ≈ meter ⇒ **U14 confirmed** (the ×10 path is the only broken part).
- display still ~10× low ⇒ the SEL is not reaching the mux's decoder (or the
  mux is damaged); return to U7.

### 7.2 Continuity (board off)

| From | To | Expect |
|---|---|---|
| U1 pin 2 (LSP) | R14 (chip-side pad) | ~0 Ω |
| U1 pin 2 | C25 pad 1 | ~0 Ω |
| U1 pin 3 (LSN) | R15 (chip-side pad) | ~0 Ω |
| U1 pin 3 | C25 pad 2 | ~0 Ω |
| U1 pin 1 (TG1) | R10 | ~0 Ω |
| U1 pin 22 (TG2) | R13 | ~0 Ω |
| U1 pin 25/26 (BG2/BG1) | R12/R11 | ~0 Ω |
| U14 pad 1 / pad 8 | SENSE_N / SENSE_P | ~0 Ω |
| U14 pad 5 | U7 pad 1 | ~0 Ω |

### 7.3 Live DC levels (output regulating 30 V, **no load**)

Boost ⇒ SW1 sits at VIN, so both LSP and LSN should read **≈ VIN (20 V)** with
~0 mV between them. A pin reading something unrelated or wandering is an open
joint. (Same nodes as test 16 — probe only with no load connected.)

### 7.4 Rework

1. **U1** — flux + hot air, reflow the sense/drive pin rows: pins **1, 2, 3**
   and **22–28** (TG1, LSP, LSN, TG2, SW2, BST2, BG2, BG1, BST1, SW1).
   A UQFN-32 is the least hand-solderable part on the board and every
   measurement so far says the *circuit* is right and the *contacts* are
   suspect.
2. **U14** — reflow; then verify its VDD (pad 6 = +3V3) and that
   **pad 1 − pad 4 ≈ 25 mV at 1.27 A**.
3. Secondary, only if the above does not cure it: verify U14's pinout against
   the INA241 datasheet (pads 3 and 7 are on the `IAMP-Ref-In` divider while
   pads 2 and 4 are GND — worth a second look), and audit the switched VC
   compensation (U7 pads 8/9, R16/C15/C26/R101/C50/C27).

### 7.5 Re-test

1. Enable with **no load, Vset 5 V, Iset 1 A** (the recipe the preflight prints).
2. Verify `supply[meas]`: `vout` tracks the setpoint, `fb_impl` ≈ 1000 mV,
   `iout` ≈ 0, `vc`-implied behaviour sane.
3. Step the load 0.1 → 1.27 → 3 A and confirm the output holds; then raise
   Iset and confirm the CC limit engages where commanded (compare with the
   Fluke).

---

## 8. Once it works — recalibrations

| Item | How |
|---|---|
| `ISENSE_ZERO_MV` (currently 248, rev2-era) | converter running, **no load**, read the `raw` field of the `isense:` line, set the constant to that count |
| `CC_CURRENT_OFFSET_MA_1X` / `_10X` (both 0) | BENCH §7: measure the actual CC limit against the Fluke in both ranges |
| CC limit verification | at ~1 A in HighGain and ~10 A in LowGain; the display must track the Fluke |
| CV accuracy | `CV_CAL_VALID = false` — needs U12 (USB-C) for the servo; see `analysis/CV_CALIBRATION_PLAN.md` |
| `cc_active` heuristic | will start working once the sense is correct (it compares `iout` with `i_limit`) |

---

## Appendix A — netlist map used above

```
R9   2 mΩ   Net-(D1-A) [SW1]                   ↔ Net-(L1-Pad1)
R14  47 Ω   Net-(D1-A)                        ↔ Net-(U1-LSP)   → U1.2, C25.1
R15  47 Ω   Net-(L1-Pad1)                     ↔ Net-(U1-LSN)   → U1.3, C25.2
C25  1 nF   Net-(U1-LSP)                      ↔ Net-(U1-LSN)
R96  2 mΩ   /Power-Sense-Out/PWR-In           ↔ /Converter/PWR_REG_OUT-
R83  27 Ω   /Power-Sense-Out/PWR-In           ↔ /Power-Sense-Out/SENSE_P
R98  27 Ω   /Converter/PWR_REG_OUT-           ↔ /Power-Sense-Out/SENSE_N
U14  INA241A1xDDF  IN+ pad 8, IN− pad 1, OUT pad 5, REF pads 3/7, VDD pad 6
R102 27 Ω / R103 1 k   INA241 REF divider (≈87 mV)
U7   TMUX1574RSV  S1A pad16 = SENSE_P, S2A pad3 = SENSE_N,
                  S1B pad1 = IAMP-In, S2B pad4 = IAMP-Ref-In,
                  D1 pad2 = ISP-L → U1.11, D2 pad5 = ISN-L → U1.12,
                  D3 pad7 = U1.17 (VC), S3A pad9 / S3B pad8 = comp networks,
                  SEL pad15 = /Converter/High-Gain, ~EN pad13 = GND, VDD pad14 = +3V3
R48 10 k    CC DAC (PA6)  ↔ U1.10 (CTRL)
R5  357 k   VREF(2 V)     ↔ U1.10
R4   75 k   U1.10         ↔ GND
Q5   EPC2367  PWR_REG_OUT- ↔ GND   (output switch, return path)
```

Note the two distinct `SENSE_P`/`SENSE_N` pairs on the board: the **input**
shunt R60's are `/Power-Sense-In/...` (INA228 U9), the **output** shunt R96's
are `/Power-Sense-Out/...` (this whole discussion).

## Appendix B — firmware constants and models involved

| Constant / model | Value | Note |
|---|---|---|
| `board::ISENSE_ZERO_MV` | 248 mV | rev2-era; must be recalibrated |
| `mv_per_a`, HighGain | 200 mV/A | `10 mV/A × 20 mΩ` |
| `mv_per_a`, LowGain | 20 mV/A | `10 mV/A × 2 mΩ` |
| CC range crossover | `iset ≤ 5000 mA` → HighGain | `src/control/supply.rs` |
| CTRL model | `V_CTRL = 250 mV + 10·V_sense` | saturated at 1350 mV |
| CTRL from code (iset 5 A, HighGain) | code 1876 → **1.350 V** | measured 1.377 V ✔ |
| CV map | `VOUT = 72 400 mV − 28.79 mV·code` | analytic, `CV_CAL_VALID = false` |
| Ramp budget | 2 s from CV-slew settle | `OUTPUT_RAMP_TIMEOUT_MS` |
| Disable sequence | `PWR_G` low → 1 s → `LOADEN` low → 1 s → chip off | bench-observable on purpose |

## Appendix C — log excerpts anchoring the analysis

Working state (light load):

```
7.901  supply[meas]: vout=30356 iout=9 verr=356 vin=20036 iin=0 pin=0
       ina_out_ok=false ... dac_nom=1187 dac_impl=1177 fb_impl=1004
```

Voltage loop regulating ⇒ `fb_impl ≈ 1000 mV` (FB reference) and
`dac_impl ≈ dac_nom` — this is the healthy signature to look for after the fix.

Under load (1.27 A), the clamp:

```
21.215  supply: OUTPUT OUT OF REGULATION: vout=25310 mV < 85% of vset=30000 mV at iout=25 mA;
        fb_impl=934 mV (ref 1000, <1000 = chip cannot deliver); i_limit=5000 mA cc_est=5000 mA
        ctrl_mv=1350 dac_nom=1187 dac_impl=1319; vin=20039 mV iin=0 mA pin=0 mW vin/vset=66%
21.294  supply[meas]: vout=16893 iout=54 verr=-13107 ... fb_impl=818
22.308  supply[meas]: vout=4764 iout=87 verr=-25236 ... fb_impl=651     ← plateau
22.307  isense: raw=333 (268 mV) zero=307 (247 mV) span=20 mV raw_i=100 mA filt_i=87 mA
```

`fb_impl ≈ 650 mV` (railed low) with `vout` at 4.8 V is the "loop wants more,
power stage/current loop refuses" signature.

## Appendix D — open items elsewhere on the board

These are unrelated to the two problems above but are open:

1. **BTN1 (PB9) stuck low.** The pin sits at 0.8 V with its 1 kΩ series resistor
   lifted, so the firmware reads it as permanently pressed — it fires once at
   boot (flipping the mode badge to `CC`) and never again. Check R66 and the
   pad; the new `input: btn1=… btn2=… btn3=… enc_btn=…` log line reports raw
   levels before debouncing.
2. **Option bytes `nSWBOOT0=0`** — USB-DFU entry will not hold until
   `FLASH_OPTR = 0xFFEFF8AA` is programmed once over SWD (preflight warns).
3. **CV accuracy** — `CV_CAL_VALID = false`; the map fit and the ±1-LSB servo
   need U12, i.e. a USB-C session (`analysis/CV_CALIBRATION_PLAN.md`, BENCH §8).
4. **`ISENSE_ZERO_MV`** — still 248 mV from rev2; recalibrate once the output
   runs (§8).
5. **XT90-mode monitoring** is intentionally absent (no input/output INA228);
   the load switch is confirmed by voltage only and the CC/OC supervision
   degrades accordingly. See README "XT90 / DC input parity".
