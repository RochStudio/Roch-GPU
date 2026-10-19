//! Model and service checks from tests/GpuTuner.Core.Tests/Program.cs (and the App tests' settings
//! checks) that the per-module unit tests do not already cover.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use roch_core::models::graphics_info::GpuIdentity;
use roch_core::models::{clock_step, ocp_bounds, voltage_plan};
use roch_core::services::{AppSettings, ProfileRecovery, ProfileStore};
use roch_core::*;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roch-ported-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[test]
fn fan_curve_normalize_clamps_and_sorts() {
    let mut n =
        FanCurve::with_points(vec![FanPoint::new(200.0, 150.0), FanPoint::new(-5.0, -10.0), FanPoint::new(50.0, 50.0)]);
    n.normalize();
    assert!(n.points[0].temperature_c == 0.0 && n.points[2].temperature_c == 110.0, "normalize sorted");
    assert!(n.points[0].fan_percent == 0.0 && n.points[2].fan_percent == 100.0, "normalize clamped");
}

#[test]
fn profile_clamp() {
    let caps = GpuCapabilities {
        core_offset_min_mhz: -200,
        core_offset_max_mhz: 200,
        memory_offset_min_mhz: -500,
        memory_offset_max_mhz: 1000,
        power_limit_min_percent: 60,
        power_limit_max_percent: 110,
        temp_limit_min_c: 65,
        temp_limit_max_c: 88,
        ..Default::default()
    };
    let mut p = TuningProfile {
        core_offset_mhz: 999,
        memory_offset_mhz: -9999,
        power_limit_percent: 200,
        temp_limit_c: 10,
        ..Default::default()
    };
    p.clamp_to(&caps);
    assert_eq!(p.core_offset_mhz, 200, "clamp core");
    assert_eq!(p.memory_offset_mhz, -500, "clamp mem");
    assert_eq!(p.power_limit_percent, 110, "clamp power");
    assert_eq!(p.temp_limit_c, 65, "clamp temp");
}

#[test]
fn clock_range_clamp() {
    let caps = GpuCapabilities {
        can_lock_clocks: true,
        clock_lock_min_mhz: 210,
        clock_lock_max_mhz: 3090,
        ..Default::default()
    };
    let clamp = |lo: i32, hi: i32| {
        let mut p = TuningProfile { clock_lock_min_mhz: lo, clock_lock_max_mhz: hi, ..Default::default() };
        p.clamp_to(&caps);
        (p.clock_lock_min_mhz, p.clock_lock_max_mhz)
    };
    assert_eq!(clamp(50, 9000), (210, 3090), "clock range clamps to what the driver locks");
    let (lo, hi) = clamp(2000, 1000);
    assert!(lo <= hi, "inverted clock window collapses rather than inverting");
    assert_eq!(clamp(0, 0), (0, 0), "an unset clock range stays unset");
    assert_eq!(clamp(0, 2400), (0, 2400), "a ceiling-only window keeps its ceiling");
}

#[test]
fn per_fan_duties() {
    let fan_caps = GpuCapabilities {
        can_set_fan_speed: true,
        fan_min_percent: 30,
        fan_max_percent: 100,
        fan_count: 3,
        ..Default::default()
    };
    let shared = TuningProfile { fixed_fan_percent: 55, ..Default::default() };
    assert!(shared.fixed_fan_percents.is_empty(), "no per-fan list by default");
    assert!(shared.fan_percent_for(0) == 55 && shared.fan_percent_for(2) == 55, "every fan falls back");

    let each = TuningProfile { fixed_fan_percent: 40, fixed_fan_percents: vec![45, 65, 90], ..Default::default() };
    assert_eq!(each.fan_percent_for(1), 65, "per-fan duties are read back by index");
    assert_eq!(each.fan_percent_for(9), 40, "a fan past the end falls back");
    assert_eq!(each.clone_profile().fixed_fan_percents[2], 90, "per-fan duties survive a clone");

    let mut wild = TuningProfile { fixed_fan_percents: vec![5, 150, 60, 60], ..Default::default() };
    wild.clamp_to(&fan_caps);
    assert!(wild.fixed_fan_percents[0] == 30 && wild.fixed_fan_percents[1] == 100, "per-fan duties clamp");
    assert_eq!(wild.fixed_fan_percents.len(), 3, "the list is cut to the fan count");
}

#[test]
fn xoc_lever_independence_and_legacy_gate() {
    let mut one = TuningProfile::default().clone_profile();
    one.xoc_armed = one.xoc_armed.with(XocLever::SYS, true);
    assert_eq!(one.xoc_armed, XocLever::SYS, "arming one lever arms only it");
    assert_eq!(one.xoc_armed.with(XocLever::VIDEO, false), XocLever::SYS, "disarming an off lever is a no-op");
    let legacy = TuningProfile::from_json(r#"{"XocEnabled":true,"VoltageRailMaxMv":1100}"#).unwrap();
    assert_eq!(legacy.xoc_armed, XocLever::ALL, "a legacy single gate arms every lever");
    assert_eq!(legacy.voltage_rail_max_mv, 1100);
    let off = TuningProfile::from_json(r#"{"XocEnabled":false}"#).unwrap();
    assert_eq!(off.xoc_armed, XocLever::NONE, "a legacy shut gate arms nothing");
}

#[test]
fn profile_store_round_trip() {
    let dir = temp_dir("store");
    let store = ProfileStore::new(Some(dir.clone())).unwrap();
    let mut prof = TuningProfile {
        name: "My UV".into(),
        core_offset_mhz: 150,
        memory_offset_mhz: 800,
        power_limit_percent: 85,
        temp_limit_c: 78,
        fan_mode: FanMode::Curve,
        fixed_fan_percents: vec![45, 65, 90],
        ..Default::default()
    };
    prof.fan_curve.points = vec![FanPoint::new(35.0, 25.0), FanPoint::new(75.0, 90.0)];
    prof.fan_curve.hysteresis_c = 4.0;
    store.save(&mut prof).unwrap();
    let back = store.load("My UV").unwrap().expect("store load not null");
    assert_eq!(back.core_offset_mhz, 150, "store core");
    assert_eq!(back.fan_mode, FanMode::Curve, "store fanmode enum");
    assert!(back.fan_curve.points.len() == 2 && back.fan_curve.points[1].fan_percent == 90.0, "store curve points");
    assert!((back.fan_curve.hysteresis_c - 4.0).abs() < 1e-9, "store hysteresis");
    // The card reports no curve and no per-fan split: a profile losing either loses it for good.
    assert_eq!(back.fixed_fan_percents, vec![45, 65, 90], "store per-fan duties");
    assert_eq!(store.list_profile_names(), vec!["My UV".to_string()], "store list");
    assert!(store.delete("My UV").unwrap() && store.list_profile_names().is_empty(), "store delete");

    let settings = AppSettings {
        startup_profile: Some("X".into()),
        apply_on_startup: true,
        poll_interval_ms: 500,
        ..Default::default()
    };
    store.save_settings(&settings).unwrap();
    let s_back = store.load_settings();
    assert!(
        s_back.startup_profile.as_deref() == Some("X") && s_back.apply_on_startup && s_back.poll_interval_ms == 500,
        "settings roundtrip"
    );

    // App tests: theme preference.
    assert!(!AppSettings::default().dark_mode, "fresh settings open in light mode");
    store.save_settings(&AppSettings { dark_mode: true, ..Default::default() }).unwrap();
    assert!(store.load_settings().dark_mode, "saved dark preference survives loading");
    store.save_settings(&AppSettings { dark_mode: false, ..Default::default() }).unwrap();
    assert!(!store.load_settings().dark_mode, "saved light preference survives loading");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn clock_step_ranges_and_grids() {
    use clock_step::*;
    const { assert!(100 % VOLTAGE_BOOST_PERCENT == 0) };
    assert_eq!(snap_within(63, VOLTAGE_BOOST_PERCENT, 0, 100), 65);
    assert_eq!(snap_within(100, VOLTAGE_BOOST_PERCENT, 0, 100), 100);
    assert_eq!(snap_within(0, VOLTAGE_BOOST_PERCENT, 0, 100), 0);
    // The full driver ranges a 5070 Ti reports: both ends reachable on their grids.
    assert_eq!(snap_within(-1000, CORE_MHZ, -1000, 1000), -1000);
    assert_eq!(snap_within(1000, CORE_MHZ, -1000, 1000), 1000);
    assert_eq!(snap_within(-1000, MEMORY_MHZ, -1000, 3000), -1000);
    assert_eq!(snap_within(3000, MEMORY_MHZ, -1000, 3000), 3000);
    assert_eq!(snap_within(310, CORE_MHZ, -1000, 1000), 315, "interior values keep the grid");

    let s = CORE_MHZ;
    assert_eq!(s, 15);
    assert_eq!(snap(150, s), 150);
    assert_eq!(snap(20, s), 15);
    assert_eq!(snap(0, s), 0);
    assert_eq!(snap(-150, s), -150);
    assert_eq!(snap(-7, s), 0);
    assert_eq!(snap(-8, s), -15);
    assert_eq!(snap(137, 1), 137);
    assert_eq!(snap_within(998, s, -1000, 1000), 1000, "nearer to the limit than to 990");
    assert_eq!(snap_within(-998, s, -1000, 1000), -1000);
    assert_eq!(snap_within(-5000, s, -1000, 1000), -1000, "out of range lands on the driver's limit");
    assert_eq!(snap_within(992, s, -1000, 1000), 990);
    assert_eq!(snap_within(137, s, -1000, 1000), 135);
    assert_eq!(snap_within(0, s, -1000, 1000), 0);
    assert_eq!(snap_within(990, s, -1000, 990), 990);
    assert_eq!(snap_within(42, s, 100, -100), 42, "inverted range returns the input");

    let m = MEMORY_MHZ;
    assert_eq!(m, 25);
    assert_eq!(snap_within(800, m, -1000, 4000), 800);
    assert_eq!(snap_within(810, m, -1000, 4000), 800);
    assert_eq!(snap_within(815, m, -1000, 4000), 825);
    assert_eq!(snap_within(10, m, -1000, 4000), 0);
    assert_eq!(snap_within(-815, m, -1000, 4000), -825);
    assert_eq!(snap_within(3999, m, -1000, 4000), 4000);
    assert_eq!(snap_within(-999, m, -1000, 4000), -1000);
    assert_eq!(snap_within(990, m, -990, 990), 990, "an off-grid driver limit is still reachable");
    assert_eq!(snap_within(980, m, -990, 990), 975, "inside the range the grid still applies");
    assert_eq!(snap_within(2518, 1, 0, 4000), 2518, "absolute clock is never snapped");
}

#[test]
fn voltage_plan_levers_and_baselines() {
    use voltage_plan::*;
    let (stock, max) = (1090, 1150); // 4070 Ti: curve top 1090, +60 mV of boost headroom
    assert_eq!(compute(stock, stock, max), (0, 0), "at stock = no levers pulled");
    assert_eq!(compute(1150, stock, max), (100, 0), "above stock -> boost only");
    assert_eq!(compute(1120, stock, max), (50, 0), "half the headroom -> 50%");
    assert_eq!(compute(1000, stock, max), (0, -90), "below stock -> flatten only");
    assert_eq!(compute(900, stock, max), (0, -190), "deep undervolt");
    assert_eq!(compute(9999, stock, max), (100, 0), "boost clamped to 100");
    assert_eq!(compute(1200, stock, stock), (0, 0), "no headroom -> no boost");
    assert_eq!(compute(1000, 0, 0), (0, 0), "unknown curve -> no-op");
    assert_eq!(compute(0, stock, max), (0, 0), "zero target -> no-op");

    // 5070 Ti: table to 1240 mV, card seen at 1035, a 1000 mV cap reported as -240 vs the table.
    let (table, seen, boosted) = (1240, 1035, 1100);
    assert_eq!(ceiling_mv(0, -240, seen, boosted, table), 1000, "flatten vs table top resolves to the cap");
    assert_eq!(ceiling_mv(0, 0, seen, boosted, table), seen, "no flatten -> the measured ceiling");
    assert_eq!(ceiling_mv(100, 0, seen, boosted, table), boosted, "boost raises the ceiling");
    assert_eq!(ceiling_mv(0, -35, seen, boosted, table), seen, "a flatten above the roof does not raise it");
    assert_eq!(ceiling_mv(100, -300, seen, boosted, table), 940, "flatten still caps a boosted card");
    assert_eq!(ceiling_mv(0, -240, seen, boosted, 0), seen, "unknown table top -> ceiling unchanged");
    assert_ne!(ceiling_mv(0, -240, seen, boosted, table), to_target_mv(0, -240, seen, boosted), "no 795 mV");

    for t in [850, 900, 1000, 1089, 1090, 1091, 1120, 1150] {
        let (b, o) = compute(t, stock, max);
        assert!(b == 0 || o == 0, "levers exclusive at {t} mV");
    }
    assert_eq!(to_target_mv(0, 0, stock, max), stock, "round trip stock");
    assert_eq!(to_target_mv(0, -90, stock, max), 1000, "round trip undervolt");
    assert_eq!(to_target_mv(100, 0, stock, max), 1150, "round trip full boost");
}

#[test]
fn ocp_bounds_window() {
    assert_eq!(ocp_bounds::min_a(300_000), 150, "OCP min is half stock");
    assert_eq!(ocp_bounds::max_a(300_000), 450, "OCP max is 150% of stock");
    assert!(ocp_bounds::min_a(120_000) == 60 && ocp_bounds::max_a(120_000) == 180, "OCP window on MSVDD");
    assert!(ocp_bounds::min_a(0) == 0 && ocp_bounds::max_a(0) == 0, "OCP window of an absent rail is empty");
}

#[test]
fn graphics_info_from_device() {
    let device = GpuDevice {
        index: 0,
        name: "Test GPU".into(),
        vendor: "NVIDIA".into(),
        bus_id: "PCI".into(),
        driver_version: "610.88".into(),
        vram_megabytes: 16304,
        bios_version: "98.00".into(),
    };
    let info = GpuGraphicsInfo {
        board_manufacturer: "Test Board".into(),
        code_name: "TEST-100".into(),
        revision: "A1".into(),
        ..GpuGraphicsInfo::from_device(&device)
    };
    let rows = info.rows();
    assert_eq!(rows.len(), 17, "graphics rows include firmware and PCI address");
    assert_eq!(info.bios_version, "98.00");
    assert_eq!(info.bus_address, "PCI");
    assert!(rows[0].name == "GPU" && rows[0].value == "Test GPU");
    assert_eq!(rows.last().unwrap().name, "Driver Date");
    assert!(rows.iter().enumerate().all(|(i, r)| r.is_banded == (i % 2 == 1)), "rows alternate shading");
    assert_eq!(rows.iter().find(|r| r.name == "Memory Type").unwrap().value, "—");
    assert_eq!(info.memory_size, "15.92 GB");
    assert_eq!(GpuIdentity::board_vendor(0x1458), "GIGABYTE Technology");
    assert_eq!(GpuIdentity::driver_date_text(Some("7-22-2026")), "2026-07-22");
}

#[test]
fn stock_profiles() {
    let amd = GpuCapabilities { memory_clock_is_absolute: true, memory_clock_default_mhz: 2518, ..Default::default() };
    assert_eq!(TuningProfile::stock(&amd, Some("Test")).memory_offset_mhz, 2518);
    assert_eq!(TuningProfile::stock(&GpuCapabilities::default(), None).memory_offset_mhz, 0);
    // The temperature default is whatever this card reports, not one value for every GPU.
    for temp_default in [65, 83, 88, 90] {
        let caps = GpuCapabilities { temp_limit_default_c: temp_default, ..Default::default() };
        assert_eq!(
            TuningProfile::stock(&caps, None).temp_limit_c,
            temp_default,
            "stock keeps a {temp_default} °C card's own default"
        );
    }
}

/// The full recovery journal flow from the C# tests (mock writes only).
#[test]
fn recovery_journal_flow() {
    let dir = temp_dir("recovery");
    let stock = TuningProfile { name: "Stock".into(), core_offset_mhz: 0, ..Default::default() };
    let mut tune = TuningProfile { name: "Slot 1".into(), core_offset_mhz: 100, ..Default::default() };
    let writes = Arc::new(Mutex::new(Vec::<i32>::new()));
    let reject = Arc::new(Mutex::new(false));
    let applier = || {
        let (writes, reject) = (writes.clone(), reject.clone());
        Box::new(move |p: TuningProfile| {
            writes.lock().unwrap().push(p.core_offset_mhz);
            if *reject.lock().unwrap() { vec!["mock driver failure".to_string()] } else { vec![] }
        }) as roch_core::services::recovery::ApplyFn
    };
    let set_reject = |v: bool| *reject.lock().unwrap() = v;
    let last_write = || *writes.lock().unwrap().last().unwrap();

    let mut recovery = ProfileRecovery::new(&dir, "mock", &stock, applier()).unwrap();
    assert!(!recovery.has_pending() && recovery.last_confirmed().is_none(), "recovery starts empty");
    recovery.begin(&tune).unwrap();
    tune.core_offset_mhz = 999;
    assert!(ProfileRecovery::new(&dir, "mock", &stock, applier()).unwrap().has_pending(), "pending survives restart");
    recovery.confirm().unwrap();
    assert_eq!(recovery.last_confirmed().unwrap().core_offset_mhz, 100, "confirmed snapshot is cloned");
    recovery.approve_startup("Slot 1").unwrap();
    recovery.begin(&tune).unwrap();
    drop(recovery);

    let mut restarted = ProfileRecovery::new(&dir, "mock", &stock, applier()).unwrap();
    restarted.revert();
    assert!(last_write() == 100 && !restarted.has_pending(), "interrupted trial restores confirmed profile");
    restarted.begin(&tune).unwrap();
    set_reject(true);
    assert!(!restarted.revert().is_empty(), "failed rollback reports error");
    assert!(ProfileRecovery::new(&dir, "mock", &stock, applier()).unwrap().has_pending(), "failed rollback is durable");
    assert!(restarted.confirm().is_err(), "failed rollback cannot be confirmed");
    set_reject(false);
    restarted.revert();
    set_reject(true);
    restarted.begin(&tune).unwrap();
    assert!(restarted.confirm().is_err(), "failed apply cannot be confirmed");
    set_reject(false);
    restarted.revert();
    restarted.begin(&tune).unwrap();
    restarted.confirm().unwrap();
    assert_eq!(restarted.startup_profile("Slot 1").unwrap().core_offset_mhz, 100, "frozen startup kept");
    let mut copy = restarted.startup_profile("Slot 1").unwrap();
    copy.core_offset_mhz = 7;
    assert_eq!(restarted.startup_profile("Slot 1").unwrap().core_offset_mhz, 100, "startup snapshots by value");

    let mut first = ProfileRecovery::new(&dir, "different mock", &stock, applier()).unwrap();
    first.begin(&tune).unwrap();
    first.revert();
    assert_eq!(last_write(), 0, "first trial rollback uses defaults");
    assert!(first.last_confirmed().is_none() && restarted.last_confirmed().is_some(), "journals are separate");
    let _ = fs::remove_dir_all(dir);
}
