# TPS26750 PD config — EPR + PPS

`config_240W_TPS26750_F8091159_epr_pps.json` is the corrected USBCPD Application
Customization Tool project for the nitride-nano sink. Regenerate the full-flash C
from it and flash it to the TPS26750's EEPROM with the firmware's **EEPROM FLASH**
screen; the config in `src/drivers/config_TPS26750_F8091159_fullFlash.c` is a
generated artifact, not something to hand-edit.

## Sink PDOs (`0x33` TX_SINK_CAPS)

| Slot | Type | Setting |
|------|------|---------|
| 1 | Fixed | 5 V / 3 A |
| 2 | Fixed | 9 V / 3 A |
| 3 | Fixed | 12 V / 3 A |
| 4 | Fixed | 15 V / 3 A |
| 5 | Fixed | 20 V / 5 A |
| 6 | SPR PPS APDO | 3.3–21 V / 3 A |
| 8 | Fixed EPR | 28 V / 5 A |
| 9 | Fixed EPR | 36 V / 5 A |
| 10 | Fixed EPR | 48 V / 5 A |
| 11 | EPR AVS APDO | 15–48 V / 240 W |

Header byte `0x26` = 6 valid SPR + 4 valid EPR PDOs (slots 7, 12, 13 empty).

Defects this fixes versus the attached JSON:

* header claimed **6 SPR + 6 EPR** with only 4 + 4 PDOs populated, so the
  Sink_Capabilities message was malformed;
* the 15–48 V APDO had APDO-type bits `00` (PPS) instead of `01` (EPR AVS), at
  register bits 356:357 — the "EPR Adjustable Voltage Supply" field TI says must
  read 1;
* no 12 V PDO and no PPS APDO.

## Autonegotiate Sink (`0x37`)

* `NoCapabilityMismatch = 1`, auto-disable off: the sink path stays on for any
  source instead of refusing low-power ones.
* Auto-computed min/max voltage, max current 5 A.
* PPS enabled (20 V / 3 A default); EPR AVS enabled (48 V / 5 A).
* The firmware overrides these fields on every rail request, so the defaults only
  cover the window between plug-in and the first host request.

## Importing into the GUI (this is where it usually goes wrong)

1. Select **TPS26750** on the first page — not the gear icon.
2. Turn on **Advanced Configuration** *before* importing.
3. **Import Settings**, and tick the box to keep Advanced Configuration changes.
4. After importing, do **not** click "Standard Configuration" or the gear: that
   re-derives the registers from `questionnaire.answers` and discards the
   Advanced edits (including `0x33`/`0x37`).
5. Verify in Advanced view that `0x33` shows the six SPR + four EPR PDOs above,
   then export a new **Full Flash** binary and rebuild the firmware.

If the GUI proves unusable, the config already in the tree has the 28/36/48 V EPR
PDOs (header `0x24`, 4 SPR + 4 EPR) but no PPS/12 V. It plus the firmware change
below gives EPR without needing the GUI at all.

## After flashing the EEPROM: power-cycle the TPS26750

`EepromLoader` writes and verifies the CAT24C512 over I2C3 but **does not reset
the TPS26750** — it reads its application config only at power-up. Until the
controller is power-cycled it keeps running the previously loaded image, so an
EEPROM flash alone changes nothing (this is very likely why the 28 V preset
looked inert). Unplug the USB-C source and the board supply for a few seconds
after flashing.

The firmware now reads the live image back on every `TPS26750 present` and logs:

```
TPS config 0x33: hdr=0x26 SPR=6 EPR=4
TPS config 0x37: b0=0x3e maxV=48000 mV maxI=5000 mA
```

If that header is not `0x26`, the controller is still running an old image.

## Diagnosing a preset that does not engage

The PD manager now logs the whole path:

| Log line | Meaning |
|---|---|
| `PD confirm: mode=manual preset=3 caps=.. choice=28000 mV epr_ok=true` | the UI confirm resolved to a 28 V request |
| `PD confirm: ... choice=20000 mV epr_ok=false` | EPR is latched off, so the preset falls back to 20 V |
| `PD source caps: SPR=6 EPR=2 last_epr=true` | the controller has entered EPR and can see EPR PDOs |
| `PD request: rail=28000 mV` | the request was actually written to `0x37` and `GSrC` issued |
| `PD request failed for rail=28000 mV` | the I2C write to `0x37` failed |
| `PD contract: 28000 mV @ 5000 mA` | the source confirmed the new contract |
| `EPR unavailable: 28000 mV request settled at 20000 mV` | EPR entry failed; SPR fallback latched |

If `PD confirm` shows `choice=28000` and `PD request` appears but no
`PD contract: 28000`, EPR entry is failing at the protocol level (cable e-marker,
source, or the `0x33`/`0x37` image).

## Root cause of "28 V preset does nothing"

Field log showed the loaded image was correct (`MODE=APP`, `0x33 hdr=0x26`) but:

```
PD confirm: mode=manual preset=3 caps=7 choice=20000 mV epr_ok=true
```

Preset 3 (28 V) resolved to a **20 V** request, so no EPR request was ever sent.
`auto_track::candidates()` decided whether the source already advertised EPR
with `voltage_mv > SPR_MAX_MV`. A **PPS APDO stores its maximum voltage in
`voltage_mv`, and PPS reaches 21 V**, so any PPS-capable source looked
EPR-capable and the declared 28/36/48 V rails were never injected. The check now
requires a *fixed* PDO above 20 V:

```rust
let advertised_epr = caps
    .iter()
    .any(|c| is_fixed(c) && c.voltage_mv > board::SPR_MAX_MV);
```

With a 7-SPR PPS source (EPR not yet entered) the presets now resolve to
12/15/20/28/36/48 V and Auto at 48 V/5 A resolves to 48 V.

## EPR mode entry — the remaining blocker

The firmware request path is correct, but the controller never enters EPR mode
(`PD source caps: SPR=7 EPR=0`, `last_epr=false`). Two request styles were tried:

* **Explicit >20 V window** (min 26.6 V, max 29.4 V). Per SDAA265 §5.3,
  "Whenever the AutoNegMaxVoltage or AutoNegMinVoltage is not met, the 5V PDO is
  chosen as a default" — before EPR mode entry the source only offers SPR PDOs
  (≤20 V), so nothing is in range and the controller requests 5 V.
* **`AutoComputeSinkMaxVoltage = 1`**. Per SDAA265 §3.5 this computes the max
  from the source PDOs *currently on offer*, which pre-entry is 20 V, so the
  controller settles at 20 V and still does not enter EPR.

Per SDAA265 §5.2 an EPR-capable controller "can successfully enter into EPR mode
with an EPR-capable USB-PD Source", so mode entry is *upstream* of these
settings. USB-PD 3.1 gates it on:

| Where | Bit | Meaning |
|---|---|---|
| Source Fixed 5 V PDO | B23 | EPR Mode Capable |
| Sink RDO | B22 | EPR Mode Capable (the controller sets it from the config) |
| Cable e-marker VDO | B17 | EPR Mode Capable |

The sink's own Fixed PDO has **no** EPR bit (Table 6-16, B22:20 reserved), so
nothing in `0x33` is missing there, and no other config register gates EPR:
`0x42` PD3 Config has no EPR enable, and `0x5C` IO Config byte 38 = `0x5c`
already maps GPIO2 to event 92 — the TPD4S480 `EPR_EN` net
(`/USB-PD/EPR_EN` = TPS26750 pin 7 GPIO2 → TPD4S480 pin 16).

**Event 92 is `VBUS_SENSE_DIVIDER`** (TRM Table 6-6): *"GPIO is enabled whenever the
PD controller is transitioning into EPR mode. This GPIO will enable the external
VBUS divider."* It is therefore a **consequence** of EPR entry, not its trigger —
the GPIO2→92 mapping cannot be why entry never happens. (Event 142,
`EPR_DISCHARGE_EVENT`, is for an external discharge circuit on EPR exit and is not
required for entry either.)

Field result with a known-good EPR source and cable:

```
source 5V PDO=176263468 epr_capable=true    # 0x0A81912C, B23 = 1
PD source caps: SPR=6 EPR=0 last_epr=false
```

The source is genuinely EPR-capable, so the blocker is the **sink**: the TPS26750
never sends `EPR_Mode(Enter)`. The contract log now also prints the active RDO's
EPR bit:

```
PD contract: 20000 mV @ 5000 mA rdo_epr=false
```

Per spec §6.4.10.1 the sink sets RDO B22 and sends `EPR_Mode(Enter)` itself. The
field result is:

```
PD contract: 12000 mV @ 3000 mA rdo_epr=true
PD request: rail=28000 mV
PD contract: 5000 mV @ 3000 mA rdo_epr=true
```

So `rdo_epr=true` on **every** contract — the controller is already attempting
EPR mode entry, its RDO satisfies §6.4.10.1 step 2a, and the source's 5 V PDO
satisfies step 2b. The entry handshake is what fails; the 5 V contract is then
SDAA265 §5.3's fallback because the [26.6, 29.4] V window no longer matches any
SPR PDO once the EPR attempt aborts.

`Enter Failed` codes the source can send (spec Table 6-50): `0x01` cable not EPR
capable, `0x02` source failed to become VCONN source, `0x03` RDO bit missing
(ruled out), `0x04` source unable at this time, `0x05` PDO bit missing (ruled
out). The TPS26750 does not expose the received code, so reading it needs a
CC-line protocol capture.

With both source and cable proven good against another EPR sink, the remaining
suspects are the `EPR_Mode(Enter)` payload (its Data field is the EPR Sink
Operational PDP), the PD-firmware version in the generated image, or the
TPD4S480 `EPR_EN` / `EPR_BLK_G` transition itself (see the E2E thread where the
`EPR_BLK_G` gate drive collapsed at 23 V and the TPD4S480 FAULT asserted).

Remaining things to try, in order:

1. Regenerate the config **without the EPR AVS APDO** (fixed 28/36/48 only). The
   GUI's `EPR_Sink_AVS_Controls` tab is documented as broken on E2E and the AVS
   APDO is the only unusual entry in `0x33`.
2. Power-cycle the TPS26750 and confirm the boot read-back shows
   `TPS config 0x37: b0=0x3e` (the config value) rather than the firmware's
   previous write — a persistent `b0=0x0a` means the image never reloaded.
3. Confirm the GUI build is 2.0.1+ and regenerate the Full Flash from it: TI
   shipped an EPR-related PD-firmware fix that only applies when a new binary
   image is generated.

## Encoder

`Qei::new` uses X4 decoding, but the fitted encoder produces a detent every
**two** counts (measured: one detent used to fire two `EncTurn` events; dividing
by 4 then made the knob half-speed, and 2 is correct). `board::ENCODER_COUNTS_PER_DETENT`
is 2 and raw counts are divided by it with the remainder carried between polls.

## Firmware changes

* `src/board.rs` — `SPR_MAX_MV`, `EPR_RAILS_MV`, `EPR_RAIL_CURRENT_MA`,
  `PPS_MIN_MV`/`PPS_MAX_MV`/`PPS_FIXED_PREFER_MV`.
* `src/pd/auto_track.rs` — injects the declared 28/36/48 V EPR rails as
  candidates whenever SPR cannot deliver the setpoint (the source's EPR PDOs are
  invisible until EPR mode has been entered); serves a preset that no fixed PDO
  matches from the source's PPS APDO.
* `src/pd/manager.rs` — issues PPS vs fixed requests, and latches EPR off after
  one request settles back at an SPR voltage, so a non-EPR source falls back to
  20 V instead of sitting on a request it can never satisfy.
* `src/drivers/tps26750.rs` — the PPSEnableSinkMode toggle is now applied only to
  PPS requests; for fixed SPR/EPR requests it used to briefly assert PPS with
  whatever PPS voltage `0x37` held and could start a spurious PPS negotiation.
  Also exposes `source_5v_pdo()` and `request_epr_entry()` (the latter currently
  unused: `AutoComputeSinkMaxVoltage` cannot reach EPR, see above).

The rail chooser is pure logic and is exercised by
`.tmp_research/autotrack_test/` (host harness that includes the real
`auto_track.rs`).

## PD analyzer captures (`PD-Captures/`)

Decoded from the two `.sqlite` captures (6-byte record prefix, then the PD header +
data objects, little-endian).

**`pd-nitride-nano.sqlite`** — the source refuses:

```
3.852  EPR_Mode  action=1 (Enter)          sink -> source  data=0x00
3.859  EPR_Mode  action=2 (Enter Ack)      source -> sink
3.909  EPR_Mode  action=4 (Enter Failed)   source -> sink  data=0x01
```

Data `0x01` is **"Cable not EPR capable"** (spec Table 6-50) — i.e. the source's
step-6 `Discover Identity` on SOP' read the cable e-marker and rejected it
(cable VDO must declare 50 V, 5 A and EPR Mode Capable).

**`pd-nitride-nano(anker737).sqlite`** — the Anker 737 accepts entry:

```
4.335  EPR_Mode  action=1 (Enter)          sink -> source  data=0x00
4.341  EPR_Mode  action=2 (Enter Ack)      source -> sink
4.468  EPR_Mode  action=3 (Enter Succeeded) source -> sink
4.606  VBUS 20.23 -> 12.22 V, fresh SPR Source_Caps
```

EPR mode entry **succeeds** here, but `EPR_Source_Capabilities` never arrives —
neither capture contains a single 28/36/48 V PDO — and the port drops back to
SPR 12 V ~140 ms later, then emits a continuous Discover-Identity retry storm
(two per ~400 ms for 20 s).

So the controller and its `EPR_Mode(Enter)` are correct; the failure is in the
**cable e-marker read / VCONN path** after Enter Ack. `EPR_EN` never rising is
consistent: event 92 drives the VBUS divider for the 20→28 V *transition*, which
is never reached.

## A/B against a working EPR sink (`PD-Captures/pd-soldering.sqlite`)

The soldering iron completes EPR with the same source and cable: `EPR_Mode`
Enter (4.010) → Ack (4.017) → **Succeeded** (4.043), and VBUS reaches **28.14 V**.

The board's trace is structurally identical through mode entry — and the source
offers the 28 V PDO (`f4c10800`) to **both**:

| | after `EPR_Mode Succeeded` | next exchange | VBUS |
|---|---|---|---|
| iron | `b1a3 2088a4c1 f4c10800` | `892a f4d14783 f4c10800` → 28 V/5 A | **28.14 V** |
| board | `b1ab 20880000 f4c10800` | `8926 0000c033 2cc10300` → 12 V/3 A | **12.22 V** |

So the source is offering 28 V in both cases, and the board's link is the one
that ends up referencing the **12 V** object. The differentiator is the board's
own host override on `0x37`: the manual preset defaults to 12 V, so the
controller is told max ≈12.6 V (`TPS config 0x37: b0=0x0a maxV=12600`) and asks
for 12 V even after entering EPR. The iron does no host override, so it stays in
EPR at 28 V. This also explains `EPR_EN` never rising.

## Boot behaviour: autonegotiate first (the fix)

The A/B capture above showed the board receiving the source's 28 V offer and then
walking itself back to 12 V. Two things in the firmware caused that:

1. At boot the firmware wrote a `0x37` window for the manual default preset
   (12 V), which the controller then requested even after entering EPR.
2. `modify_sink_register()` **cleared `EPR AVS Enable Sink Mode`** on every plain
   fixed/PPS request. That bit is what makes the controller attempt EPR mode
   entry at all, so after the first host write the board could never enter EPR
   again — which is why it only ever did so on the very first negotiation after
   a config load, and why `EPR_EN` never rose.

`PdManager` now, on `TPS26750 present` and on plug, **in Manual mode**:

* calls `Tps26750::restore_autonegotiate()`, which writes back the config's
  intent — auto-computed voltage range, PPS enabled, **EPR AVS enabled** — and
  issues `GSrC`;
* does **not** queue a rail request, so the controller's own negotiation runs and
  it can enter EPR and hold the highest rail.

Only when the user confirms a preset (or Auto-tracking decides on a change) does
the firmware write a specific window. The `epr_seen` re-request is likewise
Auto-only now, so a Manual EPR contract is not clobbered the moment it appears.

Expected on the next capture: the controller enters EPR on its own at plug
(`EPR entered (n EPR PDOs)`), holds it, and a 28 V confirm produces
`PD request: rail=28000 mV` → `PD contract: 28000 mV`.

## Config regression: "Standard Configuration" drops the Advanced PDOs

The image in the tree mid-session had reverted to a GUI **Standard
Configuration** regeneration: `0x33 hdr=0x1c` (SPR 5/9/15/20 + EPR 28/36/48, the
PPS APDO *and* the EPR-AVS APDO gone) with `0x37 b0=0x76` (PPS and EPR-AVS
disabled). Without the EPR-AVS APDO **and** its enable bit the controller never
attempts EPR mode entry, so `EPR entered` never fires — which is exactly what the
last log showed.

`src/drivers/config_TPS26750_F8091159_fullFlash.c` has had `0x33`/`0x37`
restored in place from `config_240W_TPS26750_F8091159_epr_pps.json` (all four TLV
copies, i.e. both the low and high region). A backup of the pre-patch file is at
`config_TPS26750_F8091159_fullFlash.c.bak`.

**Re-exporting from the GUI — especially pressing "Standard Configuration" —
will undo this again.** Use Advanced Configuration and import the JSON, or keep
editing the generated C.

## EPR entry succeeds, then exits — PPS priority

`PD-Captures/pd-nitride-nano(anker737)-NEW.sqlite` (Anker + validated config)
shows the full sequence for the first time:

```
3.363  EPR_Mode action=1 Enter            (sink)
3.369  EPR_Mode action=2 Enter Ack        (source)
3.493  EPR_Mode action=3 Enter Succeeded  (source)   <- EPR entered
3.519  <request carrying a PPS 21 V / 5 A object>
3.551  EPR_Mode action=5 Exit             (sink)     <- and left ~60 ms later
```

So the board **does** enter EPR with the Anker; it then negotiates a <=21 V
**PPS** contract and exits. The TRM is explicit: *"PPS contracts are prioritized
over any other supply type."* `restore_autonegotiate()` enabled PPS, which is
what drove this.

Actions taken:

* the boot path is reverted — `TPS26750 present` / plug now just set
  `negotiate_pending`, so the firmware writes its normal fixed window (which
  clears `PPSEnableSinkMode`);
* `0x33`/`0x37` in the full-flash image now ship with `PPSEnableSinkMode`
  **clear** (`0x37 byte8 = 0x02`), keeping EPR-AVS enabled and the voltage range
  auto-computed.

The firmware still enables PPS explicitly (`request_pps_profile`) when a preset
is actually served from a PPS APDO, so 12 V-via-PPS still works.

## The full-flash image is checksummed — do not hand-edit it

Each config region in `config_TPS26750_F8091159_fullFlash.c` is preceded by a
region header and a 32-bit integrity value:

```
0x880 : 01 00 78 02  <32-bit checksum>  ...config TLVs...
0x4480: 01 00 78 02  <32-bit checksum>  ...same TLVs (second copy)...
```

Every hand-patch of the TLV bytes invalidated that value, and the TPS26750 then
rejected the whole image (its Status register reports Region 0/1 EEPROM errors) —
i.e. "the EEPROM file doesn't work at all". The value is not a plain CRC-32 over
the obvious range, so it cannot be recomputed here.

**Therefore: edit the config in the TI GUI and export a new Full Flash binary;
never patch the generated C by hand.** The pre-patch file has been restored from
`config_TPS26750_F8091159_fullFlash.c.bak`.

To apply the PPS fix in the GUI: Advanced Configuration → **Autonegotiate Sink
(0x37)** → clear **PPS Enable Sink Mode** (keep **EPR AVS Enable Sink Mode** set)
→ export Full Flash. `config_240W_TPS26750_F8091159_epr_pps.json` already has
that bit cleared (`byte8 = 0x02`) for import.

## Bench checks

* Boot: `source 5V PDO=... epr_capable=true|false`. `false` = source not EPR.
* EPR source, high setpoint: `PD request: rail=48000 mV`, and the active contract
  reads 48 V.
* Non-EPR source, high setpoint: one EPR attempt, then `EPR unavailable: ...`
  and a fallback to 20 V.
* PPS source: select the 12 V preset; the contract is a PPS 12 V contract.

## How the working reference (PD240W) reaches EPR

`PD240W-main-Examplecode/` is the same sink topology on the same TPS26750, and
it holds 28 V/36 V/48 V. Its PD driver is the `theohg/tps26750_multiplatform`
submodule (pinned at `5ba57e5`), not the in-tree `src/tps26750.cpp` the Rust port
was originally cut from. The differences that matter:

| Reference does | nitride-nano did |
|---|---|
| Requests every >20 V rail as an **EPR AVS** contract (`requestAVSProfile`, 0x37 bit 128 `EPR AVS Enable Sink Mode` = 1, PPS off) | Requests a **fixed** >20 V window (`requestFixedProfile`), which *clears* the EPR AVS enable bit |
| Sends the `ESrC` 4CC task (`probeEpr()`) to make the controller enter EPR and fetch the source's EPR PDOs | Never sends `ESrC` |
| Keeps **PPS disabled** whenever EPR is wanted (PPS outranks EPR, TRM 6.3) | Config ships PPS disabled, but an earlier `restore_autonegotiate()` enabled PPS and pulled the contract back to a PPS 21 V object, then EPR exit |
| Handles interrupt bit 14 `SOURCE_CAP_RX` (raised on EPR entry *and* exit) and re-reads the PDO list | Only handled bits 3 (plug) and 12 (new contract), so the EPR PDOs were never re-read |
| Refreshes the AVS contract every 7 s (`serviceKeepAlive`) | Issued one fixed request and never refreshed |
| Requests a host min/max window with `AutoComputeSinkMaxVoltage = 0` | Wrote a fixed 26.6–29.4 V window that matches no *visible* SPR PDO |

### Why the fixed >20 V window can never enter EPR

SDAA265 §5.3: *"Whenever the AutoNegMaxVoltage or AutoNegMinVoltage is not met,
the 5 V PDO is chosen as a default."* Before EPR mode entry the source only
advertises SPR PDOs (≤20 V), so a 26.6–29.4 V window discards every PDO and the
controller settles on **5 V**; it does not enter EPR. EPR mode entry is gated on
`AUTO_NEGOTIATE_SINK.EPR AVS Enable Sink Mode` (TRM Table 4-21, bit 128): *"If
this bit is asserted, then the PD controller will attempt to negotiate a EPR AVS
sink contract."* The reference asserts it on every EPR request; the fixed-request
path in the Rust port clears it. SDAA265 §4.1's alternative — leave
`AutoComputeSinkMaxVoltage = 1` so the controller caps SPR at 20 V and then
computes the EPR maximum itself — cannot pick a *specific* rail and was already
shown to need PPS off.

The Anker capture confirms PPS was the second half of the failure: after
`EPR_Mode Enter Succeeded` the sink immediately sent a Request carrying the PPS
APDO (`64 2d a4 c1`) and then `EPR_Mode Exit` ~60 ms later.

## Fix applied (mirror the reference)

* `src/board.rs` — `EPR_AVS_MIN_MV`/`EPR_AVS_MAX_MV` (15–48 V), matching the
  sink config's `0x33` PDO 11 (`f0 96 c0 d3` = EPR AVS 15–48 V / 240 W).
* `src/pd/auto_track.rs` — every above-SPR choice now carries `avs:
  Some(ApdoWindow)`; `PpsWindow` renamed `ApdoWindow`.
* `src/drivers/tps26750.rs` — `TPS_CMD_ESRC` + `request_epr_source_caps()`, and
  `TPS_INT_SOURCE_CAP_RX` (bit 14).
* `src/pd/manager.rs` —
  * EPR entry is a **probe-and-wait** in `poll`: when the chosen rail is
    above-SPR and EPR has not been seen, the manager sends `ESrC` (logged as
    `EPR probe (ESrC) ok=`) and then **holds the 0x37 write + `GSrC` back**
    until the EPR PDOs appear in `RX_SOURCE_CAPS` (or a 2 s timeout). Issuing the
    request immediately after `ESrC` tears down the EPR session the probe just
    established — that was the field failure: `ESrC ok=true` followed at once by
    a 0x37 rewrite put the contract back at 20 V. This mirrors the reference's
    `probeEpr()` + "wait for EPR PDOs" boot sequence;
  * once the EPR PDOs are visible (`epr_seen`), `negotiate()` names the rail as
    an AVS contract on the source's AVS APDO window, or — when the source has
    only fixed EPR PDOs — through `request_fixed_epr_profile()`, which uses
    SDAA265 §4.1's ≥140 W requirement (`ANSinkCapMismatchPower = 140 W`,
    `NoCapabilityMismatch` clear) over an explicit `[15 V, 51 V]` window and the
    PPS-edge trigger. Host-window variants tried earlier and rejected on the
    bench are kept here so they are not retried: narrow window + `avs_en` set
    (GSrC *and* edge) → 5 V; narrow window + `avs_en` clear (edge) → 5 V;
    auto-compute range → 20 V (auto-compute keeps the max at the SPR 20 V even
    while EPR is active, so the EPR PDO is never ranked — hence the explicit
    window here);
  * an EPR request must **not** be triggered with `GSrC` — it re-fetches the
    *SPR* source caps and restarts SPR negotiation, dropping EPR. EPR requests
    re-evaluate with a 2 ms `PPSEnableSinkMode` edge inside
    `modify_sink_register` (`edge=true`), which is how the reference triggers
    every request. `GSrC` is still used for SPR/PPS;
  * `AutoChoice.epr` marks an above-SPR rail so the request keeps EPR alive;
  * bit 14 (`SOURCE_CAP_RX`) is consumed and forces a PDO re-read, which is how
    the entry is detected. Any successful request also sets `caps_dirty` for one
    re-read, and the PDO set is re-read on EPR exit (`epr == 0`) — **while in EPR
    a source advertises only a reduced SPR set** (5/9/12 V here), so the 15 V and
    20 V presets legitimately resolve to 12 V until EPR exits;
  * a 7 s AVS keep-alive re-issues the contract while it stays above 20 V.

Field result worth remembering: `ESrC ok=true` also proves the **source and cable
accept `EPR_Get_Source_Cap`**, so an `EPR_Mode Enter Failed` seen in an earlier
capture was a firmware sequencing artefact, not a cable rejection.

### Re-confirmed: the brownout is the source's EPR transition, not the voltage

| Source | Requested | Result |
|---|---|---|
| Anker powerbank | 28 V | held, `vin=28262 mV` |
| china charger | 28 V | browned out as VBUS crossed ~23 V |
| 240 W charger | 48 V (`maxV=50400`, targeted path) | browned out at EPR |

Two chargers brown out while a powerbank survives **at the same 28 V**, so this
is not a voltage limit and not the request framing — the firmware issues the
correct 28/48 V request in every case. It is the source's VBUS behaviour through
the 20→28/48 V transition (dV/dt, glitch, or a dip) interacting with the board's
input path.

Mitigation added: `maybe_reenable()` now holds the converter **off for
`board::EPR_SETTLE_MS` (800 ms)** after an EPR request before loading it, instead
of enabling as soon as any >20 V contract appears. Loading the converter while
the source is still settling VBUS is a plausible way to dip it hard enough to
reset the MCU, and the window is cheap to widen if 800 ms is not enough.

That did **not** fix the brownout — the MCU still dies within ~100 ms of the
request with the output already parked, so the load is not involved and the 3V3
rail itself is going away. Two firmware-visible suspects, in order:

1. **TPS26750 GPIO6 drops `/LV-Supply/3V3_EN_PD`** across the EPR transition.
   The driver now issues the `GPsh` 4CC task for GPIO6 at boot and after every
   EPR entry (`TPS GPIO6 (3V3_EN_PD) held high: <bool>`), which overrides an
   event-driven GPIO. `JP16` selects whether the 3V3 buck's `~SHDN` follows that
   signal (2–3) or is auto-enabled (1–2).
2. **3V3 buck enable over-voltage** (only if `JP16` is 1–2). `U11` (LMR16006X)
   `~SHDN` = `R43` 100 k from the buck input with `R82` 33 k + `R84` 4.7 k to
   GND, i.e. `EN ≈ 0.274 × Vin` → **7.7 V at 28 V and 13.1 V at 48 V**, above a
   normal logic-input abs max. That would be a hardware ECO (re-divide/clamp the
   SHDN node), not something firmware can fix.

Measure `+3V3` (TP65/J4), `3V3_EN` (TP67) and the buck input (`Net-(JP13-C)`)
at 48 V to tell them apart: TP67 low ⇒ case 1; TP67 high but `+3V3` collapsed ⇒
case 2 (or the buck itself).

**Result: the `GPsh` GPIO6 hold did not help.** The log shows
`TPS GPIO6 (3V3_EN_PD) held high: true` both at boot and right after EPR entry
(7.681), the 48 V request goes out at 7.686, and the MCU still dies before the
0x37 read-back. So GPIO6 is ruled out; the 3V3 collapse is downstream of the
controller. This is a hardware bring-up issue on the input/enable path — the
firmware has no further lever on it.

### Jumper fix, and the power path that drops out on EPR

Moving the 3V3/6V6 buck input jumper (`JP13`) from `VPP` (XT90) to `+VBUS`
fixed the brownout — the board now rides through EPR on the china charger.
Two things are then visible:

* the china charger **never raises VBUS to 28 V** (measured USB-C = 20 V) and
  the request settles at a 20 V/5 V contract;
* the **converter input collapses to ~4.4 V** (`vin=4407 mV`) while USB-C is at
  20 V, i.e. the board's power path opens and the converter loses its input.

The converter input is not `+VBUS_LV`; it comes through the external power path
(`analysis/root.net`):

```
POWER_PATH_EN (U4.20, TPS26750) ─→ Q8 ─→ PP_EN1 ─→ Q9 ─→ PP_EN2 ─┐
                                                                  U7 74LVC1G08 (AND)
SWITCH_EN_MANUAL (U2.41, MCU PB3) ────────────────────────────────┘
   ─→ PP_EN3 ─→ SWITCH_EN ─→ U8 LTC7004 INP ─→ Q10 gate
   ─→ +VBUS_SENSED ─→ R60 (8 mΩ) ─→ /Converter/PWR_UNREG_IN (INA228 + LT8390)
```

`SWITCH_EN` is also pulled by the INA228 `ALERT` through R55; the MCU's
`SWITCH_EN_MANUAL` is only one input of the AND, so it **cannot force the path
on** if the controller drops `POWER_PATH_EN`. `GPsh` cannot help here either —
`POWER_PATH_EN` is a dedicated pin, not a GPIO.

Diagnose by scoping, on the failing charger vs the Anker:
`TP14` (`POWER_PATH_EN`), `TP17` (`PP_EN1`), `TP19` (`PP_EN2`), `TP21`
(`PP_EN3`), `TP28` (`SWITCH_EN`), `TP25` (`LTC_GATE`), plus the INA228 `ALERT`
(U9 pin 3). Whichever node flips is the culprit; if it is `POWER_PATH_EN`, the
power-path behaviour lives in the TPS26750 application config (0x28 Port
Configuration / 0x29 Port Control) and has to be changed in the TI GUI, not in
this firmware.





## EPR 28 V — WORKS, and the shutdown is source-specific

**Confirmed working (Anker powerbank):** after `ESrC ok=true` → `EPR entered
(1 EPR PDOs)` → `PD request: rail=28000 … epr=true` → `b0=0x02
avs_en=false pps_en=false maxV=51000` → `PD contract: 28000 mV @ 5000 mA
rdo_epr=true`, and the converter holds `vin=28262 mV` continuously.

So the firmware recipe is correct: enter EPR with `ESrC`, then request the
**fixed EPR PDO** with a wide host window and the ≥140 W requirement, triggered
by the PPS-bit edge (never `GSrC`). The source in the failing runs advertised
only `SPR=3 EPR=1` and the board browned out as VBUS crossed ~23 V; the Anker
advertises `SPR=7 EPR=1` and survives the same transition. **The shutdown is
therefore a source/power-path interaction, not PD policy** — the TPD4S480 EPR
adapter section below still applies to whichever source trips it.

Known limitation of the current request: the controller picks the
highest-power EPR PDO inside the `[15 V, 51 V]` window, so on a source that
offers 36 V/48 V EPR it will not necessarily honour a 28 V preset. Narrowing the
window to `target ±5 %` would target a specific EPR PDO, but that variant was
rejected on the failing source and has not been re-tested on the Anker.

## The board shuts down when VBUS reaches EPR (source-dependent, hardware)

With the ≥140 W / wide-window request the board finally drives 28 V
(`PD 0x37 now: b0=0x02 … maxV=51000`) and then **the MCU loses SWD / resets**
exactly as VBUS crosses ~23 V. That is the TPD4S480 EPR adapter doing its job,
and it is *not* something the PD request can avoid — VBUS must pass ~23 V to
reach 28 V.

Netlist path (`analysis/root.net`):

| Net | Nodes | Note |
|---|---|---|
| `+VBUS` | connector, `U3.20` (TPD4S480 VBUS), `Q6.3` (drain), `Q10.5`, `JP13.3` | raw VBUS |
| `+VBUS_LV` | `Q6.1` (source), `U3.19` (VBUS_LV), `U4.26/27` (**TPS26750 VBUS pins**), `D9.1` | TPS26750 VBUS sense rail |
| `/USB/EPR_BLK_G` | `U3.17`, `R25.2`, `JP2.1` | TPD4S480 blocking-FET gate driver |
| `/USB/VBUS_LV_GATE` | `Q6.2` (gate), `R25.1`, `JP2.2`, `JP3.1` | Q6 gate |
| `/USB-PD/EPR_EN` | `U4.7` (**TPS26750 GPIO2**), `U3.16` (TPD4S480 EPR_EN) | EPR divider enable |

TPD4S480 behaviour (datasheet §6.3.4): asserting `EPR_EN` (or VBUS rising past
`EPR_THRESH_R` ≈ 23–24 V) makes `VBUS_LV = 0.42 × VBUS` and **disables the
`EPR_BLK_G` gate driver**, turning the external blocking NFET `Q6` **off** so
`+VBUS_LV` is isolated from raw `+VBUS`. The TPS26750's IO config maps GPIO2 to
event 92 `VBUS_SENSE_DIVIDER`, i.e. it asserts `EPR_EN` itself when entering EPR.

Jumper state matters here: `JP2` is a **normally-open** 2-pad jumper between
`EPR_BLK_G` and the Q6 gate, and `JP3` is **normally-closed** to the `D8`/`D9`
(18 V zener) network. So Q6's gate is not on the straight TPD4S480 driver unless
`JP2` is bridged — the gate network is a rework area and must be checked against
the TPD4S480 reference before EPR is trusted.

Rails that can brown the MCU out:

* `U11` (**LMR16006X**, 3V3 buck; `L2` → `+3V3`) — input `Net-(JP13-C)` selects
  `VPP`/`+VBUS` via `JP13`, and its `~SHDN` can be driven by **TPS26750 GPIO6**
  (`/LV-Supply/3V3_EN_PD`, `U4.31`) through `JP16` (1–2 = always on, 2–3 = PD
  controlled).
* `U13` (second LMR16006X) and `U5` (LM2765 → `+6V6_LDO` for the `U8` LTC7004
  gate driver) feed the high-side power path (`Q10`) that makes
  `+Converter/PWR_UNREG_IN` for the LT8390.

### MAX action (highest PDO) and fixed-vs-AVS preference

A 240 W source that advertises **fixed 28/36/48 V EPR *and* an EPR AVS APDO**
exposed a chooser bug: `choose()` preferred AVS whenever an EPR AVS APDO was
visible, and every AVS request resolved to 20 V (`avs=true … maxV=27950` →
`PD contract: 20000 mV`), so no EPR preset worked on that charger.

* `choose()` now prefers the **fixed** EPR path whenever the source advertises a
  fixed EPR PDO (AVS is kept only for AVS-only sources).
* **BTN2 in Manual = MAX**: ignores the preset and asks for the highest rail the
  source offers. `auto_track::choose_highest()` picks the highest fixed PDO
  (declared EPR rails injected pre-entry), and `negotiate()` issues it through
  `request_fixed_epr_profile()` — the wide `[15 V, 51 V]` + ≥140 W request, so
  the controller lands on the highest-power EPR PDO (48 V on a 240 W source,
  28 V on a 140 W one). The footer shows `MAX`.
* A specific EPR preset uses the new `request_epr_rail_fixed()`: same ≥140 W
  forcing but the window's upper bound is narrowed to `target +5 %`. That variant
  has not been bench-verified yet; MAX is the known-good path if a targeted
  preset does not land.

**Window floor must stay SPR-inclusive.** Field-verified: an EPR fixed request
whose window excludes the whole SPR set (`[26.6 V, 29.4 V]`) falls back to 5 V,
even though the 28 V EPR PDO is comfortably inside it. The controller only seems
to escalate to an EPR PDO when the window still contains an SPR PDO to consider;
the ≥140 W requirement is then what rules every SPR choice out. Both EPR windows
therefore use a **5 V floor**:
`request_fixed_epr_profile` = `[5 V, 51 V]` (MAX) and
`request_epr_rail_fixed` = `[5 V, target + 5 %]` (specific preset). This also
matters for a source like the china charger, which in EPR advertises only
5/9/12 V SPR — a 15 V floor would exclude every SPR PDO it has.


### Bench checks for the EPR shutdown

1. Scope `+3V3` (`TP65`/`J4`) through the EPR transition. A dip ⇒ the 3V3 enable
   or input is collapsing, not the PD link.
2. Note `JP16`: if it is on 2–3, move to 1–2 (3V3 always on) and retry. If the
   board survives, GPIO6 de-assert on EPR is the cause.
3. Scope `+VBUS_LV` (`TP9`) and `+VBUS`. `+VBUS_LV` should step *down* to
   ≈0.42 × VBUS (≈11.8 V at 28 V), never to 0 V.
4. Scope `+Converter/PWR_UNREG_IN` and `U8`/`Q10` gate — confirm the HV path FETs
   stay on through the transition.
5. Check `JP2`/`JP3` and the `D8`/`D9` gate network against the TPD4S480 EPR
   adapter reference; the E2E failure mode was exactly `EPR_BLK_G` collapsing at
   ~23 V.

Firmware-side hooks if a TPS26750 GPIO is the culprit: the `GPsh`/`GPsl` 4CC
tasks force a GPIO output regardless of its event mapping, so GPIO6 can be held
asserted from boot if the 3V3 enable proves to be the problem.

> Field note: the `TPS config 0x37` line at boot shows the **host-written** value
> that survives an MCU reset, not the EEPROM image. Only a TPS26750 power-cycle
> (or the `0x33` header, which the host never writes) reflects what is loaded.

### Bench checks after this change

* A 28 V confirm should log `PD request: rail=28000 mV` then
  `PD 0x37 now: b0=0x… avs_en=true pps_en=false`, `EPR entered (n EPR PDOs)`,
  `PD contract: 28000 mV`, and `PD AVS keep-alive: 28000 mV` every 7 s.
* A non-EPR source still falls back once via `EPR unavailable: …` and latches SPR.
* PPS presets (12 V on a PPS source) still issue a PPS request.

## Decoding `config_240W_TPS26750_F8091159_epr_pps.json` (the real image)

`*_vif.xml` is only the USB-IF certification descriptor — it has no
0x28/0x29/0x37 content — so it cannot describe the power path. The JSON is the
actual Application Customization project (register numbers are decimal):

* **0x28 Port Configuration** (`[0,8,46,1, 0…,3]` = `0x012e0800`):
  bits 1-0 = 0 → **Sink state machine only** (good); bits 17-16 = 2 → VBUS OVP
  usage 111 %; bits 21-20 = 2 → PP5V OVP 5.8 V; **bits 26-24 = 1 →
  `VBUS Sink UVP Trip HV` = 10 %**, the "VBUS disconnect when power role is
  sink" threshold. A VBUS dip >10 % through the 20→28/48 V EPR transition makes
  the controller drop the sink path; `0h`=5 % … `7h`=50 %, so this is the one
  config-level knob that widens the ride-through (try `3h`=20 %).
* **0x29 Port Control** (`[50,48,128,0]` = `0x00803032`): bits 6/7 = 0 →
  **PR_Swap to Source disabled**, so the VIF's `Requests_PR_Swap_As_Src=true`
  is stale/contradictory and the runtime image is fine here. Bits 4/5 = 1
  (process/initiate swap to sink) are odd on a sink-only port but harmless.
* **0x37** = `b0=0x3e`, byte8 `0x02` (PPS off), byte16 `0x01` (EPR AVS on),
  48 V/5 A, NoCapMismatch set, auto-disable clear — as intended.
* **0x5C IO Config**: byte 38 = 92 → **GPIO2 → event 92 `VBUS_SENSE_DIVIDER`**
  (drives TPD4S480 `EPR_EN`); byte 32 = 4 → **GPIO Event Polarity bit 2 set, so
  GPIO2's event is inverted**. TPD4S480 `EPR_EN` is active-high — verify that
  in the GUI (the chip also auto-enables its divider above ~23 V, which masks an
  inverted enable at 28/48 V). Output-enable = `0xC0F` (GPIO0-3, 10, 11); GPIO6
  has no event and is **not** output-enabled → the 3V3-buck enable comes from
  `JP16` 1–2, consistent with the `GPsh` test not helping.
* **Register 0x27 (39) is written but is not in the TRM register map**
  ("reserved; contents should not be modified") — a GUI artifact, not decodable
  from SLVUCR7.

Firmware now logs the power-path state. The `POWER_PATH_SWITCH` interrupt(bit 23) is **masked** in the app config (INT_MASK1 only enables bits 80/81), so
it never raises the IRQ and the handler never sees it — the register is polled
instead: `POWER_PATH_STATUS` (0x26) is read every 500 ms while a contract is
active and the line

`PD power path: PP5V=… PP_EXT=… VCONN=… b4=0x… contract=… mV vin=… mV`

is printed on every change. For a sink `PP_EXT` should read `3h` (enabled,
system input); `0h` disabled and `1h` disabled **due to fault** is the signature
of "converter input gone while VBUS is still valid".

### Result: the controller opens PP_EXT on the EPR request

```
0.24   contract=12000  PP5V=0 PP_EXT=3  vin=12000   external path enabled
8.80   PD request: rail=28000 mV (EPR, maxV=29400, b0=0x02)
10.79  contract=20000  PP5V=0 PP_EXT=0  vin=20000   external path DISABLED
11.12  vin=4405                                      converter input collapsed
```

`PP_EXT = 0h` is a **normal disable, not a fault** (`1h` would be faulted), so
the controller chooses to open the high-voltage path when it issues the EPR
request and does not re-close it. The converter then has no input while the
USB-C bus is still at 20 V, and the MCU only survives because the 3V3 buck was
moved onto `+VBUS` (`JP13`).

The most consistent explanation is the `VBUS Sink UVP Trip HV` setting in Port
Configuration (0x28 bits 26-24 = `1h` = 10 %): the controller is negotiating a
28 V EPR contract, the source delivers only 20 V (a ~29 % shortfall), the
disconnect threshold trips and the sink path opens. Raising it to `7h` (50 %)
should let the path ride through and leave the converter on the 20 V the source
actually provides, which is a clean test. The root cause of no-28 V remains the
source: the Anker delivers it, the china charger advertises EPR caps but does not
honour the EPR request.





## Three-source CC comparison (`PD-Captures/new/`)

Decoding the raw PD traffic side by side separates the two failures cleanly.
`EPR_Mode` action is the top byte of its data object (1=Enter, 2=Enter Ack,
3=Enter Succeeded, 4=Enter Failed, 5=Exit); the action-4 data byte is the
reason code.

### Anker 737 — `pd-nitride-nano(anker737)-NEWNEW.sqlite` (works)

```
4.070  EPR_Mode action=1 (Enter)
4.076  EPR_Mode action=2 (Enter Ack)
4.201  EPR_Mode action=3 (Enter Succeeded)
4.221  EPR_Source_Caps   … f4c10800            (28 V/5 A fixed)
4.227  Request  RDO=0x83c7d1f4, PDO=f4c10800   position 8, EPR bit set, 5 A
4.340  VBUS = 28.26 V → holds to the end of the capture
```

The sink requests the **fixed 28 V EPR PDO by position (slot 8)** with the EPR
bit in the RDO. This is the only capture that completes EPR end to end.

### China charger — `pd-nitride-nano(china)-NEW.sqlite` (source refuses EPR)

```
4.011  EPR_Mode action=1 (Enter)
4.018  EPR_Mode action=2 (Enter Ack)
4.069  EPR_Mode action=4 (Enter FAILED), data=0x01   ← source rejects EPR entry
4.089  Request  RDO=0x53c7d1f4  position 5 → 20 V
4.710  Request  RDO=0x33c00000  → 12 V
```

`EPR_Mode action=4` is the **source** refusing to enter EPR before any contract
is requested — this happens ~1 s after plug-in, not at the 28 V request. The
source later emits EPR caps anyway (8.7 s), which is why `source_cap_header`
reports `EPR=1` and the firmware believes it entered EPR, but VBUS never goes
above 20 V. Nothing in the sink can fix a rejected EPR entry.

### 240 W charger — `pd-nitride-nano(240w)-NEW.sqlite` (EPR works, board drops it)

```
3.380  EPR_Mode action=1
3.389  EPR_Mode action=3 (Enter Succeeded)
3.417  EPR_Source_Caps   … f4c10800 | f4410b00 | f4010f00 | f096c0d3  (28/36/48 + AVS)
3.423  Request  RDO=0xb3cf0064, PDO=f096c0d3   → EPR AVS APDO
4.119  VBUS = 47.97 V                            ← 48 V EPR delivered!
4.841  VBUS = 11.99 V                            ← falls back to 12 V
```

This one proves EPR 48 V works at the PD level on this source: VBUS reaches
47.97 V. It then falls back to 12 V, which is the power-path/UVP behaviour
already traced (`PP_EXT` opened, `VBUS Sink UVP Trip HV` = 10 %). So the 240 W
charger is not an EPR-compatibility problem; it is the board's ride-through.

Summary: **china = source rejects EPR entry; 240 W = EPR fine, board drops it;
Anker = full end-to-end success.**

## Menu scroll was confirming the highlighted preset (input bug)

The 240 W capture shows the 48 V EPR contract established (`4.119 VBUS=47.97`)
and then a **PPS request 27 ms later** (`4.146`, RDO `0x63c4b03c`, PDO
`6432a4c1`) followed by a drop to 12 V (`4.841`). On a source with no 12 V fixed
PDO, 12 V is served by PPS — so that request is the firmware confirming **preset
0 (12 V)**, which is what the PD grid wraps to when the encoder is turned past
48 V. Scrolling must not request anything, so the turn was being delivered as a
confirm.

`ui/input.rs` had two defects:

1. `check_button` fired on the **first** pressed sample: it compared against the
   timestamp of the last *release*, which is always older than `DEBOUNCE_MS`. A
   one-poll (5 ms) glitch on the encoder switch therefore fired `EncBtn`.
2. `poll()` wrote `EncTurn` and then let `check_button` **overwrite**
   `last_event` with `EncBtn`, so a rotation that also read the switch as closed
   became a confirm.

Fixed: buttons now require a **continuous press for `DEBOUNCE_MS`** before
firing, and a rotation **defers button sampling to the next poll**, so a turn can
never be delivered as a press. On the PD screen that means scrolling past the EPR
entries only moves the highlight; the preset is requested on a deliberate
encoder-button press.

### Follow-up: the probe was armed by the highlight, not by a request

The encoder-input fix was not the cause. The real one is in `PdManager::poll`:
the `ESrC` EPR gate was driven by `choice` (`wants_epr`), and `choice` follows
`pd_profile_index`. So merely **scrolling the PD highlight onto 28/36/48 V**
started an EPR probe and then a renegotiation, with no confirm:

```
0.236  EPR entered (4 EPR PDOs)     controller auto-negotiated 48 V at boot
0.253  PD contract: 48000 mV
0.255  PP_EXT=3 … vin=48000         power path fine at 48 V
0.256  PD request: rail=12000 … pps=true   firmware boot request drops it to 12 V
2.216  EPR exited; SPR PDOs restored
7.660  EPR probe (ESrC) ok=true     ← only scrolled, no `PD confirm`
7.764  PD request: rail=28000 mV
9.750  PP_EXT=0 … vin=4409          path off
```

Fixed: a `request_armed` flag (`negotiate_pending || renegotiate_request ||
auto_due`) now gates both the `ESrC` probe and `do_negotiate`, and a confirm is
kept armed while EPR entry is in flight so the probe timeout can still fire.
Moving the highlight no longer probes or renegotiates; the rail is requested only
on an encoder-button confirm (or an Auto re-plan).

Two observations from the same log, still open:

* The controller's own boot auto-negotiation reaches **48 V with the power path
  fine** (`PP_EXT=3`). The firmware then immediately overrides it with the
  boot preset (index 0 = 12 V, served by PPS), which exits EPR. If the board
  should keep the source's high rail at boot, the boot request itself needs
  revisiting (e.g. don't override, or restore a saved rail).
* A confirmed, **targeted** 28 V request on the 240 W still settles at 20 V and
  leaves `PP_EXT=0`, whereas that source's own auto-negotiated 48 V holds. So
  targeted fixed-EPR selection remains suspect on this controller; the wide/MAX
  path and the controller's own AVS selection are the two that have worked.

### MAX now uses the controller's own auto-negotiate

On the 240 W source, every **host-window** EPR request — targeted fixed
(`maxV=29400`) and EPR AVS (`maxV=27950`) alike — settles at 20 V and leaves
`PP_EXT=0`. The only thing observed to reach the high rail on that source is the
**controller's own auto-negotiation** from the application config
(`b0=0x3e`: auto-computed range, EPR AVS enabled, PPS off), which reached 48 V
with `PP_EXT=3` at boot.

So the MAX action (`BTN2` in Manual mode) now issues
`request_max_rail()`: the same auto-compute + `avs_en` + PPS-off state, triggered
by the PPS edge. On the 240 W that should reproduce the boot 48 V; on the Anker
(no EPR AVS APDO) it lands on the highest fixed EPR PDO, 28 V.

Specific EPR presets still use the host-window paths, so on an AVS-capable
source they may not reach the requested rail — use MAX there. The `PD request`
log prints `max=true` when the MAX path is taken; if it never appears, BTN2
didn't reach the handler (Auto mode toggles the policy instead).

### MAX reverted, and the boot request no longer clobbers a self-negotiated EPR rail

Field results from the auto-compute MAX (`request_max_rail`, `b0=0x3e`,
`avs_en=true`): **both** sources settled at 20 V —
`rail=28000 … max=true → 20000` on the Anker and `rail=48000 … max=true →
20000` on the 240 W. That is worse than the previous MAX, so `maximize` is back
on `request_fixed_epr_profile()` (wide `[5 V, 51 V]` + ≥140 W, `b0=0x02`,
`avs_en` clear), which is what held 28 V on the Anker.

The 240 W's **only** observed 48 V came from the controller's own power-up
auto-negotiation with the EEPROM config (`b0=0x3e`, `avs_en=true`), when the
TPS26750 had reloaded `0x37` from the EEPROM rather than from a previous host
write. The firmware then destroyed it with the boot preset request. `poll()` now
reads the active contract on first contact and, if it is already above
`SPR_MAX_MV`, keeps it instead of requesting preset 0:

```
PD: controller already in EPR at boot; keeping its rail
```

To see it: power-cycle the TPS26750 (unplug the source/board supply so `0x37`
reloads from the EEPROM), reconnect the 240 W and don't touch the encoder — the
48 V rail should now survive boot.

### MAX confirmed on the Anker; use the boot path on the 240 W

With `maximize` back on `request_fixed_epr_profile()` (wide `[5 V, 51 V]` +
≥140 W, `b0=0x02`, `avs_en` clear), **BTN2 in Manual mode holds 28 V on the
Anker**:

```
PD request: rail=28000 mV … max=true
PD 0x37 now: b0=0x02 avs_en=false pps_en=false maxV=51000 mV
PD contract: 28000 mV @ 5000 mA → vin=28266 mV, steady
```

The same request on the 240 W still settles at 20 V and opens `PP_EXT`
(`contract=20000 … PP_EXT=0 → vin=4407`). That source's only observed 48 V came
from its **own power-up negotiation** when `0x37` was the EEPROM auto-negotiate
config (`b0=0x3e`, `avs_en=true`) rather than a previous host write.

So for the 240 W the reliable route is not MAX but **boot**: power-cycle the
TPS26750 (unplug the source so `0x37` reloads from the EEPROM), reconnect and
leave the encoder alone. The new boot guard keeps that self-negotiated rail:

```
PD: controller already in EPR at boot; keeping its rail
```

Pressing MAX on the 240 W forces a host window, which that source answers with
20 V — so on that charger, don't use MAX; use the boot path. Targeted EPR presets
on AVS-capable sources remain the open limitation.

## 2026-09-26 — the "EPR force" path removed, staged EPR exit added

Root cause confirmed against the reference project's own driver
(`theohg/tps26750_multiplatform`, submodule of `theohg/PD240W`): it has **no**
≥140 W "force EPR" host window, and its startup mode is `HIGHEST_VOLTAGE` —
literally "TPS26750 automatically negotiates highest voltage due to its EEPROM
config. We don't interfere. Doing so breaks autonomous EPR entry sequences."
It also has a three-step EPR→SPR exit because a direct high→low request "reboots
some chargers". nitride-nano had invented the opposite of all three. See
`analysis/PD_ROOT_CAUSE.md` for the full evidence.

Changes:

* `src/drivers/tps26750.rs`
  * **Deleted** `write_epr_force`, `request_fixed_epr_profile`,
    `request_epr_rail_fixed` and `request_max_rail`. The force window cleared
    `AutoComputeSinkMaxVoltage` **and** `EPR AVS Enable Sink Mode` and cleared
    `NoCapabilityMismatch` — SDAA265 §4.1/§5.2 says that is exactly why an
    EPR-capable controller stops at the highest SPR PDO (20 V).
  * **Added** `request_fixed_epr_rail` — the reference's targeted fixed-EPR
    request: host window `[5 V, target + 5 %]`, `NoCapabilityMismatch` left set,
    re-evaluated with the `PPSEnableSinkMode` edge (never `GSrC`).
  * **`restore_autonegotiate`** now leaves PPS **off** (it was enabling PPS,
    which outranks EPR and caps at 21 V) and triggers with the PPS edge; this is
    what "MAX" issues.
* `src/pd/manager.rs`
  * MAX (`epr && maximize`) → `restore_autonegotiate`, i.e. hand voltage
    selection back to the controller's own autonegotiation. If a live EPR rail
    is already up, MAX now leaves it alone instead of re-writing 0x37.
  * EPR mode entry no longer arms a re-plan (`negotiate_pending = true` was
    pulling a live 48 V contract back down to the UI preset), and the live
    contract is seeded into `requested_rail_mv` at boot so Auto-tracking does
    not immediately re-request it.
  * **Staged EPR→SPR exit** (`EprExitStep`, `begin_epr_exit`, `step_epr_exit`):
    a request that would drop a live >20 V contract to an SPR rail now walks
    EPR AVS floor (if the source APDO reaches SPR) → 5 V fixed → the original
    target, each step bounded by `EPR_EXIT_STEP_TIMEOUT_MS` (2 s). The
    AVS keep-alive and the `EPR unavailable` latch are suppressed during the
    exit so they cannot fight it.
* `src/main.rs`: `PdManager::negotiate` now takes `&AppState` (the exit needs
  the source PDO list to find a reachable AVS APDO).

Builds clean. Bench expectations: on the 240 W, power-cycle the TPS26750 and
plug in **without touching anything** → the controller's own 48 V should now
survive boot (no override); on the Anker, MAX should hold 28 V without the
register being knocked to 20 V; any EPR→SPR confirm should log
`PD: staged EPR exit … step 1/3 … 2/3 … 3/3` instead of one direct request.

### Field result, and the real EPR-selection rule

First bench run of the above: EPR entry and the staged machinery worked, but a
**targeted fixed 28 V preset still settled at 20 V**:

```
41.47  PD confirm: preset=3 choice=28000
41.50  EPR probe (ESrC) ok=true
41.58  EPR entered (4 EPR PDOs)    caps include 28/36/48 fixed + 48 V AVS
41.60  PD request: rail=28000 … avs=false epr=true
41.62  PD 0x37 now: b0=0x0a avs_en=false maxV=29400
43.59  PD contract: 20000 mV        ← still 20 V
44.68  EPR unavailable: 28000 settled at 20000
```

A host **fixed**-EPR window does not escalate on the 240 W regardless of its
shape (wide, narrow, or mismatch-forced). The readback proves the write landed,
so the controller re-ranked and still chose the 20 V SPR PDO.

The winning recipe was in the EEPROM all along. `0x37` from
`config_240W_TPS26750_F8091159_epr_pps.json` is `3e 40 1f 00 c0 93 01 00 02 …`:
`b0 = 0x3e` (**auto-compute on**), `avs_en = 1`, **AVS output voltage = 48 V**.
The controller's own power-up request was an **EPR AVS** RDO (`0xb3cf0064`,
PDO `f096c0d3`) and the source delivered 47.97 V. Every host request that
cleared auto-compute — the mechanism SDAA265 §5.2 names — settled at 20 V.

Changes:

* `Tps26750::request_avs_profile` now writes **auto-compute on**, `avs_en`, PPS
  off, and the AVS output voltage = the requested rail, re-evaluated with the
  PPS edge. The now-unused APDO-window args were dropped.
* `PdManager::choose` now **prefers the AVS path whenever the source advertises
  an EPR AVS APDO**, and falls back to the fixed-EPR host window only for
  sources with no AVS APDO (e.g. the Anker). This is the inverse of the old
  preference, which was based on an auto-compute-off AVS test that could only
  ever reach 20 V.
* The AVS keep-alive and the staged exit's step-down use the same AVS request.

Trigger note: the TPS26750 TRM 4CC list (`Gaid`, `GSrC`, `ESrC`, `GSkC`,
`ESkC`, `SSrC`, `GPsh`/`GPsl`, …) has **no `ANeg`** — that is a TPS25751 task.
The `PPSEnableSinkMode` edge remains the way to re-evaluate 0x37.

Also note the boot line in that run: `TPS config 0x37: b0=0x0a avs_en=false
pps_en=true maxV=11950` — a **stale host-written window persisted from the
previous run**, so the controller never ran the EEPROM (auto-compute + AVS)
negotiation that reaches 48 V. Only a TPS26750 power cycle (unplug source *and*
board supply) reloads 0x37 from the EEPROM; an MCU reset does not.

Next bench run: power-cycle the TPS26750, then (a) touch nothing → expect 48 V
held; (b) BTN2/MAX → `PD request: rail=48000 … avs=true` and `b0=0x3e
avs_en=true`; (c) confirm 28 V → `avs=true` (it may resolve to the highest AVS
rail rather than exactly 28 V, because the controller computes the range).

### Second field run: the PPS-bit edge was the real culprit

The next run pressed **MAX** with the AVS + auto-compute path:

```
4.513  PD confirm: preset=0 choice=48000             (BTN2 / MAX)
4.620  EPR entered (4 EPR PDOs)
4.638  PD request: rail=48000 … avs=true epr=true max=true
4.656  PD 0x37 now: b0=0x3e avs_en=true pps_en=false maxV=51000
6.625  PD contract: 20000 mV                        ← still 20 V
7.717  EPR unavailable: 48000 settled at 20000
7.843  PD 0x37 now: b0=0x3e avs_en=true pps_en=false maxV=20000
```

The write is *exactly* the EEPROM policy (`b0 = 0x3e`, `avs_en = 1`) and it
still landed at 20 V — but the readback tells the story: the controller rewrote
`AutoNegMaxVoltage` from 51000 to **20000**, i.e. it recomputed the maximum for
SPR. **EPR had been lost between the write and the contract.**

Cause: `modify_sink_register` triggered re-evaluation by toggling
`PPSEnableSinkMode`. Enabling PPS for even 2 ms makes the controller re-evaluate
with PPS selected, and PPS is **prioritised over EPR and caps at 21 V**
(TRM §6.3) — so it dropped out of EPR, and auto-compute then reported 20 V. The
reference firmware's toggle is fine for its SPR/PPS/AVS use, but it is exactly
wrong for an EPR request.

Fix — the TRM (§2.3) states the mechanism outright: *"the PD Controller will
always prepare its own Request message based on the settings in
AUTO_NEGOTIATE_SINK (0x37) and TX_SINK_CAPS (0x33) … the host can change 0x37 …
then issue the `GSrC` 4CC Task and the PD controller will re-negotiate the PD
contract based on the updated values."*

* `modify_sink_register` no longer toggles PPS at all; it writes 0x37 once.
* `PdManager::negotiate` issues `GSrC` after **every** request (the old code
  explicitly skipped it for EPR).
* The AVS keep-alive and the staged exit's step-down also issue `GSrC`.

This also removes the need to power-cycle the TPS26750: `GSrC` makes the
controller re-run the same policy it runs at power-up, so a stale host window in
0x37 is overwritten and re-evaluated in place — which matters because on this
board the TPS26750 cannot be power-cycled while the debugger is attached.

### Third field run: `GSrC` drops EPR too — use `ESrC`

With the PPS toggle removed and `GSrC` as the trigger, MAX still landed at 20 V,
and this time the log says exactly why:

```
9.300  EPR entered (4 EPR PDOs)     SPR=6 EPR=4
9.318  PD request: rail=48000 … avs=true max=true
9.627  PD 0x37 now: b0=0x3e avs_en=true maxV=20000     ← max collapsed
11.299 PD source caps: SPR=6 EPR=0                     ← EPR GONE
11.301 EPR exited; SPR PDOs restored
```

So **`GSrC` also drops EPR**: it issues `Get_Source_Cap`, the source answers with
its *SPR* capabilities, the controller re-negotiates in SPR and auto-compute
reports the SPR max (20 V). Neither of the two non-EPR triggers can work for an
above-SPR request.

`ESrC` is the task that reads the **EPR** capabilities (TRM §5.3.8), so that is
now the trigger for every EPR request:

* `negotiate` triggers `ESrC` when `ch.epr`, `GSrC` otherwise;
* the AVS keep-alive and the staged exit's step-down use `ESrC`.

Expected next run: after `rail=48000 … avs=true`, `PD 0x37 now` should keep a
high `maxV` (48000/51000, not 20000), the caps should stay `EPR=4`, and the
contract should reach 48000.

### Fourth field run: EPR was being entered with the *stale* register

With `ESrC` as the trigger, EPR stayed entered (`SPR=6 EPR=4`) but `maxV` still
computed to 20000 and the contract stayed at 20 V. The log shows why:

```
7.037  EPR probe (ESrC) ok=true          ← entry with the *old* 0x37
9.019  EPR entered (4 EPR PDOs)
9.038  PD request: rail=48000 … avs=true
9.074  PD 0x37 now: b0=0x3e avs_en=true maxV=20000
```

The request ordering was wrong: `poll()` sent `ESrC` **first** (the "EPR entry
gate"), held the request back until entry was observed, and only then wrote the
0x37 policy. So the controller always entered EPR with the stale register
(`b0=0x0a`, PPS window) and had already computed its SPR range; the later policy
write could not lift it.

At power-up the order is the opposite: the EEPROM policy is in 0x37 **before**
the controller enters EPR, so entry and selection happen with the right config.

Fix: removed the `ESrC` gate and the entry wait from `poll()` (and the
`epr_probe_pending`/`epr_probe_at`/`EPR_ENTRY_TIMEOUT_MS` state). `negotiate`
now writes the 0x37 policy and *then* issues `ESrC`, so the controller enters
EPR with the correct policy in place — the power-up order.

### Fifth run: it all works — from a fixed rail. **PPS is the last blocker**

One run was captured that worked **completely**:

```
0.182  PD: controller already in EPR at boot; keeping its rail
0.208  PD contract: 48000 mV        PP_EXT=3  vin=48000
10.82  MAX -> "already on an EPR rail (48000 mV); leaving it"
20.67  preset 36 V -> PD contract: 36000 mV   vin=35962
27.79  preset 28 V -> PD contract: 28000 mV   vin=27964   (+ AVS keep-alive)
36.88  preset 20 V -> PD: staged EPR exit 28000 -> 20000
       step 1/3 AVS down to 15000 -> contract 15000
       step 2/3 15000 -> 5 V      -> contract 5000
       step 3/3 5 V -> target re-queued
41.17  preset 48 V -> PD contract: 48000 mV   vin=47957   (+ AVS keep-alive)
```

Every mechanism — boot guard, MAX, targeted AVS rails, the staged exit, and a
fresh EPR re-entry — behaves. That run happened to start from a **fixed** rail
(the STM32 was reset mid-negotiation, so it woke with the TPS already at 48 V).

The failing runs all start from a **PPS** contract (the boot preset 0 = 12 V,
served by PPS on this source). TRM §6.4: changing `PPSEnableSinkMode` while a
Sink PPS contract is active makes the controller **auto-re-evaluate**. So writing
the EPR policy (`pps_en = false`, `avs_en = true`) while PPS is live makes the
controller fall to the best SPR fixed PDO — 20 V — *before* `ESrC` can enter
EPR. Consistent with every observation: MAX from 12 V PPS always landed at 20 V;
the same request from a fixed rail reached 48 V.

Fix: before an EPR request, if the last contract we requested was PPS, first move
to a fixed 5 V contract (`request_fixed_profile(5 V)` + `GSrC`), then re-queue
the EPR request so the normal path writes the policy and issues `ESrC` with no
PPS contract live. New state: `pps_requested`, `epr_prep`, `epr_prep_target`,
`step_epr_prep` (2 s bound). Reset on plug and on losing the controller.

### Sixth run: the PPS release worked, but 5 V is too low a springboard

The prep step fired correctly (`PD: leaving the PPS contract before EPR entry` →
`PD contract: 5000 mV` → `PD: PPS released; EPR request re-queued`), then the
re-queued request still landed at 20 V (`maxV` 51000 → 20000 inside 34 ms).

Comparing with the one run that worked, two concrete differences remain:

| | working 48 V (run 5) | failing 48 V (runs 1/5/6) |
|---|---|---|
| request branch | `avs=true` (a manual preset) | `avs=false` (`maximize`) |
| contract it started from | **20 V** fixed | 12 V PPS, or 5 V after prep |

Changes to close both:

* `auto_track::choose_highest` now sets `avs: avs_window(...)`, so MAX takes the
  **exact same `request_avs_profile` branch** as the preset that reached 48 V.
* The PPS-release step now targets the **highest fixed SPR PDO** (20 V on this
  source) instead of 5 V, and waits for that rail — the working run escalated
  into EPR from the top of the SPR range.

### Seventh run: the 5→20 V springboard worked, 48 V still collapses

The prep now lands 20 V (`PD: PPS released (20000 mV)`) and the re-queued request
is `avs=true`, yet `maxV` still collapses 51000 → 20000 within 35 ms and the
contract stays at 20 V. So neither the branch nor the springboard was the cause.

**Boot now asks for the highest rail, not preset 0.** The boot request used to be
`PD_PRESET_VOLTAGES_MV[0]` = 12 V, and that 12 V PPS window is written into 0x37,
which persists across MCU resets — so every boot started from and re-asserted
12 V. New `boot_high` flag makes the first request use `choose_highest`; cleared
once issued, and an explicit UI confirm still wins. This fixes the 12 V pinning
(answer to "why does it fall back to 12 V at boot?"); the 20 V ceiling on a
host-initiated EPR request is still open.

### Eighth run: boot-high works; EPR re-entry is flaky, two fixes

Boot held 48 V (`keeping its rail`), 28 V/36 V/15 V presets worked, and every
staged EPR→SPR exit ran 1/3→3/3. Remaining failures are all **cold EPR entry**
(an above-SPR request made while EPR is not already active). Discriminator found
in the log:

* `36.68` — 36 V preset from 15 V: **works**. Preceded by an EPR exit through the
  **staged** path (48 V → 15 V → 5 V → 15 V), so `EPR exited` came from the 5 V
  step.
* `55.108` — the identical 36 V preset from 15 V: **fails**. Preceded by a
  **direct** 20 V → 15 V, because the old staged-exit guard only fired for a
  contract `> SPR_MAX_MV` (20 V is not), so EPR was exited without the 5 V step.

Fixes:

* The staged exit now fires whenever EPR is **entered** (`active_mv > SPR_MAX_MV
  || self.epr_seen`), so EPR is never left by a direct high→SPR request.
* Failed EPR entries are **retried** up to `EPR_RETRY_LIMIT` (2) times with a
  clean 5 V release between attempts (`force_release`, reusing the prep step)
  before `EPR unavailable` latches for the cable. The attempt counter clears on
  any live EPR contract, on plug, and when the controller is lost.

### Ninth run: my staged-exit change caused an infinite loop — fixed

```
11.807  PD: EPR exit step 3/3: 5 V -> target re-queued
11.902  PD request: rail=12000
11.904  PD: staged EPR exit 5000 -> 12000   ← again, forever
11.933  PD: EPR exit step 1/3: AVS down to 15000 ...
```

`epr_exit_done` was cleared by the contract mirror as soon as the rail reached
5 V, but `epr_seen` was still **stale-true** (the EPR caps had not been re-read),
so the re-queued SPR target re-triggered the newly-broadened exit condition
(`active > SPR_MAX || epr_seen`).

Fix: `epr_exit_done` is now cleared **only** when EPR is observed to exit (the
`EPR exited; SPR PDOs restored` branch), never on an SPR contract. Each EPR
episode therefore gets exactly one staged exit, and the next episode re-arms it.

### Tenth run: EPR→SPR works; SPR→EPR is a controller one-way door

With the loop fixed, the full session is clean: boot 48 V, every SPR preset via
the staged exit, 48 V→12 V→15 V→20 V all correct. The only failure left is going
back up:

```
18.966  PD request: rail=28000 … avs=true epr=true
18.998  PD 0x37 now: b0=0x3e avs_en=true maxV=20000
20.959  EPR entered (4 EPR PDOs)     ← EPR mode IS re-entered
21.294  vin=19986                    ← …but the contract never moves
24.357  PD request: rail=48000 … avs=true
24.388  PD 0x37 now: b0=0x3e avs_en=true maxV=51000   ← write sticks
27.458  EPR retry 1/2: 48000 settled at 20000
32.759  EPR retry 2/2: 48000 settled at 20000
38.061  EPR unavailable: 48000 settled at 20000
```

So the controller re-enters EPR mode on a host request but will **not re-select
an above-20 V contract** after a host-initiated EPR exit. Every above-SPR success
ever observed was either the autonomous boot negotiation or a change made while
EPR was already active (the 48→36→28 V steps). Retrying harder cannot help.

Fix: **don't leave EPR when the target is inside the source's EPR AVS APDO.**
`choose()` now converts an SPR-range preset into an EPR AVS contract when EPR is
already entered and the source's AVS APDO covers it (15 V and 20 V on the 240 W),
logging `PD: keeping EPR alive for N mV (AVS inside EPR)`. EPR is therefore only
exited for rails the AVS floor cannot reach (12 V on this source) — and coming
back up from those still needs a TPS26750 power cycle.

### Eleventh run: 240 W perfect; the Anker needs the PPS edge back

The 240 W is now fully correct (48 V held, 20 V/15 V via AVS inside EPR and
back). The Anker 737 regressed: after booting to its 28 V EPR rail, **no SPR
request takes effect**:

```
15.681  PD request: rail=15000 … epr=false       (Anker)
15.705  PD 0x37 now: b0=0x0a maxV=15750 trig_ok=true
16.093  vin=5199                                  ← never re-negotiated
```

The register write lands and `GSrC` succeeds, but the source does not move —
while the identical request works on the 240 W. That is the signature of a source
that only honours the reference firmware's `PPSEnableSinkMode` **edge** trigger.

Fix: `modify_sink_register` takes an `edge` flag again. `request_fixed_profile`
passes `true` (PPS-bit toggle + restore, the reference's SPR trigger); every EPR
request passes `false`, because toggling PPS there is exactly what drops EPR
(TRM §6.3). PPS requests keep `false` (their fields auto-trigger, TRM §6.4).
`GSrC` is still issued for non-EPR requests, so the 240 W sees both triggers.

### Final state — working

Verified on the bench with both sources:

* **Anker 737:** boot → 28 V EPR (its highest); 20 V → 15 V → 12 V through the
  staged exit; 12 V → 28 V straight back into EPR. All rails selectable.
* **240 W charger:** boot → 48 V EPR held; 20 V/15 V via EPR AVS (EPR stays
  up, so you can always go back); 12 V exits EPR (below the AVS floor).

The rules that made it work, in one place:

1. **Boot asks for the highest rail**, never preset 0. A 12 V window written to
   0x37 persists across MCU resets and pins every later boot to 12 V.
2. **Never overwrite a live EPR contract.** The EEPROM policy
   (auto-compute + AVS) is what lets the controller pick above 20 V.
3. **Enter EPR with the policy already in 0x37**, then re-evaluate with `ESrC`.
   `ESrC` reads the *EPR* caps; `GSrC` reads *SPR* caps and drops EPR.
4. **The PPS-bit edge only for plain fixed requests** (the Anker needs it);
   toggling PPS during an EPR request drops EPR.
5. **Leave EPR through the staged walk-down** (AVS floor → 5 V → target), once
   per EPR episode.
6. **Don't leave EPR at all** for rails inside the source's EPR AVS APDO — this
   controller will re-enter EPR mode from a host request but will not re-select
   an above-20 V contract after a host-initiated EPR exit.

Known limits: on the 240 W, 12 V leaves EPR and getting back up needs a TPS26750
power cycle (a rev3 "cyclable TPS" jumper would fix this). The Anker has no EPR
AVS APDO, so its SPR rails always exit EPR — but it re-enters fine.













