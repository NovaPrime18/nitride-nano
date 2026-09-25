//! TI INA228 input-side power monitor — plain SMBus register interface.
//!
//! On rev2 the INA228 (U9, address `0x40`, A0 = A1 = GND) sits on the PD bus
//! (I2C1: `SCL=PB8`, `SDA=PB7`) next to the TPS26750 and measures the
//! **converter input** bus: an 8 mΩ shunt (R60) between the USB-PD side
//! (`IN+`) and the converter input (`IN-`), with its VBUS pin on the converter
//! side of that shunt. It is *not* an output monitor — output telemetry still
//! comes from the MCU ADCs.
//!
//! Register access is ordinary SMBus (`write_read`), unlike the TPS26750's
//! TI "Unique Address" protocol.

use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;

use crate::board;

pub const INA228_REG_CONFIG: u8 = 0x00;
pub const INA228_REG_ADC_CONFIG: u8 = 0x01;
pub const INA228_REG_SHUNT_CAL: u8 = 0x02;
pub const INA228_REG_VSHUNT: u8 = 0x04;
pub const INA228_REG_VBUS: u8 = 0x05;
pub const INA228_REG_DIETEMP: u8 = 0x06;
pub const INA228_REG_DIAG_ALRT: u8 = 0x0B;
pub const INA228_REG_MANUFACTURER_ID: u8 = 0x3E;
pub const INA228_REG_DEVICE_ID: u8 = 0x3F;

/// MANUFACTURER_ID reset value ("TI" little-endian).
const INA228_MFG_ID_TI: u16 = 0x5449;
/// Upper 12 bits of DEVICE_ID; the low nibble is the die revision.
const INA228_DEVICE_ID: u16 = 0x228;

/// One decoded INA228 sample, in the firmware's milli-units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ina228Reading {
    /// Converter input bus voltage (converter side of the 8 mΩ shunt).
    pub vin_mv: u32,
    /// Input current; positive means current flowing into the converter.
    pub iin_ma: i32,
    /// Input power derived from `vin_mv * iin_ma`.
    pub pin_mw: i32,
    /// Die temperature in whole degrees Celsius.
    pub die_temp_c: i32,
    /// Raw shunt reading in microvolts (useful for calibration logs).
    pub shunt_uv: i32,
}

/// Handle for the INA228 at a fixed 7-bit I2C address.
pub struct Ina228 {
    addr: u8,
}

impl Ina228 {
    pub fn new(addr: u8) -> Self {
        Self { addr }
    }

    /// Configure the device and verify its identity.
    ///
    /// Returns false when the chip does not ACK or reports an unexpected
    /// manufacturer/device ID, so callers can keep the ADC fallback path.
    pub async fn init(&self, i2c: &mut I2c<'_, Async, Master>) -> bool {
        if !self
            .write_reg16(i2c, INA228_REG_CONFIG, board::INA228_CONFIG)
            .await
        {
            return false;
        }
        if !self
            .write_reg16(i2c, INA228_REG_ADC_CONFIG, board::INA228_ADC_CONFIG)
            .await
        {
            return false;
        }
        if !self
            .write_reg16(i2c, INA228_REG_SHUNT_CAL, board::INA228_SHUNT_CAL)
            .await
        {
            return false;
        }

        let mfg = self
            .read_reg16(i2c, INA228_REG_MANUFACTURER_ID)
            .await
            .unwrap_or(0);
        let dev = self
            .read_reg16(i2c, INA228_REG_DEVICE_ID)
            .await
            .unwrap_or(0);
        let ok = mfg == INA228_MFG_ID_TI && (dev >> 4) == INA228_DEVICE_ID;
        if ok {
            defmt::info!(
                "INA228 OK: addr=0x{:02x}, mfg=0x{:04x}, dev=0x{:04x}",
                self.addr,
                mfg,
                dev
            );
        } else {
            defmt::error!(
                "INA228 identity mismatch: addr=0x{:02x}, mfg=0x{:04x}, dev=0x{:04x}",
                self.addr,
                mfg,
                dev
            );
        }
        ok
    }

    /// Read and decode VSHUNT, VBUS and DIETEMP in one pass.
    ///
    /// Returns `None` on any bus error or when the reserved low nibble of a
    /// 20-bit measurement register is non-zero (frame corruption).
    pub async fn read(&self, i2c: &mut I2c<'_, Async, Master>) -> Option<Ina228Reading> {
        let raw_shunt = self.read_reg24(i2c, INA228_REG_VSHUNT).await?;
        let raw_bus = self.read_reg24(i2c, INA228_REG_VBUS).await?;
        let raw_temp = self.read_reg16(i2c, INA228_REG_DIETEMP).await?;

        if raw_shunt & 0x000F != 0 || raw_bus & 0x000F != 0 {
            return None;
        }

        // 20-bit signed field in bits 23:4. `(raw << 8) >> 12` sign-extends it.
        let shunt20 = ((raw_shunt as i32) << 8) >> 12;
        let bus20 = (raw_bus >> 4) as u32;

        // VSHUNT LSB = 312.5 nV → µV = raw * 3125 / 10000.
        // I(mA) = Vshunt(µV) / R(mΩ) = raw * 3125 / 80000 for the 8 mΩ shunt.
        let shunt_uv = ((shunt20 as i64 * 3125) / 10_000) as i32;
        let iin_ma = ((shunt20 as i64 * 3125) / 80_000) as i32;

        // VBUS LSB = 195.3125 µV → mV = raw * 1953125 / 10000000.
        let vin_mv = ((bus20 as u64 * 1_953_125) / 10_000_000) as u32;

        // DIETEMP LSB = 7.8125 m°C → °C = raw * 78125 / 10000000. Signed i16
        // input: do the multiply in i64 so a corrupt/full-scale read cannot
        // overflow i32 (which would panic in a debug build).
        let die_temp_c = (((raw_temp as i16) as i64 * 78_125) / 10_000_000) as i32;

        if iin_ma.unsigned_abs() > board::INA228_MAX_CURRENT_MA * 2 {
            return None;
        }

        let pin_mw = ((vin_mv as i64 * iin_ma as i64) / 1000) as i32;

        Some(Ina228Reading {
            vin_mv,
            iin_ma,
            pin_mw,
            die_temp_c,
            shunt_uv,
        })
    }

    /// Read the latched diagnostic/alert flags (see the INA228 datasheet for
    /// the bit map). Reading the register clears the latched flags.
    pub async fn read_diag(&self, i2c: &mut I2c<'_, Async, Master>) -> Option<u16> {
        self.read_reg16(i2c, INA228_REG_DIAG_ALRT).await
    }

    async fn read_reg16(&self, i2c: &mut I2c<'_, Async, Master>, reg: u8) -> Option<u16> {
        let mut buf = [0u8; 2];
        match i2c.write_read(self.addr, &[reg], &mut buf).await {
            Ok(()) => Some(u16::from_be_bytes(buf)),
            Err(e) => {
                crate::drivers::note_i2c_error(&crate::drivers::I2C_PD_BUS_ERRORS, "1 (PD)", e);
                None
            }
        }
    }

    async fn read_reg24(&self, i2c: &mut I2c<'_, Async, Master>, reg: u8) -> Option<u32> {
        let mut buf = [0u8; 3];
        match i2c.write_read(self.addr, &[reg], &mut buf).await {
            Ok(()) => Some(((buf[0] as u32) << 16) | ((buf[1] as u32) << 8) | buf[2] as u32),
            Err(e) => {
                crate::drivers::note_i2c_error(&crate::drivers::I2C_PD_BUS_ERRORS, "1 (PD)", e);
                None
            }
        }
    }

    async fn write_reg16(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
        reg: u8,
        value: u16,
    ) -> bool {
        let bytes = value.to_be_bytes();
        match i2c.write(self.addr, &[reg, bytes[0], bytes[1]]).await {
            Ok(()) => true,
            Err(e) => {
                crate::drivers::note_i2c_error(&crate::drivers::I2C_PD_BUS_ERRORS, "1 (PD)", e);
                false
            }
        }
    }
}
