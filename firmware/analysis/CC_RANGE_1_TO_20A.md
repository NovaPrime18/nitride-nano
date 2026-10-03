# CC current-limit range: 2.5 A floor, and how to get 1–20 A

Bench case (2026-09-27/28): 12 V output, load in CC, I-LIM stepped
0.1/0.5/1/1.5/2 A. The load's own ammeter **agrees with the nitride-nano
display**, and the output current floors at **2.5 A** and only moves ~0.7 A per
1 A of setpoint. A second run (22 V setpoint, load CV-clamped to 12 V) showed a
~3.8 A floor.

Diagnosis: this is the LT8390A's CC loop legal range, not a reading offset.
`2.5 A` is exactly the smallest limit the part can command on a 2 mΩ shunt.

---

## As built (2026-09-28): keep R18 = 2 mΩ, cancel the CC offset, settable 3.75–20 A

Decision: leave the fitted 2 mΩ shunt in place, cancel the loop's low-end offset
in firmware, and make the setpoint mean the current actually drawn.

The bench data (0.1 A → 3.75 A, 0.2 A → 3.85 A, earlier 0.3 A → 3.95 A,
0.4 A → 4.10 A) is a clean **1:1 slope with a +3.65 A intercept** — a CTRL /
ISP-ISN offset near latch-off, not a hard floor and not a DAC map error.

* `board::CC_CURRENT_OFFSET_MA = 3_650` — bench-measured loop offset.
* `board::CC_SET_MIN_MA = CC_CURRENT_OFFSET_MA + 100` = **3 750 mA**;
  `CC_SET_MAX_MA = IOUT_MAX_MA`. The I-LIM screen now reads the current actually
  drawn: 3.75–20 A.
* `CcDac::setpoint_to_code` clamps the setpoint, subtracts the offset, then runs
  the network/chip inversion. Set 3.75 A → command 0.1 A → code 294 / 252 mV,
  the point that drew 3.75 A on the bench; 20 A → command 16.35 A.
* Two earlier passes (clamp the floor to 3 A, then 6 A) were wrong: raising the
  settable floor only *raised* the achievable minimum.
* `dac_cc.rs` still models the full datasheet transfer (linear + 1.15–1.35 V
  transition) for the commanded current.
* Boot line: `boot: cc range settable 3750..20000 mA; sustain 2500..50000 mA; datasheet cold-start 4500 mA (R18=2 mOhm, ISMON 20 mV/A); ctrl 300..1350 mV`.

To re-calibrate: measure the drawn current at two low setpoints and set
`CC_CURRENT_OFFSET_MA` to the intercept (`actual − setpoint`). Caution: the 10 A
run on 2026-09-27 tracked to <1 %, so the offset fades with current — check a
mid-range setpoint before trusting the top of the range. A genuinely lower
minimum still needs a larger shunt (§3); the offset is not the floor.

---

## 1. Why the floor exists (datasheet, not firmware)

The LT8390A programs its ISP/ISN current limit from the `CTRL` pin
([datasheet, pin functions](https://www.farnell.com/datasheets/2311639.pdf)):

```
threshold = min( (V_CTRL − 0.25 V)·0.1 , 0.1 V )        (sense volts)

  0.30 V ≤ V_CTRL ≤ 1.15 V   linear, threshold 5 mV → 90 mV
  1.15 V ≤ V_CTRL ≤ 1.35 V   smooth transition 90 mV → 100 mV
           V_CTRL ≥ 1.35 V   constant 100 mV full scale
           V_CTRL < 0.30 V    chip STOPS SWITCHING (latch-off 285/300/315 mV)
```

so `I_limit = threshold / R18`, and with the firmware's own constants
(`CTRL_SERIES_OHM=10k`, `CTRL_VREF_OHM=357k`, `CTRL_GND_OHM=75k`, VREF=2.0 V)
the map commands `V_CTRL(mV) = 250 + 10 · threshold(mV)`, i.e.
`I_limit(mA) = (V_CTRL − 250) · 100 / R18_mΩ`.

Current `board.rs` has `ISENSE_SHUNT_MOHM = 2`, so:

| I-LIM set | CTRL commanded | CC code | chip state |
| ---: | ---: | ---: | --- |
| 0.1 A | 252 mV | 294 | **below 0.3 V → stops switching** |
| 0.5 A | 260 mV | 305 | below latch-off |
| 1.0 A | 270 mV | 320 | below latch-off |
| 1.5 A | 280 mV | 334 | below latch-off |
| 2.0 A | 290 mV | 349 | below latch-off |
| **2.5 A** | **300 mV** | **362/363** | first legal point (5 mV / 2 mΩ) |
| 8.0 A | 410 mV | 521 | legal |

Every setpoint in the bench list is outside the part's controllable window, which
is why they all land on the same ~2.5 A rail. The firmware currently does not
know this: `dac_cc::ma_to_code` happily maps them to a latch-off command.

**Hard range limit:** the part's window is `5 mV … 100 mV = 20:1`. No single
shunt value can give 0.1 A to 20 A — that is **200:1**.

---

## 2. Shunt trade-off table

`I_min = 5 mV / R18`, `I_max = 100 mV / R18` (linear top 90 mV at 1.15 V).
`0.1 A resolution` = threshold step `0.1·R18` mV = `0.1·R18/0.806` DAC codes.

| R18 | I_min (0.3 V) | I_max (90 mV, linear) | I_max (100 mV) | mV per 0.1 A | codes per 0.1 A | ISMON gain | P @ 20 A | Vsense @ 20 A |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 2 mΩ (fitted) | 2.50 A | 45 A | 50 A | 0.20 | 2.5 | 20 mV/A | 0.8 W | 40 mV |
| 4 mΩ | 1.25 A | 22.5 A | 25 A | 0.40 | 5.0 | 40 mV/A | 1.6 W | 80 mV |
| 4.5 mΩ | 1.11 A | 20 A | 22.2 A | 0.45 | 5.6 | 45 mV/A | 1.8 W | 90 mV |
| **5 mΩ** | **1.00 A** | 18 A | **20 A** | 0.50 | 6.2 | 50 mV/A | 2.0 W | 100 mV |
| 10 mΩ | 0.50 A | 9 A | 10 A | 1.00 | 12.4 | 100 mV/A | 4.0 W | 200 mV |
| 50 mΩ | 0.10 A | 1.8 A | 2 A | 5.00 | 62 | 500 mV/A | 20 W | 1000 mV |

Notes:

* `0.1 A resolution` is a *firmware/DAC* property and is fine at every value
  above; a larger shunt actually improves it (more codes per 0.1 A).
* The 5 mΩ bottom (1.00 A) sits exactly on the latch-off edge (min 285 mV,
  typ 300, max 315 mV), so expect part spread of roughly ±0.3 A there. Keep the
  firmware floor a little above it (≈1.1–1.2 A) for margin.
* 5 mΩ at 20 A is 2 W in the shunt — use a ≥2 W, 4-terminal (Kelvin) part.

---

## 3. Recommendation

**Fit R18 = 5 mΩ and model the CTRL transition region → 1.0 A … 20.0 A with
0.1 A resolution.** This is the closest single-range answer to the request;
0.1 A … 20 A is not reachable without a range change (§5).

Two constants in `src/board.rs`:

```rust
pub const ISENSE_SHUNT_MOHM: u32 = 5;   // was 2
pub const ISENSE_MV_PER_A: u32 = 50;    // was 20  (= 10 * R18)
```

Everything else follows from `ISENSE_SHUNT_MOHM`, but the **top of the range
needs the datasheet's 1.15–1.35 V transition modeled**, otherwise 18–20 A all
compress onto ~18 A. That is the `dac_cc.rs` change in §4.

If 20 A must stay strictly inside the guaranteed-linear band, fit **R18 = 4.5 mΩ**
instead: 1.11 A … 20 A, all at or below the 90 mV / 1.15 V linear top.

---

## 4. Firmware patch

### 4.1 `src/board.rs` — window constants and derived range

```rust
/// LT8390A CTRL → ISP/ISN threshold window (datasheet):
///   0.30–1.15 V linear (5–90 mV), 1.15–1.35 V transition to 100 mV,
///   ≥1.35 V constant 100 mV, <0.30 V stops switching.
/// Usable span 5 mV → 100 mV = 20:1. With R18 = 5 mΩ that is 1 A → 20 A;
/// no single shunt can cover 0.1 A → 20 A (200:1).
pub const LT8390_CTRL_MIN_SENSE_MV: u32 = 5;
pub const LT8390_CTRL_LINEAR_MAX_SENSE_MV: u32 = 90;
pub const LT8390_CTRL_MAX_SENSE_MV: u32 = 100;
pub const LT8390_CTRL_LINEAR_TOP_MV: u32 = 1_150;
pub const LT8390_CTRL_SAT_MV: u32 = 1_350;

/// Smallest / largest current limit the fitted shunt can command.
/// `mV / mΩ = A`, so `sense_mV · 1000 / R18_mΩ` is mA.
pub const CC_MIN_MA: u32 = LT8390_CTRL_MIN_SENSE_MV * 1_000 / ISENSE_SHUNT_MOHM;
pub const CC_MAX_MA: u32 = LT8390_CTRL_MAX_SENSE_MV * 1_000 / ISENSE_SHUNT_MOHM;
```

### 4.2 `src/control/dac_cc.rs` — piecewise transfer

Replace `ctrl_mv_for_current` and `current_ma_for_code`; `ma_to_code` and
`ctrl_mv_for_code` are unchanged (the former calls the new transfer).

```rust
/// Sense-threshold (mV across R18) a requested limit needs, capped at the
/// 100 mV full-scale value.
fn thr_mv_for_current(i_ma: u32) -> u32 {
    let t = (i_ma as u64 * board::ISENSE_SHUNT_MOHM as u64) / 1_000;
    t.min(board::LT8390_CTRL_MAX_SENSE_MV as u64) as u32
}

/// CTRL-pin voltage (mV) that a requested limit needs, including the
/// 1.15–1.35 V transition to the 100 mV full-scale threshold.
fn ctrl_mv_for_current(i_ma: u32) -> u32 {
    let thr = Self::thr_mv_for_current(i_ma);
    if thr <= board::LT8390_CTRL_LINEAR_MAX_SENSE_MV {
        board::LT8390_CTRL_OFFSET_MV + 10 * thr
    } else {
        board::LT8390_CTRL_LINEAR_TOP_MV
            + 20 * (thr - board::LT8390_CTRL_LINEAR_MAX_SENSE_MV)
    }
}

/// Current limit (mA) a DAC code actually produces. Below the 0.3 V latch-off
/// the part stops switching, so this reports 0 rather than a small limit.
pub fn current_ma_for_code(code: u16) -> u32 {
    let vctrl = Self::ctrl_mv_for_code(code);
    if vctrl < board::LT8390_CTRL_LATCHOFF_MV {
        return 0;
    }
    let thr_mv = if vctrl <= board::LT8390_CTRL_LINEAR_TOP_MV {
        (vctrl - board::LT8390_CTRL_OFFSET_MV) / 10
    } else {
        board::LT8390_CTRL_LINEAR_MAX_SENSE_MV
            + (vctrl - board::LT8390_CTRL_LINEAR_TOP_MV) / 20
    }
    .min(board::LT8390_CTRL_MAX_SENSE_MV);
    (thr_mv as u64 * 1_000 / board::ISENSE_SHUNT_MOHM as u64) as u32
}
```

Round-trip at R18 = 5 mΩ (verified against the network model):

| set | thr | CTRL | code | real |
| ---: | ---: | ---: | ---: | ---: |
| 1.0 A | 5.0 mV | 300 mV | **362 = 298 mV** | **below latch-off** |
| 1.02 A | 5.1 mV | 301 mV | 363 = 300 mV | ~1.00 A |
| 5.0 A | 25 mV | 500 mV | 651 | 4.98 A |
| 10.0 A | 50 mV | 750 mV | 1011 | 9.98 A |
| 18.0 A | 90 mV | 1150 mV | 1588 | 17.98 A |
| 20.0 A | 100 mV | 1350 mV | 1876 | 19.99 A |
| ≥20 A | 100 mV | 1350 mV | 1876 | saturates at 20 A |

Quantization caveat: `ma_to_code` rounds to nearest, and one DAC code is
0.806 mV at the pin, so the exact 1.000 A request (CTRL 300 mV) rounds *down*
to code 362 = 298 mV, which is below the 300 mV latch-off. The first legal code
is 363 (CTRL 300 mV), reached at ≈1.02 A. Keep `CC_MIN_MA`/the UI floor at
≈1.05–1.1 A, or make the low-end mapping round up.

### 4.3 `src/main.rs` — boot line so the range is visible

```rust
defmt::info!(
    "boot: cc range {}..{} mA (R18={} mOhm, ISMON {} mV/A); ctrl {}..{} mV",
    board::CC_MIN_MA,
    board::CC_MAX_MA,
    board::ISENSE_SHUNT_MOHM,
    board::ISENSE_MV_PER_A,
    board::LT8390_CTRL_LATCHOFF_MV,
    board::LT8390_CTRL_SAT_MV
);
```

### 4.4 `src/control/supply.rs` — stop commanding a latch-off

Once the floor is known, a sub-floor request should be clamped to the floor and
logged, not mapped into the stop-switching region (today's behaviour). In
`tick`, where the CC target is chosen:

```rust
let cc_req = cc_target.min(i_limit);
let cc_eff = cc_req.clamp(board::CC_MIN_MA, board::CC_MAX_MA);
if cc_req < board::CC_MIN_MA {
    defmt::warn!(
        "supply: iset={} mA below hardware CC floor {} mA (R18={} mOhm); limiting at {} mA",
        cc_req, board::CC_MIN_MA, board::ISENSE_SHUNT_MOHM, cc_eff
    );
}
self.cc.set_current(cc_eff);
```

and use `cc_eff` (not `i_limit`) for the CV-mode software OC trip threshold and
for `cc_active`, or a sub-floor setpoint will false-trip the over-current latch.
This is the one behavior change in the patch; it makes the 2 mΩ board limit
*defined* at 2.5 A until the shunt rework lands.

---

## 5. If 0.1 A is a hard requirement

The chip's 20:1 window is fixed, so 0.1–20 A needs a **10:1 range change**.
Options, roughly in order of practicality:

1. **Gain-switched ISP/ISN front end.** Sense across a 5 mΩ shunt, feed ISP/ISN
   from a precision diff-amp with selectable ×1 (1–20 A) / ×10 (0.1–2 A) gain.
   Needs a low-offset, high-CMRR amp (e.g. INA240-class) and an analog switch on
   the gain network; offset and drift now matter at 0.1 A.
2. **Relay/load-switch second shunt.** Not recommended at these resistances: a
   switch in parallel with a 45 mΩ resistor must be ≪ 5 mΩ to hold the high
   range, which no practical MOSFET/relay achieves.
3. **Different controller with a wider CC window** (or a dedicated current-loop
   op-amp driving a pass element) — a rev3 change, not a rework.

The existing `analysis/ismon-calibration/` and `board::ISENSE_ZERO_MV` path only
fix the *reading*; the limit range is a hardware property.

---

## 6. Bench validation

1. After fitting R18 = 5 mΩ and updating the two constants, check the boot line
   reads `cc range 1000..20000 mA`.
2. Output on, no load → re-read `isense: raw=… (XXX mV)` and confirm
   `ISENSE_ZERO_MV` still matches (the ISMON offset is independent of R18, but
   the gain change makes any zero error 2.5× larger in amps).
3. CC load at 1.0 / 5 / 10 / 20 A: the load's ammeter should track the setpoint
   within a few percent, and `supply[ctl]: … cc_est=…` should match `iset`.
4. Confirm no `supply: iset=… below hardware CC floor` warning for setpoints
   ≥ 1.1 A, and that a 0.1 A setpoint logs the warning and limits at ~1 A
   instead of hiccupping.
