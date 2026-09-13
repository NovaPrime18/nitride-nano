//! Auto-tracking PD rail selection.
//!
//! The LT8390A runs in a 4-switch buck-boost region whenever `VIN` and `VOUT`
//! are close (peak-buck/peak-boost cross at `VIN/VOUT` ≈ 0.98–1.04; the
//! buck-boost band spans roughly 0.75–1.33). That region switches all four
//! FETs and is the least efficient way to move power, so this module picks a
//! fixed USB-PD rail that keeps the converter in a clean buck or clean boost
//! region whenever the requested output power allows it.
//!
//! The logic here is deliberately free of embassy dependencies so it stays
//! readable and easy to reason about independently of the HAL; it only consumes
//! parsed source PDOs and the board tuning constants.

use crate::board;
use crate::drivers::tps26750::SourceCapability;
use crate::state::{AutoPolicy, PD_PRESET_VOLTAGES_MV, RailRegion};

/// The rail Auto-tracking would request, with the region it should produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoChoice {
    pub rail_mv: u32,
    pub current_ma: u32,
    /// Index into the `caps` slice the choice came from.
    pub index: u8,
    pub region: RailRegion,
}

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

/// Choose a rail for Auto-tracking PD.
///
/// `v_set_mv` / `i_set_ma` are the output setpoints; `caps` is the parsed
/// source-capability list. Returns `None` when no fixed PDO is available.
pub fn choose_rail(
    v_set_mv: u32,
    i_set_ma: u32,
    caps: &[SourceCapability],
    policy: AutoPolicy,
) -> Option<AutoChoice> {
    if v_set_mv == 0 {
        return None;
    }

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
        }
    })
}

/// Nearest fixed rail to an explicit target (manual preset selection).
pub fn choose_nearest(v_target_mv: u32, caps: &[SourceCapability]) -> Option<AutoChoice> {
    let mut best: Option<(usize, u32)> = None;
    for (i, cap) in caps.iter().enumerate() {
        if !is_fixed(cap) {
            continue;
        }
        let diff = cap.voltage_mv.abs_diff(v_target_mv);
        if best
            .map(|(bi, bd)| {
                diff < bd
                    || (diff == bd && cap.voltage_mv < caps[bi].voltage_mv)
            })
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

// Expected `choose_rail` results with the default guard bands and a source
// advertising 5/9/15/20/28/36/48 V fixed PDOs, for the bench checklist. The
// power-feasibility test uses `v_set × i_set`, so lowering the current limit
// lets the efficiency-first policy pick a clean rail for a high Vset.
//
//   Vset   i_set   output power   expected choice
//   12 V   3 A      36 W          20 V  buck   (100 W rail)
//   20 V   5 A     100 W          28 V  buck   (140 W rail)
//   24 V   5 A     120 W          36 V  buck   (180 W rail)
//   36 V   5 A     180 W          48 V  fallback (clean boost tops out at 100 W)
//   48 V   5 A     240 W          48 V  fallback (no clean rail reaches 240 W)
//   54 V   5 A     270 W          48 V  fallback (clean boost 36 V = 180 W)
//   54 V   3 A     162 W          36 V  boost  (180 W rail)
//
// Host unit tests are not possible: the crate hard-depends on embassy-stm32 and
// is `#![no_std]`, so the `test` harness cannot link. Validate this table on the
// bench via `get_active_contract` and the efficiency sweep.
