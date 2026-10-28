# nitride-nano firmware — bench bring-up checklist

Complete these steps **before** enabling full GaN power. Keep a current-limited bench supply on the input.

## 0. First-enable interlock (read this first)

The firmware refuses to enable the converter until a boot self-test passes. It
runs once, right after boot, with the output stage parked — nothing is
energised — and prints one `preflight:` line per check:

| Check | Tag when it goes wrong | Blocks? |
|---|---|---|
| ADC/DAC reference (VREFINT on ADC1 **and** ADC5, internal TS) | `[FAIL]` | **yes** |
| Thermistors plausible (both NTCs in −40…125 °C) | `[FAIL]` | **yes** |
| Gate-line readback: PB1=1, PB2=0, PB3=0, PB6=0 as driven | `[FAIL]` | **yes** |
| Input voltage within the 58 V design maximum | `[FAIL]` | **yes** |
| Input INA228 (U9) present | `[warn]` | no |
| Output INA228 (U12) present | `[warn]` | no |
| TPS26750 present (PD) | `[warn]` | no |
| SSD1306 present | `[warn]` | no |
| Output node parked (~0 V with the stage off) | `[warn]` | no |
| `BOOT0` option bytes (`nSWBOOT0`/`nBOOT1`) | `[warn]` | no |

The summary line ends the block:

```text
preflight: PASS - 8/10 checks, 2 warning(s); converter enable ARMED
preflight: FAIL - 1/10 checks failed, 2 warning(s). Converter enable BLOCKED ...
```

The USB-PD-side checks (input monitor, output monitor, TPS26750) report
`[ -- ]` **not applicable** — neither pass nor warning — when there is no PD bus
at all, which is the normal case for an XT90/DC feed (see README, "XT90 / DC
input parity"). Only a *live* PD bus with a missing device is a warning.

A `[FAIL]` latches `Fault::PreflightFailed` (OLED header `SELF-TEST`, LED code
14) and every enable attempt is refused with a log line saying so. Fix the
hardware named in the `[FAIL]` line; the override
`board::PREFLIGHT_BLOCK_ENABLE = false` exists **only** for a supervised bench
run where you have read and accepted that specific failure.

**Why these are fatal:** a bad VREF+/VDDA/EP joint or a damaged analogue
front-end means the ADCs *and both DACs* are blind — every regulation and
protection decision would be guesswork. A wrong gate-line level means the
three-pin enable sequence cannot be trusted (that is the "half-enabled output
stage" hazard). Invalid thermistors mean no thermal protection. An over-voltage
input is self-explanatory.

## 1. Tooling

- [ ] `rustup target add thumbv7em-none-eabihf`
- [ ] probe-rs detects STM32G474CEUx on SWD
- [ ] `cargo build --release` succeeds
- [ ] `defmt-rtt` logs visible (`DEFMT_LOG=info cargo run --release`)

## 2. Clocks & DAC (no power stage)

- [ ] Flash firmware; confirm boot log
- [ ] Measure VREF+ (~3.3 V: the board ties it to +3V3 and the firmware leaves VREFBUF high-Z)
- [ ] **ADC reference self-test** in the boot log: `boot: adc self-test: VREFINT raw=… (expect ~1500 with VREF+ = 3.3 V), internal TS raw=…`. VREFINT must land in ~1200–1800 and the internal temperature sensor between the rails. **If VREFINT is out of range the warning is real**: VREF+ (U2 pin 20) / VDDA (pin 21) is not at 3.3 V or is not soldered — and because the DACs share that reference, every CV/CC command is garbage too, so fix it before anything else. All five external channels then read exactly full scale (4095), the NTCs read "invalid" and an OverTemperature fault latches.
- [ ] Sweep PA4 DAC code 0→4095; log PA0 vs DMM on feedback sense node
- [ ] Sweep PA6 DAC; verify monotonic CC node voltage
- [ ] Verify the CV map against the divider network: the code-0 output is `CV_FB_REF_MV·(1 + R19/R36 + R19/R20)` and the slope is `(R19/R36)·DAC_VREF_MV/DAC_MAX_CODE` per code (`control/dac_cv.rs`, `board::CV_FB_*`). Confirm R19/R20/R36 on the assembled board match ~357k/10k/10k; if not, update those constants.

## 3. ADC scaling

- [ ] **Divide-by-check first**: the firmware prints its own divider arithmetic.
      With a USB-C source attached (so the input monitor answers) the `supply:`
      log carries a `vin divider cross-check` line naming the ADC reading, the
      monitor's reading and the exact `board::VBUS_SENSE_NUM` that would make them
      agree; a `vout divider cross-check` line does the same for
      `board::VOUT_SENSE_NUM` while the output is `Running` (the two only sense the
      same node with the load switch closed). Paste the constants and the ADC
      path — which is all an XT90/DC session has — is calibrated against the
      ±0.05 % monitors.
- [ ] With **no** USB-C (XT90 only) a DMM is the only reference: measure at the
      input pads and compare with the displayed `Vin`. `Vin = V_pin × 21` for the
      input divider (R44+R47 = 200 k over R97 = 10 k) and `Vout = V_pin × 25.8`
      for the output one (R85+R86 = 248 k over R87 = 10 k). If the displayed value
      differs, measure the divider's three resistors and the pin itself
      (10.00 V in must give 476 mV at PA7) before changing the constant.
- [ ] Apply known voltages to sense nets (safe low levels)
- [ ] Fit `VOUT_SENSE_NUM`, `VBUS_SENSE_NUM` in `board.rs` (rev3: 248 k/10 k = 85.14 V and 200 k/10 k = 69.6 V, from R85+R86/R87 and R44+R47/R97). The ISMON gain is **not** a constant any more — it comes from `CcRange::mv_per_a()` (20 mV/A at ×1, 200 mV/A at ×10)
- [ ] ISMON zero: enable the output with **no load**, set `board::ISENSE_ZERO_MV`
      from the `isense:` log's `raw (XXX mV)` field (do **not** calibrate with the
      converter parked — ISMON is unpowered then; see `analysis/ismon-calibration/`)
- [ ] Verify NTC readings at room temp; adjust `NTC_BETA` / `NTC_R25_OHM`

## 4. I2C buses

- [ ] **USB-C VBUS must be present for the PD bus.** The MCU-side PD-bus pull-ups
      (`R31`/`R32`) hang off `+3V3_LDO`, which is the **TPS26750's own internal LDO
      output** (U4 pad 1) — nothing else generates it. With no USB-C source the TPS
      is unpowered, that rail is dead, and I2C2 times out on *everything* (the TPS
      and both INA228s) even on a healthy board. An XT90-only session (logic runs
      fine off VPP) therefore has no PD bus, no PD negotiation and no INA228s. The
      firmware logs `boot: PD bus not answering - is USB-C VBUS present?` in that
      state and falls back to the display signature for the rev3 detect.
- [ ] No bodge needed on rev3: the PD bus is a real I2C2 on **PC4/PA8** and the UI bus a real I2C4 on **PC6/PB7**
- [ ] **Bus idle levels** in the boot log: `boot: bus idle levels: UI SCL(PC6) pu=1 pd=1 …`. `pd=1` proves the external pull-up is fitted and connected (the MCU's internal 40 kΩ pull-down loses to the 4.7–10 kΩ part); `pu=0` means the line is held low by a device or a solder bridge. The UI bus should show `pd=1` on both lines once +3V3 is up; the PD bus shows `pd=0` legitimately with no USB-C attached.
- [ ] **Address scan** in the boot log: `boot: … scan: ACK at 0x3c` etc. It lists every device that answers on each bus, so an OLED strapped to `0x3d` or a dead bus is obvious. A dead bus aborts the scan with a single `aborted …` line instead of stalling.
- [ ] Scan I2C2: TPS26750 @ 0x21, input INA228 @ 0x40, output INA228 @ **0x44** (U12 straps: pin 1 = `A1` → +3V3, pin 2 = `A0` → GND; the INA228 address table maps that to 0x44 — see the note on `board::INA228_OUT_ADDR`. If U12 answers at 0x41 instead, a pad got crossed — fix the constant, then re-check everything address-dependent)
- [ ] Scan I2C4: SSD1306 @ 0x3C, CAT24C512 @ 0x50 (bridge JP4/JP5 pads 2–3 for MCU EEPROM access)
- [ ] **Board-detect line** in the boot log: `boot: board detect: INA228 U9@0x40=true U12@0x44=true | OLED@0x3c ack=… | UI bus pull-ups=true -> rev3 power path ACTIVE`. Any of the three signatures opens the gate (INA on I2C2, the OLED on I2C4, or — XT90-only, where the PD bus is unpowered — the UI bus simply being pulled up on *both* lines). On a rev2 board the same line must read `PARKED`, PB1/PB2/PB3/PB6 must never leave their reset state, and an output enable must be refused with the `REFUSED` log line
- [ ] **XT90 cold boot** (no USB-C attached): power up from the XT90 with no MCU reset and confirm the OLED comes up by itself. The expected log is `OLED@0x3c ack=true`; `ack=false` with `UI bus pull-ups=true` is still a recognised board and the UI task retries — if the display stays dark, look for `boot: OLED answered on attempt n/5` (a slow panel, harmless) or `Display frame failed; re-initialising the panel` (a dying panel/bus)
- [ ] OLED shows live Vin/Vout/I/P; the output column header reads `INA` (not `ADC`)
- [ ] TPS26750 `MODE` read returns `APP ` or similar
- [ ] Both INA228 identity probes appear in the boot log: `mfg=0x5449, dev=0x228`, with `shunt=2 mOhm, shunt_cal=1000`
- [ ] Input INA228 (U9) Vin/Iin agree with a DMM + known load within ~2 %; current sign positive into the converter; no clipping near 20 A. **The shunt is 2 mΩ on rev3, not rev2's 8 mΩ** — a 4× reading error here means the wrong `INA228_IN_SHUNT_MOHM`
- [ ] Output INA228 (U12) bus voltage agrees with the PA0 ADC within a few percent, and its current matches a known load. This is the **load** current (R96 sits in the output return), so it reads zero with the load switch open even while the converter regulates
- [ ] `INA!` appears on the main screen only when an INA228 is unplugged/NACKing
- [ ] Power-cycle with the cable **already attached**: the `PdManager` watchdog should probe until the TPS26750 answers (`TPS26750 present`) and reload caps even without a plug interrupt
- [ ] Boot with the TPS26750 held off: boot completes, `TPS26750 lost` appears after two failed probes, and it recovers (`TPS26750 present`) once the controller answers — no reset needed
- [ ] I2C failures are logged as `I2C 2 (PD) error #n: <kind>` / `I2C 4 (UI) error #n: <kind>`. A one-off `nack` on an absent device is normal; a *stream* of `timeout`/`bus` means the bus is wedged and needs the (not-yet-implemented) peripheral-recovery path

## 5. UI (converter disabled)

- [ ] Buttons navigate menu (Btn3); Btn1/Btn2 adjust setpoints
- [ ] BTN3 walks the cycle `Main → V-SET → I-LIM → PD → CFG → Main`. On `V-SET` the tag reads `>V-SET`, the header shows `Fine`/`Coarse`, and a turn moves `SET` by 100 mV / 1 V; the encoder click flips Fine/Coarse, BTN2 toggles the output, BTN3 moves on to `I-LIM` and restores `Fine`
- [ ] The encoder click on `Main` reaches the same field as VSET mode (`>V-SET` tag + `SET` readout, identical layout), and leaving `Main` with BTN3 clears it — the two routes are never active together
- [ ] Encoder fine-adjusts; encoder push toggles enable
- [ ] **Encoder push level** (`enc_mv`): the boot line prints the idle level (~3300 mV released) and every real movement of the line prints `input: encoder PB0 level A -> B mV (pressed=…)`, while `input: encoder click` confirms the gesture reached the UI. Read the three cases off the log:
      * a `level` line with B **below** `board::ENC_BTN_PRESSED_MV` (2750 mV) → the switch works; if the UI still does not react, the failure is downstream (`apply_input`);
      * a `level` line whose B stays **above** the threshold → resistive press path (rev3 measured ~2400 mV: with the board off, `SW1` `S1`–`S2` should read <1 Ω; reflow the encoder's switch pads), or tune the two constants from the logged values — do not go back to a GPIO, `V_IL` is ~1 V;
      * **no `level` line at all while pressing** → the pin is not moving. That is an open contact/joint (the encoder's switch pins or their ground return), never a threshold problem — the line sits at the 3.3 V rail through `R64`, so a working switch must move it by volts
- [ ] **`BUTT_1` (PB9)**: the boot log warns `BUTT_1 (PB9) reads low with the button untouched` on a pin that cannot reach a logic high, and the button is then dead (it used to fire once at boot and never again). Confirm with a DMM: with the firmware running the pin should idle at ~3.3 V; ~0.8 V means ≈13 kΩ to ground on the pad (flux, a bridge to pin 46/48, or a damaged ESD structure). Repair the pad, or use the rework in `board::BTN1_ANALOG` — wire the MCU side of `R66` to PA2 (`ADC1_IN3`) and set that constant to `true`; the boot log then prints `BUTT_1 on PA2 (ADC1_IN3, reworked) at … mV` with the thresholds, and every real movement prints `input: btn1 PA2 level …`. A button that is already low at boot is now ignored until it has been seen released once, so a stuck pin can no longer flip the supply mode at power-up
- [ ] PB1 disable: converter stays off when `enabled=false` (header badge reads `OFF`)
- [ ] Status LED (PA15/D31): very dim double-pulse **heartbeat** while healthy, and no flicker/stuck-on while the I2C buses are busy (the PWM is hardware, so a bus timeout must not change the brightness)
- [ ] Force an error and confirm the matching blink code from the table in [README.md](README.md#status-led-pa15) — e.g. hold the TPS26750 off past the boot grace and count 8 flashes (PD CTRL LOST); the code clears when the condition does

## 6. Open-loop converter (low power)

- [ ] Confirm PB1 polarity vs LT8390 `EN/UVLO` (active high = disabled)
- [ ] Input: current-limited 12 V (or USB PD 5 V contract only)
- [ ] **Power-up sequence, no load.** Put a scope on PB1, PB3, PB2, `Vsense_REG_OUT` and `PWR_G`, and enable with a low Vset (from a discharged output). Expect **PB1 low → 20 ms → PB3 high → output ramps → PB2 high**. The header badge should walk `PRE` → `RMP` → `SWO`. (The 20 ms stretches to however long the stale-charge wait needs if the output was recently at a higher voltage — that is the `PRE` hold, not a hang.)
- [ ] **With the default `OUTPUT_SWITCH_REQUIRE_LOAD = false`, a no-load enable must reach `Running`** — the close is then confirmed by the regulated output plus a live U12, and the `switch closed and verified … peak iout 0 mA` line records that no current was seen. Flip that constant to `true` only when a real load is attached and closure must be proven: the check *is* a load test (`Q5` sits in the return path), so with nothing attached it must fault with `SWITCH FAIL` (LED code 12) no matter how well the switch closes. Either way the line that follows a failure (`output: switch context: PB2 PWR_G reads 1 …; iout ADC=… U12=…; vin=… iin=… pin=…`) separates the two causes: `iin`/`pin` near zero *with a load attached* means the `Q12`/`Q7`/`R58`/`Q5` gate chain is open, while a no-load run shows the same numbers by design.
- [ ] **Then set `board::OUTPUT_SWITCH_REQUIRE_LOAD = false` and repeat**: the same enable must reach `Running` and the badge must read `ON`. This is the no-load bring-up path.
- [ ] **Switch-verify window, USB-C.** With a USB-C source (so U12 is live) and a load that draws > `OUTPUT_SWITCH_MIN_MA`, enable and confirm the log reaches `output: switch closed and verified after … ms (N check(s), peak iout … mA)` — N = 1 on a healthy close, still inside `OUTPUT_SWITCH_SETTLE_MS + OUTPUT_SWITCH_VERIFY_MS` (250 ms + 2 s). A `switch not confirmed yet - <cause>` line followed by a pass is the retry working, not a fault; `SWITCH FAIL` must only appear in the final line, after the full window.
- [ ] **Power-down order**, same scope (the sequence is deliberately slow, ~2 s total: `OUTPUT_DISCHARGE_MS` = 1 s, then `OUTPUT_CONV_OFF_MS` = 1 s): disabling pulls **PB2 low first**, then PB3 low after 1 s, then PB1 high after another 1 s. **Do not expect the output to fall when PB3 drops**: `LOADEN` low turns `TG2` off, which *isolates* the output node — `BG1`/`BG2` clamp the inductor side, not the output caps. The node instead bleeds through its dividers at roughly 1 V/s (watch `Vsense_REG_OUT` on the DMM: tens of seconds from 30 V). That is expected, and the next item proves the firmware knows it.
- [ ] **Stale-charge re-enable (the important one).** Run at 30 V, disable, and re-enable with Vset turned down to 5 V within a few seconds. The badge must hold in `PRE` with a `holding LOADEN low - output node at … mV is above 105% of vset` log, `PWR_G` must stay low, and the sequence must only proceed (log: `node bled down to …`) once the node reaches ~5.25 V. **`PWR_G` must never close while the node sits above the setpoint** — that would dump the stale cap voltage into the load.
- [ ] **OUT TOO HIGH fault (LED 13):** hold the output node up externally (bench supply through a diode, above the setpoint) and enable — after `OUTPUT_BLEED_TIMEOUT_MS` (30 s) the sequencer must latch `Fault::OutputAboveSetpoint` and park without ever raising `LOADEN`. Clear it by raising Vset above the external voltage.
- [ ] **Abort mid-`Precharge`** (enable, then immediately disable): PB3 must **never** pulse high. This is the `hold_loaden` guard.
- [ ] **Ramp timeout**: with the input starved (or a scratch build with a short `OUTPUT_RAMP_TIMEOUT_MS`), confirm `OUTPUT FAIL` (LED 11) latches when `Vsense_REG_OUT` never reaches 90 % of the setpoint. The budget is **2 s of real ramp, timed from when the CV code finishes slewing** — the log prints `output: CV code settled after … ms - ramp budget starts now` at that instant, so a slow DAC walk is never mistaken for a slow converter.
- [ ] **Start-up CC floor**: enable with Iset below the range's cold-start value (e.g. 1 A at ×1) and confirm the boot/log line `start-up CC floored at the 4500 mA cold-start value`. Without it the part never leaves `UP/PRE`.
- [ ] Verify CV mode tracks setpoint within spec (open-loop — if there is a residual offset/gain error, check the R19/R20/R36 values and `board::CV_FB_*` against the assembled board)
- [ ] Verify CC mode limits current
- [ ] CFG (BTN3 from PD): fullscreen list scrolls; EncBtn/BTN1 activates, BTN3 backs out
- [ ] CFG → Output V sweep: returns to Main asking `ENC TO START`; confirm runs 32 points 10→56 V at ~2 s/point and parks the output at `DONE`; any button aborts and parks
- [ ] During a sweep the bottom line replaces `MAIN` and `NO PD`, and the Auto-tracking rail does not renegotiate
- [ ] Sweep efficiency diagnostics: with `DEFMT_LOG=info cargo run --release`, each point prints one `sweep point n/32: … eta=…% n=… cv_code=… u12=…` line (the trailing pair is the CV-calibration data, see §8) and the run ends with a `sweep summary` naming the max and min efficiency points; sanity-check `eta` against a reference measurement at one point
- [ ] Sweep with the INA228 unplugged: every point reports `eta=--`, the summary warns `no valid efficiency points`, and no fault/panic occurs
- [ ] On the main screen (no sweep) the `Eff` readout still matches the efficiency the sweep logs for the same telemetry

## 7. CC sense range (`High-Gain`) and the CC map

- [ ] Boot log prints **both** ranges: `cc range LowGain: R_eff=2 mOhm, ISMON 20 mV/A, settable 2600..20000 mA …` and `cc range HighGain: R_eff=20 mOhm, ISMON 200 mV/A, settable 350..5000 mA …`
- [ ] Disabled with Iset ≤ 5 A: PB6 is **high** and the I-LIM screen badge reads `x10`
- [ ] Disabled with Iset > 5 A: PB6 is **low** and the badge reads `x1`
- [ ] **Deferred range change:** with the output running at 10 A (×1), turn the I-LIM knob down to 2 A. PB6 must **not** move while running; the badge reads `x1*` and the log says `HIGH-GAIN range change deferred`. The output must keep regulating and the CC limit must clamp to the ×1 settable floor (2.6 A), not to 2 A.
- [ ] Disable and re-enable: PB6 now moves high during `Off`, the badge reads `x10`, and the 2 A limit is honoured
- [ ] **Re-measure the loop offsets.** `CC_CURRENT_OFFSET_MA_1X` / `_10X` are both **0** and must be fitted per range: set two low setpoints (e.g. 0.6 A and 1.0 A at ×10), read the drawn current from the output INA228, and set the constant to the intercept `actual − setpoint`. Repeat at ×1. Then re-check a mid-range point.
- [ ] `ISENSE_ZERO_MV`: enable with no load in **each** range and confirm the `isense:` line's `zero` field still matches. The chip's 0.25 V ISMON offset is range-independent, but the mux's common-mode step can shift it slightly.
- [ ] Cross-check the two current paths at a few loads: `filt_i` (ADC/ISMON, range-scaled) should track `iout_ina` (R96 shunt) within the ISMON spec

> If the NTCs are unpopulated or the analogue front-end is not trustworthy yet,
> set `board::NTC_SENSE_ENABLE = false` to get past the fail-safe `OVERTEMP`
> latch (a failed/open NTC reads as invalid and latches by design). Never leave
> it false with the thermal hardware fitted.

## 8. CV accuracy: map calibration + metrology servo

Implements phases 1+2 of [`analysis/CV_CALIBRATION_PLAN.md`](analysis/CV_CALIBRATION_PLAN.md).
Do this after §6/§7 pass — the sweep below needs a working output stage.

### Phase 1a — U12 ↔ DMM cross-check (once per board, needs a trusted DMM)

- [ ] Boot log shows the cal banner: `boot: cv cal valid=false ... u12 vbus scale 10000/10000; servo enabled=true ...`
- [ ] No load, enable, and set ~5 V / ~20 V / ~48 V; at each, write down the DMM reading and the `supply[meas]` log's `ina_out_vout` (the U12 value)
- [ ] `k = DMM / U12` — expect |k − 1| ≤ 0.0005 (the INA228's VBUS gain error is ±0.05 % max). Enter it as `INA228_OUT_VBUS_SCALE_NUM/DEN` (e.g. k = 1.0003 → `10003/10000`), rebuild, and confirm the log now reads back the DMM

### Phase 1b — the map fit

- [ ] Run `CFG → OUTPUT V SWEEP` (no load, `OUTPUT_SWITCH_REQUIRE_LOAD = false`, RTT captured). Each point now ends with `cv_code=NNN u12=NNNN` (the servo is frozen during sweeps, so these are raw static-map points)
- [ ] `python3 analysis/cv_cal/fit.py <rtt-log>` → paste the printed `CV_CAL_A_MV` / `CV_CAL_B_UV_PER_LSB` into `board.rs`, set `CV_CAL_VALID = true`, rebuild + flash
- [ ] Boot log now reads `[map: calibrated (A/B fit)]`; the residual stats from the fit (max |r|) are the expected pre-servo error bound
- [ ] Set 20.000 V: DMM within the fit's max-|r| of 20.000 V; step 5/12/30/48 V and record residuals (point-to-point spread = DAC INL)

### Phase 2 — the servo

- [ ] Set 20.000 V and watch RTT: `cv servo: err=... -> trim=...` lines step at ≤ one per 500 ms, then `cv servo: settled, err=... mV`; `supply[ctl]` carries `trim=N`
- [ ] DMM agrees with U12 to within the deadband (±15 mV) — the servo regulates the *load-terminal* voltage, so fuse/bead drop at current is absorbed too
- [ ] **CC freeze:** with a resistive load pushed into the hardware CC limit, the trim must not move while `cc_active` (VOUT below setpoint is correct there); it resumes in CV
- [ ] **Setpoint change:** turn VSET while running — `trim reset to the map` line, then re-settle; scope shows only the normal CV slew ramp
- [ ] **Temperature:** heat gun over the R19/R20/R36 area at 20.000 V — the DMM must stay put while the trim creeps (without the servo this drifts mV/°C)
- [ ] **U12 loss:** unplug/disable U12 mid-run — trim freezes on the last code, LED code 7, the static map keeps holding
- [ ] **Sweep:** start a sweep with the servo running — no trim motion for the whole run (frozen), so the sweep measures the static map

## 9. Closed-loop & limits

- [ ] Trip test: software OC/OV/OP at bench-safe levels
- [ ] NTC heat gun test: derate/shutdown thresholds

## 10. USB-C PD

- [ ] Negotiate 5 V fixed first
- [ ] PPS 9–20 V steps
- [ ] AVS/EPR only with appropriate cable/source
- [ ] Verify `input_power_cap_mw` limits output before 240 W attempt

## 11. Auto-tracking PD

- [ ] PD screen: BTN1 toggles Auto; footer shows `AUTO <rail> <region> EFF`
- [ ] Sweep Vset and compare the negotiated rail (`get_active_contract`) against the table in `src/pd/auto_track.rs`
- [ ] With the output enabled, a rail change parks the output, renegotiates, then re-enables
- [ ] BTN2 switches to the PWR policy (highest-power rail)
- [ ] Efficiency sweep: measure η of the chosen clean rail vs the nearest rail at the same load; tune `AUTO_TRACK_BUCK_MIN_RATIO_PCT` / `AUTO_TRACK_BOOST_MAX_RATIO_PCT`

## 12. Full-power (last)

- [ ] Thermal imaging under sustained load
- [ ] Verify 240 W cap with simultaneous V/I limits
- [ ] Long soak with enclosure closed

## 13. USB-DFU reflash (BOOT0 strap)

Full detail in [README.md](README.md#reflashing-over-usb-c-rev3) and
[`../PCB/ECO-rev3-usb-dfu.md`](../PCB/ECO-rev3-usb-dfu.md).

- [ ] `FLASH_OPTR` reads back `0xFFEFF8AA` (`nSWBOOT0 = 1`, `nBOOT1 = 1`). ST factory value is `0xFFEFF8AA`, so a fresh rev3 chip should already be correct — verify, and if not program it once via SWD/STM32CubeProgrammer
- [ ] `SW2` released, power cycle → application runs (OLED up), PB8 measures ~0 V
- [ ] `SW2` held + `SW3` tapped → `lsusb` shows `0483:df11`; `dfu-util -l` lists the device (the XT90/PD input must be connected: JP13 is bridged VPP→buck, so USB alone does not power the logic rail)
- [ ] Full write `dfu-util -a 0 -s 0x08000000:leave -D firmware.bin` completes and the `:leave` command starts the application
- [ ] **Converter state during DFU** — with the input attached, scope `Vsense_REG_OUT` across a BOOT0+RST entry: the converter-side node rises to **≈36.7 V** (the DAC-float FB target, `1 V × (1 + R19/R20)`) current-limited to ≈4.9 A, while the **load side stays dead** because `R21` holds `PWR_G` low. Verify with a load attached that the load terminals see nothing (measure at the connector: `Bus_V` is behind the open `Q5`, so it stays discharged). **Flash with the load disconnected anyway**, and fit the Q13-gate pull-up ECO below to kill the 36.7 V state entirely.
- [ ] SWD recovery still works with a deliberately corrupt application

## Known hardware notes (rev3)

> **Output-stage debugging in progress.** Two open hardware faults on the
> output-current sense chain (an under-load power clamp traced to the
> `LSP/LSN` path, and a ~15–18× low current reading traced to the U14 INA241)
> are written up with every test, its expected value and what it ruled out in
> [`analysis/REV3_OUTPUT_DEBUG.md`](analysis/REV3_OUTPUT_DEBUG.md). Read that
> before re-testing anything in §6/§7.

- **Two INA228s.** U9 (`0x40`, R60 = 2 mΩ) is the **input** monitor, between the PD
  side and the converter input. U12 (**`0x44`**, R96 = 2 mΩ — straps `A1 = +3V3`,
  `A0 = GND`; not binary-weighted, see `board::INA228_OUT_ADDR`) is the
  **output** monitor, on the load-return shunt on the connector side of the
  output switch Q5. Their `ALERT` pins are both unconnected, so protection is
  poll-based.
- **The output shunt is low-side and shared.** R96 is what U12 measures, what the
  LT8390A's `ISP/ISN` pair measures, and what the U14 INA241A1 ×10 front end
  measures — through the U7 TMUX1574. All three therefore agree on the same
  current, which is why the sequencer can trust U12 as the switch-closed proof.
- **CAT24C512** is reachable from the MCU on I2C4 only when JP4/JP5 (pads 2–3)
  are bridged; otherwise it sits on the TPS26750's own I2CC port.
- **`SWITCH_EN_READ` (PB14) is now a stub.** U7 (which used to read it) is the
  TMUX1574 range mux on rev3, so the net goes nowhere. Nothing in the firmware
  references it.
- **`+3V3_LDO` is the TPS26750's internal LDO** (U4 pad 1) and it powers the
  MCU-side PD-bus pull-ups (`R31`/`R32`), the TPS's own EEPROM-bus pull-ups
  (`R34`/`R35`), the `+6V6_LDO` charge pump (U5) and the USB SBU switch. It
  comes up **only with USB-C VBUS present**, so the PD bus is dark on an
  XT90-only bench. The main `+3V3` (logic, OLED, both INA228 supplies, the
  UI-bus pull-ups) is a separate LMR16006 buck from VPP via JP13 and needs no
  USB-C — which is why the MCU runs with the PD bus completely dead.
- **LOADEN low isolates, it does not discharge.** The LT8390A turns `TG1`/`TG2`
  off and `BG1`/`BG2` on when `LOADEN` drops — the output caps are *behind* the
  open `TG2` and only bleed through the dividers (~1 V/s). The sequencer knows
  this (`Stage::Precharge` waits out stale charge, `OUTPUT_BLEED_TIMEOUT_MS`),
  and any bench step expecting "output grounded by bottom gates" needs revision.
- **Converter-disable fail-safe (PB1/Q13):** `PB1 → R78 (10 Ω) → Q13 gate`, and
  Q13's drain pulls the LT8390A `EN/UVLO` node low. There is **no pull resistor
  on Q13's gate** (verified in the rev3 netlist: the gate net carries only `R78`
  and `Q13`), so while the MCU is in reset — including while it sits in the ROM
  loader during flashing — the gate floats and the `R7` = 357 k / `R8` = 64.9 k
  divider leaves the converter **enabled**. With both DAC pins floating it then
  regulates the converter-side node to ≈36.7 V at ≈4.9 A (`R5`'s fail-safe).
  **`PWR_G` (PB2) does have `R21` 100 kΩ to GND**, so the *load* is safely
  disconnected in that state — which removes the worst consequence, but not the
  live 36.7 V on the connector-side + terminal (through `F2`).
  Recommended ECO (rev4, or a 0402 bodge on rev3): **100 kΩ from Q13's gate to
  +3V3** — a floating pin then holds the converter *disabled* — plus a pulldown
  on `CNV-EN-LOADEN` (PB3) so a floating MCU parks the bridge instead of
  leaving `LOADEN` undefined. `High-Gain` (PB6) needs **no** external pull: the
  TMUX1574's `SEL` has an internal 6 MΩ pull-down, so it floats to the safe
  ×1 range on its own. Until that ECO is fitted, flash with the load
  disconnected.
- **JP13 selects the logic-buck input** and is bridged VPP→buck as shipped, so
  USB-C alone cannot power the MCU. A `2–3` re-bridge (+VBUS) would make
  USB-only DFU sessions possible — with the input removed, which also removes
  the floating-converter hazard entirely (at ≤8 V input the `R7`/`R8` divider
  sits below the 1.22 V enable threshold, so the chip is off).

