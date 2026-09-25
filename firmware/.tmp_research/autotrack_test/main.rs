//! Host-side harness: includes the *real* `src/pd/auto_track.rs` with stub
//! `board` / `drivers` / `state` modules so its pure logic can be exercised.
#![allow(dead_code)]

mod board {
    pub const AUTO_TRACK_BUCK_MIN_RATIO_PCT: u32 = 135;
    pub const AUTO_TRACK_BOOST_MAX_RATIO_PCT: u32 = 70;
    pub const AUTO_TRACK_POWER_HEADROOM_PCT: u32 = 110;
    pub const IOUT_MAX_MA: u32 = 20_000;
    pub const SPR_MAX_MV: u32 = 20_000;
    pub const EPR_RAILS_MV: [u32; 3] = [28_000, 36_000, 48_000];
    pub const EPR_RAIL_CURRENT_MA: u32 = 5_000;
    pub const EPR_AVS_MIN_MV: u32 = 15_000;
    pub const EPR_AVS_MAX_MV: u32 = 48_000;
    pub const PPS_MIN_MV: u32 = 3_300;
    pub const PPS_MAX_MV: u32 = 21_000;
    pub const PPS_FIXED_PREFER_MV: u32 = 1_000;
}

mod drivers {
    pub mod tps26750 {
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
    }
}

mod state {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum AutoPolicy {
        Efficiency,
        Power,
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum RailRegion {
        Buck,
        Boost,
        FallbackPower,
        Unavailable,
    }
    pub const PD_PRESET_VOLTAGES_MV: [u32; 6] = [12_000, 15_000, 20_000, 28_000, 36_000, 48_000];
}

mod pd {
    pub mod auto_track {
        include!("auto_track_included.rs");
    }
}

use drivers::tps26750::SourceCapability;
use pd::auto_track::{choose_nearest, choose_pps, choose_rail, AutoChoice};
use state::AutoPolicy;

const fn fx(v: u32, i: u32) -> SourceCapability {
    SourceCapability {
        voltage_mv: v,
        max_current_ma: i,
        is_pps: false,
        is_avs: false,
        min_voltage_mv: 0,
        max_current_9_15_ma: 0,
    }
}

const fn ppsc(min: u32, max: u32, i: u32) -> SourceCapability {
    SourceCapability {
        voltage_mv: max,
        max_current_ma: i,
        is_pps: true,
        is_avs: false,
        min_voltage_mv: min,
        max_current_9_15_ma: 0,
    }
}

fn show(tag: &str, c: Option<AutoChoice>) {
    match c {
        Some(c) => println!(
            "  {tag:<34} -> {} mV, {} mA, {:?}{}",
            c.rail_mv,
            c.current_ma,
            c.region,
            match (c.pps, c.avs) {
                (Some(w), _) => format!(", PPS {}..{} mV", w.min_mv, w.max_mv),
                (None, Some(w)) => format!(", AVS {}..{} mV", w.min_mv, w.max_mv),
                (None, None) => String::new(),
            }
        ),
        None => println!("  {tag:<34} -> None"),
    }
}

fn main() {
    let spr = [fx(5000, 3000), fx(9000, 3000), fx(15000, 3000), fx(20000, 5000)];
    let pps_src = [fx(5000, 3000), fx(9000, 3000), ppsc(3300, 21000, 3000)];

    let mut epr = [SourceCapability::EMPTY; 9];
    epr[..4].copy_from_slice(&spr);
    epr[4] = fx(28000, 5000);
    epr[5] = fx(36000, 5000);
    epr[6] = fx(48000, 5000);
    epr[7] = ppsc(3300, 21000, 3000);
    epr[8] = SourceCapability {
        voltage_mv: 48000,
        max_current_ma: 5000,
        is_pps: false,
        is_avs: true,
        min_voltage_mv: 15000,
        max_current_9_15_ma: 0,
    };

    println!("== choose_pps ==");
    show("pps 12V on PPS source", choose_pps(12000, &pps_src));
    show("pps 12V when 12V fixed exists", choose_pps(12000, &[fx(12000, 3000), ppsc(3300, 21000, 3000)]));
    show("pps 28V (out of window)", choose_pps(28000, &pps_src));

    println!("== Auto / Efficiency / SPR-only source (EPR not yet visible) ==");
    show("12V/3A (36W)", choose_rail(12000, 3000, &spr, AutoPolicy::Efficiency, true));
    show("20V/5A (100W)", choose_rail(20000, 5000, &spr, AutoPolicy::Efficiency, true));
    show("24V/5A (120W)", choose_rail(24000, 5000, &spr, AutoPolicy::Efficiency, true));
    show("36V/5A (180W)", choose_rail(36000, 5000, &spr, AutoPolicy::Efficiency, true));
    show("48V/5A (240W)", choose_rail(48000, 5000, &spr, AutoPolicy::Efficiency, true));
    show("12V/10A (120W)", choose_rail(12000, 10000, &spr, AutoPolicy::Efficiency, true));
    show("48V/5A EPR latched off", choose_rail(48000, 5000, &spr, AutoPolicy::Efficiency, false));
    show("48V/5A Power policy", choose_rail(48000, 5000, &spr, AutoPolicy::Power, true));

    println!("== Auto / Efficiency / EPR PDOs already advertised ==");
    show("24V/5A", choose_rail(24000, 5000, &epr, AutoPolicy::Efficiency, true));
    show("48V/5A", choose_rail(48000, 5000, &epr, AutoPolicy::Efficiency, true));

    println!("== Manual presets (SPR-only source, EPR injected as needed) ==");
    for (i, p) in state::PD_PRESET_VOLTAGES_MV.iter().enumerate() {
        show(&format!("preset[{i}] = {p} mV"), choose_nearest(*p, &spr, true));
    }
    println!("== Manual presets (PPS-capable source) ==");
    show("12V", choose_nearest(12000, &pps_src, true));
    show("15V", choose_nearest(15000, &pps_src, true));
    println!("== Manual with EPR latched off ==");
    show("12V", choose_nearest(12000, &spr, false));
    show("48V", choose_nearest(48000, &spr, false));
    println!("== Manual after EPR entry ==");
    show("48V", choose_nearest(48000, &epr, true));

    // Reproduces the field log: SPR=7 EPR=0, source is PPS-capable (PPS max is
    // 21 V, which used to be mistaken for an EPR PDO).
    let spr7 = [
        fx(5000, 3000),
        fx(9000, 3000),
        fx(12000, 3000),
        fx(15000, 3000),
        fx(20000, 5000),
        ppsc(3300, 21000, 3000),
        ppsc(3300, 21000, 3000),
    ];
    println!("== 7-SPR PPS source, EPR not yet entered (field case) ==");
    for (i, p) in state::PD_PRESET_VOLTAGES_MV.iter().enumerate() {
        show(&format!("preset[{i}] = {p} mV"), choose_nearest(*p, &spr7, true));
    }
    show("auto 48V/5A", choose_rail(48000, 5000, &spr7, AutoPolicy::Efficiency, true));
}
