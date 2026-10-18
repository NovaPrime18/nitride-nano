//! TI INA228 power monitor — plain SMBus register interface.
//!
//! rev3 fits **two** of these on the PD bus (I2C2: `SCL=PC4`, `SDA=PA8`):
//!
//! * **U9 at `0x40`** (`A0 = A1 = GND`) monitors the **converter input** bus: a
//!   2 mΩ shunt (R60) between the USB-PD side (`IN+`) and the converter input
//!   (`IN−`), with its VBUS pin on the converter side of that shunt.
//! * **U12 at `0x41`** (`A0 = GND`, `A1 = +3V3`) monitors the **output** bus: the
//!   2 mΩ return-path shunt (R96) between the output connector (`IN+`, fuse
//!   side) and the output switch Q5 (`IN−`), with VBUS on `Bus_V`.
//!
//! The two instances differ only in address and shunt value, so this driver is
//! parameterised over both and its reading fields are direction-neutral.
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
///
/// The field names are deliberately direction-neutral: the same struct serves
/// the input monitor (`bus_mv` = converter input voltage, `current_ma` positive
/// into the converter) and the output monitor (`bus_mv` = output bus voltage,
/// `current_ma` positive into the load).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ina228Reading {
    /// Bus voltage at the device's VBUS pin.
    pub bus_mv: u32,
    /// Shunt current; positive is `IN+` → `IN−` through the shunt.
    pub current_ma: i32,
    /// Power derived from `bus_mv * current_ma`.
    pub power_mw: i32,
    /// Die temperature in whole degrees Celsius.
    pub die_temp_c: i32,
    /// Raw shunt reading in microvolts (useful for calibration logs).
    pub shunt_uv: i32,
}

/// Handle for an INA228 at a fixed 7-bit I2C address and a known shunt.
pub struct Ina228 {
    addr: u8,
    shunt_mohm: u32,
    shunt_cal: u16,
}

impl Ina228 {
    /// Bind to an INA228 and the shunt it measures.
    ///
    /// `shunt_mohm` also sets `SHUNT_CAL`, which the chip needs before any
    /// current register is meaningful — the wrong value silently scales every
    /// current and power reading.
    pub const fn new(addr: u8, shunt_mohm: u32) -> Self {
        Self {
            addr,
            shunt_mohm,
            shunt_cal: board::ina228_shunt_cal(shunt_mohm),
        }
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
            .write_reg16(i2c, INA228_REG_SHUNT_CAL, self.shunt_cal)
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
                "INA228 OK: addr=0x{:02x}, shunt={} mOhm, shunt_cal={}, mfg=0x{:04x}, dev=0x{:04x}",
                self.addr,
                self.shunt_mohm,
                self.shunt_cal,
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

        // VSHUNT LSB = 312.5 nV → µV = raw * 3125 / 10_000.
        // I(mA) = Vshunt(µV) / R(mΩ), i.e. raw * 3125 / (10_000 * R).
        let shunt_uv = ((shunt20 as i64 * 3125) / 10_000) as i32;
        let denom = 10_000i64 * self.shunt_mohm as i64;
        let current_ma = ((shunt20 as i64 * 3125) / denom) as i32;

        // VBUS LSB = 195.3125 µV → mV = raw * 1953125 / 10000000.
        let bus_mv = ((bus20 as u64 * 1_953_125) / 10_000_000) as u32;

        // DIETEMP LSB = 7.8125 m°C → °C = raw * 78125 / 10000000. Signed i16
        // input: do the multiply in i64 so a corrupt/full-scale read cannot
        // overflow i32 (which would panic in a debug build).
        let die_temp_c = (((raw_temp as i16) as i64 * 78_125) / 10_000_000) as i32;

        if current_ma.unsigned_abs() > board::INA228_MAX_CURRENT_MA * 2 {
            return None;
        }

        let power_mw = ((bus_mv as i64 * current_ma as i64) / 1000) as i32;

        Some(Ina228Reading {
            bus_mv,
            current_ma,
            power_mw,
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
                crate::drivers::note_i2c_error(&crate::drivers::I2C_PD_BUS_ERRORS, "2 (PD)", e);
                None
            }
        }
    }

    async fn read_reg24(&self, i2c: &mut I2c<'_, Async, Master>, reg: u8) -> Option<u32> {
        let mut buf = [0u8; 3];
        match i2c.write_read(self.addr, &[reg], &mut buf).await {
            Ok(()) => Some(((buf[0] as u32) << 16) | ((buf[1] as u32) << 8) | buf[2] as u32),
            Err(e) => {
                crate::drivers::note_i2c_error(&crate::drivers::I2C_PD_BUS_ERRORS, "2 (PD)", e);
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
                crate::drivers::note_i2c_error(&crate::drivers::I2C_PD_BUS_ERRORS, "2 (PD)", e);
                false
            }
        }
    }
}
