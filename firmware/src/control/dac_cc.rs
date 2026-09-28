//! Constant-current limit DAC (PA6 / DAC2 CH1) driving the converter's CTRL pin
//! **through the board's summing network** (R48/R5/R4 — see `board::CTRL_*`).
//!
//! Two conversions matter here and they are not the same:
//!
//! * [`CcDac::ma_to_code`] turns a commanded output-current limit into a DAC
//!   code, inverting the R48/R5/R4 network and the LT8390A CTRL transfer.
//! * [`CcDac::current_ma_for_code`] / [`CcDac::ctrl_mv_for_code`] report what a
//!   code *actually* produces, for the `supply:` diagnostics.
//! * [`CcDac::setpoint_to_code`] is the entry point the UI path uses: it clamps
//!   the user setpoint to the settable range and subtracts
//!   [`board::CC_CURRENT_OFFSET_MA`], the bench-measured loop offset, so the
//!   number on the I-LIM screen is the current actually drawn.
//!
//! Driving the DAC as if it fed CTRL directly (the previous behaviour) ignored
//! the +VREF offset and the divider attenuation, so every programmed limit was
//! ~14 % low and the absolute error grew with the setpoint.

use crate::board;

/// CC limit DAC on PA6 / DAC2 CH1.
pub struct CcDac {
    /// Last computed 12-bit output code.
    pub code: u16,
}

impl CcDac {
    pub fn new() -> Self {
        Self { code: 0 }
    }

    /// Scaled conductances (nS) of the CTRL summing network. Computed from the
    /// board constants so a resistor change only needs a `board.rs` edit; a few
    /// 64-bit divides per supply tick are nothing on the G474.
    fn conductances() -> (u64, u64, u64) {
        let g_series = 1_000_000_000u64 / board::CTRL_SERIES_OHM as u64;
        let g_vref = 1_000_000_000u64 / board::CTRL_VREF_OHM as u64;
        let g_gnd = 1_000_000_000u64 / board::CTRL_GND_OHM as u64;
        (g_series, g_vref, g_gnd)
    }

    /// DAC code → CTRL-pin voltage in mV, per the R48/R5/R4 network.
    pub fn ctrl_mv_for_code(code: u16) -> u32 {
        let (g_series, g_vref, g_gnd) = Self::conductances();
        let g_sum = g_series + g_vref + g_gnd;
        let max_code = board::DAC_MAX_CODE as u64;
        let vdac_mv = (board::DAC_VREF_MV as u64 * code as u64 + max_code / 2) / max_code;
        let num = vdac_mv * g_series + board::LT8390_VREF_MV as u64 * g_vref;
        (num / g_sum) as u32
    }

    /// CTRL-pin term above the 250 mV offset, in mV, i.e. `10 · V(ISP−ISN)`.
    /// One unit is 0.1 mV of sense threshold, which keeps the 0.1 A steps from
    /// collapsing onto one DAC code at the bottom of the range; capped at the
    /// 100 mV full-scale value (`1000` = 100.0 mV).
    fn ctrl_term_mv_for_current(i_ma: u32) -> u32 {
        let t = (i_ma as u64 * board::ISENSE_SHUNT_MOHM as u64) / 100;
        t.min(board::LT8390_CTRL_MAX_SENSE_MV as u64 * 10) as u32
    }

    /// CTRL-pin voltage (`mV`) that a requested limit needs, from the LT8390A
    /// datasheet transfer with the board shunt: linear over 0.30–1.15 V
    /// (threshold 5–90 mV), then the 1.15–1.35 V transition to the 100 mV full
    /// scale (200 mV of CTRL per 10 mV of threshold, i.e. `×2` per term unit).
    fn ctrl_mv_for_current(i_ma: u32) -> u32 {
        let term = Self::ctrl_term_mv_for_current(i_ma);
        let lin_top = board::LT8390_CTRL_LINEAR_MAX_SENSE_MV * 10;
        if term <= lin_top {
            board::LT8390_CTRL_OFFSET_MV + term
        } else {
            board::LT8390_CTRL_LINEAR_TOP_MV + 2 * (term - lin_top)
        }
    }

    /// Current limit (`mA`) a DAC code actually produces, through the network and
    /// the chip transfer. Below the 0.30 V CTRL latch-off the part stops
    /// switching, so this reports 0 rather than a small-but-switching limit.
    ///
    /// The threshold is kept in µV so the result does not lose resolution to
    /// integer millivolt truncation (1 CTRL mV is 50 mA at R18 = 2 mΩ).
    pub fn current_ma_for_code(code: u16) -> u32 {
        let vctrl = Self::ctrl_mv_for_code(code);
        if vctrl < board::LT8390_CTRL_LATCHOFF_MV {
            return 0;
        }
        let thr_uv = if vctrl <= board::LT8390_CTRL_LINEAR_TOP_MV {
            (vctrl - board::LT8390_CTRL_OFFSET_MV) as u64 * 100
        } else {
            let t = board::LT8390_CTRL_LINEAR_MAX_SENSE_MV as u64 * 1_000
                + (vctrl - board::LT8390_CTRL_LINEAR_TOP_MV) as u64 * 50;
            t.min(board::LT8390_CTRL_MAX_SENSE_MV as u64 * 1_000)
        };
        // I_mA = thr_µV / R18_mΩ  (µV/mΩ = mA)
        (thr_uv / board::ISENSE_SHUNT_MOHM as u64) as u32
    }

    /// Clamp a requested limit into the range the firmware can safely command:
    /// [`board::CC_SET_MIN_MA`] … [`board::CC_SET_MAX_MA`]. The theoretical
    /// shunt floor ([`board::CC_MIN_MA`]) is not used here because one DAC code
    /// is ~40 mA at R18 = 2 mΩ and the exact floor request rounds to a code
    /// *below* the CTRL latch-off — which would stop the stage instead of
    /// limiting it.
    pub fn clamp_current(i_ma: u32) -> u32 {
        i_ma
            .max(board::CC_SET_MIN_MA)
            .min(board::CC_SET_MAX_MA)
    }

    /// User setpoint (the current the user expects to be *drawn*) → the current
    /// to actually command. The loop delivers [`board::CC_CURRENT_OFFSET_MA`]
    /// more than commanded near the low end, so the offset is removed here and
    /// the I-LIM number is the real output current.
    fn commanded_ma(i_set_ma: u32) -> u32 {
        i_set_ma.saturating_sub(board::CC_CURRENT_OFFSET_MA)
    }

    /// DAC code for a user *setpoint*: range clamp, bench offset, then the raw
    /// network/chip inversion. This is the only entry point the UI path needs.
    pub fn setpoint_to_code(i_set_ma: u32) -> u16 {
        Self::ma_to_code(Self::commanded_ma(Self::clamp_current(i_set_ma)))
    }

    /// Convert a current limit in mA to a 12-bit DAC code, inverting both the
    /// board's CTRL summing network and the LT8390A transfer.
    ///
    /// The DAC voltage that produces `i_ma` at CTRL is
    ///
    /// ```text
    ///   V_DAC = (V_CTRL·G_sum − VREF·G_R5) / G_R48
    /// ```
    ///
    /// with the conductances of R48/R5/R4. Clamped to the 12-bit range; a
    /// request whose CTRL target is below [`board::LT8390_CTRL_LATCHOFF_MV`] is
    /// reported by [`Self::current_ma_for_code`] as 0 rather than silently
    /// rounding up into a low-but-switching limit.
    pub fn ma_to_code(i_ma: u32) -> u16 {
        let (g_series, g_vref, g_gnd) = Self::conductances();
        let g_sum = g_series + g_vref + g_gnd;

        let vctrl_target = Self::ctrl_mv_for_current(i_ma) as u64;
        let num = vctrl_target * g_sum;
        let vref_term = board::LT8390_VREF_MV as u64 * g_vref;
        let vdac_mv = if num > vref_term {
            (num - vref_term + g_series / 2) / g_series
        } else {
            0
        };

        let max_code = board::DAC_MAX_CODE as u64;
        let code = (vdac_mv * max_code + board::DAC_VREF_MV as u64 / 2)
            / board::DAC_VREF_MV as u64;
        code.min(max_code) as u16
    }

    /// Update `code` for a new user setpoint (range-clamped and offset-cancelled
    /// via [`Self::setpoint_to_code`]). The caller writes it to the DAC.
    pub fn set_current(&mut self, i_set_ma: u32) {
        self.code = Self::setpoint_to_code(i_set_ma);
    }
}

impl Default for CcDac {
    fn default() -> Self {
        Self::new()
    }
}
