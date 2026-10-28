# nitride-nano firmware

Embassy-based Rust firmware for the **STM32G474CEU6** on the nitride-nano pocket buck/boost supply.

## Features

- **CV/CC control** via 12-bit DAC (PA4 CV, PA6 CC) referenced to VREF+ = +3V3 (the on-board tie; the internal VREFBUF is left high-Z); open-loop CV driven as an (inverted) map from setpoint to DAC code, derived analytically from the LT8390A feedback divider (R19/R20/R36), with code-rate slew limiting (no feedback loop)
- **Switchable CC sense range** (`High-Gain`, PB6): ×1 directly across R96, or ×10 through the U14 INA241A1, giving 2.5–20 A or 0.3–5 A with ten times the resolution. The pin only moves while the output is parked, because a range change is a ×10 step in CC loop gain
- **Sequenced output stage**: `Conv-Disable` (PB1) enables the converter with the bridge parked through `CNV-EN-LOADEN` (PB3); raising that starts conversion, and `PWR_G` (PB2) connects the load only after `Vsense_REG_OUT` reaches the setpoint, verified against the output INA228
- **ADC telemetry** on PA0 (`Vsense_REG_OUT`) / PA1 / PA3 (`ISMON`) / PA7 / PA9
- **Two INA228** power monitors on I2C2: input (U9, `0x40`, R60 = 2 mΩ) and output (U12, `0x44`, R96 = 2 mΩ — the straps are `A1 = +3V3`, `A0 = GND`, which is address **0x44**, not the binary-weighted guess; see the note on `board::INA228_OUT_ADDR`). The output one is authoritative for the displayed Vout/Iout/Pout and is the only evidence the load switch closed
- **Auto-tracking PD** — negotiates a fixed input rail from the output setpoint, keeping the LT8390A out of its 4-switch buck-boost region when the requested power allows
- **SSD1306** powersupply UI (128×64 I2C)
- **3 buttons + rotary encoder** (PB9–11, PB4/PB5 quadrature, PB0 push-button read as `ADC1_IN15` — see the note on `board::ENC_BTN_PRESSED_MV`: a rev3 board measures ~2.4 V pressed, which no GPIO reads as a logic low; `BUTT_1` has an optional analog path of its own, `board::BTN1_ANALOG`, because PB9 is not an ADC pin)
- **TPS26750** USB-C PD driver (port of `../src/tps26750.cpp`)
- **Protection**: 60 V / 20 A / 240 W / NTC overtemperature / input OCP-OVP / output-not-reached / output-switch-failed / output-above-setpoint
- **USB-C reflash** via the STM32G474 ROM USB DFU bootloader (BOOT0 hardware strap, no application code)
- **First-enable interlock**: a boot self-test (ADC/DAC reference on two ADC instances, thermistor plausibility, gate-line readback, input range, monitors, option bytes) that refuses to enable the converter on any `[FAIL]`, with one actionable `preflight:` log line per check — see [BENCH.md](BENCH.md#0-first-enable-interlock-read-this-first)
- **CV set accuracy**: a bench-fitted affine CV map (`CV_CAL_*`) plus a ±1-code metrology servo against the ±0.05 % output INA228 — ±20 mV at 20 V over temperature
- **Cross-revision safety**: the same binary runs (and cannot damage) a rev2 board — a boot-time probe of the rev3 PD bus gates every rev3-only pin and every output enable

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
never a brick.** On rev2 there is no BOOT0 strap (`PB8` is the PD-bus SCL
there) and its `nSWBOOT0 = 0`, so SWD is the *only* way to reflash a rev2 board
— exactly as it has always been.

> **Safety while flashing (both revisions):** no firmware pin can hold its state
> through a reset. What the hardware does on its own, from reset through the
> whole ROM-loader session:
>
> * **The load is safely disconnected.** `PWR_G` has a 100 kΩ pulldown (`R21`),
>   so `Q12`/`Q7`/`Q5` stay off with any input attached — a load on the output
>   can never be driven.
> * **The converter is enabled but inert.** `PB1` (rev3) / `PA11` (rev2) float,
>   `Q13` is off, and the `R7` 357 k / `R8` 64.9 k divider pulls `EN/UVLO` above
>   its 1.22 V threshold — the LT8390A is awake. But with both DAC pins
>   floating, the FB pin's 1 V regulation point bleeds through `R36` into the
>   floating DAC and no current flows there, so the CV loop regulates the
>   *converter-side* node to ≈ `1 V × (1 + R19/R20)` ≈ **36.7 V**, current-limited
>   by the CTRL divider's float value `VREF × R4/(R4+R5)` ≈ 347 mV ≈ **4.9 A**
>   (×1). That voltage sits behind the open `Q5` and behind fuse `F2` — nothing
>   external sees it except the `D21` indicator LED glowing faintly — but the
>   **output connector's positive terminal is live at ~36.7 V while flashing.**
>   **Flash with the load disconnected**, and note that rev3 needs the XT90/PD
>   input connected anyway (JP13 is bridged VPP→buck, so USB power alone does
>   not feed the logic rail).
> * The recommended permanent fix is the one-resistor ECO from
>   [BENCH.md](BENCH.md): a 100 kΩ pull-up on `Q13`'s gate to +3V3, which turns
>   "MCU not in control" into *converter disabled* instead of *converter
>   floating*.

## Cross-revision operation (rev2 ↔ rev3)

The board revisions share almost no MCU pins, so "runs everywhere" has to mean
"runs safely everywhere" — the full UI only exists on the revision the firmware
was built for. This firmware is rev3's, and it carries a **boot-time board
detect** so the wrong pairing cannot damage anything:

* **Boot detect.** Before taking control of any rev3-only pin, the firmware
  probes three signatures. (1) the PD bus (`I2C2` on PC4/PA8) for either INA228
  (U9 @`0x40`, U12 @`0x44`); on rev2 those pins carry no device at all (PC4 is
  unconnected, PA8 is rev2's OLED SCL), so nothing can ACK; five fast retries
  absorb a slow monitor power-up. (2) if that bus does not answer, the
  **SSD1306 on the UI bus** (I2C4 PC6/PB7 @`0x3c`); rev2's PC6 has no pull-up,
  so its UI bus cannot even clock. Signatures 2 and 3 matter because the PD bus
  is powered by the TPS26750's *own* internal LDO (`+3V3_LDO`, which feeds the
  MCU-side pull-ups R31/R32): with no USB-C VBUS attached the whole PD bus is
  dark on a perfectly healthy rev3, and without them an XT90-only bench session
  would park a good board. (3) the **UI bus simply being pulled up** (`R88`/
  `R89` present on both lines): rev2 leaves PC6/PB7 unconnected, so this is a
  static property of the board rather than of a device answering. It is the
  backstop for (2), because a panel still inside its power-on reset does not ACK
  and on XT90 that ACK is the only other evidence there is — which is why the
  display probe is also retried a few times instead of being taken once.
* **rev3 confirmed** → everything works as documented.
* **not confirmed** → the firmware never drives `PB1`/`PB2`/`PB3`/`PB6`
  (`parked()` HAL wrappers, pins stay floating inputs), the supply controller
  **refuses every output enable** — the sequencer stays in `Off` and both DACs
  stay parked — and the OLED task is not spawned. The chip runs (LED
  heartbeat/blink codes, buttons, RTT logs); it just cannot produce an output
  it cannot supervise. The LED shows code 7 (INA missing) after the boot grace
  period.

Once the board is recognised the OLED task keeps retrying on its own: a panel
that was slow to come up, or that stops taking frames, is re-initialised (with a
backoff) and the whole screen is repainted. The display therefore recovers
without an MCU reset.

The resulting matrix:

| Firmware ↓ / board → | rev2 | rev3 |
|---|---|---|
| **this (rev3) firmware** | runs safe, output refused, OLED/encoder dead (PB6 on rev2 is the encoder-A line — driving it would short the pin, hence the gate) | **full function** |
| **rev2 firmware** | full function | chip runs; I2C/OLED/encoder dead; load stays disconnected (`PWR_G` pulldown); converter inert while parked (CTRL at latch-off). **Do not blind-enable**: a button press un-parks the DACs on a converter the old firmware can no longer disable (its disable line, PA11, is rev3's `USB_D_N`), so if `LOADEN` happens to float high the output connector can go live at the old default setpoint (~30 V) — unsupervised |

**Powering:** the logic rail (`+3V3`) is its own buck from the VPP input, so the
MCU, OLED and both INA228 supply pins run from the XT90/PD input alone. The
**PD bus**, however, is powered by the TPS26750's internal LDO (`+3V3_LDO` —
U4 pad 1), which supplies the MCU-side I2C2 pull-ups R31/R32; it only exists
while USB-C VBUS is present. An XT90-only session therefore has a running UI
and a dead PD bus (no PD negotiation, no INA228s) — by hardware design, not a
fault. Attach any USB-C source to bring the PD side up.

Two hardware facts make all four cells safe by construction, even with no
firmware running at all (reset, DFU loader, blank chip):

* `PWR_G` → `R21` 100 kΩ pulldown: the load is disconnected whenever the MCU is
  not actively driving `PB2` high. On rev2 the same role is played by the
  LT8390A's own `LOADTG` (off below ~2.4–3 V of VOUT).
* the CC CTRL divider `R5` 357 k: a *driven* CC DAC code of 0 pulls CTRL to
  ≈48 mV — below the 285–315 mV latch-off — so a parked CC DAC is a genuine
  shutdown, and a *floating* DAC leaves the divider's ≈347 mV ≈ 4.9 A as the
  fail-safe limit.

## Pin map

### rev3

| Signal | Pin | Notes |
|--------|-----|-------|
| PD bus I2C2 SCL/SDA | PC4 / PA8 | TPS26750 (`0x21`) + both INA228s (U9 `0x40`, U12 `0x44`). PC4/PA8 is a valid single-peripheral pin pair, so rev2's SCL bodge is gone |
| UI bus I2C4 SCL/SDA | PC6 / PB7 | SSD1306 OLED (`0x3C`), and CAT24C512 EEPROM (`0x50`) when JP4/JP5 are bridged (pads 2–3). `R88`/`R89` 10 kΩ pull-ups live on the MCU-side nets |
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
| `CNV-EN-LOADEN` | PB3 | GPIO → LT8390A `LOADEN`. Low = `TG1`/`TG2` off, `BG1`/`BG2` on (bridge parked, **output node isolated** — it does *not* discharge the output caps); high releases soft-start |
| `High-Gain` | PB6 | GPIO → U7 TMUX1574 `SEL`. Low = ×1 direct across R96, high = ×10 through the U14 INA241 |
| Status LED | PA15 | D31, **active high** (PA15 → anode → R81 → GND); `TIM2_CH1` hardware PWM |
| Buttons | PB9 / PB10 / PB11 | Active low GPIO. **`BUTT_1` (PB9) has no analog function**, so if that pin cannot reach a logic high (a rev3 board measured ~0.8 V *released*, ≈13 kΩ to ground on the pad) the only firmware route is the rework described at `board::BTN1_ANALOG`: move the read to PA2 (`ADC1_IN3`) and threshold it like the encoder switch |
| Encoder A/B | PB4 / PB5 | TIM3 QEI (was TIM4 on PB6/PA12) |
| Enc button | PB0 | **Analog** (`ADC1_IN15`), not a digital input: `+3V3 → R64 10 kΩ → node → R65 1 kΩ → PB0`, `C81 100 nF`, `SW1` to GND. Pressed is thresholded at `board::ENC_BTN_PRESSED_MV` / released at `board::ENC_BTN_RELEASED_MV` because rev3 boards measure only ~2.4 V pressed (see the note on that constant) |
| PD IRQ | PB13 | EXTI |
| Spare | PA2, PA5, PA10, PC10, PC11 | PC10/PC11 are the freed USART3 pins. **PA2 is the intended landing pad for the `BUTT_1` rework** (`ADC1_IN3`), see `board::BTN1_ANALOG` |
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
| `Precharge` | **enabled** | low | **closed**¹ | Bridge parked (`TG` gates off, output isolated) while INTVCC/VREF/CTRL settle — and while any stale output charge bleeds below 105 % of the setpoint |
| `Ramping` | enabled | **high** | closed | `LOADON` asserts, soft-start begins, output rises with the load already connected |
| `SwitchOn` | enabled | high | closed | Load already connected; confirmation comes from the regulated output plus a live U12 (and from load current, if `OUTPUT_SWITCH_REQUIRE_LOAD` is set), retried for `OUTPUT_SWITCH_VERIFY_MS` |
| `Running` | enabled | high | closed | Normal CV/CC operation |
| `Disabling` | off after 2 s | low after 1 s | **open once Vout < 5 V** | Unwinds in reverse: bridge parked, then chip off, and `PWR_G` is released only after the output has fallen below `OUTPUT_SWITCH_SAFE_VOUT_MV`. Slow on purpose (bench-observable: `OUTPUT_DISCHARGE_MS` + `OUTPUT_CONV_OFF_MS` = 1 s + 1 s). The output node itself only bleeds through its dividers (seconds — see the stale-charge note below) |

¹ `board::OUTPUT_SWITCH_EARLY_CLOSE` (default `true`) — see the mux note below.

Two things make this work, and both are easy to get wrong:

* **`LOADEN` is only half the condition.** The LT8390A reaches its `UP/PRE` state
  only when CTRL is above its rising latch-off (~0.325 V) **and** `LOADEN` is high.
  A sub-floor current setpoint would therefore command CTRL below latch-off, raise
  `LOADEN`, and start nothing. The sequencer floors the CC command at the range's
  cold-start value (4.5 A at ×1, 450 mA at ×10) for the whole ramp and logs it.
* **`PWR_G` follows the output, not the script.** `Q5` is in the **return**, so
  opening it does not disconnect the load from `VOUT`; it only interrupts the
  return current, and with a load attached the whole return node (`J3.1`, `R96`,
  the `SENSE_P`/`SENSE_N` taps) is then pulled up to `VOUT` through that load. On
  rev3 the only low-voltage part on those taps is the `U7` TMUX1574 range mux,
  whose signal pins are rated **6 V absolute maximum** — its ESD clamp turns the
  floating return into a current source limited only by the 27 Ω taps, which
  destroyed two muxes and, with them, the CC loop and every current reading. The
  firmware cannot see the return node, so it enforces the only safe rule: `PWR_G`
  is closed whenever the output is above `OUTPUT_SWITCH_SAFE_VOUT_MV` (5 V) —
  closed before the ramp, released only after the output has fallen. The cost is
  that the load is present during the ramp, so a load drawing the CC limit settles
  into CC (`RAMP_CC_SETTLE_MS`) instead of tripping the ramp budget.

**Why the wait in `Precharge`:** forcing `LOADEN` low turns `TG2` off, which
*isolates* the output node from the bridge — `BG1`/`BG2` clamp the inductor
side, not the output caps. After a high-voltage session the node therefore keeps
its charge and bleeds through ~130 kΩ of dividers at roughly 1 V/s. Since the
converter cannot sink, the sequencer cannot pull it down either — so `Precharge`
refuses to raise `LOADEN` (and never closes `PWR_G`) while the node sits above
105 % of the setpoint, and latches `OUT TOO HIGH` (LED 13) if it never comes
down. That is also the safety property that matters: **a stale output can never
be connected to the load at the wrong voltage.**

> **Bench caveat.** That wait keeps `PWR_G` open with the output high, which is
> exactly the state that exposes the U7 mux if a load is attached — the firmware
> can see the output voltage but not the floating return node, so it cannot tell
> whether anything is pulling that node up. Disconnect the load (or remove the
> external source) before re-enabling onto a charged output. Likewise **never
> unplug USB-C with the output on and a load attached**: the output capacitors
> then discharge through the load into the floating return with the MCU already
> unpowered, which no firmware rule can prevent. The rev4 sense front end removes
> the hazard; see [`analysis/REV3_SCHEMATIC_AUDIT.md`](analysis/REV3_SCHEMATIC_AUDIT.md).

Timings and thresholds are all in [`src/board.rs`](src/board.rs) under
`OUTPUT_*` / `VOUT_REACHED_PCT` / `VOUT_START_MAX_PCT`, and the boot banner
prints them.

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
| 7 | INA228 FAULT | a power monitor not answering **while the PD bus is alive** |
| 8 | PD CTRL LOST | TPS26750 not answering on a live PD bus |
| 9 | PD NO RAIL | source caps known, none can serve the request |
| 10 | EEPROM FAULT | last flash attempt failed |
| 11 | OUTPUT FAIL | `Fault::OutputNotReached` — conversion never reached the setpoint |
| 12 | SWITCH FAIL | `Fault::OutputSwitchFailed` — `PWR_G` asserted but the load switch was not confirmed |
| 13 | OUT TOO HIGH | `Fault::OutputAboveSetpoint` — the output node would not bleed below 105 % of the setpoint (external source holding it up?) |
| 14 | SELF-TEST | `Fault::PreflightFailed` — the boot self-test found a `[FAIL]`; see the `preflight:` lines |
| 15 | SENSE OPEN | `Fault::SenseOpen` — the output-current chain is blind (converter delivering power while both `ISMON` and U12 read ~0, or U12 sees current `ISMON` does not). The CC loop and the over-current trip cannot see the load current, so the stage is switched off; check the U7 mux (`S1A`/`S2A`), `R83`/`R98`, `C71` and the `R96` taps |

A latched protection fault always wins over the other states. Code 15 is the
watchdog that turns the "it delivers 45 W but every current reading says 0 A"
failure into a latch instead of a silent blind run: it compares the measured
output power against the *input* INA228 (a different shunt, so it survives the
fault being detected) and cross-checks U12 against `ISMON`. It needs the input
monitor, so it is active on a USB-C session only; see `board::SENSE_OPEN_*`. Codes 7–9 describe
the **USB-PD side** and are only produced when that side is *alive* — the
TPS26750 and both INA228s are powered from the controller's own LDO, so an
**XT90/DC input has no PD bus at all** and that is a supported mode, not a fault
(see below). They are held back for `LED_SUBSYSTEM_GRACE_MS` after boot (the
chips are probed asynchronously) and can be disabled entirely with
`LED_SUBSYSTEM_CODES` for a build that does not fit them.

### XT90 / DC input parity

The USB-PD side (`TPS26750` + both `INA228`s) hangs off **`+3V3_LDO`, the
TPS26750's own internal LDO**, which only exists while a USB-C source supplies
VBUS. With an XT90/DC feed the firmware therefore treats the PD side as
*absent by design*, not broken:

* no LED blink codes 7–9 and no `NO PD` error — the OLED's Iin field reads
  `XT90`, and the PD screen reads `NO PD BUS`;
* Vin comes from the ADC divider, output current from `ISMON`;
* the load-switch verification degrades to the output-voltage check (logged as
  "confirmed by voltage only"), because `U12` — the only switch-closed evidence —
  cannot exist in that mode;
* the PD bus is probed only every ~10 s, purely so attaching a USB-C source later
  brings the PD side up without a reboot.

Because both `INA228`s live on that bus, **an XT90-only session has no current or
voltage monitor** — Vin/Vout come from the resistor dividers. With a USB-C source
attached the firmware prints `supply: vin divider cross-check` (and a `vout` one
while `Running`) naming the exact `board::VBUS_SENSE_NUM` / `VOUT_SENSE_NUM` that
makes the dividers agree with the ±0.05 % monitors, so the ADC path can be
calibrated once and used on XT90 afterwards (BENCH §3).

USB-C and XT90 are peers: nothing about the converter path depends on which one
feeds it. Every change is also logged on
RTT (`LED: OVERCURRENT (code 1)`) so the blink code has a matching message.

## Main screen controls

The Main power screen is the default view. The output-voltage setpoint has **two
routes to the same field**, and both render identically (`>V-SET` tag plus the
`SET` readout), so switching between them never moves anything on the panel:

* **Encoder click** on Main — the quick path: VSET mode inside the power screen.
* **BTN3** from Main — the explicit path: the dedicated `V-SET` screen, first stop
  in the screen cycle.

Main-screen controls:

* **BTN1** cycles the selected regulation mode, CV ↔ CC. The header badge shows
  the selection while the output is off and the detected loop while it runs.
* **BTN2** enables/disables the converter. When a protection fault is latched,
  the first press clears the latch and keeps the output off (a deliberate
  two-step recovery); a second press enables.
* **BTN3** moves on to the next screen: Main → V-SET → I-LIM → PD → CFG → Main.
* **Encoder click** toggles VSET mode. While it is on, the bottom line reads
  `>V-SET` with the voltage setpoint and the header shows the step mode; turning
  the encoder trims `v_set` by 100 mV (Fine) or 1 V (Coarse). Clicking again, or
  leaving with BTN3, exits and restores the fine step. Rotation is inert on the
  bare Main screen so a stray turn cannot move an applied setpoint.
* **Encoder double click** toggles Fine/Coarse while VSET mode is on. The single
  click is deferred by `ENC_DOUBLE_CLICK_MS` (~350 ms) so the two gestures can
  never both fire for one double click.

The `V-SET` and `I-LIM` screens behave the same way: turn to trim (100 mV / 1 V,
100 mA / 1 A), encoder click toggles Fine/Coarse, **BTN2** switches the output
without leaving the screen (with the same fault-latch rule as Main: the first
press clears the latch and keeps the output off), and **BTN3** moves on down the
cycle (restoring the fine step on the way out). Leaving Main with BTN3 clears
VSET mode, so the two routes are never active at once.

The voltage setpoint is floored at `board::VSET_MIN_MV` (3 V): the LT8390A cannot
regulate below its `LOADTG` minimum, so a lower setpoint would just time out in the
rev3 ramp stage and latch `OUTPUT FAIL`. The current setpoint spans
`CC_SET_MIN_MA` … `CC_SET_MAX_MA` (0.35–20 A); the I-LIM badge shows which sense
range is actually in force and appends `*` when the setpoint wants the other one.

Setpoint readouts use one fractional digit (`SET 30.0V`); the measurement columns
keep three (`12.000 V`).

## Auto-tracking PD

`PdContract` screen (the third stop in the BTN3 cycle, after `I-LIM`; it is not a
CFG entry): **BTN1** toggles Auto-tracking on/off, **BTN2** switches the policy
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
sweep point 7/32: vset=19839 vout=19810 iout=2100 vin=20000 pin_avg=44400 pout_avg=41800 eta=94.1% n=60 cv_code=1826 u12=19813
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

## CV set accuracy: calibration + servo

Setting 20.000 V must put ~20.00 V at the output. The setpoint path is
open-loop (DAC → 3.3 V rail → R19/R20/R36 → the LT8390A's FB reference), and
its ±0.5–1 % of stacked tolerances — multiplied by the 35.7× summing gain —
landed the bench at 20.13 V. Two mechanisms fix it
(full design in [`analysis/CV_CALIBRATION_PLAN.md`](analysis/CV_CALIBRATION_PLAN.md),
bench procedure in [BENCH.md](BENCH.md#8-cv-accuracy-map-calibration--metrology-servo)):

1. **A fitted affine map** (`VOUT = A − B·code`, `board::CV_CAL_*`) replaces
   the analytic resistor-derived map once the board has been swept and fitted
   with [`analysis/cv_cal/fit.py`](analysis/cv_cal/fit.py). The fit runs
   against the output INA228 U12 (±0.05 % VBUS), which is itself cross-checked
   once against a trusted DMM (`board::INA228_OUT_VBUS_SCALE_*`). Removes the
   systematic resistor/reference/DAC-gain errors.
2. **A metrology servo** (`board::CV_SERVO_*`, in `control::supply`) nudges the
   CV DAC target by ±1 code per 500 ms until the *measured* output (U12, at the
   load terminals — fuse/bead drop included) is within a 15 mV deadband of the
   setpoint. It only acts in settled CV regulation, is frozen during CC,
   sweeps, ramps and U12 loss, and its authority is clamped to ±64 codes, so
   it cannot fight the current limit or move the output quickly. Removes DAC
   INL and temperature drift — the things a fit cannot.

Expected end state: ±20 mV at 20 V across temperature (one DAC LSB is 28.8 mV;
finer than that would need dithering, which is deliberately out of scope).
The boot banner prints which map is in force and the whole cal/servo
configuration.

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
