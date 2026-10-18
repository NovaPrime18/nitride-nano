# nitride-nano firmware — bench bring-up checklist

Complete these steps **before** enabling full GaN power. Keep a current-limited bench supply on the input.

## 1. Tooling

- [ ] `rustup target add thumbv7em-none-eabihf`
- [ ] probe-rs detects STM32G474CEUx on SWD
- [ ] `cargo build --release` succeeds
- [ ] `defmt-rtt` logs visible (`DEFMT_LOG=info cargo run --release`)

## 2. Clocks & DAC (no power stage)

- [ ] Flash firmware; confirm boot log
- [ ] Measure VREF+ (~3.3 V: the board ties it to +3V3 and the firmware leaves VREFBUF high-Z)
- [ ] Sweep PA4 DAC code 0→4095; log PA0 vs DMM on feedback sense node
- [ ] Sweep PA6 DAC; verify monotonic CC node voltage
- [ ] Verify the CV map against the divider network: the code-0 output is `CV_FB_REF_MV·(1 + R19/R36 + R19/R20)` and the slope is `(R19/R36)·DAC_VREF_MV/DAC_MAX_CODE` per code (`control/dac_cv.rs`, `board::CV_FB_*`). Confirm R19/R20/R36 on the assembled board match ~357k/10k/10k; if not, update those constants.

## 3. ADC scaling

- [ ] Apply known voltages to sense nets (safe low levels)
- [ ] Fit `VOUT_SENSE_NUM`, `VBUS_SENSE_NUM` in `board.rs` (rev3: 248 k/10 k = 85.14 V and 200 k/10 k = 69.6 V, from R85+R86/R87 and R44+R47/R97). The ISMON gain is **not** a constant any more — it comes from `CcRange::mv_per_a()` (20 mV/A at ×1, 200 mV/A at ×10)
- [ ] ISMON zero: enable the output with **no load**, set `board::ISENSE_ZERO_MV`
      from the `isense:` log's `raw (XXX mV)` field (do **not** calibrate with the
      converter parked — ISMON is unpowered then; see `analysis/ismon-calibration/`)
- [ ] Verify NTC readings at room temp; adjust `NTC_BETA` / `NTC_R25_OHM`

## 4. I2C buses

- [ ] No bodge needed on rev3: the PD bus is a real I2C2 on **PC4/PA8** and the UI bus a real I2C4 on **PC6/PB7**
- [ ] Scan I2C2: TPS26750 @ 0x21, input INA228 @ 0x40, output INA228 @ 0x41
- [ ] Scan I2C4: SSD1306 @ 0x3C, CAT24C512 @ 0x50 (bridge JP8/JP9 for MCU EEPROM access)
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
- [ ] Encoder fine-adjusts; encoder push toggles enable
- [ ] PB1 disable: converter stays off when `enabled=false` (header badge reads `OFF`)
- [ ] Status LED (PA15/D31): very dim double-pulse **heartbeat** while healthy, and no flicker/stuck-on while the I2C buses are busy (the PWM is hardware, so a bus timeout must not change the brightness)
- [ ] Force an error and confirm the matching blink code from the table in [README.md](README.md#status-led-pa15) — e.g. hold the TPS26750 off past the boot grace and count 8 flashes (PD CTRL LOST); the code clears when the condition does

## 6. Open-loop converter (low power)

- [ ] Confirm PB1 polarity vs LT8390 `EN/UVLO` (active high = disabled)
- [ ] Input: current-limited 12 V (or USB PD 5 V contract only)
- [ ] **Power-up sequence, no load.** Put a scope on PB1, PB3, PB2, `Vsense_REG_OUT` and `PWR_G`, and enable with a low Vset. Expect **PB1 low → 20 ms → PB3 high → output ramps → PB2 high**. The header badge should walk `PRE` → `RMP` → `SWO`.
- [ ] **With `OUTPUT_SWITCH_REQUIRE_LOAD = true` and no load, `SwitchOn` must fault** with `SWITCH FAIL`, LED code 12, and a log naming the constant. This is the expected no-load result, not a bug.
- [ ] **Then set `board::OUTPUT_SWITCH_REQUIRE_LOAD = false` and repeat**: the same enable must reach `Running` and the badge must read `ON`. This is the no-load bring-up path.
- [ ] **Power-down order**, same scope: disabling pulls **PB2 low first**, then PB3 low after 50 ms (output discharged), then PB1 high after another 20 ms. Confirm the output actually falls to ~0 V when PB3 drops — that is BG1/BG2 doing their job.
- [ ] **Abort mid-`Precharge`** (enable, then immediately disable): PB3 must **never** pulse high. This is the `hold_loaden` guard.
- [ ] **Ramp timeout**: with a low `OUTPUT_RAMP_TIMEOUT_MS` on a scratch build, or with the input starved, confirm `OUTPUT FAIL` (LED 11) latches when `Vsense_REG_OUT` never reaches 90 % of the setpoint.
- [ ] **Start-up CC floor**: enable with Iset below the range's cold-start value (e.g. 1 A at ×1) and confirm the boot/log line `start-up CC floored at the 4500 mA cold-start value`. Without it the part never leaves `UP/PRE`.
- [ ] Verify CV mode tracks setpoint within spec (open-loop — if there is a residual offset/gain error, check the R19/R20/R36 values and `board::CV_FB_*` against the assembled board)
- [ ] Verify CC mode limits current
- [ ] CFG (BTN3 from PD): fullscreen list scrolls; EncBtn/BTN1 activates, BTN3 backs out
- [ ] CFG → Output V sweep: returns to Main asking `ENC TO START`; confirm runs 32 points 10→56 V at ~2 s/point and parks the output at `DONE`; any button aborts and parks
- [ ] During a sweep the bottom line replaces `MAIN` and `NO PD`, and the Auto-tracking rail does not renegotiate
- [ ] Sweep efficiency diagnostics: with `DEFMT_LOG=info cargo run --release`, each point prints one `sweep point n/32: … eta=…% n=…` line and the run ends with a `sweep summary` naming the max and min efficiency points; sanity-check `eta` against a reference measurement at one point
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

## 8. Closed-loop & limits

- [ ] Trip test: software OC/OV/OP at bench-safe levels
- [ ] NTC heat gun test: derate/shutdown thresholds

## 9. USB-C PD

- [ ] Negotiate 5 V fixed first
- [ ] PPS 9–20 V steps
- [ ] AVS/EPR only with appropriate cable/source
- [ ] Verify `input_power_cap_mw` limits output before 240 W attempt

## 10. Auto-tracking PD

- [ ] PD screen: BTN1 toggles Auto; footer shows `AUTO <rail> <region> EFF`
- [ ] Sweep Vset and compare the negotiated rail (`get_active_contract`) against the table in `src/pd/auto_track.rs`
- [ ] With the output enabled, a rail change parks the output, renegotiates, then re-enables
- [ ] BTN2 switches to the PWR policy (highest-power rail)
- [ ] Efficiency sweep: measure η of the chosen clean rail vs the nearest rail at the same load; tune `AUTO_TRACK_BUCK_MIN_RATIO_PCT` / `AUTO_TRACK_BOOST_MAX_RATIO_PCT`

## 11. Full-power (last)

- [ ] Thermal imaging under sustained load
- [ ] Verify 240 W cap with simultaneous V/I limits
- [ ] Long soak with enclosure closed

## 12. USB-DFU reflash (BOOT0 strap)

Full detail in [README.md](README.md#reflashing-over-usb-c-rev3) and
[`../PCB/ECO-rev3-usb-dfu.md`](../PCB/ECO-rev3-usb-dfu.md).

- [ ] `FLASH_OPTR` reads back `0xFFEFF8AA` (`nSWBOOT0 = 1`, `nBOOT1 = 1`)
- [ ] `SW2` released, power cycle → application runs (OLED up), PB8 measures ~0 V
- [ ] `SW2` held + `SW3` tapped → `lsusb` shows `0483:df11`; `dfu-util -l` lists the device
- [ ] Full write `dfu-util -a 0 -s 0x08000000:leave -D firmware.bin` completes and the `:leave` command starts the application
- [ ] **Converter state during DFU** — with a load attached, confirm the output stays parked across a BOOT0+RST entry (see the fail-safe note below)
- [ ] SWD recovery still works with a deliberately corrupt application

## Known hardware notes (rev3)

- **Two INA228s.** U9 (`0x40`, R60 = 2 mΩ) is the **input** monitor, between the PD
  side and the converter input. U12 (`0x41`, R96 = 2 mΩ) is the **output** monitor,
  on the load-return shunt on the connector side of the output switch Q5. Their
  `ALERT` pins are both unconnected, so protection is poll-based.
- **The output shunt is low-side and shared.** R96 is what U12 measures, what the
  LT8390A's `ISP/ISN` pair measures, and what the U14 INA241 ×10 front end
  measures — through the U7 mux. All three therefore agree on the same current,
  which is why the sequencer can trust U12 as the switch-closed proof.
- **CAT24C512** is reachable from the MCU on I2C4 only when JP8/JP9 are bridged;
  otherwise it sits on the TPS26750's own I2CC port.
- **`SWITCH_EN_READ` (PB14) is now a stub.** U7 (which used to read it) is the
  TMUX1574 range mux on rev3, so the net goes nowhere. Nothing in the firmware
  references it.
- **Converter-disable fail-safe (PB1/Q13):** `PB1 → R78 (10 Ω) → Q13 gate`, and
  Q13's drain pulls the LT8390A `EN/UVLO` node low. There is **no pull resistor on
  Q13's gate**, so while the MCU is in reset — including while it sits in the ROM
  loader during flashing — the gate floats and the `R7` = 357 k / `R8` = 64.9 k
  divider can enable the converter at an uncontrolled setpoint.
  **`PWR_G` (PB2) does have `R21` 100 kΩ to GND**, so the *load* is safely
  disconnected in that state — which removes the worst consequence, but not the
  converter running unregulated. Recommended ECO: **100 kΩ from Q13's gate to
  +3V3** (so a floating pin holds the converter *disabled*) plus pulldowns on
  `CNV-EN-LOADEN` (PB3) and `High-Gain` (PB6), neither of which has one either.
  Until that ECO is fitted, flash with the load disconnected.

