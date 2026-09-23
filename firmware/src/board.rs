//! Board constants and analog scaling (tune at bring-up).

/// Supply electrical limits (design targets).
pub const VOUT_MAX_MV: u32 = 60_000;
pub const VOUT_MIN_MV: u32 = 0;
pub const IOUT_MAX_MA: u32 = 20_000;
pub const POWER_MAX_MW: u32 = 240_000;

/// Input bus limits, supervised from the INA228 (rev2 input-side monitor).
/// These sit outside the PD contract itself and act as a firmware backstop on
/// top of the INA228 ALERT → SWITCH_EN hardware protection. The current limit
/// used at runtime is the active PD contract cap plus a small margin, falling
/// back to `IIN_MAX_MA` on a non-PD (XT90) input.
pub const VIN_MAX_MV: u32 = 58_000;
pub const IIN_MAX_MA: i32 = 22_000;
/// Headroom above the negotiated input-current cap before an input-overcurrent
/// fault is latched (covers measurement noise and transients).
pub const IIN_MARGIN_MA: i32 = 500;

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

/// DAC 12-bit. The DAC reference is VREF+, which this board ties to +3V3 (the
/// internal VREFBUF is left high-impedance in `main`), so the full-scale
/// reference is the 3.3 V rail rather than the 2.5 V VREFBUF setting.
pub const DAC_MAX_CODE: u16 = 4095;
pub const DAC_VREF_MV: u32 = 3300;

/// Max CV DAC code change per supply tick (see `control::dac_cv::CvDac::slew`).
/// The supply tick is 1 ms, so this is N LSB per ms (~N*1000 LSB/s). Tune down
/// for a gentler ramp if output-stage ringing/FET heating persists.
pub const CV_SLEW_MAX_LSB_PER_TICK: u16 = 4;

/// I2C addresses
pub const TPS26750_ADDR: u8 = 0x21;
pub const SSD1306_ADDR: u8 = 0x3C;
pub const CAT24C512_ADDR: u8 = 0x50;
/// INA228 input power monitor on the PD bus (A0 = A1 = GND).
pub const INA228_ADDR: u8 = 0x40;

/// INA228 scaling. R60 = 8 mΩ on the input bus; 20 A × 8 mΩ = 160 mV, which
/// fits the ±163.84 mV `ADCRANGE = 0` shunt range with ~2 % headroom.
pub const INA228_SHUNT_MOHM: u32 = 8;
pub const INA228_MAX_CURRENT_MA: u32 = IOUT_MAX_MA;
/// CONFIG: ADCRANGE = 0 (±163.84 mV), shunt temperature compensation off.
pub const INA228_CONFIG: u16 = 0x0000;
/// ADC_CONFIG: MODE = 0xF (continuous bus + shunt + temperature), 1052 µs per
/// conversion, AVG = 16 → ~50 ms per full update. Sample it every 100 ms.
pub const INA228_ADC_CONFIG: u16 = 0xFB6A;
/// SHUNT_CAL = 13107.2e6 × CURRENT_LSB × R_shunt with
/// CURRENT_LSB = 20 A / 2^19 = 38.147 µA and R_shunt = 8 mΩ → 4000.
pub const INA228_SHUNT_CAL: u16 = 4_000;

/// Auto-tracking PD rail selection.
///
/// The LT8390A only leaves its 4-switch buck-boost region when `VIN/VOUT` is at
/// least ~1.33 (clean buck) or at most ~0.75 (clean boost); the peak-buck/peak-
/// boost crossover sits at 0.98–1.04. These percentages add a small guard band
/// on top of the datasheet typical thresholds. They are the tuning knobs for
/// the bench efficiency sweep: lower them to allow rails closer to the
/// transition, raise them for more margin.
pub const AUTO_TRACK_BUCK_MIN_RATIO_PCT: u32 = 135;
pub const AUTO_TRACK_BOOST_MAX_RATIO_PCT: u32 = 70;
/// Delivered-power headroom required of a candidate rail before it is accepted
/// (`rail_mw >= requested_mw * pct / 100`), covering conversion losses.
pub const AUTO_TRACK_POWER_HEADROOM_PCT: u32 = 110;
/// A setpoint must be stable this long before a rail change is commanded, and
/// two rail changes are never closer together than the interval below.
pub const AUTO_TRACK_SETTLE_MS: u64 = 300;
pub const AUTO_TRACK_MIN_INTERVAL_MS: u64 = 1_000;
/// Park the converter while the input rail is renegotiated. The LT8390A copes
/// with VIN steps, but a PD source can momentarily drop VBUS to 5 V during a
/// re-request, so the output is briefly disabled to protect the stage.
pub const AUTO_TRACK_DISABLE_DURING_SWITCH: bool = true;

/// Converter disable: active level (verify on bench vs LT8390 RUN).
pub const CONVERTER_DISABLE_ACTIVE_HIGH: bool = true;

/// UI timing
pub const DEBOUNCE_MS: u64 = 25;
pub const UI_REFRESH_MS: u64 = 80;
pub const INPUT_POLL_MS: u64 = 5;
pub const SUPPLY_TICK_MS: u64 = 1;
pub const ADC_SAMPLE_MS: u64 = 2;
/// INA228 refresh period. Must be longer than the chip's ~50 ms conversion
/// cycle (ADC_CONFIG above) or reads return overlapping samples.
pub const INA228_POLL_MS: u64 = 100;

/// Service mode: hand off to the ROM bootloader so the board can be reflashed
/// over UART through the on-board FT234XD (USART3, PC10/PC11).
///
/// Listener baud for the AN3155 sync byte. It MUST match the host tool's
/// connect baud (`STM32CubeProgrammer -c ... br=`, `stm32flash -b`), because
/// only the *first* byte is matched here — after the handoff the ROM loader
/// auto-bauds. STM32CubeProgrammer defaults to 115200; stm32flash to 57600.
pub const SERVICE_UART_BAUD: u32 = 115_200;

/// Watch USART3 for the bootloader sync byte (`0x7F`) and hand off when it
/// arrives. This is what makes "open the programmer and connect" just work.
pub const SERVICE_UART_AUTODETECT: bool = true;

/// Hold BTN1 through power-up (or reset) to enter service mode. Deterministic,
/// needs no UART, and doubles as the bring-up test for the handoff path.
pub const SERVICE_BOOT_HOLD: bool = true;

/// Delay after parking the output before resetting into the bootloader, giving
/// the converter time to shut down and the output to discharge.
///
/// NOTE: this covers only the moments *before* the reset. Once the MCU resets,
/// PA11 floats and Q13's gate is unconstrained, so the converter's state during
/// flashing is set by the R7/R8 divider — see the /Converter/Conv-Disable
/// finding in `BENCH.md`.
pub const SERVICE_PARK_SETTLE_MS: u64 = 100;
