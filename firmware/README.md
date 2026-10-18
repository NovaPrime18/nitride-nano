# nitride-nano firmware

Embassy-based Rust firmware for the **STM32G474CEU6** on the nitride-nano pocket buck/boost supply.

## Features

- **CV/CC control** via 12-bit DAC (PA4 CV, PA6 CC) referenced to VREF+ = +3V3 (the on-board tie; the internal VREFBUF is left high-Z); open-loop CV driven as an (inverted) map from setpoint to DAC code, derived analytically from the LT8390A feedback divider (R19/R20/R36), with code-rate slew limiting (no feedback loop)
- **Switchable CC sense range** (`High-Gain`, PB6): ×1 directly across R96, or ×10 through the U14 INA241, giving 2.5–20 A or 0.3–5 A with ten times the resolution. The pin only moves while the output is parked, because a range change is a ×10 step in CC loop gain
- **Sequenced output stage**: `Conv-Disable` (PB1) enables the converter with the output held at 0 V through the LT8390A's bottom gates (`CNV-EN-LOADEN`, PB3); raising that starts conversion, and `PWR_G` (PB2) connects the load only after `Vsense_REG_OUT` reaches the setpoint, verified against the output INA228
- **ADC telemetry** on PA0 (`Vsense_REG_OUT`) / PA1 / PA3 (`ISMON`) / PA7 / PA9
- **Two INA228** power monitors on I2C2: input (U9, `0x40`, R60 = 2 mΩ) and output (U12, `0x41`, R96 = 2 mΩ). The output one is authoritative for the displayed Vout/Iout/Pout and is the only evidence the load switch closed
- **Auto-tracking PD** — negotiates a fixed input rail from the output setpoint, keeping the LT8390A out of its 4-switch buck-boost region when the requested power allows
- **SSD1306** powersupply UI (128×64 I2C)
- **3 buttons + rotary encoder** (PB9–11, PB4/PB5, PB0)
- **TPS26750** USB-C PD driver (port of `../src/tps26750.cpp`)
- **Protection**: 60 V / 20 A / 240 W / NTC overtemperature / input OCP-OVP / output-not-reached / output-switch-failed
- **USB-C reflash** via the STM32G474 ROM USB DFU bootloader (BOOT0 hardware strap, no application code)

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

# raw image for USB-DFU / stm32flash-style tools:
cargo objcopy --release -- -O binary firmware.bin
```

## Reflashing over USB-C (rev3)

rev3 removes the FT234XD and reflashes over the STM32G474's **built-in ROM USB
DFU bootloader** on PA11/PA12. There is **no bootloader-handoff code in the
application**: the ROM loader only stays resident when BOOT0 is high at the
reset sampling instant, which a software jump cannot arrange (this is exactly why
the rev2 UART scheme in [`../PCB/ECO-rev3-boot0.md`](../PCB/ECO-rev3-boot0.md)
never worked). Entry is therefore a hardware strap.

1. Hold **BOOT0** (`SW2`), press and release **NRST** (`SW3`), release BOOT0.
2. The device enumerates as `0483:df11`; `dfu-util -l` lists it.
3. Write and run:

```bash
dfu-util -a 0 -s 0x08000000:leave -D firmware.bin
# or: STM32CubeProgrammer -> USB -> Download
```

One-time per board: the option bytes must take BOOT0 from the PB8 pin —
**`nSWBOOT0 = 1`** (`FLASH_OPTR` bit 26), i.e. `0xFBEFF8AA → 0xFFEFF8AA`, with
`nBOOT1 = 1`. Verify with STM32CubeProgrammer → Option Bytes, or by SWD.

**SWD (`J4`) always works, even with a corrupt application, so a bad flash is
never a brick.**

> **Safety:** a firmware pin cannot hold its state through reset. `PWR_G` has a
> 100 kΩ pulldown, so the *load* is safely disconnected while the ROM loader runs,
> but `PB1`/`PB3` have no pull resistors and the R7/R8 divider can leave the
> converter enabled at an uncontrolled setpoint. See the fail-safe notes in
> [BENCH.md](BENCH.md) before flashing with a load attached.

## Pin map

### rev3

| Signal | Pin | Notes |
|--------|-----|-------|
| PD bus I2C2 SCL/SDA | PC4 / PA8 | TPS26750 (`0x21`) + both INA228s (U9 `0x40`, U12 `0x41`). PC4/PA8 is a valid single-peripheral pin pair, so rev2's SCL bodge is gone |
| UI bus I2C4 SCL/SDA | PC6 / PB7 | SSD1306 OLED (`0x3C`), and CAT24C512 EEPROM (`0x50`) when JP8/JP9 are bridged |
| USB DM/DP | PA11 / PA12 | ROM USB DFU bootloader; the only pins with USB_DM/USB_DP. Short-to-VBUS protected through U3's SBU path |
| BOOT0 | PB8 | `R59` 357 kΩ → GND, `SW2` → +3V3. Nothing in firmware |
| CV DAC | PA4 | DAC1 CH1, buffered |
| CC DAC | PA6 | DAC2 CH1, buffered |
| `Vsense_REG_OUT` | PA0 | ADC1. R85+R86 124 k / R87 10 k = 248 k/10 k (85.14 V full scale) |
| `ISMON` | PA3 | ADC1. Gain is range-dependent: 20 mV/A at ×1, 200 mV/A at ×10 |
| Bus V | PA7 | ADC2 (input-bus fallback; the input INA228 is primary) |
| NTC conv / in | PA1 / PA9 | ADC |
| `Conv-Disable` | PB1 | GPIO → `R78` 10 Ω → `Q13` gate → LT8390A `EN/UVLO`. **Active high = disabled**; `R7` 357 k / `R8` 64.9 k set the fail-safe state |
| `PWR_G` | PB2 | GPIO → `Q12` → `Q7` → `R58` 27 Ω → `Q5` gate. Active high = load switch closed. `R21` 100 kΩ pulldown = load disconnected while the MCU is in reset |
| `CNV-EN-LOADEN` | PB3 | GPIO → LT8390A `LOADEN`. Low holds BG1/BG2 on (output grounded); high releases soft-start |
| `High-Gain` | PB6 | GPIO → U7 TMUX1574 `SEL`. Low = ×1 direct across R96, high = ×10 through the U14 INA241 |
| Status LED | PA15 | D31, **active high** (PA15 → anode → R81 → GND); `TIM2_CH1` hardware PWM |
| Buttons | PB9 / PB10 / PB11 | Active low |
| Encoder A/B | PB4 / PB5 | TIM3 QEI (was TIM4 on PB6/PA12) |
| Enc button | PB0 | |
| PD IRQ | PB13 | EXTI |
| Spare | PA2, PA5, PA10, PB2 used, PC10, PC11 | PC10/PC11 are the freed USART3 pins |
| Removed | PA11 (was disable), PC10/PC11 (was USART3) | PA11 is now `USB_DM`; the FT234XD (U12) is gone |

Net names did not change with the re-pin — only the MCU pins — so the pull-ups
travelled with their buses.


## Output-stage sequence (rev3)

rev3 replaced the single `Conv-Disable` line with three pins, and the order is
safety-critical. `src/control/sequencer.rs` owns the state machine; the header
badge shows the stage (`PRE` / `RMP` / `SWO` / `ON` / `DWN` / `OFF`) instead of a
bare `ON`.

```text
  Off ──enable──▶ Precharge ──20 ms──▶ Ramping ──Vout ok──▶ SwitchOn ──verify──▶ Running
   ▲                  │                  │                    │                     │
   └──────────────────┴──────────────────┴──── Disabling ◀────┴──────disable─────────┘
```

| Stage | PB1 chip | PB3 `LOADEN` | PB2 `PWR_G` | What is happening |
|-------|----------|--------------|-------------|-------------------|
| `Off` | disabled | low | open | All three parked; DACs at minimum output |
| `Precharge` | **enabled** | low | open | LT8390A `BG1`/`BG2` hold the output at 0 V while INTVCC/VREF/CTRL settle |
| `Ramping` | enabled | **high** | open | `LOADON` asserts, soft-start begins, output rises |
| `SwitchOn` | enabled | high | **closed** | Load connected; the output INA228 must confirm it |
| `Running` | enabled | high | closed | Normal CV/CC operation |
| `Disabling` | off after 70 ms | low after 50 ms | **open immediately** | Unwinds in reverse: load off, then discharge, then chip off |

Two things make this work, and both are easy to get wrong:

* **`LOADEN` is only half the condition.** The LT8390A reaches its `UP/PRE` state
  only when CTRL is above its rising latch-off (~0.325 V) **and** `LOADEN` is high.
  A sub-floor current setpoint would therefore command CTRL below latch-off, raise
  `LOADEN`, and start nothing. The sequencer floors the CC command at the range's
  cold-start value (4.5 A at ×1, 450 mA at ×10) for the whole ramp and logs it.
* **Current is the only proof the switch closed.** `Vsense_REG_OUT` is sensed on
  the *converter* side of `Q5`, so a `Q5` that never conducts leaves the output
  looking perfectly regulated. The check is the output INA228's shunt current.
  With no load attached this cannot be satisfied — set
  `board::OUTPUT_SWITCH_REQUIRE_LOAD = false` for a no-load bench run.

Timings and thresholds are all in [`src/board.rs`](src/board.rs) under
`OUTPUT_*` / `VOUT_REACHED_PCT`, and the boot banner prints them.

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
| 7 | INA228 FAULT | either power monitor not answering |
| 8 | PD CTRL LOST | TPS26750 not answering |
| 9 | PD NO RAIL | source caps known, none can serve the request |
| 10 | EEPROM FAULT | last flash attempt failed |
| 11 | OUTPUT FAIL | `Fault::OutputNotReached` — conversion never reached the setpoint |
| 12 | SWITCH FAIL | `Fault::OutputSwitchFailed` — `PWR_G` asserted but the load switch was not confirmed |

A latched protection fault always wins over the other states. Codes 7–9 are held
back for `LED_SUBSYSTEM_GRACE_MS` after boot (the two chips are probed
asynchronously) and can be disabled entirely with `LED_SUBSYSTEM_CODES` for a
build that does not fit the INA228 or TPS26750. Every change is also logged on
RTT (`LED: OVERCURRENT (code 1)`) so the blink code has a matching message.

## Main screen controls

The Main power screen is the default view and the only place the output setpoint
is edited — there is no separate V-SET screen:

* **BTN1** cycles the selected regulation mode, CV ↔ CC. The header badge shows
  the selection while the output is off and the detected loop while it runs.
* **BTN2** enables/disables the converter. When a protection fault is latched,
  the first press clears the latch and keeps the output off (a deliberate
  two-step recovery); a second press enables.
* **BTN3** moves on to the next screen: Main → I-LIM → PD → CFG → Main.
* **Encoder click** toggles VSET mode. While it is on, the bottom line reads
  `>V-SET` with the voltage setpoint and the header shows the step mode; turning
  the encoder trims `v_set` by 100 mV (Fine) or 1 V (Coarse). Clicking again, or
  leaving with BTN3, exits and restores the fine step. Rotation is inert on the
  bare Main screen so a stray turn cannot move an applied setpoint.
* **Encoder double click** toggles Fine/Coarse while VSET mode is on. The single
  click is deferred by `ENC_DOUBLE_CLICK_MS` (~350 ms) so the two gestures can
  never both fire for one double click.

The voltage setpoint is floored at `board::VSET_MIN_MV` (3 V): the LT8390A cannot
regulate below its `LOADTG` minimum, so a lower setpoint would just time out in the
rev3 ramp stage and latch `OUTPUT FAIL`. The current setpoint spans
`CC_SET_MIN_MA` … `CC_SET_MAX_MA` (0.35–20 A); the I-LIM badge shows which sense
range is actually in force and appends `*` when the setpoint wants the other one.

Setpoint readouts use one fractional digit (`SET 30.0V`); the measurement columns
keep three (`12.000 V`).

## Auto-tracking PD

`PdContract` screen (reached with BTN3 from the `Main`/`I-LIM` cycle, not from
CFG): **BTN1** toggles Auto-tracking on/off, **BTN2** switches the policy
(efficiency-first vs maximum power), the encoder steps presets in manual mode,
and the encoder button confirms/re-evaluates.

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
`EEPROM WRITE`, `OUTPUT V SWEEP`. The `PD CONTRACT` screen is deliberately not an
entry here — it already has its own stop in the BTN3 screen cycle. Turn the
encoder to move the highlight, press the encoder (or BTN1) to activate, BTN3 to
go back. The list scrolls with a right-edge scrollbar once more entries are
added.

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

Edit scaling constants and NTC parameters in [`src/board.rs`](src/board.rs) after measuring divider networks on the assembled board.

Both INA228s are configured for a **2 mΩ** shunt (input R60, output R96),
`ADCRANGE = 0` (±163.84 mV) and 20 A full scale, which is `SHUNT_CAL = 1000`
(`INA228_*`). rev2's input shunt was 8 mΩ (`SHUNT_CAL = 4000`); a rev2 board
running this firmware would read input current 4× low.

The CC loop's low-end offsets (`CC_CURRENT_OFFSET_MA_1X` / `_10X`) are **0 pending
a rev3 measurement**: the rev2 figure (3650 mA) was taken on a different, high-side
sense node and does not transfer. Re-measure per range by setting two low
setpoints and taking the intercept of `actual − setpoint`, then re-run a
mid-range point to confirm. See
[`analysis/CC_RANGE_1_TO_20A.md`](analysis/CC_RANGE_1_TO_20A.md) and
[`analysis/REV3_CC_RANGE_FRONTEND.md`](analysis/REV3_CC_RANGE_FRONTEND.md).
