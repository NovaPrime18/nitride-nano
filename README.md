# nitride-nano

**Pocket-sized GaN buck/boost converter with USB-C PD input**

[![License: CERN-OHL-S v2.0](https://img.shields.io/badge/License-CERN--OHL--S%20v2.0-blue.svg)](https://ohwr.org/cern_ohl_s_v2.txt)
[![KiCad](https://img.shields.io/badge/Made%20with-KiCad-blue)](https://www.kicad.org/)

<table><tr>
<td><img src="https://raw.githubusercontent.com/NovaPrime18/nitride-nano/main/Pics/nitride-nano-f-n.png" width="100%"></td>
<td><img src="https://raw.githubusercontent.com/NovaPrime18/nitride-nano/main/Pics/nitride-nano-b.png" width="100%"></td>
</tr></table>
<img src="https://raw.githubusercontent.com/NovaPrime18/nitride-nano/main/Pics/nitride-nano-f.jpg" width="100%">

---

## Overview

Nitride-nano is an open-source, high-power-density buck/boost converter crammed into the smallest footprint possible. It targets the same use case as the [Miniware MDP-XP](https://www.miniware.com.cn/product/mdp-xp-digital-power-supply/)  a portable, wide-range programmable power supply. With a focus on **higher power density and a wider voltage range** rather than output ripple performance.

Built around EPC GaN FETs switching at up to 1 MHz and an LT8390A-based controller topology, it accepts either USB-C Power Delivery or high-voltage DC input and delivers up to 240 W from an XT90 output connector.

---

## Specifications

| Parameter | Value |
|---|---|
| Input voltage | 12 – 48 V |
| Output voltage | 0 – 60 V |
| Input / Output current | 20 A (design target, untested) |
| Max output power | 240 W |
| Switching frequency | 1 MHz (Rev 1) / 600 kHz (Rev 2) |
| Topology | Non-inverting buck/boost |
| Input connector | XT90 |
| Output connector | XT90 |

> ⚠️ **20 A / 240 W figures are design targets. Revision 1 testing has verified up to 164 W output (see Testing below) — the full 20 A / 240 W envelope is still unverified.**

---

## USB-C Power Delivery

Nitride-nano negotiates full USB-C PD on the input side:

- **Fixed PDO** — standard 5/9/12/15/20 V profiles
- **PPS** — 5–21 V programmable (50 mV steps)
- **AVS / EPR** — 15–48 V extended power range

---

## Hardware

| Component | Part |
|---|---|
| MCU | STM32G474CEUx |
| Power stage FETs | EPC2367 (eGaN) |
| Controller | LT8390A / EVAL-LT8390A-AZ topology |
| Power monitor | INA228 (I²C, 16-bit) |
| Display | SSD1306 0.96″ OLED |
| UI | 3-button interface |
| Overcurrent protection | Hardware (INA228-triggered) |
| EDA | KiCad |

---

## Firmware

- STM32G474 HAL / bare-metal
- SSD1306 OLED UI with 3-button navigation
- USB-C PD stack (Fixed / PPS / AVS negotiation)
- INA228 real-time power monitoring
- Hardware overcurrent fault handling

---

## Thermal Sim
<table><tr>
<td><img src="https://raw.githubusercontent.com/NovaPrime18/nitride-nano/main/Pics/Screenshot_20260525_175715.png" width="100%"></td>
<td><img src="https://raw.githubusercontent.com/NovaPrime18/nitride-nano/main/Pics/Screenshot_20260525_180133.png" width="100%"></td>
</tr></table>

Simulated with Freecad, the enclosure reaches about 38 Degrees C (with a fan) when there's 10W of losses directly going into the heatsink from the GaNFETs. Real world performance is likely better, due to not all 10W leaving the Fets through the top.

---

## Testing — Revision 1 (1 MHz)

Bench results from Revision 1 hardware, measured at the operating points below.

| # | Input | Output | Efficiency |
|---|---|---|---|
| 1 | 25 V, 7 A — 174.2 W in | 36.6 V, 4.48 A — 164 W out | 94.1 % |
| 2 | 40 V, 3.61 A — 144.3 W in | 37.1 V, 3.48 A — 129.1 W out | 89.5 % |
| 3 | 13 V, 9.28 A — 120.6 W in | 36.5 V, 3 A — 109.1 W out | 90.5 % |

**Equipment:** Riden RD6024 bench supply (input) · EA-EL 3160-60 electronic load (output)

**Thermals:** the inductor core ran hottest at ~90 °C — by far the lossiest component on the board. The GaN FETs barely got above 60 °C.

---

## Revision 2 Changes

Reworks to the power stage and the bring-up bugs found on Revision 1:

**Power stage**
- New 2.2 µH inductor (Coilcraft XAL1010-222MED) at 600 kHz switching, chosen for lower core losses — the Rev 1 core was the hottest part by far
- Turn-off diodes in the gate paths: soft turn-on, quick turn-off
  - This prevents Miller turn-on of the output top FET caused by the input bottom FET through the inductor core (annoying issue to debug)
- Flyback diodes on the power FETs: better placement and a different footprint (PMEG6020ETP)
- More output capacitance
- Series resistors in the CV and CC paths so the LT chip doesn't blow up (it happened 3×)
- The LT chip's 5 V rail is now fed from a switching regulator instead of an ultra-lossy LDO at high input voltage

**Control / MCU**
- MCU now measures the pre-output node instead of the output, for better insight into what the LT controller chip is doing
- Reset mechanism around the MCU reworked
- Both I²C busses wired correctly this time
- OLED pinout is now correct

**Connectivity**
- Corrected pinout of the FTDI chip
- Nicer USB-C connector

Firmware developed alongside Rev 2: linearized CV control with ramping, OLED error messages, filtered readings, and a split between the measurement/control and display tasks.

---

## Acknowledgements / Based On

- [**PD240W** by theohg](https://github.com/theohg/PD240W) — USB-C PD input stage reference
- [**EVAL-LT8390A-AZ** by Analog Devices](https://www.analog.com/en/resources/evaluation-hardware-and-software/evaluation-boards-kits/eval-lt8390a-az.html) — buck/boost power stage reference

---

## License

Hardware design files are licensed under the **CERN Open Hardware Licence Version 2 – Strongly Reciprocal (CERN-OHL-S v2.0)**.

See [`LICENSE`](./LICENSE) for the full text, or visit [ohwr.org/cern_ohl_s_v2](https://ohwr.org/cern_ohl_s_v2.txt).

> In short: you are free to use, study, modify, and distribute this design, provided that any derivatives are released under the same licence and source files are made available.

---

## Status

> 🚧 Work in progress — Revision 1 built and power-tested (results above); Revision 2 design complete and ordered.
