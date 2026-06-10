use crate::board;

/// CC limit DAC on PA6 / DAC2 CH1.
pub struct CcDac {
    pub code: u16,
}

impl CcDac {
    pub fn new() -> Self {
        Self { code: 0 }
    }

// In dac_cc.rs
pub fn ma_to_code(i_ma: u32) -> u16 {
    // 1. Calculate required V_CTRL in mV
    // V_CTRL (V) = 0.02 * I_out(A) + 0.25
    // V_CTRL (mV) = 0.02 * (I_ma / 1000) * 1000 + 250 = (I_ma * 20 / 1000) + 250
    let v_ctrl_mv = (i_ma * 20 / 1000) + 250;
    
    // 2. Map V_CTRL_mv to DAC code
    // Assuming DAC_REF_MV = 3300mV and 12-bit DAC
    let code = (v_ctrl_mv * board::DAC_MAX_CODE as u32) / board::DAC_VREF_MV;
    
    code.min(board::DAC_MAX_CODE as u32) as u16
}

    pub fn set_current(&mut self, i_set_ma: u32) {
        self.code = Self::ma_to_code(i_set_ma);
    }
}

impl Default for CcDac {
    fn default() -> Self {
        Self::new()
    }
}
