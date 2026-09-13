# nitride-nano firmware

Embassy-based Rust firmware for the **STM32G474CEU6** on the nitride-nano pocket buck/boost supply.

## Features

- **CV/CC control** via 12-bit DAC (PA4 CV, PA6 CC) referenced to VREF+ = +3V3 (the on-board tie; the internal VREFBUF is left high-Z); open-loop CV driven as an (inverted) calibration map from setpoint to DAC code with code-rate slew limiting (no feedback loop)
- **ADC telemetry** on PA0/PA1/PA3/PA7/PA9 (output side)
- **INA228** input power monitor (Vin/Iin/Pin + die temp) on I2C1
- **Auto-tracking PD** — negotiates a fixed input rail from the output setpoint, keeping the LT8390A out of its 4-switch buck-boost region when the requested power allows
- **SSD1306** powersupply UI (128×64 I2C)
- **3 buttons + rotary encoder** (PB9–11, PB6/PA12, PB4)
- **TPS26750** USB-C PD driver (port of `../src/tps26750.cpp`)
- **Protection**: 60 V / 20 A / 240 W / NTC overtemperature / INA228 input OCP-OVP

## Requirements

- Rust stable (`thumbv7em-none-eabihf` target)
- [probe-rs](https://probe.rs/) for flash/debug (SWD on PA13/PA14)

```bash
rustup target add thumbv7em-none-eabihf
```

## Build & flash

```bash
cd firmware
cargo build --release
cargo run --release   # uses probe-rs runner from .cargo/config.toml
```

## Pin map

| Signal | Pin | Notes |
|--------|-----|-------|
| I2C1 SCL/SDA | PB8 / PB7 | TPS26750 + INA228. **rev2 bodge:** PCB routes this bus to PC4/PB7, but no STM32G474 hardware I²C peripheral can drive PC4+PB7 together, so SCL is wired from PC4 to PB8 (I2C1_SCL). SDA stays on PB7 (I2C1_SDA). |
| I2C3 SCL/SDA | PA8 / PB5 | SSD1306 OLED, and CAT24C512 EEPROM when JP8/JP9 are bridged |
| CV DAC | PA4 | DAC1 CH1, buffered |
| CC DAC | PA6 | DAC2 CH1, buffered |
| Vout sense | PA0 | ADC1 (output side) |
| I sense | PA3 | ADC1 |
| Bus V | PA7 | ADC2 (input-bus fallback; INA228 is primary) |
| NTC conv / in | PA1 / PA9 | ADC |
| Disable | PA11 | GPIO (verify polarity at bring-up) |
| Buttons | PB9 / PB10 / PB11 | Active low |
| Encoder A/B | PB6 / PA12 | TIM4 QEI |
| Enc button | PB4 | |
| PD IRQ | PB13 | EXTI |
| UART debug | PC10/PC11 | USART3 → FT234XD |

## Auto-tracking PD

`PdContract` screen: **BTN1** toggles Auto-tracking on/off, **BTN2** switches the
policy (efficiency-first vs maximum power), the encoder steps presets in manual
mode, and the encoder button confirms/re-evaluates.

The LT8390A leaves its 4-switch buck-boost region only when `VIN/VOUT` clears
~1.35 (clean buck) or ~0.70 (clean boost); near `VIN ≈ VOUT` all four FETs
switch and efficiency drops. Auto-tracking therefore picks the fixed USB-PD rail
that keeps the converter in a clean region, and falls back to the most capable
rail only when the requested `v_set × i_set` power exceeds what the clean rail
can deliver. Lower the current limit to get the efficient rail at high `Vset`.
Guard bands live in [`src/board.rs`](src/board.rs) (`AUTO_TRACK_*`) and should be
tuned from a bench efficiency sweep.

## Bring-up

See [BENCH.md](BENCH.md) for the step-by-step validation checklist.

## Tuning

Edit scaling constants and NTC parameters in [`src/board.rs`](src/board.rs) after measuring divider networks on the assembled board. The INA228 is configured for an 8 mΩ shunt (R60), `ADCRANGE = 0` and 20 A full scale (`INA228_*`).
