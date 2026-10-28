//! rev3 output-stage power-up / power-down sequencer.
//!
//! rev3 split the old single `Conv-Disable` line into a three-pin sequence, and
//! the order is safety-critical because the pins do different jobs:
//!
//! ```text
//!   PB1 Conv-Disable   Q13 → LT8390A EN/UVLO     enables the controller
//!   PB3 CNV-EN-LOADEN  LT8390A LOADEN            isolates the output, then starts it
//!   PB2 PWR_G          Q12 → Q7 → Q5 gate        connects the load
//! ```
//!
//! Enable walks them in that order, disable walks them back:
//!
//! ```text
//!   Off ──want_on──▶ Precharge ──settle──▶ Ramping ──Vout ok──▶ SwitchOn ──verify──▶ Running
//!    ▲                  │                    │                    │                    │
//!    └──────────────────┴────────────────────┴────── Disabling ◀──┴────!want_on─────────┘
//! ```
//!
//! * **Precharge** — chip enabled, `LOADEN` low: `TG1`/`TG2` are off, so the
//!   bridge drives nothing and INTVCC/VREF/CTRL settle with the load side
//!   isolated. While here the output node must also bleed down to at most
//!   [`crate::board::VOUT_START_MAX_PCT`] of the setpoint (see the stale-charge
//!   note below). With [`crate::board::OUTPUT_SWITCH_EARLY_CLOSE`] `PWR_G` is
//!   closed here too, at ~0 V, so the load is connected *before* the ramp.
//! * **Ramping** — `LOADEN` raised; the part soft-starts and the output rises.
//!   With the load already connected, a load that draws the CC limit settles
//!   into CC instead of reaching the setpoint, which
//!   [`crate::board::RAMP_CC_SETTLE_MS`] accepts as a valid ramp.
//! * **SwitchOn** — output is up; the output INA228 confirms the load is
//!   connected and drawing. `PWR_G` was already closed, so this stage verifies
//!   rather than closes.
//! * **Disabling** — `LOADEN` drops, then the chip is shut down; `PWR_G` stays
//!   closed until the output has fallen below
//!   [`crate::board::OUTPUT_SWITCH_SAFE_VOUT_MV`].
//!
//! # Why `PWR_G` follows the output (rev3 safety net)
//!
//! `Q5` sits in the output **return**, so opening it does not disconnect the
//! load from `VOUT` — it only interrupts the return current. With a load
//! attached the whole return node (`J3.1`, `R96`, the `SENSE_P`/`SENSE_N` taps)
//! is then pulled up to `VOUT` through that load. On rev3 the only low-voltage
//! part on those taps is the U7 TMUX1574 range mux, whose signal pins are rated
//! **6 V absolute maximum**: its ESD clamp turns the floating return into a
//! current source limited only by the 27 Ω taps, and two muxes were destroyed
//! that way. The firmware cannot see the return node and cannot tell whether a
//! load is attached, so it enforces the only safe rule it can: `PWR_G` is closed
//! whenever the output is above [`crate::board::OUTPUT_SWITCH_SAFE_VOUT_MV`] —
//! closed before the ramp, released only after the output has fallen.
//!
//! That rule cannot cover a **USB-C unplug with a load attached**: the output
//! capacitors then discharge through the load into the floating return with the
//! MCU already unpowered. Disconnect the load before removing input power, or
//! fit the rev4 sense front end (see `analysis/REV3_SCHEMATIC_AUDIT.md`).
//!
//! # Why `LOADEN` low does *not* discharge the output
//!
//! The LT8390A datasheet is precise about what forcing `LOADEN` low does:
//! *"turns off TG1 and TG2, turns on BG1 and BG2, disconnects the VC pin from
//! all internal loads, and turns off LOADTG."* `TG2` is the switch between
//! `SW2` and `VOUT`, so with it off the **output node is isolated from the
//! bridge** — `BG1`/`BG2` clamp the *inductor* side (`SW1`/`SW2`) to ground and
//! catch the inductor's stored energy, but the output capacitors only bleed
//! through their dividers (~0.3–0.7 mA total: `R19`/`R20`, `R85`–`R87`, the
//! `D20` indicator LED), i.e. over **tens of seconds**, not milliseconds.
//!
//! That has two consequences the sequencer takes into account:
//!
//! * after a high-voltage session the output node holds charge for a while —
//!   [`Stage::Precharge`] therefore refuses to raise `LOADEN` (and especially
//!   refuses to close `PWR_G`) while the node sits above ~105 % of the new
//!   setpoint, otherwise the ramp check would pass on the *stale* voltage and
//!   `Q5` would connect the load straight into an overvoltage;
//! * [`Stage::Disabling`] timings are a *settle* window, not a discharge: the
//!   output falls to ground only as fast as the bleeders drain it.
//!
//! This module is the decision logic only: it is handed a snapshot of the inputs
//! and returns the pin states, a DAC directive and any fault to latch.
//! [`crate::control::supply::SupplyController`] owns an instance and applies the
//! outputs, which keeps the pin ordering in one place and `supply.rs` readable.

use embassy_time::Instant;

use crate::board;
use crate::state::Fault;

/// Where the output stage is in its power-up/power-down sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Chip disabled, load disconnected, output held down by the parked DAC.
    Off,
    /// Chip enabled, `LOADEN` low: the bridge is parked (TG gates off) while
    /// the output node bleeds to a safe starting level. With
    /// [`crate::board::OUTPUT_SWITCH_EARLY_CLOSE`] `PWR_G` also closes here, at
    /// ~0 V, so the load is connected before the ramp.
    /// See the module docs for why `LOADEN` low isolates — rather than shorts —
    /// the output node.
    Precharge,
    /// `LOADEN` high: soft-start, output ramping toward the setpoint.
    Ramping,
    /// Output up and the load switch closed: awaiting confirmation that the load
    /// is actually connected (retried until
    /// [`crate::board::OUTPUT_SWITCH_VERIFY_MS`]). With
    /// [`crate::board::OUTPUT_SWITCH_EARLY_CLOSE`] `PWR_G` was already closed in
    /// `Precharge`, so this stage verifies rather than closes it.
    SwitchOn,
    /// Verified: converter regulating, load connected.
    Running,
    /// Unwinding in the safe order on the way back to [`Stage::Off`].
    Disabling,
}

impl Stage {
    /// Short label for the `supply:` diagnostics and the OLED header badge.
    ///
    /// **At most three characters.** The header badge is drawn at column 110 and
    /// `Ssd1306::draw_str` stops drawing once `x > 122`, so a fourth character is
    /// silently dropped.
    pub fn label(self) -> &'static str {
        match self {
            Stage::Off => "OFF",
            Stage::Precharge => "PRE",
            Stage::Ramping => "RMP",
            Stage::SwitchOn => "SWO",
            Stage::Running => "RUN",
            Stage::Disabling => "DWN",
        }
    }

    /// True while the converter may be regulating, i.e. the user setpoint (rather
    /// than the cold-start floor) governs the CC DAC.
    ///
    /// Not called: [`Outputs::cc_startup_clamp`] carries the same bit to the
    /// caller, which is the interface `control::supply` actually uses. Kept as
    /// the readable statement of what the stages mean.
    #[allow(dead_code)]
    pub fn is_regulating(self) -> bool {
        matches!(self, Stage::Running)
    }
}

/// Everything the sequencer needs to decide this tick.
#[derive(Clone, Copy, Debug)]
pub struct Inputs {
    /// The caller wants the output on and no fault is latched.
    pub want_on: bool,
    /// Current output-voltage setpoint.
    pub v_set_mv: u32,
    /// **Fast** output voltage from the ADC divider on `Vsense_REG_OUT` (PA0).
    /// The 1 ms supply tick cannot wait for the INA228's 50 ms conversion, and
    /// this is the signal the ramp decision is made on.
    pub vout_adc_mv: u32,
    /// True while the output INA228 (U12) is answering.
    pub out_ina_ok: bool,
    /// Has the CV DAC finished walking to this tick's target code?
    ///
    /// [`Stage::Ramping`] holds the ramp timeout until this is true, so the code
    /// slew (up to ~1 s out of the parked code) is not charged against the
    /// converter's ramp budget.
    pub cv_slew_settled: bool,
    /// Is U12 *expected* to be there in this input mode?
    ///
    /// The output monitor lives on the PD bus, which is powered from the
    /// TPS26750's own LDO and therefore only exists while a USB-C source is
    /// attached. On an XT90/DC feed the monitor is structurally absent, so the
    /// INA-based half of [`Self::verify_switch`]'s evidence does not apply and
    /// the switch is confirmed by the output voltage alone (logged). On a
    /// *live* PD bus a missing U12 is a real fault and the load switch is not
    /// closed.
    pub out_ina_expected: bool,
    /// Load current reported by the output INA228.
    pub out_ina_current_ma: i32,
    /// Best available output-current magnitude, in mA: `max(ISMON, |U12|)`.
    ///
    /// Used only to decide whether releasing the load switch while the output is
    /// still up is safe — see [`crate::board::OUTPUT_SWITCH_SAFE_OPEN_MA`].
    pub load_current_ma: u32,
    /// The output current is at the commanded limit this tick.
    ///
    /// With the load switch closed before the ramp
    /// ([`crate::board::OUTPUT_SWITCH_EARLY_CLOSE`]) a load that draws the CC
    /// limit holds the output below the setpoint *on purpose*; that is CC
    /// operation, not a failed ramp. The sequencer accepts it once it has
    /// persisted for [`crate::board::RAMP_CC_SETTLE_MS`].
    pub cc_limited: bool,
}

/// What the caller must drive this tick.
#[derive(Clone, Copy, Debug)]
pub struct Outputs {
    pub stage: Stage,
    /// Drive `Conv-Disable` active (chip off).
    pub conv_disable: bool,
    /// `CNV-EN-LOADEN` level.
    pub load_enable: bool,
    /// `PWR_G` level.
    pub pwr_g: bool,
    /// Command the CC DAC at or above the cold-start floor instead of at the
    /// user setpoint. True for every stage except [`Stage::Running`].
    pub cc_startup_clamp: bool,
    /// A fault to latch, if this tick produced one.
    pub fault: Option<Fault>,
}

/// Why one `Stage::SwitchOn` verification attempt did not pass.
///
/// The check is retried for up to [`crate::board::OUTPUT_SWITCH_VERIFY_MS`], so
/// this is *evidence so far*, not a verdict — only the attempt that runs out the
/// window latches `Fault::OutputSwitchFailed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwitchCheck {
    /// `Vsense_REG_OUT` is below [`crate::board::VOUT_REACHED_PCT`] of the
    /// setpoint: the output is sagging (inrush, load step) or has collapsed.
    Collapsed,
    /// U12 is not answering on a live PD bus, so current cannot be confirmed.
    InaMissing,
    /// U12 is answering but reports less than
    /// [`crate::board::OUTPUT_SWITCH_MIN_MA`] — either `Q5` is not conducting,
    /// or the load simply draws less than the threshold.
    NoCurrent,
}

impl SwitchCheck {
    /// Short cause for the retry line (`output: switch not confirmed yet - ...`).
    fn describe(self) -> &'static str {
        match self {
            SwitchCheck::Collapsed => "output sagged below the reached threshold",
            SwitchCheck::InaMissing => "output INA228 (U12) is not answering",
            SwitchCheck::NoCurrent => "output INA228 reads no load current",
        }
    }
}

/// The sequencer state machine.
pub struct Sequencer {
    stage: Stage,
    /// Timestamp the current stage was entered.
    since: Instant,
    /// While unwinding, whether `CNV-EN-LOADEN` should be *held* high through the
    /// discharge window. Only true when the sequence had actually raised it —
    /// otherwise an abort from [`Stage::Precharge`] would pulse `LOADEN` high and
    /// start the conversion it is trying to avoid.
    hold_loaden: bool,
    /// True while [`Stage::Precharge`] is holding `LOADEN` down because the
    /// output node sits above [`crate::board::VOUT_START_MAX_PCT`] of the
    /// setpoint (stale charge from a previous session, or an external source
    /// on the output). Edge state for logging.
    waiting_bleed: bool,
    /// When the ramp budget started: set once the CV code has finished slewing
    /// inside [`Stage::Ramping`] (see the arm there — the code walk must not be
    /// charged against the converter's ramp time).
    ramp_started: Option<Instant>,
    /// Last [`SwitchCheck`] this `SwitchOn` window logged, so a check that keeps
    /// failing the same way logs once per [`crate::board::OUTPUT_SWITCH_RETRY_LOG_MS`]
    /// rather than on every 1 ms tick (a *change* of cause always logs).
    switch_check: Option<SwitchCheck>,
    /// When `SwitchCheck` last produced a line; `None` before the first failure.
    switch_log_at: Option<Instant>,
    /// Verification attempts made in this `SwitchOn` window.
    switch_attempts: u32,
    /// Highest `|i_out|` seen across those attempts: distinguishes "the switch
    /// never conducted" from "the load draws less than the threshold".
    switch_max_ma: u32,
    /// Lowest `vout_adc_mv` seen across those attempts: with the value at the
    /// timeout it separates a one-off inrush sag (output back up by the end)
    /// from a genuine collapse.
    switch_min_vout_mv: u32,
    /// When the output current first sat at the commanded limit, for the
    /// CC-limited ramp acceptance (see [`crate::board::RAMP_CC_SETTLE_MS`]).
    cc_limited_since: Option<Instant>,
    /// Edge state for the "holding PWR_G closed during the disable" log.
    disable_holding: bool,
}

impl Sequencer {
    pub fn new() -> Self {
        Self {
            stage: Stage::Off,
            since: Instant::now(),
            hold_loaden: false,
            waiting_bleed: false,
            ramp_started: None,
            switch_check: None,
            switch_log_at: None,
            switch_attempts: 0,
            switch_max_ma: 0,
            switch_min_vout_mv: u32::MAX,
            cc_limited_since: None,
            disable_holding: false,
        }
    }

    fn enter(&mut self, stage: Stage, now: Instant) {
        if stage == Stage::Ramping {
            // Re-arm the ramp budget: it starts when the CV code settles, not now.
            self.ramp_started = None;
        }
        if matches!(stage, Stage::Precharge | Stage::Ramping) {
            // A new enable starts with a clean CC-limited clock: the previous
            // session (or the load during `Disabling`) may have left it running,
            // and a stale one would declare the new ramp complete at 0 V.
            self.cc_limited_since = None;
        }
        if stage == Stage::SwitchOn {
            // Fresh retry bookkeeping for this close (see
            // `board::OUTPUT_SWITCH_VERIFY_MS`).
            self.switch_check = None;
            self.switch_log_at = None;
            self.switch_attempts = 0;
            self.switch_max_ma = 0;
            self.switch_min_vout_mv = u32::MAX;
        }
        self.stage = stage;
        self.since = now;
    }

    /// Enter [`Stage::Disabling`], remembering whether `LOADEN` was raised so the
    /// unwind does not pulse it back on.
    fn enter_disabling(&mut self, now: Instant) {
        self.hold_loaden = matches!(
            self.stage,
            Stage::Ramping | Stage::SwitchOn | Stage::Running
        );
        self.disable_holding = false;
        self.enter(Stage::Disabling, now);
    }

    fn elapsed_ms(&self, now: Instant) -> u64 {
        now.duration_since(self.since).as_millis()
    }

    /// One control step. Pure with respect to the hardware: the caller applies
    /// [`Outputs`].
    pub fn step(&mut self, now: Instant, inp: Inputs) -> Outputs {
        let mut fault = None;
        let mut clamp = true;

        // Track how long the output has been pinned at the commanded current
        // limit. A CC-limited output is an accepted operating point, but the
        // inrush into a load's input capacitance also touches the limit for a few
        // milliseconds, so the acceptance only starts once the condition persists
        // (`board::RAMP_CC_SETTLE_MS`).
        if inp.cc_limited {
            if self.cc_limited_since.is_none() {
                self.cc_limited_since = Some(now);
            }
        } else {
            self.cc_limited_since = None;
        }

        // The transition tests are written as "what the *previous* stage wanted
        // to do is now done", and each arm falls through into the output block
        // below so the pin states always match the stage that was just entered.
        match self.stage {
            Stage::Off => {
                if inp.want_on {
                    self.waiting_bleed = false;
                    self.enter(Stage::Precharge, now);
                }
            }
            Stage::Precharge => {
                if !inp.want_on {
                    self.enter_disabling(now);
                } else if Self::output_too_high(&inp) {
                    // The output node sits above ~105 % of the setpoint: either
                    // stale charge from a previous session (the node bleeds
                    // through its dividers over seconds, not ms — see the
                    // module docs) or an external source holding it up. Neither
                    // may be connected to the load, and the converter cannot
                    // sink, so hold `LOADEN` low until it bleeds below the
                    // threshold.
                    if !self.waiting_bleed {
                        self.waiting_bleed = true;
                        defmt::warn!(
                            "output: holding LOADEN low - output node at {} mV is above {}% of vset={} mV (stale charge bleeding, or an external source on the output); PWR_G stays open until it falls. If a load is attached, its return now floats at the output voltage and the U7 S1A/S2A pins (6 V rating) are exposed - disconnect the load or remove the external source",
                            inp.vout_adc_mv,
                            board::VOUT_START_MAX_PCT,
                            inp.v_set_mv
                        );
                    }
                    if self.elapsed_ms(now) >= board::OUTPUT_BLEED_TIMEOUT_MS {
                        defmt::warn!(
                            "output: output node never bled down: {} mV still above {}% of vset={} mV after {} ms - an external source may be holding the output up",
                            inp.vout_adc_mv,
                            board::VOUT_START_MAX_PCT,
                            inp.v_set_mv,
                            board::OUTPUT_BLEED_TIMEOUT_MS
                        );
                        fault = Some(Fault::OutputAboveSetpoint);
                        self.enter_disabling(now);
                    }
                } else if self.elapsed_ms(now) >= board::OUTPUT_PRELOADEN_MS {
                    if self.waiting_bleed {
                        self.waiting_bleed = false;
                        defmt::info!(
                            "output: node bled down to {} mV (vset={} mV) - starting the ramp",
                            inp.vout_adc_mv,
                            inp.v_set_mv
                        );
                    }
                    self.enter(Stage::Ramping, now);
                }
            }
            Stage::Ramping => {
                if !inp.want_on {
                    self.enter_disabling(now);
                } else if Self::voltage_reached(&inp) || self.cc_sustained(&inp, now) {
                    self.enter(Stage::SwitchOn, now);
                } else {
                    // Start the ramp budget once the CV code has finished walking:
                    // before that the converter has not been asked for the new
                    // voltage at all (the slew alone is ~0.8 s to a 48 V setpoint).
                    if self.ramp_started.is_none() && inp.cv_slew_settled {
                        self.ramp_started = Some(now);
                        defmt::info!(
                            "output: CV code settled after {} ms - ramp budget starts now ({} ms, vset={} mV, vout={} mV)",
                            self.elapsed_ms(now),
                            board::OUTPUT_RAMP_TIMEOUT_MS,
                            inp.v_set_mv,
                            inp.vout_adc_mv
                        );
                    }
                    if self.ramp_started.is_none() {
                        // The code walk is bounded, so one that never finishes is a
                        // stuck DAC/map rather than something to wait out.
                        if self.elapsed_ms(now) >= board::OUTPUT_SLEW_GUARD_MS {
                            defmt::warn!(
                                "output: OUTPUT FAIL - the CV code never finished slewing within {} ms (vset={} mV): the slew limiter or the setpoint map is stuck",
                                board::OUTPUT_SLEW_GUARD_MS,
                                inp.v_set_mv
                            );
                            fault = Some(Fault::OutputNotReached);
                            self.enter_disabling(now);
                        }
                    } else if now.duration_since(self.ramp_started.unwrap_or(now)).as_millis()
                        >= board::OUTPUT_RAMP_TIMEOUT_MS
                    {
                        defmt::warn!(
                            "output: ramp timeout {} ms after the CV code settled: vout={} mV never reached {}% of vset={} mV (ina_out_ok={}, ctrl/loop may be railed)",
                            board::OUTPUT_RAMP_TIMEOUT_MS,
                            inp.vout_adc_mv,
                            board::VOUT_REACHED_PCT,
                            inp.v_set_mv,
                            inp.out_ina_ok
                        );
                        fault = Some(Fault::OutputNotReached);
                        self.enter_disabling(now);
                    }
                }
            }
            Stage::SwitchOn => {
                if !inp.want_on {
                    self.enter_disabling(now);
                } else if self.elapsed_ms(now) >= board::OUTPUT_SWITCH_SETTLE_MS {
                    // Re-check every tick instead of deciding on the one sample
                    // taken at the settle boundary: the first post-`PWR_G`
                    // U12 sample can be read late (100 ms poll, ~50 ms
                    // conversion, 2 s poll back-off), the load can sag the
                    // output on inrush, and a load with its own soft-start can
                    // take a moment to draw current. See
                    // `board::OUTPUT_SWITCH_VERIFY_MS`. A healthy close still
                    // passes on the first attempt.
                    match self.verify_switch(&inp, now) {
                        Ok(()) => {
                            if !inp.out_ina_expected {
                                // No output monitor in this input mode (XT90/DC):
                                // the PD bus that hosts U12 is structurally
                                // absent, so the switch is confirmed by the
                                // output voltage alone.
                                defmt::info!(
                                    "output: load switch confirmed by voltage only - no output INA228 in this input mode (XT90/DC), so current confirmation is unavailable"
                                );
                                defmt::info!(
                                    "output: switch closed and verified after {} ms (voltage only): vout={} mV vset={} mV",
                                    self.elapsed_ms(now),
                                    inp.vout_adc_mv,
                                    inp.v_set_mv
                                );
                            } else {
                                defmt::info!(
                                    "output: switch closed and verified after {} ms ({} check(s), peak iout {} mA, require_load={}): vout={} mV vset={} mV",
                                    self.elapsed_ms(now),
                                    self.switch_attempts + 1,
                                    self.switch_max_ma,
                                    board::OUTPUT_SWITCH_REQUIRE_LOAD,
                                    inp.vout_adc_mv,
                                    inp.v_set_mv
                                );
                            }
                            self.enter(Stage::Running, now);
                        }
                        Err(check) => {
                            let elapsed = self.elapsed_ms(now);
                            self.switch_attempts = self.switch_attempts.saturating_add(1);
                            self.switch_max_ma = self
                                .switch_max_ma
                                .max(inp.out_ina_current_ma.unsigned_abs());
                            self.switch_min_vout_mv = self.switch_min_vout_mv.min(inp.vout_adc_mv);
                            let log_due = match self.switch_log_at {
                                None => true,
                                Some(t) => {
                                    now.duration_since(t).as_millis()
                                        >= board::OUTPUT_SWITCH_RETRY_LOG_MS
                                }
                            };
                            if self.switch_check != Some(check) || log_due {
                                self.switch_check = Some(check);
                                self.switch_log_at = Some(now);
                                defmt::warn!(
                                    "output: switch not confirmed yet ({} check(s)) - {}: vout={} mV (vset={} mV), iout={} mA (peak {} mA), ina_ok={}; re-checking for up to {} ms more",
                                    self.switch_attempts,
                                    check.describe(),
                                    inp.vout_adc_mv,
                                    inp.v_set_mv,
                                    inp.out_ina_current_ma,
                                    self.switch_max_ma,
                                    inp.out_ina_ok,
                                    (board::OUTPUT_SWITCH_SETTLE_MS
                                        + board::OUTPUT_SWITCH_VERIFY_MS)
                                        .saturating_sub(elapsed)
                                );
                            }
                            let budget =
                                board::OUTPUT_SWITCH_SETTLE_MS + board::OUTPUT_SWITCH_VERIFY_MS;
                            if elapsed >= budget {
                                match check {
                                    SwitchCheck::Collapsed => defmt::error!(
                                        "output: SWITCH FAIL - the output never stayed up with the load connected: vout={} mV (lowest seen {} mV) below {}% of vset={} mV after {} checks over {} ms",
                                        inp.vout_adc_mv,
                                        self.switch_min_vout_mv,
                                        board::VOUT_REACHED_PCT,
                                        inp.v_set_mv,
                                        self.switch_attempts,
                                        elapsed
                                    ),
                                    SwitchCheck::InaMissing => defmt::error!(
                                        "output: SWITCH FAIL - output INA228 (U12, addr 0x{:02x}) is not answering on a live PD bus, so the load switch cannot be confirmed; set board::OUTPUT_SWITCH_REQUIRE_INA = false on a board with U12 unpopulated",
                                        board::INA228_OUT_ADDR
                                    ),
                                    SwitchCheck::NoCurrent => defmt::error!(
                                        "output: SWITCH FAIL - output INA228 reads {} mA (peak {} mA over {} checks), below the {} mA threshold: the load switch is not carrying current. board::OUTPUT_SWITCH_REQUIRE_LOAD is true, so the close needs load current; with nothing attached there is none to see, however well the switch closes - set it false (the default) for a no-load bench run.",
                                        inp.out_ina_current_ma,
                                        self.switch_max_ma,
                                        self.switch_attempts,
                                        board::OUTPUT_SWITCH_MIN_MA
                                    ),
                                }
                                fault = Some(Fault::OutputSwitchFailed);
                                self.enter_disabling(now);
                            }
                        }
                    }
                }
            }
            Stage::Running => {
                if !inp.want_on {
                    self.enter_disabling(now);
                } else {
                    clamp = false;
                }
            }
            Stage::Disabling => {
                let done = board::OUTPUT_DISCHARGE_MS + board::OUTPUT_CONV_OFF_MS;
                let holding = self.hold_switch(&inp, now);
                if holding && !self.disable_holding {
                    self.disable_holding = true;
                    defmt::info!(
                        "output: holding PWR_G closed during the disable - output still at {} mV with {} mA of load current; the return node must not float while the output is up (U7 S1A/S2A are rated 6 V)",
                        inp.vout_adc_mv,
                        inp.load_current_ma
                    );
                }
                if self.elapsed_ms(now) >= done && !holding {
                    if inp.vout_adc_mv > board::OUTPUT_SWITCH_SAFE_VOUT_MV {
                        defmt::warn!(
                            "output: releasing PWR_G with the output still at {} mV after {} ms (load current {} mA, safe-open threshold {} mA) - if a load is attached its return now floats at the output voltage and the U7 S1A/S2A pins are exposed; nothing discharged the output inside board::OUTPUT_SWITCH_HOLD_MAX_MS",
                            inp.vout_adc_mv,
                            self.elapsed_ms(now),
                            inp.load_current_ma,
                            board::OUTPUT_SWITCH_SAFE_OPEN_MA
                        );
                    }
                    self.enter(Stage::Off, now);
                }
            }
        }

        self.outputs(now, clamp, fault, &inp)
    }

    /// Has the output reached its setpoint?
    ///
    /// Compared in `mV * 100` so the percentage test stays integral. A zero
    /// setpoint counts as reached immediately, which is what lets a degenerate
    /// setpoint still complete the sequence instead of stalling in `Ramping`.
    fn voltage_reached(inp: &Inputs) -> bool {
        inp.vout_adc_mv as u64 * 100 >= inp.v_set_mv as u64 * board::VOUT_REACHED_PCT as u64
    }

    /// Has the output been pinned at the commanded current limit long enough to
    /// count as a legitimate CC operating point rather than a failed ramp?
    ///
    /// Requires the output to be above
    /// [`crate::board::OUTPUT_RAMP_CC_MIN_PCT`] of the setpoint, so a dead short
    /// (also current-limited, but at 0 V) still fails the ramp. Only meaningful
    /// with the load switch closed before the ramp — see
    /// [`crate::board::RAMP_CC_SETTLE_MS`].
    fn cc_sustained(&self, inp: &Inputs, now: Instant) -> bool {
        let above_floor = inp.vout_adc_mv as u64 * 100
            >= inp.v_set_mv as u64 * board::OUTPUT_RAMP_CC_MIN_PCT as u64;
        above_floor
            && match self.cc_limited_since {
                Some(t) => now.duration_since(t).as_millis() >= board::RAMP_CC_SETTLE_MS,
                None => false,
            }
    }

    /// Must `PWR_G` stay closed through this disable tick?
    ///
    /// The load switch may only be opened once opening it cannot feed the U7
    /// mux a damaging current: either the output has fallen below
    /// [`board::OUTPUT_SWITCH_SAFE_VOUT_MV`], or the load current is already
    /// below [`board::OUTPUT_SWITCH_SAFE_OPEN_MA`] (nothing is holding the return
    /// node up). [`board::OUTPUT_SWITCH_HOLD_MAX_MS`] bounds the wait so an
    /// unloaded output — which only bleeds through its dividers — does not hang
    /// the disable. See the `OUTPUT_SWITCH_*` constants for the why.
    fn hold_switch(&self, inp: &Inputs, now: Instant) -> bool {
        inp.vout_adc_mv > board::OUTPUT_SWITCH_SAFE_VOUT_MV
            && inp.load_current_ma >= board::OUTPUT_SWITCH_SAFE_OPEN_MA
            && self.elapsed_ms(now) < board::OUTPUT_SWITCH_HOLD_MAX_MS
    }

    /// Must [`Stage::Precharge`] keep holding `LOADEN` low because the output
    /// node sits above [`board::VOUT_START_MAX_PCT`] of the setpoint?
    ///
    /// This is what stops the sequencer from declaring the ramp "done" on stale
    /// charge: with the converter unable to sink, a node that starts above the
    /// setpoint can only fall through its bleeders, so the only safe move is to
    /// wait — the alternative (`reached` passing instantly on the old voltage
    /// and `PWR_G` closing) would connect the load into an overvoltage. Uses
    /// the same 105 % margin as the supervisor's overvoltage guard, so a
    /// normal re-enable at a similar setpoint is never delayed by it.
    fn output_too_high(inp: &Inputs) -> bool {
        inp.vout_adc_mv as u64 * 100 > inp.v_set_mv as u64 * board::VOUT_START_MAX_PCT as u64
    }

    /// Post-`PWR_G` checks. `Ok` only when the load switch demonstrably closed;
    /// `Err` names the evidence that is missing *on this attempt*, which the
    /// caller retries until [`crate::board::OUTPUT_SWITCH_VERIFY_MS`] expires.
    ///
    /// Deliberately silent: the retry loop logs the first failure, any change of
    /// cause, and the final verdict, so a 1 ms cadence cannot flood RTT.
    fn verify_switch(&self, inp: &Inputs, now: Instant) -> Result<(), SwitchCheck> {
        if !Self::voltage_reached(inp) && !self.cc_sustained(inp, now) {
            return Err(SwitchCheck::Collapsed);
        }
        if !inp.out_ina_expected {
            // No output monitor in this input mode (XT90/DC): the PD bus that
            // hosts U12 is structurally absent, so the switch is confirmed by the
            // output voltage alone. The load still never connects above the
            // setpoint — `voltage_reached` above and the pre-switch hold in
            // `Stage::Ramping` cover that.
            return Ok(());
        }
        if board::OUTPUT_SWITCH_REQUIRE_INA && !inp.out_ina_ok {
            return Err(SwitchCheck::InaMissing);
        }
        if board::OUTPUT_SWITCH_REQUIRE_LOAD
            && inp.out_ina_current_ma.unsigned_abs() < board::OUTPUT_SWITCH_MIN_MA
        {
            return Err(SwitchCheck::NoCurrent);
        }
        Ok(())
    }

    /// Pin states for the current stage.
    ///
    /// `PWR_G` follows the output rather than the load-switch script: it is
    /// closed from `Precharge` onward (see
    /// [`crate::board::OUTPUT_SWITCH_EARLY_CLOSE`]) and released only once the
    /// output has fallen below [`crate::board::OUTPUT_SWITCH_SAFE_VOUT_MV`], so
    /// the return node never floats at the output voltage.
    fn outputs(&self, now: Instant, clamp: bool, fault: Option<Fault>, inp: &Inputs) -> Outputs {
        // rev3 safety net: `PWR_G` is closed whenever the output is above
        // `board::OUTPUT_SWITCH_SAFE_VOUT_MV`, so the return node (and the U7
        // mux pins tied to it) never sees the output voltage. See the
        // `OUTPUT_SWITCH_*` constants for the failure this prevents.
        let early = board::OUTPUT_SWITCH_EARLY_CLOSE;
        let (conv_disable, load_enable, pwr_g) = match self.stage {
            Stage::Off => (true, false, false),
            // Close before the ramp: the load is connected at ~0 V, so the ramp
            // then runs with the return held at ground. Keep it open while the
            // node is above the start limit (stale charge or an external source).
            Stage::Precharge => (false, false, early && !Self::output_too_high(inp)),
            Stage::Ramping => (false, true, early),
            Stage::SwitchOn => (false, true, true),
            Stage::Running => (false, true, true),
            Stage::Disabling => {
                let t = now.duration_since(self.since).as_millis();
                // Only hold LOADEN up if the sequence actually raised it; see
                // `hold_loaden`.
                let load_enable = self.hold_loaden && t < board::OUTPUT_DISCHARGE_MS;
                let conv_disable = t >= board::OUTPUT_DISCHARGE_MS + board::OUTPUT_CONV_OFF_MS;
                (conv_disable, load_enable, self.hold_switch(inp, now))
            }
        };
        Outputs {
            stage: self.stage,
            conv_disable,
            load_enable,
            pwr_g,
            cc_startup_clamp: clamp,
            fault,
        }
    }
}

impl Default for Sequencer {
    fn default() -> Self {
        Self::new()
    }
}
