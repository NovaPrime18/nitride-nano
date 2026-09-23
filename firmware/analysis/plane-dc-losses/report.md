# Plane DC I²R loss — nitride-nano buck/boost power pours

- Board: `d705578:PCB/nitride-nano.kicad_pcb`
- Copper: 70 µm on all 6 layers (2 oz), ρ = 1.72e-08 Ω·m at 20 °C, T = 20 °C
- Sheet resistance Rs = 0.2457 mΩ/square
- Grid pitch: 0.050 mm base × 3×3 subcell coverage; GND at ×2.5

Method: every copper item of a net (zone fills, tracks, pads, via pads) is rasterised, made into a sheet-resistance network with via barrels coupling the layers, and solved for a 1 A injection between terminal buses; P = Σ g·ΔV².  Nets are linear, so each operating point is P = I²R.
Excluded: component resistance (FETs, shunts, inductor, ferrites, fuse), AC/skin effects, and the logic rails.

## Effective resistance per net (I = 1 A solve)

| Net | layer area (mm²) | nodes | vias | floating (mm²) | case | R_eff (mΩ) | solved |
|---|---|---|---|---|---|---|---|
| `VPP` | 594.1 | 239999 | 10 | 0.00 | Iin | 0.0400 | direct |
| `+VBUS` | 94.0 | 38296 | 3 | 0.00 | Iin | 1.5603 | direct |
| `/Power-Sense/+VBUS_SENSED` | 246.7 | 100485 | 14 | 0.00 | Iin | 0.0479 | direct |
| `/Converter/PWR_UNREG_IN` | 249.8 | 101631 | 15 | 0.00 | Iin | 0.0663 | direct |
| `Net-(U1-VIN)` | 448.4 | 182891 | 38 | 0.00 | Iin | 0.2458 | direct |
| `Net-(D1-A)` | 147.0 | 60448 | 27 | 0.00 | IL | 0.2245 | direct |
| `Net-(L1-Pad1)` | 254.9 | 103248 | 33 | 0.00 | IL | 0.1962 | direct |
| `Net-(D2-A)` | 359.2 | 145341 | 59 | 0.00 | IL | 0.1797 | direct |
| `Net-(D3-K)` | 317.8 | 128836 | 28 | 0.00 | Iout | 0.4018 | direct |
| `Net-(D20-A)` | 155.5 | 63963 | 12 | 0.00 | Iout | 0.1489 | direct |
| `Net-(FB5-Pad1)` | 286.5 | 115850 | 28 | 0.00 | Iout | 0.0189 | direct |
| `/Converter/PWR_REG_OUT` | 458.2 | 184630 | 16 | 0.00 | Iout | 0.0246 | direct |
| `GND` | 25501.9 | 1666439 | 326 | 164.17 | Iin | 0.2298 | cg |
| `GND` | 25501.9 | 1666439 | 326 | 164.17 | Iout | 0.1758 | cg |

## Loss per operating point

| Scenario | Vin→Vout | Iout | Iin | IL | `VPP` | `+VBUS` | `/Power-Sense/+VBUS_SENSED` | `/Converter/PWR_UNREG_IN` | `Net-(U1-VIN)` | `Net-(D1-A)` | `Net-(L1-Pad1)` | `Net-(D2-A)` | `Net-(D3-K)` | `Net-(D20-A)` | `Net-(FB5-Pad1)` | `/Converter/PWR_REG_OUT` | `GND` | total |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| S1 buck 48->5V 20A | 48→5.0 V | 20.0 A | 2.19 A | 20.00 A | 0.2 | 0.0 | 0.2 | 0.3 | 1.2 | 89.8 | 78.5 | 71.9 | 160.7 | 59.5 | 7.6 | 9.8 | 71.4 | **0.551 W** |
| S2 buck 20->5V 20A | 20→5.0 V | 20.0 A | 5.26 A | 20.00 A | 1.1 | 0.0 | 1.3 | 1.8 | 6.8 | 89.8 | 78.5 | 71.9 | 160.7 | 59.5 | 7.6 | 9.8 | 76.7 | **0.566 W** |
| S3 buck 48->18V 18A | 48→18.0 V | 18.0 A | 7.11 A | 18.00 A | 2.0 | 0.0 | 2.4 | 3.3 | 12.4 | 72.8 | 63.6 | 58.2 | 130.2 | 48.2 | 6.1 | 8.0 | 68.6 | **0.476 W** |
| S4 buck 48->14.5V 14.5A | 48→14.5 V | 14.5 A | 4.61 A | 14.50 A | 0.8 | 0.0 | 1.0 | 1.4 | 5.2 | 47.2 | 41.2 | 37.8 | 84.5 | 31.3 | 4.0 | 5.2 | 41.9 | **0.302 W** |
| S5 boost 12->48V 5A | 12→48.0 V | 5.0 A | 21.05 A | 21.05 A | 17.7 | 0.0 | 21.2 | 29.4 | 108.9 | 99.5 | 86.9 | 79.7 | 10.0 | 3.7 | 0.5 | 0.6 | 106.3 | **0.564 W** |
| S6 boost 20->48V 5A | 20→48.0 V | 5.0 A | 12.63 A | 12.63 A | 6.4 | 0.0 | 7.6 | 10.6 | 39.2 | 35.8 | 31.3 | 28.7 | 10.0 | 3.7 | 0.5 | 0.6 | 41.1 | **0.216 W** |
| S7 boost 24->60V 4A | 24→60.0 V | 4.0 A | 10.53 A | 10.53 A | 4.4 | 0.0 | 5.3 | 7.3 | 27.2 | 24.9 | 21.7 | 19.9 | 6.4 | 2.4 | 0.3 | 0.4 | 28.3 | **0.149 W** |
| S8 4-switch 24->24V 10A | 24→24.0 V | 10.0 A | 10.53 A | 20.00 A | 4.4 | 0.0 | 5.3 | 7.3 | 27.2 | 89.8 | 78.5 | 71.9 | 40.2 | 14.9 | 1.9 | 2.5 | 43.0 | **0.387 W** |
| S9 USB-C 48->24V 5A | 48→24.0 V | 5.0 A | 2.63 A | 5.00 A | 0.0 | 10.8 | 0.3 | 0.5 | 1.7 | 5.6 | 4.9 | 4.5 | 10.0 | 3.7 | 0.5 | 0.6 | 6.0 | **0.049 W** |

_All loss figures are milliwatts except the total, which is watts._
VPP only conducts on the XT90 input path and +VBUS only on the USB-C path.

## Breakdown by layer and copper type — S2 buck 20->5V 20A

Loss in mW at that operating point's own current through each net.

| Net | I (A) | F.Cu | In1.Cu | In2.Cu | In3.Cu | In4.Cu | B.Cu | via | zone | track+pad | total |
|---|---|---|---|---|---|---|---|---|---|---|
| `VPP` | 5.26 | 0.72 | 0.10 | 0.03 | 0.01 | 0.00 | 0.00 | 0.25 | 0.86 | 0.00 | **1.11** |
| `+VBUS` | — (not on this input path) | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | **0.00** |
| `/Power-Sense/+VBUS_SENSED` | 5.26 | 1.00 | 0.08 | 0.02 | 0.01 | 0.00 | 0.00 | 0.22 | 1.11 | 0.00 | **1.32** |
| `/Converter/PWR_UNREG_IN` | 5.26 | 1.28 | 0.18 | 0.00 | 0.03 | 0.01 | 0.01 | 0.33 | 1.50 | 0.00 | **1.83** |
| `Net-(U1-VIN)` | 5.26 | 2.30 | 1.23 | 0.00 | 0.77 | 0.67 | 0.62 | 1.20 | 5.60 | 0.01 | **6.81** |
| `Net-(D1-A)` | 20.00 | 36.01 | 15.90 | 0.00 | 6.24 | 4.46 | 3.77 | 23.40 | 66.22 | 0.16 | **89.77** |
| `Net-(L1-Pad1)` | 20.00 | 38.05 | 12.27 | 0.00 | 4.00 | 2.44 | 1.85 | 19.82 | 58.61 | 0.00 | **78.43** |
| `Net-(D2-A)` | 20.00 | 32.27 | 0.00 | 0.00 | 9.99 | 8.28 | 7.59 | 13.75 | 58.13 | 0.00 | **71.88** |
| `Net-(D3-K)` | 20.00 | 67.14 | 0.00 | 0.00 | 23.65 | 19.77 | 19.47 | 30.68 | 129.80 | 0.23 | **160.70** |
| `Net-(D20-A)` | 20.00 | 50.33 | 3.12 | 0.00 | 0.79 | 0.48 | 0.02 | 4.78 | 54.75 | 0.00 | **59.53** |
| `Net-(FB5-Pad1)` | 20.00 | 5.81 | 0.57 | 0.00 | 0.10 | 0.04 | 0.02 | 1.03 | 6.55 | 0.00 | **7.57** |
| `/Converter/PWR_REG_OUT` | 20.00 | 5.69 | 1.27 | 0.00 | 0.38 | 0.26 | 0.22 | 2.00 | 7.82 | 0.00 | **9.82** |
| `GND` | 5.26/20.00 | 16.95 | 13.29 | 59.77 | 19.45 | 17.86 | 12.10 | 34.02 | 138.40 | 1.01 | **173.44** |

## Headline

| Scenario | plane copper loss | recorded FET loss | plane as % of FET |
|---|---|---|---|
| S1 buck 48->5V 20A | **551 mW** | — | — |
| S2 buck 20->5V 20A | **566 mW** | — | — |
| S3 buck 48->18V 18A | **476 mW** | 8.91 W | 5.3 % |
| S4 buck 48->14.5V 14.5A | **302 mW** | 9.61 W | 3.1 % |
| S5 boost 12->48V 5A | **564 mW** | — | — |
| S6 boost 20->48V 5A | **216 mW** | — | — |
| S7 boost 24->60V 4A | **149 mW** | — | — |
| S8 4-switch 24->24V 10A | **387 mW** | — | — |
| S9 USB-C 48->24V 5A | **49 mW** | — | — |

Highest plane loss: **566 mW** at S2 buck 20->5V 20A; lowest 49 mW. Copper-only: excludes R9/R18 (2 mΩ), R60 (8 mΩ), FET Rds(on), inductor DCR, ferrite and fuse resistance.

## Validation

| Net | R_eff (mΩ) | bar model (mΩ) | ratio | energy check |
|---|---|---|---|---|
| `VPP` | 0.0400 | 0.0094 | 4.25 | ok |
| `+VBUS` | 1.5603 | 0.8059 | 1.94 | ok |
| `/Power-Sense/+VBUS_SENSED` | 0.0479 | 0.0171 | 2.79 | ok |
| `/Converter/PWR_UNREG_IN` | 0.0663 | 0.0193 | 3.43 | ok |
| `Net-(U1-VIN)` | 0.2458 | 0.0574 | 4.28 | ok |
| `Net-(D1-A)` | 0.2245 | 0.1174 | 1.91 | ok |
| `Net-(L1-Pad1)` | 0.1962 | 0.0700 | 2.80 | ok |
| `Net-(D2-A)` | 0.1797 | 0.0803 | 2.24 | ok |
| `Net-(D3-K)` | 0.4018 | 0.0953 | 4.22 | ok |
| `Net-(D20-A)` | 0.1489 | 0.0421 | 3.53 | ok |
| `Net-(FB5-Pad1)` | 0.0189 | 0.0064 | 2.96 | ok |
| `/Converter/PWR_REG_OUT` | 0.0246 | 0.0243 | 1.01 | ok |
| `GND` | 0.2298 | — | — | ok |
