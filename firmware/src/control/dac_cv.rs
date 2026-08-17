//! Constant-voltage setpoint DAC (PA4 / DAC1 CH1): open-loop linear mapping
//! from a voltage setpoint to a DAC code.
//!
//! The board's hardware *inverts* the DAC: code 0 produces the maximum output
//! (~55.4 V) and the full-scale code produces 0 V, so this mapping runs
//! backward (high Vout → low code).

use crate::board;

/// CV setpoint DAC with an open-loop (inverted) voltage→code calibration map.
pub struct CvDac {
    pub code: u16,
    cal: CalTable,
}

#[derive(Clone, Copy)]
struct CalPoint {
    code: u16,
    vout_mv: u32,
}

/// Piecewise-linear DAC-code ↔ Vout calibration, measured at bring-up.
///
/// Stored sorted by `code` ascending. Because the converter inverts the DAC,
/// `vout_mv` is monotonically *decreasing*: code 0 → max output (~55.4 V),
/// full-scale code → 0 V. The interior points come from a crude bench
/// calibration (each comment lists the commanded setpoint → measured Vout);
/// the two endpoints are the hardware limits. Points must stay sorted by `code`.
const CAL: [CalPoint; 11] = [
    CalPoint {
        code: 0,
        vout_mv: 55_400,
    },
    CalPoint {
        code: 103,
        vout_mv: 54_800,
    }, // set 54 V -> measured 54.8 V
    CalPoint {
        code: 694,
        vout_mv: 45_500,
    }, // set 46 V -> measured 45.5 V
    CalPoint {
        code: 1138,
        vout_mv: 38_630,
    }, // set 40 V -> measured 38.63 V
    CalPoint {
        code: 1286,
        vout_mv: 36_200,
    }, // set 38 V -> measured 36.2 V
    CalPoint {
        code: 2173,
        vout_mv: 22_150,
    }, // set 26 V -> measured 22.15 V
    CalPoint {
        code: 2394,
        vout_mv: 20_470,
    }, // set 23 V -> measured 20.47 V
    CalPoint {
        code: 2433,
        vout_mv: 17_650,
    }, // set 20 V -> measured 17.65 V
    CalPoint {
        code: 2682,
        vout_mv: 14_000,
    }, // set 17 V -> measured 14.00 V
    CalPoint {
        code: 3097,
        vout_mv: 11_410,
    }, // set 12 V -> measured 11.41 V
    CalPoint {
        code: 4095,
        vout_mv: 0,
    },
];

type CalTable = [CalPoint; 11];

impl CvDac {
    pub fn new() -> Self {
        Self {
            // Default to the minimum output (full-scale code on this inverted
            // hardware); the first supply tick overwrites it from the setpoint.
            code: board::DAC_MAX_CODE,
            cal: CAL,
        }
    }

    /// Open-loop estimate of the DAC code that produces `vout_mv`, interpolated
    /// between calibration points. Because the hardware inverts the DAC, the
    /// mapping runs backward: high Vout → low code (code 0 → ~55.4 V), low Vout
    /// → high code (0 V → full scale). Setpoints outside the table's output
    /// range clamp to the nearest end.
    pub fn mv_to_code(&self, vout_mv: u32) -> u16 {
        let hi = self.cal[0].vout_mv; // code 0 -> max output
        let lo = self.cal[self.cal.len() - 1].vout_mv; // full-scale code -> min output
        let mv = vout_mv.min(hi).max(lo);

        for i in 0..self.cal.len() - 1 {
            let a = self.cal[i]; // lower code, higher vout (inverted)
            let b = self.cal[i + 1];
            if mv >= b.vout_mv {
                let span = a.vout_mv.saturating_sub(b.vout_mv);
                if span == 0 {
                    return a.code;
                }
                let t = a.vout_mv.saturating_sub(mv);
                let code = a.code as u32 + (t * (b.code - a.code) as u32) / span;
                return code.min(board::DAC_MAX_CODE as u32) as u16;
            }
        }
        self.cal[self.cal.len() - 1].code
    }
}

impl Default for CvDac {
    fn default() -> Self {
        Self::new()
    }
}
