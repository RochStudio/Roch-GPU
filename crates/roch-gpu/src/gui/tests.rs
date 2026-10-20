//! View-model tests: the checks of `tests/GpuTuner.App.Tests/Program.cs`, plus the actions the
//! WPF suite only exercised by hand (apply, slots, levers, reset, startup). Everything runs on the
//! simulated GPU with a throwaway profile folder and a fake logon task.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use roch_core::mock::MockBackend;
use roch_core::services::{AppSettings, ProfileStore, TuningService};
use roch_core::{FanCurve, FanMode, GpuCapabilities, Result, TuningProfile, VoltageControlStyle, XocLever};

use super::shared::{Shared, StartupTasks};
use super::view_model::{Field, MainViewModel};

/// A logon task that lives in memory.
#[derive(Default)]
struct FakeStartup {
    registered: AtomicBool,
    name: Mutex<Option<String>>,
}

impl StartupTasks for FakeStartup {
    fn exists(&self) -> bool {
        self.registered.load(Ordering::SeqCst)
    }
    fn register(&self, _exe: &str, profile: &str) -> Result<()> {
        self.registered.store(true, Ordering::SeqCst);
        *self.name.lock().unwrap() = Some(profile.to_string());
        Ok(())
    }
    fn unregister(&self) -> Result<()> {
        self.registered.store(false, Ordering::SeqCst);
        Ok(())
    }
}

/// A unique temporary folder, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "RochGpuGuiTests-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    _dir: TempDir,
    shared: Shared,
    startup: Arc<FakeStartup>,
}

fn fixture() -> Fixture {
    let dir = TempDir::new();
    let store = Arc::new(ProfileStore::new(Some(dir.0.clone())).unwrap());
    let service = Arc::new(TuningService::new(Box::new(MockBackend::new())));
    service.initialize(0).unwrap();
    let startup = Arc::new(FakeStartup::default());
    let shared = Shared::new(service, store, AppSettings::default(), startup.clone());
    let recovery = shared.open_recovery().unwrap();
    shared.set_recovery(Some(recovery));
    Fixture { _dir: dir, shared, startup }
}

fn multi_fixture() -> Fixture {
    let dir = TempDir::new();
    let store = Arc::new(ProfileStore::new(Some(dir.0.clone())).unwrap());
    let service = Arc::new(TuningService::new(Box::new(MockBackend::with_second_gpu())));
    service.initialize(0).unwrap();
    let startup = Arc::new(FakeStartup::default());
    let shared = Shared::new(service, store, AppSettings::default(), startup.clone());
    let recovery = shared.open_recovery().unwrap();
    shared.set_recovery(Some(recovery));
    Fixture { _dir: dir, shared, startup }
}

fn vm(f: &Fixture) -> MainViewModel {
    MainViewModel::new(f.shared.clone())
}

#[test]
fn switching_gpu_rebuilds_the_editor_and_persists_the_choice() {
    let f = multi_fixture();
    let mut vm = vm(&f);
    assert_eq!(vm.gpu_count(), 2);
    assert_eq!(vm.gpu_index(), 0);
    assert!(vm.gpu_choice_label(1).contains("Simulated RTX 4070 SUPER (2)"), "{}", vm.gpu_choice_label(1));
    let second = f.shared.service.devices()[1].name.clone();

    // A fixed fan duty is handed back before the switch: it belongs to the old card.
    f.shared.service.set_fans(FanMode::Fixed, 70, &[], &FanCurve::default(), None);
    assert!(f.shared.service.with_backend(|b| b.read_tuning_state(0)).unwrap().fan_manual);

    vm.switch_gpu(1).unwrap();
    assert_eq!(vm.gpu_index(), 1);
    assert_eq!(vm.device.name, second);
    assert_eq!(f.shared.service.gpu_index(), 1);
    assert_eq!(f.shared.settings().gpu_index, 1, "choice is persisted");
    assert!(!f.shared.service.with_backend(|b| b.read_tuning_state(0)).unwrap().fan_manual, "old fans back to auto");

    // An index the backend did not report leaves the editor on the current card.
    assert!(vm.switch_gpu(9).is_err());
    assert_eq!(vm.gpu_index(), 1);
    assert_eq!(vm.device.name, second);

    vm.switch_gpu(0).unwrap();
    assert_eq!(vm.gpu_index(), 0);
    assert_eq!(f.shared.settings().gpu_index, 0);
}

#[test]
fn theme_preference_round_trips() {
    let dir = TempDir::new();
    let store = ProfileStore::new(Some(dir.0.clone())).unwrap();
    assert!(!store.load_settings().dark_mode, "fresh settings open in light mode");
    store.save_settings(&AppSettings { dark_mode: true, ..Default::default() }).unwrap();
    assert!(store.load_settings().dark_mode, "saved dark preference survives loading");
    store.save_settings(&AppSettings { dark_mode: false, ..Default::default() }).unwrap();
    assert!(!store.load_settings().dark_mode, "saved light preference survives loading");
}

#[test]
fn fan_window_writes_stay_in_sync_with_the_editor() {
    let f = fixture();
    let mut vm = vm(&f);
    let svc = f.shared.service.clone();
    // Start with a different old value, exactly as when the fan window owns its own slider.
    vm.set(Field::FixedFan, 35);
    let errors = svc.set_fans(FanMode::Fixed, 65, &[], &FanCurve::default(), None);
    assert!(errors.is_empty(), "fan-only apply succeeds: {errors:?}");
    vm.sync_fans_from_service(FanMode::Fixed, 65, &[]);
    assert_eq!(vm.get(Field::FixedFan), 65, "linked fan speed synchronized");

    vm.set(Field::PowerLimit, 95);
    let next = vm.build_profile_from_editor("Power change");
    assert!(
        next.fan_mode == FanMode::Fixed && next.fixed_fan_percent == 65 && next.fixed_fan_percents.is_empty(),
        "power profile retains selected fixed fan speed"
    );
    assert!(svc.apply(&next).is_empty(), "main apply succeeds");
    let actual = svc.with_backend(|b| b.read_tuning_state(0)).unwrap();
    assert!(
        actual.power_limit_percent == 95 && actual.fan_manual && actual.fan_percent == 65,
        "power changes while fan stays fixed"
    );

    let mut separate = vec![60, 75];
    vm.sync_fans_from_service(FanMode::Fixed, 60, &separate);
    separate[0] = 10;
    let next = vm.build_profile_from_editor("Separate fans");
    assert!(
        next.fixed_fan_percent == 60 && next.fixed_fan_percents == vec![60, 75],
        "individual fan settings are copied"
    );
    vm.sync_fans_from_service(FanMode::Fixed, 70, &[]);
    let next = vm.build_profile_from_editor("Relink fans");
    assert!(
        next.fixed_fan_percent == 70 && next.fixed_fan_percents.is_empty(),
        "relinked speed replaces old individual settings"
    );
    vm.sync_fans_from_service(FanMode::Auto, 70, &[]);
    assert_eq!(vm.build_profile_from_editor("Auto").fan_mode, FanMode::Auto, "automatic mode still synchronizes");
}

fn intel_caps(base: &GpuCapabilities) -> GpuCapabilities {
    GpuCapabilities {
        voltage_style: VoltageControlStyle::Percent,
        core_offset_step_mhz: 1,
        core_offset_min_mhz: -300,
        memory_clock_is_absolute: true,
        memory_clock_unit: "Mbps".into(),
        memory_offset_min_mhz: 19000,
        memory_offset_max_mhz: 22000,
        temp_limit_min_c: 60,
        temp_limit_max_c: 100,
        power_limit_max_percent: 120,
        ..base.clone()
    }
}

#[test]
fn intel_steps_ranges_and_clock_window() {
    let f = fixture();
    let mut vm = vm(&f);
    // Intel capabilities without touching a physical GPU.
    vm.caps = intel_caps(&vm.caps);
    vm.load_into_editor(&TuningProfile {
        core_offset_mhz: 91,
        memory_offset_mhz: 19001,
        voltage_boost_percent: 41,
        power_limit_percent: 101,
        temp_limit_c: 99,
        ..Default::default()
    });
    assert_eq!(
        [Field::CoreOffset, Field::MemoryOffset, Field::VoltageBoost, Field::PowerLimit, Field::TempLimit]
            .map(|x| vm.get(x)),
        [91, 19001, 41, 101, 99],
        "loading Intel settings preserves off-grid values"
    );
    vm.set(Field::CoreOffset, 90);
    vm.set(Field::MemoryOffset, 19000);
    vm.set(Field::VoltageBoost, 40);
    vm.set(Field::PowerLimit, 100);
    vm.set(Field::TempLimit, 90);
    for key in ["core", "mem", "voltboost", "power", "temp"] {
        vm.nudge_spec(&format!("{key}:1"));
    }
    assert_eq!(vm.get(Field::CoreOffset), 105, "Intel core step is 15 MHz");
    assert_eq!(vm.get(Field::MemoryOffset), 19100, "Intel memory step is 100 Mbps");
    assert_eq!(vm.memory_label(), "Memory Speed");
    assert_eq!(vm.field_unit(Field::MemoryOffset), "Mbps", "Mbps is drawn next to the value");
    assert_eq!(vm.get(Field::VoltageBoost), 45, "Intel voltage step is 5 percent");
    assert_eq!(vm.get(Field::PowerLimit), 105, "Intel power step is 5 percent");
    assert_eq!(vm.get(Field::TempLimit), 95, "Intel temperature step is 5 percent");

    vm.set(Field::MemoryOffset, 22099);
    vm.set(Field::CoreOffset, -999);
    vm.set(Field::PowerLimit, 999);
    vm.set(Field::TempLimit, 999);
    assert_eq!(
        [Field::MemoryOffset, Field::CoreOffset, Field::PowerLimit, Field::TempLimit].map(|x| vm.get(x)),
        [22000, -300, 120, 100],
        "Intel edits stay inside driver ranges"
    );

    vm.set_intel_clock_range_enabled(true);
    let (lo, hi) = (vm.caps.clock_lock_min_mhz, vm.caps.clock_lock_max_mhz);
    vm.set(Field::ClockLockMin, lo);
    vm.set(Field::ClockLockMax, hi);
    let next = vm.build_profile_from_editor("Intel range");
    assert!(
        next.clock_lock_min_mhz > 0 && next.clock_lock_max_mhz > 0 && next.xoc_armed.has(XocLever::CLOCK_RANGE),
        "Intel full hardware range is explicit, not factory reset"
    );
    vm.set_intel_clock_range_enabled(false);
    let next = vm.build_profile_from_editor("Factory range");
    assert!(
        next.clock_lock_min_mhz == 0 && next.clock_lock_max_mhz == 0 && !next.xoc_armed.has(XocLever::CLOCK_RANGE),
        "Intel disabled range requests factory defaults"
    );
    vm.load_into_editor(&TuningProfile {
        xoc_armed: XocLever::CLOCK_RANGE,
        clock_lock_min_mhz: 500,
        clock_lock_max_mhz: 2300,
        ..Default::default()
    });
    assert!(
        vm.intel_clock_range_enabled()
            && vm.build_profile_from_editor("Restored range").clock_lock_min_mhz == 500
            && vm.get(Field::ClockLockMax) == 2300,
        "Intel clock-range profile round trip"
    );
}

#[test]
fn nvidia_values_snap_to_the_driver_grid_and_clamp() {
    let f = fixture();
    let mut vm = vm(&f);
    vm.commit_input(Field::CoreOffset, "+97");
    assert_eq!(vm.get(Field::CoreOffset), 90, "core snaps to 15 MHz");
    assert_eq!(vm.input_text(Field::CoreOffset), "+90");
    vm.commit_input(Field::MemoryOffset, " 1012 MHz");
    assert_eq!(vm.get(Field::MemoryOffset), 1000, "memory offset snaps to 25 MHz");
    vm.commit_input(Field::CoreOffset, "garbage");
    assert_eq!(vm.get(Field::CoreOffset), 90, "garbage leaves the value alone");
    vm.commit_input(Field::PowerLimit, "9999");
    assert_eq!(vm.get(Field::PowerLimit), vm.caps.power_limit_max_percent, "typed values clamp");
    assert_eq!(
        vm.input_text(Field::PowerLimit),
        vm.caps.power_limit_max_percent.to_string(),
        "absolute boxes unsigned"
    );
    vm.nudge(Field::CoreOffset, -1);
    assert_eq!(vm.get(Field::CoreOffset), 75);
    vm.nudge_spec("mem:-1");
    assert_eq!(vm.get(Field::MemoryOffset), 975);
    vm.set(Field::FixedFan, 63);
    assert_eq!(vm.get(Field::FixedFan), 65, "fan duty snaps to 5%");
    assert!(vm.pending_changes(), "edits mark the editor dirty");
}

#[test]
fn apply_runs_as_a_job_and_reports() {
    let f = fixture();
    let mut vm = vm(&f);
    assert_eq!(vm.applied_summary(), "Nothing applied this session (driver state shown)");
    vm.set(Field::CoreOffset, 150);
    vm.apply();
    assert!(vm.busy(), "an apply is queued, not run on the UI thread");
    vm.run_jobs_inline();
    assert!(!vm.busy());
    assert!(!vm.status_is_error, "{}", vm.status);
    assert!(vm.status.starts_with("Applied at "), "{}", vm.status);
    assert!(!vm.pending_changes());
    assert!(vm.applied_summary().starts_with("On GPU: "), "{}", vm.applied_summary());
    assert!(vm.applied_summary().contains("core +150"), "{}", vm.applied_summary());
    assert_eq!(f.shared.service.read_tuning_state().unwrap().core_offset_mhz, 150);
    // "Session" is never written, so it must not become the remembered profile.
    assert!(f.shared.settings().last_profile_by_gpu.is_empty());
}

#[test]
fn slots_save_load_and_clear() {
    let f = fixture();
    let mut vm = vm(&f);
    vm.on_slot_clicked(2);
    assert!(vm.status_is_error && vm.status.starts_with("Slot 2 is empty"), "{}", vm.status);

    vm.set(Field::CoreOffset, 60);
    vm.set_save_armed(true);
    assert_eq!(vm.status, "Pick a slot (1-5) to store the current settings");
    vm.on_slot_clicked(2);
    assert!(!vm.save_armed());
    assert_eq!(vm.status, "Saved current settings to slot 2");
    let slot = vm.slots()[1].clone();
    assert!(slot.occupied && slot.is_active);
    assert!(slot.summary.contains("core +60 MHz"), "{}", slot.summary);
    assert_eq!(vm.selected_profile(), Some("Slot 2"));

    vm.set(Field::CoreOffset, 0);
    vm.on_slot_clicked(2);
    assert_eq!(vm.get(Field::CoreOffset), 60, "clicking a slot loads it");
    vm.run_jobs_inline();
    assert!(vm.status.starts_with("Slot 2 applied at "), "{}", vm.status);
    assert_eq!(f.shared.settings().last_profile_by_gpu.get(&vm.device.name).map(String::as_str), Some("Slot 2"));

    // A new view model lights up the slot applied last.
    let again = MainViewModel::new(f.shared.clone());
    assert!(again.slots()[1].is_active);

    vm.clear_slot(2);
    assert_eq!(vm.status, "Slot 2 cleared");
    assert!(!vm.slots()[1].occupied && !vm.slots()[1].is_active);
}

#[test]
fn xoc_levers_arm_and_disarm_one_at_a_time() {
    let f = fixture();
    let mut vm = vm(&f);
    assert_eq!(vm.xoc_status_text(), "Nothing armed - the card is running the driver's own values.");
    vm.toggle_lever(XocLever::NVVDD);
    vm.run_jobs_inline();
    assert!(vm.is_armed(XocLever::NVVDD), "{}", vm.status);
    assert_eq!(vm.status, "NVVDD range armed");
    assert_eq!(vm.xoc_status_text(), "Live on the card: NVVDD.");
    vm.toggle_lever(XocLever::NVVDD);
    vm.run_jobs_inline();
    assert!(!vm.is_armed(XocLever::NVVDD));
    assert_eq!(vm.status, "NVVDD range disarmed");
    assert_eq!(vm.get(Field::VoltageRailFloor), vm.caps.voltage_rail_stock_floor_mv);
}

#[test]
fn reset_loads_stock_and_disarms() {
    let f = fixture();
    let mut vm = vm(&f);
    vm.set(Field::CoreOffset, 150);
    vm.apply();
    vm.toggle_lever(XocLever::MSVDD);
    vm.run_jobs_inline();
    vm.reset();
    vm.run_jobs_inline();
    assert_eq!(vm.status, "Reset to driver defaults");
    assert_eq!(vm.get(Field::CoreOffset), 0);
    assert!(vm.xoc_armed().is_empty());
    assert!(!vm.pending_changes());
}

#[test]
fn startup_task_registers_a_confirmed_snapshot() {
    let f = fixture();
    let mut vm = vm(&f);
    assert!(!vm.apply_on_startup(), "the registered task is the truth");
    vm.set_apply_on_startup(true);
    vm.run_jobs_inline();
    assert!(vm.status_is_error && vm.status == "Pick a startup profile first", "{}", vm.status);
    assert!(!vm.apply_on_startup(), "the tick settles on what is registered");

    vm.set_save_armed(true);
    vm.on_slot_clicked(1);
    vm.set_startup_profile(Some("Slot 1".into()));
    vm.set_apply_on_startup(true);
    vm.run_jobs_inline();
    assert_eq!(vm.status, "Startup task registered for 'Slot 1'");
    assert!(vm.apply_on_startup() && f.startup.exists());
    assert_eq!(f.startup.name.lock().unwrap().as_deref(), Some("Slot 1"));
    assert!(f.shared.recovery().as_ref().unwrap().startup_profile("Slot 1").is_some());
    assert!(f.shared.settings().apply_on_startup);

    // Saving over the startup slot says the logon copy did not change.
    vm.on_slot_clicked(1);
    vm.run_jobs_inline();
    vm.set_save_armed(true);
    vm.on_slot_clicked(1);
    assert!(vm.status.ends_with("toggle Startup off/on to update it"), "{}", vm.status);

    vm.set_apply_on_startup(false);
    vm.run_jobs_inline();
    assert_eq!(vm.status, "Startup task removed");
    assert!(!f.startup.exists());
}

#[test]
fn trial_without_a_journal_applies_nothing() {
    let f = fixture();
    f.shared.set_recovery(None);
    let errors = f.shared.trial_profile(&TuningProfile::default());
    assert_eq!(errors, vec!["Profile recovery is unavailable; no settings applied.".to_string()]);
}

#[test]
fn identity_lines_and_limiter_text() {
    let f = fixture();
    let mut vm = vm(&f);
    assert!(vm.driver_line().starts_with("Driver: "));
    assert!(vm.bios_line().starts_with("vBIOS: "));
    assert!(vm.device_display_name().ends_with("GB)"), "{}", vm.device_display_name());
    assert_eq!(vm.limit_reason_text(), "Limiter — open the monitor to sample");
    assert!(!vm.limit_is_active());
    let t = roch_core::GpuTelemetry { limit_reason: "Power".into(), ..Default::default() };
    vm.set_telemetry(Some(t));
    assert_eq!(vm.limit_reason_text(), "Limited by power");
    assert!(vm.limit_is_active());
    vm.set_telemetry(None);
    assert!(!vm.limit_is_active());
}

#[test]
fn windows_can_queue_their_own_driver_jobs() {
    let f = fixture();
    let mut vm = vm(&f);
    vm.queue_custom("Setting fans…", |sh| {
        let errors = sh.service.set_fans(FanMode::Fixed, 55, &[], &FanCurve::default(), None);
        Box::new(move |vm: &mut MainViewModel| {
            assert!(errors.is_empty(), "{errors:?}");
            vm.sync_fans_from_service(FanMode::Fixed, 55, &[]);
        })
    });
    assert_eq!(vm.busy_label(), Some("Setting fans…"));
    vm.run_jobs_inline();
    assert!(!vm.busy());
    assert!(vm.is_fixed_fan() && vm.get(Field::FixedFan) == 55);
}

// ------------------------------------------------------------------ software temperature limit

/// The fixture on an RTX 50-style simulated card: no hardware temperature limit.
fn rtx50_fixture() -> Fixture {
    let dir = TempDir::new();
    let store = Arc::new(ProfileStore::new(Some(dir.0.clone())).unwrap());
    let service = Arc::new(TuningService::new(Box::new(MockBackend::without_hardware_temp_limit())));
    service.initialize(0).unwrap();
    let startup = Arc::new(FakeStartup::default());
    let shared = Shared::new(service, store, AppSettings::default(), startup.clone());
    let recovery = shared.open_recovery().unwrap();
    shared.set_recovery(Some(recovery));
    Fixture { _dir: dir, shared, startup }
}

/// A hot reading for the controller, with the clock the card is running.
fn hot(f: &Fixture) {
    let t = roch_core::GpuTelemetry { temperature_c: 88.0, core_clock_mhz: 2610.0, ..Default::default() };
    f.shared.service.step_soft_thermal(&t);
}

#[test]
fn soft_temp_row_replaces_a_missing_hardware_limit() {
    let f = fixture();
    let vm1 = vm(&f);
    assert!(vm1.has_temp_limit() && !vm1.has_soft_temp_limit(), "a card with its own limit keeps it");
    assert_eq!(vm1.build_profile_from_editor("x").soft_thermal_limit_c, 0);

    let f = rtx50_fixture();
    let vm = vm(&f);
    assert!(!vm.has_temp_limit() && vm.has_soft_temp_limit());
    assert_eq!(vm.soft_temp_label(), "Temperature Limit");
    let tip = vm.range_text(Field::SoftTempLimit);
    assert!(tip.starts_with("50 … 105 °C. The default is 88 °C"), "{tip}");
    for words in ["only while Roch GPU is running", "from the tray", "core clock is capped", "slowdown"] {
        assert!(tip.contains(words), "tooltip says {words:?}: {tip}");
    }
    // Nothing applied yet: the card's own limit, the default/off point.
    assert_eq!(vm.input_text(Field::SoftTempLimit), "88");
    assert_eq!(vm.slider_range(Field::SoftTempLimit), (50, 105));
    assert_eq!(vm.step(Field::SoftTempLimit), 1);
    assert_eq!(vm.build_profile_from_editor("x").soft_thermal_limit_c, 0);
}

#[test]
fn soft_temp_default_follows_each_cards_own_limit() {
    let f = rtx50_fixture();
    let mut vm = vm(&f);
    for card in [85, 88, 89, 93, 100, 105] {
        vm.caps.soft_thermal_card_limit_c = card;
        vm.load_into_editor(&TuningProfile::default());
        assert_eq!(vm.input_text(Field::SoftTempLimit), card.to_string(), "default on a {card} °C card");
        assert_eq!(vm.soft_temp_off_c(), card);
        assert_eq!(
            vm.build_profile_from_editor("x").soft_thermal_limit_c,
            0,
            "the displayed default is stored as off on a {card} °C card"
        );
    }
}

#[test]
fn soft_temp_value_box_and_nudges() {
    let f = rtx50_fixture();
    let mut vm = vm(&f);
    let typed = |vm: &mut MainViewModel, text: &str| {
        vm.commit_input(Field::SoftTempLimit, text);
        (vm.input_text(Field::SoftTempLimit), vm.build_profile_from_editor("x").soft_thermal_limit_c)
    };
    assert_eq!(typed(&mut vm, "83"), ("83".into(), 83));
    assert!(vm.pending_changes());
    assert_eq!(typed(&mut vm, "0"), ("88".into(), 0), "0 is off, the card's own limit");
    assert_eq!(typed(&mut vm, "75"), ("75".into(), 75));
    assert_eq!(typed(&mut vm, "off"), ("88".into(), 0));
    assert_eq!(typed(&mut vm, "70"), ("70".into(), 70));
    assert_eq!(typed(&mut vm, "card"), ("88".into(), 0));
    assert_eq!(typed(&mut vm, "30"), ("50".into(), 50), "below the window is its bottom");
    assert_eq!(typed(&mut vm, "99"), ("99".into(), 99), "above the card's limit, below the 105 °C top");
    assert_eq!(typed(&mut vm, "abc"), ("99".into(), 99), "garbage leaves it alone");
    assert_eq!(typed(&mut vm, "87"), ("87".into(), 87), "one below the card's limit is enforced");
    vm.nudge(Field::SoftTempLimit, 1);
    assert_eq!(vm.input_text(Field::SoftTempLimit), "88", "one step up from 87 is the card's own limit");
    assert_eq!(vm.build_profile_from_editor("x").soft_thermal_limit_c, 0);
    vm.nudge(Field::SoftTempLimit, 1);
    assert_eq!(vm.input_text(Field::SoftTempLimit), "89", "the window keeps going to 105");
    vm.nudge(Field::SoftTempLimit, 1);
    assert_eq!(vm.input_text(Field::SoftTempLimit), "90");
    vm.nudge(Field::SoftTempLimit, -1);
    assert_eq!(vm.input_text(Field::SoftTempLimit), "89");
    vm.nudge_spec("softtemp:-1");
    assert_eq!(vm.get(Field::SoftTempLimit), 88);
    assert_eq!(vm.build_profile_from_editor("x").soft_thermal_limit_c, 0);
    vm.set(Field::SoftTempLimit, 105);
    assert_eq!(vm.build_profile_from_editor("x").soft_thermal_limit_c, 105, "105 is kept");
    vm.nudge(Field::SoftTempLimit, 1);
    assert_eq!(vm.input_text(Field::SoftTempLimit), "105", "105 °C is the top");
}

#[test]
fn percent_and_mhz_units_sit_beside_the_value() {
    let f = fixture();
    let vm = vm(&f);
    // MHz, % and °C leave the labels and are drawn next to the number.
    assert_eq!(vm.core_label(), "Core Offset");
    assert_eq!(vm.memory_label(), "Memory Clock Offset");
    assert_eq!(vm.power_label(), "Power Limit");
    assert_eq!(vm.temp_label(), "Temperature Limit");
    assert_eq!(vm.voltage_boost_label(), "Voltage Boost");
    for f in [
        Field::CoreOffset,
        Field::XbarOffset,
        Field::SysOffset,
        Field::VideoOffset,
        Field::MemoryOffset,
        Field::ClockLockMin,
        Field::ClockLockMax,
    ] {
        assert_eq!(vm.field_unit(f), "MHz", "{f:?}");
    }
    for f in [Field::PowerLimit, Field::VoltageBoost] {
        assert_eq!(vm.field_unit(f), "%", "{f:?}");
    }
    assert_eq!(vm.field_unit(Field::TempLimit), "°C");
    assert_eq!(vm.field_unit(Field::SoftTempLimit), "°C");
    // Units still shown in their labels.
    assert_eq!(vm.field_unit(Field::VoltageOffset), "");
}

#[test]
fn soft_temp_applies_shows_live_state_and_saves_in_slots() {
    let f = rtx50_fixture();
    let mut vm = vm(&f);
    vm.commit_input(Field::SoftTempLimit, "70");
    vm.apply();
    vm.run_jobs_inline();
    assert!(!vm.status_is_error, "{}", vm.status);
    let s = f.shared.service.soft_thermal_status();
    assert!(s.enabled && s.limit_c == 70 && !s.active, "{s:?}");
    assert!(vm.applied_summary().contains("temp 70°C (software)"), "{}", vm.applied_summary());
    assert_eq!(vm.limit_reason_text(), "Limiter — open the monitor to sample");

    // The card passes the limit: the limiter line says what is holding it, even with no sample.
    hot(&f);
    let s = f.shared.service.soft_thermal_status();
    assert!(s.active, "{s:?}");
    assert_eq!(vm.limit_reason_text(), format!("Thermal limit active: capped at {} MHz", s.ceiling_mhz));
    assert!(vm.limit_is_active());

    // Saved in a slot like everything else, and loaded back from it.
    vm.set_save_armed(true);
    vm.on_slot_clicked(1);
    let saved = f.shared.store.load("Slot 1").unwrap().unwrap();
    assert_eq!(saved.soft_thermal_limit_c, 70);
    assert!(vm.slots()[0].summary.contains("soft temp 70 °C"), "{}", vm.slots()[0].summary);

    // Reset turns it off, hands the clock back and shows the card's own limit.
    vm.reset();
    vm.run_jobs_inline();
    assert_eq!(vm.input_text(Field::SoftTempLimit), "88");
    assert!(!f.shared.service.soft_thermal_status().enabled);
    assert_eq!(f.shared.service.read_tuning_state().unwrap().locked_clock_max_mhz, 0);
    assert!(!vm.limit_is_active());

    vm.on_slot_clicked(1);
    assert_eq!(vm.input_text(Field::SoftTempLimit), "70");
    vm.run_jobs_inline();
    assert!(f.shared.service.soft_thermal_status().enabled);

    // A new window (the app restarted from the tray, say) seeds the row from what is running.
    let again = MainViewModel::new(f.shared.clone());
    assert_eq!(again.input_text(Field::SoftTempLimit), "70");
}

#[test]
fn soft_temp_and_an_armed_clock_range_agree() {
    let f = rtx50_fixture();
    let mut vm = vm(&f);
    vm.set(Field::ClockLockMin, 1500);
    vm.set(Field::ClockLockMax, 2700);
    vm.commit_input(Field::SoftTempLimit, "70");
    vm.set_lever(XocLever::CLOCK_RANGE, true);
    vm.run_jobs_inline();
    assert!(vm.is_armed(XocLever::CLOCK_RANGE), "{}", vm.status);
    vm.apply();
    vm.run_jobs_inline();
    assert_eq!(vm.soft_thermal_clock_range_note(), None, "nothing to say while the clock is free");
    hot(&f);
    let note = vm.soft_thermal_clock_range_note().expect("a note while holding");
    assert!(note.starts_with("The software temperature limit (70 °C) is holding the clock at or below "), "{note}");
    // The sliders keep the user's range; the card has the capped one inside it.
    assert_eq!((vm.get(Field::ClockLockMin), vm.get(Field::ClockLockMax)), (1500, 2700));
    let live = f.shared.service.read_tuning_state().unwrap();
    assert_eq!(live.locked_clock_min_mhz, 1500);
    assert!(live.locked_clock_max_mhz < 2700, "{live:?}");
    assert_eq!(vm.xoc_status_text(), "Live on the card: clock range.");
    // Exit: the user's own range comes back.
    f.shared.service.release_soft_thermal_limit();
    let live = f.shared.service.read_tuning_state().unwrap();
    assert_eq!((live.locked_clock_min_mhz, live.locked_clock_max_mhz), (1500, 2700));
}
