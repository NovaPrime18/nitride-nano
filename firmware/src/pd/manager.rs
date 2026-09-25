//! USB-PD policy: reacts to the TPS26750 interrupt line, (re)negotiates the
//! contract selected in the UI or derived by Auto-tracking PD, and mirrors the
//! active contract into the shared supply caps.
//!
//! Polling is split in two so a rail change can park the converter *before* the
//! I2C request: [`PdManager::poll`] plans the change and disables the output,
//! the main loop runs one `SupplyController::tick` to actually park the DACs,
//! and [`PdManager::negotiate`] issues the request.

use embassy_stm32::exti::ExtiInput;
use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;
use embassy_time::{Duration, Instant, Timer};

use crate::board;
use crate::drivers::tps26750::{
    Tps26750, TPS_INT_NEW_CONTRACT_AS_SINK, TPS_INT_PLUG_INSERT_REMOVAL, TPS_INT_POWER_PATH_SWITCH,
    TPS_INT_SOURCE_CAP_RX, TPS_REG_AUTONEGOTIATE_SINK, TPS_REG_MODE, TPS_REG_POWER_PATH_STATUS,
    TPS_REG_TX_SINK_CAPS,
};
use crate::pd::auto_track::{self, AutoChoice};
use crate::state::{
    AppState, Fault, PdAutoError, PdMode, PdState, RailRegion, SweepPhase,
    PD_PRESET_VOLTAGES_MV,
};

/// How long to wait for a newly requested contract before re-enabling the
/// output anyway (a source that never confirms must not leave the supply off).
const SWITCH_REENABLE_TIMEOUT_MS: u64 = 3_000;
/// Retry period while the source-capability list is still empty (the source
/// may not have sent Source_Capabilities yet right after a plug event).
const CAPS_RETRY_MS: u64 = 500;
/// Re-probe period while the TPS26750 is absent. The controller runs its own
/// firmware loaded from EEPROM, so it can NACK for a while after power-up; a
/// single boot-time probe would miss it for the whole power cycle.
const TPS_ABSENT_RETRY_MS: u64 = 1_000;
/// Presence re-probe period while the controller answers, so a controller that
/// drops off the bus mid-run is noticed and re-probed.
const TPS_PRESENT_REPROBE_MS: u64 = 5_000;
/// Consecutive failed presence probes before the controller is considered lost
/// (a single transient NACK must not flap it).
const TPS_LOST_FAILURES: u8 = 2;
/// Re-request interval for a live EPR AVS contract. Programmable contracts
/// (PPS/AVS) must be refreshed well inside the USB-PD 10 s keep-alive window or
/// the source reverts to 5 V. The reference PD240W firmware uses 7 s.
const AVS_KEEPALIVE_MS: u64 = 7_000;
/// How long to wait for EPR mode entry after an `ESrC` probe before giving up
/// and issuing the request anyway (which then settles on an SPR rail and latches
/// EPR off). The reference waits ~600–1200 ms during boot.
const EPR_ENTRY_TIMEOUT_MS: u64 = 2_000;
/// `POWER_PATH_STATUS` (0x26) poll period while a contract is active. The
/// `POWER_PATH_SWITCH` interrupt is masked in the app config, so the register is
/// polled instead and logged on change.
const PP_POLL_MS: u64 = 500;

/// Stateful wrapper around the TPS26750 driver, polled from the main loop.
pub struct PdManager {
    /// TPS26750 presence, tracked by the watchdog in [`Self::poll`]. While false
    /// no bus traffic is issued to the controller.
    present: bool,
    /// Earliest time to run the next presence probe.
    next_probe: Instant,
    /// Consecutive failed presence probes (see [`TPS_LOST_FAILURES`]).
    probe_failures: u8,
    negotiate_pending: bool,
    next_caps_retry: Instant,
    /// Last output setpoints seen, with the time they last changed, so a rail
    /// change waits for the user to stop turning the encoder.
    last_vset_mv: u32,
    last_iset_ma: u32,
    last_vset_change: Instant,
    /// Rail currently requested (0 = none).
    requested_rail_mv: u32,
    last_renegotiate: Instant,
    /// Request planned by [`Self::poll`], issued by [`Self::negotiate`].
    pending: Option<AutoChoice>,
    /// Output was on when a rail change forced a temporary disable.
    reenable_output: bool,
    /// Set once an EPR request settled back at an SPR voltage: this source (or
    /// cable) cannot enter EPR, so stop injecting EPR rails until the cable is
    /// replugged or the controller is re-probed.
    epr_unavailable: bool,
    /// True once the controller has actually entered EPR mode (the source now
    /// advertises EPR PDOs). Until then an EPR target is requested through the
    /// controller's own autonegotiate, which performs EPR mode entry.
    epr_seen: bool,
    /// `ESrC` sent and EPR mode entry not yet observed. While set, an above-SPR
    /// request is held back so the entry handshake is not clobbered by a 0x37
    /// write + `GSrC`.
    epr_probe_pending: bool,
    /// Time the `ESrC` probe was issued, for [`EPR_ENTRY_TIMEOUT_MS`].
    epr_probe_at: Instant,
    /// A request was issued, so the source's advertised PDO set may have changed
    /// (EPR mode entry *and* exit both change it). Forces one capability re-read.
    caps_dirty: bool,
    /// Live EPR AVS contract to keep alive: `(rail_mv, current_ma, min_mv,
    /// max_mv)`. `Some` only while an above-SPR AVS contract is requested.
    avs_keepalive: Option<(u32, u32, u32, u32)>,
    /// Earliest time to re-issue the AVS contract.
    next_avs_keepalive: Instant,
    /// Last active-contract voltage mirrored into the UI, for change logging.
    last_contract_mv: u32,
    /// Last `POWER_PATH_STATUS` (0x26) value, for change logging. Polled
    /// directly because the `POWER_PATH_SWITCH` interrupt is masked in the
    /// application config, so it never raises the IRQ line.
    pp_status: u32,
    /// Next `POWER_PATH_STATUS` poll.
    next_pp_poll: Instant,
}

/// Fall back to the board's design input limits when no PD contract governs the
/// input: an XT90 feed, a removed cable, or an absent controller.
///
/// The output stage is limited by *power* only (`control::supply`), so this is
/// what lets a non-PD feed reach the full 240 W design maximum instead of
/// inheriting the last negotiated contract's power. `input_current_cap_ma` is
/// the input-side INA228 backstop, not an output limit.
fn use_design_input_limits(app: &mut AppState) {
    app.supply.input_current_cap_ma = board::IIN_MAX_MA as u32;
    app.supply.input_power_cap_mw = board::POWER_MAX_MW;
}

impl PdManager {
    pub fn new() -> Self {
        Self {
            present: false,
            next_probe: Instant::now(),
            probe_failures: 0,
            negotiate_pending: false,
            next_caps_retry: Instant::now(),
            last_vset_mv: 0,
            last_iset_ma: 0,
            last_vset_change: Instant::now(),
            requested_rail_mv: 0,
            last_renegotiate: Instant::now(),
            pending: None,
            reenable_output: false,
            epr_unavailable: false,
            epr_seen: false,
            epr_probe_pending: false,
            epr_probe_at: Instant::now(),
            caps_dirty: false,
            avs_keepalive: None,
            next_avs_keepalive: Instant::now(),
            last_contract_mv: 0,
            pp_status: 0,
            next_pp_poll: Instant::now(),
        }
    }

    /// True when [`Self::negotiate`] has work queued (used by the main loop to
    /// decide whether an extra supply tick is needed).
    pub fn has_pending_request(&self) -> bool {
        self.pending.is_some()
    }

    /// Plan one polling step.
    ///
    /// 0. Probe the TPS26750 and track its presence (it boots its own firmware).
    /// 1. Track setpoint changes for the auto-track settle debounce.
    /// 2. If the TPS26750 IRQ line is asserted, drain the interrupt bitmap and
    ///    arm renegotiation on plug events.
    /// 3. Decide the desired rail (manual preset or auto-tracked). When the
    ///    rail must change, park the output and queue the request.
    /// 4. Mirror the active contract into the supply's input caps.
    pub async fn poll(
        &mut self,
        tps: &mut Tps26750,
        i2c: &mut I2c<'_, Async, Master>,
        app: &mut AppState,
        irq: &mut ExtiInput<'static>,
    ) {
        let now = Instant::now();

        // 0. Presence watchdog. The TPS26750 loads its application firmware from
        //    EEPROM on power-up and NACKs until it is ready, so probe at boot and
        //    keep probing periodically instead of giving up after one try. The
        //    probe is a MODE read; two consecutive failures mark the controller
        //    lost so a transient NACK cannot flap it. Every other bus access
        //    below is gated on `self.present`.
        if now >= self.next_probe {
            let ok = tps.init(i2c).await;
            self.next_probe = now
                + Duration::from_millis(if ok {
                    TPS_PRESENT_REPROBE_MS
                } else {
                    TPS_ABSENT_RETRY_MS
                });
            if ok {
                self.probe_failures = 0;
                if !self.present {
                    defmt::info!("TPS26750 present");
                    // Read back the loaded application config once. Without this
                    // a stale EEPROM image shows up only as "EPR does nothing";
                    // with it the log says whether the running image really
                    // declares the EPR sink PDOs the firmware assumes.
                    //
                    // MODE is "APP " once the EEPROM application config has been
                    // loaded; anything else means the controller is still running
                    // its boot/default policy (which has no EPR at all).
                    let mut mode = [0u8; 4];
                    if tps.read_register(i2c, TPS_REG_MODE, &mut mode).await {
                        defmt::info!("TPS mode: {=[u8]:a}", mode);
                    }
                    let mut caps = [0u8; 4];
                    if tps
                        .read_register(i2c, TPS_REG_TX_SINK_CAPS, &mut caps)
                        .await
                    {
                        defmt::info!(
                            "TPS config 0x33: hdr=0x{:02x} SPR={} EPR={}",
                            caps[0],
                            caps[0] & 0x07,
                            (caps[0] >> 3) & 0x07
                        );
                    }
                    let mut an = [0u8; 24];
                    if tps
                        .read_register(i2c, TPS_REG_AUTONEGOTIATE_SINK, &mut an)
                        .await
                    {
                        let max_v = ((an[4] as u32) | (((an[5] & 0x03) as u32) << 8)) * 50;
                        let max_i = ((an[1] as u32) >> 4) | (((an[2] & 0x3F) as u32) << 4);
                        defmt::info!(
                            "TPS config 0x37: b0=0x{:02x} avs_en={} pps_en={} maxV={} mV maxI={} mA",
                            an[0],
                            an[16] & 1 != 0,
                            an[8] & 1 != 0,
                            max_v,
                            max_i * 10
                        );
                    }
                    // A fresh controller invalidates any cached capabilities and
                    // re-arms the capability fetch / contract mirror.
                    app.pd_cap_count = 0;
                    self.epr_unavailable = false;
                    self.epr_seen = false;
                    self.present = true;
                    // Hold the board's 3V3-buck enable (GPIO6 -> /LV-Supply/
                    // 3V3_EN_PD) asserted: if the controller drops it across an
                    // EPR transition the MCU loses its rail and resets.
                    let gpio_ok = tps.set_gpio_high(i2c, 6).await;
                    defmt::info!("TPS GPIO6 (3V3_EN_PD) held high: {}", gpio_ok);
                    // Normally request a rail straight away. Do NOT enable PPS
                    // here: PPS is prioritised over EPR and caps at 21 V, so
                    // enabling it makes the controller negotiate a PPS contract
                    // and then *exit EPR* (seen in the capture).
                    //
                    // Exception: if the controller has already negotiated an EPR
                    // rail on its own at power-up — which it does when the
                    // EEPROM config has auto-compute + EPR AVS enabled and the
                    // source is capable (48 V on the 240 W) — leave that contract
                    // alone. Overriding it with the boot preset (index 0 = 12 V)
                    // was dropping a perfectly good 48 V rail.
                    let already_epr = tps
                        .get_active_contract(i2c)
                        .await
                        .map(|(v, _)| v > board::SPR_MAX_MV)
                        .unwrap_or(false);
                    if already_epr {
                        defmt::info!("PD: controller already in EPR at boot; keeping its rail");
                        self.negotiate_pending = false;
                    } else {
                        self.negotiate_pending = true;
                    }
                }
            } else {
                self.probe_failures = self.probe_failures.saturating_add(1);
                if self.present && self.probe_failures >= TPS_LOST_FAILURES {
                    defmt::warn!("TPS26750 lost");
                    app.pd_cap_count = 0;
                    self.negotiate_pending = false;
                    self.epr_probe_pending = false;
                    self.avs_keepalive = None;
                    self.present = false;
                    use_design_input_limits(app);
                }
            }
        }

        // 1. Setpoint tracking.
        if app.supply.v_set_mv != self.last_vset_mv || app.supply.i_set_ma != self.last_iset_ma {
            self.last_vset_mv = app.supply.v_set_mv;
            self.last_iset_ma = app.supply.i_set_ma;
            self.last_vset_change = now;
        }

        // 2. Interrupt handling.
        //
        // INT_EVENT1 is read-only on the TPS26750: a latched bit keeps the IRQ
        // line low until it is written back to INT_CLEAR1. Clear exactly the
        // bits consumed here; otherwise the plug/new-contract event re-arms
        // every poll and the output is parked and renegotiated in a loop.
        if self.present && irq.is_low() {
            let mut events = [0u8; 11];
            if tps.read_interrupts(i2c, &mut events).await {
                let mut clear = [0u8; 11];
                if Tps26750::is_interrupt_set(&events, TPS_INT_PLUG_INSERT_REMOVAL) {
                    app.pd = PdState::Negotiating;
                    // Insert or removal both set this bit; drop the cached caps
                    // so a stale list from the previous cable can't be used, and
                    // let a fresh cable retry EPR.
                    app.pd_cap_count = 0;
                    self.epr_unavailable = false;
                    self.epr_seen = false;
                    self.epr_probe_pending = false;
                    self.avs_keepalive = None;
                    self.negotiate_pending = true;
                    // The old contract is gone either way; drop back to the
                    // design limits until the new one is mirrored.
                    use_design_input_limits(app);
                    Tps26750::set_interrupt_bit(&mut clear, TPS_INT_PLUG_INSERT_REMOVAL);
                }
                if Tps26750::is_interrupt_set(&events, TPS_INT_NEW_CONTRACT_AS_SINK) {
                    app.pd = PdState::ContractActive;
                    Tps26750::set_interrupt_bit(&mut clear, TPS_INT_NEW_CONTRACT_AS_SINK);
                }
                // The source (re)advertised its capabilities. The controller
                // raises this on EPR mode entry *and* exit, which is exactly
                // when the EPR PDOs appear/disappear, so re-read the list: the
                // chooser and `epr_seen` must see them before the next request.
                if Tps26750::is_interrupt_set(&events, TPS_INT_SOURCE_CAP_RX) {
                    app.pd_cap_count = 0;
                    self.next_caps_retry = now;
                    Tps26750::set_interrupt_bit(&mut clear, TPS_INT_SOURCE_CAP_RX);
                }
                // The controller moved between its internal 5 V path and the
                // external high-voltage path. That switch is what feeds the
                // converter, so log the new PP state: a faulted/disabled PP3 is
                // exactly "converter input collapsed while VBUS is still valid".
                if Tps26750::is_interrupt_set(&events, TPS_INT_POWER_PATH_SWITCH) {
                    self.log_power_path(tps, i2c).await;
                    Tps26750::set_interrupt_bit(&mut clear, TPS_INT_POWER_PATH_SWITCH);
                }
                if clear != [0u8; 11] {
                    let _ = tps.clear_interrupts(i2c, &clear).await;
                }
            }
        }

        // Refresh the source-capability list whenever a negotiation is armed and
        // whenever the cache is empty. The latter covers a cable that was already
        // attached at power-up (so no plug interrupt is generated) and a
        // controller that came back after the watchdog re-probed it. Retry while
        // the source has not advertised its PDOs yet, and keep re-reading while
        // an `ESrC` EPR probe is waiting for the EPR PDOs to appear.
        let want_caps = self.negotiate_pending
            || app.pd_cap_count == 0
            || self.epr_probe_pending
            || self.caps_dirty;
        if self.present && want_caps && now >= self.next_caps_retry {
            app.pd_cap_count = tps.get_source_capabilities(i2c, &mut app.pd_caps).await;
            self.caps_dirty = false;
            for i in 0..app.pd_cap_count as usize {
                let c = app.pd_caps[i];
                defmt::info!(
                    "  cap[{}] {} mV {} mA pps={} avs={}",
                    i,
                    c.voltage_mv,
                    c.max_current_ma,
                    c.is_pps,
                    c.is_avs
                );
            }
            if let Some((spr, epr, last_epr)) = tps.source_cap_header(i2c).await {
                defmt::info!(
                    "PD source caps: SPR={} EPR={} last_epr={}",
                    spr,
                    epr,
                    last_epr
                );
                if epr > 0 && !self.epr_seen {
                    self.epr_seen = true;
                    self.epr_probe_pending = false;
                    // Re-plan once against the now-visible EPR PDOs, in both
                    // modes: `choose()` then keeps AVS if the source has an AVS
                    // APDO or switches to the fixed EPR PDO if it does not. The
                    // target rail is unchanged, so this re-request is not a
                    // clobber -- it is the same contract (or its fixed form)
                    // re-issued after entry.
                    self.negotiate_pending = true;
                    defmt::info!("EPR entered ({} EPR PDOs)", epr);
                    // Re-assert the 3V3 enable through the EPR transition.
                    let gpio_ok = tps.set_gpio_high(i2c, 6).await;
                    defmt::info!("TPS GPIO6 (3V3_EN_PD) held high: {}", gpio_ok);
                } else if epr == 0 && self.epr_seen {
                    // The controller left EPR: the source re-advertises its full
                    // SPR PDO set (15 V/20 V reappear), so a later EPR selection
                    // must re-probe.
                    self.epr_seen = false;
                    self.epr_probe_pending = false;
                    defmt::info!("EPR exited; SPR PDOs restored");
                }
            }
            if let Some(pdo) = tps.source_5v_pdo(i2c).await {
                defmt::info!(
                    "source 5V PDO={} epr_capable={}",
                    pdo,
                    (pdo >> 23) & 1 != 0
                );
            }
            if app.pd_cap_count == 0 {
                self.next_caps_retry = now + Duration::from_millis(CAPS_RETRY_MS);
            }
        }

        // 3. Choose the rail.
        let cap_count = app.pd_cap_count.min(app.pd_caps.len() as u8) as usize;
        let choice = self.choose(app, cap_count);

        // A UI confirm is a one-shot edge; log what it resolved to so a request
        // that is never issued (no caps, no rail, EPR latched off) is visible.
        if app.pd_control.renegotiate_request {
            defmt::info!(
                "PD confirm: mode={} preset={} caps={} choice={} mV epr_ok={}",
                match app.pd_control.mode {
                    PdMode::Manual => "manual",
                    PdMode::Auto => "auto",
                },
                app.ui.pd_profile_index,
                cap_count,
                choice.map(|c| c.rail_mv).unwrap_or(0),
                !self.epr_unavailable
            );
        }

        app.pd_control.error = if cap_count == 0 {
            PdAutoError::NoCable
        } else if choice.is_none() {
            PdAutoError::NoRail
        } else {
            PdAutoError::None
        };
        if let Some(ch) = choice {
            app.pd_control.target_mv = ch.rail_mv;
            app.pd_control.region = ch.region;
        } else {
            app.pd_control.target_mv = 0;
            app.pd_control.region = RailRegion::Unavailable;
        }

        let auto_due = app.pd_control.mode == PdMode::Auto
            // Freeze the rail for the whole output-voltage sweep: a mid-sweep
            // renegotiation would park the output and invalidate the run.
            && app.sweep.phase != SweepPhase::Running
            && choice
                .map(|ch| ch.rail_mv != self.requested_rail_mv)
                .unwrap_or(false)
            && now.duration_since(self.last_vset_change)
                >= Duration::from_millis(board::AUTO_TRACK_SETTLE_MS)
            && now.duration_since(self.last_renegotiate)
                >= Duration::from_millis(board::AUTO_TRACK_MIN_INTERVAL_MS);

        // A request is only *armed* by an explicit action: a queued negotiation
        // (boot / plug / EPR re-plan), a UI confirm, or Auto-tracking's own
        // re-plan. Merely moving the PD-screen highlight must never probe or
        // renegotiate — it did, because the EPR gate below was driven by
        // `choice` alone, and `choice` follows `pd_profile_index`. Scrolling
        // onto 28/36/48 V therefore ran `ESrC` and re-requested the rail with no
        // confirm, which is what tore down a live 48 V contract.
        let request_armed =
            self.negotiate_pending || app.pd_control.renegotiate_request || auto_due;

        // EPR entry gate. An above-SPR rail cannot be requested until the
        // controller is in EPR mode, and writing 0x37 + `GSrC` at that point
        // tears down the entry handshake. So run the `ESrC` probe and *hold the
        // request back* until the EPR PDOs show up in `RX_SOURCE_CAPS` — the
        // reference does exactly this at boot. The probe is not re-issued while
        // one is in flight.
        let wants_epr = choice.map(|ch| ch.epr).unwrap_or(false);
        if self.present && wants_epr && !self.epr_seen && request_armed {
            if !self.epr_probe_pending {
                let ok = tps.request_epr_source_caps(i2c).await;
                defmt::info!("EPR probe (ESrC) ok={}", ok);
                self.epr_probe_pending = true;
                self.epr_probe_at = now;
                self.next_caps_retry = now;
            } else if now.duration_since(self.epr_probe_at)
                >= Duration::from_millis(EPR_ENTRY_TIMEOUT_MS)
            {
                defmt::warn!("EPR entry not seen after ESrC; issuing request anyway");
                self.epr_probe_pending = false;
            }
        }
        let epr_entry_pending = wants_epr && !self.epr_seen && self.epr_probe_pending;

        // Only queue a new request once the previous one has been issued, only
        // while the controller is actually answering, only on an armed request,
        // and not while EPR mode entry is still in flight.
        let do_negotiate = self.present
            && self.pending.is_none()
            && !epr_entry_pending
            && request_armed
            && choice.is_some();

        if do_negotiate {
            if let Some(ch) = choice {
                if board::AUTO_TRACK_DISABLE_DURING_SWITCH
                    && app.supply.enabled
                    && app.supply.fault == Fault::None
                {
                    // The main loop runs a supply tick before `negotiate`, so
                    // the DACs are parked and the converter disabled first.
                    app.supply.enabled = false;
                    self.reenable_output = true;
                }
                self.pending = Some(ch);
            }
            self.negotiate_pending = false;
            app.pd_control.renegotiate_request = false;
        } else {
            // Keep the request armed while an above-SPR rail is still waiting
            // for EPR entry; otherwise stop re-reading every poll once the caps
            // are in.
            if self.negotiate_pending && cap_count > 0 && !epr_entry_pending {
                self.negotiate_pending = false;
            }
            // Keep the confirm armed while EPR mode entry is in flight: clearing
            // it here would disarm the probe (and its timeout) and leave the
            // request stuck with no way to proceed.
            if !epr_entry_pending {
                // A confirm that cannot be acted on now (no cable / no eligible
                // rail) is dropped rather than left armed to fire later.
                app.pd_control.renegotiate_request = false;
            }
        }

        // 4. Mirror the active contract.
        if self.present {
            if let Some((v_mv, i_ma)) = tps.get_active_contract(i2c).await {
                app.supply.input_current_cap_ma = i_ma;
                app.supply.input_power_cap_mw = v_mv.saturating_mul(i_ma) / 1000;
                app.telemetry.vin_mv = v_mv;
                if app.pd == PdState::Negotiating {
                    app.pd = PdState::ContractActive;
                }
                if v_mv != self.last_contract_mv {
                    let rdo = tps.active_rdo(i2c).await.unwrap_or(0);
                    defmt::info!(
                        "PD contract: {} mV @ {} mA rdo_epr={}",
                        v_mv,
                        i_ma,
                        (rdo >> 22) & 1 != 0
                    );
                    self.last_contract_mv = v_mv;
                }
                // Poll the power path directly: the POWER_PATH_SWITCH interrupt
                // is masked in the app config, so a switch to PP_EXT (or a fault
                // that disables it) would otherwise be invisible. Log every
                // change; this is what tells us whether the converter input is
                // being cut by the controller or by the source.
                if now >= self.next_pp_poll {
                    self.next_pp_poll = now + Duration::from_millis(PP_POLL_MS);
                    let mut pp = [0u8; 5];
                    if tps
                        .read_register(i2c, TPS_REG_POWER_PATH_STATUS, &mut pp)
                        .await
                    {
                        let raw = u32::from_le_bytes([pp[0], pp[1], pp[2], pp[3]]);
                        if raw != self.pp_status {
                            self.pp_status = raw;
                            let vin = app.telemetry.vin_mv;
                            defmt::info!(
                                "PD power path: PP5V={} PP_EXT={} VCONN={} b4=0x{:02x} contract={} mV vin={} mV",
                                (raw >> 6) & 0x07,
                                (raw >> 12) & 0x07,
                                raw & 0x03,
                                pp[4],
                                v_mv,
                                vin
                            );
                        }
                    }
                }
                // An EPR window that settled back at an SPR voltage means this
                // source (or cable) cannot enter EPR. Stop injecting EPR rails
                // so Auto-tracking and the manual presets fall back to the best
                // SPR rail instead of pinning the UI to a voltage the contract
                // never reached.
                if !self.epr_unavailable
                    && self.pending.is_none()
                    && self.requested_rail_mv > board::SPR_MAX_MV
                    && v_mv <= board::SPR_MAX_MV
                    && now.duration_since(self.last_renegotiate)
                        >= Duration::from_millis(SWITCH_REENABLE_TIMEOUT_MS)
                {
                    defmt::warn!(
                        "EPR unavailable: {} mV request settled at {} mV",
                        self.requested_rail_mv,
                        v_mv
                    );
                    self.epr_unavailable = true;
                    // Force the chooser to re-plan, even in Manual mode.
                    self.requested_rail_mv = 0;
                    self.negotiate_pending = true;
                }
                // EPR AVS keep-alive. An AVS contract is a programmable
                // contract: the source reverts to 5 V if it is not re-requested
                // inside the PD keep-alive window. Re-issue the same request
                // while the contract is up; drop the tracking once the contract
                // falls back to SPR (the EPR-unavailable path above takes over).
                if let Some((rail, cur, min_v, max_v)) = self.avs_keepalive {
                    if v_mv <= board::SPR_MAX_MV {
                        self.avs_keepalive = None;
                    } else if self.pending.is_none()
                        && v_mv.abs_diff(rail) <= (rail / 10).max(100)
                        && now >= self.next_avs_keepalive
                    {
                        let _ = tps
                            .request_avs_profile(i2c, rail, cur, min_v, max_v)
                            .await;
                        let _ = tps.trigger_renegotiation(i2c).await;
                        self.next_avs_keepalive = now + Duration::from_millis(AVS_KEEPALIVE_MS);
                        defmt::info!("PD AVS keep-alive: {} mV @ {} mA", rail, cur);
                    }
                }
                // Never undo the park for a request that has not been issued yet:
                // `requested_rail_mv` still refers to the previous contract.
                if self.pending.is_none() {
                    self.maybe_reenable(app, v_mv, now);
                }
            } else {
                // No active contract: an XT90 feed, a removed cable, or the
                // controller has not confirmed one yet. Use the design limits so
                // a non-PD feed gets the full 240 W rather than inheriting the
                // last contract's caps (or a zeroed register).
                use_design_input_limits(app);
                if app.pd == PdState::ContractActive {
                    // cable removed
                    app.pd = PdState::NoCable;
                    app.pd_cap_count = 0;
                }
            }
        } else {
            // Controller absent: it cannot hold a contract, so the input is not
            // PD-limited (XT90 feed or no PD source).
            app.pd = PdState::NoCable;
            use_design_input_limits(app);
        }

        // Safety net: never leave the output parked forever if the source
        // refuses to confirm the new contract.
        if self.pending.is_none()
            && self.reenable_output
            && now.duration_since(self.last_renegotiate)
                >= Duration::from_millis(SWITCH_REENABLE_TIMEOUT_MS)
        {
            app.supply.enabled = true;
            self.reenable_output = false;
        }

        let _ = Timer::after(Duration::from_millis(1)).await;
    }

    /// Issue the request queued by [`Self::poll`] and trigger renegotiation.
    ///
    /// Call this after a supply tick has run, so a temporary output disable is
    /// already applied to the DACs and the converter enable line.
    pub async fn negotiate(&mut self, tps: &mut Tps26750, i2c: &mut I2c<'_, Async, Master>) {
        let Some(ch) = self.pending.take() else {
            return;
        };
        if !self.present {
            // The watchdog said the controller is gone between `poll` and here;
            // drop the request rather than issue traffic to an absent chip.
            defmt::warn!("PD request dropped: TPS26750 absent");
            return;
        }

        defmt::info!(
            "PD request: rail={} mV, i={} mA, region={} avs={} pps={} epr={} max={}",
            ch.rail_mv,
            ch.current_ma,
            region_label(ch.region),
            ch.avs.is_some(),
            ch.pps.is_some(),
            ch.epr,
            ch.maximize
        );

        // EPR is never requested as a bare fixed >20 V window: that matches no
        // visible SPR PDO before EPR entry, so the controller picks 5 V as the
        // default (SDAA265 section 5.3). EPR mode entry is driven separately by
        // the `ESrC` probe in `poll`. By the time a request is queued either the
        // controller is in EPR, or the entry probe timed out.
        //
        // Above-SPR rails take one of three forms:
        // * `avs` set -> an EPR AVS contract on that window (sources with an AVS
        //   APDO and no fixed EPR PDO);
        // * `epr` + `maximize` -> the controller-computed wide window, which
        //   lands on the highest-power EPR PDO (the "MAX" action);
        // * `epr` + a specific rail -> the same ≥140 W forcing with a window
        //   narrowed to that EPR PDO.
        // Both fixed forms keep EPR alive and are re-evaluated with the PPS-bit
        // edge, never `GSrC` (which restarts SPR negotiation and drops EPR).
        let ok = if let Some(win) = ch.avs {
            tps.request_avs_profile(i2c, ch.rail_mv, ch.current_ma, win.min_mv, win.max_mv)
                .await
        } else if ch.epr && ch.maximize {
            // Wide host window + ≥140 W, `avs_en` clear. This is the MAX request
            // that actually holds the top rail on the Anker (28 V); the
            // auto-compute/`avs_en` variant (`request_max_rail`) settled at 20 V
            // on both the Anker and the 240 W.
            tps.request_fixed_epr_profile(i2c, ch.current_ma).await
        } else if ch.epr {
            tps.request_epr_rail_fixed(i2c, ch.rail_mv, ch.current_ma)
                .await
        } else if let Some(win) = ch.pps {
            tps.request_pps_profile(i2c, ch.rail_mv, ch.current_ma, win.min_mv, win.max_mv)
                .await
        } else {
            tps.request_fixed_profile(i2c, ch.rail_mv, ch.current_ma)
                .await
        };
        if ok {
            // An EPR request already re-evaluated the register with a PPS-bit
            // edge inside `request_*_profile`. `GSrC` must NOT be issued for it:
            // it re-fetches the SPR source caps and restarts SPR negotiation,
            // which drops the controller out of EPR (field-verified: the 28 V
            // EPR PDO was visible and the fixed EPR request still fell to 5 V).
            let trig_ok = if ch.epr {
                true
            } else {
                tps.trigger_renegotiation(i2c).await
            };
            // Read 0x37 straight back: if the write did not stick (bus NACK,
            // controller busy/faulted) the request has no effect and the
            // contract will not move. Read the whole 24-byte register so the AVS
            // (byte 16) and PPS (byte 8) enable bits can be reported.
            let mut an = [0u8; 24];
            if tps
                .read_register(i2c, TPS_REG_AUTONEGOTIATE_SINK, &mut an)
                .await
            {
                let max_v = ((an[4] as u32) | (((an[5] & 0x03) as u32) << 8)) * 50;
                defmt::info!(
                    "PD 0x37 now: b0=0x{:02x} avs_en={} pps_en={} maxV={} mV trig_ok={}",
                    an[0],
                    an[16] & 1 != 0,
                    an[8] & 1 != 0,
                    max_v,
                    trig_ok
                );
            }
            self.requested_rail_mv = ch.rail_mv;
            // The advertised PDO set changes on EPR entry and exit; re-read it
            // once so the chooser does not keep using a stale list.
            self.caps_dirty = true;
            // Track (AVS) or drop (fixed/PPS) the keep-alive with the request
            // type. A live EPR AVS contract must be refreshed periodically.
            self.avs_keepalive = ch
                .avs
                .map(|win| (ch.rail_mv, ch.current_ma, win.min_mv, win.max_mv));
            self.next_avs_keepalive = Instant::now() + Duration::from_millis(AVS_KEEPALIVE_MS);
        } else {
            // Leave `requested_rail_mv` on the previous contract: `maybe_reenable`
            // then sees the still-active rail as "close enough" and restores the
            // output, and auto-tracking retries after the minimum interval.
            defmt::warn!("PD request failed for rail={} mV", ch.rail_mv);
        }
        self.last_renegotiate = Instant::now();
    }

    /// Manual preset → nearest fixed PDO; Auto → the auto-track chooser;
    /// `max_request` (Manual only) → the highest rail the source offers.
    fn choose(&self, app: &AppState, cap_count: usize) -> Option<AutoChoice> {
        if cap_count == 0 {
            return None;
        }
        let caps = &app.pd_caps[..cap_count];
        let allow_epr = !self.epr_unavailable;
        let mut choice = if app.pd_control.max_request && app.pd_control.mode == PdMode::Manual {
            auto_track::choose_highest(caps, allow_epr)
        } else {
            match app.pd_control.mode {
                PdMode::Auto => auto_track::choose_rail(
                    app.supply.v_set_mv,
                    app.supply.i_set_ma,
                    caps,
                    app.pd_control.policy,
                    allow_epr,
                ),
                PdMode::Manual => {
                    let idx =
                        (app.ui.pd_profile_index as usize).min(PD_PRESET_VOLTAGES_MV.len() - 1);
                    auto_track::choose_nearest(PD_PRESET_VOLTAGES_MV[idx], caps, allow_epr)
                }
            }
        };

        // Refine an above-SPR choice now that the source's own EPR PDOs may be
        // visible.
        //
        // Prefer the **fixed** EPR PDO whenever the source advertises one: the
        // AVS path is tried first otherwise, and field-tested a source that
        // offers fixed 28/36/48 V *plus* an EPR AVS APDO (a 240 W charger)
        // resolved every AVS request to 20 V. The fixed path with the ≥140 W
        // requirement is the one that reaches EPR. AVS is kept only for sources
        // that expose an EPR AVS APDO and no fixed EPR PDO.
        if let Some(ch) = choice.as_mut() {
            if ch.epr {
                let has_fixed_epr = caps
                    .iter()
                    .any(|c| !c.is_pps && !c.is_avs && c.voltage_mv > board::SPR_MAX_MV);
                let avs_apdo = caps
                    .iter()
                    .find(|c| c.is_avs && c.voltage_mv > board::SPR_MAX_MV);
                if self.epr_seen && (has_fixed_epr || avs_apdo.is_none()) {
                    // Fixed EPR PDO path (also the case where nothing AVS-shaped
                    // is advertised): `negotiate` picks wide vs targeted.
                    ch.avs = None;
                } else if let Some(apdo) = avs_apdo {
                    ch.avs = Some(auto_track::ApdoWindow {
                        min_mv: apdo.min_voltage_mv.min(ch.rail_mv),
                        max_mv: apdo.voltage_mv.max(ch.rail_mv),
                    });
                }
            }
        }
        choice
    }

    /// Read and log `POWER_PATH_STATUS` (0x26): which power-path switch is
    /// actually closed. For a sink, `PP_EXT` (bits 14-12) should read `3h`
    /// (enabled, system input); `0h` is disabled and `1h` is *disabled due to
    /// fault*. A `1h`/`0h` there is exactly "converter input gone while VBUS is
    /// still valid".
    async fn log_power_path(&self, tps: &mut Tps26750, i2c: &mut I2c<'_, Async, Master>) {
        let mut buf = [0u8; 5];
        if !tps
            .read_register(i2c, TPS_REG_POWER_PATH_STATUS, &mut buf)
            .await
        {
            return;
        }
        let raw = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let pp_ext = (raw >> 12) & 0x07;
        let pp5v = (raw >> 6) & 0x07;
        let pp_cable = raw & 0x03;
        defmt::info!(
            "PD power path: PP5V={} PP_EXT={} VCONN={} src={} b4=0x{:02x}",
            pp5v,
            pp_ext,
            pp_cable,
            (buf[4] >> 6) & 0x03,
            buf[4]
        );
    }

    /// Re-enable the output once the requested rail is actually active.
    fn maybe_reenable(&mut self, app: &mut AppState, active_mv: u32, now: Instant) {
        if !self.reenable_output {
            return;
        }
        let target = self.requested_rail_mv;
        let is_epr = active_mv > board::SPR_MAX_MV;
        // Do not load the converter the instant an EPR contract appears: some
        // sources are still settling VBUS through the 20 V -> 28/48 V
        // transition, and pulling load then can dip VBUS hard enough to brown the
        // board out (two chargers did; a powerbank at the same 28 V did not).
        // Hold the output off for a settle window first.
        if is_epr && now.duration_since(self.last_renegotiate) < Duration::from_millis(board::EPR_SETTLE_MS)
        {
            return;
        }
        let close_enough = (target > 0 && active_mv.abs_diff(target) <= (target / 10).max(100))
            // An EPR mode-entry request names no rail: the controller picks the
            // highest-power EPR PDO, so accept any EPR contract as satisfying it.
            || is_epr;
        if close_enough
            || now.duration_since(self.last_renegotiate)
                >= Duration::from_millis(SWITCH_REENABLE_TIMEOUT_MS)
        {
            app.supply.enabled = true;
            self.reenable_output = false;
        }
    }
}

impl Default for PdManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Short static label for defmt logging.
fn region_label(region: RailRegion) -> &'static str {
    match region {
        RailRegion::Buck => "buck",
        RailRegion::Boost => "boost",
        RailRegion::FallbackPower => "fallback",
        RailRegion::Unavailable => "none",
    }
}
