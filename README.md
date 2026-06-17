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

Nitride-nano is an open-source, high-power-density buck/boost converter crammed into the smallest footprint possible. It targets the same use case as the [Miniware MDP-XP](https://www.miniware.com.cn/product/mdp-xp-digital-power-supply/) — a portable, wide-range programmable power supply — with a focus on **higher power density and a wider voltage range** rather than output ripple performance.

Built around EPC GaN FETs switching at 1 MHz and an LT8390A-based controller topology, it accepts either USB-C Power Delivery or high-voltage DC input and delivers up to 240 W from an XT90 output connector.

---

## Specifications

| Parameter | Value |
|---|---|
| Input voltage | 12 – 60 V |
| Output voltage | 0 – 60 V |
| Input / Output current | 20 A (design target, untested) |
| Max output power | 240 W |
| Switching frequency | 1 MHz |
| Topology | Non-inverting buck/boost |
| Input connector | XT90 |
| Output connector | XT90 |

> ⚠️ **20 A / 240 W figures are design targets and have not yet been verified by testing.**

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

Simulated with Freecad, the enclosure reaches about 38 Degrees C (with a fan) when there's 10W of of losses directly going into the heatsink from the GaNFETs. Real world performance is likely better, due to not all 10W leaving the Fets through the top.

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

> 🚧 Work in progress — PCB design phase. Not yet built or tested.
