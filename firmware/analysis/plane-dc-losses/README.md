# Plane DC I²R loss — nitride-nano buck/boost power pours

Answers: **how much power does the PCB copper itself burn in DC resistance on the
buck/boost power path, on a 2 oz / 6-layer board?**

Plane copper only. Component resistance (FET R<sub>ds(on)</sub>, the 2 mΩ shunts,
the 8 mΩ input shunt, inductor DCR, ferrites, fuse) is deliberately excluded —
this measures the board, not the parts.

## Result

Total DC copper loss in the power pours, at 2 oz / 20 °C:

| Operating point | Plane copper loss |
|---|---|
| 48→5 V, 20 A | 551 mW |
| 20→5 V, 20 A | **566 mW** (worst) |
| 48→18 V, 18 A | 476 mW |
| 48→14.5 V, 14.5 A | 302 mW |
| 12→48 V, 5 A | 564 mW |
| 20→48 V, 5 A | 216 mW |
| 24→60 V, 4 A | 149 mW |
| 4-switch 24→24 V, 10 A | 387 mW |
| USB-C 48→24 V, 5 A | 49 mW |

So the board's own copper costs **~0.05–0.57 W**, which is **~3 %** of the
recorded FET loss (9.61 W at 48→14.5 V/14.5 A in `PCB/losses.txt`). It is not the
dominant loss, but it is not negligible either.

Where it goes:

- Worst net is **`Net-(D3-K)`** (power-stage output): 0.40 mΩ → 161 mW at 20 A,
  two thirds of it on `F.Cu` alone.
- The switching-node pours `D1-A`, `L1-Pad1`, `D2-A` are next (0.18–0.22 mΩ each,
  ~72–90 mW at 20 A).
- **Vias carry 15–25 %** of the loss on those nets — the pours are stitched, but
  the barrels are the narrowest part of the current path.
- `+VBUS` (USB-C input) is a **single-layer** pour: 1.56 mΩ, ~100× the XT90 input
  rail. Fine at 5 A (39 mW) but it is the one rail with real DC drop.
- GND return: 0.230 mΩ input side, 0.176 mΩ output side (~71 mW + 69 mW at 20 A).


## Quick start

```bash
# from this directory
.venv/bin/python plane_dc_loss.py --list-nets          # copper area per net per layer
.venv/bin/python plane_dc_loss.py                      # full run: report.md + maps
.venv/bin/python plane_dc_loss.py --nets "Net-(D3-K)" --pitch 0.05
.venv/bin/python plane_dc_loss.py --converge "Net-(L1-Pad1)"   # mesh convergence study
.venv/bin/python gerber_area_check.py                  # independent geometry check
```

The venv is `python3 -m venv --system-site-packages` plus `scipy`, so the system
`pcbnew`, `numpy` and `shapely` stay visible.

By default the tool materialises git revision **`d705578` ("Rev2 ordered")** — the
revision that was fabricated — via `git show`, writes it to a temp file and
analyses that. Use `--no-git-rev --board <path>` to analyse a working tree instead.

## Files

| File | What |
|---|---|
| `plane_dc_loss.py` | the analysis tool (extraction → mesh → solve → report) |
| `gerber_area_check.py` | independent check: parse the fab Gerbers, compare copper area |
| `report.md` | generated results |
| `results.json` | machine-readable R<sub>eff</sub> and per-layer/per-type breakdown |
| `maps/*.svg` | per-net per-layer current-density maps (log scale) |

## Method

For each net:

1. **Extract** every copper item of that net — filled zone polygons (holes
   preserved), track segments, arcs, pad copper, via pads — into shapely
   geometry, per layer.
2. **Rasterise** onto a uniform grid (default 0.05 mm, 3×3 subcell coverage so
   partially-covered edge cells get a fractional conductance).
3. **Build a resistor network**:
   - in-plane: `g = (1/Rs)·min(cov_i, cov_j)` between adjacent cells,
     `Rs = ρ(T)/t`;
   - out-of-plane: each via is a barrel resistor coupling **every copper layer
     along its barrel**, including layers where this net has no pour (skipping
     those silently splits a net in two and gives garbage);
   - terminals: each terminal group is shorted to a bus node with a stiff
     conductance.
4. **Reduce to the connected component(s) touching the terminals** and report
   anything else as floating copper (it carries no current).
5. **Solve** `L·V = I` for a 1 A injection between the terminal buses — sparse
   direct solve (SuperLU) for ≤350 k nodes, CG above that.
6. **Loss** `P = Σ g·ΔV²`, cross-checked against `P = I·ΔV`. Because the network
   is linear, every operating point is then just `P = I²R`.

Thermal-relief spokes, clearances and antipads need no separate model: they are
already in the filled polygons that KiCad stores, so the mesh sees the real
constrictions at the pads.

## Constants and assumptions

| Item | Value | Where to change |
|---|---|---|
| Copper thickness | 70 µm (2 oz) on all 6 layers | `--thickness` (µm) |
| Copper resistivity | 1.72e-8 Ω·m at 20 °C, +0.393 %/°C | `RHO20`, `ALPHA_CU` |
| Temperature | 20 °C | `--temp` |
| Via | 0.6/0.3 mm, 25 µm barrel plating | `VIA_DRILL`, `VIA_PLATING` |
| Board thickness | 1.6 mm | `BOARD_THICKNESS` |
| Efficiency | η = 0.95 | per scenario, `SCENARIOS` |
| Grid | 0.05 mm × 3×3 subcell; GND ×2.5 coarser | `--pitch`, `--subcell` |

Inductor current is `IL = Iout` in clean buck, `IL = Iin` in clean boost, and
`Iin = Vout·Iout/(Vin·η)`. In the 4-switch transition band `IL` can approach
`2·Iout`; scenario S8 models that as the worst case.

**Note:** the `.kicad_pro` stackup still says 0.035 mm (1 oz) on every layer while
the board was ordered at 2 oz. This tool uses 2 oz. Re-run with
`--thickness 35` to see the 1 oz numbers.

## Nets in scope

`VPP` (XT90 input), `+VBUS` (USB-C input), `/Power-Sense/+VBUS_SENSED`,
`/Converter/PWR_UNREG_IN`, `Net-(U1-VIN)`, `Net-(D1-A)` (SW1), `Net-(L1-Pad1)`
(across the 2 mΩ shunt), `Net-(D2-A)` (SW2), `Net-(D3-K)` (power-stage output),
`Net-(D20-A)`, `Net-(FB5-Pad1)`, `/Converter/PWR_REG_OUT`, and `GND`.

`VPP` only conducts on the XT90 path, `+VBUS` only on the USB-C path; the report
applies the right one per scenario.

Excluded: the logic rails (`+3V3` etc.), AC/skin/proximity effects at 1 MHz, and
component internal resistance.

## Validation

| Check | Result |
|---|---|
| Mesh convergence, `Net-(L1-Pad1)` | R stable within ±1 % from 0.2 mm to 0.05 mm |
| Mesh convergence, `GND` | Iin 0.2235→0.2298 mΩ, Iout 0.1736→0.1758 mΩ from 0.25→0.125 mm (<1 %) |
| Energy balance `Σg·ΔV²` vs `I·ΔV` | exact to solver tolerance on every net |
| Bar-model sanity `Rs·ℓ²/A` | FEM is 1–4.3× the naive bar model, as expected for constricted pours |
| Independent Gerber area check | all 6 layers agree with the fab Gerbers within 0.5–2.7 % |

`gerber_area_check.py` parses the production Gerbers with its own RS-274X reader
(regions, aperture macros incl. KiCad `FreePoly` outline/rotation, arcs) and
compares against pcbnew's copper area — a completely separate code path from the
extraction used by the loss tool.

## Limitations

- DC only. The switch-node pours (`D1-A`, `D2-A`, `D3-K`) physically carry a
  pulsating current; using the average current understates their true ohmic loss.
  An AC/RMS pass would be the next step.
- The copper thickness is taken as uniform at the ordered 2 oz; outer layers in
  production are typically 2 oz + plating, so outer-layer loss is slightly
  pessimistic here.
- GND is a board-wide plane modelled with converter terminals only; unrelated
  logic return currents are not injected.
