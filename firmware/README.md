# nitride-nano firmware

Embassy-based Rust firmware for the **STM32G474CEU6** on the nitride-nano pocket buck/boost supply.

## Features

- **CV/CC control** via 12-bit DAC (PA4 CV, PA6 CC) with VREFBUF; open-loop CV driven as an (inverted) calibration map from setpoint to DAC code with code-rate slew limiting (no feedback loop)
- **ADC telemetry** on PA0/PA1/PA3/PA7/PA9 (INA228 not on MCU I2C on current PCB)
- **SSD1306** powersupply UI (128×64 I2C)
- **3 buttons + rotary encoder** (PB9–11, PB6/PA12, PB4)
- **TPS26750** USB-C PD driver (port of `../src/tps26750.cpp`)
- **Protection**: 60 V / 20 A / 240 W / NTC overtemperature

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
| I2C3 SCL/SDA | PA8 / PB5 | TPS26750, SSD1306, CAT24C512 |
| CV DAC | PA4 | DAC1 CH1, buffered |
| CC DAC | PA6 | DAC2 CH1, buffered |
| Vout sense | PA0 | ADC1 |
| I sense | PA3 | ADC1 |
| Bus V | PA7 | ADC2 |
| NTC conv / in | PA1 / PA9 | ADC |
| Disable | PA11 | GPIO (verify polarity at bring-up) |
| Buttons | PB9 / PB10 / PB11 | Active low |
| Encoder A/B | PB6 / PA12 | TIM4 QEI |
| Enc button | PB4 | |
| PD IRQ | PB13 | EXTI |
| UART debug | PC10/PC11 | USART3 → FT234XD |

## Bring-up

See [BENCH.md](BENCH.md) for the step-by-step validation checklist.

## Tuning

Edit scaling constants and NTC parameters in [`src/board.rs`](src/board.rs) after measuring divider networks on the assembled board.
