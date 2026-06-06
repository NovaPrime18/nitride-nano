//! Shared runtime state between tasks.

use crate::board;
use crate::drivers::tps26750::SourceCapability;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupplyMode {
    Off,
    Cv,
    Cc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    OverCurrent,
    OverVoltage,
    OverPower,
    OverTemp,
    UserOff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdState {
    NoCable,
    Negotiating,
    ContractActive,
    Fault,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuScreen {
    Main,
    CvSetpoint,
    CcLimit,
    Enable,
    PdProfile,
    Settings,
}

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

#[derive(Clone, Copy, Debug)]
pub struct SupplyState {
    pub mode: SupplyMode,
    pub enabled: bool,
    pub v_set_mv: u32,
    pub i_set_ma: u32,
    pub v_slewed_mv: u32,
    pub fault: Fault,
    pub input_power_cap_mw: u32,
    pub input_current_cap_ma: u32,
}

impl Default for SupplyState {
    fn default() -> Self {
        Self {
            mode: SupplyMode::Off,
            enabled: false,
            v_set_mv: 5_000,
            i_set_ma: 1_000,
            v_slewed_mv: 0,
            fault: Fault::None,
            input_power_cap_mw: board::POWER_MAX_MW,
            input_current_cap_ma: board::IOUT_MAX_MA,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct UiState {
    pub screen: MenuScreen,
    pub editing: bool,
    pub pd_profile_index: u8,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            screen: MenuScreen::Main,
            editing: false,
            pd_profile_index: 0,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub supply: SupplyState,
    pub telemetry: Telemetry,
    pub ui: UiState,
    pub pd: PdState,
    pub pd_caps: [SourceCapability; 13],
    pub pd_cap_count: u8,
    pub encoder_delta: i16,
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
            encoder_delta: 0,
        }
    }
}
