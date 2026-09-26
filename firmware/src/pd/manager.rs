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
    Tps26750, AVS_REQUEST_STEP_MV, TPS_INT_NEW_CONTRACT_AS_SINK, TPS_INT_PLUG_INSERT_REMOVAL,
    TPS_INT_POWER_PATH_SWITCH, TPS_INT_SOURCE_CAP_RX, TPS_REG_AUTONEGOTIATE_SINK, TPS_REG_MODE,
    TPS_REG_POWER_PATH_STATUS, TPS_REG_TX_SINK_CAPS,
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
/// `POWER_PATH_STATUS` (0x26) poll period while a contract is active. The
/// `POWER_PATH_SWITCH` interrupt is masked in the app config, so the register is
/// polled instead and logged on change.
const PP_POLL_MS: u64 = 500;
/// How long one step of the staged EPR→SPR exit may take before the next step is
/// issued anyway. The exit must never deadlock: a source that refuses to confirm
/// an intermediate AVS/5 V contract still gets the target requested.
const EPR_EXIT_STEP_TIMEOUT_MS: u64 = 2_000;
/// How many times an above-SPR request that settled back at SPR is retried
/// (with a clean 5 V release between attempts) before EPR is latched off for the
/// current cable.
const EPR_RETRY_LIMIT: u8 = 2;

/// One step of the staged EPR→SPR exit.
///
/// A request that drops a live above-SPR contract straight to an SPR rail can
/// make some sources collapse VBUS, which on this board also takes the MCU's 3V3
/// buck down (its input is the PD rail). The reference PD240W firmware works
/// around the same behaviour with a three-step exit; this mirrors it:
/// EPR AVS step-down (if the source offers a reachable AVS APDO) → 5 V fixed →
/// the original target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EprExitStep {
    /// No exit in progress; `negotiate` handles the queue normally.
    None,
    /// Stepping down inside EPR with an AVS contract.
    SteppingDown,
    /// Requesting the 5 V fixed PDO.
    Requesting5v,
}

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
    /// advertises EPR PDOs).
    epr_seen: bool,
    /// A request was issued, so the source's advertised PDO set may have changed
    /// (EPR mode entry *and* exit both change it). Forces one capability re-read.
    caps_dirty: bool,
    /// Live EPR AVS contract to keep alive: `(rail_mv, current_ma)`. `Some` only
    /// while an above-SPR AVS contract is requested.
    avs_keepalive: Option<(u32, u32)>,
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
    /// Staged EPR→SPR exit state (see [`EprExitStep`]).
    epr_exit_step: EprExitStep,
    /// The SPR request deferred until the staged exit finishes.
    epr_exit_target: Option<AutoChoice>,
    /// Time the current exit step was issued, for [`EPR_EXIT_STEP_TIMEOUT_MS`].
    epr_exit_at: Instant,
    /// The staged exit has already run for the current EPR contract. Prevents a
    /// source that ignores the intermediate requests from looping the exit
    /// forever; cleared as soon as any SPR contract is active again.
    epr_exit_done: bool,
    /// The last request issued was a PPS contract. A live PPS contract has to be
    /// left *before* the EPR policy is written — see [`Self::negotiate`].
    pps_requested: bool,
    /// The pending request is the boot request: use the highest rail rather than
    /// the UI preset. Cleared once the request is issued.
    boot_high: bool,
    /// An EPR request deferred while a live PPS contract is released.
    epr_prep_target: Option<AutoChoice>,
    /// True while the "leave PPS first" step is in flight.
    epr_prep: bool,
    /// Time the prep step was issued.
    epr_prep_at: Instant,
    /// Intermediate SPR rail the prep step is waiting for.
    epr_prep_spr_mv: u32,
    /// Above-SPR requests that settled at SPR since the last live EPR contract.
    epr_retries: u8,
    /// The next EPR request must first force a clean 5 V release.
    force_release: bool,
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

/// Highest fixed SPR PDO the source advertises, used as the intermediate
/// contract before an EPR request. Field-verified: the controller escalates into
/// EPR from the top of its SPR range — the one run that reached 48 V did so from
/// a 20 V contract, while the same request from 5 V (or from a live 12 V PPS
/// contract) settled at 20 V.
fn highest_spr_mv(app: &AppState) -> u32 {
    let n = app.pd_cap_count.min(app.pd_caps.len() as u8) as usize;
    app.pd_caps[..n]
        .iter()
        .filter(|c| {
            !c.is_pps && !c.is_avs && c.voltage_mv > 0 && c.voltage_mv <= board::SPR_MAX_MV
        })
        .map(|c| c.voltage_mv)
        .max()
        .unwrap_or(board::SPR_MAX_MV)
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
            caps_dirty: false,
            avs_keepalive: None,
            next_avs_keepalive: Instant::now(),
            last_contract_mv: 0,
            pp_status: 0,
            next_pp_poll: Instant::now(),
            epr_exit_step: EprExitStep::None,
            epr_exit_target: None,
            epr_exit_at: Instant::now(),
            epr_exit_done: false,
            pps_requested: false,
            boot_high: false,
            epr_prep_target: None,
            epr_prep: false,
            epr_prep_at: Instant::now(),
            epr_prep_spr_mv: 0,
            epr_retries: 0,
            force_release: false,
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
                    let active = tps.get_active_contract(i2c).await;
                    let already_epr = active
                        .map(|(v, _)| v > board::SPR_MAX_MV)
                        .unwrap_or(false);
                    if already_epr {
                        defmt::info!("PD: controller already in EPR at boot; keeping its rail");
                        self.negotiate_pending = false;
                        // Seed the requested rail with the live contract so
                        // Auto-tracking does not immediately see a "different"
                        // rail and re-request it — the old boot path pulled a
                        // perfectly good 48 V contract down to preset 0 (12 V).
                        if let Some((v, i)) = active {
                            self.requested_rail_mv = v;
                            app.supply.input_current_cap_ma = i;
                            app.supply.input_power_cap_mw = v.saturating_mul(i) / 1000;
                        }
                    } else {
                        // No live EPR rail: ask for the **highest** rail the
                        // source offers, not the UI preset. The preset default is
                        // index 0 = 12 V, and writing that 12 V PPS window into
                        // 0x37 persists across MCU resets — so every boot started
                        // from 12 V and re-asserted it. `boot_high` is cleared as
                        // soon as this first request is issued, and an explicit UI
                        // confirm still wins over it.
                        self.negotiate_pending = true;
                        self.boot_high = true;
                    }
                }
            } else {
                self.probe_failures = self.probe_failures.saturating_add(1);
                if self.present && self.probe_failures >= TPS_LOST_FAILURES {
                    defmt::warn!("TPS26750 lost");
                    app.pd_cap_count = 0;
                    self.negotiate_pending = false;
                    self.avs_keepalive = None;
                    self.pps_requested = false;
                    self.epr_prep = false;
                    self.epr_prep_target = None;
                    self.epr_retries = 0;
                    self.force_release = false;
                    self.present = false;
                    use_design_input_limits(app);
                }
            }
        }

        // Publish the watchdog's verdict after the probe block above so the
        // status LED (and the UI) see this poll's result, not the previous one.
        app.pd_present = self.present;

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
                    self.avs_keepalive = None;
                    // Abandon any staged exit: the contract it was stepping away
                    // from no longer exists.
                    self.epr_exit_step = EprExitStep::None;
                    self.epr_exit_target = None;
                    self.epr_exit_done = false;
                    self.epr_prep = false;
                    self.epr_prep_target = None;
                    self.pps_requested = false;
                    self.epr_retries = 0;
                    self.force_release = false;
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
        // the source has not advertised its PDOs yet.
        let want_caps = self.negotiate_pending
            || app.pd_cap_count == 0
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
                    // Deliberately do NOT arm a re-plan here. EPR mode entry
                    // re-advertises the PDO set, and re-requesting on entry is
                    // what pulled a live 48 V contract back down to the UI preset
                    // (12 V, served by PPS).
                    defmt::info!("EPR entered ({} EPR PDOs)", epr);
                    // Re-assert the 3V3 enable through the EPR transition.
                    let gpio_ok = tps.set_gpio_high(i2c, 6).await;
                    defmt::info!("TPS GPIO6 (3V3_EN_PD) held high: {}", gpio_ok);
                } else if epr == 0 && self.epr_seen {
                    // The controller left EPR: the source re-advertises its full
                    // SPR PDO set (15 V/20 V reappear), so a later EPR selection
                    // must re-probe. This is the only place `epr_exit_done` is
                    // cleared in normal operation: an EPR episode gets exactly one
                    // staged exit, and the next episode re-arms it.
                    self.epr_seen = false;
                    self.epr_exit_done = false;
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

        // EPR mode entry is NOT done here, and the request is NOT held back for
        // it. `negotiate` writes the 0x37 policy first and then issues `ESrC`, so
        // the controller enters EPR with the correct policy already in place —
        // exactly the order it uses at power-up. Entering EPR with the stale
        // register and writing 0x37 afterwards was why every above-SPR request
        // still settled at 20 V (observed: `ESrC` -> `EPR entered` -> policy
        // write -> `maxV` recomputed to 20000).

        // Only queue a new request once the previous one has been issued, and
        // only while the controller is actually answering.
        let do_negotiate = self.present
            && self.pending.is_none()
            && self.epr_exit_step == EprExitStep::None
            && !self.epr_prep
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
            // Stop re-reading every poll once the caps are in.
            if self.negotiate_pending && cap_count > 0 {
                self.negotiate_pending = false;
            }
            // A confirm that cannot be acted on now (no cable / no eligible
            // rail) is dropped rather than left armed to fire later.
            app.pd_control.renegotiate_request = false;
        }

        // 4. Mirror the active contract.
        if self.present {
            if let Some((v_mv, i_ma)) = tps.get_active_contract(i2c).await {
                // Do NOT clear `epr_exit_done` on an SPR contract. It must
                // survive until EPR has actually been *observed* to exit
                // (`EPR exited` in the caps block), otherwise the re-queued SPR
                // target re-triggers the staged exit while `epr_seen` is still
                // stale-true, and the firmware loops 5 V -> AVS -> 5 V forever.
                if v_mv > board::SPR_MAX_MV {
                    // A live EPR contract clears the retry budget.
                    self.epr_retries = 0;
                    self.force_release = false;
                }
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
                    && self.epr_exit_step == EprExitStep::None
                    && self.pending.is_none()
                    && self.requested_rail_mv > board::SPR_MAX_MV
                    && v_mv <= board::SPR_MAX_MV
                    && now.duration_since(self.last_renegotiate)
                        >= Duration::from_millis(SWITCH_REENABLE_TIMEOUT_MS)
                {
                    // EPR entry is flaky on this controller: the same request
                    // sometimes reaches the rail and sometimes settles at 20 V.
                    // Retry a bounded number of times with a clean 5 V release
                    // (the state every successful entry shared) before latching
                    // EPR off for this cable.
                    if self.epr_retries < EPR_RETRY_LIMIT {
                        self.epr_retries += 1;
                        defmt::warn!(
                            "EPR retry {}/{}: {} mV settled at {} mV",
                            self.epr_retries,
                            EPR_RETRY_LIMIT,
                            self.requested_rail_mv,
                            v_mv
                        );
                        self.requested_rail_mv = 0;
                        self.force_release = true;
                        self.negotiate_pending = true;
                    } else {
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
                }
                // EPR AVS keep-alive. An AVS contract is a programmable
                // contract: the source reverts to 5 V if it is not re-requested
                // inside the PD keep-alive window. Re-issue the same request
                // while the contract is up; drop the tracking once the contract
                // falls back to SPR (the EPR-unavailable path above takes over).
                if let Some((rail, cur)) = self.avs_keepalive {
                    if v_mv <= board::SPR_MAX_MV {
                        self.avs_keepalive = None;
                    } else if self.pending.is_none()
                        && v_mv.abs_diff(rail) <= (rail / 10).max(100)
                        && now >= self.next_avs_keepalive
                    {
                        let _ = tps.request_avs_profile(i2c, rail, cur).await;
                        let _ = tps.request_epr_source_caps(i2c).await;
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
    pub async fn negotiate(
        &mut self,
        tps: &mut Tps26750,
        i2c: &mut I2c<'_, Async, Master>,
        app: &AppState,
    ) {
        // A staged EPR→SPR exit owns negotiation until it finishes; it issues its
        // own intermediate requests and re-queues the target at the end.
        if self.epr_exit_step != EprExitStep::None {
            self.step_epr_exit(tps, i2c).await;
            return;
        }
        // A live PPS contract is released before an EPR request; see the prep
        // step further down.
        if self.epr_prep {
            self.step_epr_prep(tps, i2c).await;
            return;
        }

        let Some(ch) = self.pending.take() else {
            return;
        };
        self.boot_high = false;
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

        // Leaving EPR for an SPR rail. Use the staged walk-down whenever EPR is
        // *entered*, not only when the live contract is above 20 V: field-verified
        // that a direct exit from a 20 V EPR contract (which is not `> SPR_MAX`)
        // leaves the controller unable to re-enter EPR, while the same 36 V
        // preset works when the previous exit went through the 5 V step.
        if !ch.epr {
            let active_mv = tps
                .get_active_contract(i2c)
                .await
                .map(|(v, _)| v)
                .unwrap_or(0);
            if (active_mv > board::SPR_MAX_MV || self.epr_seen) && !self.epr_exit_done {
                defmt::info!(
                    "PD: staged EPR exit {} mV -> {} mV (direct transitions brown out sources)",
                    active_mv,
                    ch.rail_mv
                );
                self.begin_epr_exit(ch, app, tps, i2c).await;
                return;
            }
        }

        // MAX while the controller already sits on an EPR rail: that is exactly
        // what MAX asks for, so leave it alone. Re-writing 0x37 here is what
        // knocked the controller back to 20 V.
        if ch.epr && ch.maximize {
            let active_mv = tps
                .get_active_contract(i2c)
                .await
                .map(|(v, _)| v)
                .unwrap_or(0);
            if active_mv > board::SPR_MAX_MV {
                defmt::info!(
                    "PD: MAX already on an EPR rail ({} mV); leaving it",
                    active_mv
                );
                self.requested_rail_mv = active_mv;
                self.caps_dirty = true;
                self.last_renegotiate = Instant::now();
                return;
            }
        }

        // A live PPS contract must be released *before* the EPR policy is
        // written. Disabling PPS in 0x37 while a Sink PPS contract is active
        // makes the controller auto-re-evaluate (TRM §6.4) and fall to the best
        // SPR fixed PDO — 20 V — before `ESrC` can enter EPR. Field-verified:
        // MAX from a 12 V PPS contract always landed at 20 V, while the same
        // request from a fixed rail reached 48 V. Move to the **highest fixed SPR
        // PDO** first (the controller escalates into EPR from the top of its SPR
        // range), then write the policy and `ESrC`.
        //
        // The same step is reused to retry an EPR entry that settled at SPR
        // (`force_release`): there it goes to 5 V, because a clean 5 V release is
        // what the successful re-entries all had in common.
        if ch.epr && (self.pps_requested || self.force_release) {
            let spr_mv = if self.force_release {
                5_000
            } else {
                highest_spr_mv(app)
            };
            self.force_release = false;
            self.epr_prep_spr_mv = spr_mv;
            let _ = tps.request_fixed_profile(i2c, spr_mv, ch.current_ma).await;
            let _ = tps.trigger_renegotiation(i2c).await;
            self.pps_requested = false;
            self.epr_prep_target = Some(ch);
            self.epr_prep = true;
            self.epr_prep_at = Instant::now();
            defmt::info!("PD: releasing SPR before EPR entry (target {} mV)", spr_mv);
            return;
        }

        // EPR is never requested as a bare fixed >20 V window: that matches no
        // visible SPR PDO before EPR entry, so the controller picks 5 V as the
        // default (SDAA265 §5.3). Above-SPR rails take one of three forms:
        // * `avs` set -> an EPR AVS contract at that rail, with auto-compute left
        //   on (the EEPROM's own winning policy);
        // * `epr` + `maximize` -> hand voltage selection back to the controller
        //   so its own autonegotiation reproduces the power-up (EEPROM) result;
        // * `epr` + a specific rail -> a narrow host window around that fixed EPR
        //   PDO with `NoCapabilityMismatch` left set.
        let ok = if ch.avs.is_some() {
            tps.request_avs_profile(i2c, ch.rail_mv, ch.current_ma)
                .await
        } else if ch.epr && ch.maximize {
            tps.restore_autonegotiate(i2c, ch.current_ma).await
        } else if ch.epr {
            tps.request_fixed_epr_rail(i2c, ch.rail_mv, ch.current_ma)
                .await
        } else if let Some(win) = ch.pps {
            tps.request_pps_profile(i2c, ch.rail_mv, ch.current_ma, win.min_mv, win.max_mv)
                .await
        } else {
            tps.request_fixed_profile(i2c, ch.rail_mv, ch.current_ma)
                .await
        };
        self.pps_requested = ch.pps.is_some();
        if ok {
            // An above-SPR request must be re-evaluated with `ESrC`, not `GSrC`
            // and not the PPS-bit edge. Field-verified on the 240 W:
            //   * the PPS-bit edge briefly enables PPS, which outranks EPR and
            //     caps at 21 V;
            //   * `GSrC` issues Get_Source_Cap, so the source answers with its
            //     **SPR** capabilities, the controller drops EPR
            //     (`SPR=6 EPR=0`, `maxV` 51000 -> 20000) and lands at 20 V.
            // `ESrC` is the task that (re-)reads the EPR capabilities, so the
            // controller re-runs its EPR policy from the updated 0x37.
            // SPR/PPS requests still use `GSrC` (TRM §2.3).
            let trig_ok = if ch.epr {
                tps.request_epr_source_caps(i2c).await
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
            self.avs_keepalive = ch.avs.map(|_| (ch.rail_mv, ch.current_ma));
            self.next_avs_keepalive = Instant::now() + Duration::from_millis(AVS_KEEPALIVE_MS);
        } else {
            // Leave `requested_rail_mv` on the previous contract: `maybe_reenable`
            // then sees the still-active rail as "close enough" and restores the
            // output, and auto-tracking retries after the minimum interval.
            defmt::warn!("PD request failed for rail={} mV", ch.rail_mv);
        }
        self.last_renegotiate = Instant::now();
    }

    /// Begin a staged EPR→SPR exit for `target`.
    ///
    /// Step 1 is an EPR **AVS** request down to the source APDO's floor when that
    /// floor reaches into SPR (so VBUS comes down inside EPR first); otherwise
    /// the 5 V fixed PDO is requested directly, as the reference does when no
    /// AVS APDO is available.
    async fn begin_epr_exit(
        &mut self,
        target: AutoChoice,
        app: &AppState,
        tps: &mut Tps26750,
        i2c: &mut I2c<'_, Async, Master>,
    ) {
        self.epr_exit_target = Some(target);
        self.epr_exit_at = Instant::now();
        // The AVS keep-alive would re-issue the high AVS contract between exit
        // steps and fight the step-down.
        self.avs_keepalive = None;

        let cap_count = app.pd_cap_count.min(app.pd_caps.len() as u8) as usize;
        let apdo = app.pd_caps[..cap_count].iter().find(|c| {
            c.is_avs && c.voltage_mv > board::SPR_MAX_MV && c.min_voltage_mv <= board::SPR_MAX_MV
        });

        if let Some(apdo) = apdo {
            // Align the floor up to the AVS step so the request is inside the APDO.
            let floor = apdo
                .min_voltage_mv
                .div_ceil(AVS_REQUEST_STEP_MV)
                .saturating_mul(AVS_REQUEST_STEP_MV)
                .min(apdo.voltage_mv);
            let cur = apdo.max_current_ma.min(board::IOUT_MAX_MA);
            let _ = tps.request_avs_profile(i2c, floor, cur).await;
            let _ = tps.request_epr_source_caps(i2c).await;
            self.epr_exit_step = EprExitStep::SteppingDown;
            defmt::info!("PD: EPR exit step 1/3: AVS down to {} mV", floor);
        } else {
            let cur = target.current_ma;
            let _ = tps.request_fixed_profile(i2c, 5_000, cur).await;
            let _ = tps.trigger_renegotiation(i2c).await;
            self.epr_exit_step = EprExitStep::Requesting5v;
            defmt::warn!("PD: EPR exit: no reachable AVS APDO; 5 V fixed directly");
        }
    }

    /// Advance an in-flight EPR→SPR exit. Each step is bounded by
    /// [`EPR_EXIT_STEP_TIMEOUT_MS`] so a source that never confirms cannot strand
    /// the sequence with the output parked.
    async fn step_epr_exit(&mut self, tps: &mut Tps26750, i2c: &mut I2c<'_, Async, Master>) {
        let now = Instant::now();
        let timed_out = now.duration_since(self.epr_exit_at)
            >= Duration::from_millis(EPR_EXIT_STEP_TIMEOUT_MS);
        let active_mv = tps
            .get_active_contract(i2c)
            .await
            .map(|(v, _)| v)
            .unwrap_or(0);

        match self.epr_exit_step {
            EprExitStep::None => {}
            EprExitStep::SteppingDown => {
                if active_mv <= board::SPR_MAX_MV || timed_out {
                    let cur = self.epr_exit_target.map(|t| t.current_ma).unwrap_or(5_000);
                    let _ = tps.request_fixed_profile(i2c, 5_000, cur).await;
                    let _ = tps.trigger_renegotiation(i2c).await;
                    self.epr_exit_step = EprExitStep::Requesting5v;
                    self.epr_exit_at = now;
                    defmt::info!("PD: EPR exit step 2/3: {} mV -> 5 V", active_mv);
                }
            }
            EprExitStep::Requesting5v => {
                // `active_mv == 0` means the controller reports no contract at
                // all (e.g. the source renegotiated); proceed rather than hang.
                if active_mv <= 5_500 || active_mv == 0 || timed_out {
                    self.epr_exit_step = EprExitStep::None;
                    self.epr_exit_done = true;
                    self.pending = self.epr_exit_target.take();
                    self.last_renegotiate = now;
                    defmt::info!("PD: EPR exit step 3/3: 5 V -> target re-queued");
                }
            }
        }
    }

    /// Advance the "leave PPS before EPR" step: once the controller reports a
    /// fixed rail, re-queue the deferred EPR request so the normal path writes
    /// the 0x37 policy and issues `ESrC` with no PPS contract live.
    async fn step_epr_prep(&mut self, tps: &mut Tps26750, i2c: &mut I2c<'_, Async, Master>) {
        let now = Instant::now();
        let timed_out = now.duration_since(self.epr_prep_at)
            >= Duration::from_millis(EPR_EXIT_STEP_TIMEOUT_MS);
        let active_mv = tps
            .get_active_contract(i2c)
            .await
            .map(|(v, _)| v)
            .unwrap_or(0);
        // Wait for the requested SPR rail (or any fixed rail if the source
        // cannot serve it). Never hang: the timeout re-queues regardless.
        let landed = active_mv > 0
            && active_mv <= board::SPR_MAX_MV
            && active_mv.abs_diff(self.epr_prep_spr_mv)
                <= (self.epr_prep_spr_mv / 10).max(500);
        if landed || active_mv == 0 || timed_out {
            self.epr_prep = false;
            self.pending = self.epr_prep_target.take();
            defmt::info!("PD: PPS released ({} mV); EPR request re-queued", active_mv);
        }
    }

    /// Manual preset → nearest fixed PDO; Auto → the auto-track chooser;
    /// `max_request` (Manual only) → the highest rail the source offers.
    fn choose(&self, app: &AppState, cap_count: usize) -> Option<AutoChoice> {
        if cap_count == 0 {
            return None;
        }
        let caps = &app.pd_caps[..cap_count];
        let allow_epr = !self.epr_unavailable;
        // MAX in Manual, or the boot request: highest rail the source offers.
        // An explicit UI confirm (`renegotiate_request`) outranks `boot_high`.
        let want_highest = (app.pd_control.max_request && app.pd_control.mode == PdMode::Manual)
            || (self.boot_high && !app.pd_control.renegotiate_request);
        let mut choice = if want_highest {
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
        // Prefer the **EPR AVS** path whenever the source advertises an AVS APDO.
        // `request_avs_profile` keeps `AutoComputeSinkMaxVoltage` set, which is
        // what lets the controller select above 20 V at all (SDAA265 §5.2): the
        // boot EEPROM policy negotiates a 48 V AVS contract on the 240 W, while
        // every auto-compute-off host window (fixed or AVS) settled at 20 V. The
        // fixed-EPR window is used only for sources with no AVS APDO (e.g. the
        // Anker 737), where it is the proven path.
        if let Some(ch) = choice.as_mut() {
            if ch.epr {
                let avs_apdo = caps
                    .iter()
                    .find(|c| c.is_avs && c.voltage_mv > board::SPR_MAX_MV);
                if let Some(apdo) = avs_apdo {
                    ch.avs = Some(auto_track::ApdoWindow {
                        min_mv: apdo.min_voltage_mv.min(ch.rail_mv),
                        max_mv: apdo.voltage_mv.max(ch.rail_mv),
                    });
                } else if self.epr_seen {
                    // No AVS APDO: request the fixed EPR PDO through a host
                    // window. Before EPR entry the declared AVS window is kept,
                    // because that is what makes the controller attempt entry.
                    ch.avs = None;
                }
            } else if self.epr_seen {
                // Keep an already-entered EPR session alive. Field-verified: this
                // controller re-enters EPR mode from a host request but will
                // **not** re-select an above-20 V contract after a host-initiated
                // EPR exit (`EPR entered (4 EPR PDOs)` with `maxV=51000` written,
                // contract stuck at 20000). A preset inside the source's EPR AVS
                // APDO is therefore served as an AVS contract *inside* EPR rather
                // than as a fixed SPR PDO, so EPR is never the one-way door.
                // Rails below the AVS floor (12 V on this source) still exit.
                let avs_apdo = caps.iter().find(|c| {
                    c.is_avs
                        && c.voltage_mv > board::SPR_MAX_MV
                        && c.min_voltage_mv <= ch.rail_mv
                        && ch.rail_mv <= c.voltage_mv
                });
                if let Some(apdo) = avs_apdo {
                    defmt::info!("PD: keeping EPR alive for {} mV (AVS inside EPR)", ch.rail_mv);
                    ch.avs = Some(auto_track::ApdoWindow {
                        min_mv: ch.rail_mv,
                        max_mv: apdo.voltage_mv.max(ch.rail_mv),
                    });
                    ch.epr = true;
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
