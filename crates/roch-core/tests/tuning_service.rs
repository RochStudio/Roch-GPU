//! TuningService against the mock and two shaped test backends, ported from
//! tests/GpuTuner.Core.Tests/Program.cs. Each block names the C# section it came from.

use std::any::Any;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use roch_core::mock::MockBackend;
use roch_core::models::voltage_plan;
use roch_core::services::TuningService;
use roch_core::*;

// ------------------------------------------------------------------ helpers

fn new_svc() -> TuningService {
    let s = TuningService::new(Box::new(MockBackend::new()));
    s.initialize(0).unwrap();
    s
}

fn st(svc: &TuningService) -> GpuTuningState {
    svc.read_tuning_state().unwrap()
}

fn mock<R>(svc: &TuningService, f: impl FnOnce(&mut MockBackend) -> R) -> R {
    svc.with_backend(|b| f(b.as_mock().expect("mock backend")))
}

fn calls(svc: &TuningService) -> Vec<String> {
    mock(svc, |m| m.calls.clone())
}

fn has_call(svc: &TuningService, name: &str) -> bool {
    calls(svc).iter().any(|c| c == name)
}

fn last_index(calls: &[String], name: &str) -> i64 {
    calls.iter().rposition(|c| c == name).map(|i| i as i64).unwrap_or(-1)
}

fn curve(points: &[(f64, f64)]) -> FanCurve {
    FanCurve::with_points(points.iter().map(|&(t, f)| FanPoint::new(t, f)).collect())
}

fn profile() -> TuningProfile {
    TuningProfile::default()
}

/// A manual-reset event, standing in for `ManualResetEventSlim`.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    fn set(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
    fn is_set(&self) -> bool {
        *self.0.0.lock().unwrap()
    }
    fn wait(&self, timeout: Duration) -> bool {
        let g = self.0.0.lock().unwrap();
        let (g, _) = self.0.1.wait_timeout_while(g, timeout, |set| !*set).unwrap();
        *g
    }
}

/// Minimal AMD-shaped backend: records what Apply asked the driver to do.
struct OffsetStyleBackend {
    calls: Vec<String>,
    voltage_offset_mv: i32,
    core_offset_mhz: i32,
    memory_mhz: i32,
    power_percent: i32,
    zero_rpm: bool,
    memory_timing: i32,
    boost_percent: i32,
    override_caps: Option<GpuCapabilities>,
    current_curve: Option<FanCurve>,
    devices: Vec<GpuDevice>,
}

impl OffsetStyleBackend {
    fn new() -> Self {
        Self {
            calls: Vec::new(),
            voltage_offset_mv: 0,
            core_offset_mhz: 0,
            memory_mhz: 2518,
            power_percent: 0,
            zero_rpm: true,
            memory_timing: 0,
            boost_percent: 0,
            override_caps: None,
            current_curve: None,
            devices: vec![GpuDevice {
                index: 0,
                name: "Test RDNA".into(),
                vendor: "AMD".into(),
                bus_id: "PCI".into(),
                driver_version: "1.0".into(),
                vram_megabytes: 16384,
                bios_version: String::new(),
            }],
        }
    }

    fn call(&mut self, name: &str) {
        self.calls.push(name.into());
    }
}

impl GpuBackend for OffsetStyleBackend {
    fn backend_name(&self) -> String {
        "Offset-style test GPU".into()
    }
    fn initialize(&mut self) -> Result<()> {
        Ok(())
    }
    fn devices(&self) -> &[GpuDevice] {
        &self.devices
    }
    fn capabilities(&mut self, _gpu: usize) -> Result<GpuCapabilities> {
        Ok(self.override_caps.clone().unwrap_or_else(|| GpuCapabilities {
            can_set_core_offset: true,
            core_offset_min_mhz: -500,
            core_offset_max_mhz: 1000,
            can_set_memory_offset: true,
            memory_offset_min_mhz: 2518,
            memory_offset_max_mhz: 3000,
            memory_clock_is_absolute: true,
            memory_clock_default_mhz: 2518,
            can_set_power_limit: true,
            power_limit_min_percent: -30,
            power_limit_max_percent: 10,
            power_limit_default_percent: 0,
            power_limit_is_offset: true,
            can_set_temp_limit: false,
            voltage_style: VoltageControlStyle::Offset,
            voltage_offset_min_mv: -200,
            voltage_offset_max_mv: 0,
            can_set_voltage_curve: false,
            can_read_voltage: true,
            can_set_zero_rpm: true,
            zero_rpm_default: true,
            can_set_memory_timing: true,
            memory_timing_options: vec!["Default".into(), "Fast timing".into()],
            can_set_fan_speed: true,
            fan_curve_is_hardware: true,
            fan_curve_points: 5,
            fan_min_percent: 30,
            fan_max_percent: 100,
            fan_count: 1,
            ..Default::default()
        }))
    }
    fn read_telemetry(&mut self, _gpu: usize) -> Result<GpuTelemetry> {
        Ok(GpuTelemetry { core_clock_mhz: 2900.0, voltage_mv: 900.0, ..Default::default() })
    }
    fn read_tuning_state(&mut self, _gpu: usize) -> Result<GpuTuningState> {
        Ok(GpuTuningState {
            core_offset_mhz: self.core_offset_mhz,
            memory_offset_mhz: self.memory_mhz,
            power_limit_percent: self.power_percent,
            voltage_offset_mv: self.voltage_offset_mv,
            zero_rpm: self.zero_rpm,
            memory_timing_level: self.memory_timing,
            voltage_boost_percent: self.boost_percent,
            detected_fan_mode: self.current_curve.as_ref().map(|_| FanMode::Curve),
            hardware_fan_curve: self.current_curve.clone(),
            ..Default::default()
        })
    }
    fn set_core_offset(&mut self, _gpu: usize, mhz: i32) -> Result<()> {
        self.call("SetCoreOffset");
        self.core_offset_mhz = mhz;
        Ok(())
    }
    fn set_memory_offset(&mut self, _gpu: usize, mhz: i32) -> Result<()> {
        self.call("SetMemoryOffset");
        self.memory_mhz = mhz;
        Ok(())
    }
    fn set_power_limit(&mut self, _gpu: usize, pct: i32) -> Result<()> {
        self.call("SetPowerLimit");
        self.power_percent = pct;
        Ok(())
    }
    fn set_temp_limit(&mut self, _gpu: usize, _c: i32) -> Result<()> {
        self.call("SetTempLimit");
        Ok(())
    }
    fn set_voltage_boost(&mut self, _gpu: usize, pct: i32) -> Result<()> {
        self.call("SetVoltageBoost");
        self.boost_percent = pct;
        Ok(())
    }
    fn set_voltage_curve_offset(&mut self, _gpu: usize, mv: i32, _extra: i32) -> Result<()> {
        self.call("SetVoltageCurveOffset");
        self.voltage_offset_mv = mv;
        Ok(())
    }
    fn set_voltage_lock(&mut self, _gpu: usize, _mv: i32) -> Result<()> {
        self.call("SetVoltageLock");
        Ok(())
    }
    fn set_zero_rpm(&mut self, _gpu: usize, on: bool) -> Result<()> {
        self.call("SetZeroRpm");
        self.zero_rpm = on;
        Ok(())
    }
    fn set_memory_timing(&mut self, _gpu: usize, level: i32) -> Result<()> {
        self.call("SetMemoryTiming");
        self.memory_timing = level;
        Ok(())
    }
    fn set_fan_curve(&mut self, _gpu: usize, _c: &FanCurve) -> Result<()> {
        self.call("SetFanCurve");
        Ok(())
    }
    fn set_fan_speed(&mut self, _gpu: usize, _fan: i32, _pct: i32) -> Result<()> {
        self.call("SetFanSpeed");
        Ok(())
    }
    fn set_fan_auto(&mut self, _gpu: usize) -> Result<()> {
        self.call("SetFanAuto");
        Ok(())
    }
    fn reset_to_defaults(&mut self, _gpu: usize) -> Result<()> {
        self.call("ResetToDefaults");
        Ok(())
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

fn offset<R>(svc: &TuningService, f: impl FnOnce(&mut OffsetStyleBackend) -> R) -> R {
    svc.with_backend(|b| f(b.as_any_mut().unwrap().downcast_mut::<OffsetStyleBackend>().unwrap()))
}

type Hook = Option<Box<dyn Fn() + Send>>;

/// Wraps the mock and counts which telemetry path the poll loop took, so BackgroundMode is tested
/// for what it costs rather than just what it returns.
#[derive(Default)]
struct CountingBackend {
    inner: MockBackend,
    full_reads: usize,
    temp_reads: usize,
    last_fan_percent: i32,
    before_telemetry: Hook,
    before_ui_call: Hook,
}

impl CountingBackend {
    fn reset(&mut self) {
        self.full_reads = 0;
        self.temp_reads = 0;
    }
    fn ui(&self) {
        if let Some(f) = &self.before_ui_call {
            f();
        }
    }
}

impl GpuBackend for CountingBackend {
    fn backend_name(&self) -> String {
        "Counting".into()
    }
    fn initialize(&mut self) -> Result<()> {
        self.inner.initialize()
    }
    fn devices(&self) -> &[GpuDevice] {
        self.inner.devices()
    }
    fn read_graphics_info(&mut self, gpu: usize) -> GpuGraphicsInfo {
        self.ui();
        GpuGraphicsInfo::from_device(&self.inner.devices()[gpu])
    }
    fn capabilities(&mut self, gpu: usize) -> Result<GpuCapabilities> {
        self.inner.capabilities(gpu)
    }
    fn read_telemetry(&mut self, gpu: usize) -> Result<GpuTelemetry> {
        if let Some(f) = &self.before_telemetry {
            f();
        }
        self.full_reads += 1;
        self.inner.read_telemetry(gpu)
    }
    fn read_temperature_only(&mut self, _gpu: usize) -> Result<GpuTelemetry> {
        self.temp_reads += 1;
        Ok(GpuTelemetry { temperature_c: 55.0, ..Default::default() })
    }
    fn read_tuning_state(&mut self, gpu: usize) -> Result<GpuTuningState> {
        self.ui();
        self.inner.read_tuning_state(gpu)
    }
    fn read_voltage_lock_mv(&mut self, gpu: usize) -> i32 {
        self.ui();
        self.inner.read_voltage_lock_mv(gpu)
    }
    fn set_voltage_lock(&mut self, gpu: usize, mv: i32) -> Result<()> {
        self.ui();
        self.inner.set_voltage_lock(gpu, mv)
    }
    fn set_core_offset(&mut self, gpu: usize, mhz: i32) -> Result<()> {
        self.inner.set_core_offset(gpu, mhz)
    }
    fn set_memory_offset(&mut self, gpu: usize, mhz: i32) -> Result<()> {
        self.inner.set_memory_offset(gpu, mhz)
    }
    fn set_power_limit(&mut self, gpu: usize, pct: i32) -> Result<()> {
        self.inner.set_power_limit(gpu, pct)
    }
    fn set_temp_limit(&mut self, gpu: usize, c: i32) -> Result<()> {
        self.inner.set_temp_limit(gpu, c)
    }
    fn set_voltage_boost(&mut self, gpu: usize, pct: i32) -> Result<()> {
        self.inner.set_voltage_boost(gpu, pct)
    }
    fn set_voltage_curve_offset(&mut self, gpu: usize, mv: i32, extra: i32) -> Result<()> {
        self.inner.set_voltage_curve_offset(gpu, mv, extra)
    }
    fn set_fan_speed(&mut self, gpu: usize, fan: i32, pct: i32) -> Result<()> {
        self.last_fan_percent = pct;
        self.inner.set_fan_speed(gpu, fan, pct)
    }
    fn set_fan_auto(&mut self, gpu: usize) -> Result<()> {
        self.inner.set_fan_auto(gpu)
    }
    fn reset_to_defaults(&mut self, gpu: usize) -> Result<()> {
        self.inner.reset_to_defaults(gpu)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

fn counting<R>(svc: &TuningService, f: impl FnOnce(&mut CountingBackend) -> R) -> R {
    svc.with_backend(|b| f(b.as_any_mut().unwrap().downcast_mut::<CountingBackend>().unwrap()))
}

// ------------------------------------------------------------------ tests

/// "TuningService against mock: apply, fan curve drives fan, release returns auto"
#[test]
fn apply_fan_curve_release_reset() {
    let svc = new_svc();
    let errs = svc.apply(&TuningProfile {
        core_offset_mhz: 120,
        memory_offset_mhz: 500,
        power_limit_percent: 90,
        temp_limit_c: 80,
        voltage_boost_percent: 25,
        fan_mode: FanMode::Fixed,
        fixed_fan_percent: 66,
        ..profile()
    });
    assert!(errs.is_empty(), "apply no errors: {errs:?}");
    let s = st(&svc);
    assert_eq!(s.core_offset_mhz, 120, "mock applied core");
    assert_eq!(s.voltage_boost_percent, 25, "mock applied voltage boost");
    assert!(s.fan_manual && s.fan_percent == 66, "mock applied fan fixed");

    // Out-of-range value gets clamped rather than rejected, and the clamp is noted.
    let errs = svc.apply(&TuningProfile { core_offset_mhz: 5000, ..profile() });
    assert_eq!(st(&svc).core_offset_mhz, 1000, "apply clamps");
    assert_eq!(errs, vec!["Note: core offset 5000 MHz is outside this card's range — applied 1000 MHz instead."]);
    assert!(only_notes(&errs));

    let cp = TuningProfile { fan_mode: FanMode::Curve, fan_curve: curve(&[(0.0, 77.0), (120.0, 77.0)]), ..profile() };
    svc.apply(&cp);
    svc.start_polling(20).unwrap();
    thread::sleep(Duration::from_millis(300));
    svc.stop_polling().unwrap();
    let s2 = st(&svc);
    assert!(s2.fan_manual && s2.fan_percent == 77, "curve drove fan to 77");

    svc.release_fan_control();
    assert!(!st(&svc).fan_manual, "release → auto");

    svc.reset_to_defaults().unwrap();
    assert_eq!(st(&svc).core_offset_mhz, 0, "reset");
}

/// "Clock range: a window the card is held inside, armed like the other levers."
#[test]
fn clock_range_is_a_gated_lever() {
    let svc = new_svc();
    svc.apply(&TuningProfile {
        xoc_armed: XocLever::CLOCK_RANGE,
        clock_lock_min_mhz: 1500,
        clock_lock_max_mhz: 1800,
        ..profile()
    });
    let s = st(&svc);
    assert_eq!((s.locked_clock_min_mhz, s.locked_clock_max_mhz), (1500, 1800), "clock range applied");

    svc.apply(&profile());
    let s = st(&svc);
    assert_eq!((s.locked_clock_min_mhz, s.locked_clock_max_mhz), (0, 0), "clock range cleared when unset");

    svc.apply(&TuningProfile { clock_lock_min_mhz: 900, clock_lock_max_mhz: 1200, ..profile() });
    assert_eq!(st(&svc).locked_clock_min_mhz, 0, "clock range disarmed stays unpinned");

    svc.apply(&TuningProfile {
        xoc_armed: XocLever::CLOCK_RANGE,
        clock_lock_min_mhz: 900,
        clock_lock_max_mhz: 1200,
        ..profile()
    });
    let s = st(&svc);
    assert_eq!((s.locked_clock_min_mhz, s.locked_clock_max_mhz), (900, 1200), "clock range armed pins the card");

    svc.reset_to_defaults().unwrap();
    let s = st(&svc);
    assert_eq!((s.locked_clock_min_mhz, s.locked_clock_max_mhz), (0, 0), "reset unpins the clock");
}

/// "XOC gate": never-armed rails belong to the driver; Voltage Boost moves their baseline.
#[test]
fn disabled_rails_follow_the_driver_baseline_under_boost() {
    let svc = new_svc();
    svc.seed_rail_defaults(1035, 985);
    mock(&svc, |m| m.calls.clear());

    svc.apply(&TuningProfile { voltage_boost_percent: 100, xoc_armed: XocLever::NONE, ..profile() });
    let boosted = st(&svc);
    assert_eq!(boosted.voltage_rail_max_mv, 1055, "voltage boost leaves disabled NVVDD on driver-derived ceiling");
    assert_eq!(boosted.msvdd_rail_max_mv, 1005, "voltage boost leaves disabled MSVDD on driver-derived ceiling");
    assert!(
        !has_call(&svc, "SetVoltageRailMax") && !has_call(&svc, "SetMsvddRailMax"),
        "normal boost apply does not write disabled rail ceilings"
    );

    svc.apply(&TuningProfile { voltage_boost_percent: 0, ..profile() });
    let s = st(&svc);
    assert_eq!((s.voltage_rail_max_mv, s.msvdd_rail_max_mv), (1035, 985), "zero boost returns disabled rails to stock");

    let explicit = TuningProfile {
        voltage_boost_percent: 100,
        xoc_armed: XocLever::NVVDD | XocLever::MSVDD,
        voltage_rail_max_mv: 1075,
        msvdd_rail_max_mv: 1050,
        ..profile()
    };
    svc.apply(&explicit);
    svc.apply(&TuningProfile { voltage_boost_percent: 100, ..profile() });
    let s = st(&svc);
    assert_eq!(s.voltage_rail_max_mv, 1055, "disabling NVVDD at full boost restores its boosted driver baseline");
    assert_eq!(s.msvdd_rail_max_mv, 1005, "disabling MSVDD at full boost restores its boosted driver baseline");

    svc.apply(&explicit);
    svc.set_xoc_lever(&explicit, XocLever::NVVDD, false);
    let s = st(&svc);
    assert_eq!((s.voltage_rail_max_mv, s.msvdd_rail_max_mv), (1055, 1050), "per-lever NVVDD disable");
    svc.set_xoc_lever(&explicit, XocLever::MSVDD, false);
    assert_eq!(st(&svc).msvdd_rail_max_mv, 1005, "per-lever MSVDD disable restores boosted driver baseline");
}

/// "XOC gate: the rails and crossbar are written only while armed, and disarming puts them back."
#[test]
fn xoc_levers_arm_disarm_and_reset() {
    let svc = new_svc();
    svc.seed_rail_defaults(1035, 985);
    let armed = TuningProfile {
        xoc_armed: XocLever::ALL,
        voltage_rail_max_mv: 1075,
        msvdd_rail_max_mv: 1050,
        voltage_rail_floor_mv: 850,
        msvdd_rail_floor_mv: 850,
        xbar_offset_mhz: 100,
        sys_offset_mhz: 45,
        video_offset_mhz: 30,
        ..profile()
    };
    svc.apply(&armed);
    let s = st(&svc);
    assert_eq!(s.voltage_rail_max_mv, 1075, "xoc armed writes nvvdd");
    assert_eq!(s.msvdd_rail_max_mv, 1050, "xoc armed writes msvdd");
    assert!(s.voltage_rail_floor_mv == 850 && s.msvdd_rail_floor_mv == 850, "xoc armed writes floors");
    assert_eq!(s.xbar_offset_mhz, 100, "xoc armed writes xbar");
    assert!(s.sys_offset_mhz == 45 && s.video_offset_mhz == 30, "xoc armed writes sys and video");

    svc.apply(&TuningProfile { xoc_armed: XocLever::NONE, ..armed.clone() });
    let s = st(&svc);
    assert_eq!(s.voltage_rail_max_mv, 1035, "xoc disarmed restores nvvdd default");
    assert_eq!(s.msvdd_rail_max_mv, 985, "xoc disarmed restores msvdd default");
    assert!(s.voltage_rail_floor_mv == 0 && s.msvdd_rail_floor_mv == 0, "xoc disarmed zeroes floors");
    assert_eq!(s.xbar_offset_mhz, 0, "xoc disarmed zeroes xbar");
    assert!(s.sys_offset_mhz == 0 && s.video_offset_mhz == 0, "xoc disarmed zeroes sys and video");

    // One lever's Enable/Disable moves that lever and nothing else.
    svc.apply(&TuningProfile { core_offset_mhz: 150, power_limit_percent: 110, ..profile() });
    svc.set_xoc_lever(&armed, XocLever::NVVDD, true);
    let s = st(&svc);
    assert_eq!(s.voltage_rail_max_mv, 1075, "SetXocLever(Nvvdd) writes that rail");
    assert_eq!(s.msvdd_rail_max_mv, 985, "SetXocLever(Nvvdd) leaves msvdd alone");
    assert_eq!(s.xbar_offset_mhz, 0, "SetXocLever(Nvvdd) leaves xbar alone");
    assert_eq!(s.core_offset_mhz, 150, "SetXocLever leaves core alone");
    assert_eq!(s.power_limit_percent, 110, "SetXocLever leaves power alone");

    svc.set_xoc_lever(&armed, XocLever::XBAR, true);
    let s = st(&svc);
    assert_eq!(s.xbar_offset_mhz, 100, "SetXocLever(Xbar) writes that domain");
    assert_eq!(s.voltage_rail_max_mv, 1075, "SetXocLever(Xbar) leaves the armed rail standing");

    svc.set_xoc_lever(&armed, XocLever::NVVDD, false);
    let s = st(&svc);
    assert_eq!(s.voltage_rail_max_mv, 1035, "SetXocLever(Nvvdd, false) restores that rail");
    assert_eq!(s.xbar_offset_mhz, 100, "SetXocLever(Nvvdd, false) leaves xbar armed");
    assert_eq!(s.core_offset_mhz, 150, "SetXocLever(false) leaves core alone");

    svc.set_xoc_lever(&armed, XocLever::XBAR, false);
    assert_eq!(st(&svc).xbar_offset_mhz, 0, "SetXocLever(Xbar, false) zeroes that domain");

    // An unknown default is left alone rather than guessed at: guessing low browns the card out.
    {
        let bare = new_svc();
        bare.apply(&TuningProfile { xoc_armed: XocLever::NVVDD, voltage_rail_max_mv: 1100, ..profile() });
        bare.apply(&profile());
        assert_eq!(st(&bare).voltage_rail_max_mv, 1100, "xoc disarm without a recorded default leaves the ceiling");
    }

    // Reset means every gated lever, not the ones somebody remembered to list.
    svc.apply(&armed);
    svc.reset_to_defaults().unwrap();
    let s = st(&svc);
    assert_eq!(s.xbar_offset_mhz, 0, "reset clears the crossbar");
    assert!(s.sys_offset_mhz == 0 && s.video_offset_mhz == 0, "reset clears sys and video");
    assert!(s.voltage_rail_max_mv == 1035 && s.msvdd_rail_max_mv == 985, "reset restores both rail ceilings");
    assert!(s.voltage_rail_floor_mv == 0 && s.msvdd_rail_floor_mv == 0, "reset zeroes both rail floors");
    assert!(s.locked_clock_min_mhz == 0 && s.locked_clock_max_mhz == 0, "reset unpins the clock range");
    assert_eq!(svc.armed_on_card(), XocLever::NONE, "reset leaves nothing armed on the card");

    // ArmedOnCard is what was written, not what a profile asked for.
    svc.set_xoc_lever(&armed, XocLever::SYS, true);
    assert_eq!(svc.armed_on_card(), XocLever::SYS, "arming one lever records only it");
    svc.set_xoc_lever(&armed, XocLever::VIDEO, true);
    assert_eq!(svc.armed_on_card(), XocLever::SYS | XocLever::VIDEO, "arming a second records both");
    svc.set_xoc_lever(&armed, XocLever::SYS, false);
    assert_eq!(svc.armed_on_card(), XocLever::VIDEO, "disarming clears only that one");
    svc.reset_to_defaults().unwrap();

    assert_eq!(armed.clone_profile().xoc_armed, XocLever::ALL, "gates survive clone");
    assert_eq!(
        TuningProfile::stock(&svc.capabilities(), Some("x")).xoc_armed,
        XocLever::NONE,
        "stock profile is disarmed"
    );
    // The applied profile keeps describing the card after a per-lever write.
    svc.apply(&profile());
    svc.set_xoc_lever(&armed, XocLever::XBAR, true);
    let ap = svc.applied_profile().unwrap();
    assert!(ap.xoc_armed.has(XocLever::XBAR) && ap.xbar_offset_mhz == 100);
}

/// "Fan writes go through one routine, so an apply and the fan window cannot disagree."
#[test]
fn fan_writes_share_one_routine() {
    let svc = new_svc();
    svc.apply(&TuningProfile { fan_mode: FanMode::Fixed, fixed_fan_percent: 55, ..profile() });
    let s = st(&svc);
    assert!(s.fan_percent == 55 && s.fan_manual, "a shared duty reaches the fans");

    svc.set_fans(FanMode::Fixed, 40, &[40, 40, 40], &FanCurve::default(), None);
    svc.apply(&TuningProfile {
        fan_mode: FanMode::Fixed,
        fixed_fan_percent: 70,
        fixed_fan_percents: vec![70],
        ..profile()
    });
    assert!(mock(&svc, |m| m.fan_percents).iter().all(|&v| v == 70), "a one-entry list reaches every cooler");

    svc.set_fans(FanMode::Fixed, 45, &[45, 65, 90], &FanCurve::default(), None);
    assert_eq!(mock(&svc, |m| m.fan_percents), [45, 65, 90], "a per-fan list addresses each cooler");

    svc.apply(&TuningProfile { fan_mode: FanMode::Fixed, fixed_fan_percent: 250, ..profile() });
    let caps = svc.capabilities();
    let max = if caps.fan_max_percent <= 0 { 100 } else { caps.fan_max_percent };
    assert_eq!(st(&svc).fan_percent, max, "an apply clamps the fan duty");

    svc.set_fans(FanMode::Auto, 50, &[], &FanCurve::default(), None);
    assert!(!st(&svc).fan_manual, "auto releases the fans");
}

/// "BackgroundMode: what the poll costs when nothing is on screen"
#[test]
fn background_mode_costs() {
    let svc = TuningService::new(Box::new(CountingBackend::default()));
    svc.initialize(0).unwrap();

    svc.start_polling(20).unwrap();
    thread::sleep(Duration::from_millis(200));
    svc.stop_polling().unwrap();
    assert!(counting(&svc, |c| c.full_reads) > 0, "foreground reads full telemetry");
    assert!(!svc.history().snapshot().is_empty(), "foreground fills history");
    assert!(svc.latest().is_some(), "foreground sets Latest");

    counting(&svc, |c| c.reset());
    svc.set_background_mode(true);
    svc.start_polling(20).unwrap();
    thread::sleep(Duration::from_millis(200));
    svc.stop_polling().unwrap();
    assert_eq!(counting(&svc, |c| c.full_reads), 0, "background idle: no full reads");
    assert_eq!(counting(&svc, |c| c.temp_reads), 0, "background idle: no temperature reads either");

    let history_before = svc.history().snapshot().len();
    svc.apply(&TuningProfile {
        fan_mode: FanMode::Curve,
        fan_curve: curve(&[(0.0, 44.0), (120.0, 44.0)]),
        ..profile()
    });
    counting(&svc, |c| c.reset()); // Apply itself reads state; only count the polls
    svc.start_polling(20).unwrap();
    thread::sleep(Duration::from_millis(200));
    svc.stop_polling().unwrap();
    assert_eq!(counting(&svc, |c| c.full_reads), 0, "background curve: no full reads");
    assert!(counting(&svc, |c| c.temp_reads) > 0, "background curve: temperature read instead");
    assert_eq!(counting(&svc, |c| c.last_fan_percent), 44, "background curve still drives the fan");
    assert_eq!(svc.history().snapshot().len(), history_before, "background does not grow history");
}

/// Leaving background mode samples at once instead of waiting out the interval, so a window
/// that shows readings fills in immediately; staying in it (or entering it) wakes nothing.
#[test]
fn leaving_background_mode_samples_at_once() {
    let svc = TuningService::new(Box::new(CountingBackend::default()));
    svc.initialize(0).unwrap();
    svc.set_background_mode(true);
    svc.start_polling(60_000).unwrap();
    thread::sleep(Duration::from_millis(100));
    svc.set_background_mode(true);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(counting(&svc, |c| c.full_reads), 0, "background: no reads, and no wake while staying there");

    let events = svc.subscribe();
    svc.set_background_mode(false);
    let sample = events.recv_timeout(Duration::from_secs(5));
    svc.stop_polling().unwrap();
    assert!(sample.is_ok(), "a sample arrived long before the 60 s interval");
    assert_eq!(counting(&svc, |c| c.full_reads), 1);
}

/// "Stock ceiling tracking, and the voltage levers end to end through the service."
#[test]
fn stock_ceiling_tracking_and_voltage_levers() {
    let svc = new_svc();
    let caps = svc.capabilities();
    let curve_top = caps.stock_max_voltage_mv; // 1100 on the mock
    let max_mv = caps.max_voltage_mv; // 1160
    assert_eq!(svc.stock_ceiling_mv(), curve_top, "ceiling starts at the curve top");

    svc.note_voltage_observation((curve_top + 10) as f64, 100.0);
    assert_eq!(svc.stock_ceiling_mv(), curve_top + 10, "ceiling follows a nearby observation");

    svc.note_voltage_observation(max_mv as f64, 100.0);
    assert_eq!(svc.stock_ceiling_mv(), curve_top + 10, "ceiling ignores a boosted observation");

    // Idle samples say nothing about the ceiling.
    let idle = new_svc();
    idle.note_voltage_observation(1000.0, 5.0);
    assert_eq!(idle.observed_max_voltage_mv(), 0, "idle sample ignored");

    let boosted = new_svc();
    let legacy_target =
        TuningProfile { target_voltage_mv: max_mv, power_limit_percent: 100, temp_limit_c: 83, ..profile() };
    boosted.apply(&legacy_target);
    boosted.note_voltage_observation(max_mv as f64, 100.0);
    assert_eq!(boosted.stock_ceiling_mv(), curve_top, "no ceiling drift while boosting");
    assert_eq!(boosted.observed_max_boosted_voltage_mv(), max_mv, "a full boost measures the boosted roof");

    let again = boosted.apply(&legacy_target);
    assert!(again.is_empty(), "second boost has no errors: {again:?}");
    assert_eq!(st(&boosted).voltage_boost_percent, 100, "second boost still boosts");
    assert_eq!(boosted.read_voltage_lock_mv(), 0, "boost leaves no voltage lock");

    let capped = new_svc();
    let errs = capped.apply(&TuningProfile { target_voltage_mv: 950, ..profile() });
    assert!(errs.is_empty(), "cap applies cleanly: {errs:?}");
    assert_eq!(capped.read_voltage_lock_mv(), 950, "cap becomes a 950 mV lock");
    capped.apply(&TuningProfile { target_voltage_mv: curve_top, ..profile() });
    assert_eq!(capped.read_voltage_lock_mv(), 0, "back to stock clears the lock");

    // Boost and cap are independent levers.
    let b = new_svc();
    b.apply(&TuningProfile { voltage_boost_percent: 50, ..profile() });
    assert_eq!(st(&b).voltage_boost_percent, 50, "boost alone sets the percentage");
    assert_eq!(b.read_voltage_lock_mv(), 0, "boost alone leaves no cap");

    let c = new_svc();
    c.apply(&TuningProfile { target_voltage_mv: 900, ..profile() });
    assert_eq!(c.read_voltage_lock_mv(), 900, "cap alone locks at 900");
    assert_eq!(st(&c).voltage_boost_percent, 0, "cap alone does not boost");

    let both = new_svc();
    both.apply(&TuningProfile { voltage_boost_percent: 40, target_voltage_mv: 950, ..profile() });
    assert_eq!(st(&both).voltage_boost_percent, 40, "both: boost kept");
    assert_eq!(both.read_voltage_lock_mv(), 950, "both: cap kept");

    let boosted_cap = new_svc();
    boosted_cap.apply(&TuningProfile { voltage_boost_percent: 100, target_voltage_mv: curve_top + 30, ..profile() });
    assert_eq!(boosted_cap.read_voltage_lock_mv(), curve_top + 30, "cap above stock but under the roof still locks");
    assert_eq!(st(&boosted_cap).voltage_boost_percent, 100, "cap above stock keeps its boost");

    let at_roof = new_svc();
    at_roof.apply(&TuningProfile { voltage_boost_percent: 100, target_voltage_mv: max_mv, ..profile() });
    assert_eq!(at_roof.read_voltage_lock_mv(), 0, "cap at the boosted roof locks nothing");

    let reach = new_svc();
    assert_eq!(reach.reachable_ceiling_mv(0), curve_top, "reach with no boost is the stock ceiling");
    assert_eq!(reach.reachable_ceiling_mv(100), max_mv, "reach at full boost is the roof");

    let expl = new_svc();
    expl.apply(&TuningProfile { voltage_boost_percent: 25, target_voltage_mv: max_mv, ..profile() });
    assert_eq!(st(&expl).voltage_boost_percent, 25, "explicit boost wins over a high target");

    let legacy = new_svc();
    legacy.apply(&legacy_target);
    assert_eq!(st(&legacy).voltage_boost_percent, 100, "legacy target above ceiling still boosts");
    assert_eq!(legacy.read_voltage_lock_mv(), 0, "legacy boost sets no cap");

    let read = new_svc();
    read.apply(&TuningProfile { voltage_boost_percent: 60, ..profile() });
    let back = read.read_current_as_profile().unwrap();
    assert_eq!(back.voltage_boost_percent, 60, "read back: boost preserved");
    assert_eq!(back.target_voltage_mv, 0, "read back: no phantom cap");

    assert_eq!(voltage_plan::compute(950, max_mv, max_mv), (0, 950 - max_mv), "unknown curve falls back");

    // --uv: a negative offset on an absolute card becomes a cap under the ceiling.
    let uv = new_svc();
    let errs = uv.apply(&TuningProfile { voltage_offset_mv: -90, ..profile() });
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(uv.read_voltage_lock_mv(), curve_top - 90, "undervolt offset becomes a cap");
    assert_eq!(uv.applied_profile().unwrap().target_voltage_mv, curve_top - 90, "applied profile records the cap");

    // Ordering: the cap must be the last voltage-related write.
    let ordered = new_svc();
    mock(&ordered, |m| m.calls.clear());
    ordered.apply(&TuningProfile { target_voltage_mv: 950, core_offset_mhz: 30, ..profile() });
    let cs = calls(&ordered);
    let (i_lock, i_curve, i_core) =
        (last_index(&cs, "SetVoltageLock"), last_index(&cs, "SetVoltageCurveOffset"), last_index(&cs, "SetCoreOffset"));
    assert!(i_lock > i_curve && i_curve >= 0, "cap written after curve offset ({i_lock} > {i_curve})");
    assert!(i_lock > i_core && i_core >= 0, "cap written after core offset ({i_lock} > {i_core})");
    assert_eq!(ordered.read_voltage_lock_mv(), 950, "cap survives the whole apply");
}

/// Seeds are held to the same bounds as live observations.
#[test]
fn seeded_ceilings_are_bounded() {
    let svc = new_svc();
    svc.seed_observed_max_voltage(2000);
    svc.seed_observed_max_voltage(500); // below the card's minimum tunable voltage (850)
    svc.seed_observed_max_voltage(1200); // more than one step above the 1100 curve top
    assert_eq!(svc.observed_max_voltage_mv(), 0);
    svc.seed_observed_max_voltage(1080);
    assert_eq!(svc.stock_ceiling_mv(), 1080);
    svc.seed_observed_max_boosted_voltage(1070); // not above stock: measures no headroom
    svc.seed_observed_max_boosted_voltage(1300); // above the compiled-in roof
    assert_eq!(svc.observed_max_boosted_voltage_mv(), 0);
    svc.seed_observed_max_boosted_voltage(1120);
    assert_eq!(svc.boost_ceiling_mv(), 1120, "a measured roof wins over the guess");
}

/// "AMD-style card: offset voltage, absolute memory clock, hardware fan curve, zero RPM."
#[test]
fn offset_style_card() {
    let svc = TuningService::new(Box::new(OffsetStyleBackend::new()));
    svc.initialize(0).unwrap();
    let caps = svc.capabilities();
    assert_eq!(caps.voltage_style, VoltageControlStyle::Offset, "offset style detected");
    assert!(caps.memory_clock_is_absolute, "memory slider is absolute");

    let errs = svc.apply(&TuningProfile {
        voltage_offset_mv: -120,
        core_offset_mhz: 250,
        memory_offset_mhz: 2800,
        power_limit_percent: 5,
        zero_rpm: false,
        memory_timing_level: 1,
        fan_mode: FanMode::Curve,
        ..profile()
    });
    assert!(errs.is_empty(), "amd apply is clean ({})", errs.join(" | "));
    offset(&svc, |b| {
        assert_eq!(b.voltage_offset_mv, -120, "voltage offset written");
        assert!(!b.calls.iter().any(|c| c == "SetVoltageLock"), "no voltage lock attempted");
        assert!(!b.calls.iter().any(|c| c == "SetVoltageBoost"), "no voltage boost attempted");
        assert_eq!(b.core_offset_mhz, 250, "core offset written");
        assert_eq!(b.memory_mhz, 2800, "absolute memory clock written");
        assert_eq!(b.power_percent, 5, "power offset written");
        assert!(!b.zero_rpm, "zero rpm written");
        assert_eq!(b.memory_timing, 1, "memory timing written");
        assert!(b.calls.iter().any(|c| c == "SetFanCurve"), "hardware fan curve used");
        assert!(!b.calls.iter().any(|c| c == "SetFanSpeed"), "software fan loop not engaged");
    });

    let stock = TuningProfile::stock(&caps, Some("Test"));
    assert_eq!(stock.memory_offset_mhz, 2518, "stock memory = rated clock");
    assert!(stock.zero_rpm, "stock zero rpm follows the card default");
}

/// "ReleaseFanControl respects who owns the curve"
#[test]
fn release_fan_control_respects_owner() {
    let hw = TuningService::new(Box::new(OffsetStyleBackend::new()));
    hw.initialize(0).unwrap();
    assert!(hw.capabilities().fan_curve_is_hardware, "test backend really is hardware-curve");
    hw.release_fan_control();
    assert!(!offset(&hw, |b| b.calls.iter().any(|c| c == "SetFanAuto")), "driver-owned fan table survives exit");

    let soft = new_svc();
    assert!(!soft.capabilities().fan_curve_is_hardware, "mock backend enforces curves in software");
    soft.with_backend(|b| b.set_fan_speed(0, -1, 70)).unwrap();
    assert!(st(&soft).fan_manual, "mock fan is manual once a duty is set");
    soft.release_fan_control();
    assert!(!st(&soft).fan_manual, "software fan control is still handed back on exit");
}

/// "OCP current limits are gated like every other XOC lever"
#[test]
fn ocp_limits_are_gated() {
    let svc = new_svc();
    svc.seed_ocp_defaults(300_000, 120_000);
    assert!(svc.capabilities().can_set_ocp, "mock exposes OCP");

    svc.apply(&TuningProfile { nvvdd_ocp_milliamps: 250_000, msvdd_ocp_milliamps: 100_000, ..profile() });
    let s = st(&svc);
    assert!(s.nvvdd_ocp_milliamps == 300_000 && s.msvdd_ocp_milliamps == 120_000, "disarmed OCP stays stock");

    svc.apply(&TuningProfile {
        xoc_armed: XocLever::NVVDD_OCP,
        nvvdd_ocp_milliamps: 250_000,
        msvdd_ocp_milliamps: 100_000,
        ..profile()
    });
    let s = st(&svc);
    assert_eq!(s.nvvdd_ocp_milliamps, 250_000, "armed NVVDD OCP applies");
    assert_eq!(s.msvdd_ocp_milliamps, 120_000, "MSVDD OCP left alone while its own lever is disarmed");

    svc.apply(&TuningProfile { xoc_armed: XocLever::MSVDD_OCP, msvdd_ocp_milliamps: 100_000, ..profile() });
    let s = st(&svc);
    assert_eq!(s.nvvdd_ocp_milliamps, 300_000, "disarming NVVDD OCP restores stock");
    assert_eq!(s.msvdd_ocp_milliamps, 100_000, "armed MSVDD OCP applies");

    svc.reset_to_defaults().unwrap();
    let s = st(&svc);
    assert!(s.nvvdd_ocp_milliamps == 300_000 && s.msvdd_ocp_milliamps == 120_000, "reset restores both OCP limits");
}

/// "fan-only Zero RPM apply" (the fan window's own button).
#[test]
fn fan_only_apply_writes_only_fans() {
    let svc = TuningService::new(Box::new(OffsetStyleBackend::new()));
    svc.initialize(0).unwrap();
    svc.apply(&profile());
    offset(&svc, |b| b.calls.clear());
    let errs = svc.set_fans(FanMode::Auto, 50, &[], &FanCurve::default(), Some(false));
    offset(&svc, |b| {
        assert!(errs.is_empty() && b.calls.iter().any(|c| c == "SetZeroRpm") && !b.zero_rpm, "Zero RPM apply");
        assert!(b.calls.iter().all(|c| c == "SetFanAuto" || c == "SetZeroRpm"), "fan-only apply never writes tuning");
    });
    assert!(!svc.applied_profile().unwrap().zero_rpm, "applied profile reflects Zero RPM");
}

/// Intel-shaped card through the offset backend: percentage voltage, profile ownership, reset.
#[test]
fn intel_percent_style_card() {
    let good_curve = curve(&[(25.0, 30.0), (70.0, 50.0), (100.0, 100.0)]);
    let intel_caps = GpuCapabilities {
        voltage_style: VoltageControlStyle::Percent,
        can_set_voltage_boost: true,
        can_edit_vf_curve: true,
        voltage_boost_default_percent: 100,
        can_set_core_offset: true,
        core_offset_min_mhz: -300,
        core_offset_max_mhz: 1000,
        core_offset_step_mhz: 1,
        can_set_memory_offset: true,
        memory_clock_is_absolute: true,
        memory_clock_unit: "Mbps".into(),
        memory_clock_default_mhz: 19000,
        memory_offset_min_mhz: 19000,
        memory_offset_max_mhz: 22000,
        temp_limit_is_percent: true,
        can_set_fan_speed: true,
        fan_curve_is_hardware: true,
        fan_curve_points: 10,
        ..Default::default()
    };
    assert_eq!(TuningProfile::stock(&intel_caps, None).memory_offset_mhz, 19000, "Intel stock memory is 19 Gbps");
    assert_eq!(TuningProfile::stock(&intel_caps, None).voltage_boost_percent, 100, "Intel stock voltage default");
    assert_eq!(TuningProfile::stock(&GpuCapabilities::default(), None).voltage_boost_percent, 0, "NVIDIA stays 0");
    assert!(!intel_caps.can_set_voltage() && intel_caps.temp_limit_unit() == "%", "no fabricated mV cap");

    let mut backend = OffsetStyleBackend::new();
    backend.override_caps = Some(intel_caps);
    backend.current_curve = Some(good_curve.clone());
    let svc = TuningService::new(Box::new(backend));
    svc.initialize(0).unwrap();
    let mut current = svc.read_current_as_profile().unwrap();
    assert_eq!(current.fan_mode, FanMode::Curve, "Intel current profile preserves hardware curve mode");
    assert_eq!(current.fan_curve.points, good_curve.points, "Intel current profile preserves curve points");
    current.target_voltage_mv = 1100;
    current.voltage_boost_percent = 61;
    svc.apply(&current);
    offset(&svc, |b| {
        assert!(b.calls.iter().any(|c| c == "SetVoltageBoost") && b.boost_percent == 61, "percentage voltage setter");
        assert!(
            !b.calls.iter().any(|c| c == "SetVoltageCurveOffset" || c == "SetVoltageLock"),
            "Intel never invokes NVIDIA cap/flatten"
        );
        b.calls.clear();
    });
    current.gpu_name = Some("NVIDIA GeForce RTX 4070".into());
    let refused = svc.apply(&current);
    assert!(!refused.is_empty() && offset(&svc, |b| b.calls.is_empty()), "Intel rejects another GPU profile");
    svc.reset_to_defaults().unwrap();
    let applied = svc.applied_profile().unwrap();
    assert_eq!(applied.voltage_boost_percent, 100, "Intel reset profile retains factory voltage limit");
    svc.apply(&applied);
    assert_eq!(offset(&svc, |b| b.boost_percent), 100, "Applying after reset does not lower Intel voltage limit");
}

/// "A UI read or voltage-cap edit must wait while a telemetry call owns the driver."
#[test]
fn ui_calls_wait_for_a_stalled_poll() {
    let entered = Gate::default();
    let release = Gate::default();
    let overlaps = Arc::new(Mutex::new(0));
    let svc = Arc::new(TuningService::new(Box::new(CountingBackend::default())));
    svc.initialize(0).unwrap();
    {
        let (entered, release, release2, overlaps) =
            (entered.clone(), release.clone(), release.clone(), overlaps.clone());
        counting(&svc, move |c| {
            c.before_telemetry = Some(Box::new(move || {
                entered.set();
                assert!(release.wait(Duration::from_secs(5)), "Test poll stalled");
            }));
            c.before_ui_call = Some(Box::new(move || {
                if !release2.is_set() {
                    *overlaps.lock().unwrap() += 1;
                }
            }));
        });
    }
    svc.start_polling(10_000).unwrap();
    assert!(entered.wait(Duration::from_secs(5)), "poll entered blocking driver");

    type Action = Box<dyn FnOnce(&TuningService) + Send>;
    let actions: Vec<Action> = vec![
        Box::new(|s| {
            let _ = s.read_graphics_info();
        }),
        Box::new(|s| {
            let _ = s.read_tuning_state();
        }),
        Box::new(|s| {
            let _ = s.read_voltage_lock_mv();
        }),
        Box::new(|s| {
            let _ = s.set_voltage_lock(0);
        }),
        Box::new(|s| {
            let _ = s.read_current_as_profile();
        }),
    ];
    let (done_tx, done_rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    for action in actions {
        let (svc, done_tx, ready_tx) = (svc.clone(), done_tx.clone(), ready_tx.clone());
        thread::spawn(move || {
            ready_tx.send(()).unwrap();
            action(&svc);
            done_tx.send(()).unwrap();
        });
    }
    for _ in 0..5 {
        ready_rx.recv_timeout(Duration::from_secs(5)).expect("all UI callers started");
    }
    assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err(), "UI calls wait while driver is polling");
    assert_eq!(*overlaps.lock().unwrap(), 0);
    assert!(svc.stop_polling_timeout(Duration::from_millis(50)).is_err(), "stalled polling cannot be abandoned");
    release.set();
    for _ in 0..5 {
        done_rx.recv_timeout(Duration::from_secs(5)).expect("UI calls complete after poll releases driver");
    }
    assert_eq!(*overlaps.lock().unwrap(), 0, "UI calls never overlap telemetry");
    svc.stop_polling().unwrap();
    assert!(!svc.is_polling());
}

/// Events reach both listeners and channel subscribers, outside the service's locks.
#[test]
fn events_are_delivered() {
    let svc = Arc::new(new_svc());
    let rx = svc.subscribe();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let weak = Arc::downgrade(&svc);
    let id = {
        let seen = seen.clone();
        svc.add_log_listener(move |m| {
            seen.lock().unwrap().push(m.to_string());
            // Re-entering the service from a listener must not deadlock.
            if let Some(s) = weak.upgrade() {
                let _ = s.capabilities();
            }
        })
    };
    let samples = Arc::new(Mutex::new(0));
    {
        let samples = samples.clone();
        svc.add_telemetry_listener(move |_| *samples.lock().unwrap() += 1);
    }
    svc.apply(&TuningProfile { core_offset_mhz: 30, ..profile() });
    assert!(seen.lock().unwrap().iter().any(|m| m == "Core offset: ok"));
    svc.start_polling(10).unwrap();
    thread::sleep(Duration::from_millis(100));
    svc.stop_polling().unwrap();
    assert!(*samples.lock().unwrap() > 0);
    let events: Vec<_> = rx.try_iter().collect();
    assert!(events.iter().any(|e| matches!(e, services::ServiceEvent::Telemetry(_))));
    assert!(events.iter().any(|e| matches!(e, services::ServiceEvent::Log(m) if m == "Core offset: ok")));
    svc.remove_listener(id);
    let before = seen.lock().unwrap().len();
    svc.reset_to_defaults().unwrap();
    assert_eq!(seen.lock().unwrap().len(), before, "removed listener hears nothing");
}

#[test]
fn bad_gpu_index_is_refused() {
    let svc = TuningService::new(Box::new(MockBackend::new()));
    assert!(svc.initialize(3).is_err());
    assert_eq!(svc.devices().len(), 1, "devices are known even when the index is not");
}

/// Dropping a polling service stops the thread (C# Dispose).
#[test]
fn drop_stops_polling() {
    let svc = new_svc();
    svc.start_polling(5).unwrap();
    thread::sleep(Duration::from_millis(30));
    drop(svc);
}

/// A hand-edited curve is reported as overwritten by an apply that shares the delta table.
#[test]
fn manual_curve_overwrite_is_noted() {
    let svc = new_svc();
    let pts = svc.read_vf_curve().unwrap();
    svc.set_vf_curve_targets(&pts[..2]).unwrap();
    assert!(svc.manual_curve_active());
    let errs = svc.apply(&profile());
    assert!(only_notes(&errs) && errs[0].contains("hand-edited V/F curve"), "{errs:?}");
    assert!(!svc.manual_curve_active());
}
