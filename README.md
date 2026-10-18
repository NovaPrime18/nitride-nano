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
| Input / Output current | 20 A (design target; 17.2 A input / 12.92 A output verified on Rev 2) |
| Max output power | 240 W |
| Switching frequency | 1 MHz (Rev 1 and Rev 2; `R1` = 147 kΩ on `RT`) |
| Topology | Non-inverting buck/boost |
| Input connector | XT90 |
| Output connector | XT90 |

> ⚠️ **20 A / 240 W figures are design targets. Revision 2 testing has verified up to 219 W output (52.4 V @ 4.18 A, 94.8 %); the full 240 W output envelope is still unverified.**

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
- USB-C PD stack (Fixed / PPS / AVS negotiation) over I²C
- **Auto-tracking PD** — derives the input rail from the requested output voltage so the LT8390A stays in a clean buck or boost region instead of its lossier 4-switch band
- INA228 input-bus power monitoring (Vin / Iin / Pin + die temp)
- Hardware overcurrent fault handling, plus firmware input OCP/OVP from the INA228

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

## Testing — Revision 2 (2.2 µH, 1 MHz)

Bench results from Revision 2 hardware with the 2.2 µH Coilcraft XAL1010-222MED
inductor at 1 MHz switching. Efficiency is `Pout / Pin`.

### USB-C input

| # | Input | Output | Efficiency |
|---|---|---|---|
| 1 | 28 V, 3.957 A — 110.8 W in | 12.4 V, 8.04 A — 99.7 W out | 90.0 % |
| 2 | 36 V, 2.288 A — 82.4 W in | 12.4 V, 6.09 A — 75.5 W out | 91.7 % |
| 3 | 20 V, 4.75 A — 95.0 W in | 6.4 V, 12.92 A — 82.7 W out | 87.0 % |

### XT90 input

| # | Input | Output | Efficiency |
|---|---|---|---|
| 1 | 16 V, 10.54 A — 168.6 W in | 52 V, 2.97 A — 154.4 W out | 91.6 % |
| 2 | 13.81 V, 12.71 A — 175.5 W in | 38.6 V, 4 A — 154.4 W out | 88.0 % |
| 3 | 16 V, 14.13 A — 226.1 W in | 51.7 V, 4 A — 206.8 W out | 91.5 % |
| 4 | 16 V, 14.86 A — 237.8 W in | 51.8 V, 4.2 A — 217.6 W out | 91.5 % |
| 5 | 14 V, 17.2 A — 240.8 W in | 51.7 V, 4.2 A — 217.1 W out | 90.2 % |
| 6 | 24 V, 9.72 A — 233.3 W in | 52 V, 4.18 A — 217.4 W out | 93.2 % |
| 7 | 36 V, 6.42 A — 231.1 W in | 52.4 V, 4.18 A — 219.0 W out | 94.8 % |

**Peak efficiency:** 94.8 % at 36 V in → 52.4 V, 4.18 A (219.0 W out). The
highest input-power point was 240.8 W (14 V, 17.2 A) at 90.2 %; peak output
power was 219.0 W, up from 164 W on Revision 1.

---

## Revision 2 Changes

Reworks to the power stage and the bring-up bugs found on Revision 1:

**Power stage**
- New 2.2 µH inductor (Coilcraft XAL1010-222MED) at 1 MHz switching (`R1` = 147 kΩ on `RT`), chosen for lower core losses — the Rev 1 core was the hottest part by far
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

> 🚧 Work in progress — Revision 1 and Revision 2 built and power-tested (results above).
