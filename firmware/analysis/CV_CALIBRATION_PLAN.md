# CV set-accuracy: static calibration + metrology servo — implementation plan

Status: **phases 1+2 implemented** (firmware + fit.py; constants ship at their
analytic defaults until the first bench fit — `CV_CAL_VALID = false`). The
bench procedure is BENCH.md §8. Predecessor of the rev4 hardware
reference fix; both are designed to coexist (the servo absorbs what the
hardware doesn't).

## 0. Goal & non-goals

- **Goal:** setting `20.000 V` puts **20.00–20.02 V** (±20 mV) at the output
  terminals, at any board temperature, and the OLED shows a DMM-traceable
  value. Today's open-loop map lands ±0.5–1 % off (20.13 V observed) and
  drifts.
- **Non-goals:** sub-LSB setting granularity. One CV DAC LSB is
  `3.3 V / 4095 × (R19/R36) = 28.8 mV` at the output — that is accepted as the
  step size. **No dithering.**
- The LT8390A's own FB loop is not the problem — it holds FB at its reference
  tightly. The problem is that the *setpoint path* (DAC → 3.3 V rail →
  R19/R20/R36 → chip's ±1.5 % FB reference) is open-loop, and every error in it
  is multiplied by the **35.7×** summing gain.

## 1. Why a static cal and a servo *both* (the mix)

| Error source | Size at 20 V | Static cal (fit) | Servo |
|---|---|---|---|
| +3V3 as DAC VREF (±1–2 %) | ±1–2 V | removed | removed |
| R19/R20/R36 ratio (±1 %) | ±0.5–1 V | removed | removed |
| LT8390A FB ref (±1.5 % spec) | up to ±1.1 V | removed | removed |
| DAC INL (±1–4 LSB) | ±30–120 mV | only on average | **removed** (measured out) |
| Resistor TCR + VREF tempco | ~2–10 mV/°C at output | **not** removed — drifts back | **removed** (continuous) |
| Load-path drop (beads + F2 at current) | ~10–50 mV | partially | **removed** (measures connector side) |
| DAC quantization (1 LSB) | 28.8 mV steps | no | no (accepted) |

Static-only leaves INL wiggle and all temperature drift on the table.
Servo-only works, but it would have to hold a large trim (bad: it exercises
its authority constantly, settles slowly after every setpoint change, and if
U12 ever dies the output is left with a big stale correction). **Fit first, then
let the servo only carry the small, slowly-moving residue** — that is also the
standard structure in test equipment: factory cal constants + a slow digital
trim against a better reference.

## 2. Chain of custody

```text
bench DMM ──(once, 2–3 points)──▶ U12 VBUS scale factor k ──▶ CV map fit (A, B)
                                                                      │
v_set ──▶ fitted map A−B·code ──(+ trim)──▶ CV DAC ──▶ LT8390A ──▶ VOUT
   ▲                                                        │
   └────────── servo compares v_set vs k·U12_VBUS ◀─────────┘ (100 ms cadence)
```

The INA228's VBUS path is **±0.05 % max gain error (±0.01 % typ), 20 ppm/°C
drift, 195 µV resolution** — after the one-time DMM cross-check it is a
~±0.02 % transfer standard, roughly 50× better than the chain it calibrates.
U12's VBUS pin is on `Bus_V` (connector side of F2), so the servo regulates the
*load-terminal* voltage, absorbing the fuse/bead drop for free.

## 3. Phase 1 — static calibration (fit the map)

### 3.1 Firmware changes (small)

1. `board.rs` — new constants, defaults disabled until a fit exists:
   ```rust
   pub const CV_CAL_VALID: bool = false;
   /// VOUT at DAC code 0, in mV (fit: ≈72 400).
   pub const CV_CAL_A_MV: u32 = 72_400;
   /// VOUT change per +1 CV DAC code, in µV (fit: ≈28 790 → the map is inverted).
   pub const CV_CAL_B_UV_PER_LSB: u32 = 28_790;
   /// U12 VBUS scale correction as a rational: bus_mv·num/den (k≈1.0003 → e.g. 10003/10000).
   pub const INA228_OUT_VBUS_SCALE_NUM: u32 = 10_000;
   pub const INA228_OUT_VBUS_SCALE_DEN: u32 = 10_000;
   ```
2. `control/dac_cv.rs` — `mv_to_code` / `code_to_vout_mv` / the boot-log helpers
   use `A − B·code/1000` when `CV_CAL_VALID`, else today's analytic
   resistor-derived map (kept: it documents the physics and is the fallback).
3. `sense/ina_sense.rs` — optional VBUS scale on construction; the output
   monitor is built with `num/den`, the input monitor with 1/1. Apply at the
   `Ina228Reading::bus_mv` source so display *and* servo see the same
   corrected number (current/power inherit k — a 0.05 % effect, documented).
4. `state.rs` — publish the raw DAC codes into `SupplyState`
   (`dac_cv_code`, `dac_cc_code: u16`) so the sweep log can carry them.
5. `control/sweep.rs` — extend the per-point log line with
   `cv_code=… u12_vbus=…`. The sweep stays the vehicle: it already walks
   32 points at 2 s dwell with settle/sample windows.

### 3.2 Host side

`analysis/cv_cal/fit.py` (new, plain numpy-free python is fine — 2-parameter
least squares is a few lines):

- Input: the RTT log of one `OUTPUT V SWEEP` run (`sweep point …` lines).
- Output: `CV_CAL_A_MV`, `CV_CAL_B_UV_PER_LSB`, residual stats
  (max/typical |residual| — this is the DAC INL made visible), and a
  copy-paste `board.rs` block.
- Deliberately **deterministic, no LLM**: this is re-run after hardware
  changes and must be reproducible/auditable. (An LLM can do it; polyfit
  already did.)

### 3.3 Bench procedure

1. **Once per board (needs the DMM):** set ~5 V, ~20 V, ~48 V (no load),
   write down DMM vs the `supply[meas]` log's `ina_out_vout` at each →
   k = DMM/U12 (expect k within 1.0005; if not, note the INA228's VBUS offset
   spec ±2.5 mV max only matters near 0 V).
2. Run `CFG → OUTPUT V SWEEP` with `OUTPUT_SWITCH_REQUIRE_LOAD = false`,
   RTT captured, board at room temp, input on a stable PD rail.
3. Run `fit.py` → paste constants → `CV_CAL_VALID = true` → rebuild, flash.
4. Verify: set 20.000 V → DMM within ±15 mV static; step 5/10/20/30/48 V and
   record residuals (expect ±30–60 mV point-to-point — that is INL + ½ LSB).
5. Repeat the sweep once at a mid load (~3 A) and compare fits: the Δ
   quantifies the bead+F2 drop (expected <50 mV at ≤5 A; the servo absorbs it
   in Phase 2, but keep the no-load fit as the map).

## 4. Phase 2 — metrology servo (the trim loop)

### 4.1 Constants (`board.rs`)

```rust
pub const CV_SERVO_ENABLE: bool = true;
/// One trim step per period; ≥2 INA228 conversion cycles (2 × ~50 ms).
pub const CV_SERVO_PERIOD_MS: u64 = 500;
/// Stop stepping when |v_set − vout| is below this (½ LSB + margin: converges,
/// cannot limit-cycle, since one step moves 28.8 mV).
pub const CV_SERVO_DEADBAND_MV: u32 = 15;
/// Total servo authority. 64 LSB ≈ 1.84 V: enough for drift + INL, far below
/// the 105 % OV guard, so a lying INA228 cannot walk the output far.
pub const CV_SERVO_MAX_TRIM_LSB: i16 = 64;
```

### 4.2 Mechanism (`control/supply.rs`)

- `struct CvTrim { lsb: i16, last_step: Instant }` inside `SupplyController`.
- Every supply tick, when **all** of these hold:
  - `stage == Running`, `mode == Cv`, `fault == None`,
  - `!cc_active`, `!sagging` (VOUT below setpoint is *correct* in CC — the
    anti-windup; also keeps the servo out of the chip's CV↔CC handover),
  - `!sweep_running` (a sweep moves the setpoint continuously; the servo
    would chase it — freeze it, the sweep measures the static map),
  - `out_ina.ok`, `rev3_power_path`,
  - the CV slew has settled (`self.cv.code == target`: never trim mid-ramp),
  - `elapsed ≥ CV_SERVO_PERIOD_MS` since the last step;
  then: `err_mv = v_set − out_ina.bus_mv` (k-corrected by Phase 1);
  if `|err_mv| > DEADBAND` step `trim` by 1 LSB toward the error
  (**output low → code − 1**: the map is inverted), clamp to ±`MAX_TRIM_LSB`,
  record `last_step`.
- **Trim folds into the slew target**, not on top of it:
  `target = mv_to_code(v_set) − trim`, so the existing 4 LSB/ms slew limiter
  still owns every code change and the servo can never jump the DAC.
- **Reset `trim = 0` on** setpoint change, mode change, disable, or fault —
  the fitted map is the correct starting point at every voltage (a B-scale
  error at 20 V is 3.5 LSB but only 0.9 LSB at 5 V; carrying the old trim
  would over-correct).
- Failure mode: any condition breaking (U12 gone, CC entered, sweep started)
  **freezes** the trim — the output stays at the last code, i.e. on the fitted
  map (±0.1 % class), and the LED already codes a missing U12. The servo can
  only ever move 1 LSB per 500 ms, bounded to ±64 LSB — it is structurally
  incapable of "half-enabling" or stepping the output quickly.

### 4.3 Logging

- Edge line on each step (throttled to the first of a burst plus every 8th):
  `cv servo: err=+31 mV -> trim=-3 lsb (vset=20000 vout_ina=19969)`.
- Add `trim=N` to the periodic `supply[ctl]` line.
- One `cv servo: settled, err=+8 mV` line when it parks inside the deadband.

### 4.4 Bench items (append to BENCH §6/7)

- [ ] 20.000 V no load: trim converges to ≤ ±1–2 LSB, `settled` line, DMM
      within ±20 mV.
- [ ] Step 20 V→30 V→5 V: trim resets, re-settles each time; no interaction
      with the CV slew (scope shows only the normal slew ramp).
- [ ] CC freeze: resistive load that trips the hardware CC limit → trim
      frozen while `cc_active`, resumes when back in CV.
- [ ] Temperature: heat gun on the R19/R20/R36 area → DMM stays put while the
      trim log creeps (this is the servo earning its keep; without it the
      output would drift mV/°C).
- [ ] Unplug U12 mid-run: trim freezes, LED code 7, output holds.
- [ ] Sweep with servo enabled: no trim motion during the run (frozen), the
      sweep's static map is uncontaminated.

## 5. Phase 3 (optional, later) — per-board cal in the CAT24C512

When JP4/JP5 are bridged the MCU already reaches the CAT24C512 on I2C4; store
`struct CvCal { magic, version, crc, a_mv, b_uv_per_lsb, vbus_num, vbus_den }`
at a fixed offset **past the end of the TPS26750 flash image** (first check
`TPS26750_FULL_FLASH_LEN` in `src/drivers/tps26750_full_flash.rs` and pick the
offset with margin; the EEPROM is 64 KB). Boot: read, verify magic/version/CRC,
use it when valid, else fall back to the compiled constants. This makes cal
data survive rebuilds and keeps one firmware image for multiple boards —
worth it only once more than one board exists; until then compiled constants
(the project already calibrates `ISENSE_ZERO_MV`/`CC_CURRENT_OFFSET_*` that
way) are consistent and fine.

## 6. Rev4 hardware notes (planned separately, complements this)

- Precision 3.3 V reference (ADR4533/REF3033 class, ≤0.05 %, ~10 ppm/°C)
  feeding VREF+ (cut the +3V3 tie; the firmware already leaves VREFBUF
  high-Z, which is correct for an external ref). Improves the DAC *and* the
  ADC paths ~20× on the reference term.
- 0.1 % / 25 ppm/°C parts (or a matched network) for R19/R20/R36 — the
  *ratio* is what matters; a matched trio kills both map terms at once.
- After rev4 lands, re-run the Phase 1 fit (new hardware = new A/B), and
  expect the servo's typical trim to collapse to ±1–2 LSB.

## 7. Expected end state

| Path | Accuracy at 20 V |
|---|---|
| Today (open-loop map) | ±0.5–1 % (20.13 observed) |
| Phase 1 only (fit) | ±30–60 mV at the fit temperature, drifting ~2–10 mV/°C |
| Phase 1 + 2 (this plan) | **±20 mV (deadband 15 mV + U12 residue), held over temperature** |
| + rev4 hardware | same number, but the static cal alone is nearly as good and the servo idles |

Setting granularity stays 28.8 mV (one DAC LSB) — accepted.

## 8. Order of work

1. Bring-up first: this builds on a working rev3 output stage (BENCH §1–7).
2. Phase 1 firmware (~half day) → bench fit (~1 h).
3. Phase 2 firmware (~half day) → bench items (~2 h).
4. Phase 3 whenever a second board exists.