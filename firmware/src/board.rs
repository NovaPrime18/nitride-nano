//! Board constants and analog scaling (tune at bring-up).

/// Supply electrical limits (design targets).
pub const VOUT_MAX_MV: u32 = 60_000;
pub const VOUT_MIN_MV: u32 = 0;
pub const IOUT_MAX_MA: u32 = 20_000;
pub const POWER_MAX_MW: u32 = 240_000;

/// Lowest output-voltage **setpoint** worth commanding.
///
/// The LT8390A's `LOADTG`/load-switch drive is only specified from
/// `VOUT = 2.4–3 V` up, and this board senses `Vsense_REG_OUT` on the converter
/// side of the output switch. Commanding below this cannot be regulated, so with
/// the rev3 sequencer it would simply time out in `Ramping` and latch
/// `Fault::OutputNotReached`. The UI floors the knob here so it cannot be walked
/// into a guaranteed fault. Distinct from [`VOUT_MIN_MV`], which is only the
/// Vout bar's display origin.
pub const VSET_MIN_MV: u32 = 3_000;

/// Input bus limits, supervised from the INA228 (U9, rev3 input-side monitor).
/// These sit outside the PD contract itself and act as a firmware backstop on
/// top of the hardware power-path protection. The current limit used at runtime
/// is the active PD contract cap plus a small margin, falling back to
/// `IIN_MAX_MA` on a non-PD (XT90) input.
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

/// Enable NTC overtemperature supervision.
///
/// A failed (open/shorted) thermistor reads as `NTC_INVALID_C` and latches
/// `Fault::OverTemp` by design — fail-safe. On a bring-up board whose NTCs are
/// unpopulated, or whose analogue front-end is not yet working (so the readings
/// are meaningless), that latch blocks every output enable. Set this **false**
/// for those bench sessions only: it disables the overtemperature trip
/// entirely, so nothing watches the junction temperature — never leave it
/// false once the thermal hardware is known good.
pub const NTC_SENSE_ENABLE: bool = true;

// ---------------------------------------------------------------------------
// First-enable interlock (see `src/preflight.rs`)
// ---------------------------------------------------------------------------

/// Block the converter enable when the boot self-test reports a `[FAIL]`.
///
/// The self-test runs at boot and logs one `preflight:` line per check (ADC/DAC
/// reference, thermistors, power-path gate readback, current monitors, input
/// voltage, option bytes). A failure means the firmware would be driving power
/// hardware it cannot measure or supervise, so the enable is refused and
/// `Fault::PreflightFailed` is latched (OLED label `SELF-TEST`, LED code 14).
///
/// Set this **false only for a supervised bench run** where you have accepted
/// the specific failed check — the log names it.
pub const PREFLIGHT_BLOCK_ENABLE: bool = true;
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

/// Divider ratios: physical = adc_counts * SCALE / 4096 (12-bit ADC).
///
/// Both are the *series* resistance over the bottom leg, times the 3.3 V
/// reference; the resistor designators are from the rev3 sheets.
///
/// * `VOUT_SENSE_NUM` — `Vsense_REG_OUT` (PA0): R85 + R86 = 248 kΩ over R87 = 10 kΩ.
/// * `VBUS_SENSE_NUM` — `Bus voltage` (PA7): R44 + R47 = 200 kΩ over R97 = 10 kΩ.
pub const VOUT_SENSE_NUM: u32 = 85_140; // mV at full scale (248k/10k, 3.3 V ref)
pub const VBUS_SENSE_NUM: u32 = 69_600; // mV at full scale (200k/10k, 3.3 V ref)

/// Zero-current ISMON voltage at the PA3 node, in mV — the per-board offset
/// calibration.
///
/// The LT8390A gives `V_ISMON = 10 · V(ISP−ISN) + V_OFFSET`, and it is only
/// specified to 0.20–0.30 V, so the full 100 mV spread is 5 A of error at the ×1
/// gain of 20 mV/A (0.5 A at the ×10 gain of 200 mV/A). Even the residual left by
/// a near-typical part is 0.25 A on the display at *every* load at ×1 — which is
/// what the 2026-09-25 bench run showed (readings low by ~0.25 A from 0.25 A to
/// 2.5 A).
///
/// The offset is **range independent** (it is inside the chip, after the
/// differential), but the ×10 mux does step the ISP/ISN common mode by ~87 mV
/// (`R102`/`R103`), so re-check this in both ranges.
///
/// MEASURE IT WITH THE OUTPUT **ENABLED** AND NO LOAD. The LT8390A's ISMON
/// buffer is powered down with the rest of the chip while `EN/UVLO` is low, so
/// the level read with the converter parked is not the operating offset. The
/// previous firmware learned the zero at boot with the converter disabled; that
/// is why the calibration never took. This constant replaces that learn.
///
/// Bench procedure: enable the output with no load, read the `isense:` RTT line
/// (`raw … mV`), or DMM the ISMON node / R49, and set this to that value. The
/// value below was fitted from the 2026-09-27 bench run: with the converter
/// enabled and no load, PA3 read raw 307–308 counts = 247–248 mV on that board,
/// so the previous 244 mV constant showed ~200–250 mA at true zero and carried
/// that offset up through the whole range.
pub const ISENSE_ZERO_MV: u32 = 248;

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

// ---------------------------------------------------------------------------
// CV map calibration & metrology servo
// ---------------------------------------------------------------------------
//
// The open-loop CV map above is *analytic*: it assumes nominal resistors, a
// perfect 3.3 V DAC reference and a typical LT8390A FB reference. On real
// hardware those three stack to ±0.5–1 % at the output (the bench saw +0.65 %),
// because the R19/R36 summing gain multiplies every one of them by 35.7.
//
// Two corrections live here (see `analysis/CV_CALIBRATION_PLAN.md`):
//
// 1. a **fitted affine map** — `VOUT = CV_CAL_A_MV − CV_CAL_B_UV_PER_LSB·code`,
//    produced by the bench sweep + `analysis/cv_cal/fit.py` against the output
//    INA228 (itself cross-checked once against a DMM via the scale below).
//    This removes the systematic resistor/reference/DAC-gain errors.
// 2. a **metrology servo** (`control::supply::CvTrim`) — a ±1 LSB-every-period
//    trim against the output INA228 that removes what the fit cannot: DAC INL
//    and temperature drift. It is frozen outside steady CV operation and its
//    authority is clamped, so it can never fight the CC loop, chase a sweep, or
//    move the output quickly.

/// True once `CV_CAL_A_MV`/`CV_CAL_B_UV_PER_LSB` hold a bench fit for *this
/// board*. While false, the analytic map above is used and the servo starts
/// every settle from that (larger) error.
pub const CV_CAL_VALID: bool = false;
/// Fitted map: output (mV) commanded at CV DAC code 0 (analytic ≈ 72_400).
pub const CV_CAL_A_MV: u32 = 72_400;
/// Fitted map: output change (µV) per +1 CV DAC code (analytic ≈ 28_790; the
/// map is inverted, so a *positive* slope means larger codes give less Vout).
pub const CV_CAL_B_UV_PER_LSB: u32 = 28_790;

/// U12 (output INA228) VBUS scale correction, as a rational `bus_mv·num/den`.
///
/// The INA228's VBUS gain error is only ±0.05 % max (±20 ppm/°C), but the
/// servo's accuracy *is* this factor, so measure it once per board against a
/// trusted DMM at two or three voltages: `k = DMM/U12`, then enter `k` here
/// with `num`/`den` integers (e.g. 1.0003 → 10003/10000). Applies to the
/// output monitor's `bus_mv` (and its derived power) only.
pub const INA228_OUT_VBUS_SCALE_NUM: u32 = 10_000;
pub const INA228_OUT_VBUS_SCALE_DEN: u32 = 10_000;

/// Enable the CV metrology servo. Safe to leave on permanently once the map
/// is calibrated; with the uncalibrated analytic map it still works, it just
/// exercises the trim clamp until a fit exists.
pub const CV_SERVO_ENABLE: bool = true;
/// One servo trim step per period. At least two INA228 conversion cycles
/// (~50 ms each as configured) so every step is judged on settled readings.
pub const CV_SERVO_PERIOD_MS: u64 = 500;
/// Servo stop threshold: no stepping while `|v_set − vout|` is below this.
/// Just above ½ LSB (14.4 mV): one step moves 28.8 mV, so from outside the
/// deadband one step always lands inside it — the servo converges and cannot
/// limit-cycle.
pub const CV_SERVO_DEADBAND_MV: u32 = 15;
/// Total servo authority, in CV DAC codes (64 × 28.8 mV ≈ ±1.84 V). Enough for
/// drift + INL, far below the supervisor's 105 % overvoltage guard, so even a
/// lying INA228 cannot walk the output far. When the trim saturates here the
/// static map needs (re-)calibrating.
pub const CV_SERVO_MAX_TRIM_LSB: i16 = 64;

/// How often the supply supervisor prints its consolidated `supply:` diagnostic.
/// One second gives a slow enough scroll to read while still catching a fault or
/// mode change within a second of the event; edge-triggered events (faults, a
/// binding power cap, a CV↔CC transition) are logged immediately regardless.
pub const SUPPLY_LOG_MS: u64 = 1_000;

/// Supply-snapshot period while the output is out of regulation (below
/// [`VOUT_SAG_PCT`] of the setpoint). A converter saturation/collapse can be
/// short-lived, so the 1 s cadence misses the interesting part; this tightens it
/// and a dedicated edge line records the transition either way.
pub const SUPPLY_LOG_SAG_MS: u64 = 200;

/// An enabled output below this percentage of its setpoint is reported as a CV
/// regulation failure (the LT8390A is not holding FB at 1 V), as opposed to a
/// deliberate current limit.
pub const VOUT_SAG_PCT: u32 = 85;

/// Advisory threshold on the commanded step-down ratio `VIN/VOUT`, in percent.
///
/// The 2026-09-27 bench run showed this board cannot hold regulation above
/// roughly 70 W when run at 48 V in / 12 V out (`VIN/VOUT = 4.0`) — it sags and
/// "chitters" — while the *same* hardware reached its full 10 A programmed CC
/// limit at 28 V in (`VIN/VOUT = 2.3`). That is the high-step-down / low-duty
/// corner (1 MHz from R1 = 147 k, with a 2.2 µH inductor and the LSP/LSN filter
/// as built). A one-shot `supply:` warning is emitted above this ratio so a
/// marginal rail is visible in the log instead of looking like a firmware bug.
/// Prefer the lowest PD rail that is still a clean buck (`control::auto_track`
/// already does this in Auto mode).
pub const DEEP_BUCK_WARN_RATIO_PCT: u32 = 300;

/// The measured output current must exceed the commanded limit by this much
/// before the software over-current fault latches. The ISMON path is only
/// specified to ±3 %, its offset is part-specific (see `ISENSE_ZERO_MV`), and
/// the observed bias on the bench was ~0.6 A — which is >5 % of a 3 A limit.
/// The hardware CC loop is the primary limit; this is a runaway backstop, so a
/// generous margin is correct. 115 % still sits below the 20 A design cap for
/// every limit below ~17 A and the absolute `IOUT_MAX_MA` check covers the top.
pub const IOUT_TRIP_MARGIN_PCT: u32 = 115;

// ---------------------------------------------------------------------------
// Output-current sense-chain plausibility watchdog (rev3 safety net)
// ---------------------------------------------------------------------------
//
// The LT8390A's ISMON pin and the output INA228 (U12) are *independent*
// consumers of the same R96 shunt taps: ISMON goes through the U7 range mux and
// the U14 INA241, U12 taps the shunt directly. On rev3 the mux is also the part
// that dies when the output-return node floats (see
// `analysis/REV3_SCHEMATIC_AUDIT.md`), and when it dies the CC loop goes blind
// with it — the bench then delivered ~45 W into a load while every current
// reading said 0 A, and nothing in the firmware noticed.
//
// These constants make that state a latched fault. The comparison is against the
// *input* power, which is measured on a different shunt (R60/U9) and therefore
// survives the fault being detected: a converter delivering meaningful power
// must show a comparable output current, and a live U12 must agree with ISMON.

/// Minimum input power before the watchdog is allowed to judge the output
/// current. At 30 V out this is ~150 mA of load, well above the converter's own
/// quiescent draw (~1–2 W), so a genuinely light load can never trip it.
///
/// The watchdog needs an independent power measurement, which on this board is
/// the input INA228 (U9) — so it is active on a USB-C session only. On an
/// XT90/DC feed the PD bus (and with it both INA228s) is structurally absent and
/// there is nothing to cross-check against, so the watchdog is inert there.
pub const SENSE_OPEN_MIN_PIN_MW: i32 = 5_000;

/// The measured output power must be at least this percentage of the input
/// power. A healthy converter runs 85–95 %; 30 % leaves room for a poor
/// operating point while still catching a sense chain that reads zero.
pub const SENSE_OPEN_RATIO_PCT: u32 = 30;

/// U12 must report at least this much current before "ISMON reads much less than
/// U12" is treated as evidence that the mux/ISMON path is dead rather than both
/// readings being noise. One INA228 LSB is ~38 µA.
pub const SENSE_OPEN_U12_MIN_MA: u32 = 200;

/// How long the implausible condition must persist before
/// [`Fault::SenseOpen`] latches. Long enough to ride out a load's own
/// soft-start and the INA228/U12 sample latency (~50 ms conversion, 100 ms
/// poll); short enough to stop the stage before it can deliver much energy blind.
pub const SENSE_OPEN_CONFIRM_MS: u64 = 1_500;

// ---------------------------------------------------------------------------
// LT8390A output-current-limit input (CTRL pin) network
// ---------------------------------------------------------------------------
//
// The CC DAC does NOT drive the LT8390A CTRL pin directly. On the board,
//
//     PA6 / CC_Set ── R48 ──┬── CTRL (U1 pin 10)
//                           │
//     LT8390A VREF ── R5 ───┤
//                           │
//                          R4
//                           │
//                          GND
//
// so CTRL is a resistor-summing node:
//
//     V_CTRL = k · V_DAC + V_ctrl0
//     k       = (R4 || R5) / (R48 + (R4 || R5))
//     V_ctrl0 = VREF · R4 / (R4 + R5)
//
// `control::dac_cc` inverts this to turn a requested current limit into a DAC
// code (and reports the limit a code actually produces), and `control::supply`
// logs both so a divider mismatch is visible instead of silent.
//
// NOTE (rev2 fitted values): R48 = 10 k, R5 = 357 k, R4 = 75 k. R5 was raised
// from 124 k to 357 k as the "DAC high-Z / MCU held in reset" fail-safe: with
// the DAC floated, V_CTRL = VREF·R4/(R4+R5) ≈ 0.347 V, i.e. ≈ 4.9 A, instead of
// ≈ 25 A with 124 k. **If the next board revision changes this divider, update
// these three constants or every CC limit will be wrong.**
pub const LT8390_VREF_MV: u32 = 2_000; // LT8390A VREF pin (NOT the 1.0 V FB ref)
pub const CTRL_SERIES_OHM: u32 = 10_000; // R48, CC_Set → CTRL
pub const CTRL_VREF_OHM: u32 = 357_000; // R5,  VREF    → CTRL
pub const CTRL_GND_OHM: u32 = 75_000; // R4,  CTRL    → GND

/// LT8390A CTRL→current-limit transfer, from the datasheet:
///
/// ```text
///   V(ISP−ISN)_threshold = (V_CTRL − 0.25 V) · 0.1     (0.3 V ≤ V_CTRL ≤ 1.15 V)
///   I_LIMIT              = V(ISP−ISN)_threshold / R_sense
/// ```
///
/// so with the 2 mΩ shunt at ×1, `I_LIMIT(mA) = (V_CTRL(mV) − 250) · 50`. The
/// transfer saturates at `V_CTRL ≥ 1.35 V` (100 mV ⇒ 50 A) and the part latches
/// switching off below [`LT8390_CTRL_LATCHOFF_MV`] — which is why the disabled
/// path's CC code of 0 is a genuine shutdown, not just a low limit.
///
/// `R_sense` is **range dependent** on rev3: it is 2 mΩ at ×1 and 20 mΩ at ×10
/// (see [`CcRange`]), so every conversion here takes a [`CcRange`].
pub const LT8390_CTRL_OFFSET_MV: u32 = 250;
/// Sense-threshold volts per volt of CTRL, scaled ×1000 (0.1 V/V).
pub const LT8390_CTRL_SLOPE_MILLI: u32 = 100;
/// Below this CTRL voltage the LT8390A stops switching (latch-off, 285–315 mV).
pub const LT8390_CTRL_LATCHOFF_MV: u32 = 300;

// ---------------------------------------------------------------------------
// Achievable CC-limit range (the part's CTRL window, not the firmware's choice)
// ---------------------------------------------------------------------------
//
// The datasheet's CTRL → ISP/ISN threshold transfer is three regions:
//
//   0.30 V ≤ V_CTRL ≤ 1.15 V   linear,  threshold 5 mV → 90 mV
//   1.15 V ≤ V_CTRL ≤ 1.35 V   smooth transition 90 mV → 100 mV
//           V_CTRL ≥ 1.35 V   constant 100 mV full scale
//           V_CTRL < 0.30 V   stops switching (latch-off)
//
// so the part can command a **20:1** current window (5 → 100 mV) per shunt. No
// single shunt value can cover 0.1 A → 20 A; that is 200:1. rev3 therefore makes
// the sense shunt switchable at runtime through the ×10 INA241 front end.

/// Sense-threshold window endpoints, in mV across the effective shunt.
pub const LT8390_CTRL_MIN_SENSE_MV: u32 = 5;
pub const LT8390_CTRL_LINEAR_MAX_SENSE_MV: u32 = 90;
pub const LT8390_CTRL_MAX_SENSE_MV: u32 = 100;
/// CTRL voltages where the window is linear-top / saturated, in mV.
pub const LT8390_CTRL_LINEAR_TOP_MV: u32 = 1_150;
pub const LT8390_CTRL_SAT_MV: u32 = 1_350;

/// CTRL voltage needed to *leave* latch-off: the falling-threshold worst case
/// (315 mV) plus the 25 mV hysteresis.
///
/// This is the trap at the low end of the CC range. The part can *sustain* a
/// limit down at [`LT8390_CTRL_LATCHOFF_MV`] **once it is switching**, but from a
/// cold start it will not begin switching until CTRL exceeds ~0.34 V. On rev3
/// this is also the condition the output sequencer has to satisfy before
/// `CNV-EN-LOADEN` can take effect, because the LT8390A's `LOADON` signal needs
/// CTRL above its rising latch-off as well as `LOADEN` high.
pub const LT8390_CTRL_LATCHOFF_MAX_MV: u32 = 315;
pub const LT8390_CTRL_LATCHOFF_HYST_MV: u32 = 25;
pub const LT8390_CTRL_START_MV: u32 =
    LT8390_CTRL_LATCHOFF_MAX_MV + LT8390_CTRL_LATCHOFF_HYST_MV;

/// The CC sense range, i.e. which shunt the LT8390A's `ISP/ISN` pair sees.
///
/// rev3 wires a TMUX1574 (U7) between the output shunt R96 and the LT8390A:
/// `SEL` low selects the shunt directly, `SEL` high routes it through the INA241
/// gain-10 amplifier. `SEL` is driven by the `High-Gain` pin (PB6), so this enum
/// is the firmware's name for that pin's state. The mux switches the `VC`
/// compensation network on its third channel at the same time.
///
/// `R_eff` is what the chip *sees*, which is what every CC conversion must use:
///
/// | | [`CcRange::LowGain`] | [`CcRange::HighGain`] |
/// |---|---|---|
/// | `High-Gain` pin | low | high |
/// | `R_eff` | 2 mΩ (R96) | 20 mΩ (R96 × 10) |
/// | ISMON gain | 20 mV/A | 200 mV/A |
/// | CC window | 2.5 A … 50 A | 0.25 A … 5 A |
/// | cold-start floor | 4.5 A | 450 mA |
///
/// The two windows overlap over 2.5–5 A, which is where [`HIGH_GAIN_MAX_SET_MA`]
/// puts the switch point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, defmt::Format)]
pub enum CcRange {
    /// `High-Gain` low: the chip senses R96 directly.
    #[default]
    LowGain,
    /// `High-Gain` high: the chip senses R96 through the INA241 ×10 amplifier.
    HighGain,
}

impl CcRange {
    /// Effective shunt the LT8390A's `ISP/ISN` pair sees, in milliohms.
    pub const fn shunt_mohm(self) -> u32 {
        match self {
            CcRange::LowGain => 2,
            CcRange::HighGain => 20,
        }
    }

    /// ISMON gain at the PA3 node, in mV per amp.
    ///
    /// The LT8390A gives `V_ISMON = 10 · V(ISP−ISN) + 0.25 V`, and
    /// `V(ISP−ISN) = R_eff · I`, so the gain is `10 · R_eff` — 20 mV/A at ×1 and
    /// **200 mV/A** at ×10. The 0.25 V offset is inside the chip, *after* the
    /// differential, so it does not scale with the range (see
    /// [`ISENSE_ZERO_MV`]).
    pub const fn mv_per_a(self) -> u32 {
        10 * self.shunt_mohm()
    }

    /// Lowest current limit the part can **sustain** (`5 mV / R_eff`).
    ///
    /// This sits exactly on the latch-off, so it is only reachable once the
    /// stage is already switching — see [`Self::set_min_ma`] and
    /// [`Self::start_min_ma`].
    pub const fn min_ma(self) -> u32 {
        LT8390_CTRL_MIN_SENSE_MV * 1_000 / self.shunt_mohm()
    }

    /// Highest current limit the part can command (`100 mV / R_eff`).
    pub const fn max_ma(self) -> u32 {
        LT8390_CTRL_MAX_SENSE_MV * 1_000 / self.shunt_mohm()
    }

    /// Datasheet-guaranteed cold-start floor: the current whose sense threshold
    /// puts CTRL at [`LT8390_CTRL_START_MV`]. 4.5 A at ×1, 450 mA at ×10.
    ///
    /// The output sequencer floors the CC command here for the whole ramp, and
    /// the LT8390A will not leave its `UP/PRE` state below it.
    pub const fn start_min_ma(self) -> u32 {
        (LT8390_CTRL_START_MV - LT8390_CTRL_OFFSET_MV) * 100 / self.shunt_mohm()
    }

    /// Bench-measured low-end loop offset for this range, in mA: near the bottom
    /// of the window the loop delivers this much *more* current than commanded.
    ///
    /// **Both are 0 pending a rev3 measurement.** The rev2 value (3650 mA) was
    /// measured on the old high-side R18 sense node and does **not** transfer to
    /// rev3's low-side, mux-routed node. Re-measure per range by setting two low
    /// setpoints and taking the intercept (`actual − setpoint`); see
    /// `analysis/CC_RANGE_1_TO_20A.md`.
    pub const fn offset_ma(self) -> u32 {
        match self {
            CcRange::LowGain => CC_CURRENT_OFFSET_MA_1X,
            CcRange::HighGain => CC_CURRENT_OFFSET_MA_10X,
        }
    }

    /// Lowest **settable** current: the loop offset plus a 0.1 A command, floored
    /// above [`Self::min_ma`] so a request can never round to a DAC code below
    /// the CTRL latch-off (which would stop the stage instead of limiting it).
    pub const fn set_min_ma(self) -> u32 {
        let v = self.offset_ma() + 100;
        let lo = self.min_ma() + 100;
        if v > lo {
            v
        } else {
            lo
        }
    }

    /// Highest **settable** current: the design maximum, capped by what this
    /// range can actually command.
    pub const fn set_max_ma(self) -> u32 {
        let m = self.max_ma();
        if IOUT_MAX_MA < m {
            IOUT_MAX_MA
        } else {
            m
        }
    }

    /// Which range a given user setpoint wants.
    ///
    /// [`FORCE_CC_RANGE`] overrides the setpoint decision when a board has to be
    /// pinned to one path (see that constant's docs).
    pub const fn for_setpoint(i_set_ma: u32) -> Self {
        match FORCE_CC_RANGE {
            Some(range) => range,
            None => {
                if i_set_ma <= HIGH_GAIN_MAX_SET_MA {
                    CcRange::HighGain
                } else {
                    CcRange::LowGain
                }
            }
        }
    }
}

/// rev3 per-range CC loop offsets, in mA. **Re-measure both on the rev3 bench**;
/// the rev2 figure was 3650 mA on a different sense node.
pub const CC_CURRENT_OFFSET_MA_1X: u32 = 0;
pub const CC_CURRENT_OFFSET_MA_10X: u32 = 0;

/// CC setpoint at or below which [`CcRange::HighGain`] is selected.
///
/// 5 A is the top of the ×10 window and sits squarely inside the 2.5–5 A overlap
/// with ×1, so every setpoint up to 5 A gets the extra low-end resolution while
/// the range stays legal on both sides of the switch. Above it, ×1 covers the
/// rest of the design range.
///
/// The gain pin is only moved while the stage is parked, because a range change
/// is a ×10 step in the CC loop gain; see `control::supply`. A setpoint that
/// wants the other range while running is deferred and logged.
pub const HIGH_GAIN_MAX_SET_MA: u32 = 5_000;

/// Pin the CC sense range, ignoring what the setpoint would otherwise pick.
///
/// **Set this to `Some(CcRange::HighGain)` on a rev3 bench board whose range
/// mux (U7) is suspect.** The ×1 (`LowGain`) path is the one that connects the
/// mux's `S1A`/`S2A` straight to the output-return shunt taps — the node that
/// floats to `VOUT` when `Q5` is open and a load is attached, and the one that
/// destroyed U7. The ×10 (`HighGain`) path reaches the chip through the INA241,
/// whose inputs are rated −5…110 V, so it survives the float. Pinning the range
/// keeps the chip on that path even if the setpoint later wants ×1.
///
/// **This is only half a fix:** it does not stop the 30 V reaching the mux's
/// `S1A`/`S2A` pins, because they are tied to the taps whether or not the switch
/// selects them. It is the pair to the "lift U7 pins 16 and 3" bodge: with those
/// two pins lifted, `HighGain` is the only path left and this constant makes it
/// the only path the firmware can ask for. With ×10 pinned the settable span is
/// [`CcRange::HighGain::set_max_ma`] (5 A), which is the right ceiling for a
/// 30 V bench run.
///
/// The default `None` preserves the normal setpoint-driven range selection.
pub const FORCE_CC_RANGE: Option<CcRange> = None;

/// Settable CC span across **both** ranges, used by the UI so the I-LIM knob can
/// always be moved anywhere in the design range.
///
/// The per-range clamp is applied where the DAC is actually programmed
/// (`CcDac::clamp_current`), because while the output is running the active range
/// can deliberately lag what the setpoint wants — clamping the *setpoint* to the
/// active range there would trap the knob inside the current range with no way
/// out.
pub const CC_SET_MIN_MA: u32 = CcRange::HighGain.set_min_ma(); // 350 mA
/// Upper end of the UI's settable span. Normally 20 A (the ×1 window); when
/// [`FORCE_CC_RANGE`] pins the board to one range it follows that range instead,
/// so the knob cannot be walked to a setpoint the CC DAC will silently clamp.
pub const CC_SET_MAX_MA: u32 = match FORCE_CC_RANGE {
    Some(range) => range.set_max_ma(),
    None => CcRange::LowGain.set_max_ma(),
}; // 20 A, or 5 A with FORCE_CC_RANGE = Some(HighGain)

/// I2C addresses
pub const TPS26750_ADDR: u8 = 0x21;
pub const SSD1306_ADDR: u8 = 0x3C;
pub const CAT24C512_ADDR: u8 = 0x50;
/// rev3 input power monitor, U9 on the PD bus (`A0 = A1 = GND`).
pub const INA228_IN_ADDR: u8 = 0x40;
/// rev3 output power monitor, U12 on the PD bus (`A1 = +3V3 (VS)`, `A0 = GND`).
///
/// The INA228 address is *not* binary-weighted on the strap pins: with the
/// real pin map (pin 1 = `A1`, pin 2 = `A0` — verified against the TI
/// datasheet's DGS-10 pin configuration and this project's symbol) the strap
/// `A1 = VS, A0 = GND` lands on row 4 of the datasheet's address table,
/// **0x44**. The tempting `0x40 | A1<<1 | A0 = 0x42` (and the earlier guess of
/// `0x41`, which would require `A1 = GND, A0 = VS`) are both wrong for this
/// strap. Sanity-check on the bench with an I2C scan before trusting any
/// address-dependent behaviour: U9 must ACK at 0x40 and U12 at 0x44.
///
/// U12 senses the output-return shunt R96, so its shunt current is the **load**
/// current, and its VBUS pin sits on `Power-Sense-Out/Bus_V` (the connector side
/// of the output fuse F2). Its `ALERT` pin is unconnected, so the MCU must poll.
pub const INA228_OUT_ADDR: u8 = 0x44;

/// INA228 shunt values, one per monitor.
///
/// * **Input (U9), R60 = 2 mΩ.** rev2 fitted 8 mΩ here (20 A × 8 mΩ = 160 mV,
///   right at the ±163.84 mV `ADCRANGE = 0` limit); rev3 dropped it to 2 mΩ, so
///   a full-scale 20 A now develops only 40 mV.
/// * **Output (U12), R96 = 2 mΩ**, in the load-return path on the connector side
///   of the output switch Q5. This is the same shunt the LT8390A `ISP/ISN` pair
///   and the ×10 INA241 front end sense (through the U7 mux), so it is the one
///   node the CC loop, the output monitor and the sequencer's switch-closed
///   check all agree on.
pub const INA228_IN_SHUNT_MOHM: u32 = 2;
pub const INA228_OUT_SHUNT_MOHM: u32 = 2;

/// Sanity bound on a decoded shunt current. `INA228_MAX_CURRENT_MA` is also the
/// `CURRENT_LSB` denominator below, so both monitors share one LSB.
pub const INA228_MAX_CURRENT_MA: u32 = IOUT_MAX_MA;
/// CONFIG: ADCRANGE = 0 (±163.84 mV), shunt temperature compensation off.
pub const INA228_CONFIG: u16 = 0x0000;
/// ADC_CONFIG: MODE = 0xF (continuous bus + shunt + temperature), 1052 µs per
/// conversion, AVG = 16 → ~50 ms per full update. Sample it every 100 ms.
pub const INA228_ADC_CONFIG: u16 = 0xFB6A;
/// `SHUNT_CAL = 13107.2e6 × CURRENT_LSB × R_shunt` with
/// `CURRENT_LSB = 20 A / 2^19 = 38.147 µA`. At 2 mΩ that is 1000 for both
/// monitors; rev2's 8 mΩ input shunt needed 4000.
pub const INA228_SHUNT_CAL_PER_MOHM: u32 = 500; // 1000 per 2 mΩ
/// `SHUNT_CAL` for a shunt of `mohm` milliohms, clamped to the 16-bit register.
pub const fn ina228_shunt_cal(mohm: u32) -> u16 {
    let v = INA228_SHUNT_CAL_PER_MOHM * mohm;
    if v > u16::MAX as u32 {
        u16::MAX
    } else {
        v as u16
    }
}
pub const INA228_IN_SHUNT_CAL: u16 = ina228_shunt_cal(INA228_IN_SHUNT_MOHM);
pub const INA228_OUT_SHUNT_CAL: u16 = ina228_shunt_cal(INA228_OUT_SHUNT_MOHM);

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
/// Before EPR mode has been entered, an above-SPR rail is requested as an **EPR
/// AVS** contract inside this window: asserting `EPR AVS Enable Sink Mode`
/// (0x37 bit 128) is what makes the controller attempt EPR mode entry (TRM
/// Table 4-21), and SDAA265 §5.3 makes a bare fixed >20 V window fall back to
/// 5 V. Once EPR is entered and the source advertises its own EPR PDOs, a
/// specific above-SPR preset is requested as a **fixed** EPR PDO through
/// `Tps26750::request_fixed_epr_rail` instead (see `PD_ROOT_CAUSE.md`).
/// Keep in step with `0x33`/`0x37` of the config image.
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

// ---------------------------------------------------------------------------
// rev3 output-stage power-up sequence
// ---------------------------------------------------------------------------
//
// rev3 gives the MCU three power-path pins instead of one:
//
//   PB1  Conv-Disable    → Q13 → LT8390A EN/UVLO          (enables the chip)
//   PB3  CNV-EN-LOADEN   → LT8390A LOADEN                 (isolates, then starts)
//   PB2  PWR_G           → Q12 → Q7 → R58 → Q5 gate       (connects the load)
//
// The LT8390A `LOADEN` input is the mechanism the sequence is built on. Per the
// datasheet (Rev. C): *"Forcing the pin low turns off TG1 and TG2, **turns on
// BG1 and BG2**, disconnects the VC pin from all internal loads, and turns off
// LOADTG."* `TG2` is the switch between `SW2` and `VOUT`, so `LOADEN` low
// **isolates the output node from the bridge**: `BG1`/`BG2` clamp the *inductor*
// side to ground (a defined, zero-current state for the bridge and a 0 V *start*
// for a discharged output), but any charge already on the output capacitors stays
// there and only bleeds through the divider network over seconds. That is why
// `Stage::Precharge` also gates on [`VOUT_START_MAX_PCT`] before it lets the
// ramp start. Raising `LOADEN` reconnects `VOUT` to the bridge and releases it
// into soft-start, and the output ramps.
//
// `LOADEN` does not take effect immediately: the start-up sequence reaches its
// `UP/PRE` state only once **CTRL is above its rising latch-off threshold
// (≈0.325 V)** *and* `LOADEN` is high. That is why the sequencer forces the CC
// DAC up to the cold-start floor for the whole ramp instead of applying the user
// setpoint straight away — a sub-floor setpoint would otherwise command CTRL
// below latch-off, `LOADEN` would be raised, and nothing would ever start.

/// Delay after releasing `Conv-Disable` (chip enabled) before raising
/// `CNV-EN-LOADEN`. Covers INTVCC/VREF settling and the CTRL node reaching the
/// rising latch-off so `LOADON` asserts on the first try, while the output is
/// still isolated behind `TG2` (`LOADEN` low).
pub const OUTPUT_PRELOADEN_MS: u64 = 20;

/// Maximum level the output node may sit at (as a percentage of the setpoint)
/// before `CNV-EN-LOADEN` is allowed to rise.
///
/// The same 105 % margin the supply supervisor uses for its overvoltage guard.
/// `LOADEN` low *isolates* the output node (`TG2` off), so after a high-voltage
/// session the node keeps its charge and only bleeds through its dividers over
/// seconds — and the converter cannot sink, so firmware cannot pull it down.
/// Raising `LOADEN`/closing `PWR_G` on top of that charge would let the ramp
/// check pass on the *stale* voltage and connect the load into an overvoltage;
/// `Stage::Precharge` therefore waits it out (see
/// [`OUTPUT_BLEED_TIMEOUT_MS`]).
pub const VOUT_START_MAX_PCT: u32 = 105;

/// How long [`crate::control::sequencer::Stage::Precharge`] holds `LOADEN` low
/// waiting for the output node to bleed below [`VOUT_START_MAX_PCT`] of the
/// setpoint before latching [`Fault::OutputAboveSetpoint`].
///
/// Sized for a real bleed-down, not for a stuck node: the bleeders
/// (`R19`/`R20` 367 kΩ, `R85`–`R87` 258 kΩ, `D20`+`R75` ~75 kΩ) drain the
/// output caps at roughly 0.5–1.5 V/s, so even a 60 V → 3 V recovery fits
/// inside 30 s. A node that never falls in that window is being held up from
/// outside (a battery or source on the output) — the fault message says so.
pub const OUTPUT_BLEED_TIMEOUT_MS: u64 = 30_000;

/// Window allowed for `Vsense_REG_OUT` to reach [`VOUT_REACHED_PCT`] of the
/// setpoint after `CNV-EN-LOADEN` goes high.
///
/// How long the *converter* gets to bring the output to the setpoint, measured
/// from the moment the CV code has finished slewing — not from stage entry.
///
/// That anchoring matters: the DAC walks at most [`CV_SLEW_MAX_LSB_PER_TICK`]
/// counts per 1 ms tick, so leaving the parked code (4095) for a 48 V setpoint is
/// ≈3250 counts ≈ 812 ms during which the converter has not even been *asked* for
/// the new voltage. Charging that walk against the ramp budget used to leave only
/// ~400 ms of real ramp and produced false `OUTPUT FAIL` faults on large setpoint
/// steps. Measured on rev2, a full cold start to the setpoint took ≈2 s, so the
/// budget is 2 s of real ramp on top of the slew.
pub const OUTPUT_RAMP_TIMEOUT_MS: u64 = 2_000;

/// How long the output must sit at the commanded current limit before
/// [`crate::control::sequencer::Stage::Ramping`] accepts it as a *reached*
/// level instead of waiting for the setpoint.
///
/// With the load switch closed before the ramp (see
/// [`OUTPUT_SWITCH_EARLY_CLOSE`]) a load that draws the CC limit holds the output
/// below the setpoint on purpose — that is CC operation, not a failed ramp. The
/// delay rides out the inrush pulse into the load's input capacitance, which also
/// hits the limit but only for a few milliseconds.
pub const RAMP_CC_SETTLE_MS: u64 = 300;

/// Minimum output level, as a percentage of the setpoint, for a CC-limited ramp
/// to count as a valid operating point.
///
/// A load that draws the commanded limit is a legitimate CC state at whatever
/// voltage the load needs, but a *dead short* also pins the current at the limit
/// — at 0 % of the setpoint. 5 % (1.5 V at 30 V) keeps a short on the
/// `OUTPUT FAIL` path while still accepting a heavy load in CC.
pub const OUTPUT_RAMP_CC_MIN_PCT: u32 = 5;

/// Escape hatch for [`OUTPUT_RAMP_TIMEOUT_MS`]'s anchoring: if the CV code never
/// finishes slewing, the ramp clock never starts and the sequence would sit in
/// `Ramping` forever. The walk is bounded (≤ 4095 counts ÷
/// [`CV_SLEW_MAX_LSB_PER_TICK`] per ms ≈ 1024 ms), so anything beyond this is a
/// stuck DAC/map, reported as `OUTPUT FAIL`.
pub const OUTPUT_SLEW_GUARD_MS: u64 = 1_500;

/// Settle time after asserting `PWR_G` before the output INA228 is trusted.
/// One full INA228 conversion cycle is ~50 ms (`INA228_ADC_CONFIG`); 250 ms
/// guarantees at least one *post-switch* sample has been published even if the
/// poll landed the instant `PWR_G` moved.
///
/// This is only the *first* look: the check is then repeated every supply tick
/// until it passes or [`OUTPUT_SWITCH_VERIFY_MS`] expires, so a single unlucky
/// sample can no longer fail an enable that is actually fine.
pub const OUTPUT_SWITCH_SETTLE_MS: u64 = 250;

/// How long [`crate::control::sequencer::Stage::SwitchOn`] keeps re-checking the
/// load switch after the [`OUTPUT_SWITCH_SETTLE_MS`] settle, before latching
/// `Fault::OutputSwitchFailed`.
///
/// The check used to be a single snapshot at 250 ms, which was too strict on a
/// USB-C session — the one input mode where the output INA228 (U12) exists at
/// all, and therefore the only mode that applies the current evidence. Four
/// effects can make that one sample read "not closed" while `Q5` closes fine:
///
/// * **Sample latency.** U12 is polled from the PD loop at
///   [`INA228_POLL_MS`] (100 ms) and each conversion averages 16 samples
///   (~50 ms), so the first sample *taken after* `PWR_G` moves can be read up
///   to ~150 ms late; if a bus transaction is slow the loop backs off to 2 s,
///   and the stale sample then reports the pre-switch 0 A.
/// * **Load inrush.** Closing `Q5` into a load's input capacitance sags
///   `Vsense_REG_OUT`; a snapshot taken inside that sag looks like a
///   "collapsed output" even though it recovers in a few ms.
/// * **Load start-up.** A load with its own soft-start (a charger, a PD sink, a
///   DC/DC) draws less than [`OUTPUT_SWITCH_MIN_MA`] for tens of ms after the
///   switch closes.
/// * **Current noise at the 50 mA threshold.** A light load can cross the
///   threshold between two consecutive samples.
///
/// Retrying is strictly better than simply waiting longer before the single
/// check: a healthy close still reaches `Running` at 250 ms, and only a close
/// that is *never* confirmed pays the full window. 2 s covers the backed-off
/// PD poll plus a conversion; raise it if a genuinely slow load needs longer.
///
/// The retry is safe for the one failure that could look alarming: a genuinely
/// collapsed output is *below* the setpoint, never above it, and the over-
/// current, over-power and over-temperature supervision is already live in
/// [`crate::control::sequencer::Stage::SwitchOn`] — so the window only delays the
/// `SWITCH FAIL` report, it does not extend an unsafe condition. It never
/// re-closes `PWR_G` either: the switch is already asserted for the whole stage.
pub const OUTPUT_SWITCH_VERIFY_MS: u64 = 2_000;

/// Minimum spacing of the `switch not confirmed yet` lines while
/// [`OUTPUT_SWITCH_VERIFY_MS`] is still running.
///
/// The retry loop runs at the 1 ms supply tick and would otherwise flood RTT;
/// a cause change always logs immediately, so this only sets the cadence of the
/// "still waiting" repeats — enough to watch `iout` climb toward
/// [`OUTPUT_SWITCH_MIN_MA`] on a scope-free bench.
pub const OUTPUT_SWITCH_RETRY_LOG_MS: u64 = 500;

/// `Vsense_REG_OUT` must reach this percentage of the setpoint for the output to
/// count as "conversion started".
pub const VOUT_REACHED_PCT: u32 = 90;

/// Minimum `|I_out|` from the output INA228 that counts as "the output switch is
/// carrying a load". One INA228 LSB is ~38 µA, so this is well above noise.
pub const OUTPUT_SWITCH_MIN_MA: u32 = 50;

/// Require the output INA228 to report load current before declaring the switch
/// closed. **Default `false`** — see below before turning it on.
///
/// The output voltage `Vsense_REG_OUT` is sensed on the **converter** side of
/// `Q5`, so a `Q5` that never conducts leaves the output looking perfectly
/// regulated: current through R96 is the only *direct* evidence that the load is
/// connected. But `Q5` sits in the **return** path, so with nothing attached there
/// is no current to see *however well the switch closes* — the check is really a
/// "a load is present and drawing" test, not a "the switch closed" test. Requiring
/// it fails every no-load enable with `SWITCH FAIL`, which is what a rev3
/// bring-up run hit twice (30.5 V regulated, `0 mA` over the whole window, 423 mW
/// in at the input). It therefore defaults to `false`: a close is then confirmed
/// by the regulated output voltage plus a live output INA228, and the current
/// evidence is still **logged** (`peak iout … mA`) without being able to fail the
/// enable.
///
/// Set this **true** on a board that must prove closure with a real load attached
/// (the [`OUTPUT_SWITCH_VERIFY_MS`] retry window then gives that load time to
/// start drawing). Below [`OUTPUT_SWITCH_MIN_MA`] it cannot be satisfied, and a
/// genuine `Q5` that never conducts with a load attached will be caught by the
/// over-current/over-power supervision and by the 0 A reading on screen rather
/// than by this check.
pub const OUTPUT_SWITCH_REQUIRE_LOAD: bool = false;

/// Require the output INA228 to be answering at the end of the switch-on window.
/// Set false on a board with U12 unpopulated.
///
/// This is automatically inapplicable when the whole PD bus is absent — an
/// XT90/DC feed has no USB-C VBUS, so the TPS26750's LDO is off and U12 cannot
/// exist. The sequencer then confirms the load switch from the output voltage
/// alone and logs that current confirmation is unavailable. On a *live* PD bus
/// a missing U12 is still treated as a fault.
pub const OUTPUT_SWITCH_REQUIRE_INA: bool = true;

// ---------------------------------------------------------------------------
// rev3 load-switch / output-node interlock (protects the U7 range mux)
// ---------------------------------------------------------------------------
//
// `Q5` is in the output **return**, so opening it does not disconnect the load
// from `VOUT` — it only interrupts the return current. With a load attached the
// return node (`J3.1` → `R96` → `PWR_G`) is then pulled up to `VOUT` through the
// load, and every low-voltage part on that node sees the full output voltage.
//
// On rev3 the only such part is the U7 TMUX1574 (`S1A`/`S2A` → `R83`/`R98`). Its
// signal pins are rated **6 V absolute maximum** (5.5 V recommended), and its ESD
// clamp turns the floating return into a current source limited only by the 27 Ω
// taps: at 30 V that is ~1 A into a switch rated for ±25 mA. That is what killed
// two muxes, and with the mux goes the CC loop and every current reading.
//
// The firmware cannot measure the return node and cannot tell whether a load is
// attached, so it enforces the only rule it can: **`PWR_G` is closed whenever the
// output is above [`OUTPUT_SWITCH_SAFE_VOUT_MV`]**, on the way up and on the way
// down. The load is therefore connected *before* the ramp and released only after
// the output has fallen, which keeps the return node at ground throughout.

/// Output level above which `PWR_G` must not be open, in mV.
///
/// 5 V sits below the TMUX1574's 6 V absolute maximum with ~20 % margin, and
/// above the parked/precharge level, so a normal re-enable never trips it.
pub const OUTPUT_SWITCH_SAFE_VOUT_MV: u32 = 5_000;

/// Close `PWR_G` in [`crate::control::sequencer::Stage::Precharge`], before
/// `LOADEN` is raised, instead of waiting for the output to reach the setpoint.
///
/// This is the enable-side half of the interlock: the ramp then happens with the
/// load connected and the return node at ground, so `S1A`/`S2A` never see more
/// than the shunt drop. The cost is that the load is present during the ramp:
/// inrush is handled by the CC limit/soft-start, and a load that draws the full
/// limit settles into CC (accepted via [`RAMP_CC_SETTLE_MS`]) instead of tripping
/// the ramp budget.
///
/// **Do not leave this on with an active source (a battery) on the output.** The
/// load would be connected to a 0 V output and would back-feed through `Q5` at
/// whatever current the source can supply. With the constant `false` the
/// original ramp-then-close order is restored exactly.
pub const OUTPUT_SWITCH_EARLY_CLOSE: bool = true;

/// Longest the load switch is held closed during a disable, waiting for the
/// output to fall below [`OUTPUT_SWITCH_SAFE_VOUT_MV`].
///
/// A loaded output discharges in milliseconds; an unloaded one only bleeds
/// through its dividers at ~1 V/s, so the timeout is what stops an unloaded
/// disable from hanging. Reaching the timeout with the output still high is safe
/// *only* because it implies nothing is loading it — the sequencer logs that
/// assumption when it releases the switch.
pub const OUTPUT_SWITCH_HOLD_MAX_MS: u64 = 3_000;

/// Load current below which opening the switch is considered safe even with the
/// output still up.
///
/// The clamp current the mux would carry is approximately the load current at
/// `VOUT − 4 V`, so a load drawing less than the switch's ±25 mA rating cannot
/// damage it. This is an escape hatch for the unloaded/light-load disable: it
/// releases `PWR_G` immediately instead of waiting out the bleed.
pub const OUTPUT_SWITCH_SAFE_OPEN_MA: u32 = 20;

/// While disabling: how long the bridge keeps regulating (`LOADEN` high) before
/// `CNV-EN-LOADEN` is pulled low and the output node is isolated from the bridge.
///
/// With [`OUTPUT_SWITCH_EARLY_CLOSE`] the load switch stays **closed** through
/// the disable — opening it while the output is still up is what destroys the U7
/// mux — so the load is powered for this window and then discharges the output
/// node once `LOADEN` drops.
///
/// Both disable delays are deliberately **slow (bench-observable)**: the whole
/// power-down then takes ~2 s, so the three power-path pins and the output can be
/// watched on a scope at a comfortable time base. This is a settle window, not a
/// discharge: `LOADEN` low isolates the output node, which then only bleeds
/// through its dividers at ~1 V/s when nothing is attached (the re-enable path
/// waits for that via `VOUT_START_MAX_PCT`). Shorten both to ~50 ms / ~20 ms for
/// production behaviour.
pub const OUTPUT_DISCHARGE_MS: u64 = 1_000;

/// While disabling: how long the bridge stays parked (output isolated) before
/// `Conv-Disable` is asserted and the LT8390A is shut down.
pub const OUTPUT_CONV_OFF_MS: u64 = 1_000;

/// UI timing
pub const DEBOUNCE_MS: u64 = 25;
/// Max gap between two encoder-button presses that counts as a double click.
/// The single-click action (VSET mode on the Main screen) is deferred by this
/// long so a double click can be recognised instead; it comfortably exceeds
/// `DEBOUNCE_MS` while staying short enough that entering VSET mode still feels
/// immediate.
pub const ENC_DOUBLE_CLICK_MS: u64 = 350;
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

/// Read `PB1`/`PB2`/`PB3`/`PB6` back from the GPIO **input** register — the actual
/// pin level, not the output latch — so a shorted or unsoldered gate-drive line
/// shows up as a number in the log instead of a mystery.
///
/// Returns `(conv_disable PB1, pwr_g PB2, load_enable PB3, high_gain PB6)`, all
/// active-high as named in the pin map. `IDR` keeps reporting the pad while the
/// pin is configured as an output, so this works during a sequence.
pub fn power_path_pin_levels() -> (bool, bool, bool, bool) {
    use embassy_stm32::pac::gpio::vals::Idr;
    let idr = embassy_stm32::pac::GPIOB.idr().read();
    let hi = |n: usize| idr.idr(n) == Idr::HIGH;
    (hi(1), hi(2), hi(3), hi(6))
}

// ---------------------------------------------------------------------------
// Button 1 (`BUTT_1` → PB9, or PA2 after a rework)
// ---------------------------------------------------------------------------

/// Read `BUTT_1` as an analog level instead of a GPIO. **Requires a rework** —
/// leave it `false` on an unmodified board.
///
/// `BUTT_1` is `PB9` (U2 pin 47), and **PB9 has no analog input** on the
/// STM32G474: its alternate functions are `FDCAN1_TX`, `I2C1_SDA`, `TIM4_CH4`,
/// `TIM17_CH1`, `TIM8_CH3`, `TIM1_CH3N`, `USART3_TX`, `SAI1_D2/FS_A` and
/// `COMP2_OUT` — no `ADCx_INy` and no comparator input. A GPIO threshold cannot be
/// moved either (`V_IL` ≈ 0.3 · VDD ≈ 1 V, `V_IH` ≈ 0.7 · VDD ≈ 2.3 V), so a
/// `BUTT_1` line that idles *below* `V_IL` instead of at the rail reads as
/// **permanently pressed** and the button is dead in firmware — firing once at
/// boot and never again. A rev3 board measured ~0.8 V there with the button
/// released, which against the internal ~40 kΩ pull-up means roughly a **13 kΩ
/// path to ground** on the pin (flux, a bridge to a neighbouring pad, or a damaged
/// ESD structure). That is a board fault, not a firmware one: check the PB9 pad
/// against pins 46/48, `R66`, and the switch's ground return.
///
/// Two fixes work:
///
/// 1. **Repair the pin** so `BUTT_1` idles at 3.3 V again. Keep this `false`; the
///    button is then a plain GPIO and behaves exactly as it did before rev3.
/// 2. **Move the read to an ADC pin**: cut the `R66`-to-`PB9` connection and wire
///    the MCU side of `R66` to **PA2** (spare, `ADC1_IN3`), then set this to `true`.
///    The firmware then thresholds the level exactly like the encoder button,
///    with [`BTN1_PRESSED_MV`] / [`BTN1_RELEASED_MV`], and logs every movement of
///    the line.
pub const BTN1_ANALOG: bool = false;

/// `BUTT_1` level (mV) at or below which the button counts as pressed. Used only
/// when [`BTN1_ANALOG`] is true.
///
/// On the bodged path the node is `+3V3 → R67 10 kΩ → node → R66 1 kΩ → PA2`, with
/// `C82` 100 nF and the switch on the node, so a press divides the rail down to
/// ≈ 300 mV while a release sits at ≈ 3300 mV (lower if the original leak turns
/// out to be on the node rather than on the pad). 400 mV separates the two with
/// margin and still works on a repaired board, where a press goes to ~0 V.
pub const BTN1_PRESSED_MV: u32 = 400;

/// `BUTT_1` level (mV) at or above which the button counts as released; the band
/// between this and [`BTN1_PRESSED_MV`] is hysteresis.
pub const BTN1_RELEASED_MV: u32 = 700;

// ---------------------------------------------------------------------------
// Encoder push-button (`ENC_BUTT` → PB0 → ADC1_IN15)
// ---------------------------------------------------------------------------

/// `ENC_BUTT` level (mV) at or below which the encoder switch counts as pressed.
///
/// The circuit is `+3V3 → R64 10 kΩ → node → R65 1 kΩ → PB0`, with `C81 100 nF` on
/// the node and `SW1`'s switch between it and GND (rev3 schematic). With the
/// switch closed that node is a hard ground, so PB0 sits within a few tens of mV
/// of 0 V even with the MCU's internal pull-up in circuit — which is how a GPIO
/// digital input would read it.
///
/// **rev3 boards have measured only ~2.4 V when pressed.** That is not something
/// the drawn circuit can do (it implies ~27 kΩ in the press path: a fluxy/cold
/// joint on `SW1`/`S1`/`S2`, or a high-resistance switch contact — worth
/// reflowing and checking with the board off, `S1`–`S2` should read <1 Ω), but the
/// firmware now copes with it: PB0 is sampled as an **analog input**
/// (`ADC1_IN15`) and compared against this threshold instead of a logic level.
/// A GPIO cannot do this job — `V_IL` is only ~0.3 · VDD ≈ 1 V.
///
/// The threshold sits between the two measured levels with margin on both sides:
/// 0.35 V above the 2.4 V "pressed" reading, 0.55 V below the 3.3 V rail that
/// `R64` pulls the released line to. The `input:` log prints the live level in
/// mV, so a board that reads differently can be calibrated by editing this.
///
/// Depends on `R64`: in analog mode the MCU's internal pull-up is off, so the
/// external 10 kΩ is the only thing that holds the released line high.
pub const ENC_BTN_PRESSED_MV: u32 = 2_750;

/// `ENC_BUTT` level (mV) at or above which the encoder switch counts as released.
///
/// Hysteresis band [`ENC_BTN_PRESSED_MV`]..[`ENC_BTN_RELEASED_MV`]: a level inside
/// it keeps the previous state, so a slowly ramping press, a noisy idle, or a
/// marginal joint whose resistance drifts cannot chatter the input.
pub const ENC_BTN_RELEASED_MV: u32 = 2_950;

/// Movement of an analog button level (`ENC_BUTT`, and `BUTT_1` when
/// [`BTN1_ANALOG`] is set) that is worth a log line even when it does not cross a
/// threshold, in millivolts.
///
/// The edge-triggered `input:` line only prints when the *verdict* flips, so a
/// press that lands between the two thresholds (a marginal switch path, exactly
/// the rev3 bench case) used to be completely silent — indistinguishable from
/// "the knob was never pressed". With this, any real movement of the line prints
/// its measured level, and **no line at all while pressing means the pin is not
/// moving**: an open switch contact, an unsoldered series resistor, or the
/// switch's ground return, not a threshold problem.
pub const ANALOG_BTN_LOG_DELTA_MV: u32 = 100;

// ---------------------------------------------------------------------------
// Status LED (D31 on PA15)
// ---------------------------------------------------------------------------

/// Status LED wiring: `PA15 → D31 (anode → cathode) → R81 → GND`, so PA15
/// HIGH lights it — **active high**. PA15 is `TIM2_CH1`; TIM2 is otherwise
/// unused (TIM3 is the encoder QEI on rev3, TIM15 is the Embassy time driver),
/// so the LED is driven as hardware PWM and can be dimmed precisely.
pub const LED_PWM_HZ: u32 = 1_000;

/// LED task tick. Only the on/off *envelope* is quantised to this; the
/// brightness inside an "on" phase is set by the PWM duty, so the heartbeat
/// stays dim and steady even at this tick rate.
pub const LED_TICK_MS: u64 = 5;

/// PWM duty for the healthy heartbeat pulses. Low enough to read as "very dim"
/// next to the full-brightness error codes — tune to taste on the bench.
pub const LED_HEARTBEAT_DUTY_PCT: u8 = 3;

/// PWM duty for fault/error code flashes (full brightness, hard to miss).
pub const LED_FAULT_DUTY_PCT: u8 = 100;

/// PWM duty for the in-progress (EEPROM activity) pulse.
pub const LED_ACTIVITY_DUTY_PCT: u8 = 35;

/// Report "INA228 missing" / "PD controller lost" / "PD no rail" as blink
/// codes. Set false to limit the LED to latched faults + EEPROM status, which
/// is what a build without those parts fitted wants.
pub const LED_SUBSYSTEM_CODES: bool = true;

/// Suppress the INA228 / PD-controller "missing" codes for this long after
/// boot. Both subsystems are probed asynchronously and are legitimately absent
/// for the first moments of a power-up, so the LED would otherwise flash a
/// code on a perfectly healthy boot.
pub const LED_SUBSYSTEM_GRACE_MS: u64 = 4_000;

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
