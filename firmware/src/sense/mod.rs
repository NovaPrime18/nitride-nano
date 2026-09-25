//! Analog front-end: ADC sampling, unit scaling, telemetry filtering, input
//! power monitoring, and the derived efficiency measurement shared with the
//! sweep diagnostics.

pub mod adc_sense;
pub mod efficiency;
pub mod ina_sense;
