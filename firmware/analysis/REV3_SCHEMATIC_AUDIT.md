# Rev3 output stage — design-file audit (2026-10-28)

Scope: `PCB/*.kicad_sch`, `PCB/nitride-nano.kicad_pcb`, `firmware/src/**`, read against
the LT8390A, INA241A1 and TMUX1574 datasheets and against
[`REV3_OUTPUT_DEBUG.md`](REV3_OUTPUT_DEBUG.md).

**Headline: the design files are internally consistent — no wiring error, no wrong
pinout, no schematic/PCB desync, and the firmware scaling algebra is correct. So
neither "the LT chip again", "the TMUX again", nor "the firmware scales it wrong" is
supported. Two of the three conclusions in `REV3_OUTPUT_DEBUG.md` rest on a single
measurement each, and *both of those measurements are contradicted by other numbers
in the same log.** This document separates what is proved from what is inferred, and
lists the defects that are actually in the files.

---

## 0. What was checked, and how

| Check | Method | Result |
|---|---|---|
| LT8390A pin functions | `lib_symbols` in `Converter.kicad_sch` vs UFD-28 datasheet §Pin Functions | **exact match, all 29 pins** |
| INA241A1 pin functions | U14 symbol vs INA241 DDF(SOT-23-8) datasheet Table 5-1 | **exact match** (incl. pin 4 = NC/tie-to-GND) |
| TMUX1574 pin functions | U7 symbol vs TMUX1574 RSV(UQFN-16) datasheet Table 5-1 | **exact match** (UQFN column, not TSSOP) |
| Schematic ↔ PCB | all 827 `(ref,pin)`↔net pairs diffed against all 827 `(ref,pad)`↔net pairs | **0 mismatches, 0 orphans** |
| Range mux function | SEL/EN truth table vs schematic S1A/S1B/D1, S2A/S2B/D2, S3A/S3B/D3 | **correct in all three channels** |
| Firmware scaling | CV divider, CC divider, ISMON gain, ADC pin map, DAC channels | **algebraically correct** (§4.1) |

The PCB is *not* stale relative to the schematic, so nothing here is a "rev change that
never reached the layout" bug. Everything the firmware assumes about connectivity is
what the board actually is.

---

## 1. Ruled out (do not spend bench time on these)

1. **Wrong pinout / wrong footprint / layout desync.** All three parts are wired per
   datasheet, and the board matches the schematic pad-for-pad. Reflowing cannot fix a
   design that is already right — which is exactly what you observed.
2. **The TMUX1574 range mux.** `~EN`=GND (enabled), VDD=+3V3, SEL=PB6, and the truth
   table is `SEL=1 → B` (the INA241). The schematic puts the INA241 on S1B/S2B and the
   raw shunt on S1A/S2A. Correct.
3. **The ×10 `VC` compensation being mis-scaled.** `REV3_CC_RANGE_FRONTEND.md` §3.1/§5
   warned that the ×10 front end needs the compensation switched or the CC crossover
   moves ×10. It *is* switched, and the values are right: R16 18 k/C15 1 nF (×1) vs
   R101 1 k/C50 10 nF (×10) with a 100 pF shunt leg in both. Working the impedances,
   the ×10 branch's network impedance is ~9× lower than the ×1 branch over the
   mid-band, which cancels the 10× sense-gain step: **the two CC loops have the same
   gain to within ~10 % up to ~10 kHz.** That item is done properly — don't "fix" it.
4. **The CC threshold network and the CTRL→sense model.** `dac_cc.rs` implements the
   datasheet's three-region transfer (linear 0.30–1.15 V, transition 1.15–1.35 V,
   saturated 100 mV above that) and inverts R48/R5/R4 exactly. It even saturates
   `cc_est` at 100 mV. Matches test 10 (1.377 V measured vs 1.350 V modelled).
5. **The firmware's ISMON scaling.** `iout = (raw − zero)·3300·1000/(4096·mv_per_a)`
   with `mv_per_a = 10·R_eff`, `R_eff` = 2 mΩ / 20 mΩ. That is the correct inverse of
   `ISMON = 10·(R_eff·I) + 0.25`. Verified end to end.
6. **The power stage.** SW1/SW2 nodes, the four GaN FETs, R9 in the SW1↔L1 inductor
   segment, bootstrap (C23/C24 + 5.1 V clamps; note the LT8390A already has an
   internal INTVCC→BST Schottky, so D5/D6/R24/R23 are redundant but harmless), and the
   two PMEG6020 freewheel diodes are all correctly connected and correctly oriented.

---

## 2. The two conclusions that the log contradicts

### 2.1 "U14 is not amplifying" — the ISMON data says otherwise

`ISMON = 10·V(ISP−ISN) + 0.25 V`, and the chip's monitor gain is **10, independent of
what the mux is doing**. So the ISMON *span* is what tells you what the chip sees.

| Mux state | V(ISP−ISN) at 1.27 A | ISMON span |
|---|---|---|
| ×10 path, INA241 working | 10 × 2.54 mV = **25.4 mV** | **254 mV** |
| ×10 path, INA241 at unity gain (the doc's hypothesis) | 1.4 mV | **14 mV** |
| ×1 path (raw shunt) | 2.54 mV | **25.4 mV** |

The logged line is `isense: raw=333 (268 mV) zero=307 (247 mV) … span=20 mV`.

**Observed span ≈ 21 mV is 83 % of the ×1 prediction and 8 % of what the ×10 path
requires.** It is much closer to "the chip is on the raw shunt" than to "the amplifier
is dead" — and the `iout` that follows (21 mV / 200 mV/A = 105 mA, logged 87–100 mA) is
the *definition* of the "exactly 10× low" case in §4.1 of the debug doc.

So the amplifier's health is **not** what the log proves. The only measurement that
separates the two hypotheses is test 15 — and it is the weakest number in the whole
file, for a mechanical reason:

* It is a differential measurement of **U7 pads 1 and 4**, which are **1.2 mm apart on a
  0.4 mm-pitch UQFN-16**, with pads 2 and 3 in between. Two hand-held probe tips on
  0.2 mm pads at 0.4 mm pitch will not land reliably, and a tip that slides onto a
  neighbouring pad changes the answer completely.
* If the probes went to **U14** pads 1 and 4 instead (a very easy mix-up — U14 pin 1 is
  `IN−` = SENSE_N ≈ 0 V and pin 4 is `NC`/GND), you get **≈0 mV**, i.e. "1.4 mV".
* The recommended probe point for this differential is **U14 pad 5 (OUT) and pad 3
  (REF)** — both on the same row, but still 0.65 mm pitch — or better, `TP`/the
  `ISP-L`/`ISN-L` nets, and at a **deliberately known 2 A**, not at a load that is
  itself collapsing.

**Action:** re-do test 15, or (better, 5 minutes, no soldering) run §7.1 — set
`Iset` > 5 A so the range is `LowGain`, load 1.27 A, compare the display against the
meter on its 10 A range. The ×1 path has no amplifier in it and is provably healthy
(R96 taps measured 2.02 mΩ at the amplifier's own input), so:

* display reads ≈1.27 A ⇒ the ×10 path is the only broken thing (amp, its supply, or
  the SEL decode) — and `ISENSE…`/range logic is exonerated;
* display still reads ~0.1 A ⇒ the mux is not actually selecting B even though SEL
  toggles; then suspect U7 itself, not the wiring.

### 2.2 "Pressure-sensitive ⇒ marginal joint at U1 pins 2/3" — dead, and the probe is the best evidence yet for HW-1

> **Superseded 2026-10-28 by bench feedback.** The probes are PCBite (light, spring
> pogo tips), not hand-held, and the effect is too large and too *beneficial* to be
> mechanical. See §2.4. The paragraph below is kept only for the record.

The doc's argument is sound *as an argument*: a 10 MΩ meter across a node that already
has 1 nF (C25) and a 2 mΩ source cannot change the loop electrically. But that leaves
**two** mechanical explanations, and the doc only considered one.

LSP is pin 2 and LSN is pin 3 — **adjacent pins on a 4 × 5 mm, 0.4 mm-pitch QFN-28.**
Touching them both with two probe tips is likely to **short them to each other** (and
to short the 1 nF C25 that is directly across them). Shorting LSP to LSN removes the
inner current-sense signal entirely: the peak-current loop then runs on nothing, which
is exactly "regulation breaks, ceiling ≈14 V, then `OUTPUT FAIL`".

That explanation is *created by* probing, whereas the doc's joint explanation would
have been cured by the reflow you already did. **The reflow is the experiment that
falsifies the joint hypothesis** — so test 16 should be retired, not re-run.

### 2.4 Bench update 2026-10-28 — the probe effect, and what is now ruled in/out

Three facts, all from the bench:

1. **`U14` REF = 86.1 mV** (expected 86.8 mV). The `R103`/`R102` divider, the C74 pad, and
   U14 pads 3 and 7 are all good.
2. The probe is a **PCBite** — a light spring pogo tip on a magnetic base, not a
   hand-held DMM probe.
3. **Probing `C25` is the *only* thing that breaks it.** With the probe removed the
   converter regulates **30 V unloaded, cleanly**; with the probe attached it cannot
   reach 30 V at all, so the output switch never closes. And
   **`Vsense_REG_OUT` = 1.181 V at 30 V out**, i.e. the 248 k/10 k divider is
   healthy (nominal 1.163 V, so 1.5 % — within the 1 % resistors and exactly what
   `CV_CAL_VALID = false` exists to trim).

**What that rules out, and it is a lot:** the converter core, the bootstrap
(BST1/TG1/C23/D5/R24), all four gate drives, the CV error amplifier and its divider, the
sequencer and the load-switch control are all **working**. Problem 1 is therefore
*strictly load-dependent* — it is not a "converter cannot boost" fault.

**What it rules in:** the `LSP/LSN` node has essentially **no noise margin**. An unloaded
20 V → 30 V boost needs only a few hundred mA of inductor current, i.e. **~0.2–1 mV of
sense voltage**, and at that level a probe's ~50 pF (or a scope's ground loop) is enough
to decide the comparator's answer. That is a no-margin signature, not a probe artefact —
and it means every perturbation will make the load ceiling move, which is consistent
with everything measured so far.

**The one-component explanation worth checking first:** if **`C25` is not actually
1 nF** (open, cracked, or the wrong value fitted), then the filter **does not exist**,
the `LSP/LSN` pins see the raw SW1 edge through 47 Ω plus only their own few pF of pin
capacitance, and a probe's capacitance becomes a significant fraction of the whole node.
That single fault explains *all* of: the probe sensitivity, the ~2 A peak ceiling, the
flawless DC reading at R14/R15, and why reflowing R14/R15 changed nothing.
**Measure C25 with the power off — 30 seconds.** Even if it is good, 1 nF is the wrong
value anyway (§3 HW-1): a 3.39 MHz corner on a 0.93 MHz converter.



### 2.3 A third arithmetic slip: `fb_impl`

The logged `dac_nom=1187` is a **DAC code**, not millivolts. Its voltage is
`3300 × 1187 / 4095 = 956.5 mV`. Recomputing the FB summing node at `vout = 4764 mV`:

```
FB = (Vout/357k + Vdac/10k) / (1/357k + 1/10k + 1/10k)
   = (13.345 + 95.65) µA / 0.202801 mS = 537 mV
```

The doc's 651 mV comes from using `1187 mV` for `Vdac`. The real figure is **537 mV**,
i.e. the voltage loop is railed even harder than stated (54 % of reference, not 65 %).
This does not change the qualitative conclusion — the loop wants more and does not get
it — but it does mean the "chip cannot deliver" signature is stronger than recorded.

---

## 2.5 What actually changed rev2 → rev3 (from `git`, not from memory)

Compared by generating both netlists (`d705578` "Rev2 ordered" vs `a6e047f` "Preliminary
finish of Rev3") and diffing the nodes around U1. **The VC compensation values did not
change much at all. Two other things did, and one of them is a real regression.**

### The "wildly different VC values" are a designator collision

```
REV2  Net-(U1-VC) = { U1.17, R2 = 18 kΩ, C3 = (VALUE LEFT BLANK IN THE SCHEMATIC) }
      R2: VC → C3 → GND                        ← the actual VC compensation
      R16 = 10 Ω : U1.ISP  ↔ shunt+            ← NOT compensation
      R17 = 10 Ω : U1.ISN  ↔ shunt−            ← NOT compensation
      C26 = 1 µF : U1.ISP  ↔ U1.ISN            ← NOT compensation

REV3  Net-(U1-VC) = { U1.17, U7.7 }
      R16 = 18 kΩ + C15 = 1 nF ∥ C26 = 100 pF  ← rev2's R2/C3, now with a value (1 nF)
      R101 = 1 kΩ + C50 = 10 nF ∥ C27 = 100 pF ← the ×10-scaled copy
```

**Rev2's `R16`/`R17`/`C26` were the ISP/ISN input filter. Rev3 reused those same
designators for the VC compensation.** So "rev2 R16 = 10 Ω vs rev3 R16 = 18 kΩ" is
comparing two completely different functions, not an 1800× value change. Rev3's A branch
(`18 kΩ + 1 nF`) is *the rev2 network with the blank capacitor finally given a value* —
`R2 = 18 kΩ` is the same number. **The VC values are not the regression.**

Two things worth knowing anyway: rev2's compensation capacitor `C3` **has no value in the
schematic** (the `Value` field is empty), so no one can say what rev2 was actually
compensated with — every rev2 board's loop depended on whatever was assembled there. And
rev3 has reduced VC's compensation to a *single* implementation with **nothing hard-wired
in parallel**: if mux channel 3 does not conduct, `VC` has no compensation network at all.
Rev2 could not fail that way. That fragility is worth a bodge test (§5 step 5).

### The real regression: the ISP/ISN filter moved to the wrong side of the switch *(corrected — see below)*

> **Correction 2026-10-28.** An earlier revision of this file said the ISP/ISN filter was
> "deleted". That is wrong. `R83`/`R98` (27 Ω) with `C71` (100 nF) *is* a differential
> filter across the shunt, and because the differential loop closes through both
> resistors the corner is `1/(2π·(27+27)·100n) = 29.5 kHz` — so rev3 is **3.7× faster**
> than rev2, not unfiltered. What is genuinely absent is only a capacitance on the
> *chip* side of the mux, and that turns out not to matter for the symptom (see §3
> HW-6). The regression is real but smaller than stated.

```
REV2  Net-(U1-ISP) = { U1.11, R16 = 10 Ω (→ shunt+), C26 = 1 µF (→ U1.ISN) }
      Net-(U1-ISN) = { U1.12, R17 = 10 Ω (→ shunt−), C26 = 1 µF }
      ⇒ 20 Ω with 1 µF directly across the chip's ISP/ISN pins
        τ = 20 µs   f_c = 8 kHz      ← "near-DC", exactly what the LT8390A expects

REV3  /Converter/ISP-L = { U1.11, U7.2 }      ← two-node nets. No R, no C.
      /Converter/ISN-L = { U1.12, U7.5 }
      ⇒ filtering exists only at the shunt: R83/R98 = 27 Ω + C71 = 200 nF
        differential, τ = 5.4 µs, f_c = 29.5 kHz  ← 3.7× faster than rev2, and on the
        SHUNT side of the mux, so in the ×10 path it filters the amplifier's INPUT
        while the chip's ISP/ISN see the amplifier's buffered 1.1 MHz output directly.
```

`REV3_CC_RANGE_FRONTEND.md` §6.5 asks for the filter on the **chip side** of the mux.
Rev3 has none there. **But do not "fix" it:** the CC threshold is 100 mV of
`V(ISP−ISN)`, which on a 2 mΩ shunt is **50 A** (5 A in ×10), so EA2 cannot be clamping
the converter at 1.5 A no matter how noisy its input is. Filtering this node cannot
change the power ceiling; it only cleans up behaviour *at* the limit. Moving the cap to
the chip side is also not free: on `ISP-L` it would load the INA241's output, whose
maximum capacitive load is 1 nF, so it would have to be switched or kept in `LowGain`.

### Second regression: the shunt moved high-side → low-side

```
REV2: R18 = 2 mΩ between VOUT and the output terminal (HIGH side, CM = Vout)
      U1.ISP/ISN therefore ran in the chip's HIGH-side mode: +23 µA, sinking

REV3: R96 = 2 mΩ in the load RETURN between the output connector and Q5
      U1.ISP/ISN run in LOW-side mode: −10 µA, sourcing
      and the low-side tap now sits on Q5's drain — a node the load switch modulates
```

That is a different bias-current direction, a different internal input mode, and a
completely different noise environment for the same pins. Any "rev2 was fine, rev3 is not"
comparison has to carry all three changes, not just the amplifier.

---

## 2.7 The power stage is bit-identical rev2 ↔ rev3 — so "what broke" is not there

Diffed every component in the switching stage between `d705578` (rev2) and `a6e047f`
(rev3):

| | rev2 | rev3 |
|---|---|---|
| Q1–Q4 | EPC2367 | EPC2367 |
| R10–R13 (gate) | 0 Ω | 0 Ω |
| D25–D28 (gate) | PMEG3005EL | PMEG3005EL |
| D1/D2 (BST clamp) | BZX58550-C5V1X 5.1 V | BZX58550-C5V1X 5.1 V |
| D3/D4 (freewheel) | PMEG6020ETP | PMEG6020ETP |
| C23/C24 (BST caps) | 100 nF | 100 nF |
| R23/R24 (BST feed) | 10 Ω | 10 Ω |
| D5/D6 (BST diodes) | BAT46WS | BAT46WS |
| L1 | 2.2 µH | 2.2 µH |
| R1 (f_OSC) | 147 k | 147 k |
| R9 (inductor shunt) | 2 mΩ | 2 mΩ |
| **R14/R15/C25 (LSP/LSN)** | **47 Ω / 47 Ω / 1 nF** | **47 Ω / 47 Ω / 1 nF** |
| C7 (VIN) | 100 µF | 100 µF |
| U13 (+5 V, feeds INTVCC via JP15) | LMR16006 | LMR16006 |

**Nothing in the switching stage changed.** So the LSP/LSN network cannot be "the thing
that broke in rev3", and changing C25 is a *robustness* change, not a regression fix. The
only rev3-specific blocks are: the CC front end (§2.5), VC behind the mux, the output
switch (rev2 `HSBB0115` in the output path → rev3 `EPC2367` in the return), the output
path (`FB5–FB8` valued **"TBD"**, plus `F2` 20 A), and U7's function (rev2 74LVC1G08 AND
gate → rev3 TMUX1574).

### The honest answer to "why did rev2 just work"

It was **never asked the same question.** Rev2 was characterised at **10–20 A**
(115 W at 28 V→12 V, 10 A at the CC limit). At 10 A the LSP/LSN signal is **20 mV**; at
1.5 A it is **3 mV**. The same fixed amount of switching-edge coupling is irrelevant at
20 mV and dominant at 3 mV. Rev3 is the first time this board has been asked to hold
1.5 A, and that is where the marginal filter finally bites.

### The quantitative version

At the 5.2 V / 1.55 A collapse, `VC` = 0.645 V, which on a linear `VC → V(LSP−LSN)`
transfer (50 mV full scale) implies the chip *believes* it is at roughly **10–13 mV** of
sense — i.e. it thinks the peak current is ~5–6 A when the real inductor peak is ~2.5 A.
**The chip is tripping on ≈10 mV that is not real current.** On a 2 mΩ shunt, 10 mV of
switching-edge junk *is* 5 A of phantom current, and a 20 V edge coupling into a
0.4 mm-pitch `LSP`/`LSN` pair through 47 Ω is easily 1:2000. The partial improvement from
1 nF → 10 nF (6.05 → 8.06 W) fits that amplitude too.

**Acceptance criterion for C25 = 0.1 µF:** if the phantom current is the limiter, the
deliverable current should rise roughly with the extra rejection (14 % → 94 %, i.e.
several-fold). If it stays near 1.5 A, the edge theory is wrong and the next suspect is
Q1's switching loss (see §2.8).

### 2.8 The thermal picture is normal — but the loss budget is not

Q1 hot, Q2/Q4 slightly warm, Q3 cold is **exactly what a healthy converter looks like in
deep buck**: Q1/Q2 chop (Q1 carries the switching loss), Q4 is continuously ON carrying
DC, Q3 is OFF. 26 mW in Q4's `Rds(on)` at 1.55 A will not warm a thermal camera. **This is
not evidence that the boost leg is dead**, and reflowing the GaN FETs is unlikely to help
(they are unchanged from a board that delivered 115 W).

What *is* anomalous is the loss budget: **10 W in, 8.06 W out = 1.94 W lost** (and earlier
4 W) with Q1 the only warm part. At 1.55 A in deep buck, Q1 should dissipate ≈0.3 W
(switching) + 7 mW (conduction). If it is visibly hot it is burning ≥1 W, which is a
**turn-on/turn-off loss problem** — i.e. a `TG1` gate-drive/timing defect, and that *can*
limit deliverable power. It is the natural second suspect if the filter change stalls.

### 2.9 Resolved 2026-10-28: R96 is good (2 mΩ), so the amplifier is the ×16 culprit

Bench: **R96 confirmed 2 mΩ**, and **shorting `C71` (so `V(ISP−ISN)` = 0, CC loop fully
defeated) leaves the power collapse unchanged** — while the OLED correctly reports zero
current. That is a clean, end-to-end proof that the ISMON chain tracks its input, and
that **the outer CC loop is not the limiter**. Combined with "any sight of current
draw" surviving 3 more reflows, the collapse is in the **inner peak-current loop or the
power stage** (§2.7/§2.8).

With R96 = 2 mΩ confirmed, the earlier conflict collapses in one direction:

```
U14 OUT: +1.235 mV/A  →  input differential 0.1235 mV/A
R96:     2 mΩ, real   →  the amplifier should see 2.0 mV/A
⇒ INA241 effective gain 0.617, not 10
```

**The INA241 must be replaced.** No current-sense amplifier has a gain below 1, so the
part is damaged or counterfeit; the wiring, the REF divider (86.1 mV measured) and the
supply are all proven good. The two "3 mV at 5 A" readings were almost certainly the
load's *setpoint*, not the actual current, with the real current ≈1.5 A — which gives
1.94 mΩ and matches R96 = 2 mΩ.


---

## 2.10 **REFUTED 2026-10-28** — the 5 V buck proximity theory

> **Tested on the bench and it is NOT the cause.** Back-feeding `+5V` from an external
> supply (so the LMR16006 stops switching) leaves the behaviour exactly the same. The
> geometry below is real and is still worth fixing for rev4 — `L3` did move from 23.9 mm
> to 2.9 mm from the sense pair — but it is **not** the fault being chased.
>
> **Also retracted: the "C25 = 0.1 µF" advice.** See §3 HW-1. The corner must stay
> *above* the switching frequency or the filter eats the inductor-current ramp that
> peak-current-mode control needs.

### The measurement (kept for rev4 layout guidance)

### The sense network did not change by one micron

Measured on both boards (`PCB/nitride-nano.kicad_pcb` vs `git show d705578:PCB/...`):

```
                                  REV2                    REV3
U1                                99.12 , 100.29     same
R9                                88.70 ,  80.98     same
R14 / R15 (LSP/LSN series)        85.91 , 82.40 / 81.15   same
C25                               84.26 ,  81.72     same
L1                                98.85 ,  83.10     same
Net-(U1-LSP)  24 segs, 43.82 mm, F/B/In1, 3 vias   IDENTICAL
Net-(U1-LSN)  26 segs, 42.54 mm, F/B/In1, 3 vias   IDENTICAL
segments per layer            F 16 / B 17 / In1 17   IDENTICAL
same-layer neighbours within 0.5 mm  D26-K, U1-VIN  IDENTICAL
```

So there is nothing to find "in the compensation network" — it was not touched. What
moved is the thing that *radiates into it*.

### The 5 V regulator moved into the near field of the current-sense pair

```
                                  REV2 (distance to LSP/LSN)   REV3
U13  LMR16006 5 V buck             25.48 mm                     7.37 mm
L3   22 µH buck inductor           23.94 mm                     2.87 mm   ←
C90                                27.63 mm                     6.36 mm
C91  10 µF output cap              32.16 mm                     3.01 mm
TP66 +5V test point                32.35 mm                     1.76 mm   ←
+5V routing      103.0 mm, B/F/In3, gap 4.31 mm   80.3 mm, B/F/In1, gap 1.17 mm
```

Two independent mechanisms, both new in rev3:

1. **`L3` (the 5 V buck's 22 µH inductor) is 2.87 mm from the sense pair** and 23.94 mm
   away in rev2. Its fringing field is the near-field source; the sense pair is a *long*
   loop (44 mm of trace, three layer transitions, three via pairs) so it has a large
   pickup area.
2. **The `+5V` rail was re-routed onto `In1.Cu`** — the same inner layer that carries 17
   of the LSP/LSN segments — at a **1.17 mm** gap. In rev2 that rail lived on `In3.Cu`
   and never came within **4.31 mm**. The rail carries the 5 V buck's ripple/edge
   current, i.e. exactly the di/dt that a current-sense loop turns into voltage.

### Why this explains every observation on the bench

| observation | explained |
|---|---|
| 3 reflows change nothing | it is layout, not solder |
| identical BOM and values | the *network* is identical — the *neighbourhood* is not |
| rev2 fine at 6 V/20 A **and** 12 V/1 A | rev2's regulator was 24–32 mm away, on a different inner layer |
| collapse at small current; worse at higher current | the injected noise is a fixed amplitude; the wanted signal is `I × 2 mΩ`, so the signal only clears it at high current |
| `C25` 1 nF → 10 nF gave +33 % | more differential filtering helps a little, but 10 Ω/10 nF is still 1.59 MHz |
| probing `C25` changes behaviour | the probe adds capacitance **in the one node that matters**, which is only true if that node is riding on injected charge |
| Q1 hot, Q3 cold, VOUT sags | deep-buck equilibrium of a peak-current loop that is false-tripping |

Quantitatively it is the right size: the pair must resolve **2.5 mV** at 1.25 A against a
comparator full scale of 50 mV, while sitting 2.9 mm from a 22 µH inductor and 1.2 mm
from a switching rail. A few mV of 0.7–2.1 MHz pickup into a 44 mm three-via loop is
ample, and a few mV at 2 mΩ *is* several amps of phantom current.

### The 2-minute test that proves it

**Back-feed the `+5V` rail from the bench supply so the LMR16006 stops switching:**

> Set a bench supply to **5.1 V** and apply it to `TP66` (+5V), current limit ~300 mA.
> A buck cannot sink, so the LMR16006 simply stops switching and goes quiet.
> Then re-run the load test.

* Power collapses **much later or not at all** ⇒ the 5 V buck's switching was injecting
  into the sense pair. Confirmed, and it is a layout fault.
* Behaviour **unchanged** ⇒ the coupling is not the culprit; fall back to replacing U1.

Do not exceed ~5.1 V: `+5V` feeds `INTVCC` (via `JP15`) and the bootstrap diodes, and
`INTVCC`'s absolute maximum is 6 V.

Then the durable fix, for rev4: move `L3` and the `+5 V` route away from the
`R9/R14/R15/C25` region, keep `+5V` off the inner layer the sense pair uses, never run a
switching rail parallel to a Kelvin-sense pair, and route LSP/LSN as a tight
side-by-side pair on one layer with as few vias as possible (44 mm and three via pairs is
a lot of loop area for a 2 mΩ signal).

---

## 3. Defects that are actually in the files

Ranked by how much they can hurt. Confidence is stated per item.

### HW-1 — The `LSP/LSN` filter does essentially nothing at the switching frequency. *(high confidence, real)*

```
R14 = R15 = 47 Ω     C25 = 1 nF     R1 = 147 kΩ ⇒ f_OSC ≈ 0.93 MHz
f_c = 1/(2π·47·1nF) = 3.39 MHz      ⇒ attenuation at f_OSC ≈ 3.5 %  (−0.3 dB)
```

47 Ω/1 nF is a **3.4 MHz** filter on a **0.93 MHz** converter — it is transparent to
the SW1 leading-edge spike that the peak-current comparator is trying to ignore, and
47 ns of time constant is far shorter than the ~250 ns on-time in deep buck. The
LT8390A reference design uses **10 Ω** here; 47 Ω is 4.7× that, which buys nothing at
the switching frequency and *costs* bias offset:

```
LSP/LSN pin bias current = 60 µA (datasheet, typ)
60 µA × 47 Ω = 2.82 mV  ⇒ 1.41 A of equivalent shunt current on R9 = 2 mΩ
60 µA × 10 Ω = 0.60 mV  ⇒ 0.30 A
```

Only the *mismatch* between the two pins' bias currents appears differentially, but it
is unspecified — so 47 Ω multiplies an unspecified error by 4.7× while the wanted
signal at the limit is only ~2.5 mV (1.27 A) to ~50 mV (full scale).

**This is the only mechanism found that produces a stable low peak-current ceiling with
a provably correct shunt waveform and survives a reflow — so it is the leading
candidate for Problem 1, and the PCBite probe result (§2.4) is direct experimental
support: adding capacitance to that node raises the ceiling, which only a filtering
mechanism can do.** It also explains why the ceiling does not move with input
voltage the way the 48 V→12 V deep-buck corner did on rev2 (that was a *duty-cycle*
limit; this is a *sense* limit).

Fix: **do not lower the corner below the switching frequency.** (Corrected 2026-10-28 —
the earlier "0.1 µF" advice in this section was wrong.)

`C25` is across the inductor-current shunt, so it carries the *signal*: at 0.93 MHz the
comparator needs to see the ~4 mV peak-to-peak ramp of `I_L` to know when the peak is
reached, and the slope-compensation network is tuned around that ramp. A filter corner
below `f_sw` attenuates the ramp itself and adds phase lag **inside** the current loop,
which is how you get subharmonic oscillation and a converter that cannot deliver
current. **`C25` must stay well above `f_sw`, so it can never be the fix for a
low-current ceiling.** `1 nF`/`47 Ω` (3.4 MHz) is the right *kind* of value; `10 nF`/`10 Ω`
(1.59 MHz) is already at the edge and should not be pushed further.

| | rev3 as built | **tried 2026-10-28** | 0.1 µF (rejected) |
|---|---|---|---|
| R14/R15 | 47 Ω | 10 Ω | 10 Ω |
| C25 | 1 nF | 10 nF | 0.1 µF |
| f_c | 3.39 MHz | 1.59 MHz | 159 kHz |
| f_c / f_sw (0.93 MHz) | 3.6× | **1.7×** | **0.17× — eats the ramp** |
| bias offset (60 µA × R) | 2.8 mV | 0.6 mV | 0.6 mV |
| measured ceiling | 6.05 W | 8.06 W | — |

The 10 Ω change is worth keeping for the bias-current reason; the extra 10 nF is neutral
to slightly harmful and can stay or go. **Do not go bigger.**

Also note this partly answers "why did rev2 work": rev2 was validated at **115 W / 10 A**,
where the sense signal is ~20–26 mV and simply swamps the edge. At 1.5 A the signal is
~3 mV and the edge wins. **It was always the same marginal filter — rev3 is just the
first time the board has been asked to hold a low current at a low voltage.**

**Caveat on `DEEP_BUCK_CC_DEBUG.md`:** it recommends raising R14/R15 to 100 Ω. That makes
the unspecified 60 µA bias-current mismatch *worse* and buys nothing at the switching
frequency unless C goes up by far more. Prefer 10 Ω + a big C.

### HW-2 — Problem 2: **two measurements still disagree — settle it with an ohmmeter.** *(open)*

> Superseded by §2.6. The amplifier is provably linear and correctly referenced, but
> whether the fault is the shunt (16× low) or the amplifier (gain 0.62) depends on which
> of the two current figures in the R96 measurement is real. **Measure R96, power off.**

The arithmetic below is kept because it is what produced the (still-live) 16× hypothesis.

**Bench data:** `U14` pad 5 (OUT) vs GND, referred to the 86.2 mV it sits at with no
current:

| output current | OUT | Δ(OUT) | Δ per amp |
|---|---|---|---|
| 0 A | 86.2 mV | — | — |
| 1.7 A | 88.3 mV | 2.1 mV | **1.24 mV/A** |
| 7.38 A | 95.3 mV | 9.1 mV | **1.23 mV/A** |

Two currents 4.3× apart give the same slope to within 1 %. That is a **healthy,
linear, correctly-biased amplifier** — it sits exactly at its reference with no
current, so `VOUT = 10·Vsense + (REF1+REF2)/2` is being obeyed, and nothing is railed,
dead, or open.

Invert it. The INA241A1's gain is fixed at 10 V/V, so the differential it is being fed is

```
Vsense = 1.235 mV/A ÷ 10 = 0.1235 mV/A
```

On the board's 2 mΩ shunt that should be **2.0 mV/A**. It is **16.2× low**, and the
effective shunt resistance *as seen by the amplifier's input pins* is

```
R_eff_measured = 0.1235 mΩ      (not 2 mΩ)
```

**This reproduces Problem 2 exactly.** `ISMON` span = 10 × 0.1235 mV/A = 12.35 mV/A;
the firmware divides by 200 mV/A (×10), so 1.27 A displays **78 mA**, and the bench
logged 87–100 mA. The debug doc's "~15–18× low" is 16.2× by another route.

Three consequences worth stating plainly:

* **The INA241A1IDDFR is the correct part** and does not need replacing. The design
  doc's BOM picked it (gain 10 V/V, ±10 µV), and the schematic's IN+/IN−/REF1/REF2/OUT/VS
  mapping matches the DDF datasheet pin-for-pin (§0). **Do not change it, and do not
  reflow U7 again.**
* **The debug doc's §7.1 confirming test could never work.** It reasons "LowGain uses
  the raw shunt, so the reading should be correct there". That assumes the shunt is
  2 mΩ. If the shunt itself reads 16× low, *both* ranges read 16× low — which is exactly
  what the bench now reports ("when set to 5.5 A … x1 … the behaviour is identical").
  **§7.1 is retired as a discriminator.**
* **Do not try to cross-check with U12 (INA228).** It is on the same taps, so it will
  agree with U14 and both will be wrong together. That is precisely why the debug doc's
  "all three agree on the same current" argument (§Appendix, Known hardware notes) felt
  reassuring and proved nothing.

#### The file-level defect behind it

```
design intent (REV3_CC_RANGE_FRONTEND.md §6.4):  WSLP27262L000FEA
    2 mΩ, ±1 %, 5 W, package 2726 — a FOUR-TERMINAL Kelvin shunt with dedicated
    voltage-sense tabs whose resistance is negligible by construction.

as laid out (nitride-nano.kicad_pcb, R96, at 125.3 / 89.4):
    Resistor_SMD:R_2512_6332Metric  — TWO pads, 1.225 mm × 3.35 mm, at ±2.9625 mm
```

Those are not interchangeable, and a 4-terminal part cannot even be fitted to a 2-pad
land. So whatever is on the board is a 2-terminal 2512 resistor whose "Kelvin taps" are
necessarily ordinary traces hung off the two current-carrying pads. And that is fatal at
2 mΩ, because a 2512 pad is *itself* a resistor of the same order:

```
R_sheet(1 oz = 35 µm copper) = 1.72e-8 / 35e-6 = 0.49 mΩ/square
pad squares                  = 3.35 mm / 1.225 mm = 2.73
R per pad                    = 0.49 × 2.73 = 1.34 mΩ      ← comparable to the 2 mΩ element
R both pads                  = 2.7 mΩ                      ← larger than the shunt
```

With a shared pad of that size, the fraction of the element's drop that reaches the
amplifier's pins is decided entirely by *where on the pad* the sense trace attaches and
*where* the power plane enters. That fraction is not 1 — it can be anything from roughly
1 down to roughly 0.06, and 0.062 is what this board is doing. **This is a systematic,
geometry-only error: it survives every reflow, it is identical in both CC ranges, it
does not move with current, and a DMM at the tap nodes confirms the nodes are where you
think they are.** It is also why the original "marginal joint" theory fit the
observations so well while being wrong.

#### Three measurements that pin it down (10 minutes, power off for the first)

1. **Resistance across R96's own two pads** (4-wire if you have it, otherwise 2-wire with
   the leads nulled and the probes on the *inner* edges). Expect **1.9–2.1 mΩ**.
   Reading ~0.1 mΩ ⇒ the wrong part is fitted (a 0.1 mΩ-class 2512, not 2 mΩ) — done,
   that is the whole fault.
2. **With a known DC current**: V across the **pad inner edges**, then V across
   **SENSE_P/SENSE_N** (at R83/R98 or C71). If the second is ≪ the first, the taps are
   picking up the wrong part of the pad — confirmed layout bug.
3. **No-power continuity from J3.1 (PWR-In) to GND** — must be open with Q5 off. If it
   reads low, the shunt is *bypassed* by a parallel path (a zone tie, a stray via, or a
   chassis/standoff connection on the `MountingHole_3.2mm_M3_Pad_Via` pads H1–H4), which
   would also explain current flowing while the output switch never closes.

**Fix:** a respin with the 4-terminal part and its proper land pattern (power tabs + a
separate narrow sense-trace pair from the inner edge of each sense tab, no plane contact
along the sense path). A rev3 bodge is possible but fragile: lift the sense traces off
the pads and re-attach them at the pad edge nearest the element with the narrowest
possible trace. **Until then the CC limit is ~16× off: 100 mV of threshold corresponds
to ~800 A, so there is no over-current protection whatsoever. Keep the bench supply
current-limited.**

### HW-2b — The range split still makes the default case the blind one

* **`Iset` defaults to 5 A, which selects `HighGain`** (`HIGH_GAIN_MAX_SET_MA = 5000`,
  `i_set ≤ 5000 → HighGain`). That is the ×10 path, i.e. the one with an extra part in
  the signal chain. It is also where a "10× low" reading and a dead amplifier are
  indistinguishable from ISMON alone (§2.1). Not a bug, but it means the default
  configuration is the least diagnosable one. See FW-1.


### HW-3 — `R3 = 100 kΩ` from VREF to `SS` defeats the soft-start fault-reset path. *(high confidence, low urgency)*

`Net-(U1-SS)` = C2 100 nF + R3 100 k to VREF(2 V) + U1.15. The LT8390A discharges SS at
**1.25 µA** in the output-short condition (datasheet: `SS Pull-Down Current, VFB = 0.1 V`),
and needs SS below its **0.2 V** reset threshold to retry. R3 can inject up to
`(2.0 − 0.2)/100 k = 18 µA` — **14× the discharge current** — so SS can never fall below
≈1.98 V and the retry/latch-reset cycle cannot complete. Every 100 k of R3 "leaks" the
fault state away.

R3 is not in the LT8390A reference circuit. If it is there to define the soft-start
ramp, `R3·C2 = 10 ms` is the intent; if it is there to stop the DAC-float 36.7 V state
(it is not — that is R5 on CTRL), remove it. If it stays, verify SS can actually reach
< 0.2 V with the chip pulled into a short, otherwise the part will latch off and never
come back without a power cycle.

### HW-4 — `R46` in parallel with `R44` contradicts the firmware's `vin` scale. *(medium confidence)*

```
schematic:  +VBUS_SENSED ─ R44 (100 k) ─┬─ R47 (100 k) ─ sense ─ R97 (10 k) ─ GND
                        └ R46 (100 k) ─┘        (R44 ∥ R46 = 50 k)
            ⇒ 150 k / 10 k = 16 : 1     ⇒ 3.3 V full scale = 52.8 V

firmware:   board::VBUS_SENSE_NUM = 69_600 mV  ⇒ 200 k / 10 k = 21 : 1
```

If R46 is fitted, `vin` reads **31 % high** (a 20 V input shows 26.3 V). The bench log
shows `vin=20036` for a ~20 V supply, so R46 must be DNP on this build — but the
schematic marks it `100k` with no DNP flag, so a future build (or a re-supply of the
BOM) silently breaks the input-voltage telemetry, the UVLO/derating logic and the
`vin/vset` deep-buck warning. Mark R46 DNP, or delete it, or document it.

### HW-5 — `SWITCH_EN_READ` (PB14) is a dead stub. *(confirmed, documented)*

ERC error: *"Sheet pin SW_EN_READ has no matching hierarchical label inside the sheet"*,
and the netlist shows `/MCU/SWITCH_EN_READ` = {U2.27} only. `BENCH.md` already records
this ("U7 … so the net goes nowhere. Nothing in the firmware references it"). Flagging
it only because it means **the load-switch state is unobservable** — the sequencer
proves the switch closed by watching `vout`, which is a weaker proof and is exactly
what `XT90` mode has to fall back on.

### HW-6 — No RC filter on the chip side of the range mux. *(medium confidence)*

`/Converter/ISP-L` = {U1.11, U7.2} and `/Converter/ISN-L` = {U1.12, U7.5} — **bare two-point
nets**. The only filtering is `C71 = 100 nF` on the *shunt* side (τ = 27 Ω × 100 nF =
2.7 µs), i.e. **on the wrong side of the switch**, exactly what
`REV3_CC_RANGE_FRONTEND.md` §5.1/§6.5 says not to do ("keep the ISP/ISN RC filter on
the chip side"). In `×1` the path is rescued because SENSE_P/SENSE_N are already
filtered by C71; in `×10` the chip sees the INA241's buffered output through the mux's
2 Ω with nothing to absorb the mux's charge injection. Cheap fix if you ever respin:
10 Ω + 1 nF at U1.11/U1.12.

### HW-7 — The INA241 `REF` sits 37 mV above the amplifier's linearity floor. *(medium confidence)*

`REF = 3.3 V × 27/(27 + 1000) = 86.8 mV`. The design note calls for ~200 mV
(`REV3_CC_RANGE_FRONTEND.md` §6.5), and the datasheet's gain-error specification only
applies for `GND + 50 mV < VOUT`. At 86.8 mV the zero-current output has **only 37 mV
of margin**; any *negative* input offset (which is normal in a low-side return shunt)
drives the output into the 8–20 mV non-linear floor and the reported current goes to
zero rather than negative. It also makes the `ISMON` zero range-dependent (the mux
steps the common mode by 86.8 mV between ranges) — which `board.rs` admits but does not
account for.

If you touch the board: `R102 = 62 Ω`, `R103 = 1 kΩ` ⇒ REF = 193 mV, which is what the
design note asked for. Not worth a bodge on its own.

---

## 4. Firmware

### 4.1 What is correct (verified, do not "fix")

* `CcRange::shunt_mohm()` / `mv_per_a()`: 2 mΩ→20 mV/A, 20 mΩ→200 mV/A. Correct.
* `CcDac`: the R48/R5/R4 conductance inversion and the three-region CTRL transfer are
  both right, and `current_ma_for_code` correctly saturates the sense threshold at
  100 mV instead of extrapolating.
* `AdcSense::sample`: `PA0`=Vsense_REG_OUT, `PA3`=ISMON, `PA7`=Bus voltage, `PA1`=TEMP_SENSE_CONV,
  `PA9`=Temp PWR In — all five match U2 pins 8/11/15/9/31 in the netlist.
* `HighGain::new(Output::new(PB6, Level::Low), CcRange::LowGain)` — the pin and the
  software view start in the same state, so the first `set_range(HighGain)` really does
  drive PB6 high. (`HighGain::parked()` is also self-consistent: it forces the software
  view to `LowGain`, matching where the floating SEL pull-down puts the mux.)
* Range changes are gated on `Stage::Off`. Correct and necessary.

### FW-1 — The default setpoint sits exactly on the ×10 range's CTRL saturation knee

`HIGH_GAIN_MAX_SET_MA = 5_000`, and in `HighGain` 5 A × 20 mΩ = **100 mV**, which needs
`V_CTRL = 1.350 V` — the datasheet's saturation point. So at the *default* 5 A setting
the current loop runs with **zero CTRL headroom**: any DAC/divider error (you measured
+27 mV, 2 %) pushes the command past saturation, and `cc_est` can no longer distinguish
"exactly at the top of the window" from "way above it".

Move the crossover to **4 A** (4 A × 20 mΩ = 80 mV ⇒ `V_CTRL = 1.05 V`, still inside the
linear region, and 2.5–4 A remains a legal overlap with ×1's 2.5 A floor). One constant:

```rust
pub const HIGH_GAIN_MAX_SET_MA: u32 = 4_000;
```

### FW-2 — `ISENSE_ZERO_MV = 248` is a rev2 constant (known, still open)

±50 mV of offset is ±250 mA at 200 mV/A, i.e. a large fraction of the ×10 window. Fitting
it *per range* (the `isense:` line prints `zero` and `span`) is the only way the top of
the ×10 window is usable.

### FW-3 — A failed OLED silently downgrades the whole power path to ×1

On an XT90-only board the rev3 signature is the SSD1306 ACK. If the OLED is absent
during one boot, `rev3 = false` and **PB1/PB2/PB3/PB6 are never driven**: the converter
enable, the load switch and the range select all go to `parked()`. The firmware logs it,
but the *consequence* is not obvious from the symptom (the output refuses to enable).
`BENCH.md` flags the INA-less case; make sure the OLED-less case is equally visible,
because it also silently pins the CC sense range to ×1 while the UI still shows the
requested one.

### FW-4 — While the ×10 path is unproven, the over-current backstop is inert

`iout` is scaled by the *assumed* range. With the ×10 path producing ~1/10 of the true
signal, the 115 % software trip sits at ~57 A real and the hardware CC at 100 mV sits at
~50 A real (raw-shunt sense). **Nothing protects the output.** Keep the bench supply
current-limited and consider forcing `LowGain` (one constant, or a bench flag) until
§7.1 passes, so at least the display and the software trip are honest.

---

## 5. Do this, in this order

| # | Test | Time | Decides |
|---|---|---|---|
| 1 | ~~U14 VS / REF~~ **done 2026-10-28: REF = 86.1 mV ✔** — divider, C74 pad and pads 3/7 are good. (Note: REF is a *passive* divider into the REF pins, so this proves the pads and the divider, **not** that U14's die is powered.) | done | U14's reference network |
| 2 | **U14 pad 5 (OUT) vs GND**, single-ended, 10 MΩ. Output enabled, **no load** ⇒ expect **≈86 mV** (OUT = REF + 0). Then 1.27 A ⇒ expect **≈112 mV** (86 + 25). This is the one measurement that proves the amplifier is alive, and it is a buffered op-amp output so a probe on it is genuinely harmless. | 5 min | the amplifier, definitively |
| 3 | **§7.1 range cross-check**: `Iset` > 5 A (LowGain), 1.27 A load, compare display with the meter's 10 A range. Zero probing. | 5 min | ×1 path vs ×10 path |
| 4 | **Watch the bench PSU's input current** while stepping the load 0.2 → 1.27 A. Pinned at ~0.5 A while `vout` moves ⇒ the converter is current-limited (Problem 1 is a limit, not a regulation fault). Rising with the load ⇒ it is not. | 2 min | classification of Problem 1 |
| 5 | **Change the filter and watch the ceiling.** `R14 = R15 = 10 Ω`, `C25 = 22 nF`; re-run the 1.27 A test. **This is the only way to test HW-1 without probing LSP/LSN.** Ceiling rises ⇒ HW-1 confirmed. Unchanged ⇒ the limiter is elsewhere (and then the next thing to look at is `VC`/EA2, §3 HW-2). | 15 min | Problem 1's mechanism |
| 6 | Reflow U14 (never done — only U1 and U7 were). Re-measure steps 2/3 first. | 15 min | assembly vs part |
| 7 | Only then: `SS` pin behaviour (HW-3); respin items HW-1/HW-4/HW-6/HW-7. | — | board revision |

### The cheap hardware fix worth trying first

Because the probe experiment already points at filtering, this is no longer conditional:

**`R14 = R15 = 10 Ω` (datasheet value) and `C25 = 22 nF`** ⇒ f_c = 724 kHz (below the
0.93 MHz switching frequency) and the LSP/LSN bias-current error drops 4.7×
(2.82 mV → 0.60 mV, i.e. 1.41 A → 0.30 A of equivalent shunt current).

If 22 nF is awkward on the 0402 pads, `C25 = 10 nF` with `R14 = R15 = 22 Ω` gives the
same corner with only 1.3 mV of bias offset.

Second, free lever: **`R1` sets f_OSC.** `R1 = 147 kΩ` ⇒ ≈0.93 MHz; `R1 = 267 kΩ` ⇒
≈0.51 MHz. Halving the frequency doubles the on-time and halves the per-edge coupling,
which buys margin on the same problem without touching the sense network — at the cost
of ~2× the inductor ripple. Worth a try if the filter change is not enough.


---

## Appendix — pin map actually in the files (for the next person)

```
U1 LT8390A UFD-28           U14 INA241A1 DDF (SOT-23-8)     U7 TMUX1574 RSV (UQFN-16)
1  TG1   → R10 0Ω → Q1.G    1  IN−  → SENSE_N               1  S1B ← IAMP-In (U14.5)
2  LSP   → R14 47Ω ← SW1    2  GND                         2  D1  → ISP-L (U1.11)
3  LSN   → R15 47Ω ← L1.1   3  REF2 → IAMP-Ref-In          3  S2A ← SENSE_N
4  VIN                      4  NC   → GND                  4  S2B ← IAMP-Ref-In
5  INTVCC  (JP15 → +5V)     5  OUT  → IAMP-In              5  D2  → ISN-L (U1.12)
6  EN/UVLO ← R7 357k / R8 64.9k / Q13                       6  GND
7  TEST                     6  VS   → +3V3                  7  D3  → U1.17 VC
8  LOADEN  ← PB3            7  REF1 → IAMP-Ref-In          8  S3B → R101 1k + C50 10n, C27 100p
9  VREF 2.000 V             8  IN+  → SENSE_P               9  S3A → R16 18k + C15 1n, C26 100p
10 CTRL ← R48 10k / R5 357k / R4 75k                       10  D4  n/c
11 ISP   ← U7.2              REF divider: R103 1k → +3V3   11  S4B n/c
12 ISN   ← U7.5                         R102 27Ω → GND     12  S4A n/c
13 ISMON → R49 10k, PA3                 C74 100n → GND     13  ~EN → GND  (enabled)
14 PGOOD → R6 100k pull-up     ⇒ V_REF = 86.8 mV           14  VDD → +3V3
15 SS    → C2 100n, R3 100k→VREF                           15  SEL ← PB6 (High-Gain)
16 FB    → R19 357k / R20 10k / R36 10k ← CV_Set (PA4)      16  S1A ← SENSE_P
17 VC    → U7.7  (no local comp network — see HW-2/HW-6)
18 RT    → R1 147k → GND  (f ≈ 0.93 MHz)
19 SYNC/SPRD → JP1
20 LOADTG n/c
21 VOUT → Q4.D, R19, R85, D3.K, FB5–FB8
22 TG2   → R13 0Ω → Q4.G
23 SW2   → Q3.D, Q4.S, L1.2, R15
24 BST2  → C24 100n to SW2
25 BG2   → R12 0Ω → Q3.G
26 BG1   → R11 0Ω → Q2.G
27 BST1  → C23 100n to SW1
28 SW1   → Q1.S, Q2.D, R9.1, R14
29 GND (EP)

Power path:  J6(XT90-F) → F1 → R60 2mΩ → PWR_UNREG_IN → FB1-4 → U1.VIN
              VOUT → FB5-8 → PWR_REG_OUT+ → F2 → J3.2 (+ out)
              J3.1 (return) → R96 2mΩ → PWR_REG_OUT- → Q5 → GND
```
