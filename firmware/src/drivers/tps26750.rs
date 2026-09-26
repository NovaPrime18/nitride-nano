//! TI TPS26750 USB PD controller — Unique Address I2C protocol.
//! Ported from `src/tps26750.cpp`.

use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;
use embassy_time::{Duration, Timer};

// Register addresses in the TPS26750 "Unique Address" I2C interface.
// Only the subset the firmware actually exercises is kept active below.
pub const TPS_REG_MODE: u8 = 0x03;
pub const TPS_REG_CMD1: u8 = 0x08;
// TODO(dead-code): DATA1 (0x09) is the command data register in the TI register
// map, but no command used here carries a data payload, so it is never addressed.
// pub const TPS_REG_DATA1: u8 = 0x09;
pub const TPS_REG_INT_EVENT1: u8 = 0x14;
/// Write-only companion to `INT_EVENT1`: bits written as 1 are cleared. The IRQ
/// pin stays low while any event bit is set, so a read alone does **not** clear
/// it — see [`Tps26750::clear_interrupts`].
pub const TPS_REG_INT_CLEAR1: u8 = 0x18;
// TODO(dead-code): STATUS (0x1A) polling was dropped in favour of the IRQ line on
// PB13; kept for register-map reference.
// pub const TPS_REG_STATUS: u8 = 0x1A;
pub const TPS_REG_RX_SOURCE_CAPS: u8 = 0x30;
/// `POWER_PATH_STATUS` (0x26) — which internal/external power path switch is
/// actually closed. Bits 14-12 = PP3 (PP_EXT) state, bits 8-6 = PP1 (PP5V);
/// `2h`/`3h` mean enabled, `1h` means disabled due to fault. Diagnostics.
pub const TPS_REG_POWER_PATH_STATUS: u8 = 0x26;
/// `TX_SINK_CAPS` — the sink capabilities loaded from the application config.
/// Read back only for the boot diagnostic that proves which image is loaded.
pub const TPS_REG_TX_SINK_CAPS: u8 = 0x33;
pub const TPS_REG_ACTIVE_CONTRACT_PDO: u8 = 0x34;
pub const TPS_REG_ACTIVE_CONTRACT_RDO: u8 = 0x35;
pub const TPS_REG_AUTONEGOTIATE_SINK: u8 = 0x37;
// TODO(dead-code): PD3_STATUS (0x41) is unused — EPR/SPR detection is done by
// decoding the PDO type bits in `parse_pdo` instead.
// pub const TPS_REG_PD3_STATUS: u8 = 0x41;

pub const TPS_CMD_GSRC: &[u8; 4] = b"GSrC";
/// `ESrC` — "EPR Get Source Capabilities". Instructs the controller to run the
/// EPR mode-entry handshake and fetch the source's EPR PDOs. A source's EPR
/// PDOs are invisible in `RX_SOURCE_CAPS` (0x30) until this succeeds, which is
/// why the reference PD240W firmware probes with it before an EPR request.
pub const TPS_CMD_ESRC: &[u8; 4] = b"ESrC";
/// `GPsh` — force a GPIO output high regardless of its event mapping. Takes the
/// GPIO number in `DATA1` byte 0. Used to hold GPIO6 (`/LV-Supply/3V3_EN_PD`,
/// the board's 3V3-buck enable) asserted through an EPR transition.
pub const TPS_CMD_GPSH: &[u8; 4] = b"GPsh";
/// `DATA1` — command payload register (byte 0 is the first argument byte).
pub const TPS_REG_DATA1: u8 = 0x09;
pub const TPS_INT_NEW_CONTRACT_AS_SINK: u8 = 12;
/// `POWER_PATH_SWITCH` — set whenever `POWER_PATH_STATUS` (0x26) changes, i.e.
/// when the controller moves between PP5V and the external high-voltage path.
pub const TPS_INT_POWER_PATH_SWITCH: u8 = 23;
/// `SOURCE_CAP_RX` fires when the source (re)advertises its capabilities; the
/// controller raises it on EPR mode entry/exit, which is when the EPR PDOs
/// become visible.
pub const TPS_INT_SOURCE_CAP_RX: u8 = 14;
pub const TPS_INT_PLUG_INSERT_REMOVAL: u8 = 3;

const TASK_WAIT_TIMEOUT_MS: u64 = 1500;

const PDO_TYPE_SHIFT: u8 = 30;
const PDO_TYPE_MASK: u32 = 0x03;
const PDO_TYPE_FIXED: u8 = 0;
const PDO_TYPE_AUGMENTED: u8 = 3;
const APDO_TYPE_SHIFT: u8 = 28;
const APDO_TYPE_MASK: u32 = 0x03;
const APDO_TYPE_PPS: u8 = 0;
const APDO_TYPE_EPR_AVS: u8 = 1;
const APDO_TYPE_SPR_AVS: u8 = 2;
const FIXED_PDO_VOLTAGE_SHIFT: u8 = 10;
const FIXED_PDO_VOLTAGE_MASK: u32 = 0x3FF;
const FIXED_PDO_VOLTAGE_UNIT_MV: u32 = 50;
const FIXED_PDO_CURRENT_MASK: u32 = 0x3FF;
const FIXED_PDO_CURRENT_UNIT_MA: u32 = 10;
const PPS_RDO_VOLTAGE_SHIFT: u8 = 9;
const PPS_RDO_VOLTAGE_MASK: u32 = 0xFFF;
const PPS_RDO_VOLTAGE_UNIT_MV: u32 = 20;
const AVS_RDO_VOLTAGE_SHIFT: u8 = 9;
const AVS_RDO_VOLTAGE_MASK: u32 = 0x7FF;
const AVS_RDO_VOLTAGE_UNIT_MV: u32 = 25;
const APDO_RDO_CURRENT_MASK: u32 = 0x7F;
const APDO_RDO_CURRENT_UNIT_MA: u32 = 50;
const EPR_AVS_PDO_MAX_VOLTAGE_SHIFT: u8 = 17;
const EPR_AVS_PDO_MAX_VOLTAGE_MASK: u32 = 0x1FF;
const PROGRAMMABLE_PDO_MIN_VOLTAGE_SHIFT: u8 = 8;
const PROGRAMMABLE_PDO_MIN_VOLTAGE_MASK: u32 = 0xFF;
const PROGRAMMABLE_PDO_VOLTAGE_UNIT_MV: u32 = 100;
const EPR_AVS_PDO_PDP_MASK_W: u32 = 0xFF;
const SPR_AVS_9_15_CURRENT_SHIFT: u8 = 10;
const SPR_AVS_CURRENT_MASK: u32 = 0x3FF;
const SPR_AVS_CURRENT_UNIT_MA: u32 = 10;
const PPS_PDO_MAX_VOLTAGE_SHIFT: u8 = 17;
const PPS_PDO_MAX_VOLTAGE_MASK: u32 = 0xFF;
const PPS_PDO_CURRENT_MASK: u32 = 0x7F;
const PPS_PDO_CURRENT_UNIT_MA: u32 = 50;
const SPR_PDO_COUNT_MASK: u8 = 0x07;
const EPR_PDO_COUNT_SHIFT: u8 = 3;
const EPR_PDO_COUNT_MASK: u8 = 0x07;
const SPR_PDO_START_OFFSET: u8 = 1;
const EPR_PDO_START_OFFSET: u8 = 29;
const PDO_BYTES: u8 = 4;
const PPS_REQUEST_STEP_MV: u32 = 20;
/// AVS voltage step (PD spec: programmable AVS requests are aligned to 25 mV).
pub const AVS_REQUEST_STEP_MV: u32 = 25;

/// One parsed source PDO from the PD source, normalised to millivolt/milliamp.
///
/// `max_current_9_15_ma` is only meaningful for SPR AVS PDOs (the 9–15 V current
/// rating); it is parsed for completeness but currently not consumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceCapability {
    pub voltage_mv: u32,
    pub max_current_ma: u32,
    pub is_pps: bool,
    pub is_avs: bool,
    pub min_voltage_mv: u32,
    pub max_current_9_15_ma: u32,
}

impl SourceCapability {
    pub const EMPTY: Self = Self {
        voltage_mv: 0,
        max_current_ma: 0,
        is_pps: false,
        is_avs: false,
        min_voltage_mv: 0,
        max_current_9_15_ma: 0,
    };
}

/// Handle for the TPS26750 at a fixed 7-bit I2C address.
pub struct Tps26750 {
    addr: u8,
}

impl Tps26750 {
    pub fn new(addr: u8) -> Self {
        Self { addr }
    }

    /// Probe the device by reading the MODE register; returns true when the chip
    /// ACKs and returns a non-empty register payload.
    pub async fn init(&self, i2c: &mut I2c<'_, Async, Master>) -> bool {
        let mut mode = [0u8; 4];
        self.read_register(i2c, TPS_REG_MODE, &mut mode).await
    }

    pub async fn read_register(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
        reg: u8,
        dest: &mut [u8],
    ) -> bool {
        if dest.is_empty() {
            return false;
        }
        let len = dest.len();
        let mut temp = [0u8; 64];
        if len + 1 > temp.len() {
            return false;
        }
        // The TPS26750 Unique Address interface is an SMBus block read: send the
        // register index, then a *repeated start* for the data. A separate
        // write + read (which inserts a STOP between the two phases) is off the
        // protocol and leaves the device's register pointer implicit, so use
        // `write_read` for the whole exchange.
        if let Err(e) = i2c
            .write_read(self.addr, &[reg], &mut temp[..len + 1])
            .await
        {
            crate::drivers::note_i2c_error(&crate::drivers::I2C_PD_BUS_ERRORS, "1 (PD)", e);
            return false;
        }
        let count = temp[0];
        if count == 0 {
            return false;
        }
        let copy = count.min(len as u8) as usize;
        dest[..copy].copy_from_slice(&temp[1..1 + copy]);
        true
    }

    pub async fn write_register(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
        reg: u8,
        src: &[u8],
    ) -> bool {
        let mut buf = [0u8; 66];
        let n = src.len();
        if n + 2 > buf.len() {
            return false;
        }
        buf[0] = reg;
        buf[1] = n as u8;
        buf[2..2 + n].copy_from_slice(src);
        match i2c.write(self.addr, &buf[..n + 2]).await {
            Ok(()) => true,
            Err(e) => {
                crate::drivers::note_i2c_error(&crate::drivers::I2C_PD_BUS_ERRORS, "1 (PD)", e);
                false
            }
        }
    }

    pub async fn send_command(&self, i2c: &mut I2c<'_, Async, Master>, cmd: &[u8; 4]) -> bool {
        self.write_register(i2c, TPS_REG_CMD1, cmd).await
    }

    /// Poll CMD1 until the app firmware clears it (command accepted) or
    /// `TASK_WAIT_TIMEOUT_MS` elapses.
    pub async fn wait_for_command_clear(&self, i2c: &mut I2c<'_, Async, Master>) -> bool {
        let start = embassy_time::Instant::now();
        let mut cmd = [0u8; 4];
        loop {
            if !self.read_register(i2c, TPS_REG_CMD1, &mut cmd).await {
                return false;
            }
            if cmd == [0, 0, 0, 0] {
                return true;
            }
            if start.elapsed() > Duration::from_millis(TASK_WAIT_TIMEOUT_MS) {
                return false;
            }
            Timer::after(Duration::from_millis(10)).await;
        }
    }

    /// The active contract's RDO (register `0x35`).
    ///
    /// RDO bit 22 is the USB-PD 3.1 "EPR Mode Capable" bit: the Sink sets it in
    /// a Request when it intends to enter EPR mode, and the Source checks it
    /// (spec §6.4.10.1 step 2a) before acknowledging `EPR_Mode(Enter)`. If it is
    /// clear, the controller is not attempting EPR at all. Diagnostics only.
    pub async fn active_rdo(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
    ) -> Option<u32> {
        let mut buf = [0u8; 4];
        if !self
            .read_register(i2c, TPS_REG_ACTIVE_CONTRACT_RDO, &mut buf)
            .await
        {
            return None;
        }
        Some(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]))
    }

    /// Decode the active PDO/RDO pair into `(voltage_mv, max_current_ma)`.
    /// Returns `None` when no contract is active or the read fails.
    pub async fn get_active_contract(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
    ) -> Option<(u32, u32)> {
        let mut pdo_buf = [0u8; 6];
        let mut rdo_buf = [0u8; 4];
        if !self
            .read_register(i2c, TPS_REG_ACTIVE_CONTRACT_PDO, &mut pdo_buf)
            .await
        {
            return None;
        }
        if !self
            .read_register(i2c, TPS_REG_ACTIVE_CONTRACT_RDO, &mut rdo_buf)
            .await
        {
            return None;
        }
        let pdo = read_le32(&pdo_buf);
        let rdo = read_le32(&rdo_buf);
        let supply_type = extract_bits(pdo, PDO_TYPE_SHIFT, PDO_TYPE_MASK) as u8;
        let (v, i) = if supply_type == PDO_TYPE_AUGMENTED {
            let apdo = extract_bits(pdo, APDO_TYPE_SHIFT, APDO_TYPE_MASK) as u8;
            if apdo == APDO_TYPE_EPR_AVS || apdo == APDO_TYPE_SPR_AVS {
                (
                    extract_bits(rdo, AVS_RDO_VOLTAGE_SHIFT, AVS_RDO_VOLTAGE_MASK)
                        * AVS_RDO_VOLTAGE_UNIT_MV,
                    extract_bits(rdo, 0, APDO_RDO_CURRENT_MASK) * APDO_RDO_CURRENT_UNIT_MA,
                )
            } else if apdo == APDO_TYPE_PPS {
                (
                    extract_bits(rdo, PPS_RDO_VOLTAGE_SHIFT, PPS_RDO_VOLTAGE_MASK)
                        * PPS_RDO_VOLTAGE_UNIT_MV,
                    extract_bits(rdo, 0, APDO_RDO_CURRENT_MASK) * APDO_RDO_CURRENT_UNIT_MA,
                )
            } else {
                return None;
            }
        } else {
            (
                extract_bits(pdo, FIXED_PDO_VOLTAGE_SHIFT, FIXED_PDO_VOLTAGE_MASK)
                    * FIXED_PDO_VOLTAGE_UNIT_MV,
                extract_bits(pdo, 0, FIXED_PDO_CURRENT_MASK) * FIXED_PDO_CURRENT_UNIT_MA,
            )
        };

        // A real contract always has a non-zero voltage and current. When the
        // controller holds no contract (e.g. the board is running from the XT90
        // feed) the ACTIVE_CONTRACT registers read back as zero, which must NOT
        // be decoded as a 0 V / 0 A contract — that would drive the output
        // power cap to zero.
        if v == 0 || i == 0 {
            return None;
        }
        Some((v, i))
    }

    /// Read and parse the source-capabilities PDOs advertised by the attached
    /// source. Returns how many entries of `caps` were filled.
    pub async fn get_source_capabilities(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
        caps: &mut [SourceCapability],
    ) -> u8 {
        // Register layout: 1 header byte + 7 SPR PDOs (bytes 1..28) + 6 EPR PDOs
        // (bytes 29..52) = 53 bytes. The EPR count field is 3 bits, so a source
        // *can* report 7, but the register only holds 6 — the loop below skips a
        // PDO whose 4 bytes would fall outside the buffer. Do NOT enlarge this:
        // reading past the register length upsets the TPS26750's block transfer.
        let mut raw = [0u8; 53];
        if !self
            .read_register(i2c, TPS_REG_RX_SOURCE_CAPS, &mut raw)
            .await
        {
            return 0;
        }
        let num_spr = raw[0] & SPR_PDO_COUNT_MASK;
        let num_epr = (raw[0] >> EPR_PDO_COUNT_SHIFT) & EPR_PDO_COUNT_MASK;
        let total = num_spr + num_epr;
        let to_parse = total.min(caps.len() as u8);
        let mut valid = 0u8;
        for i in 0..to_parse {
            let offset = if i < num_spr {
                SPR_PDO_START_OFFSET + i * PDO_BYTES
            } else {
                EPR_PDO_START_OFFSET + (i - num_spr) * PDO_BYTES
            };
            // Defensive: never index past the buffer even if the device reports
            // a malformed PDO count.
            let start = offset as usize;
            if start + PDO_BYTES as usize > raw.len() {
                break;
            }
            let pdo = read_le32(&raw[start..]);
            if let Some(temp) = parse_pdo(pdo) {
                if temp.voltage_mv > 0 && temp.max_current_ma > 0 {
                    caps[valid as usize] = temp;
                    valid += 1;
                }
            }
        }
        valid
    }

    /// Read just the `RX_SOURCE_CAPS` header as `(num SPR, num EPR, last caps
    /// EPR)`. Diagnostics only: the EPR pair going non-zero is what proves the
    /// controller actually entered EPR mode. `None` on a bus error.
    pub async fn source_cap_header(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
    ) -> Option<(u8, u8, bool)> {
        let mut hdr = [0u8; 1];
        if !self
            .read_register(i2c, TPS_REG_RX_SOURCE_CAPS, &mut hdr)
            .await
        {
            return None;
        }
        Some((
            hdr[0] & SPR_PDO_COUNT_MASK,
            (hdr[0] >> EPR_PDO_COUNT_SHIFT) & EPR_PDO_COUNT_MASK,
            hdr[0] & 0x40 != 0,
        ))
    }

    /// The source's first (5 V) Fixed Supply PDO.
    ///
    /// Source Fixed Supply PDO bit 23 is the USB-PD 3.1 "EPR Mode Capable" bit
    /// (Table 6-9). If it is clear the source is not offering EPR and the sink
    /// can never enter EPR mode, no matter what the host requests. Diagnostics
    /// only; `None` on a bus error.
    pub async fn source_5v_pdo(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
    ) -> Option<u32> {
        let mut buf = [0u8; 5];
        if !self
            .read_register(i2c, TPS_REG_RX_SOURCE_CAPS, &mut buf)
            .await
        {
            return None;
        }
        Some(u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]))
    }

    pub async fn request_fixed_profile(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
        voltage_mv: u32,
        max_current_ma: u32,
    ) -> bool {
        let min_v = voltage_mv - voltage_mv / 20;
        let max_v = voltage_mv + voltage_mv / 20;
        self.modify_sink_register(
            i2c, min_v, max_v, max_current_ma, 0, 0, false, 0, 0, false, false, true,
        )
        .await
    }

    /// Target a **specific fixed EPR PDO** with a host window whose upper bound
    /// is `voltage_mv + 5 %` and `NoCapabilityMismatch` left **set**.
    ///
    /// Only used for sources with no EPR AVS APDO (see
    /// [`Self::request_avs_profile`], which is the path for AVS-capable
    /// sources). The caller triggers re-negotiation with `GSrC`.
    ///
    /// This replaces the old ≥140 W "EPR force" window. That request cleared
    /// `AutoComputeSinkMaxVoltage` *and* `EPR AVS Enable Sink Mode`, which TI
    /// documents (SDAA265 §4.1/§5.2) as the controller never accepting more than
    /// the highest SPR PDO — 20 V — and it cleared `NoCapabilityMismatch`, which
    /// made every SPR contract a mismatch contract.
    ///
    /// The window floor is 5 V, **not** `target ±5 %`: field-verified that a
    /// window which excludes the whole SPR set (e.g. `[26.6, 29.4] V`) makes the
    /// controller fall back to 5 V instead of escalating. With an SPR PDO inside
    /// the window the controller ranks by power, so 28 V/5 A still beats
    /// 20 V/5 A and the target wins. EPR mode entry itself is driven by the
    /// `ESrC` probe in [`crate::pd::manager::PdManager::poll`]; a fixed >20 V
    /// window alone does not enter EPR.
    pub async fn request_fixed_epr_rail(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
        voltage_mv: u32,
        max_current_ma: u32,
    ) -> bool {
        let max_v = voltage_mv + voltage_mv / 20;
        self.modify_sink_register(
            i2c,
            5_000,
            max_v,
            max_current_ma,
            0,
            0,
            false, // PPS off (it outranks EPR)
            0,
            0,
            false, // fixed EPR PDO, not AVS
            false, // host window; NoCapabilityMismatch stays set
            false, // no PPS edge: an EPR request re-evaluates with ESrC
        )
        .await
    }

    /// Hand voltage selection back to the controller so its **own**
    /// autonegotiation reproduces the power-up (EEPROM) behaviour.
    ///
    /// This is what a "MAX" request now issues. It deliberately writes **no**
    /// host voltage window: clearing `AutoComputeSinkMaxVoltage` (bit 5) or
    /// `EPR AVS Enable Sink Mode` (bit 128) is what makes the controller stop at
    /// the highest SPR PDO — 20 V — even on an EPR-capable source (SDAA265 §4.1
    /// and §5.2). PPS is left **off** because a PPS APDO outranks EPR and caps
    /// at 21 V (TRM §6.3). With the EEPROM policy restored and `GSrC` issued, the
    /// controller re-negotiates and picks the highest-power PDO the source
    /// offers, exactly as it did at power-up (TRM §2.3).
    ///
    /// Only reached on sources with no EPR AVS APDO; AVS-capable sources go
    /// through [`Self::request_avs_profile`].
    pub async fn restore_autonegotiate(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
        max_current_ma: u32,
    ) -> bool {
        self.modify_sink_register(
            i2c,
            5_000,
            51_000,
            max_current_ma,
            0,
            0,
            false, // PPS off (it outranks EPR and caps at 21 V)
            48_000,
            5_000,
            true, // EPR AVS enable: what makes the controller enter EPR
            true, // auto-compute the range -> the controller picks the PDO
            false, // no PPS edge: an EPR request re-evaluates with ESrC
        )
        .await
    }

    pub async fn request_pps_profile(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
        voltage_mv: u32,
        current_ma: u32,
        pdo_min_mv: u32,
        _pdo_max_mv: u32,
    ) -> bool {
        let std_max = if voltage_mv >= pdo_min_mv + PPS_REQUEST_STEP_MV {
            voltage_mv - PPS_REQUEST_STEP_MV
        } else {
            pdo_min_mv
        };
        self.modify_sink_register(
            i2c, pdo_min_mv, std_max, current_ma, voltage_mv, current_ma, true, 0, 0, false, false,
            false,
        )
        .await
    }

    /// Request an **EPR AVS** contract at `voltage_mv`.
    ///
    /// This keeps `AutoComputeSinkMaxVoltage` **set** (via `auto_epr`). That is
    /// the difference that matters on AVS-capable sources: the EEPROM policy
    /// (`b0 = 0x3e`, `avs_en = 1`, AVS output = 48 V) negotiates a 48 V EPR AVS
    /// contract, while every host window with auto-compute cleared settles at the
    /// highest SPR PDO (20 V) — SDAA265 §4.1/§5.2. The AVS output-voltage field
    /// is the requested rail; the controller still computes the legal range, so
    /// on a source that honours AVS this can resolve to the highest AVS rail
    /// rather than the exact target.
    pub async fn request_avs_profile(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
        voltage_mv: u32,
        current_ma: u32,
    ) -> bool {
        self.modify_sink_register(
            i2c,
            5_000,
            51_000,
            current_ma,
            0,
            0,
            false, // PPS off (it outranks EPR)
            voltage_mv,
            current_ma,
            true, // EPR AVS enable
            true, // auto-compute range: required for EPR entry/selection
            false, // no PPS edge: an EPR request re-evaluates with ESrC
        )
        .await
    }

    /// Issue the `GSrC` 4CC command so the PD controller re-runs the sink
    /// negotiation with the updated Autonegotiate Sink register.
    pub async fn trigger_renegotiation(&self, i2c: &mut I2c<'_, Async, Master>) -> bool {
        if !self.send_command(i2c, TPS_CMD_GSRC).await {
            return false;
        }
        self.wait_for_command_clear(i2c).await
    }

    /// Issue the `ESrC` (EPR Get Source Capabilities) 4CC task.
    ///
    /// The controller only attempts EPR mode entry when an EPR AVS contract is
    /// enabled (0x37 bit 128) or when its autonegotiation computes an EPR max
    /// voltage. `ESrC` is the explicit "enter EPR and read the EPR PDOs" task;
    /// until it succeeds, `RX_SOURCE_CAPS` (0x30) holds only SPR PDOs, so the
    /// chooser cannot see the source's 28/36/48 V offers.
    pub async fn request_epr_source_caps(&self, i2c: &mut I2c<'_, Async, Master>) -> bool {
        if !self.send_command(i2c, TPS_CMD_ESRC).await {
            return false;
        }
        self.wait_for_command_clear(i2c).await
    }

    /// 4CC `GPsh`: drive a TPS26750 GPIO output high.
    ///
    /// Writes the GPIO number to `DATA1` byte 0 then issues `GPsh`. Per the TRM
    /// this overrides a GPIO that is also mapped to an event, so treat it as a
    /// hold/override rather than a clean configuration. Used to keep GPIO6
    /// (`/LV-Supply/3V3_EN_PD` → `JP16` → U11 `~SHDN`) asserted if the
    /// controller de-asserts it across an EPR transition and browns the MCU out.
    pub async fn set_gpio_high(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
        gpio: u8,
    ) -> bool {
        if !self.write_register(i2c, TPS_REG_DATA1, &[gpio]).await {
            return false;
        }
        if !self.send_command(i2c, TPS_CMD_GPSH).await {
            return false;
        }
        self.wait_for_command_clear(i2c).await
    }

    async fn modify_sink_register(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
        min_v: u32,
        max_v: u32,
        op_i: u32,
        pps_v: u32,
        pps_i: u32,
        pps_en: bool,
        avs_v: u32,
        avs_i: u32,
        avs_en: bool,
        auto_epr: bool,
        edge: bool,
    ) -> bool {
        let mut buf = [0u8; 24];
        if !self
            .read_register(i2c, TPS_REG_AUTONEGOTIATE_SINK, &mut buf)
            .await
        {
            return false;
        }
        if auto_epr {
            // Hand voltage selection back to the controller. With
            // AutoComputeSinkMaxVoltage set it caps SPR at 20 V and then does the
            // EPR mode-entry handshake itself, which is the only path that has
            // been observed to work.
            buf[0] &= !(1 << 6);
            buf[0] |= (1 << 5) | (1 << 4) | (1 << 2) | (1 << 3);
        } else {
            buf[0] &= !((1 << 6) | (1 << 5) | (1 << 4) | (1 << 2));
            buf[0] |= 1 << 3;
        }
        buf[2] &= 0x3F;
        buf[3] = 0;
        buf[6] &= 0x0F;
        buf[7] &= 0xC0;

        let max_v_val = (max_v / 50) as u16;
        buf[4] = (max_v_val & 0xFF) as u8;
        buf[5] = (buf[5] & 0xFC) | (((max_v_val >> 8) & 0x03) as u8);
        let min_v_val = (min_v / 50) as u16;
        buf[5] = (buf[5] & 0x03) | (((min_v_val & 0x3F) << 2) as u8);
        buf[6] = (buf[6] & 0xF0) | (((min_v_val >> 6) & 0x0F) as u8);
        let max_i_val = (op_i / 10) as u16;
        buf[1] = (buf[1] & 0x0F) | (((max_i_val & 0x0F) << 4) as u8);
        buf[2] = (buf[2] & 0xC0) | (((max_i_val >> 4) & 0x3F) as u8);

        if pps_en {
            buf[8] |= 0x01;
        } else {
            buf[8] &= !0x01;
        }
        if pps_en {
            let pps_i_val = ((pps_i / 50) & 0x7F) as u8;
            buf[12] = (buf[12] & 0x80) | pps_i_val;
            let pps_v_val = (pps_v / 20) as u16;
            buf[13] = (buf[13] & 0x01) | (((pps_v_val & 0x7F) << 1) as u8);
            buf[14] = (buf[14] & 0xF0) | (((pps_v_val >> 7) & 0x0F) as u8);
        }
        if avs_en {
            buf[16] |= 0x01;
        } else {
            buf[16] &= !0x01;
        }
        if avs_en {
            let avs_i_val = ((avs_i / 50) & 0x7F) as u8;
            buf[20] = (buf[20] & 0x80) | avs_i_val;
            let avs_v_val = (avs_v / 25) as u16;
            buf[21] = (buf[21] & 0x01) | (((avs_v_val & 0x7F) << 1) as u8);
            buf[22] = (buf[22] & 0xE0) | (((avs_v_val >> 7) & 0x1F) as u8);
        }

        if !self
            .write_register(i2c, TPS_REG_AUTONEGOTIATE_SINK, &buf)
            .await
        {
            return false;
        }
        // The write alone does not re-negotiate: TRM §2.3 says the controller
        // prepares its Request from 0x37 + 0x33 and the host issues `GSrC` to
        // re-negotiate. The caller does that for EPR and PPS requests.
        //
        // `edge` additionally re-evaluates with a `PPSEnableSinkMode` toggle —
        // the reference PD240W firmware's trigger. It is ONLY safe for a plain
        // fixed request: toggling PPS briefly *enables* it, and PPS outranks EPR,
        // so on an EPR request it made the controller drop out of EPR and
        // recompute `AutoNegMaxVoltage` back to 20 V. Field-verified that some
        // sources (the Anker 737) ignore a fixed request re-evaluated only by
        // `GSrC`, while the 240 W accepts either.
        if edge {
            let mut toggle = buf;
            toggle[8] ^= 0x01;
            if !self
                .write_register(i2c, TPS_REG_AUTONEGOTIATE_SINK, &toggle)
                .await
            {
                return false;
            }
            Timer::after(Duration::from_millis(2)).await;
            self.write_register(i2c, TPS_REG_AUTONEGOTIATE_SINK, &buf)
                .await
        } else {
            true
        }
    }

    /// Test one bit in the 88-bit INT_EVENT1 bitmap; out-of-range indices are false.
    pub fn is_interrupt_set(buffer: &[u8], bit_index: u8) -> bool {
        if bit_index > 87 {
            return false;
        }
        let byte = bit_index / 8;
        let bit = bit_index % 8;
        buffer
            .get(byte as usize)
            .map(|b| (b & (1 << bit)) != 0)
            .unwrap_or(false)
    }

    /// Set one bit in an 11-byte interrupt mask for [`Self::clear_interrupts`].
    /// Out-of-range indices are ignored.
    pub fn set_interrupt_bit(mask: &mut [u8; 11], bit_index: u8) {
        if bit_index > 87 {
            return;
        }
        mask[(bit_index / 8) as usize] |= 1 << (bit_index % 8);
    }

    /// Read the latched interrupt events. Reading does **not** clear them; the
    /// caller must write the consumed bits back via [`Self::clear_interrupts`]
    /// or the IRQ line stays asserted.
    pub async fn read_interrupts(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
        events: &mut [u8; 11],
    ) -> bool {
        self.read_register(i2c, TPS_REG_INT_EVENT1, events).await
    }

    /// Clear the given bits in `INT_EVENT1` (1 = clear). Must be called after
    /// [`Self::read_interrupts`] for every bit that was acted on, otherwise the
    /// interrupt latches forever and the IRQ-driven poll re-fires each pass.
    pub async fn clear_interrupts(
        &self,
        i2c: &mut I2c<'_, Async, Master>,
        mask: &[u8; 11],
    ) -> bool {
        self.write_register(i2c, TPS_REG_INT_CLEAR1, mask).await
    }
}

fn extract_bits(value: u32, shift: u8, mask: u32) -> u32 {
    (value >> shift) & mask
}

fn read_le32(buf: &[u8]) -> u32 {
    u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])
}

/// Decode one 32-bit PDO (fixed, PPS, SPR AVS, or EPR AVS) into a
/// [`SourceCapability`]. Returns `None` for battery/variable PDOs and for
/// programmable PDOs with a degenerate (min >= max) voltage range.
fn parse_pdo(pdo: u32) -> Option<SourceCapability> {
    let mut temp = SourceCapability::EMPTY;
    let pdo_type = extract_bits(pdo, PDO_TYPE_SHIFT, PDO_TYPE_MASK) as u8;
    if pdo_type == PDO_TYPE_AUGMENTED {
        let apdo = extract_bits(pdo, APDO_TYPE_SHIFT, APDO_TYPE_MASK) as u8;
        match apdo {
            APDO_TYPE_EPR_AVS => {
                temp.is_avs = true;
                temp.voltage_mv = extract_bits(
                    pdo,
                    EPR_AVS_PDO_MAX_VOLTAGE_SHIFT,
                    EPR_AVS_PDO_MAX_VOLTAGE_MASK,
                ) * PROGRAMMABLE_PDO_VOLTAGE_UNIT_MV;
                temp.min_voltage_mv = extract_bits(
                    pdo,
                    PROGRAMMABLE_PDO_MIN_VOLTAGE_SHIFT,
                    PROGRAMMABLE_PDO_MIN_VOLTAGE_MASK,
                ) * PROGRAMMABLE_PDO_VOLTAGE_UNIT_MV;
                let pwr = extract_bits(pdo, 0, EPR_AVS_PDO_PDP_MASK_W);
                if temp.voltage_mv > 0 {
                    temp.max_current_ma = pwr * 1_000_000 / temp.voltage_mv;
                }
            }
            APDO_TYPE_SPR_AVS => {
                temp.is_avs = true;
                let i9 = extract_bits(pdo, SPR_AVS_9_15_CURRENT_SHIFT, SPR_AVS_CURRENT_MASK)
                    * SPR_AVS_CURRENT_UNIT_MA;
                let i15 = extract_bits(pdo, 0, SPR_AVS_CURRENT_MASK) * SPR_AVS_CURRENT_UNIT_MA;
                temp.min_voltage_mv = 9000;
                if i15 > 0 {
                    temp.voltage_mv = 20000;
                    temp.max_current_ma = i15;
                } else {
                    temp.voltage_mv = 15000;
                    temp.max_current_ma = i9;
                }
                temp.max_current_9_15_ma = i9;
            }
            APDO_TYPE_PPS => {
                temp.is_pps = true;
                temp.voltage_mv =
                    extract_bits(pdo, PPS_PDO_MAX_VOLTAGE_SHIFT, PPS_PDO_MAX_VOLTAGE_MASK)
                        * PROGRAMMABLE_PDO_VOLTAGE_UNIT_MV;
                temp.min_voltage_mv = extract_bits(
                    pdo,
                    PROGRAMMABLE_PDO_MIN_VOLTAGE_SHIFT,
                    PROGRAMMABLE_PDO_MIN_VOLTAGE_MASK,
                ) * PROGRAMMABLE_PDO_VOLTAGE_UNIT_MV;
                temp.max_current_ma =
                    extract_bits(pdo, 0, PPS_PDO_CURRENT_MASK) * PPS_PDO_CURRENT_UNIT_MA;
            }
            _ => return None,
        }
    } else if pdo_type == PDO_TYPE_FIXED {
        temp.voltage_mv = extract_bits(pdo, FIXED_PDO_VOLTAGE_SHIFT, FIXED_PDO_VOLTAGE_MASK)
            * FIXED_PDO_VOLTAGE_UNIT_MV;
        temp.max_current_ma =
            extract_bits(pdo, 0, FIXED_PDO_CURRENT_MASK) * FIXED_PDO_CURRENT_UNIT_MA;
    } else {
        // Battery (01b) and Variable (10b) PDOs reuse the same bit positions for
        // different fields (e.g. max power instead of max current), so decoding
        // them with the Fixed layout yields nonsense. This firmware only
        // requests fixed rails, so ignore them rather than mis-parse.
        return None;
    }
    if temp.is_pps || temp.is_avs {
        if temp.min_voltage_mv >= temp.voltage_mv {
            return None;
        }
    }
    Some(temp)
}
