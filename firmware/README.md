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

## Service mode — UART reflash: **known not to work on this silicon/board** (2026-09)

The running firmware *does* hand control to the ST ROM bootloader — the trigger,
the `.uninit` handoff marker, the `SYSRESETREQ` and the jump to `0x1FFF0000` all
work and were each verified on hardware. **The ROM bootloader then returns to the
application almost immediately instead of staying resident, so no host tool ever
gets to sync.** Treat UART reflash as unavailable until the design changes; use
SWD (`cargo run --release`, or `probe-rs download`) to flash.

### What was verified, and how

- **The FT234XD link works.** A `0x7F` on the port is received on USART3,
  accepted by the listener, and the output is parked. The "FTDI enumerates but
  the MCU ignores the port" symptom was a *different* bug (below).
- **The handoff marker works.** `request_on_next_boot()` stores `0xB00710AD` /
  `0x4FF8EF52` in `.uninit`; it survives `sys_reset()`; `take_request()` accepts
  it; and the jump runs with the correct system-memory vectors
  (`0x20002160` / `0x1FFF5049`), confirmed by a trace in otherwise-unused SRAM.
- **The G4 ROM bootloader is present and healthy**, but does not stay. Setting
  the core straight to the ROM entry from a pristine reset
  (`pc = 0x1FFF5049`, `msp = 0x20002160`, then resume) puts the PC back in the
  application within ~100 ms, with the app's own boot banner on the UART. No
  `0x79` ACK is ever produced, at any baud/parity, even while flooding `0x7F`.
  The boot configuration is `nBOOT0=1`, `nSWBOOT0=0` — i.e. "boot from main
  flash" — which is what the loader falls back to. This matches the widely
  reported STM32G4/G0 behaviour that a software jump to the system loader does
  not keep it resident; it is not a firmware defect and no amount of driver
  teardown or interrupt-state fiddling changes it.

### The bug that *was* real (and is fixed)

`defmt-rtt`'s default blocking write froze the single-threaded executor a few
seconds after boot whenever no `probe-rs` reader was draining RTT: the RTT
control block lives in `.uninit`, its "host connected" flag survives resets, so
after any probe session the buffer fills and `blocking_write` spins forever. The
listener was therefore dead by the time a flasher connected — which is exactly
why triggering only ever worked in the first seconds after a power cycle. Fixed
in `Cargo.toml` with `defmt-rtt = { version = "0.4", features =
["disable-blocking-mode"] }`.

### What would make USB-C reflash actually work

1. **Hardware ECO (rev3) — chosen direction.** `BOOT0` must be strapped high at
   reset. On the G4 BOOT0 *is* PB8, which is also `/MCU/I2C0_SCL` (pulled high by
   R32), so the pin has to be freed from the I²C bus and given a strap that is low
   by default and high only while a host is flashing. The full change — I²C
   re-pin, the FT234XD `~RTS` strap circuit, the `nSWBOOT0` option byte, and the
   firmware simplification — is written up in
   [`../PCB/ECO-rev3-boot0.md`](../PCB/ECO-rev3-boot0.md). With it, stock
   `stm32flash` / `STM32CubeProgrammer` work with no host script.
2. **Zero-hardware alternative, works on rev2 today.** The G4 can also take
   BOOT0 from the `nBOOT0` software option bit, so the *firmware* can select the
   loader and reset — no board change. See §7 of the ECO.
3. **In-app updater (fallback).** If neither of the above is wanted, the USART3
   ↔ FT234XD link already works, so the application can implement its own
   erase/write protocol. Safest shape is A/B across the two flash halves so a
   failed transfer can never brick the board.

> **Safety:** a firmware pin cannot hold its state through reset, so anything
> that resets into the ROM loader leaves the converter unregulated. See the
> PA11/Q13 fail-safe ECO in [BENCH.md](BENCH.md) before flashing with a load
> attached.

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
| Status LED | PA15 | D31, **active high** (PA15 → anode → R81 → GND); `TIM2_CH1` hardware PWM |
| Buttons | PB9 / PB10 / PB11 | Active low |
| Encoder A/B | PB6 / PA12 | TIM4 QEI |
| Enc button | PB4 | |
| PD IRQ | PB13 | EXTI |
| UART debug / service mode | PC10/PC11 | USART3 → FT234XD. Intended as the ROM-bootloader reflash port — see "Service mode" above, it does **not** work on this board. |

## Status LED (PA15)

`D31` is wired **active high** (`PA15 → anode → R81 → GND`) and driven from
`TIM2_CH1` as hardware PWM, so it can be dimmed precisely and keeps its
brightness even while a blocking I2C timeout stalls the executor. A background
task ([`src/ui/led.rs`](src/ui/led.rs)) plays one of these indications:

* **Heartbeat** — everything fitted is present and no fault is latched: two
  3 %-duty pulses ("thump-thump") roughly every 2 s. Very dim by design; tune
  `LED_HEARTBEAT_DUTY_PCT` in [`src/board.rs`](src/board.rs) to taste.
* **Activity** — an EEPROM write/verify is running: a medium-duty pulse at ~2 Hz.

Otherwise the LED repeats a **blink code**: `N` full-brightness flashes, then a
long dark pause.

| `N` | Meaning | Trigger |
|-----|---------|---------|
| 1 | OVERCURRENT | `Fault::OverCurrent` |
| 2 | OVERVOLTAGE | `Fault::OverVoltage` |
| 3 | OVERPOWER | `Fault::OverPower` |
| 4 | OVERTEMP | `Fault::OverTemp` |
| 5 | IN OCP | `Fault::InputOverCurrent` |
| 6 | IN OVP | `Fault::InputOverVoltage` |
| 7 | INA228 FAULT | input monitor not answering |
| 8 | PD CTRL LOST | TPS26750 not answering |
| 9 | PD NO RAIL | source caps known, none can serve the request |
| 10 | EEPROM FAULT | last flash attempt failed |

A latched protection fault always wins over the other states. Codes 7–9 are held
back for `LED_SUBSYSTEM_GRACE_MS` after boot (the two chips are probed
asynchronously) and can be disabled entirely with `LED_SUBSYSTEM_CODES` for a
build that does not fit the INA228 or TPS26750. Every change is also logged on
RTT (`LED: OVERCURRENT (code 1)`) so the blink code has a matching message.

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

### Sweep efficiency diagnostics

Each point also logs a filtered input-to-output efficiency to the defmt console
(`DEFMT_LOG=info cargo run --release`):

```text
sweep point 7/32: vset=19839 vout=19810 iout=2100 vin=20000 pin_avg=44400 pout_avg=41800 eta=94.1% n=60
...
sweep summary (complete): 32 points, 30 valid
  max eta=94.1% at point 7 (19839 mV)
  min eta=88.3% at point 31 (54258 mV)
```

The report is `η = mean(pout) / mean(pin)`: both powers are integrated over the
settled part of each 2 s dwell (`SWEEP_SETTLE_MS` / `SWEEP_SAMPLE_MS` in
[`src/board.rs`](src/board.rs)) and divided once, rather than ratioing a single
noisy sample, so the board's ADC/INA228 jitter does not swamp the result. A point
with too few accepted samples, or a reading above 100 % (measurement error), is
logged with `excluded` and left out of the summary; if the INA228 is absent or
the output is too lightly loaded, every point prints `eta=--` and the summary
warns instead of recording a bogus maximum. Aborting a run early still prints the
summary for the points already measured.

## Bring-up

See [BENCH.md](BENCH.md) for the step-by-step validation checklist.

## Tuning

Edit scaling constants and NTC parameters in [`src/board.rs`](src/board.rs) after measuring divider networks on the assembled board. The INA228 is configured for an 8 mΩ shunt (R60), `ADCRANGE = 0` and 20 A full scale (`INA228_*`).
