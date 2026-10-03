# ECO — rev3: make UART reflash over the FT234XD actually work (BOOT0)

Status: **proposed**, for rev3. Supersedes the "software jump to the ROM loader"
scheme that rev2 shipped with (see `firmware/README.md` → "Service mode").

## 1. Why the rev2 scheme cannot work

Measured on the rev2 board, September 2026:

- The FT234XD ↔ USART3 link is good: `0x7F` arrives on PC11, the listener fires,
  the output is parked.
- The `.uninit` request marker is good: it survives `sys_reset()`,
  `take_request()` accepts it, and `jump_to_system_bootloader()` runs with the
  correct system-memory vectors (`0x20002160` / `0x1FFF5049`).
- **The ROM loader then returns to the application within ~100 ms.** Setting the
  core directly to the loader entry (`pc = 0x1FFF5049`, `msp = 0x20002160`) from
  a pristine reset reproduces it, so it is not the application's fault. No `0x79`
  ACK is ever produced, at any baud or parity, even while flooding `0x7F`.

The loader leaves because **BOOT0 is low at the reset sampling instant**. On the
STM32G4 BOOT0 is not a dedicated pin — it *is* PB8 (U2 pin 46). A software jump
runs the loader's code but the boot configuration still says "boot from main
flash", and the loader acts on that. Only a hardware strap at reset makes it
stay.

Two option-byte facts make this worse (RM0440 §4.4.1, FLASH_OPTR):

- `nSWBOOT0 = 0` on this part (`FLASH_OPTR = 0xFBEFF8AA`), i.e. BOOT0 is taken
  from the **software bit `nBOOT0`**, and the PB8 pin is ignored entirely. The
  ST factory value is `0xFFEFF8AA`, where BOOT0 comes from the pin.
- Selecting the pin as shipped would be worse, not better: PB8 is
  `/MCU/I2C0_SCL` and **R32 (4.7 kΩ) pulls it to +3V3**, so the pin is high at
  every reset — the board would boot the loader every time.

**Therefore rev3 must (a) free PB8 from the I²C bus, and (b) give it a strap that
is low by default and high only while a host is flashing.**

## 2. Change 1 — re-pin the two I²C buses so PB8 is free

Both buses keep their pull-ups and their net names; only the MCU pins move.

| bus | rev2 | rev3 | note |
|---|---|---|---|
| PD bus — INA228 (U9), TPS26750 (U4) | **I2C1**: PB8 = SCL, PB7 = SDA | **I2C2**: **PC4 = SCL**, **PA8 = SDA** | PC4 is the only SCL-capable free pin |
| OLED (J5, SSD1306) | **I2C3**: PA8 = SCL, PB5 = SDA | **I2C4**: **PC6 = SCL**, PB7 = SDA | frees PA8 for the PD bus |

Pin deltas: **PC4 and PC6 become used; PB8 is freed (→ BOOT0); PB5 is freed.**
All four alternate functions are valid on the STM32G474CEU (AF4 for I2C2, AF8/AF3
for I2C4), and the metapac confirms I2C2/I2C4 exist with `I2C2_EV/ER`,
`I2C4_EV/ER` interrupts. PF0/PF1 are not candidates — they carry the 8 MHz HSE
(`Y1` + C75/C76).

Keeping the same two buses (rather than merging them) preserves the firmware's
PD/UI bus separation and lock order.

## 3. Change 2 — BOOT0 strap and host control

Add at U2 pin 46 (PB8/BOOT0):

- `R_PU` **100 kΩ** from BOOT0 to **+3V3**
- `Q1` N-channel MOSFET (BSS138 / 2N7002): drain → BOOT0, source → GND,
  gate ← `R_g` **10 kΩ** ← **U12 pin 8 (~RTS)**
- `R_gs` **100 kΩ** from the gate to **+3V3**
- A test pad and a 2-pin solder jumper (`JP`) to +3V3, for a manual force/recovery

| host port | U12 pin 8 (~RTS) | Q1 | BOOT0 at reset | result |
|---|---|---|---|---|
| closed | HIGH (FTDI idle) | on | **LOW** | boots the application |
| open (flashing) | LOW (asserted by `open()`) | off | **HIGH** | ROM loader, stays |
| FTDI absent/unpowered | pulled high by `R_gs` | on | **LOW** | boots the application |

Notes:

- The FT234XD has **no DTR**; `~RTS` (pin 8) is the only host-controlled output
  that a plain `open()` toggles. `~CTS` (pin 11) is an input and `CBUS0`
  (pin 6) currently drives the D30 activity LED through R58.
- The polarity is deliberate: `open()` asserts RTS, which drives the pin low,
  which turns Q1 off, which lets BOOT0 rise. So *opening the port is what arms
  the loader* and closing it restores normal boot — with no host-side script
  required, so stock `stm32flash` works.

## 4. Change 3 — option byte

Set **`nSWBOOT0 = 1`** (FLASH_OPTR bit 26), i.e. `0xFBEFF8AA → 0xFFEFF8AA`,
so BOOT0 follows the PB8 pin. Keep `nBOOT1 = 1` so BOOT0-high selects system
memory. Program once per board (STM32CubeProgrammer → Option Bytes, or via SWD).
**Verify on the bench**: with PB8 forced high the device must enter the loader on
reset and stay there.

## 5. Firmware delta

- **Delete the handoff machinery.** With BOOT0 strapped high, `SCB::sys_reset()`
  lands in the ROM loader on its own; the `.uninit` marker, `take_request()`,
  `take_request_diag()`, `JUMP_*` and `jump_to_system_bootloader()` become dead
  code. The service trigger reduces to: park the output → `sys_reset()`.
- **Keep** the USART3 `0x7F` listener — it is the trigger. Keep the BTN1 path,
  but note it now only reaches the loader while the host port is open.
- **I²C re-pin**: `I2C1 → I2C2` (PD bus, PC4/PA8) and `I2C3 → I2C4` (OLED,
  PC6/PB7); update `bind_interrupts!`, the `I2c::new` instances/pin refs, and
  `runtime.rs`'s bus mutexes. No timing change.
- Retire the misleading `romsp`/`rompc` boot-diagnostic fields (they display
  unrelated values and caused a long false trail).

## 6. Optional — host-driven reset for recovery

If the application is corrupt, the loader is still reachable: the port being open
holds BOOT0 high, so a power cycle lands in the loader. For a *scripted* reset,
wire **CBUS0** (U12 pin 6, today the D30 LED) to NRST through a small
N-MOSFET, configure CBUS0 as a GPIO in the FTDI EEPROM, and pulse it from a host
script. Stock `stm32flash` cannot drive CBUS0, so this is a recovery aid only.

## 7. Zero-hardware alternative — works on rev2 today

The `nBOOT0` software bit can also select the loader, so this needs no board
change and is worth validating while rev3 is laid out:

1. On the trigger, the app programs FLASH_OPTR so that software BOOT0 selects
   **system memory**, then launches the option-byte reload (`OBL_LAUNCH`).
2. The chip resets, samples the option bit, and enters the ROM loader — properly,
   so it **stays**, and `stm32flash` can sync.
3. The freshly flashed image restores the flash-boot option bit on its first boot.

Costs: option-byte erase cycles on every flash, and if power is lost between
steps 1 and 3 the board boots the loader on power-up (recoverable by flashing
over UART, i.e. not a brick). This is the same mechanism the shared-PB8 problem
is normally solved with, and it is the fastest way to a working USB-C reflash.

## 8. Bench validation for rev3

1. Option bytes read back as `FLASH_OPTR = 0xFFEFF8AA`.
2. Port closed, power cycle → application runs (OLED up). PB8 measures ~0 V.
3. `stm32flash -b 115200 /dev/ttyUSB0` with no arguments → syncs and prints the
   bootloader version and `Device ID: 0x468`.
4. Full write: `stm32flash -b 115200 -w firmware.bin -v -g 0x08000000 /dev/ttyUSB0`
   completes; the `Go` command returns to the application.
5. Close the terminal/port, power cycle → application still runs.
6. Unplug the FT234XD entirely, power cycle → application runs (Q1 held on by
   `R_gs`).
