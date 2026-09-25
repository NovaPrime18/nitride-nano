//! Output-voltage sweep behind the CFG menu's "Output V sweep" option.
//!
//! Steps `supply.v_set_mv` through [`board::SWEEP_POINTS`] evenly spaced points
//! from [`board::SWEEP_START_MV`] to [`board::SWEEP_END_MV`], holding each for
//! [`board::SWEEP_STEP_MS`]. The controller owns only the step timing; the
//! phase/index the OLED renders live in [`crate::state::SweepState`] and are
//! driven from here.
//!
//! [`SweepController::tick`] runs once per supply tick inside the main loop's
//! `APP_STATE` critical section, immediately before `SupplyController::tick`, so
//! each new setpoint is pushed to the CV DAC in the same pass.

use embassy_time::{Duration, Instant};

use crate::board;
use crate::state::{AppState, Fault, SupplyMode, SweepPhase};

/// Drives one output-voltage sweep at a time.
pub struct SweepController {
    /// Instant the current point's dwell ends. Only meaningful while
    /// [`SweepPhase::Running`].
    next_step: Instant,
}

impl SweepController {
    pub fn new() -> Self {
        Self {
            next_step: Instant::now(),
        }
    }

    /// Setpoint for an inclusive 0-based point index: index 0 is
    /// [`board::SWEEP_START_MV`] and index `SWEEP_POINTS - 1` is exactly
    /// [`board::SWEEP_END_MV`]. Rounds to the nearest millivolt so the interior
    /// points stay symmetric rather than all biased low.
    pub fn setpoint_mv(index: u8) -> u32 {
        let last = board::SWEEP_POINTS.saturating_sub(1) as u32;
        if last == 0 {
            return board::SWEEP_START_MV;
        }
        let span = board::SWEEP_END_MV - board::SWEEP_START_MV;
        board::SWEEP_START_MV + (index as u32 * span + last / 2) / last
    }

    /// Advance the sweep by at most one point.
    ///
    /// Call once per supply tick. Consumes the UI's `start_request`, refuses to
    /// start on a latched fault, aborts if the output is disabled underneath it
    /// (fault or PD rail-change park), and parks the output on completion.
    pub fn tick(&mut self, app: &mut AppState) {
        if app.sweep.start_request {
            app.sweep.start_request = false;
            if app.supply.fault != Fault::None {
                defmt::warn!("Sweep start refused: fault latched");
                app.sweep.phase = SweepPhase::Off;
            } else {
                defmt::info!(
                    "Sweep start: {} points, {}..{} mV, {} ms/point",
                    board::SWEEP_POINTS,
                    board::SWEEP_START_MV,
                    board::SWEEP_END_MV,
                    board::SWEEP_STEP_MS
                );
                app.sweep.phase = SweepPhase::Running;
                app.sweep.index = 0;
                app.supply.mode = SupplyMode::Cv;
                app.supply.enabled = true;
                app.supply.v_set_mv = Self::setpoint_mv(0);
                self.next_step = Instant::now() + Duration::from_millis(board::SWEEP_STEP_MS);
            }
        }

        if app.sweep.phase != SweepPhase::Running {
            return;
        }

        // Anything that parks the output under us (a fault, or a PD rail-change
        // park) ends the sweep rather than silently commanding into a dead stage.
        if app.supply.fault != Fault::None || !app.supply.enabled {
            defmt::warn!("Sweep aborted: output no longer enabled");
            app.sweep.phase = SweepPhase::Off;
            app.supply.enabled = false;
            return;
        }

        if Instant::now() < self.next_step {
            return;
        }

        if app.sweep.index + 1 >= board::SWEEP_POINTS {
            defmt::info!("Sweep complete");
            app.sweep.phase = SweepPhase::Done;
            app.supply.enabled = false;
            return;
        }

        app.sweep.index += 1;
        app.supply.v_set_mv = Self::setpoint_mv(app.sweep.index);
        // Rearm from "now" rather than accumulating: a slow I2C poll between
        // ticks must not make the sweep skip points to catch up.
        self.next_step = Instant::now() + Duration::from_millis(board::SWEEP_STEP_MS);
        defmt::info!(
            "Sweep point {}/{}: {} mV",
            app.sweep.index + 1,
            board::SWEEP_POINTS,
            app.supply.v_set_mv
        );
    }
}

impl Default for SweepController {
    fn default() -> Self {
        Self::new()
    }
}
