# nitride-nano firmware — bench bring-up checklist

Complete these steps **before** enabling full GaN power. Keep a current-limited bench supply on the input.

## 1. Tooling

- [ ] `rustup target add thumbv7em-none-eabihf`
- [ ] probe-rs detects STM32G474CEUx on SWD
- [ ] `cargo build --release` succeeds
- [ ] `defmt-rtt` logs visible (`DEFMT_LOG=info cargo run --release`)

## 2. Clocks & DAC (no power stage)

- [ ] Flash firmware; confirm boot log
- [ ] Measure VREFBUF (~2.5 V on VREF+ pin)
- [ ] Sweep PA4 DAC code 0→4095; log PA0 vs DMM on feedback sense node
- [ ] Sweep PA6 DAC; verify monotonic CC node voltage
- [ ] Record inverted CV cal table (code 0 → max V, full-scale → 0 V) → update `control/dac_cv.rs` `CAL` array

## 3. ADC scaling

- [ ] Apply known voltages to sense nets (safe low levels)
- [ ] Fit `VOUT_SENSE_NUM`, `VBUS_SENSE_NUM`, `ISENSE_MV_PER_A` in `board.rs`
- [ ] Verify NTC readings at room temp; adjust `NTC_BETA` / `NTC_R25_OHM`

## 4. I2C bus

- [ ] Scan: SSD1306 @ 0x3C, TPS26750 @ 0x21, EEPROM @ 0x50
- [ ] OLED shows live Vin/Vout/I/P
- [ ] TPS26750 `MODE` read returns `APP ` or similar

## 5. UI (converter disabled)

- [ ] Buttons navigate menu (Btn3); Btn1/Btn2 adjust setpoints
- [ ] Encoder fine-adjusts; encoder push toggles enable
- [ ] PA11 disable: converter stays off when `enabled=false`

## 6. Open-loop converter (low power)

- [ ] Confirm PA11 polarity vs LT8390 RUN/SHDN
- [ ] Input: current-limited 12 V (or USB PD 5 V contract only)
- [ ] Enable at **low** Vset (e.g. 5 V) and Iset (e.g. 0.5 A)
- [ ] Verify CV mode tracks setpoint within spec (open-loop — re-sweep DAC to calibrate `CAL` if offset/gain is off)
- [ ] Verify CC mode limits current

## 7. Closed-loop & limits

- [ ] Trip test: software OC/OV/OP at bench-safe levels
- [ ] NTC heat gun test: derate/shutdown thresholds

## 8. USB-C PD

- [ ] Negotiate 5 V fixed first
- [ ] PPS 9–20 V steps
- [ ] AVS/EPR only with appropriate cable/source
- [ ] Verify `input_power_cap_mw` limits output before 240 W attempt

## 9. Full-power (last)

- [ ] Thermal imaging under sustained load
- [ ] Verify 240 W cap with simultaneous V/I limits
- [ ] Long soak with enclosure closed

## Known hardware notes

- **INA228** is on TPS `I2C0`, not MCU I2C — firmware uses ADC only until PCB routes INA228 to PA8/PB5.
- **CAT24C512** driver not required for v1; cal table is compile-time (EEPROM optional later).
