# Rev3: switchable ×10 CC sense front end — feasibility

Question: put a ×10 amplifier on the output-current shunt, selectable in
firmware, to get resolution at low current while keeping the 20 A top end. Does
the LT8390A's need for direct shunt access make that dumb?

**Verdict: not dumb, and it targets the right node — but the thing that bites you
is not propagation delay. It is that the CC loop gain scales with sensed
volts-per-amp, so a ×10 front end raises the CC loop gain (and crossover) ×10
unless you switch the `VC` compensation with it.**

---

## 1. The chip has two sense paths, and only one is fast

From the [LT8390A datasheet](https://www.farnell.com/datasheets/2311639.pdf)
(block diagram / "Main Control Loop"):

* **LSP/LSN** across `R_SENSE` (on this board **R9 = 2 mΩ**) → amplifier A1 →
  buck/boost peak-current comparators A3/A4. This is the **fast inner
  current-mode loop**; `VC` sets its threshold. It needs a direct, Kelvin,
  low-inductance connection. **Never put anything in series with this.**
* **ISP/ISN** across `R_IS` (on this board **R18 = 2 mΩ**, in the output path) →
  error amp **EA2**, whose threshold is set by `CTRL` (5–100 mV). EA2 is
  diode-ORed with the voltage-loop EA1 into `VC`. This is the **slow outer CC
  limit**, not the inner loop. The datasheet explicitly balances EA1/EA2 gains so
  one `VC` compensation works for both, and expects the ISP/ISN node to be
  RC-filtered (near-DC). **This is the node to instrument.**

So your "the LT chip expects direct access to the shunt" instinct is right for
**LSP/LSN** and wrong for **ISP/ISN**. A gain stage on ISP/ISN sits in the outer
loop, where some delay is already tolerated — propagation delay is a second-order
concern here. The first-order concern is loop gain.

Other ISP/ISN facts worth knowing:

* `ISMON = 10·V(ISP−ISN) + 0.25 V` (monitor output; the ADC path).
* A separate **ISP/ISN over-current threshold of 750 mV**.
* ISP/ISN supports **low-side and high-side** common mode, with an internal
  switchover at ≈1.7–1.8 V.

---

## 2. What ×10 buys you (R18 = 2 mΩ)

| range | effective R_IS | CTRL window 5–100 mV | resolution |
| --- | ---: | ---: | ---: |
| 1× (direct) | 2 mΩ | 2.5 A … 50 A (design cap 20 A) | 40 mA/code |
| 10× | 20 mΩ | **0.25 A … 5 A** | **4 mA/code** (≈25 codes per 0.1 A) |

Combined **0.25 A … 20 A**, with the two ranges overlapping between 2.5 A and
5 A — so the switch point can sit wherever the gain/offset trade is best.

It also **fixes your offset in the right place**: the +3.65 A you are cancelling
in firmware is the chip's ISP/ISN-referred offset. A ×10 pre-gain divides that
offset by 10 when referred back to the shunt, so low-end accuracy and the
settable minimum both improve — this is the hardware version of the
`CC_CURRENT_OFFSET_MA` fudge.

---

## 3. The catches, in order of how much they will hurt

### 3.1 CC loop gain / crossover moves ×10  ← the real problem

EA2's small-signal gain from output current to `VC` is proportional to the sensed
volts-per-amp, i.e. to `R_IS` (or effective `R_IS`). EA1 and EA2 share `VC` and
its compensation *because their gains were balanced for the nominal `R_IS`*.
Multiply the sensed signal by 10 and the CC loop crossover moves up ~10× — from
(tens of kHz) toward the 1 MHz switching frequency, where the plant phase is
poor. That is the instability you were worried about, and it appears even if the
amplifier itself is infinitely fast.

Mitigations:

1. **Switch the `VC` compensation with the range.** Add a second RC network on
   `VC`, selected by the same GPIO/mux as the gain, scaled to move the crossover
   back down ~10× in the low range. This is the clean fix.
2. **Design the compensation for the ×10 case** and accept a 10× slower CC loop in
   the 1× high-current range. A slow current limit at 20 A may be acceptable
   (it is a ceiling, not a fast regulator), but verify the transient.
3. **Do neither and measure** — only if a bench Bode/current-step shows adequate
   phase margin at both gains. Do not assume it; the numbers say it will be close.

### 3.2 Common mode / level shift

ISP/ISN sits at the shunt's common mode. On the current board `R18` is high-side
in the output path, so ISP/ISN is at **V_OUT (0–60 V)**. A ×10 stage must present
a *differential* voltage at that common mode — a standard ground-referenced
high-side current-sense amp (INA2xx-class) outputs single-ended and is not
directly usable; you would need a floating/level-shifted differential drive.

**Much simpler: move `R_IS` low-side for rev3.** The chip supports ISP/ISN common
mode below ~1.8 V, so a ground-referenced difference amp can drive `ISP = amp
out`, `ISN = amp ref`. Costs: the load return floats by `I·R_IS` (40 mV at 20 A
with 2 mΩ / 90 mV at 4.5 A in the ×10 range) and the sense ground must be
Kelvin-referenced. Usually acceptable; decide early because it changes the
power-path layout.

### 3.3 Switching transients

A gain change is a ×10 step in the CC feedback. Switching it while regulating
will kick the loop (and can momentarily release the current limit). Gate the
switch on `supply.enabled == false`, or at zero/low current, then let it settle.
Firmware already owns `supply.enabled`, so this is cheap to enforce — but the
hardware must not glitch the ISP/ISN node during the mux transition
(make-before-break, and keep the RC filter on the chip side of the mux).

### 3.4 Scaling side effects the firmware must track

* **ISMON/ADC gain changes 10×** with the range — the current display and the
  software OC backstop must be scaled per range (a new state field).
* **The 750 mV ISP/ISN fast OCP moves**: 750 mV at ISP/ISN is 10× less shunt
  voltage in the ×10 range, so the hardware trip point shifts. Decide whether
  that is acceptable or add a compensating clamp.
* The `CTRL`-programmed limit itself is unchanged (5–100 mV at ISP/ISN) — only
  what that means in amps changes.

### 3.5 Amplifier requirements

Low offset and low drift (that is the whole point — keep its input offset well
under the chip's), bandwidth ≥10× the CC crossover in both configurations, good
CMRR, and a differential/level-shifted output if you keep high-side sensing.

---

## 4. Alternatives

* **Switchable sense taps across two series shunts.** Keep 2 mΩ, add 18 mΩ in
  series for the low range, bypass the 18 mΩ with a FET in the high range, and
  switch the Kelvin taps (sense across 2 mΩ in high range, 2+18 mΩ in low
  range). Keeps an amplifier out of the signal path and, because you sense only
  across 2 mΩ in the high range, the bypass FET's `R_DS(on)` does not corrupt the
  high-range reading. But it adds a power FET and has the **same ×10 loop-gain
  change**, so the compensation problem is identical.
* **Bigger fixed shunt.** Simplest, but 10 mΩ caps you at ~9 A and 20 mΩ at
  ~4.5 A. No 20 A.
* **Different controller with a wider CC window**, or a dedicated CC loop in
  cascade. A respin-scale change; only if the LT8390A's 20:1 window is otherwise
  a blocker.

---

## 5. Recommendation

Do it, with three things designed in from the start:

1. **Switch the range only while the output is disabled**, with a
   make-before-break mux and the ISP/ISN RC filter on the chip side.
2. **Switch the `VC` compensation with the range** (or prove the ×10 crossover
   is stable on the bench). This is the item most likely to be forgotten.
3. **Prefer low-side `R_IS`** to avoid a high-common-mode differential stage.

Prototype the front end on a breakout against the real board and measure the CC
loop phase margin at both gains with a current step (scope `VC`), before
committing it to rev3.

---

## 6. LCSC / JLCPCB BOM (checked against the local database, 2026-09-28)

All parts below are `Extended` library (there is no Basic-library current-sense
amp), in stock at LCSC as of the query. Stock/price are the DB's numbers.

### 6.1 Sense amplifier — gain 10 V/V, low-side, in stock

| MPN | LCSC | Gain | BW | Vos (max) | CM range | Package | Stock | Price |
| --- | --- | ---: | ---: | ---: | ---: | --- | ---: | ---: |
| **INA241A1IDDFR** | **C19712054** | 10 | 1.1 MHz | ±10 µV | −5…110 V | SOT-23-8 | **2661** | $4.51 |
| INA241A1IDR | C22427304 | 10 | 1.1 MHz | ±10 µV | −5…110 V | SOIC-8 | 154 | $4.46 |
| INA241B1IDDFR | C7433256 | 10 | 1.1 MHz | 150 µV | −5…110 V | SOT-23-8 | 51 | $3.99 |

**Pick: INA241A1IDDFR (C19712054).** Gain 10, 1.1 MHz, 5 µV typ / ±10 µV max
offset (0.25 µV/°C), 166 dB CMRR, bidirectional with a programmable REF, and
"enhanced PWM rejection". The A version's 5 µV offset is 2.5 mA referred to a
2 mΩ shunt — negligible next to the chip's own offset. The B version (150 µV,
$0.50 cheaper) is the budget fallback.

**Not suitable:** `LT1999CMS8-10#PBF` (C665955) — 1.5 mV max offset, which is
**0.75 A** referred to 2 mΩ, plus 5 V-only supply and 10 in stock. Fine part,
wrong offset for this job.

### 6.2 If gain 20 is acceptable (even lower floor, clean 2.5 A crossover)

Gain 20 puts the low range at 0.125–2.5 A, which meets the 1× range (2.5–20 A)
exactly, with no overlap gap. The loop-gain shift is 20× instead of 10×.

| MPN | LCSC | Gain | BW | Vos | Package | Stock | Price |
| --- | --- | ---: | ---: | ---: | --- | ---: | ---: |
| INA240A1D | C1346458 | 20 | 400 kHz | 5 µV | SOIC-8 | 190 | $1.69 |
| INA240A1PWR | C93965 | 20 | 400 kHz | 5 µV | TSSOP-8 | 5247 | $2.48 |
| INA241A2QDRQ1 | C31327639 | 20 | 1.1 MHz | 3 µV | SOIC-8 | 56 | $5.92 |
| INA181A1IDBVR | C2058943 | 20 | 350 kHz | 25 µV | SOT-23-6 | 3322 | **$0.33** |
| INA290A1IDCKR | C2908054 | 20 | 1.1 MHz | 25 µV | SC-70-5 | 879 | $3.01 |
| COSINA241A2TR | C54934062 | 20 | 1.1 MHz | — | SOT-23-8 | 2421 | $1.55 |

Watch bandwidth: 350–400 kHz (INA181/INA240) may be marginal if the ×20 CC
crossover is high; the 1.1 MHz INA241A2 is the safe one. `COSINA241A2TR` is a
low-cost clone — verify before trusting it in the loop.

### 6.3 Range switch (1× / 10×)

| MPN | LCSC | Function | Ron | Package | Stock | Price |
| --- | --- | --- | ---: | --- | ---: | ---: |
| **TS5A23157DGSR** | **C11133** | dual SPDT (2× 2:1) | 10 Ω | MSOP-10 | 5000 | $0.51 |
| TMUX1574PWR | C2673443 | quad SPDT (4× 2:1) | 2 Ω | TSSOP-16 | 5975 | $0.59 |
| SN74LVC1G3157DCKR | C38663 | single SPDT | 6 Ω | SC-70-6 | 99304 | $0.081 |

The **TS5A23157** is the natural fit: one 2:1 for `ISP`, one for `ISN`, selected
by a single GPIO. The **TMUX1574** has two spare channels to switch the `VC`
compensation with the same control, and brings fault protection / fail-safe
disconnect.

### 6.4 Shunt (4-terminal Kelvin, low-side)

| MPN | LCSC | Value | Power | Tolerance | Package | Stock | Price |
| --- | --- | ---: | ---: | ---: | --- | ---: | ---: |
| **WSLP27262L000FEA** | **C2076510** | 2 mΩ | 5 W | ±1%, ±75 ppm/°C | 2726 | 2000 | $2.18 |
| WSLP27261L000FEA | C500635 | 1 mΩ | 7 W | ±1%, ±75 ppm/°C | 2726 | 2289 | $2.93 |
| WSLP2726L5000FEA | C844297 | 0.5 mΩ | 5 W | ±1%, ±75 ppm/°C | 2726 | 1476 | $1.13 |

### 6.5 Design notes for this BOM

* **Bias the INA241 `REF` pin to ~200 mV, not GND.** At gain 10 the output is
  `I × 20 mΩ` = 5–100 mV across the range, which sits on the output stage's
  near-ground rail. A 200 mV reference lifts it to 205–300 mV; drive `ISN` from
  the same low-impedance node so `V(ISP)−V(ISN)` stays `10·V_shunt` and the CM
  stays below the LT8390A's 1.8 V low-side switchover.
* Keep the ISP/ISN RC filter on the **chip side** of the mux.
* Switch the range only with the output disabled (`supply.enabled == false`).
* The ×10 amp multiplies EA2's loop gain ×10 — switch the `VC` compensation with
  the range (use the TMUX1574's spare channels), or prove the crossover is stable.

### 6.6 Wiring: one amplifier, not two — OUT→ISP and REF→ISN

"CSA is differential-in, single-ended-out" is true, and it does not mean you need
two of them. The LT8390A's ISP/ISN is the input of an internal **difference**
amplifier: it only measures `V(ISP) − V(ISN)`. Each pin is a high-impedance sense
input with its own wide common-mode range, and the part switches to a low-side
input mode below ~1.8 V, so it does **not** require a balanced, floating, or
shunt-driven pair.

A current-sense amp output is `V_OUT = V_REF + G·V_shunt` (REF sets the
zero-current level). So:

```
INA241 IN+  ── shunt+  (load-return side)
INA241 IN−  ── shunt−  (board GND side)
INA241 OUT  ──(mux)──  LT8390A ISP
INA241 REF  ──(mux)──  LT8390A ISN
```

gives

```
V(ISP) − V(ISN) = V_OUT − V_REF = G·V_shunt
```

which is exactly the differential the chip regulates against `CTRL`. Tying `ISN`
to the same REF node also parks the ISP/ISN common mode at the REF voltage
(~200 mV), comfortably inside the chip's low-side range. The REF node must be a
**low-impedance** source (buffer, or a stiff divider with a bypass cap) because
the LT8390A's ISP/ISN bias current flows through it and would otherwise appear as
a threshold offset.

For the 1× range the same mux just selects the shunt directly:

```
shunt+ ──(mux)── ISP
shunt− ──(mux)── ISN
```

Both paths therefore present the same polarity (`ISP` on the side current enters
the shunt) and both sit in low-side mode; the only change across the switch is a
~200 mV common-mode step, which is why the switch should happen with the output
disabled.

**Two amplifiers would be wrong:** their independent offsets and reference
voltages would add straight into the differential the chip sees, doubling the
error you are trying to remove, and the two REF nodes would fight.
