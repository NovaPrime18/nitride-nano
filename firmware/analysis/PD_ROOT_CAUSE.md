# Why EPR "doesn't work" on nitride-nano — root-cause analysis

_Fresh look, 2026-09-26. Supersedes the running commentary in `PD_EPR_PPS_NOTES.md`;
the evidence there is still valid, this file states the conclusion._

## Verdict first

**It is not a defect inside the TPS26750 or the STM32.** The PD silicon and the
CC/PD link do their job. Three independent proofs:

1. **The 240 W source really does deliver 48 V to this board.** In
   `PD-Captures/new/pd-nitride-nano(240w)-NEW.sqlite` VBUS reaches **47.97 V**
   (t≈4.12 s) and the firmware log from the same source shows
   `PP_EXT=3 … vin=48000`. A chip that can hold a 48 V/5 A EPR contract is not
   broken.
2. **Every symptom is documented TPS26750 behaviour.** "EPR-capable controller
   keeps requesting 20 V" is exactly TI **SDAA265 §5.2** (`AutoComputeSinkMaxVoltage
   = 0` + a host `AutoNegMaxVoltage` ⇒ never accepts > 20 V), and the
   re-evaluation rules are §6.4. The controller is obeying the register we wrote.
3. **The reference firmware runs the *same* silicon with a nearly identical
   driver and only "works" because it does less.** I pulled the upstream driver
   (`theohg/tps26750_multiplatform`, submodule of `theohg/PD240W`) and diffed the
   register strategy — see below.

The hard part is not the chip. It is that **the firmware and the controller are
both driving the same state machine**, and the board's own logic rail hangs off
the PD rail.

---

## What the reference actually does differently

The upstream driver (`src/tps26750.cpp`, `modifySinkRegister`) is the source our
Rust `modify_sink_register` was ported from. Two things it does **not** have:

* **No "force EPR" host window.** The reference has only `requestFixedProfile`,
  `requestPPSProfile`, `requestAVSProfile`. All of them go through
  `modifySinkRegister`, which **always sets `NoCapabilityMismatch = 1`** and never
  programs `ANSinkCapMismatchPower`.
* **No interference with autonomous EPR entry.** `PdManager::negotiateStartupContract`
  defaults to `StartupContractMode::HIGHEST_VOLTAGE`, whose whole body is:

  ```c
  // TPS26750 automatically negotiates highest voltage due to its EEPROM config.
  // We don't interfere. Doing so breaks autonomous EPR entry sequences.
  LOG_INFO("Startup negotiation: Highest voltage - letting TPS26750 auto-negotiate");
  return false;
  ```

  It also *waits* for EPR PDOs to appear before falling back
  (`_startup_restore_waiting_for_epr`), instead of re-requesting a low rail.
* It implements a **staged EPR→SPR exit** (`logic/pd_diagnostics.h`,
  `needsEprExit`/`findAvsSafeVoltage`, 3 steps: AVS step-down → 5 V → target)
  because "a direct request reboots some chargers".

nitride-nano added exactly the thing the reference refuses to do.

### The divergent code

`src/drivers/tps26750.rs` → `write_epr_force()` (used by
`request_fixed_epr_profile` / `request_epr_rail_fixed`, i.e. MAX and every EPR
preset) does:

```rust
buf[0] &= !((1 << 6) | (1 << 5) | (1 << 4) | (1 << 2) | (1 << 3));
//        AutoDisable  AutoComputeMax  AutoComputeMin  AutoComputeMinPwr  NoCapMismatch
...
let mismatch_w: u16 = 560;      // ANSinkCapMismatchPower = 140 W
```

Consequences, all documented:

* Clearing **bit 5 (`AutoComputeSinkMaxVoltage`)** hands the voltage range to the
  host. Per SDAA265 §4.1 the controller only *caps SPR at 20 V and then performs
  the EPR entry handshake itself* while that bit is **set**. With it clear, the
  controller has no reason to escalate — it ranks the SPR set and 20 V/5 A wins.
  This is SDAA265 §5.2 verbatim.
* Clearing **bit 128 (`EPR AVS Enable Sink Mode`)** removes the other trigger the
  driver's own comment (`request_epr_source_caps`) names as required for
  autonomous EPR entry.
* Clearing **bit 3 (`NoCapabilityMismatch`)** and setting the mismatch power to
  140 W makes the controller assert the capability-mismatch bit on every SPR
  contract (100 W < 140 W). It keeps the path on, but the contract is now a
  mismatch contract, not a clean one.
* The result — auto-negotiated 48 V knocked down to a host window that can only
  resolve to 20 V — is a **direct EPR→SPR transition**, the exact thing the
  reference guards with its 3-step exit.

So "MAX works on the Anker but not the 240 W" is not random: on the Anker the
autonomous EEPROM configuration had already entered EPR, and the Anker tolerates
the churn; on the 240 W the host window lands at 20 V and the source does not
re-offer 48 V after the abrupt drop.

---

## The second problem: the board dies because the MCU rides the PD rail

The captures and the netlist agree:

* In the working `180W-weird-DCDC` capture VBUS goes 5 V → 20 V → **24.9 V**,
  dips to 12 V at t≈5.0 s, then is **re-requested back to 24.8 V at t≈12.6 s and
  held to the end**. The "unsafeguarded" DC-DC simply rides through the
  transition.
* In the 240 W capture VBUS goes 5 V → 20 V → **47.97 V** (t≈4.0 s), then a
  **PPS request 27 ms later** (t≈4.15 s, `RDO=0x63c4b03c`, PDO `6432a4c1`,
  served by PPS because there is no 12 V fixed PDO) drops it to 12 V, and it
  never returns above 20 V.
* Netlist (`analysis/nitride.net`):
  `U11 = LMR16006X` makes `+3V3` (the MCU/OLED/logic rail) and its **input is
  `Net-(JP13-C)`**, i.e. `VPP` (via `JP13` 1–2) — which, on USB-C power, *is the
  PD contract rail*. The high-voltage path is
  `+VBUS → Q10 (LTC7004, U8) → +VBUS_SENSED → F1 → VPP`, gated by
  `POWER_PATH_EN` (U4.20) through `Q8/Q9/U7`. The EPR adapter is
  `U3 = TPD4S480` with `Q6`/`JP2`/`JP3` and `EPR_EN` from TPS26750 GPIO2.

Consequence: **any** PD event that dips VBUS below the LMR16006's input minimum,
or opens `PP_EXT`, takes the MCU down with it — hence the `SwdDpError`/reset
right after an EPR request. The converter being parked (`board::EPR_SETTLE_MS`)
does not help, because the load is not the cause: the 3V3 rail itself is going
away. The prior session already ruled out TPS26750 GPIO6 (`GPsh` hold logged
`true` and the MCU still died).

This is a **board-level robustness issue**, not a chip defect — but it is real,
and it is why a firmware bug turns into "the board shuts down" instead of "the
rail is wrong".

---

## What to do, in order

### Firmware — **implemented 2026-09-26**

1. **The `write_epr_force` path is gone.** Above-SPR rails now go through the
   EEPROM's own winning recipe: `request_avs_profile` writes **auto-compute on +
   `avs_en` + AVS output = the target** (the config that negotiates 48 V on the
   240 W), and `choose()` prefers it whenever the source advertises an EPR AVS
   APDO. Sources with no AVS APDO (the Anker) use the fixed-EPR window
   (`request_fixed_epr_rail`); MAX with no AVS APDO uses
   `restore_autonegotiate`. A first bench run showed a host **fixed**-EPR window
   still landing at 20 V, which is why the AVS/auto-compute path became the
   default — see `PD_EPR_PPS_NOTES.md` §"the real EPR-selection rule".
2. **No re-request while an EPR contract is live.** EPR mode entry no longer
   arms a re-plan, the boot path seeds `requested_rail_mv` from the live
   contract, and MAX leaves an already-live EPR rail alone.
3. **Staged EPR→SPR exit** added (`EprExitStep`: AVS floor → 5 V → target,
   2 s per step) so a confirm cannot command a direct high→low transition.
4. **The PPS-bit edge trigger is gone.** A second field run showed a MAX write
   that was byte-for-byte the EEPROM policy (`b0=0x3e`, `avs_en=1`) still landing
   at 20 V, with the controller rewriting `AutoNegMaxVoltage` 51000 → 20000 —
   i.e. it had dropped out of EPR. The cause was the `PPSEnableSinkMode` toggle
   used as the re-evaluation trigger: it enables PPS, which outranks EPR and caps
   at 21 V (TRM §6.3). The TRM §2.3 documents the real trigger — change 0x37 and
   issue **`GSrC`** — which is what the firmware now does after every request.
   This also removes the need to power-cycle the TPS26750 to clear a stale host
   window, which matters on this board because the controller cannot be
   power-cycled while the debugger is attached.
5. `ESrC` remains only as an EPR-capability probe.

Remaining to verify on the bench (the firmware cannot prove these):

* the 240 W holding its own 48 V after a TPS26750 power-cycle with no UI input;
* MAX on the Anker still reaching 28 V;
* the exit sequence log (`PD: EPR exit step 1/3 … 3/3`) on an EPR→SPR confirm.


### Bench (to close the hardware question)

1. **Isolate silicon from firmware:** power-cycle the TPS26750 (unplug source and
   board supply so `0x37` reloads from EEPROM), reconnect the 240 W, touch
   nothing. If 48 V holds with `PP_EXT=3`, the chip and PD link are proven good
   and everything else is firmware policy.
2. **Isolate board from source:** at 48 V with the converter parked, scope
   `+VBUS`, `+VBUS_LV` (TP9 — should be ≈0.42×VBUS, never 0), `POWER_PATH_EN`
   (TP14), `SWITCH_EN` (TP28), `LTC_GATE` (TP25), `+3V3` (TP65). Whichever node
   collapses names the culprit.
3. If the MCU rail proves to be the casualty, the rev3 ECO is to stop feeding
   `U11` from the switched PD rail (or hold it up) so a PD transition cannot
   reset the MCU.

### Config (TI GUI)

* `0x28` bits 26–24 `VBUS Sink UVP Trip HV` is `1h` = 10 %. Widening it to
  `7h` (50 %) is a cheap test for the "source delivers 20 V while a 28 V EPR
  contract is pending → controller opens the path" behaviour seen in the notes.

---

## One-line answer to "is it the chip?"

No. The TPS26750 reaches 48 V on the 240 W and 24.9 V on the 180 W, and its
"stuck at 20 V" behaviour is exactly what TI documents for the register state the
firmware writes. The reference project succeeds because it *does not write that
state* — it lets the controller's EEPROM autonegotiation enter EPR and only then
issues narrow, mismatch-tolerant requests. The remaining fragility
(the board resetting) is the MCU's 3V3 buck being downstream of the PD power
path, which a staged EPR exit plus the rev3 rail change will fix.
