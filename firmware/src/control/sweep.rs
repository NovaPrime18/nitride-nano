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
//!
//! # Efficiency diagnostics
//!
//! Each point also reports a filtered input-to-output efficiency to the defmt
//! console: input and output power are integrated over the settled part of the
//! dwell (see [`board::SWEEP_SETTLE_MS`] / [`board::SWEEP_SAMPLE_MS`]) and
//! divided once, so the log carries a stable `eta` per point instead of the
//! board's noisy instantaneous ratio. A run ends with a best/worst summary, so
//! the maximum- and minimum-efficiency output voltages can be read directly off
//! the console. Points with too few accepted samples, or an over-unity ratio
//! (measurement error), are logged but excluded from that summary.

use embassy_time::{Duration, Instant};

use crate::board;
use crate::sense::efficiency::{EfficiencyAccumulator, ETA_UNITY_TENTHS};
use crate::state::{AppState, Fault, SupplyMode, SweepPhase};

/// Fixed diagnostic record for one completed sweep point.
#[derive(Clone, Copy, Debug)]
struct PointResult {
    /// 0-based point index.
    index: u8,
    v_set_mv: u32,
    vout_mv: u32,
    iout_ma: u32,
    vin_mv: u32,
    /// Mean input power over the measurement window.
    pin_mw: u32,
    /// Mean output power over the measurement window.
    pout_mw: u32,
    /// Accepted samples in the window.
    samples: u16,
    /// Windowed efficiency in tenths of a percent (`None` with no valid data).
    tenths_pct: Option<u32>,
}

/// Drives one output-voltage sweep at a time.
pub struct SweepController {
    /// Instant the current point's dwell ends. Only meaningful while
    /// [`SweepPhase::Running`].
    next_step: Instant,
    /// True from the moment a sweep starts until it ends for any reason. Lets
    /// `tick` notice an operator abort, which the UI performs by clearing
    /// `phase` directly without telling the controller.
    active: bool,
    /// Instant the current point's measurement window opens, i.e. the end of the
    /// settle skip.
    next_sample: Instant,
    /// Power integrator for the current point.
    acc: EfficiencyAccumulator,
    /// Best/worst valid point: (0-based index, efficiency in tenths of a percent).
    best: Option<(u8, u32)>,
    worst: Option<(u8, u32)>,
    /// Points that met the validity rules and entered the best/worst search.
    valid_points: u8,
}

impl SweepController {
    pub fn new() -> Self {
        Self {
            next_step: Instant::now(),
            active: false,
            next_sample: Instant::now(),
            acc: EfficiencyAccumulator::new(),
            best: None,
            worst: None,
            valid_points: 0,
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
        let now = Instant::now();

        // An operator abort clears `sweep.phase` from the UI without touching the
        // controller, so the only way to know the run ended is that the phase
        // left `Running` while we still believed we were sweeping. Summarise what
        // was measured instead of dropping it.
        if self.active && app.sweep.phase != SweepPhase::Running {
            self.active = false;
            defmt::warn!("Sweep aborted by operator");
            self.log_summary("aborted");
            self.reset_results();
            return;
        }

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
                self.next_step = now + Duration::from_millis(board::SWEEP_STEP_MS);
                self.active = true;
                self.reset_results();
                self.begin_point(now);
                defmt::info!(
                    "Sweep point 1/{}: {} mV",
                    board::SWEEP_POINTS,
                    app.supply.v_set_mv
                );
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
            self.active = false;
            self.log_summary("aborted");
            self.reset_results();
            self.acc.reset();
            return;
        }

        if now < self.next_step {
            self.sample(now, app);
            return;
        }

        // Dwell elapsed: close out the point we just finished, then either end
        // the sweep or step to the next setpoint.
        let result = self.finish_point(app, app.sweep.index);
        Self::log_point(&result);
        self.record(&result);

        if app.sweep.index + 1 >= board::SWEEP_POINTS {
            self.log_summary("complete");
            self.active = false;
            app.sweep.phase = SweepPhase::Done;
            app.supply.enabled = false;
            self.reset_results();
            return;
        }

        app.sweep.index += 1;
        app.supply.v_set_mv = Self::setpoint_mv(app.sweep.index);
        // Rearm from "now" rather than accumulating: a slow I2C poll between
        // ticks must not make the sweep skip points to catch up.
        self.next_step = now + Duration::from_millis(board::SWEEP_STEP_MS);
        self.begin_point(now);
        defmt::info!(
            "Sweep point {}/{}: {} mV",
            app.sweep.index + 1,
            board::SWEEP_POINTS,
            app.supply.v_set_mv
        );
    }

    /// Start the measurement window for a new point: clear the integrator and
    /// open sampling after [`board::SWEEP_SETTLE_MS`].
    fn begin_point(&mut self, now: Instant) {
        self.acc.reset();
        self.next_sample = now + Duration::from_millis(board::SWEEP_SETTLE_MS);
    }

    /// Integrate one telemetry sample if the window is open and the cadence has
    /// elapsed. The next sample is rearmed from `now`, so a stalled loop yields
    /// fewer samples rather than a burst of catch-up ones.
    fn sample(&mut self, now: Instant, app: &AppState) {
        if now < self.next_sample {
            return;
        }
        self.acc
            .push(app.telemetry.pin_mw, app.telemetry.pout_mw, app.telemetry.ina_ok);
        self.next_sample = now + Duration::from_millis(board::SWEEP_SAMPLE_MS);
    }

    /// Snapshot the just-finished point for logging and the best/worst search.
    fn finish_point(&self, app: &AppState, index: u8) -> PointResult {
        PointResult {
            index,
            v_set_mv: app.supply.v_set_mv,
            vout_mv: app.telemetry.vout_mv,
            iout_ma: app.telemetry.iout_ma,
            vin_mv: app.telemetry.vin_mv,
            pin_mw: self.acc.mean_pin_mw(),
            pout_mw: self.acc.mean_pout_mw(),
            samples: self.acc.samples(),
            tenths_pct: self.acc.tenths_pct(),
        }
    }

    /// A point only enters the best/worst search when it has enough samples and
    /// its ratio is physically plausible. An over-unity reading is measurement
    /// error, so recording it as the maximum would be actively misleading.
    fn valid(result: &PointResult) -> bool {
        result.samples >= board::SWEEP_MIN_SAMPLES
            && matches!(result.tenths_pct, Some(t) if t <= ETA_UNITY_TENTHS)
    }

    /// Fold a completed point into the running best/worst extremes.
    fn record(&mut self, result: &PointResult) {
        if !Self::valid(result) {
            return;
        }
        let Some(tenths) = result.tenths_pct else {
            return;
        };
        self.valid_points = self.valid_points.saturating_add(1);

        match self.best {
            Some((_, t)) if t >= tenths => {}
            _ => self.best = Some((result.index, tenths)),
        }
        match self.worst {
            Some((_, t)) if t <= tenths => {}
            _ => self.worst = Some((result.index, tenths)),
        }
    }

    /// One console line per point, greppable and self-describing. Points that do
    /// not qualify for the summary are tagged `excluded` so the operator can see
    /// why the best/worst count is smaller than the point count.
    fn log_point(result: &PointResult) {
        match result.tenths_pct {
            Some(t) => {
                let suffix = if Self::valid(result) { "" } else { " excluded" };
                defmt::info!(
                    "sweep point {}/{}: vset={} vout={} iout={} vin={} pin_avg={} pout_avg={} eta={}.{}% n={}{}",
                    result.index + 1,
                    board::SWEEP_POINTS,
                    result.v_set_mv,
                    result.vout_mv,
                    result.iout_ma,
                    result.vin_mv,
                    result.pin_mw,
                    result.pout_mw,
                    t / 10,
                    t % 10,
                    result.samples,
                    suffix
                )
            }
            None => defmt::info!(
                "sweep point {}/{}: vset={} vout={} iout={} vin={} pin_avg={} pout_avg={} eta=-- n={}",
                result.index + 1,
                board::SWEEP_POINTS,
                result.v_set_mv,
                result.vout_mv,
                result.iout_ma,
                result.vin_mv,
                result.pin_mw,
                result.pout_mw,
                result.samples
            ),
        }
    }

    /// End-of-run summary: the maximum- and minimum-efficiency operating points.
    fn log_summary(&self, reason: &str) {
        defmt::info!(
            "sweep summary ({}): {} points, {} valid",
            reason,
            board::SWEEP_POINTS,
            self.valid_points
        );

        if let Some((index, t)) = self.best {
            defmt::info!(
                "  max eta={}.{}% at point {} ({} mV)",
                t / 10,
                t % 10,
                index as u32 + 1,
                Self::setpoint_mv(index)
            );
        }
        if let Some((index, t)) = self.worst {
            defmt::info!(
                "  min eta={}.{}% at point {} ({} mV)",
                t / 10,
                t % 10,
                index as u32 + 1,
                Self::setpoint_mv(index)
            );
        }
        if self.best.is_none() {
            defmt::warn!("  no valid efficiency points (INA228 absent or too lightly loaded)");
        }
    }

    /// Clear the best/worst search, ready for a fresh sweep.
    fn reset_results(&mut self) {
        self.best = None;
        self.worst = None;
        self.valid_points = 0;
    }
}

impl Default for SweepController {
    fn default() -> Self {
        Self::new()
    }
}
