//! Board constants and analog scaling (tune at bring-up).

/// Supply electrical limits (design targets).
pub const VOUT_MAX_MV: u32 = 60_000;
pub const VOUT_MIN_MV: u32 = 0;
pub const IOUT_MAX_MA: u32 = 20_000;
pub const POWER_MAX_MW: u32 = 240_000;

/// Default slew rate for CV setpoint changes (mV/s).
pub const CV_SLEW_MV_PER_S: u32 = 1_000;

/// NTC (placeholder — measure R25/Beta on bench).
pub const NTC_BETA: f32 = 3950.0;
pub const NTC_R25_OHM: f32 = 10_000.0;
pub const NTC_PULLUP_OHM: f32 = 10_000.0;
pub const NTC_OVERTEMP_C: i32 = 110;
pub const NTC_DERATE_START_C: i32 = 70;

/// ADC full-scale reference (VREF+ = VDDA unless VREFBUF used for DAC only).
pub const ADC_VREF_MV: u32 = 3300;
pub const ADC_MAX: u32 = 65535; // 16-bit oversampled

/// Divider ratios: physical = adc_counts * SCALE / ADC_MAX
/// TODO: derive from Converter.kicad_sch resistor networks.
pub const VOUT_SENSE_NUM: u32 = 60_000; // mV at full scale
pub const ISENSE_MV_PER_A: u32 = 50; // mV/A at ISMON node (placeholder)
pub const VBUS_SENSE_NUM: u32 = 69_600;

/// DAC 12-bit, VREFBUF target ~2.5 V
pub const DAC_MAX_CODE: u16 = 4095;
pub const DAC_VREF_MV: u32 = 2500;

/// I2C addresses
pub const TPS26750_ADDR: u8 = 0x21;
pub const SSD1306_ADDR: u8 = 0x3C;
pub const CAT24C512_ADDR: u8 = 0x50;

/// Converter disable: active level (verify on bench vs LT8390 RUN).
pub const CONVERTER_DISABLE_ACTIVE_HIGH: bool = true;

/// CV closed-loop trim gain (DAC LSBs per mV error, scaled down).
pub const CV_TRIM_GAIN_NUM: i32 = 1;
pub const CV_TRIM_GAIN_DEN: i32 = 4;

/// UI timing
pub const DEBOUNCE_MS: u64 = 25;
pub const UI_REFRESH_MS: u64 = 80;
pub const INPUT_POLL_MS: u64 = 5;
pub const SUPPLY_TICK_MS: u64 = 1;
pub const ADC_SAMPLE_MS: u64 = 2;
