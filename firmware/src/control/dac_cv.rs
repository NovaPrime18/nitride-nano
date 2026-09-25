//! Constant-voltage setpoint DAC (PA4 / DAC1 CH1): open-loop map from a voltage
//! setpoint to a DAC code, with code-rate slew limiting between setpoint
//! changes.
//!
//! The converter's feedback network *inverts* the DAC: code 0 produces the
//! maximum output (~72.4 V) and larger codes produce less, so this mapping runs
//! backward (high Vout → low code).
//!
//! The map is derived analytically from the physical feedback network
//! ([`board::CV_FB_REF_MV`], [`board::CV_FB_TOP_OHM`], [`board::CV_FB_BOTTOM_OHM`],
//! [`board::CV_SUM_OHM`]) rather than from a hand-entered calibration table. The
//! network is linear, so no table is needed; retuning the divider only means
//! updating those constants.

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
    /// `CV_FB_REF_MV·(1 + R19/R36 + R19/R20)`.
    fn vout_at_code_zero_mv() -> u64 {
        let top = board::CV_FB_TOP_OHM as u64;
        let bottom = board::CV_FB_BOTTOM_OHM as u64;
        let sum = board::CV_SUM_OHM as u64;
        (board::CV_FB_REF_MV as u64 * (sum * bottom + top * bottom + top * sum)) / (sum * bottom)
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
    /// Setpoints at or above the code-0 output clamp to code 0; everything is
    /// clamped to the 12-bit range.
    pub fn mv_to_code(&self, vout_mv: u32) -> u16 {
        let top = board::CV_FB_TOP_OHM as u64;
        let sum = board::CV_SUM_OHM as u64;
        let max_code = board::DAC_MAX_CODE as u64;

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
