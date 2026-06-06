use embassy_time::Instant;

use crate::board;

/// CV setpoint DAC with calibration table, slew limiting, and closed-loop trim.
pub struct CvDac {
    pub code: u16,
    cal: CalTable,
    last_slew: Instant,
}

#[derive(Clone, Copy)]
struct CalPoint {
    code: u16,
    vout_mv: u32,
}

const CAL: [CalPoint; 5] = [
    CalPoint {
        code: 0,
        vout_mv: 0,
    },
    CalPoint {
        code: 1024,
        vout_mv: 15_000,
    },
    CalPoint {
        code: 2048,
        vout_mv: 30_000,
    },
    CalPoint {
        code: 3072,
        vout_mv: 45_000,
    },
    CalPoint {
        code: 4095,
        vout_mv: 60_000,
    },
];

type CalTable = [CalPoint; 5];

impl CvDac {
    pub fn new() -> Self {
        Self {
            code: 0,
            cal: CAL,
            last_slew: Instant::now(),
        }
    }

    pub fn mv_to_code(&self, vout_mv: u32) -> u16 {
        let mv = vout_mv.min(board::VOUT_MAX_MV);
        for i in 0..self.cal.len() - 1 {
            let a = self.cal[i];
            let b = self.cal[i + 1];
            if mv <= b.vout_mv {
                let span = b.vout_mv.saturating_sub(a.vout_mv);
                if span == 0 {
                    return a.code;
                }
                let t = mv.saturating_sub(a.vout_mv);
                let code = a.code as u32 + (t * (b.code - a.code) as u32) / span;
                return code.min(board::DAC_MAX_CODE as u32) as u16;
            }
        }
        board::DAC_MAX_CODE
    }

    pub fn apply_slew(&mut self, target_mv: u32, now: Instant) -> u32 {
        let dt_ms = now.duration_since(self.last_slew).as_millis() as u32;
        self.last_slew = now;
        let max_step = board::CV_SLEW_MV_PER_S * dt_ms / 1000;
        let current = self.code_to_mv(self.code);
        if target_mv > current {
            (current + max_step).min(target_mv)
        } else {
            current.saturating_sub(max_step).max(target_mv)
        }
    }

    pub fn code_to_mv(&self, code: u16) -> u32 {
        let c = code as u32;
        for i in 0..self.cal.len() - 1 {
            let a = self.cal[i];
            let b = self.cal[i + 1];
            if c <= b.code as u32 {
                let span = (b.code - a.code) as u32;
                if span == 0 {
                    return a.vout_mv;
                }
                let t = c.saturating_sub(a.code as u32);
                return a.vout_mv + t * (b.vout_mv - a.vout_mv) / span;
            }
        }
        board::VOUT_MAX_MV
    }

    pub fn trim(&mut self, error_mv: i32) {
        if error_mv == 0 {
            return;
        }
        let step = error_mv * board::CV_TRIM_GAIN_NUM / board::CV_TRIM_GAIN_DEN;
        let new = (self.code as i32 + step).clamp(0, board::DAC_MAX_CODE as i32) as u16;
        self.code = new;
    }

    pub fn update_closed_loop(&mut self, v_set_mv: u32, v_meas_mv: u32) {
        let error = v_set_mv as i32 - v_meas_mv as i32;
        if error.abs() < 5 {
            self.code = self.mv_to_code(v_set_mv);
            if error > 0 && self.code < board::DAC_MAX_CODE {
                self.code += 1;
            }
        } else {
            self.code = self.mv_to_code(v_set_mv);
            self.trim(error);
        }
    }
}

impl Default for CvDac {
    fn default() -> Self {
        Self::new()
    }
}
