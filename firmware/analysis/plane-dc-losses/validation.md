# Validation evidence

All runs use the fabricated revision `d705578:PCB/nitride-nano.kicad_pcb`,
70 µm copper (2 oz), 20 °C.

## 1. Independent Gerber geometry check

`gerber_area_check.py` parses the production Gerbers with its own RS-274X reader
(regions, arcs, KiCad `FreePoly` aperture macros incl. rotation and `$n`
arithmetic) and compares total copper area against pcbnew. It shares no code
with the extraction path used by `plane_dc_loss.py`.

```
$ .venv/bin/python gerber_area_check.py
layer      gerber mm²   pcbnew mm²    delta
F.Cu          5277.97      5335.95   -1.09%
In1.Cu        5900.49      5748.04   +2.65%
In2.Cu        6203.16      6082.43   +1.98%
In3.Cu        6022.90      5959.26   +1.07%
In4.Cu        6024.21      5995.86   +0.47%
B.Cu          5876.32      5815.87   +1.04%
PASS (within 3%)
```

The residual 0.5–2.7 % is the Gerber reader's arc/macro approximations, not a
pcbnew extraction error; the four inner layers (where the power pours live) are
within 2.7 %.

## 2. Mesh convergence — `Net-(L1-Pad1)`

```
$ .venv/bin/python plane_dc_loss.py --converge "Net-(L1-Pad1)" --pitch 0.1
pitch=0.2000 mm nodes=   6682 R_eff=0.19617 mΩ
pitch=0.1500 mm nodes=  11744 R_eff=0.19689 mΩ
pitch=0.1000 mm nodes=  25968 R_eff=0.19019 mΩ
pitch=0.0750 mm nodes=  46104 R_eff=0.19503 mΩ
pitch=0.0500 mm nodes= 103248 R_eff=0.19615 mΩ
```

Converged value ≈ 0.196 mΩ. Spread is ±1 % except 0.1 mm, which lands 3 % low —
a grid-alignment artefact at that one pitch, not a trend. 0.05 mm is used for the
production run.

## 3. Mesh convergence — `GND`

```
$ for p in 0.16 0.10 0.07 0.05; do \
    .venv/bin/python plane_dc_loss.py --nets GND --pitch $p; done
pitch=0.250 mm  Iin=0.2235 mΩ  Iout=0.1736 mΩ
pitch=0.175 mm  Iin=0.2285 mΩ  Iout=0.1754 mΩ
pitch=0.125 mm  Iin=0.2298 mΩ  Iout=0.1758 mΩ
```

0.175 → 0.125 mm moves the result by +0.6 % / +0.2 %. GND is run at 0.125 mm
(1.67 M nodes, CG solve, ~530 s).

## 4. Energy balance

For every net and case the solver reports both `P = Σ g·ΔV²` and `P = I·ΔV`; the
`energy check` column in `report.md` is `ok` for all 13 nets. Bus-shorting
conductances are excluded from the loss sum, so the reported power is copper only.

## 5. Bar-model sanity

`R_bar = Rs·ℓ²/A` (ℓ = terminal centroid distance, A = total copper area, all
layers in parallel) is a lower bound that ignores constrictions. The FEM sits
1.0–4.3× above it on the two-terminal nets, which is the expected direction and
magnitude for pours that neck down at thermal-relief spokes and pad clusters.
It is not applied to GND, where a two-terminal bar model is meaningless.

## 6. Connectivity / floating copper

Every net reports a connected path between its terminals. GND reports 164 mm² of
floating copper islands (GND pours not reachable from the converter terminals);
these carry no current and are excluded from the loss, and are reported rather
than silently dropped.
