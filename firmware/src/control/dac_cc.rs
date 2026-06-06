use crate::board;

/// CC limit DAC on PA6 / DAC2 CH1.
pub struct CcDac {
    pub code: u16,
}

impl CcDac {
    pub fn new() -> Self {
        Self { code: 0 }
    }

    pub fn ma_to_code(i_ma: u32) -> u16 {
        let i = i_ma.min(board::IOUT_MAX_MA);
        ((i * board::DAC_MAX_CODE as u32) / board::IOUT_MAX_MA) as u16
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
