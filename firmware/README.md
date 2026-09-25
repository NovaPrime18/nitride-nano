# nitride-nano firmware

Embassy-based Rust firmware for the **STM32G474CEU6** on the nitride-nano pocket buck/boost supply.

## Features

- **CV/CC control** via 12-bit DAC (PA4 CV, PA6 CC) referenced to VREF+ = +3V3 (the on-board tie; the internal VREFBUF is left high-Z); open-loop CV driven as an (inverted) map from setpoint to DAC code, derived analytically from the LT8390A feedback divider (R19/R20/R36), with code-rate slew limiting (no feedback loop)
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

## Service mode — reflash over UART (no SWD, no BOOT0 strap)

The running firmware can hand control to the ST ROM bootloader, so the board can
be reflashed over the on-board FT234XD without a probe and **without touching
BOOT0 or option bytes**. (PB8 doubles as BOOT0 on this package and is pulled to a
switched rail, so the pin route is unreliable — see [`src/hal/bootloader.rs`](src/hal/bootloader.rs).)

Two triggers, both ending in the same handoff — park the output, reset, ROM loader:

1. **UART (zero-touch):** open the programmer on the FT234XD port. Both tools
   send `0x7F` on connect, which the firmware watches for on USART3, so merely
   connecting hands the device over (allow one retry while it resets).
2. **BTN1 held through power-up/reset** — deterministic, and needs no UART.

```bash
# STM32CubeProgrammer
STM32_Programmer_CLI -c port=/dev/ttyUSB0 br=115200 -w nitride.bin 0x08000000 -v

# or stm32flash (set -b to match SERVICE_UART_BAUD)
sudo stm32flash -b 115200 -w /tmp/nitride.bin -v -g 0x08000000 /dev/ttyUSB0
```

The listener baud must match the tool's connect baud — `board::SERVICE_UART_BAUD`,
default 115200 — because only the *first* byte is matched here; the ROM loader
auto-bauds after the handoff.

> **Safety:** the ROM bootloader runs with the converter unregulated, and a
> firmware pin cannot hold its state through reset. See the PA11/Q13 fail-safe
> ECO in [BENCH.md](BENCH.md) before flashing with a load attached.

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
| UART debug / service mode | PC10/PC11 | USART3 → FT234XD. Also the ROM-bootloader reflash port ("Service mode" above) |

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

## CFG menu & output V sweep

`CFG` (reached with BTN3 from the `PD` screen) is a fullscreen, scrollable list:
`EEPROM WRITE`, `OUTPUT V SWEEP`, `PD CONTRACT`. Turn the encoder to move the
highlight, press the encoder (or BTN1) to activate, BTN3 to go back. The list
scrolls with a right-edge scrollbar once more entries are added.

`OUTPUT V SWEEP` returns to the main screen and waits for an encoder-button
press, then sweeps the output setpoint through 32 points from 10 V to 56 V at
2 s per point (≈64 s) in CV mode. The bottom line shows `>SWEEP` plus the
confirm prompt, the point/percent progress, or `DONE`, replacing the usual
`MAIN` tag and the `NO PD`/`Iin` field for the whole run. Any button press
aborts and parks the output, and the sweep also parks itself on completion or a
latched fault. Auto-tracking PD holds its rail for the duration of the sweep.
Range, point count and dwell live in [`src/board.rs`](src/board.rs)
(`SWEEP_*`); the map reaches all 32 points, including the final 56 V one.

## Bring-up

See [BENCH.md](BENCH.md) for the step-by-step validation checklist.

## Tuning

Edit scaling constants and NTC parameters in [`src/board.rs`](src/board.rs) after measuring divider networks on the assembled board. The INA228 is configured for an 8 mΩ shunt (R60), `ADCRANGE = 0` and 20 A full scale (`INA228_*`).
