//! Constant-current limit DAC (PA6 / DAC2 CH1) driving the converter's I_CTRL pin.

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

    /// Convert a current limit in mA to a 12-bit DAC code.
    ///
    /// The converter's current-limit input follows V_CTRL = 0.02 · I_out(A) + 0.25 V
    /// (LT8390 ISMON characteristic), i.e. `i_ma * 20 / 1000 + 250` in mV. The DAC
    /// reference is [`board::DAC_VREF_MV`] (2.5 V from VREFBUF), not VDDA.
    pub fn ma_to_code(i_ma: u32) -> u16 {
        // V_CTRL (mV) = 0.02 * (I_ma / 1000) * 1000 + 250 = (I_ma * 20 / 1000) + 250
        let v_ctrl_mv = (i_ma * 20 / 1000) + 250;

        let code = (v_ctrl_mv * board::DAC_MAX_CODE as u32) / board::DAC_VREF_MV;

        code.min(board::DAC_MAX_CODE as u32) as u16
    }

    /// Update `code` for a new current limit. The caller writes it to the DAC.
    pub fn set_current(&mut self, i_set_ma: u32) {
        self.code = Self::ma_to_code(i_set_ma);
    }
}

impl Default for CcDac {
    fn default() -> Self {
        Self::new()
    }
}
