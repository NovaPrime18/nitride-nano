//! Port of the tps_eeprom_workflow C++ module.
//! Manages high-level states like Compare, Confirm, Flash logic.

use crate::eeprom_loader::{EepromLoader, LoaderState};
use crate::ui::input::{InputEvent, InputHandler};

#[derive(Debug, PartialEq)]
pub enum WorkflowState {
    Comparing,
    Confirming,
    LoadingConfig,
    Flashing,
    Done,
    Error,
}

pub trait ProgressReporter {
    fn report_progress(&mut self, message: &str);
}

/// Basic console reporter for progress messages
pub struct ConsoleProgressReporter;

impl ProgressReporter for ConsoleProgressReporter {
    fn report_progress(&mut self, message: &str) {
        println!("{}", message);
    }
}

pub struct EepromWorkflow<'a> {
    pub state: WorkflowState,
    pub loader: EepromLoader,
    pub progress_message: String,
    input_handler: &'a mut InputHandler,
    reporter: Option<Box<dyn ProgressReporter>>,
    display: &'a mut I2CDisplay<_, ssd1306::prelude::I2CInterface<_>>,
}

impl<'a> EepromWorkflow<'a> {

    pub fn new(loader: EepromLoader, input_handler: &'a mut InputHandler, display: &'a mut I2CDisplay<_, ssd1306::prelude::I2CInterface<_>>) -> Self {
        Self {
            state: WorkflowState::LoadingConfig,
            loader,
            progress_message: String::from("Loading configuration..."),
            input_handler,
            reporter: None,
            display,
        }
    }

    pub fn set_reporter(&mut self, reporter: Box<dyn ProgressReporter>) {
        self.reporter = Some(reporter);
    }

    /// Executes one iteration of the workflow state machine.
    pub fn update(&mut self) -> WorkflowState {
        if let Some(ref mut reporter) = self.reporter {
            reporter.report_progress(&self.progress_message);
        }

        match self.state {
            WorkflowState::LoadingConfig => {
                let mut buffer = [0; EEPROM_PAGE_SIZE];
                if let Err(e) = self.loader.load_config(&mut buffer) {
                    if let Some(ref mut reporter) = self.reporter {
                        reporter.report_progress("Configuration Load Failed.");
                    }
                    self.state = WorkflowState::Error;
                    return WorkflowState::Error;
                }
                // Assuming configuration is loaded, proceed to compare
                self.state = WorkflowState::Comparing;
                self.progress_message = "Checking EEPROM...";        
            }
            WorkflowState::Comparing => {
                // Logic to check if data is consistent
                self.state = WorkflowState::Confirming;
                self.progress_message = "Ready to flash. Press confirm.";
                WorkflowState::Confirming
            }
            WorkflowState::Confirming => {
                // Check for user input here (e.g., GPIO buttons)
                let event = self.input_handler.get_event();
                if event.is_some() && event.as_ref().unwrap().is_confirmation() {
                    self.state = WorkflowState::Flashing;
                    self.progress_message = "Flash initiated. Please wait...";
                }
                WorkflowState::Confirming
            }
            WorkflowState::Flashing => {
                let loader_status = self.loader.update_step();
                match loader_status {
                    LoaderState::Complete => {
                        if let Some(ref mut reporter) = self.reporter {
                            reporter.report_progress("Flash Complete!");
                        }
                        self.state = WorkflowState::Done;
                        WorkflowState::Done
                    }
                    LoaderState::Error => {
                        if let Some(ref mut reporter) = self.reporter {
                            reporter.report_progress("Flash Error.");
                        }
                        self.state = WorkflowState::Error;
                        WorkflowState::Error
                    }
                    _ => WorkflowState::Flashing,
                }
            }
            WorkflowState::Done | WorkflowState::Error => self.state.clone(),
        }
    }

    pub fn report_progress(&mut self, message: &str) {
        ssd1306_ui::show_message(self.display, message);
    }
}