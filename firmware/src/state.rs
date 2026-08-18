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

/// Filtered analog telemetry snapshot shared with the UI and control loop.
#[derive(Clone, Copy, Debug)]
pub struct Telemetry {
    pub vin_mv: u32,
    pub vout_mv: u32,
    pub iout_ma: u32,
    pub pout_mw: u32,
    pub temp_conv_c: i32,
    pub temp_input_c: i32,
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
            input_current_cap_ma: board::IOUT_MAX_MA,
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
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            screen: MenuScreen::Main,
            editing: false,
            pd_profile_index: 0,
            encoder_step_mode: StepMode::Fine,
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
}

impl Default for EepromUiSnapshot {
    fn default() -> Self {
        Self {
            title: "EEPROM FLASH",
            message: "",
            progress_percent: 0,
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
    /// Parsed source capabilities from the PD controller (max 7 SPR + 7 EPR PDOs
    /// would overflow the Rx Source Capabilities register; 13 covers what fits).
    pub pd_caps: [SourceCapability; 13],
    pub pd_cap_count: u8,
    // TODO(dead-code): written nowhere and read nowhere — encoder deltas are passed
    // directly from the main loop into `ui::input::InputHandler::poll` instead.
    // pub encoder_delta: i16,
    pub eeprom_ui: EepromUiSnapshot,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            supply: SupplyState::default(),
            telemetry: Telemetry::default(),
            ui: UiState::default(),
            pd: PdState::NoCable,
            pd_caps: [SourceCapability::EMPTY; 13],
            pd_cap_count: 0,
            eeprom_ui: EepromUiSnapshot::default(),
        }
    }
}
