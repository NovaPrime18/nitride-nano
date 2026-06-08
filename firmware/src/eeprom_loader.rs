//! Port of the tps_eeprom_loader C++ module for EEPROM management.
//! This handles low-level I2C communication, page management, and write cycles for the CAT24C512.

use embedded_hal::i2c::I2c;

pub const EEPROM_I2C_ADDR: u8 = 0x50;
pub const EEPROM_PAGE_SIZE: usize = 128;
pub const EEPROM_WRITE_DELAY_MS: u32 = 6;
pub const EEPROM_I2C_TIMEOUT_US: u32 = 50_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoaderState {
    Idle,
    Writing,
    Verifying,
    Complete,
    Error,
}

pub struct EepromLoader {
    /// The I2C peripheral instance.
    pub i2c: I2c,
    pub state: LoaderState,
    pub write_offset: usize,
    pub total_size: usize,
    pub last_error: Option<&'static str>,
    pub buffer: [u8; EEPROM_PAGE_SIZE],
}

impl EepromLoader {
    /// Creates a new instance of the `EepromLoader`.
    ///
    /// # Arguments
    /// * `i2c` - The I2C peripheral instance to use for communication with the EEPROM.
    /// * `total_size` - The total size of the EEPROM in bytes.
    pub fn new(i2c: I2c, total_size: usize) -> Self {
        Self {
            i2c,
            state: LoaderState::Idle,
            write_offset: 0,
            total_size,
            last_error: None,
            buffer: [0; EEPROM_PAGE_SIZE],
        }
    }

    /// Writes a block of data to the EEPROM.
    ///
    /// # Arguments
    /// * `address` - The starting address in the EEPROM where the data should be written.
    /// * `data` - A slice of bytes containing the data to write to the EEPROM.
    pub fn write_block(&mut self, address: u32, data: &[u8]) -> Result<(), &'static str> {
        if data.is_empty() || data.len() % EEPROM_PAGE_SIZE != 0 {
            return Err("Invalid data length for EEPROM");
        }
        
        let mut current_addr = address as usize;
        let mut data_idx = 0;

        while data_idx < data.len() {
            let page_offset = current_addr % EEPROM_PAGE_SIZE;
            let remaining_in_page = EEPROM_PAGE_SIZE - page_offset;
            let chunk_size = if (data.len() - data_idx) > remaining_in_page {
                remaining_in_page
            } else {
                data.len() - data_idx
            };

            let &chunk = &data[data_idx..data_idx + chunk_size];
            
            // Perform the I2C write
            match self.i2c.write(EEPROM_I2C_ADDR, &data_len_bytes_for_address(current_addr).to_be_bytes()).and_then(|_| {
                self.i2c.write(EEPROM_I2C_ADDR, &chunk)
            }) {
                Ok(_) => (),
                Err(e) => {
                    self.last_error = Some("I2C Write Failed");
                    return Err(e);
                }
            }

            current_addr += chunk_size;
            data_idx += chunk_size;

            // Add delay to respect EEPROM write cycle time
            delay_ms(EEPROM_WRITE_DELAY_MS);
        }

        Ok(())
    }

    /// Reads a block of data from the EEPROM.
    ///
    /// # Arguments
    /// * `address` - The starting address in the EEPROM from which to read the data.
    /// * `buffer` - A mutable slice of bytes where the read data will be stored.
    pub fn read_block(&mut self, address: u32, buffer: &mut [u8]) -> Result<(), &'static str> {
        // Using "Repeated Start" logic as seen in original code
        self.i2c.write_read(EEPROM_I2C_ADDR, &address.to_be_bytes(), buffer).map_err(|_| "I2C Read Failed")
    }

    /// Loads configuration data from the EEPROM.
    pub fn load_config(&mut self, buffer: &mut [u8]) -> Result<(), &'static str> {
        if buffer.len() != EEPROM_PAGE_SIZE {
            return Err("Invalid buffer size for config");
        }
        self.read_block(0, buffer)
    }

    /// Processes one step of the flash state machine.
    pub fn update_step(&mut self) -> LoaderState {
        match self.state {
            LoaderState::Idle => {
                self.state = LoaderState::LoadingConfig;
                LoaderState::LoadingConfig
            }
            LoaderState::LoadingConfig => {
                if let Err(e) = self.load_config(&mut self.buffer) {
                    self.last_error = Some("Load Config Failed");
                    return LoaderState::Error;
                }
                self.state = LoaderState::Writing;
                self.write_offset = 0;
                LoaderState::Writing
            }
            LoaderState::Writing => {
                if self.write_offset < self.total_size {
                    // Logic for handling write cycles and updates
                    let write_result = self.write_block(self.write_offset as u32, &self.buffer[self.write_offset..self.write_offset + EEPROM_PAGE_SIZE]);
                    match write_result {
                        Ok(_) => {
                            self.write_offset += EEPROM_PAGE_SIZE;
                            LoaderState::Writing
                        },
                        Err(e) => {
                            self.last_error = Some("Write Block Failed");
                            return LoaderState::Error;
                        }
                    }
                } else {
                    self.state = LoaderState::Verifying;
                    LoaderState::Verifying
                }
            }
            LoaderState::Verifying => {
                // Verification logic
                if verify_data(&self.buffer, &self.i2c) {
                    self.state = LoaderState::Complete;
                    LoaderState::Complete
                } else {
                    self.last_error = Some("Verification Failed");
                    self.state = LoaderState::Error;
                    LoaderState::Error
                }
            }
            _ => self.state,
        }
    }
}

/// Converts a memory address to its corresponding byte representation for I2C communication.
///
/// # Arguments
/// * `addr` - The memory address to convert.
fn data_len_bytes_for_address(addr: usize) -> [u8; 2] {
    [(addr >> 8) as u8, addr as u8]
}