//! Board constants and analog scaling (tune at bring-up).

/// Supply electrical limits (design targets).
pub const VOUT_MAX_MV: u32 = 60_000;
pub const VOUT_MIN_MV: u32 = 0;
pub const IOUT_MAX_MA: u32 = 20_000;
pub const POWER_MAX_MW: u32 = 240_000;

/// NTC thermistor settings (matches PD240W-Firmware-example).
/// Circuit: 3.3V → R_PULLUP (4.7k) → NTC → GND, voltage at junction.
pub const NTC_BETA: f32 = 3950.0;
pub const NTC_R25_OHM: f32 = 10_000.0;
pub const NTC_PULLUP_OHM: f32 = 4_700.0;
pub const NTC_OVERTEMP_C: i32 = 80;
// TODO(dead-code): derating threshold carried over from the PD240W example, but no
// thermal-derating logic exists in this firmware yet, so nothing reads it.
// Preserved for when graduated derating is implemented.
// pub const NTC_DERATE_START_C: i32 = 70;

// TODO(dead-code): graduated temperature thresholds from the PD240W example AppConfig.
// Only NTC_OVERTEMP_C is actually enforced (see control::supply); these graduated
// levels are never referenced. Preserved for a future UI/derating feature.
// pub const TEMP_CAUTION_C: i32 = 50;
// pub const TEMP_WARNING_C: i32 = 65;
// pub const TEMP_CRITICAL_C: i32 = 75;
// pub const TEMP_SHUTDOWN_C: i32 = 80;

/// ADC full-scale reference (VREF+ = VDDA unless VREFBUF used for DAC only).
pub const ADC_VREF_MV: u32 = 3300;
// TODO(dead-code): misleading leftover — all ADCs run at 12-bit resolution and every
// scaling path in `sense::adc_sense` divides raw counts by 4096. Nothing references
// this 16-bit "oversampled" constant.
// pub const ADC_MAX: u32 = 65535; // 16-bit oversampled

/// Divider ratios: physical = adc_counts * SCALE / 4096 (12-bit ADC)
/// TODO: derive from Converter.kicad_sch resistor networks.
pub const VOUT_SENSE_NUM: u32 = 85_140; // mV at full scale (248k/10k divider, 3.3V ref)
pub const ISENSE_MV_PER_A: u32 = 18; // mV/A at ISMON node (PA3, calibrated)
pub const ISENSE_OFFSET_MV: u32 = 248; // mV offset at 0A (PA3, calibrated)
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

/// UI timing
pub const DEBOUNCE_MS: u64 = 25;
pub const UI_REFRESH_MS: u64 = 80;
pub const INPUT_POLL_MS: u64 = 5;
pub const SUPPLY_TICK_MS: u64 = 1;
pub const ADC_SAMPLE_MS: u64 = 2;
