//! rev3 output-stage power-up / power-down sequencer.
//!
//! rev3 split the old single `Conv-Disable` line into a three-pin sequence, and
//! the order is safety-critical because the pins do different jobs:
//!
//! ```text
//!   PB1 Conv-Disable   Q13 → LT8390A EN/UVLO     enables the controller
//!   PB3 CNV-EN-LOADEN  LT8390A LOADEN            grounds the output, then starts it
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
//! * **Precharge** — chip enabled, `LOADEN` low, so BG1/BG2 hold the output at
//!   0 V while INTVCC/VREF/CTRL settle. Nothing is connected yet.
//! * **Ramping** — `LOADEN` raised; the part soft-starts and the output rises.
//! * **SwitchOn** — output is up, so `PWR_G` closes `Q5` and the load is
//!   connected; the output INA228 then has to confirm it.
//! * **Disabling** — `PWR_G` opens first (load off), then `LOADEN` drops (bottom
//!   gates discharge the output), then and only then is the chip shut down.
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
    /// Chip enabled, `LOADEN` low: the bottom gates hold the output at 0 V.
    Precharge,
    /// `LOADEN` high: soft-start, output ramping toward the setpoint.
    Ramping,
    /// Output up, `PWR_G` asserted: the load is connected, awaiting verification.
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
    /// Load current reported by the output INA228.
    pub out_ina_current_ma: i32,
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
}

impl Sequencer {
    pub fn new() -> Self {
        Self {
            stage: Stage::Off,
            since: Instant::now(),
            hold_loaden: false,
        }
    }

    fn enter(&mut self, stage: Stage, now: Instant) {
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

        // The transition tests are written as "what the *previous* stage wanted
        // to do is now done", and each arm falls through into the output block
        // below so the pin states always match the stage that was just entered.
        match self.stage {
            Stage::Off => {
                if inp.want_on {
                    self.enter(Stage::Precharge, now);
                }
            }
            Stage::Precharge => {
                if !inp.want_on {
                    self.enter_disabling(now);
                } else if self.elapsed_ms(now) >= board::OUTPUT_PRELOADEN_MS {
                    self.enter(Stage::Ramping, now);
                }
            }
            Stage::Ramping => {
                if !inp.want_on {
                    self.enter_disabling(now);
                } else if Self::output_reached(&inp) {
                    self.enter(Stage::SwitchOn, now);
                } else if self.elapsed_ms(now) >= board::OUTPUT_RAMP_TIMEOUT_MS {
                    defmt::warn!(
                        "output: ramp timeout after {} ms: vout={} mV never reached {}% of vset={} mV (ina_out_ok={})",
                        self.elapsed_ms(now),
                        inp.vout_adc_mv,
                        board::VOUT_REACHED_PCT,
                        inp.v_set_mv,
                        inp.out_ina_ok
                    );
                    fault = Some(Fault::OutputNotReached);
                    self.enter_disabling(now);
                }
            }
            Stage::SwitchOn => {
                if !inp.want_on {
                    self.enter_disabling(now);
                } else if self.elapsed_ms(now) >= board::OUTPUT_SWITCH_SETTLE_MS {
                    match Self::verify_switch(&inp) {
                        Ok(()) => {
                            defmt::info!(
                                "output: switch closed and verified: vout={} mV vset={} mV iout={} mA",
                                inp.vout_adc_mv,
                                inp.v_set_mv,
                                inp.out_ina_current_ma
                            );
                            self.enter(Stage::Running, now);
                        }
                        Err(f) => {
                            fault = Some(f);
                            self.enter_disabling(now);
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
                if self.elapsed_ms(now) >= done {
                    self.enter(Stage::Off, now);
                }
            }
        }

        self.outputs(now, clamp, fault)
    }

    /// Has the output reached its setpoint?
    ///
    /// Compared in `mV * 100` so the percentage test stays integral. A zero
    /// setpoint counts as reached immediately, which is what lets a degenerate
    /// setpoint still complete the sequence instead of stalling in `Ramping`.
    fn output_reached(inp: &Inputs) -> bool {
        inp.vout_adc_mv as u64 * 100 >= inp.v_set_mv as u64 * board::VOUT_REACHED_PCT as u64
    }

    /// Post-`PWR_G` checks. `Ok` only when the load switch demonstrably closed.
    fn verify_switch(inp: &Inputs) -> Result<(), Fault> {
        let reached = Self::output_reached(inp);
        if !reached {
            defmt::error!(
                "output: SWITCH FAIL - output collapsed when the load was connected: vout={} mV < {}% of vset={} mV",
                inp.vout_adc_mv,
                board::VOUT_REACHED_PCT,
                inp.v_set_mv
            );
            return Err(Fault::OutputSwitchFailed);
        }
        if board::OUTPUT_SWITCH_REQUIRE_INA && !inp.out_ina_ok {
            defmt::error!(
                "output: SWITCH FAIL - output INA228 (U12, addr 0x{:02x}) is not answering, so the load switch cannot be confirmed; set board::OUTPUT_SWITCH_REQUIRE_INA = false on a board with U12 unpopulated",
                board::INA228_OUT_ADDR
            );
            return Err(Fault::OutputSwitchFailed);
        }
        if board::OUTPUT_SWITCH_REQUIRE_LOAD
            && inp.out_ina_current_ma.unsigned_abs() < board::OUTPUT_SWITCH_MIN_MA
        {
            defmt::error!(
                "output: SWITCH FAIL - output INA228 reads {} mA, below the {} mA threshold: the load switch is not carrying current. With no load attached this is expected - set board::OUTPUT_SWITCH_REQUIRE_LOAD = false for a no-load bench run.",
                inp.out_ina_current_ma,
                board::OUTPUT_SWITCH_MIN_MA
            );
            return Err(Fault::OutputSwitchFailed);
        }
        Ok(())
    }

    /// Pin states for the current stage. `Disabling` walks its own two delays so
    /// the load is disconnected before the output is grounded, and the output is
    /// grounded before the chip is shut down.
    fn outputs(&self, now: Instant, clamp: bool, fault: Option<Fault>) -> Outputs {
        let (conv_disable, load_enable, pwr_g) = match self.stage {
            Stage::Off => (true, false, false),
            Stage::Precharge => (false, false, false),
            Stage::Ramping => (false, true, false),
            Stage::SwitchOn => (false, true, true),
            Stage::Running => (false, true, true),
            Stage::Disabling => {
                let t = now.duration_since(self.since).as_millis();
                // Only hold LOADEN up if the sequence actually raised it; see
                // `hold_loaden`.
                let load_enable = self.hold_loaden && t < board::OUTPUT_DISCHARGE_MS;
                let conv_disable = t >= board::OUTPUT_DISCHARGE_MS + board::OUTPUT_CONV_OFF_MS;
                (conv_disable, load_enable, false)
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
