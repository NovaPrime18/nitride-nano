//! Shared runtime state between tasks.

use crate::board;
use crate::drivers::tps26750::SourceCapability;

/// Output stage operating mode: which control loop drives the converter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupplyMode {
    Off,
    Cv,
    Cc,
}

/// Latched hardware fault. Set by [`crate::control::supply::SupplyController`];
/// cleared from the UI (encoder button on the main screen).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    OverCurrent,
    OverVoltage,
    OverPower,
    OverTemp,
    /// Input-bus overcurrent from the INA228 (beyond the PD contract cap).
    InputOverCurrent,
    /// Input-bus overvoltage from the INA228.
    InputOverVoltage,
    // TODO(dead-code): reserved for a "user switched the output off" state, but the
    // UI toggles `SupplyState::enabled` directly instead of raising a fault. Never
    // constructed or matched anywhere.
    // UserOff,
}

impl Fault {
    /// Short display label for the OLED header, replacing the temperature badges
    /// while a fault is latched. Returns `None` when no fault prevents turn-on.
    pub fn label(self) -> Option<&'static str> {
        match self {
            Fault::None => None,
            Fault::OverCurrent => Some("OVERCURRENT"),
            Fault::OverVoltage => Some("OVERVOLTAGE"),
            Fault::OverPower => Some("OVERPOWER"),
            Fault::OverTemp => Some("OVERTEMP"),
            Fault::InputOverCurrent => Some("IN OCP"),
            Fault::InputOverVoltage => Some("IN OVP"),
        }
    }
}

/// USB-PD connection state machine, driven by [`crate::pd::manager::PdManager`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdState {
    NoCable,
    Negotiating,
    ContractActive,
    // TODO(dead-code): no PD error path currently transitions into this state;
    // failures are only logged via defmt. Preserved for future error reporting.
    // Fault,
}

/// How the PD input rail is chosen: manually from the preset list, or
/// automatically from the output setpoint by [`crate::pd::auto_track`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdMode {
    Manual,
    Auto,
}

/// Optimisation target for Auto-tracking PD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoPolicy {
    /// Stay outside the LT8390A 4-switch buck-boost region; fall back to the
    /// most capable rail only when no clean rail can supply the requested power.
    Efficiency,
    /// Always pick the rail that can deliver the most power, accepting 4-switch
    /// losses when the closest rail sits near Vout.
    Power,
}

/// Which LT8390A operating region a chosen rail is expected to produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailRegion {
    Buck,
    Boost,
    /// Efficiency-first policy had to fall back to a rail that does not clear
    /// the 4-switch band because the clean options could not supply the power.
    FallbackPower,
    Unavailable,
}

/// Why Auto-tracking could not produce a rail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdAutoError {
    None,
    NoCable,
    NoRail,
}

/// PD rail-selection state shared with the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PdControl {
    pub mode: PdMode,
    pub policy: AutoPolicy,
    /// Rail currently requested (0 when none).
    pub target_mv: u32,
    pub region: RailRegion,
    pub error: PdAutoError,
    /// Set by the UI when the user changes the mode/preset/policy; the PD
    /// manager consumes it and renegotiates once.
    pub renegotiate_request: bool,
    /// One-shot "MAX" request: ignore the preset and ask for the highest rail the
    /// source offers (highest EPR PDO when EPR is available, else the highest SPR
    /// fixed PDO). Set by BTN2 in Manual mode; cleared once the manager plans it.
    pub max_request: bool,
}

impl Default for PdControl {
    fn default() -> Self {
        Self {
            mode: PdMode::Manual,
            policy: AutoPolicy::Efficiency,
            target_mv: 0,
            region: RailRegion::Unavailable,
            error: PdAutoError::NoCable,
            renegotiate_request: false,
            max_request: false,
        }
    }
}

/// USB-PD contract preset voltages in millivolts.
pub const PD_PRESET_VOLTAGES_MV: [u32; 6] = [12_000, 15_000, 20_000, 28_000, 36_000, 48_000];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuScreen {
    Main,
    CvSetpoint,
    CcLimit,
    PdContract,
    Settings,
    EepromFlash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepMode {
    Fine,
    Coarse,
}

/// One entry in the fullscreen CFG (Settings) list.
///
/// Adding an option is a four-step change: a new variant here, its label in
/// [`CfgItem::label`], an activation arm in `ui::menu::cfg_activate`, and the
/// element appended to [`CFG_ITEMS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CfgItem {
    EepromWrite,
    OutputSweep,
    PdContract,
}

impl CfgItem {
    /// Display label for the CFG list row. Must stay within the 128 px panel at
    /// the list's x offset (21 characters max) — keep it short.
    pub fn label(self) -> &'static str {
        match self {
            CfgItem::EepromWrite => "EEPROM WRITE",
            CfgItem::OutputSweep => "OUTPUT V SWEEP",
            CfgItem::PdContract => "PD CONTRACT",
        }
    }
}

/// Fullscreen CFG list contents, in display order.
pub const CFG_ITEMS: [CfgItem; 3] = [
    CfgItem::EepromWrite,
    CfgItem::OutputSweep,
    CfgItem::PdContract,
];

/// Number of CFG rows that fit on screen at once; the list scrolls when
/// [`CFG_ITEMS`] grows past this.
pub const CFG_VISIBLE_ROWS: u8 = 4;

/// Lifecycle of the CFG "Output V sweep" option.
///
/// `Armed` is entered by selecting the option in the CFG list (which returns to
/// the Main screen); the encoder button confirms and starts the sweep. `Done`
/// is the parked end state, dismissed by any button press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepPhase {
    Off,
    Armed,
    Running,
    Done,
}

/// User-visible sweep state, mirrored from `control::sweep::SweepController`.
///
/// The controller owns the `Instant`-based step timing; only the phase, the
/// 0-based point index, and the one-shot start request live here so the UI task
/// can render progress from a plain `AppState` clone.
#[derive(Clone, Copy, Debug)]
pub struct SweepState {
    pub phase: SweepPhase,
    /// 0-based index of the point currently commanded (`Running`/`Done`).
    pub index: u8,
    /// Set by the UI when the encoder button confirms an armed sweep; consumed
    /// by the controller on the next supply tick.
    pub start_request: bool,
}

impl Default for SweepState {
    fn default() -> Self {
        Self {
            phase: SweepPhase::Off,
            index: 0,
            start_request: false,
        }
    }
}

/// Filtered analog telemetry snapshot shared with the UI and control loop.
///
/// Output-side fields (`vout_mv`, `iout_ma`, `pout_mw`) come from the MCU ADCs;
/// the input-side fields (`vin_mv`, `iin_ma`, `pin_mw`, `ina_temp_c`) are
/// refreshed from the INA228 when it is present, with the ADC's Vbus reading as
/// the fallback for `vin_mv`.
#[derive(Clone, Copy, Debug)]
pub struct Telemetry {
    pub vin_mv: u32,
    pub vout_mv: u32,
    pub iout_ma: u32,
    pub pout_mw: u32,
    pub temp_conv_c: i32,
    pub temp_input_c: i32,
    /// Input current from the INA228 (positive = flowing into the converter).
    pub iin_ma: i32,
    /// Input power from the INA228.
    pub pin_mw: i32,
    /// INA228 die temperature.
    pub ina_temp_c: i32,
    /// True while the most recent INA228 reads are succeeding.
    pub ina_ok: bool,
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            vin_mv: 0,
            vout_mv: 0,
            iout_ma: 0,
            pout_mw: 0,
            temp_conv_c: 25,
            temp_input_c: 25,
            iin_ma: 0,
            pin_mw: 0,
            ina_temp_c: 25,
            ina_ok: false,
        }
    }
}

/// Setpoints, slew state, and caps for the programmable output stage.
#[derive(Clone, Copy, Debug)]
pub struct SupplyState {
    pub mode: SupplyMode,
    pub enabled: bool,
    pub v_set_mv: u32,
    pub i_set_ma: u32,
    pub fault: Fault,
    pub input_power_cap_mw: u32,
    pub input_current_cap_ma: u32,
}

impl Default for SupplyState {
    fn default() -> Self {
        Self {
            mode: SupplyMode::Off,
            enabled: false,
            v_set_mv: 30_000,
            i_set_ma: 5_000,
            fault: Fault::None,
            input_power_cap_mw: board::POWER_MAX_MW,
            // Input-bus backstop only (see `control::supply`): the design max
            // until a PD contract narrows it, and restored to this whenever the
            // contract goes away so an XT90 feed gets the full design current.
            input_current_cap_ma: board::IIN_MAX_MA as u32,
        }
    }
}

/// Menu/navigation state for the OLED UI.
#[derive(Clone, Copy, Debug)]
pub struct UiState {
    pub screen: MenuScreen,
    pub editing: bool,
    pub pd_profile_index: u8,
    pub encoder_step_mode: StepMode,
    /// Selected row in the fullscreen CFG list.
    pub cfg_index: u8,
    /// First CFG row currently visible (viewport top); keeps the highlight in
    /// view as the list scrolls.
    pub cfg_scroll: u8,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            screen: MenuScreen::Main,
            editing: false,
            pd_profile_index: 0,
            encoder_step_mode: StepMode::Fine,
            cfg_index: 0,
            cfg_scroll: 0,
        }
    }
}

/// Immutable snapshot of the EEPROM flashing workflow, for display on the OLED.
///
/// Kept separate from the workflow itself so the UI task never has to touch the
/// loader state machine directly.
#[derive(Clone, Copy, Debug)]
pub struct EepromUiSnapshot {
    pub title: &'static str,
    pub message: &'static str,
    pub progress_percent: u8,
    /// Loader is actively writing/verifying. Drives the status LED's activity
    /// pulse while the flash screen is open.
    pub busy: bool,
    /// The last flash attempt failed. Drives the status LED's EEPROM error code.
    pub failed: bool,
}

impl Default for EepromUiSnapshot {
    fn default() -> Self {
        Self {
            title: "EEPROM FLASH",
            message: "",
            progress_percent: 0,
            busy: false,
            failed: false,
        }
    }
}

/// Root shared state, guarded by the `APP_STATE` mutex in [`crate::runtime`].
#[derive(Clone)]
pub struct AppState {
    pub supply: SupplyState,
    pub telemetry: Telemetry,
    pub ui: UiState,
    pub pd: PdState,
    /// TPS26750 presence, published by [`crate::pd::manager::PdManager`]'s
    /// watchdog. False until the controller first answers, and again after it is
    /// declared lost. Read by the status LED (and available to the UI).
    pub pd_present: bool,
    /// Parsed source capabilities from the PD controller (max 7 SPR + 7 EPR PDOs
    /// would overflow the Rx Source Capabilities register; 13 covers what fits).
    pub pd_caps: [SourceCapability; 13],
    pub pd_cap_count: u8,
    /// Manual/auto rail selection state, owned by the UI and the PD manager.
    pub pd_control: PdControl,
    // TODO(dead-code): written nowhere and read nowhere — encoder deltas are passed
    // directly from the main loop into `ui::input::InputHandler::poll` instead.
    // pub encoder_delta: i16,
    pub eeprom_ui: EepromUiSnapshot,
    /// CFG "Output V sweep" lifecycle/progress, rendered on the Main screen's
    /// bottom line while not [`SweepPhase::Off`].
    pub sweep: SweepState,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            supply: SupplyState::default(),
            telemetry: Telemetry::default(),
            ui: UiState::default(),
            pd: PdState::NoCable,
            pd_present: false,
            pd_caps: [SourceCapability::EMPTY; 13],
            pd_cap_count: 0,
            pd_control: PdControl::default(),
            eeprom_ui: EepromUiSnapshot::default(),
            sweep: SweepState::default(),
        }
    }
}
