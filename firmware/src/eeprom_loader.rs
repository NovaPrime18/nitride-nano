//! CAT24C512 EEPROM writer for the TPS26750 full-flash image.

use embassy_stm32::i2c::{Error as I2cError, I2c, Master};
use embassy_stm32::mode::Async;
use embassy_time::{Duration, Timer};

use crate::board::CAT24C512_ADDR;

pub const EEPROM_PAGE_SIZE: usize = 128;
pub const EEPROM_WRITE_DELAY_MS: u64 = 6;

include!(concat!(env!("OUT_DIR"), "/tps26750_full_flash.rs"));

pub const TPS26750_FULL_FLASH: &[u8] = &TPS26750_FULL_FLASH_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderState {
    Idle,
    Writing,
    Verifying,
    Complete,
    Error,
}

pub struct EepromLoader {
    image: &'static [u8],
    pub state: LoaderState,
    pub offset: usize,
    pub last_error: Option<&'static str>,
    probed: bool,
}

impl EepromLoader {
    pub const fn new(image: &'static [u8]) -> Self {
        Self {
            image,
            state: LoaderState::Idle,
            offset: 0,
            last_error: None,
            probed: false,
        }
    }

    pub fn full_flash() -> Self {
        Self::new(TPS26750_FULL_FLASH)
    }

    pub fn total_size(&self) -> usize {
        self.image.len()
    }

    pub fn progress_bytes(&self) -> usize {
        self.offset.min(self.image.len())
    }

    pub fn start(&mut self) {
        self.offset = 0;
        self.last_error = None;
        self.probed = false;
        self.state = LoaderState::Writing;
        defmt::info!(
            "EEPROM flash start: addr=0x{:02x}, image={} bytes, page={} bytes",
            CAT24C512_ADDR,
            self.image.len(),
            EEPROM_PAGE_SIZE
        );
    }

    pub async fn update_step(&mut self, i2c: &mut I2c<'_, Async, Master>) -> LoaderState {
        match self.state {
            LoaderState::Idle => self.start(),
            LoaderState::Writing => {
                if !self.probed {
                    if self.validate_eeprom_presence(i2c).await.is_err() {
                        defmt::error!("EEPROM validation probe failed before first page");
                        self.state = LoaderState::Error;
                    }
                } else if self.offset >= self.image.len() {
                    defmt::info!("EEPROM write phase complete; starting verify");
                    self.offset = 0;
                    self.state = LoaderState::Verifying;
                } else if self.write_next_page(i2c).await.is_err() {
                    defmt::error!("EEPROM write phase failed at offset=0x{:04x}", self.offset);
                    self.state = LoaderState::Error;
                }
            }
            LoaderState::Verifying => {
                if self.offset >= self.image.len() {
                    defmt::info!("EEPROM verify complete");
                    self.state = LoaderState::Complete;
                } else if self.verify_next_page(i2c).await.is_err() {
                    defmt::error!("EEPROM verify phase failed at offset=0x{:04x}", self.offset);
                    self.state = LoaderState::Error;
                }
            }
            LoaderState::Complete | LoaderState::Error => {}
        }

        self.state
    }

    async fn validate_eeprom_presence(
        &mut self,
        i2c: &mut I2c<'_, Async, Master>,
    ) -> Result<(), ()> {
        let address_pointer = [0x00, 0x00];
        let mut byte = [0u8; 1];

        defmt::info!(
            "EEPROM validation: zero-length write probe to dev=0x{:02x}",
            CAT24C512_ADDR
        );

        match i2c.write(CAT24C512_ADDR, &[]).await {
            Ok(()) => defmt::info!(
                "EEPROM validation OK: dev=0x{:02x} ACKed zero-length write",
                CAT24C512_ADDR
            ),
            Err(err) => defmt::warn!(
                "EEPROM zero-length write probe failed: dev=0x{:02x}, err={}",
                CAT24C512_ADDR,
                i2c_error_label(err)
            ),
        }

        defmt::info!(
            "EEPROM validation: address-pointer write probe to dev=0x{:02x}",
            CAT24C512_ADDR
        );

        if let Err(err) = i2c.write(CAT24C512_ADDR, &address_pointer).await {
            self.last_error = Some("EEPROM address probe failed");
            defmt::error!(
                "EEPROM address-pointer probe failed: dev=0x{:02x}, err={}",
                CAT24C512_ADDR,
                i2c_error_label(err)
            );
            self.scan_address_pins(i2c).await;
            return Err(());
        }

        defmt::info!(
            "EEPROM validation OK: dev=0x{:02x} ACKed address-pointer write",
            CAT24C512_ADDR
        );

        defmt::info!(
            "EEPROM validation: 1-byte random read probe from addr=0x0000"
        );

        if let Err(err) = i2c
            .write_read(CAT24C512_ADDR, &address_pointer, &mut byte)
            .await
        {
            self.last_error = Some("EEPROM read probe failed");
            defmt::error!(
                "EEPROM 1-byte read probe failed: dev=0x{:02x}, err={}",
                CAT24C512_ADDR,
                i2c_error_label(err)
            );
            return Err(());
        }

        self.probed = true;
        defmt::info!(
            "EEPROM validation OK: read addr=0x0000 -> 0x{:02x}",
            byte[0]
        );
        Ok(())
    }

    async fn scan_address_pins(&mut self, i2c: &mut I2c<'_, Async, Master>) {
        let address_pointer = [0x00, 0x00];

        defmt::info!("EEPROM validation: scanning 0x50..0x57 with address-pointer writes");
        for address in 0x50..=0x57 {
            match i2c.write(address, &address_pointer).await {
                Ok(()) => defmt::info!("EEPROM scan: ACK at dev=0x{:02x}", address),
                Err(I2cError::Nack) => {}
                Err(err) => defmt::warn!(
                    "EEPROM scan: dev=0x{:02x}, err={}",
                    address,
                    i2c_error_label(err)
                ),
            }
            Timer::after(Duration::from_millis(1)).await;
        }
    }

    async fn write_next_page(&mut self, i2c: &mut I2c<'_, Async, Master>) -> Result<(), ()> {
        let end = (self.offset + EEPROM_PAGE_SIZE).min(self.image.len());
        let chunk = &self.image[self.offset..end];
        let page_start = self.offset;
        let mut write = [0u8; EEPROM_PAGE_SIZE + 2];
        write[0] = (self.offset >> 8) as u8;
        write[1] = self.offset as u8;
        write[2..2 + chunk.len()].copy_from_slice(chunk);

        if page_start % 1024 == 0 {
            defmt::info!(
                "EEPROM writing: offset=0x{:04x}, len={}, progress={}/{}",
                page_start,
                chunk.len(),
                page_start,
                self.image.len()
            );
        }

        if let Err(err) = i2c
            .write(CAT24C512_ADDR, &write[..2 + chunk.len()])
            .await
        {
            defmt::error!(
                "EEPROM I2C write failed: dev=0x{:02x}, offset=0x{:04x}, len={}, err={}",
                CAT24C512_ADDR,
                page_start,
                chunk.len(),
                i2c_error_label(err)
            );
            self.last_error = Some("EEPROM write failed");
            return Err(());
        }

        Timer::after(Duration::from_millis(EEPROM_WRITE_DELAY_MS)).await;
        self.offset = end;
        Ok(())
    }

    async fn verify_next_page(&mut self, i2c: &mut I2c<'_, Async, Master>) -> Result<(), ()> {
        let end = (self.offset + EEPROM_PAGE_SIZE).min(self.image.len());
        let expected = &self.image[self.offset..end];
        let page_start = self.offset;
        let addr = [(self.offset >> 8) as u8, self.offset as u8];
        let mut read = [0u8; EEPROM_PAGE_SIZE];

        if page_start % 1024 == 0 {
            defmt::info!(
                "EEPROM verifying: offset=0x{:04x}, len={}, progress={}/{}",
                page_start,
                expected.len(),
                page_start,
                self.image.len()
            );
        }

        if let Err(err) = i2c
            .write_read(CAT24C512_ADDR, &addr, &mut read[..expected.len()])
            .await
        {
            defmt::error!(
                "EEPROM I2C readback failed: dev=0x{:02x}, offset=0x{:04x}, len={}, err={}",
                CAT24C512_ADDR,
                page_start,
                expected.len(),
                i2c_error_label(err)
            );
            self.last_error = Some("EEPROM readback failed");
            return Err(());
        }

        if &read[..expected.len()] != expected {
            for idx in 0..expected.len() {
                if read[idx] != expected[idx] {
                    defmt::error!(
                        "EEPROM verify mismatch: offset=0x{:04x}, expected=0x{:02x}, got=0x{:02x}",
                        page_start + idx,
                        expected[idx],
                        read[idx]
                    );
                    break;
                }
            }
            self.last_error = Some("EEPROM verify mismatch");
            return Err(());
        }

        self.offset = end;
        Ok(())
    }
}

fn i2c_error_label(err: I2cError) -> &'static str {
    match err {
        I2cError::Bus => "bus",
        I2cError::Arbitration => "arbitration",
        I2cError::Nack => "nack",
        I2cError::Timeout => "timeout",
        I2cError::Crc => "crc",
        I2cError::Overrun => "overrun",
        I2cError::ZeroLengthTransfer => "zero-length",
    }
}
