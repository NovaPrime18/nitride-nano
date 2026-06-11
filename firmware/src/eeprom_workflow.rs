//! High-level EEPROM upload workflow used by the OLED UI.

use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;

use crate::eeprom_loader::{EepromLoader, LoaderState};
use crate::ui::input::InputEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowState {
    Confirming,
    Flashing,
    Done,
    Error,
}

pub struct EepromWorkflow {
    pub state: WorkflowState,
    pub loader: EepromLoader,
}

impl EepromWorkflow {
    pub fn new() -> Self {
        let loader = EepromLoader::full_flash();
        defmt::info!(
            "EEPROM workflow ready: TPS26750 full-flash image={} bytes",
            loader.total_size()
        );
        Self {
            state: WorkflowState::Confirming,
            loader,
        }
    }

    pub fn handle_input(&mut self, event: InputEvent) {
        match (self.state, event) {
            (
                WorkflowState::Confirming | WorkflowState::Done | WorkflowState::Error,
                InputEvent::Btn1,
            )
            | (
                WorkflowState::Confirming | WorkflowState::Done | WorkflowState::Error,
                InputEvent::EncBtn,
            ) => {
                defmt::info!("EEPROM workflow start requested");
                self.loader.start();
                self.state = WorkflowState::Flashing;
            }
            _ => {}
        }
    }

    pub async fn update(&mut self, i2c: &mut I2c<'_, Async, Master>) -> WorkflowState {
        if self.state != WorkflowState::Flashing {
            return self.state;
        }

        match self.loader.update_step(i2c).await {
            LoaderState::Complete => {
                defmt::info!("EEPROM workflow complete");
                self.state = WorkflowState::Done;
            }
            LoaderState::Error => {
                defmt::error!("EEPROM workflow error");
                self.state = WorkflowState::Error;
            }
            _ => {}
        }

        self.state
    }

    pub fn title(&self) -> &'static str {
        match self.state {
            WorkflowState::Confirming => "EEPROM FLASH",
            WorkflowState::Flashing => "EEPROM BUSY",
            WorkflowState::Done => "EEPROM DONE",
            WorkflowState::Error => "EEPROM ERROR",
        }
    }

    pub fn message(&self) -> &'static str {
        match self.state {
            WorkflowState::Confirming => "BTN1 TO START",
            WorkflowState::Flashing => match self.loader.state {
                LoaderState::Writing => "WRITING IMAGE",
                LoaderState::Verifying => "VERIFYING",
                _ => "STARTING",
            },
            WorkflowState::Done => "VERIFY OK",
            WorkflowState::Error => self.loader.last_error.unwrap_or("FAILED"),
        }
    }

    pub fn progress_percent(&self) -> u8 {
        let total = self.loader.total_size().max(1);
        ((self.loader.progress_bytes() * 100) / total).min(100) as u8
    }
}

impl Default for EepromWorkflow {
    fn default() -> Self {
        Self::new()
    }
}
