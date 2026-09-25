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
- [ ] Fit `VOUT_SENSE_NUM`, `VBUS_SENSE_NUM`, `ISENSE_MV_PER_A` in `board.rs`
- [ ] ISMON zero: enable the output with **no load**, set `board::ISENSE_ZERO_MV`
      from the `isense:` log's `raw (XXX mV)` field (do **not** calibrate with the
      converter parked — ISMON is unpowered then; see `analysis/ismon-calibration/`)
- [ ] Verify NTC readings at room temp; adjust `NTC_BETA` / `NTC_R25_OHM`

## 4. I2C buses

- [ ] Fit the rev2 SCL bodge: wire **PB8 → the I2C0 SCL net** (TPS26750 pin 9 + INA228 SCL). SDA stays on PB7; PC4 may be left hi-Z.
- [ ] Scan I2C1: TPS26750 @ 0x21, INA228 @ 0x40
- [ ] Scan I2C3: SSD1306 @ 0x3C, CAT24C512 @ 0x50 (bridge JP8/JP9 for MCU EEPROM access)
- [ ] OLED shows live Vin/Vout/I/P
- [ ] TPS26750 `MODE` read returns `APP ` or similar
- [ ] INA228 identity probe in the boot log: `mfg=0x5449, dev=0x228`
- [ ] INA228 Vin/Iin agree with a DMM + known load within ~2 %; current sign positive into the converter; no clipping near 20 A
- [ ] `INA!` appears on the main screen only when the INA228 is unplugged/NACKing
- [ ] Power-cycle with the cable **already attached**: the `PdManager` watchdog should probe until the TPS26750 answers (`TPS26750 present`) and reload caps even without a plug interrupt
- [ ] Boot with the TPS26750 held off: boot completes, `TPS26750 lost` appears after two failed probes, and it recovers (`TPS26750 present`) once the controller answers — no reset needed
- [ ] I2C failures are logged as `I2C 1 (PD) error #n: <kind>` / `I2C 3 (UI) error #n: <kind>`. A one-off `nack` on an absent device is normal; a *stream* of `timeout`/`bus` means the bus is wedged and needs the (not-yet-implemented) peripheral-recovery path

## 5. UI (converter disabled)

- [ ] Buttons navigate menu (Btn3); Btn1/Btn2 adjust setpoints
- [ ] Encoder fine-adjusts; encoder push toggles enable
- [ ] PA11 disable: converter stays off when `enabled=false`

## 6. Open-loop converter (low power)

- [ ] Confirm PA11 polarity vs LT8390 RUN/SHDN
- [ ] Input: current-limited 12 V (or USB PD 5 V contract only)
- [ ] Enable at **low** Vset (e.g. 5 V) and Iset (e.g. 0.5 A)
- [ ] Verify CV mode tracks setpoint within spec (open-loop — if there is a residual offset/gain error, check the R19/R20/R36 values and `board::CV_FB_*` against the assembled board)
- [ ] Verify CC mode limits current
- [ ] CFG (BTN3 from PD): fullscreen list scrolls; EncBtn/BTN1 activates, BTN3 backs out
- [ ] CFG → Output V sweep: returns to Main asking `ENC TO START`; confirm runs 32 points 10→56 V at ~2 s/point and parks the output at `DONE`; any button aborts and parks
- [ ] During a sweep the bottom line replaces `MAIN` and `NO PD`, and the Auto-tracking rail does not renegotiate

## 7. Closed-loop & limits

- [ ] Trip test: software OC/OV/OP at bench-safe levels
- [ ] NTC heat gun test: derate/shutdown thresholds

## 8. USB-C PD

- [ ] Negotiate 5 V fixed first
- [ ] PPS 9–20 V steps
- [ ] AVS/EPR only with appropriate cable/source
- [ ] Verify `input_power_cap_mw` limits output before 240 W attempt

## 9. Auto-tracking PD

- [ ] PD screen: BTN1 toggles Auto; footer shows `AUTO <rail> <region> EFF`
- [ ] Sweep Vset and compare the negotiated rail (`get_active_contract`) against the table in `src/pd/auto_track.rs`
- [ ] With the output enabled, a rail change parks the output, renegotiates, then re-enables
- [ ] BTN2 switches to the PWR policy (highest-power rail)
- [ ] Efficiency sweep: measure η of the chosen clean rail vs the nearest rail at the same load; tune `AUTO_TRACK_BUCK_MIN_RATIO_PCT` / `AUTO_TRACK_BOOST_MAX_RATIO_PCT`

## 10. Full-power (last)

- [ ] Thermal imaging under sustained load
- [ ] Verify 240 W cap with simultaneous V/I limits
- [ ] Long soak with enclosure closed

## 11. Service mode (UART reflash, no BOOT0 strap, no SWD)

- [ ] **Before first use:** with the MCU held in reset, measure `/Converter/Conv-Disable`. Pre-ECO it floats (see the note below) — do this step with the load disconnected.
- [ ] Hold BTN1 through power-up: the board parks the output and hands off to the ROM bootloader (attach probe-rs and confirm PC is in `0x1FFFxxxx`).
- [ ] With the app running, `STM32_Programmer_CLI -c port=/dev/ttyUSB0 br=115200` connects; one retry during the handoff is normal.
- [ ] Flash an image and confirm "run after programming" returns to the application.
- [ ] Confirm option bytes, BOOT0 (PB8) and the SWD probe are all untouched by the exchange.
- [ ] With the FT234XD attached, a long soak must not self-trigger service mode.

## Known hardware notes

- **rev2 I2C0 (PC4/PB7)** cannot be driven by any single STM32G474 hardware I²C peripheral: `PC4` is only `I2C2_SCL` and `PB7` is only `I2C1_SDA`/`I2C4_SDA`. Firmware therefore uses **I2C1** with SCL bodged from PC4 to **PB8** (SDA stays on PB7).
- **INA228** (0x40, 8 mΩ shunt R60) is an **input**-bus monitor (PD side → converter input); the MCU ADCs remain the output-side telemetry.
- **CAT24C512** is reachable from the MCU on I2C3 only when JP8/JP9 are bridged; otherwise it sits on the TPS26750's own I2CC port.
- Hardware INA228 ALERT drives the `SWITCH_EN` node directly; firmware input OCP/OVP is an additional backstop.
- **Converter-disable fail-safe (PA11/Q13):** `PA11 → R78 (10 Ω) → Q13 gate`, and Q13's drain pulls the LT8390 `EN/UVLO` node low. There is **no pull resistor on Q13's gate**, so while the MCU is in reset — including while it sits in the ROM bootloader during flashing — the gate floats and the R7 = 357 k / R8 = 64.9 k divider can enable the converter at an uncontrolled setpoint. Recommended ECO: **100 kΩ from Q13's gate to +3V3**, so a floating pin holds the converter *disabled* (enabling then requires PA11 to be actively low). Until that ECO is fitted, do service-mode/UART flashing with the load disconnected: the firmware parks the output before the reset, but a pin cannot hold its state *through* a reset.

