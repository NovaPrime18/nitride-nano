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
    Tps26750, TPS_INT_NEW_CONTRACT_AS_SINK, TPS_INT_PLUG_INSERT_REMOVAL,
};
use crate::pd::auto_track::{self, AutoChoice};
use crate::state::{
    AppState, Fault, PdAutoError, PdMode, PdState, RailRegion, PD_PRESET_VOLTAGES_MV,
};

/// How long to wait for a newly requested contract before re-enabling the
/// output anyway (a source that never confirms must not leave the supply off).
const SWITCH_REENABLE_TIMEOUT_MS: u64 = 3_000;
/// Retry period while the source-capability list is still empty (the source
/// may not have sent Source_Capabilities yet right after a plug event).
const CAPS_RETRY_MS: u64 = 500;

/// Stateful wrapper around the TPS26750 driver, polled from the main loop.
pub struct PdManager {
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
}

impl PdManager {
    pub fn new() -> Self {
        Self {
            negotiate_pending: false,
            next_caps_retry: Instant::now(),
            last_vset_mv: 0,
            last_iset_ma: 0,
            last_vset_change: Instant::now(),
            requested_rail_mv: 0,
            last_renegotiate: Instant::now(),
            pending: None,
            reenable_output: false,
        }
    }

    /// True when [`Self::negotiate`] has work queued (used by the main loop to
    /// decide whether an extra supply tick is needed).
    pub fn has_pending_request(&self) -> bool {
        self.pending.is_some()
    }

    /// Plan one polling step.
    ///
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
        if irq.is_low() {
            let mut events = [0u8; 11];
            if tps.read_interrupts(i2c, &mut events).await {
                let mut clear = [0u8; 11];
                if Tps26750::is_interrupt_set(&events, TPS_INT_PLUG_INSERT_REMOVAL) {
                    app.pd = PdState::Negotiating;
                    self.negotiate_pending = true;
                    // Insert or removal both set this bit; drop the cached caps
                    // so a stale list from the previous cable can't be used.
                    app.pd_cap_count = 0;
                    Tps26750::set_interrupt_bit(&mut clear, TPS_INT_PLUG_INSERT_REMOVAL);
                }
                if Tps26750::is_interrupt_set(&events, TPS_INT_NEW_CONTRACT_AS_SINK) {
                    app.pd = PdState::ContractActive;
                    Tps26750::set_interrupt_bit(&mut clear, TPS_INT_NEW_CONTRACT_AS_SINK);
                }
                if clear != [0u8; 11] {
                    let _ = tps.clear_interrupts(i2c, &clear).await;
                }
            }
        }

        // Refresh the source-capability list whenever a negotiation is armed,
        // retrying while the source has not advertised its PDOs yet.
        if self.negotiate_pending && now >= self.next_caps_retry {
            app.pd_cap_count = tps.get_source_capabilities(i2c, &mut app.pd_caps).await;
            if app.pd_cap_count == 0 {
                self.next_caps_retry = now + Duration::from_millis(CAPS_RETRY_MS);
            }
        }

        // 3. Choose the rail.
        let cap_count = app.pd_cap_count.min(app.pd_caps.len() as u8) as usize;
        let choice = self.choose(app, cap_count);

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
            && choice
                .map(|ch| ch.rail_mv != self.requested_rail_mv)
                .unwrap_or(false)
            && now.duration_since(self.last_vset_change)
                >= Duration::from_millis(board::AUTO_TRACK_SETTLE_MS)
            && now.duration_since(self.last_renegotiate)
                >= Duration::from_millis(board::AUTO_TRACK_MIN_INTERVAL_MS);

        // Only queue a new request once the previous one has been issued.
        let do_negotiate = self.pending.is_none()
            && (self.negotiate_pending || app.pd_control.renegotiate_request || auto_due)
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
            if self.negotiate_pending && cap_count > 0 {
                // Caps are in, but nothing choosable: stop re-reading every poll.
                self.negotiate_pending = false;
            }
            // A confirm that cannot be acted on now (no cable / no eligible rail)
            // is dropped rather than left armed to fire unexpectedly later.
            app.pd_control.renegotiate_request = false;
        }

        // 4. Mirror the active contract.
        if let Some((v_mv, i_ma)) = tps.get_active_contract(i2c).await {
            app.supply.input_current_cap_ma = i_ma;
            app.supply.input_power_cap_mw = v_mv.saturating_mul(i_ma) / 1000;
            app.telemetry.vin_mv = v_mv;
            if app.pd == PdState::Negotiating {
                app.pd = PdState::ContractActive;
            }
            // Never undo the park for a request that has not been issued yet:
            // `requested_rail_mv` still refers to the previous contract.
            if self.pending.is_none() {
                self.maybe_reenable(app, v_mv, now);
            }
        } else if app.pd == PdState::ContractActive {
            // cable removed
            app.pd = PdState::NoCable;
            app.pd_cap_count = 0;
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

        defmt::info!(
            "PD request: rail={} mV, i={} mA, region={}",
            ch.rail_mv,
            ch.current_ma,
            region_label(ch.region)
        );

        let ok = tps
            .request_fixed_profile(i2c, ch.rail_mv, ch.current_ma)
            .await;
        if ok {
            let _ = tps.trigger_renegotiation(i2c).await;
            self.requested_rail_mv = ch.rail_mv;
        } else {
            // Leave `requested_rail_mv` on the previous contract: `maybe_reenable`
            // then sees the still-active rail as "close enough" and restores the
            // output, and auto-tracking retries after the minimum interval.
            defmt::warn!("PD request failed for rail={} mV", ch.rail_mv);
        }
        self.last_renegotiate = Instant::now();
    }

    /// Manual preset → nearest fixed PDO; Auto → the auto-track chooser.
    fn choose(&self, app: &AppState, cap_count: usize) -> Option<AutoChoice> {
        if cap_count == 0 {
            return None;
        }
        let caps = &app.pd_caps[..cap_count];
        match app.pd_control.mode {
            PdMode::Auto => auto_track::choose_rail(
                app.supply.v_set_mv,
                app.supply.i_set_ma,
                caps,
                app.pd_control.policy,
            ),
            PdMode::Manual => {
                let idx = (app.ui.pd_profile_index as usize).min(PD_PRESET_VOLTAGES_MV.len() - 1);
                auto_track::choose_nearest(PD_PRESET_VOLTAGES_MV[idx], caps)
            }
        }
    }

    /// Re-enable the output once the requested rail is actually active.
    fn maybe_reenable(&mut self, app: &mut AppState, active_mv: u32, now: Instant) {
        if !self.reenable_output {
            return;
        }
        let target = self.requested_rail_mv;
        let close_enough = target > 0 && active_mv.abs_diff(target) <= (target / 10).max(100);
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
