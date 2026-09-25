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
/// ISMON monitor gain at the PA3 node, in mV per amp. The LT8390A datasheet
/// gives `V_ISMON = 10 · V(ISP−ISN) + V_OFFSET`, and the board's output-current
/// shunt is R18 = 2 mΩ, so the gain is `10 · 2 mΩ = 20 mV/A`. This is a
/// datasheet-derived value, not a per-board trim: verify it against a known load
/// (a shunt swap scales it directly) before changing it.
pub const ISENSE_MV_PER_A: u32 = 20;

/// Zero-current ISMON voltage at the PA3 node, in mV — the per-board offset
/// calibration.
///
/// The LT8390A offset is only specified to 0.20–0.30 V while the current signal
/// is 20 mV/A, so the full 100 mV spread is 5 A of error. Even the 5 mV residual
/// left by a near-typical part is 0.25 A on the display at *every* load — which
/// is what the 2026-09-25 bench run showed (readings low by ~0.25 A from 0.25 A
/// to 2.5 A).
///
/// MEASURE IT WITH THE OUTPUT **ENABLED** AND NO LOAD. The LT8390A's ISMON
/// buffer is powered down with the rest of the chip while `EN/UVLO` is low, so
/// the level read with the converter parked is not the operating offset. The
/// previous firmware learned the zero at boot with the converter disabled; that
/// is why the calibration never took. This constant replaces that learn.
///
/// Bench procedure: enable the output with no load, read the `isense:` RTT line
/// (`raw … mV`), or DMM the ISMON node / R49, and set this to that value. The
/// value below was fitted from the bench run in `analysis/ismon-calibration/`.
pub const ISENSE_ZERO_MV: u32 = 244;
pub const VBUS_SENSE_NUM: u32 = 69_600;

/// DAC 12-bit. The DAC reference is VREF+, which this board ties to +3V3 (the
/// internal VREFBUF is left high-impedance in `main`), so the full-scale
/// reference is the 3.3 V rail rather than the 2.5 V VREFBUF setting.
pub const DAC_MAX_CODE: u16 = 4095;
pub const DAC_VREF_MV: u32 = 3300;

/// CV feedback network (Converter sheet, U1 = LT8390A). The FB pin is a
/// current-summing node:
///
/// ```text
///   VOUT ── R19 ──┬── R20 ── GND
///                 │
///   CV_Set ─ R36 ─┴── FB
/// ```
///
/// The LT8390A regulates FB to [`CV_FB_REF_MV`] (1.000 V typ), so summing the
/// currents into FB gives the open-loop (inverted) control law
///
/// ```text
///   V_OUT = CV_FB_REF_MV·(1 + R19/R36 + R19/R20) − V_DAC·(R19/R36)
/// ```
///
/// with `V_DAC = DAC_VREF_MV · code / DAC_MAX_CODE`. These resistor values are
/// the source of truth for the CV map — `control::dac_cv` derives the
/// setpoint→code mapping from them analytically. (A previous hand-entered
/// calibration table did not match this network: it assumed ~13.5 mV/code while
/// the network gives ~28.8 mV/code, so the real output tracked ~1.9× the
/// setpoint with an offset.)
pub const CV_FB_REF_MV: u32 = 1_000; // LT8390A FB regulation (1.00 V typ)
pub const CV_FB_TOP_OHM: u32 = 357_000; // R19, VOUT → FB
pub const CV_FB_BOTTOM_OHM: u32 = 10_000; // R20, FB → GND
pub const CV_SUM_OHM: u32 = 10_000; // R36, CV_Set → FB

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

/// Highest SPR contract voltage. A rail above this can only be obtained by
/// entering EPR mode; the TPS26750 attempts EPR on its own once a request
/// window above this is allowed and the loaded sink configuration declares EPR
/// PDOs. Keep in step with `0x33` / `0x37` of `config_TPS26750_*_fullFlash.c`.
pub const SPR_MAX_MV: u32 = 20_000;
/// Fixed EPR rails the sink configuration declares (`0x33` PDOs 8-10). The
/// source's own EPR PDOs are invisible in `RX_SOURCE_CAPS` until EPR mode has
/// been entered, so [`crate::pd::auto_track`] injects these as candidates when
/// the output setpoint cannot be served from SPR alone.
pub const EPR_RAILS_MV: [u32; 3] = [28_000, 36_000, 48_000];
/// Current assumed for an injected EPR rail (EPR fixed PDOs carry 5 A).
pub const EPR_RAIL_CURRENT_MA: u32 = 5_000;
/// EPR AVS APDO window declared by the sink configuration (`0x33` PDO 11).
///
/// Any rail above [`SPR_MAX_MV`] is requested as an **EPR AVS** contract inside
/// this window rather than as a fixed PDO. A fixed window above 20 V matches no
/// *visible* SPR PDO before EPR mode entry, and SDAA265 §5.3 then makes the
/// controller fall back to 5 V without ever entering EPR. Asserting
/// `EPR AVS Enable Sink Mode` (0x37 bit 128) is what makes the controller
/// attempt EPR mode entry (TRM Table 4-21). The reference PD240W firmware uses
/// exactly this path. Keep in step with `0x33`/`0x37` of the config image.
pub const EPR_AVS_MIN_MV: u32 = 15_000;
pub const EPR_AVS_MAX_MV: u32 = 48_000;

/// How long to keep the converter output disabled after an EPR request before
/// loading it. Some sources are still settling VBUS through the 20→28/48 V
/// transition; enabling the load then can dip VBUS enough to brown the board
/// out. Two chargers did this at 28 V while a powerbank at the same voltage did
/// not, so this is a ride-through window, not a fixed voltage limit.
pub const EPR_SETTLE_MS: u64 = 800;
/// USB-PD SPR PPS is only defined inside this window; a manual preset that
/// lands here with no matching fixed PDO is served by a PPS contract.
pub const PPS_MIN_MV: u32 = 3_300;
pub const PPS_MAX_MV: u32 = 21_000;
/// A preset this close to a fixed PDO uses the fixed PDO instead of PPS.
pub const PPS_FIXED_PREFER_MV: u32 = 1_000;

/// Converter disable: active level (verify on bench vs LT8390 RUN).
pub const CONVERTER_DISABLE_ACTIVE_HIGH: bool = true;

/// UI timing
pub const DEBOUNCE_MS: u64 = 25;
/// Quadrature counts per encoder detent. `Qei::new` configures the timer for
/// X4 decoding (`Sms::ENCODER_MODE_3`). The fitted encoder produces a detent
/// every two counts (measured: one detent used to fire two `EncTurn` events, and
/// dividing by 4 made the knob half-speed). Raw counts are divided by this so
/// every screen receives one event per detent.
pub const ENCODER_COUNTS_PER_DETENT: i32 = 2;
pub const UI_REFRESH_MS: u64 = 80;
pub const INPUT_POLL_MS: u64 = 5;
pub const SUPPLY_TICK_MS: u64 = 1;
pub const ADC_SAMPLE_MS: u64 = 2;
/// INA228 refresh period. Must be longer than the chip's ~50 ms conversion
/// cycle (ADC_CONFIG above) or reads return overlapping samples.
pub const INA228_POLL_MS: u64 = 100;

/// CFG menu → "Output V sweep": 32 points evenly spaced from 10 V to 56 V,
/// inclusive at both ends (31 intervals, ≈1.48 V/step), each held for 2 s
/// (~64 s total).
///
/// NOTE: `control::dac_cv::CvDac::mv_to_code` derives its map from the FB
/// network, whose code-0 output is well above 60 V, so every sweep point is
/// reached without clamping. `VOUT_MAX_MV` is 60 V, so the supply supervisor's
/// 105 % overvoltage guard never trips on a sweep.
pub const SWEEP_START_MV: u32 = 10_000;
pub const SWEEP_END_MV: u32 = 56_000;
/// Number of points in one sweep (inclusive of both endpoints).
pub const SWEEP_POINTS: u8 = 32;
/// Dwell time per sweep point.
pub const SWEEP_STEP_MS: u64 = 2_000;

/// Sweep efficiency diagnostics: skip this much of each point's dwell before
/// accumulating anything.
///
/// The CV DAC slews at [`CV_SLEW_MAX_LSB_PER_TICK`] (4 LSB/ms ≈ 28.8 mV/code).
/// The worst case is point 0, which starts from the parked code and needs
/// ~482 ms to reach 10 V; 800 ms also covers the output capacitor and the ADC
/// telemetry EMA (tau 150 ms). The remaining 1.2 s of the dwell is the
/// measurement window.
pub const SWEEP_SETTLE_MS: u64 = 800;

/// Accumulation cadence inside the measurement window: one input/output power
/// pair every interval (a 2 s dwell gives ~60 samples).
pub const SWEEP_SAMPLE_MS: u64 = 20;

/// A point needs at least this many accepted samples to count toward the sweep's
/// best/worst summary. Below it the point is still logged but marked invalid, so
/// a stalled loop or a barely-answering INA228 cannot produce a bogus record.
pub const SWEEP_MIN_SAMPLES: u16 = 10;

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
