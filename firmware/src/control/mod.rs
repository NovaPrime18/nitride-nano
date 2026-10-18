//! Output-stage control: CV/CC setpoint DACs, fault supervision, the rev3
//! power-up sequencer, and the output-voltage sweep.

pub mod dac_cc;
pub mod dac_cv;
pub mod sequencer;
pub mod supply;
pub mod sweep;
