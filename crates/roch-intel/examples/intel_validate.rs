//! Write/readback/restore validation on a real Arc card, after the procedure the C# build was
//! validated with on the B60 (docs/intel-arc-b60.md). Each step moves one control by the smallest
//! step it allows — downward where the range permits — verifies the readback, restores it, and
//! checks the whole tuning state is back to the snapshot taken at the start. The first failure
//! stops the run; the starting state is then restored and verified either way.
//!
//! Dry run, read-only (prints the plan):  cargo run -p roch-intel --example intel_validate
//! Write run:                             cargo run -p roch-intel --example intel_validate -- --write
//!
//! Close Roch GPU (window and tray) first: two processes tuning one card would race each other.
//! A log is written to `test-results/intel/` (git-ignored).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use roch_core::{FanCurve, FanMode, FanPoint, GpuBackend, GpuCapabilities, GpuTuningState, Result, VfCurveSample};
use roch_intel::IntelBackend;

const GPU: usize = 0;
/// Fixed duty for the fan step: audible but harmless at any temperature the card idles at.
const FIXED_FAN_PERCENT: i32 = 40;
/// How far the V/F step lowers one point, and the clock-range step lowers the ceiling.
const VF_STEP_MHZ: i32 = 50;
const CLOCK_RANGE_STEP_MHZ: i32 = 100;

type Setter = fn(&mut IntelBackend, usize, i32) -> Result<()>;
type Getter = fn(&GpuTuningState) -> i32;

struct Scalar {
    name: &'static str,
    unit: &'static str,
    set: Setter,
    get: Getter,
}

enum Step {
    Scalar { s: Scalar, target: i32 },
    VfPoint { index: usize, target: i32 },
    FanFixed(i32),
    FanCurve(FanCurve),
    ClockRange(i32, i32),
}

impl Step {
    fn describe(&self, initial: &GpuTuningState, curve: &[VfCurveSample]) -> String {
        match self {
            Step::Scalar { s, target } => {
                format!("{}: {} -> {} {} -> back", s.name, (s.get)(initial), target, s.unit)
            }
            Step::VfPoint { index, target } => {
                format!("V/F point {index}: {} -> {target} MHz -> stock curve", curve[*index].live_mhz)
            }
            Step::FanFixed(p) => format!("Fans: {} -> fixed {p}% -> back", fan_text(initial)),
            Step::FanCurve(c) => format!("Fans: {} -> curve {} -> back", fan_text(initial), curve_text(c)),
            Step::ClockRange(lo, hi) => format!(
                "Clock range: {}-{} -> {lo}-{hi} MHz -> factory",
                initial.locked_clock_min_mhz, initial.locked_clock_max_mhz
            ),
        }
    }
}

struct Log(String);

impl Log {
    fn say(&mut self, line: impl AsRef<str>) {
        println!("{}", line.as_ref());
        let _ = writeln!(self.0, "{}", line.as_ref());
    }
}

fn fan_text(s: &GpuTuningState) -> String {
    match s.detected_fan_mode {
        Some(FanMode::Fixed) => format!("fixed {}%", s.fan_percent),
        Some(FanMode::Curve) => format!("curve {}", s.hardware_fan_curve.as_ref().map(curve_text).unwrap_or_default()),
        _ => "auto".into(),
    }
}

fn curve_text(c: &FanCurve) -> String {
    c.points.iter().map(|p| format!("({},{})", p.temperature_c, p.fan_percent)).collect::<Vec<_>>().join(" ")
}

fn is_stock(curve: &[VfCurveSample]) -> bool {
    curve.iter().all(|p| p.live_mhz == p.stock_mhz)
}

fn frequencies(curve: &[VfCurveSample]) -> Vec<(i32, i32)> {
    curve.iter().map(|p| (p.stock_mhz, p.live_mhz)).collect()
}

/// One below where the range allows, otherwise one above.
fn nudge(value: i32, min: i32, max: i32) -> Option<i32> {
    if value > min {
        Some(value - 1)
    } else if value < max {
        Some(value + 1)
    } else {
        None
    }
}

fn plan(caps: &GpuCapabilities, initial: &GpuTuningState, curve: &[VfCurveSample], log: &mut Log) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut scalar = |can: bool, s: Scalar, min: i32, max: i32| {
        if !can {
            return;
        }
        match nudge((s.get)(initial), min, max) {
            Some(target) => steps.push(Step::Scalar { s, target }),
            None => log.say(format!("skip {}: no room to move within {min}..{max}", s.name)),
        }
    };
    scalar(
        caps.can_set_core_offset,
        Scalar { name: "Core offset", unit: "MHz", set: |b, g, v| b.set_core_offset(g, v), get: |s| s.core_offset_mhz },
        caps.core_offset_min_mhz,
        caps.core_offset_max_mhz,
    );
    scalar(
        caps.can_set_memory_offset,
        Scalar {
            name: "Memory speed",
            unit: "Mbps",
            set: |b, g, v| b.set_memory_offset(g, v),
            get: |s| s.memory_offset_mhz,
        },
        caps.memory_offset_min_mhz,
        caps.memory_offset_max_mhz,
    );
    scalar(
        caps.can_set_power_limit,
        Scalar { name: "Power limit", unit: "%", set: |b, g, v| b.set_power_limit(g, v), get: |s| s.power_limit_percent },
        caps.power_limit_min_percent,
        caps.power_limit_max_percent,
    );
    scalar(
        caps.can_set_temp_limit,
        Scalar { name: "Temperature limit", unit: "%", set: |b, g, v| b.set_temp_limit(g, v), get: |s| s.temp_limit_c },
        caps.temp_limit_min_c,
        caps.temp_limit_max_c,
    );
    scalar(
        caps.can_set_voltage_boost,
        Scalar {
            name: "Voltage limit",
            unit: "%",
            set: |b, g, v| b.set_voltage_boost(g, v),
            get: |s| s.voltage_boost_percent,
        },
        caps.voltage_boost_min_percent,
        caps.voltage_boost_max_percent,
    );

    if caps.can_edit_vf_curve && !curve.is_empty() {
        if is_stock(curve) {
            // A middle point, lowered, staying above its lower neighbour so the curve stays monotonic.
            let index = curve.len() / 2;
            let target = curve[index].live_mhz - VF_STEP_MHZ;
            if index > 0 && target >= curve[index - 1].live_mhz {
                steps.push(Step::VfPoint { index, target });
            } else {
                log.say("skip V/F: the middle point has no room to move down");
            }
        } else {
            log.say("skip V/F: a custom curve is active and clearing it would not restore it");
        }
    }

    if caps.can_set_fan_speed {
        steps.push(Step::FanFixed(FIXED_FAN_PERCENT));
        let points: Vec<FanPoint> = [(30.0, 30.0), (45.0, 30.0), (60.0, 40.0), (70.0, 55.0), (80.0, 75.0), (90.0, 90.0)]
            .into_iter()
            .take(caps.fan_curve_points.max(0) as usize)
            .map(|(t, p)| FanPoint::new(t, p))
            .collect();
        if points.len() >= 2 {
            steps.push(Step::FanCurve(FanCurve::with_points(points)));
        }
    }

    if caps.can_lock_clocks {
        let (lo, hi) = (initial.locked_clock_min_mhz, initial.locked_clock_max_mhz - CLOCK_RANGE_STEP_MHZ);
        if hi > lo && lo >= caps.clock_lock_min_mhz {
            steps.push(Step::ClockRange(lo, hi));
        } else {
            log.say("skip clock range: no room to lower the ceiling");
        }
    }
    steps
}

fn err(e: roch_core::GpuError) -> String {
    e.to_string()
}

fn state(b: &mut IntelBackend) -> std::result::Result<GpuTuningState, String> {
    b.read_tuning_state(GPU).map_err(err)
}

fn restore_fans(b: &mut IntelBackend, initial: &GpuTuningState) -> Result<()> {
    match (initial.detected_fan_mode, &initial.hardware_fan_curve) {
        (Some(FanMode::Fixed), _) => b.set_fan_speed(GPU, -1, initial.fan_percent),
        (Some(FanMode::Curve), Some(curve)) => b.set_fan_curve(GPU, curve),
        _ => b.set_fan_auto(GPU),
    }
}

fn restore_clock_range(b: &mut IntelBackend, initial: &GpuTuningState) -> std::result::Result<(), String> {
    b.set_clock_range(GPU, 0, 0).map_err(err)?;
    let now = state(b)?;
    if (now.locked_clock_min_mhz, now.locked_clock_max_mhz) != (initial.locked_clock_min_mhz, initial.locked_clock_max_mhz)
    {
        // The starting range was a custom one, not the factory range.
        b.set_clock_range(GPU, initial.locked_clock_min_mhz, initial.locked_clock_max_mhz).map_err(err)?;
    }
    Ok(())
}

/// Write, verify, restore. The whole state must match the snapshot afterwards, so a step that
/// disturbs another control fails too.
fn run_step(b: &mut IntelBackend, step: &Step, initial: &GpuTuningState) -> std::result::Result<(), String> {
    match step {
        Step::Scalar { s, target } => {
            (s.set)(b, GPU, *target).map_err(err)?;
            let got = (s.get)(&state(b)?);
            if got != *target {
                return Err(format!("readback {got}, expected {target}"));
            }
            (s.set)(b, GPU, (s.get)(initial)).map_err(err)?;
        }
        Step::VfPoint { index, target } => {
            let curve = b.read_vf_curve(GPU).map_err(err)?;
            let mut edit = curve[*index];
            edit.live_mhz = *target;
            b.set_vf_curve_targets(GPU, &[edit]).map_err(err)?;
            let live = b.read_vf_curve(GPU).map_err(err)?;
            for p in &live {
                let want = if p.index as usize == *index { *target } else { p.stock_mhz };
                if p.live_mhz != want {
                    return Err(format!("point {} reads {} MHz, expected {want}", p.index, p.live_mhz));
                }
            }
            b.set_vf_curve_targets(GPU, &[]).map_err(err)?;
            let cleared = b.read_vf_curve(GPU).map_err(err)?;
            if !is_stock(&cleared) {
                return Err(format!("curve not back to stock: {:?}", frequencies(&cleared)));
            }
        }
        Step::FanFixed(p) => {
            b.set_fan_speed(GPU, -1, *p).map_err(err)?;
            let now = state(b)?;
            if now.detected_fan_mode != Some(FanMode::Fixed) || now.fan_percent != *p {
                return Err(format!("reads {}, expected fixed {p}%", fan_text(&now)));
            }
            restore_fans(b, initial).map_err(err)?;
        }
        Step::FanCurve(c) => {
            b.set_fan_curve(GPU, c).map_err(err)?;
            let now = state(b)?;
            let same = now.hardware_fan_curve.as_ref().is_some_and(|got| curve_text(got) == curve_text(c));
            if now.detected_fan_mode != Some(FanMode::Curve) || !same {
                return Err(format!("reads {}, expected curve {}", fan_text(&now), curve_text(c)));
            }
            restore_fans(b, initial).map_err(err)?;
        }
        Step::ClockRange(lo, hi) => {
            b.set_clock_range(GPU, *lo, *hi).map_err(err)?;
            let now = state(b)?;
            if (now.locked_clock_min_mhz, now.locked_clock_max_mhz) != (*lo, *hi) {
                return Err(format!(
                    "reads {}-{} MHz, expected {lo}-{hi}",
                    now.locked_clock_min_mhz, now.locked_clock_max_mhz
                ));
            }
            restore_clock_range(b, initial)?;
        }
    }
    let after = state(b)?;
    if after != *initial {
        return Err(format!("state after restore differs from the snapshot:\n  was {initial:?}\n  now {after:?}"));
    }
    Ok(())
}

/// Best effort: put back everything a failed step may have left behind, logging what fails.
fn restore_all(b: &mut IntelBackend, initial: &GpuTuningState, curve_was_stock: bool, log: &mut Log) {
    let mut attempt = |what: &str, r: std::result::Result<(), String>| {
        if let Err(e) = r {
            log.say(format!("  restore {what}: FAILED: {e}"));
        }
    };
    // Clearing the curve writes a zero core offset, so it goes before the scalars.
    if curve_was_stock && b.read_vf_curve(GPU).is_ok_and(|c| !is_stock(&c)) {
        attempt("V/F curve", b.set_vf_curve_targets(GPU, &[]).map_err(err));
    }
    let caps = b.capabilities(GPU).unwrap_or_default();
    if caps.can_set_core_offset {
        attempt("core offset", b.set_core_offset(GPU, initial.core_offset_mhz).map_err(err));
    }
    if caps.can_set_memory_offset {
        attempt("memory speed", b.set_memory_offset(GPU, initial.memory_offset_mhz).map_err(err));
    }
    if caps.can_set_power_limit {
        attempt("power limit", b.set_power_limit(GPU, initial.power_limit_percent).map_err(err));
    }
    if caps.can_set_temp_limit {
        attempt("temperature limit", b.set_temp_limit(GPU, initial.temp_limit_c).map_err(err));
    }
    if caps.can_set_voltage_boost {
        attempt("voltage limit", b.set_voltage_boost(GPU, initial.voltage_boost_percent).map_err(err));
    }
    if caps.can_set_fan_speed {
        attempt("fans", restore_fans(b, initial).map_err(err));
    }
    if caps.can_lock_clocks {
        attempt("clock range", restore_clock_range(b, initial));
    }
}

fn save(log: &Log) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-results/intel");
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let path = dir.join(format!("rust-validate-{secs}.txt"));
    match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &log.0)) {
        Ok(()) => println!("Log: {}", path.display()),
        Err(e) => println!("Could not write the log: {e}"),
    }
}

fn main() -> ExitCode {
    let write = std::env::args().any(|a| a == "--write");
    let mut log = Log(String::new());
    let mut b = IntelBackend::new();
    if let Err(e) = b.initialize() {
        println!("No Intel Arc card: {e}");
        return ExitCode::FAILURE;
    }
    let d = &b.devices()[GPU];
    log.say(format!("{}  driver {}  vBIOS {}", d.name, d.driver_version, d.bios_version));

    let read = (|| -> Result<_> {
        let caps = b.capabilities(GPU)?;
        let initial = b.read_tuning_state(GPU)?;
        let curve = b.read_vf_curve(GPU)?;
        Ok((caps, initial, curve))
    })();
    let (caps, initial, curve) = match read {
        Ok(r) => r,
        Err(e) => {
            println!("Could not read the starting state: {e}");
            return ExitCode::FAILURE;
        }
    };
    log.say(format!(
        "Start: core {} MHz, mem {} Mbps, power {}%, temp {}%, vlimit {}%, fans {}, clock range {}-{} MHz, V/F {}",
        initial.core_offset_mhz,
        initial.memory_offset_mhz,
        initial.power_limit_percent,
        initial.temp_limit_c,
        initial.voltage_boost_percent,
        fan_text(&initial),
        initial.locked_clock_min_mhz,
        initial.locked_clock_max_mhz,
        if is_stock(&curve) { "stock" } else { "custom" }
    ));

    let steps = plan(&caps, &initial, &curve, &mut log);
    log.say("Plan:");
    for (i, s) in steps.iter().enumerate() {
        log.say(format!("  {}. {}", i + 1, s.describe(&initial, &curve)));
    }
    if !write {
        println!("\nDry run: nothing was written. Re-run with --write to execute the plan.");
        return ExitCode::SUCCESS;
    }

    log.say("\nRunning:");
    let mut failed = false;
    for (i, step) in steps.iter().enumerate() {
        match run_step(&mut b, step, &initial) {
            Ok(()) => log.say(format!("  PASS {}. {}", i + 1, step.describe(&initial, &curve))),
            Err(e) => {
                log.say(format!("  FAIL {}. {}: {e}", i + 1, step.describe(&initial, &curve)));
                failed = true;
                break;
            }
        }
    }

    log.say("\nRestoring the starting state...");
    restore_all(&mut b, &initial, is_stock(&curve), &mut log);
    let end = b.read_tuning_state(GPU);
    let end_curve = b.read_vf_curve(GPU);
    let state_ok = end.as_ref().is_ok_and(|s| *s == initial);
    let curve_ok = end_curve.as_ref().is_ok_and(|c| frequencies(c) == frequencies(&curve));
    if state_ok && curve_ok {
        log.say("Final state matches the snapshot (all settings, fans, clock range, V/F frequencies).");
    } else {
        failed = true;
        log.say("FINAL STATE DIFFERS FROM THE SNAPSHOT:");
        log.say(format!("  was   {initial:?}\n  now   {end:?}"));
        log.say(format!("  curve was {:?}\n  curve now {:?}", frequencies(&curve), end_curve.map(|c| frequencies(&c))));
    }
    log.say(if failed { "\nRESULT: FAILED" } else { "\nRESULT: PASSED" });
    save(&log);
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
