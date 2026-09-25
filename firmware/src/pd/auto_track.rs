//! Auto-tracking PD rail selection.
//!
//! The LT8390A runs in a 4-switch buck-boost region whenever `VIN` and `VOUT`
//! are close (peak-buck/peak-boost cross at `VIN/VOUT` ≈ 0.98–1.04; the
//! buck-boost band spans roughly 0.75–1.33). That region switches all four
//! FETs and is the least efficient way to move power, so this module picks a
//! fixed USB-PD rail that keeps the converter in a clean buck or clean boost
//! region whenever the requested output power allows it.
//!
//! Two classes of rail need special handling:
//!
//! * **EPR (28/36/48 V).** A source's EPR PDOs do not appear in
//!   `RX_SOURCE_CAPS` until EPR mode has been entered. The fixed EPR rails the
//!   sink configuration declares are therefore injected as candidates whenever
//!   the output setpoint cannot be served from SPR alone, and any above-SPR rail
//!   is requested as an **EPR AVS** contract inside the declared AVS window:
//!   asserting `EPR AVS Enable Sink Mode` is what makes the controller enter EPR
//!   (a fixed >20 V window instead falls back to 5 V, SDAA265 §5.3).
//! * **PPS.** A manual preset no fixed PDO matches (notably 12 V) is served by
//!   a PPS contract drawn from the source's PPS APDO. PPS is never enabled for
//!   an EPR rail: a PPS APDO outranks EPR and would pull the contract back to
//!   ≤21 V (TRM 6.3).
//!
//! The logic here is deliberately free of embassy dependencies so it stays
//! readable and easy to reason about independently of the HAL; it only consumes
//! parsed source PDOs and the board tuning constants.

use crate::board;
use crate::drivers::tps26750::SourceCapability;
use crate::state::{AutoPolicy, PD_PRESET_VOLTAGES_MV, RailRegion};

/// Candidate buffer size: the 13 PDOs the source-capabilities register can hold
/// plus the injected EPR rails.
pub const MAX_CANDIDATES: usize = 16;

/// The voltage window of the APDO (PPS or EPR AVS) a programmable contract is
/// drawn from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApdoWindow {
    pub min_mv: u32,
    pub max_mv: u32,
}

/// The rail Auto-tracking would request, with the region it should produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoChoice {
    pub rail_mv: u32,
    pub current_ma: u32,
    /// Index into the candidate slice the choice came from (0 for PPS).
    pub index: u8,
    pub region: RailRegion,
    /// `Some` when the request must be issued as a PPS contract at `rail_mv`
    /// instead of a fixed PDO.
    pub pps: Option<ApdoWindow>,
    /// `Some` when the request must be issued as an **EPR AVS** contract at
    /// `rail_mv`. Before EPR entry this carries the declared AVS window, which is
    /// what makes the controller enter EPR (TRM 0x37 bit 128). After entry it
    /// carries the source's own AVS APDO window, or is cleared to `None` when the
    /// source has no AVS APDO.
    pub avs: Option<ApdoWindow>,
    /// True for an above-SPR rail. The request must keep EPR mode enabled: a
    /// fixed EPR PDO has to be requested with `avs_en` still asserted, because
    /// writing 0x37 with it clear drops the controller back to SPR and the EPR
    /// PDO disappears (field-verified).
    pub epr: bool,
    /// True for a "MAX" request: ask for the highest rail the source offers
    /// rather than a specific rail. Above-SPR this uses the controller-computed
    /// selection over a wide window, so it lands on the highest-power EPR PDO.
    pub maximize: bool,
}

/// EPR AVS window for an above-SPR rail, or `None` for an SPR rail that is
/// requested as a fixed PDO.
fn avs_window(voltage_mv: u32) -> Option<ApdoWindow> {
    if voltage_mv > board::SPR_MAX_MV {
        Some(ApdoWindow {
            min_mv: board::EPR_AVS_MIN_MV,
            max_mv: board::EPR_AVS_MAX_MV,
        })
    } else {
        None
    }
}

const fn epr_rail(voltage_mv: u32) -> SourceCapability {
    SourceCapability {
        voltage_mv,
        max_current_ma: board::EPR_RAIL_CURRENT_MA,
        is_pps: false,
        is_avs: false,
        min_voltage_mv: 0,
        max_current_9_15_ma: 0,
    }
}

/// Fixed EPR rails the TPS26750 sink configuration declares. They are injected
/// as candidates because the source's own EPR PDOs are invisible until EPR
/// mode entry succeeds.
const EPR_CANDIDATES: [SourceCapability; board::EPR_RAILS_MV.len()] = [
    epr_rail(board::EPR_RAILS_MV[0]),
    epr_rail(board::EPR_RAILS_MV[1]),
    epr_rail(board::EPR_RAILS_MV[2]),
];

/// Delivered power a rail can supply, in mW.
fn rail_mw(cap: &SourceCapability) -> u64 {
    cap.voltage_mv as u64 * cap.max_current_ma as u64 / 1000
}

/// The rail's operating region for a given output setpoint, assuming it is
/// eligible (outside the 4-switch band).
fn classify(cap: &SourceCapability, v_set_mv: u32) -> RailRegion {
    let v100 = cap.voltage_mv as u64 * 100;
    let target = v_set_mv as u64;
    if v100 >= target * board::AUTO_TRACK_BUCK_MIN_RATIO_PCT as u64 {
        RailRegion::Buck
    } else if v100 <= target * board::AUTO_TRACK_BOOST_MAX_RATIO_PCT as u64 {
        RailRegion::Boost
    } else {
        RailRegion::FallbackPower
    }
}

fn current_ma(cap: &SourceCapability) -> u32 {
    cap.max_current_ma.min(board::IOUT_MAX_MA)
}

/// True for the fixed supply PDOs Auto-tracking may request directly.
fn is_fixed(cap: &SourceCapability) -> bool {
    !cap.is_pps && !cap.is_avs && cap.voltage_mv > 0 && cap.max_current_ma > 0
}

/// Highest power an advertised fixed SPR PDO can deliver.
fn best_spr_power_mw(caps: &[SourceCapability]) -> u64 {
    caps.iter()
        .filter(|c| is_fixed(c) && c.voltage_mv <= board::SPR_MAX_MV)
        .map(rail_mw)
        .max()
        .unwrap_or(0)
}

/// True when only an EPR contract can serve this setpoint: the output setpoint
/// is above the SPR ceiling, or the advertised SPR rails cannot deliver the
/// requested power at all. Deliberately ignores the efficiency headroom — a
/// rail that only meets the raw power is still a valid SPR choice, and
/// escalating to EPR costs a failed attempt on non-EPR sources.
fn needs_epr(v_set_mv: u32, i_set_ma: u32, caps: &[SourceCapability]) -> bool {
    if v_set_mv > board::SPR_MAX_MV {
        return true;
    }
    let requested_mw = v_set_mv as u64 * i_set_ma as u64 / 1000;
    best_spr_power_mw(caps) < requested_mw
}

/// Copy the advertised PDOs into `buf`, appending the declared EPR rails when
/// they are needed and the source has not advertised EPR PDOs yet. Returns the
/// filled prefix of `buf`.
fn candidates<'a>(
    caps: &[SourceCapability],
    v_set_mv: u32,
    i_set_ma: u32,
    allow_epr: bool,
    buf: &'a mut [SourceCapability; MAX_CANDIDATES],
) -> &'a [SourceCapability] {
    let mut n = 0;
    for cap in caps {
        if n == buf.len() {
            break;
        }
        buf[n] = *cap;
        n += 1;
    }
    // Only a *fixed* PDO above the SPR ceiling means the source has advertised
    // real EPR rails. A PPS/AVS APDO carries its maximum voltage in
    // `voltage_mv`, and PPS reaches 21 V, so testing `voltage_mv` alone
    // misreads any PPS-capable source as EPR-capable and suppresses injection —
    // which is exactly how a 28 V preset ended up requesting 20 V.
    let advertised_epr = caps
        .iter()
        .any(|c| is_fixed(c) && c.voltage_mv > board::SPR_MAX_MV);
    if allow_epr && !advertised_epr && needs_epr(v_set_mv, i_set_ma, caps) {
        for cap in EPR_CANDIDATES {
            if n == buf.len() {
                break;
            }
            buf[n] = cap;
            n += 1;
        }
    }
    &buf[..n]
}

/// Choose a rail for Auto-tracking PD.
///
/// `v_set_mv` / `i_set_ma` are the output setpoints; `caps` is the parsed
/// source-capability list. `allow_epr` is false once an EPR request has been
/// seen to settle back at an SPR voltage, so the chooser stops re-requesting a
/// rail the source cannot provide. Returns `None` when no rail is available.
pub fn choose_rail(
    v_set_mv: u32,
    i_set_ma: u32,
    caps: &[SourceCapability],
    policy: AutoPolicy,
    allow_epr: bool,
) -> Option<AutoChoice> {
    if v_set_mv == 0 {
        return None;
    }
    let mut buf = [SourceCapability::EMPTY; MAX_CANDIDATES];
    let caps = candidates(caps, v_set_mv, i_set_ma, allow_epr, &mut buf);
    choose_rail_over(v_set_mv, i_set_ma, caps, policy)
}

fn choose_rail_over(
    v_set_mv: u32,
    i_set_ma: u32,
    caps: &[SourceCapability],
    policy: AutoPolicy,
) -> Option<AutoChoice> {
    let requested_mw = v_set_mv as u64 * i_set_ma as u64 / 1000;
    let need_mw = requested_mw * board::AUTO_TRACK_POWER_HEADROOM_PCT as u64 / 100;

    // Most capable fixed rail, used for the power policy and the efficiency
    // policy's fallback.
    let mut best_power: Option<(usize, u64)> = None;
    // Efficiency preferences: gentlest step-down (lowest eligible buck rail)
    // and gentlest step-up (highest eligible boost rail) that still meet power.
    let mut best_buck: Option<usize> = None;
    let mut best_boost: Option<usize> = None;

    for (i, cap) in caps.iter().enumerate() {
        if !is_fixed(cap) {
            continue;
        }

        let mw = rail_mw(cap);
        if best_power
            .map(|(bi, bmw)| {
                let b = &caps[bi];
                mw > bmw
                    || (mw == bmw
                        && (cap.max_current_ma > b.max_current_ma
                            || (cap.max_current_ma == b.max_current_ma
                                && cap.voltage_mv < b.voltage_mv)))
            })
            .unwrap_or(true)
        {
            best_power = Some((i, mw));
        }

        if policy == AutoPolicy::Efficiency && mw >= need_mw {
            match classify(cap, v_set_mv) {
                RailRegion::Buck
                    if best_buck.is_none_or(|bi| cap.voltage_mv < caps[bi].voltage_mv) =>
                {
                    best_buck = Some(i);
                }
                RailRegion::Boost
                    if best_boost.is_none_or(|bi| cap.voltage_mv > caps[bi].voltage_mv) =>
                {
                    best_boost = Some(i);
                }
                _ => {}
            }
        }
    }

    if policy == AutoPolicy::Efficiency {
        if let Some(i) = best_buck.or(best_boost) {
            let cap = &caps[i];
            return Some(AutoChoice {
                rail_mv: cap.voltage_mv,
                current_ma: current_ma(cap),
                index: i as u8,
                region: classify(cap, v_set_mv),
                pps: None,
                avs: avs_window(cap.voltage_mv),
                epr: cap.voltage_mv > board::SPR_MAX_MV,
                maximize: false,
            });
        }
    }

    best_power.map(|(i, _)| {
        let cap = &caps[i];
        // Under the power policy (or the efficiency fallback) the region is
        // whatever the ratio gives; `FallbackPower` marks a rail that does not
        // clear the 4-switch band.
        let region = match policy {
            AutoPolicy::Efficiency => RailRegion::FallbackPower,
            AutoPolicy::Power => classify(cap, v_set_mv),
        };
        AutoChoice {
            rail_mv: cap.voltage_mv,
            current_ma: current_ma(cap),
            index: i as u8,
            region,
            pps: None,
            avs: avs_window(cap.voltage_mv),
            epr: cap.voltage_mv > board::SPR_MAX_MV,
            maximize: false,
        }
    })
}

/// Nearest rail to an explicit manual preset.
///
/// A PPS contract wins when the preset falls inside the source's PPS window and
/// no fixed PDO is a close match (this is what makes a 12 V preset reachable).
/// Otherwise the nearest fixed PDO is used, with the declared EPR rails
/// injected so a 28/36/48 V preset can be requested before EPR mode has been
/// entered.
pub fn choose_nearest(
    v_target_mv: u32,
    caps: &[SourceCapability],
    allow_epr: bool,
) -> Option<AutoChoice> {
    if let Some(choice) = choose_pps(v_target_mv, caps) {
        return Some(choice);
    }
    // `i_set_ma = 0` here: for a manual preset only the target voltage decides
    // whether EPR rails need injecting.
    let mut buf = [SourceCapability::EMPTY; MAX_CANDIDATES];
    let caps = candidates(caps, v_target_mv, 0, allow_epr, &mut buf);
    choose_nearest_fixed(v_target_mv, caps)
}

fn choose_nearest_fixed(v_target_mv: u32, caps: &[SourceCapability]) -> Option<AutoChoice> {
    let mut best: Option<(usize, u32)> = None;
    for (i, cap) in caps.iter().enumerate() {
        if !is_fixed(cap) {
            continue;
        }
        let diff = cap.voltage_mv.abs_diff(v_target_mv);
        if best
            .map(|(bi, bd)| diff < bd || (diff == bd && cap.voltage_mv < caps[bi].voltage_mv))
            .unwrap_or(true)
        {
            best = Some((i, diff));
        }
    }
    best.map(|(i, _)| {
        let cap = &caps[i];
        AutoChoice {
            rail_mv: cap.voltage_mv,
            current_ma: current_ma(cap),
            index: i as u8,
            region: classify(cap, v_target_mv),
            pps: None,
            avs: avs_window(cap.voltage_mv),
            epr: cap.voltage_mv > board::SPR_MAX_MV,
            maximize: false,
        }
    })
}

/// A PPS contract at `target_mv` when a source PPS APDO covers it and no fixed
/// PDO is a close match. Returns `None` outside the PPS window or when the
/// target is already served by a fixed rail.
pub fn choose_pps(target_mv: u32, caps: &[SourceCapability]) -> Option<AutoChoice> {
    if target_mv < board::PPS_MIN_MV || target_mv > board::PPS_MAX_MV {
        return None;
    }
    if caps
        .iter()
        .any(|c| is_fixed(c) && c.voltage_mv.abs_diff(target_mv) <= board::PPS_FIXED_PREFER_MV)
    {
        return None;
    }
    let apdo = caps.iter().find(|c| {
        c.is_pps && c.max_current_ma > 0 && c.min_voltage_mv <= target_mv && target_mv <= c.voltage_mv
    })?;
    let rail = SourceCapability {
        voltage_mv: target_mv,
        ..*apdo
    };
    Some(AutoChoice {
        rail_mv: target_mv,
        current_ma: apdo.max_current_ma.min(board::IOUT_MAX_MA),
        index: 0,
        region: classify(&rail, target_mv),
        pps: Some(ApdoWindow {
            min_mv: apdo.min_voltage_mv,
            max_mv: apdo.voltage_mv,
        }),
        avs: None,
        epr: false,
        maximize: false,
    })
}

/// Highest rail the source advertises, for the "MAX" action.
///
/// Picks the highest-voltage fixed PDO, preferring EPR when `allow_epr` — the
/// declared EPR rails are injected when the source has not revealed its own yet,
/// exactly as for a manual 48 V preset. The returned choice has `maximize` set;
/// for an EPR rail the manager issues a controller-computed request so the
/// controller itself lands on the highest-power EPR PDO.
pub fn choose_highest(caps: &[SourceCapability], allow_epr: bool) -> Option<AutoChoice> {
    let mut buf = [SourceCapability::EMPTY; MAX_CANDIDATES];
    let caps = candidates(caps, board::EPR_AVS_MAX_MV, board::EPR_RAIL_CURRENT_MA, allow_epr, &mut buf);

    let mut best: Option<(usize, u32)> = None;
    for (i, cap) in caps.iter().enumerate() {
        if !is_fixed(cap) {
            continue;
        }
        if best
            .map(|(bi, bv)| {
                cap.voltage_mv > bv
                    || (cap.voltage_mv == bv && cap.max_current_ma > caps[bi].max_current_ma)
            })
            .unwrap_or(true)
        {
            best = Some((i, cap.voltage_mv));
        }
    }

    best.map(|(i, _)| {
        let cap = &caps[i];
        AutoChoice {
            rail_mv: cap.voltage_mv,
            current_ma: current_ma(cap),
            index: i as u8,
            region: classify(cap, cap.voltage_mv),
            pps: None,
            avs: None,
            epr: cap.voltage_mv > board::SPR_MAX_MV,
            maximize: true,
        }
    })
}

/// Index of the preset-cell nearest a rail, for highlighting on the PD screen.
pub fn nearest_preset_index(rail_mv: u32) -> u8 {
    let mut best = 0usize;
    let mut best_diff = u32::MAX;
    for (i, &preset) in PD_PRESET_VOLTAGES_MV.iter().enumerate() {
        let diff = preset.abs_diff(rail_mv);
        if diff < best_diff {
            best_diff = diff;
            best = i;
        }
    }
    best as u8
}

// Expected `choose_rail` results with the default guard bands, for a source
// advertising 5/9/15/20 V fixed SPR PDOs. The declared EPR rails (28/36/48 V)
// are injected whenever the setpoint cannot be met from SPR, so a high setpoint
// produces an EPR AVS request (with `EPR AVS Enable Sink Mode` asserted) that
// the TPS26750 turns into EPR mode entry. The power-feasibility test uses
// `v_set × i_set`, so lowering the current limit lets the efficiency-first
// policy pick a clean rail for a high Vset.
//
//   Vset   i_set   output power   expected choice
//   12 V   3 A      36 W          injected? no  → 20 V  boost (100 W rail)
//   20 V   5 A     100 W          injected? no  → 20 V  rail (100 W)
//   24 V   5 A     120 W          injected yes → 48 V  boost (240 W rail)
//   36 V   5 A     180 W          injected yes → 48 V  boost (240 W rail)
//   48 V   5 A     240 W          injected yes → 48 V  fallback (no clean rail)
//   54 V   5 A     270 W          injected yes → 48 V  fallback (max rail)
//
// Host unit tests are not possible: the crate hard-depends on embassy-stm32 and
// is `#![no_std]`, so the `test` harness cannot link. Validate this table on the
// bench via `get_active_contract` and the efficiency sweep.
