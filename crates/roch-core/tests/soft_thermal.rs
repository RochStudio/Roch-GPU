//! The software temperature limit through `TuningService`, against the simulated GPU: the
//! controller driving a card that heats and cools with its clock, and every path that has to put the
//! clock back.

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use roch_core::mock::MockBackend;
use roch_core::services::soft_thermal::{BACKOFF_TICKS, MAX_FAILURES, REASSERT_TICKS};
use roch_core::services::{ServiceEvent, TuningService};
use roch_core::*;

// ------------------------------------------------------------------ a mock the test can still see

/// The mock behind a shared handle, so the test can look at it after the service is gone, plus the
/// two NVIDIA-only facts the controller listens for.
#[derive(Clone)]
struct Probe {
    mock: Arc<Mutex<MockBackend>>,
    resets: Arc<AtomicU64>,
    voltage_cap_mhz: Arc<AtomicI32>,
    voltage_locks: Arc<Mutex<Vec<i32>>>,
}

impl Probe {
    fn new(mock: MockBackend) -> Self {
        Self {
            mock: Arc::new(Mutex::new(mock)),
            resets: Arc::default(),
            voltage_cap_mhz: Arc::default(),
            voltage_locks: Arc::default(),
        }
    }
    fn m<R>(&self, f: impl FnOnce(&mut MockBackend) -> R) -> R {
        f(&mut self.mock.lock().unwrap())
    }
    fn writes(&self) -> Vec<(i32, i32)> {
        self.m(|m| m.clock_range_writes.clone())
    }
    fn range(&self) -> (i32, i32) {
        self.m(|m| m.clock_range())
    }
}

struct Shared {
    p: Probe,
    devices: Vec<GpuDevice>,
}

impl GpuBackend for Shared {
    fn backend_name(&self) -> String {
        "Simulated GPU".into()
    }
    fn initialize(&mut self) -> Result<()> {
        Ok(())
    }
    fn devices(&self) -> &[GpuDevice] {
        &self.devices
    }
    fn capabilities(&mut self, gpu: usize) -> Result<GpuCapabilities> {
        self.p.m(|m| m.capabilities(gpu))
    }
    fn read_telemetry(&mut self, gpu: usize) -> Result<GpuTelemetry> {
        self.p.m(|m| m.read_telemetry(gpu))
    }
    /// Only the temperature, as the NVIDIA backend's background read.
    fn read_temperature_only(&mut self, gpu: usize) -> Result<GpuTelemetry> {
        let t = self.p.m(|m| m.read_telemetry(gpu))?;
        Ok(GpuTelemetry { temperature_c: t.temperature_c, ..Default::default() })
    }
    fn read_tuning_state(&mut self, gpu: usize) -> Result<GpuTuningState> {
        self.p.m(|m| m.read_tuning_state(gpu))
    }
    fn set_core_offset(&mut self, gpu: usize, v: i32) -> Result<()> {
        self.p.m(|m| m.set_core_offset(gpu, v))
    }
    fn set_memory_offset(&mut self, gpu: usize, v: i32) -> Result<()> {
        self.p.m(|m| m.set_memory_offset(gpu, v))
    }
    fn set_power_limit(&mut self, gpu: usize, v: i32) -> Result<()> {
        self.p.m(|m| m.set_power_limit(gpu, v))
    }
    fn set_temp_limit(&mut self, gpu: usize, v: i32) -> Result<()> {
        self.p.m(|m| m.set_temp_limit(gpu, v))
    }
    fn set_voltage_boost(&mut self, gpu: usize, v: i32) -> Result<()> {
        self.p.m(|m| m.set_voltage_boost(gpu, v))
    }
    fn set_voltage_curve_offset(&mut self, gpu: usize, mv: i32, extra: i32) -> Result<()> {
        self.p.m(|m| m.set_voltage_curve_offset(gpu, mv, extra))
    }
    fn set_voltage_lock(&mut self, gpu: usize, mv: i32) -> Result<()> {
        self.p.voltage_locks.lock().unwrap().push(mv);
        self.p.m(|m| m.set_voltage_lock(gpu, mv))
    }
    fn read_voltage_lock_mv(&mut self, gpu: usize) -> i32 {
        self.p.m(|m| m.read_voltage_lock_mv(gpu))
    }
    fn set_clock_range(&mut self, gpu: usize, lo: i32, hi: i32) -> Result<()> {
        // NVIDIA: an explicit clock range takes the lock over from the voltage cap.
        self.p.voltage_cap_mhz.store(0, Ordering::SeqCst);
        self.p.m(|m| m.set_clock_range(gpu, lo, hi))
    }
    fn voltage_cap_clock_mhz(&self, _gpu: usize) -> i32 {
        self.p.voltage_cap_mhz.load(Ordering::SeqCst)
    }
    fn driver_reset_count(&self) -> u64 {
        self.p.resets.load(Ordering::SeqCst)
    }
    fn set_fan_speed(&mut self, gpu: usize, fan: i32, pct: i32) -> Result<()> {
        self.p.m(|m| m.set_fan_speed(gpu, fan, pct))
    }
    fn set_fan_auto(&mut self, gpu: usize) -> Result<()> {
        self.p.m(|m| m.set_fan_auto(gpu))
    }
    fn reset_to_defaults(&mut self, gpu: usize) -> Result<()> {
        self.p.m(|m| m.reset_to_defaults(gpu))
    }
}

/// An RTX 50-style simulated card held at full load and already warm.
fn rig() -> (TuningService, Probe) {
    let mut mock = MockBackend::without_hardware_temp_limit();
    mock.fixed_load = Some(100.0);
    mock.set_sim_temp_c(72.0);
    let devices = mock.devices().to_vec();
    let p = Probe::new(mock);
    let svc = TuningService::new(Box::new(Shared { p: p.clone(), devices }));
    svc.initialize(0).unwrap();
    (svc, p)
}

fn limit(c: i32) -> TuningProfile {
    TuningProfile { soft_thermal_limit_c: c, ..TuningProfile::default() }
}

/// One poll: a fresh reading from the card, stepped into the controller. Returns the temperature.
fn tick(svc: &TuningService, p: &Probe) -> f64 {
    let t = p.m(|m| m.read_telemetry(0)).unwrap();
    svc.step_soft_thermal(&t);
    t.temperature_c
}

fn ticks(svc: &TuningService, p: &Probe, n: usize) -> Vec<f64> {
    (0..n).map(|_| tick(svc, p)).collect()
}

/// Drive the card until the controller holds it.
fn until_holding(svc: &TuningService, p: &Probe) {
    for _ in 0..50 {
        tick(svc, p);
        if svc.soft_thermal_status().active {
            return;
        }
    }
    panic!("the controller never engaged");
}

fn logs(rx: &std::sync::mpsc::Receiver<ServiceEvent>) -> Vec<String> {
    rx.try_iter()
        .filter_map(|e| match e {
            ServiceEvent::Log(m) => Some(m),
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------------ tests

#[test]
fn offered_only_where_the_driver_has_no_limit() {
    let (svc, _p) = rig();
    let c = svc.capabilities();
    assert!(c.can_set_soft_thermal_limit && !c.can_set_temp_limit);
    assert_eq!((c.soft_thermal_limit_min_c, c.soft_thermal_limit_max_c), (50, 105), "runs to the 105 °C ceiling");
    assert_eq!(c.soft_thermal_card_limit_c, 88, "the card's own limit is still 88 °C");

    // A card with its own thermal policy never runs the software limit, whatever the profile says.
    let svc = TuningService::new(Box::new(MockBackend::new()));
    svc.initialize(0).unwrap();
    assert!(!svc.capabilities().can_set_soft_thermal_limit);
    svc.apply(&limit(60));
    for _ in 0..20 {
        let t = svc.with_backend(|b| b.read_telemetry(0)).unwrap();
        svc.step_soft_thermal(&GpuTelemetry { temperature_c: 99.0, ..t });
    }
    assert!(!svc.soft_thermal_status().enabled);
    assert_eq!(svc.read_tuning_state().unwrap().locked_clock_max_mhz, 0, "nothing capped");
}

#[test]
fn holds_the_card_at_the_limit_then_lets_go() {
    let (svc, p) = rig();
    let rx = svc.subscribe();
    let errs = svc.apply(&limit(65));
    assert!(errs.is_empty(), "{errs:?}");
    assert!(svc.soft_thermal_status().enabled);
    assert_eq!(svc.applied_profile().unwrap().soft_thermal_limit_c, 65);

    // Hot at full load: the first step caps below the clock the card was running.
    tick(&svc, &p);
    let s = svc.soft_thermal_status();
    assert!(s.active, "{s:?}");
    assert!(s.ceiling_mhz > 0 && s.ceiling_mhz < 2610, "{s:?}");
    let first = logs(&rx);
    assert!(first.iter().any(|m| m.starts_with("Thermal limit: 72 °C ≥ 65 °C, core capped at ")), "{first:?}");

    // It settles: after a few minutes the card sits at the limit, give or take the hysteresis.
    let temps = ticks(&svc, &p, 300);
    let tail = &temps[200..];
    let hottest = tail.iter().cloned().fold(f64::MIN, f64::max);
    let coolest = tail.iter().cloned().fold(f64::MAX, f64::min);
    assert!(hottest <= 66.0, "held near 65 °C, saw {hottest}");
    assert!(coolest >= 60.0, "not throttled far below the limit, saw {coolest}");
    let s = svc.soft_thermal_status();
    assert!(s.active && s.ceiling_mhz >= 1545, "never below the floor: {s:?}");
    let (lo, hi) = p.range();
    assert_eq!((lo, hi), (0, s.ceiling_mhz), "only the ceiling is written");

    // The load goes away: the ceiling climbs back and the clock is handed to the driver.
    p.m(|m| m.fixed_load = Some(10.0));
    ticks(&svc, &p, 200);
    let s = svc.soft_thermal_status();
    assert!(!s.active && s.enabled, "{s:?}");
    assert_eq!(p.range(), (0, 0), "back to the driver");
    assert!(logs(&rx).iter().any(|m| m.contains("core clock handed back to the driver")));
}

#[test]
fn writes_only_when_the_ceiling_moves() {
    let (svc, p) = rig();
    svc.apply(&limit(65));
    until_holding(&svc, &p);
    let ticks_run = 400;
    let before = p.writes().len();
    let mut changes = 0;
    let mut last = svc.soft_thermal_status().ceiling_mhz;
    for _ in 0..ticks_run {
        tick(&svc, &p);
        let now = svc.soft_thermal_status().ceiling_mhz;
        if now != last {
            changes += 1;
        }
        last = now;
    }
    let writes = p.writes().len() - before;
    // One write per change, plus a re-assert of an unchanged ceiling every REASSERT_TICKS at most.
    let reasserts = ticks_run / REASSERT_TICKS as usize;
    assert!(writes >= changes && writes <= changes + reasserts, "{writes} writes for {changes} changes");
    assert!(writes < ticks_run / 2, "settled: most ticks write nothing ({writes} of {ticks_run})");
}

#[test]
fn works_inside_the_users_clock_range() {
    let (svc, p) = rig();
    let user = TuningProfile {
        xoc_armed: XocLever::CLOCK_RANGE,
        clock_lock_min_mhz: 1800,
        clock_lock_max_mhz: 2500,
        ..limit(60)
    };
    svc.apply(&user);
    assert_eq!(p.range(), (1800, 2500));
    until_holding(&svc, &p);
    ticks(&svc, &p, 100);
    for &(lo, hi) in &p.writes()[1..] {
        assert_eq!(lo, 1800, "the user's floor stays");
        assert!((1800..=2500).contains(&hi), "never above the user's ceiling: {hi}");
    }
    // What the window seeds its sliders from is the user's range, not the thermal ceiling.
    let cur = svc.read_current_as_profile().unwrap();
    assert_eq!((cur.clock_lock_min_mhz, cur.clock_lock_max_mhz), (1800, 2500));
    assert_eq!(cur.soft_thermal_limit_c, 60);

    // Cooling down hands back exactly the user's range.
    p.m(|m| m.fixed_load = Some(5.0));
    ticks(&svc, &p, 200);
    assert!(!svc.soft_thermal_status().active);
    assert_eq!(p.range(), (1800, 2500));
}

#[test]
fn the_clock_range_lever_rewrites_the_ceiling_inside_the_new_range() {
    let (svc, p) = rig();
    svc.apply(&limit(65));
    until_holding(&svc, &p);
    let held = svc.soft_thermal_status().ceiling_mhz;
    // Arming a range writes it over the thermal ceiling; the next step caps again, inside it.
    let user = TuningProfile { clock_lock_min_mhz: 1600, clock_lock_max_mhz: 2200, ..limit(65) };
    svc.set_xoc_lever(&user, XocLever::CLOCK_RANGE, true);
    assert_eq!(p.range(), (1600, 2200));
    tick(&svc, &p);
    let (lo, hi) = p.range();
    assert_eq!(lo, 1600);
    assert!(hi <= 2200.min(held), "{hi}");
}

#[test]
fn every_way_of_turning_it_off_puts_the_clock_back() {
    // An apply without the limit.
    let (svc, p) = rig();
    svc.apply(&limit(60));
    until_holding(&svc, &p);
    svc.apply(&limit(0));
    assert_eq!(p.range(), (0, 0));
    assert!(!svc.soft_thermal_status().enabled);
    ticks(&svc, &p, 10);
    assert_eq!(p.range(), (0, 0), "and it stays off");

    // Reset to defaults.
    let (svc, p) = rig();
    svc.apply(&limit(60));
    until_holding(&svc, &p);
    svc.reset_to_defaults().unwrap();
    assert_eq!(p.range(), (0, 0));
    ticks(&svc, &p, 10);
    assert_eq!(p.range(), (0, 0));

    // Release at exit — to the user's armed range, not the driver's.
    let (svc, p) = rig();
    let rx = svc.subscribe();
    svc.apply(&TuningProfile {
        xoc_armed: XocLever::CLOCK_RANGE,
        clock_lock_min_mhz: 1500,
        clock_lock_max_mhz: 2700,
        ..limit(60)
    });
    until_holding(&svc, &p);
    assert_ne!(p.range(), (1500, 2700));
    svc.release_soft_thermal_limit();
    assert_eq!(p.range(), (1500, 2700));
    assert!(
        logs(&rx).iter().any(|m| m == "Thermal limit: released; core clock back to your 1500-2700 MHz clock range")
    );
    let n = p.writes().len();
    ticks(&svc, &p, 10);
    svc.release_soft_thermal_limit();
    assert_eq!(p.writes().len(), n, "released for good: no more steps, a second release writes nothing");

    // Switching GPU (re-selecting the only one is still a switch away from what was applied).
    let (svc, p) = rig();
    svc.apply(&limit(60));
    until_holding(&svc, &p);
    svc.select_gpu(0).unwrap();
    assert_eq!(p.range(), (0, 0));
    ticks(&svc, &p, 10);
    assert_eq!(p.range(), (0, 0));
}

#[test]
fn dropping_the_service_releases_the_clock() {
    let (svc, p) = rig();
    svc.apply(&limit(60));
    until_holding(&svc, &p);
    svc.start_polling(20).unwrap();
    assert_ne!(p.range(), (0, 0));
    drop(svc);
    assert_eq!(p.range(), (0, 0), "a ceiling must not outlive the process enforcing it");

    // Nothing held, nothing written.
    let (svc, p) = rig();
    let n = p.writes().len();
    drop(svc);
    assert_eq!(p.writes().len(), n);
}

#[test]
fn the_background_poll_runs_it() {
    let (svc, p) = rig();
    svc.apply(&limit(60));
    svc.set_background_mode(true);
    let n = p.writes().len();
    svc.start_polling(30).unwrap();
    // The controller steps about once a second, whatever the poll interval.
    thread::sleep(Duration::from_millis(1300));
    svc.stop_polling().unwrap();
    let w = p.writes();
    let caps = &w[n..];
    assert!(!caps.is_empty() && caps.len() <= 3, "{caps:?}");
    // The background reading has no clock in it; the first cap still starts below the live clock.
    assert!(caps[0].1 < 2610 && caps[0].1 > 2400, "{caps:?}");
}

#[test]
fn a_driver_reset_gets_the_ceiling_written_again() {
    let (svc, p) = rig();
    svc.apply(&limit(65));
    until_holding(&svc, &p);
    // Settle into the hold band so steps stop writing.
    p.m(|m| m.set_sim_temp_c(63.5));
    p.m(|m| m.fixed_load = Some(60.0));
    tick(&svc, &p);
    let n = p.writes().len();
    let held = p.range();
    // The driver resets: the lock is gone and the backend says so.
    p.m(|m| m.set_clock_range(0, 0, 0)).unwrap();
    p.resets.fetch_add(1, Ordering::SeqCst);
    let rx = svc.subscribe();
    tick(&svc, &p);
    let w = p.writes();
    assert_eq!(w.len(), n + 2, "the reset's own write, then ours: {:?}", &w[n..]);
    assert!(held.1 > 0);
    assert_eq!(p.range().1, svc.soft_thermal_status().ceiling_mhz, "capped again");
    assert!(logs(&rx).iter().any(|m| m.contains("display driver was reset")));
}

#[test]
fn a_voltage_caps_clock_lock_is_kept_and_restored() {
    let (svc, p) = rig();
    // A 950 mV cap, which (the NVIDIA fallback) the driver holds as a 2400 MHz clock lock: the
    // apply writes the clock range first and the cap after it, so the lock is the cap's.
    svc.apply(&TuningProfile { target_voltage_mv: 950, ..limit(60) });
    assert_eq!(svc.read_voltage_lock_mv(), 950);
    p.voltage_cap_mhz.store(2400, Ordering::SeqCst);
    until_holding(&svc, &p);
    for _ in 0..30 {
        tick(&svc, &p);
        assert!(p.range().1 <= 2400, "never lifts the voltage cap's ceiling: {:?}", p.range());
    }
    p.voltage_locks.lock().unwrap().clear();
    svc.release_soft_thermal_limit();
    assert_eq!(p.range(), (0, 0));
    assert_eq!(*p.voltage_locks.lock().unwrap(), vec![950], "the cap is put back after the release");
}

#[test]
fn failed_writes_back_off_then_stop() {
    let (svc, p) = rig();
    let rx = svc.subscribe();
    svc.apply(&limit(60));
    p.m(|m| m.fail_clock_range = true);
    let n = p.writes().len();
    tick(&svc, &p);
    assert_eq!(p.writes().len(), n + 1, "one attempt");
    assert!(logs(&rx).iter().any(|m| m.contains("Simulated clock-range failure") && m.contains("trying again")));
    ticks(&svc, &p, BACKOFF_TICKS as usize);
    assert_eq!(p.writes().len(), n + 1, "backing off, not spinning");
    for _ in 1..MAX_FAILURES {
        ticks(&svc, &p, BACKOFF_TICKS as usize + 1);
    }
    let s = svc.soft_thermal_status();
    assert!(s.stopped, "{s:?}");
    assert!(logs(&rx).iter().any(|m| m.contains("stopped after 3 failed attempts")));
    let n = p.writes().len();
    ticks(&svc, &p, 100);
    assert_eq!(p.writes().len(), n, "stopped: no more writes");

    // The next apply tries again.
    p.m(|m| m.fail_clock_range = false);
    svc.apply(&limit(60));
    assert!(!svc.soft_thermal_status().stopped);
    tick(&svc, &p);
    assert!(svc.soft_thermal_status().active);
}

#[test]
fn reapplying_keeps_the_ceiling_it_worked_out() {
    let (svc, p) = rig();
    svc.apply(&limit(65));
    until_holding(&svc, &p);
    ticks(&svc, &p, 60);
    let held = svc.soft_thermal_status().ceiling_mhz;
    svc.apply(&TuningProfile { core_offset_mhz: 30, ..limit(65) });
    assert_eq!(p.range(), (0, 0), "the apply's own clock-range write");
    tick(&svc, &p);
    let now = p.range().1;
    assert!((held - 45..=held + 15).contains(&now), "picked up near {held}, not from the top: {now}");
}

#[test]
fn clamp_is_noted() {
    let (svc, _p) = rig();
    let errs = svc.apply(&limit(30));
    assert_eq!(
        errs,
        vec!["Note: software temperature limit 30 °C is outside this card's range — applied 50 °C instead."]
    );
    assert_eq!(svc.soft_thermal_status().limit_c, 50);
    // Below the card's own limit still arms the controller.
    let errs = svc.apply(&limit(87));
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(svc.soft_thermal_status().limit_c, 87);
    // At or above the card's own limit the value is kept, but the card manages itself.
    let errs = svc.apply(&limit(97));
    assert_eq!(
        errs,
        vec!["Note: temperature limit 97 °C is at or above this card's own limit (88 °C) — left to the card."]
    );
    assert_eq!(svc.soft_thermal_status().limit_c, 0);
    let errs = svc.apply(&limit(105));
    assert_eq!(
        errs,
        vec!["Note: temperature limit 105 °C is at or above this card's own limit (88 °C) — left to the card."]
    );
    assert_eq!(svc.soft_thermal_status().limit_c, 0);
    // Beyond the window is clamped to off.
    let errs = svc.apply(&limit(106));
    assert_eq!(
        errs,
        vec!["Note: temperature limit 106 °C is outside this card's window — applied off (the card's own limit)."]
    );
    assert_eq!(svc.soft_thermal_status().limit_c, 0);
}

#[test]
fn a_refused_gpu_switch_keeps_the_limit_running() {
    let (svc, p) = rig();
    svc.apply(&limit(60));
    until_holding(&svc, &p);
    let held = p.range();
    assert!(svc.select_gpu(5).is_err());
    assert!(svc.soft_thermal_status().active, "nothing switched, nothing released");
    assert_eq!(p.range(), held);
}
