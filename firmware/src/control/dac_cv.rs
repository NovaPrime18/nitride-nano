//! Constant-voltage setpoint DAC (PA4 / DAC1 CH1): open-loop map from a voltage
//! setpoint to a DAC code, with code-rate slew limiting between setpoint
//! changes.
//!
//! The converter's feedback network *inverts* the DAC: code 0 produces the
//! maximum output (~72.4 V) and larger codes produce less, so this mapping runs
//! backward (high Vout → low code).
//!
//! # Two maps
//!
//! * The **analytic** map is derived from the physical feedback network
//!   ([`board::CV_FB_REF_MV`], [`board::CV_FB_TOP_OHM`], [`board::CV_FB_BOTTOM_OHM`],
//!   [`board::CV_SUM_OHM`]) rather than from a hand-entered table. The network
//!   is linear, so no table is needed; retuning the divider only means
//!   updating those constants. It is correct to ±0.5–1 % on real hardware
//!   (resistor/reference/DAC tolerances × the 35.7 summing gain).
//! * Once the board has been swept and fitted
//!   (`analysis/cv_cal/fit.py`, see `analysis/CV_CALIBRATION_PLAN.md`), the
//!   **calibrated** affine map `A − B·code` ([`board::CV_CAL_*`]) takes over
//!   and the analytic one remains as the fallback and the physics reference.
//!
//! The map in force is whichever `CV_CAL_VALID` selects; the `implied_*`
//! diagnostics below always use the analytic network, because their job is to
//! ask "is the DAC pin where the nominal network assumes".

use crate::board;

/// CV setpoint DAC with an open-loop (inverted) voltage→code map derived from
/// the converter's feedback network, plus code-rate slew limiting.
pub struct CvDac {
    pub code: u16,
}

impl CvDac {
    pub fn new() -> Self {
        Self {
            // Default to the minimum output (full-scale code on this inverted
            // hardware); the first supply tick overwrites it from the setpoint.
            code: board::DAC_MAX_CODE,
        }
    }

    /// Output voltage (mV) commanded with the CV DAC at code 0, i.e. the top of
    /// the output range. From the FB current sum, this is
    /// `CV_FB_REF_MV·(1 + R19/R36 + R19/R20)`; with a bench fit active it is
    /// [`board::CV_CAL_A_MV`] instead.
    pub fn vout_at_code_zero_mv() -> u64 {
        if board::CV_CAL_VALID {
            return board::CV_CAL_A_MV as u64;
        }
        let top = board::CV_FB_TOP_OHM as u64;
        let bottom = board::CV_FB_BOTTOM_OHM as u64;
        let sum = board::CV_SUM_OHM as u64;
        (board::CV_FB_REF_MV as u64 * (sum * bottom + top * bottom + top * sum)) / (sum * bottom)
    }

    /// Output voltage (mV) the open-loop map commands for a DAC code. This is
    /// the inverse of [`Self::mv_to_code`], used by the `supply:` diagnostics.
    pub fn code_to_vout_mv(code: u16) -> u32 {
        if board::CV_CAL_VALID {
            // VOUT = A − B·code, kept in µV so the µV-per-LSB slope keeps its
            // resolution; negative targets (large codes) clamp to 0 mV.
            let a_uv = board::CV_CAL_A_MV as u64 * 1_000;
            let v_uv = a_uv.saturating_sub(board::CV_CAL_B_UV_PER_LSB as u64 * code as u64);
            return (v_uv / 1_000) as u32;
        }
        let v0 = Self::vout_at_code_zero_mv() as i64;
        let top = board::CV_FB_TOP_OHM as i64;
        let sum = board::CV_SUM_OHM as i64;
        let vdac_mv = (board::DAC_VREF_MV as i64 * code as i64) / board::DAC_MAX_CODE as i64;
        (v0 - top * vdac_mv / sum).max(0) as u32
    }

    /// Which map is in force, for the boot banner.
    pub fn map_label() -> &'static str {
        if board::CV_CAL_VALID {
            "calibrated (A/B fit)"
        } else {
            "analytic (R19/R20/R36)"
        }
    }

    /// Nominal DAC-pin voltage (mV) for a code, i.e. `VREF·code/DAC_MAX_CODE`.
    pub fn nominal_dac_mv(code: u16) -> u32 {
        (board::DAC_VREF_MV as u64 * code as u64 / board::DAC_MAX_CODE as u64) as u32
    }

    /// DAC-pin voltage (mV) implied by a measured output, assuming FB is
    /// regulated to [`board::CV_FB_REF_MV`] and the nominal divider. Comparing
    /// this with [`Self::nominal_dac_mv`] for the code in use separates a
    /// DAC/reference error from a feedback-network mismatch: if the two differ,
    /// the DAC pin is not at the voltage the map assumes.
    pub fn implied_dac_mv(vout_mv: u32) -> i32 {
        let v0 = Self::vout_at_code_zero_mv() as i64;
        let top = board::CV_FB_TOP_OHM as i64;
        let sum = board::CV_SUM_OHM as i64;
        ((v0 - vout_mv as i64) * sum / top) as i32
    }

    /// FB-pin voltage (mV) implied by a measured output and the DAC code in use,
    /// assuming the nominal divider. The LT8390A regulates FB to
    /// [`board::CV_FB_REF_MV`] (1 V), so:
    ///
    /// * `≈ 1000 mV` — the chip is regulating; any output error is the load or
    ///   the feedback network, not saturation.
    /// * `< 1000 mV` — the chip is asking for more output but cannot deliver it
    ///   (current limit, deep-buck sensing, thermal, ...). This is the signature
    ///   of the collapse seen on the 2026-09-27 bench run.
    /// * `> 1000 mV` — the output overshot / the loop is above target.
    pub fn implied_fb_mv(vout_mv: u32, code: u16) -> u32 {
        let g19 = 1_000_000_000u64 / board::CV_FB_TOP_OHM as u64;
        let g36 = 1_000_000_000u64 / board::CV_SUM_OHM as u64;
        let g20 = 1_000_000_000u64 / board::CV_FB_BOTTOM_OHM as u64;
        let g_sum = g19 + g20 + g36;
        let vdac = Self::nominal_dac_mv(code) as u64;
        ((vout_mv as u64 * g19 + vdac * g36) / g_sum) as u32
    }

    /// DAC code that produces `vout_mv` in the open-loop (inverted) map.
    ///
    /// Summing the currents into the LT8390A FB pin (see `board::CV_FB_*`) gives
    ///
    /// ```text
    ///   V_OUT = V(code 0) − (R19/R36)·V_DAC,   V_DAC = VREF·code/DAC_MAX_CODE
    /// ```
    ///
    /// so inverting it yields
    ///
    /// ```text
    ///   code = (V(code 0) − V_OUT) · R36 · DAC_MAX_CODE / (R19 · VREF)
    /// ```
    ///
    /// With a bench fit active the calibrated affine map is inverted instead:
    ///
    /// ```text
    ///   code = (CV_CAL_A_MV − V_OUT) · 1000 / CV_CAL_B_UV_PER_LSB
    /// ```
    ///
    /// Setpoints at or above the code-0 output clamp to code 0; everything is
    /// clamped to the 12-bit range.
    pub fn mv_to_code(&self, vout_mv: u32) -> u16 {
        let max_code = board::DAC_MAX_CODE as u64;
        if board::CV_CAL_VALID {
            let a_mv = board::CV_CAL_A_MV as u64;
            let mv = (vout_mv as u64).min(a_mv);
            let num = (a_mv - mv) * 1_000;
            let den = board::CV_CAL_B_UV_PER_LSB as u64;
            // Round to nearest instead of truncating, then clamp to the DAC range.
            return ((num + den / 2) / den).min(max_code) as u16;
        }
        let top = board::CV_FB_TOP_OHM as u64;
        let sum = board::CV_SUM_OHM as u64;

        let v_at_code0 = Self::vout_at_code_zero_mv();
        let mv = (vout_mv as u64).min(v_at_code0);

        let num = (v_at_code0 - mv) * sum * max_code;
        let den = top * board::DAC_VREF_MV as u64;
        // Round to nearest instead of truncating, then clamp to the DAC range.
        ((num + den / 2) / den).min(max_code) as u16
    }

    /// Nudge the current `code` toward `target` by at most
    /// [`board::CV_SLEW_MAX_LSB_PER_TICK`] counts per call. Called once per
    /// supply tick so the DAC output steps gently instead of jumping on a
    /// setpoint change (avoids output-stage ringing / FET heating). On the
    /// inverted DAC, raising the output voltage means `target < code`.
    pub fn slew(&mut self, target: u16) {
        let step = board::CV_SLEW_MAX_LSB_PER_TICK as i32;
        let delta = target as i32 - self.code as i32;
        if delta > step {
            self.code = (self.code as i32 + step) as u16;
        } else if delta < -step {
            self.code = (self.code as i32 - step).max(0) as u16;
        } else {
            self.code = target;
        }
    }
}

impl Default for CvDac {
    fn default() -> Self {
        Self::new()
    }
}
