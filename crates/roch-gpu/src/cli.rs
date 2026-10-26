//! RochGPU.exe — the headless half of the one executable (C# `GpuTuner.Cli/Program.cs`). Same
//! engine, no window. See `main.rs` for which half runs.
//!
//! ```text
//!   RochGPU.exe info [--gpu 0]             show GPUs, capabilities, current tuning
//!   RochGPU.exe monitor [--gpu 0] [--interval 1000]  live telemetry until Ctrl+C
//!   RochGPU.exe apply [--gpu 0] [--core +150] [--mem +800] [--power 90] [--temp 80] [--soft-temp 83] [--fan 60|auto]
//!   RochGPU.exe apply-profile <name> [--gpu 0]     apply a saved GUI profile
//!   RochGPU.exe list-profiles
//!   RochGPU.exe reset [--gpu 0]
//!   RochGPU.exe startup --enable <profile> | --disable | --status
//!   add --mock anywhere to use the simulated GPU (--mock rtx50: one without a hardware temp limit;
//!   --mock multi: a two-card machine)
//! ```

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use roch_core::services::startup_task;
use roch_core::services::{AppSettings, ProfileStore, ServiceEvent, TuningService};
use roch_core::{FanMode, GpuError, GpuTelemetry, NOTE_PREFIX, TuningProfile, VoltageControlStyle, XocLever};

use crate::{backends, console};

macro_rules! outln {
    ($w:expr) => { let _ = writeln!($w); };
    ($w:expr, $($arg:tt)*) => { let _ = writeln!($w, $($arg)*); };
}

/// How a command can fail, mapped onto the C# catch blocks and their exit codes.
#[derive(Debug)]
pub enum CliError {
    /// `FormatException`: a numeric option was not a number (exit 2).
    BadNumber,
    /// `ArgumentOutOfRangeException`: no GPU at that `--gpu` index (exit 2).
    BadGpuIndex,
    /// `GpuBackendException` (exit 1).
    Gpu(GpuError),
}

impl From<GpuError> for CliError {
    fn from(e: GpuError) -> Self {
        CliError::Gpu(e)
    }
}

type CliResult<T> = std::result::Result<T, CliError>;

/// Run the command line against the real console. `args` excludes the program name.
pub fn run(args: &[String]) -> i32 {
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    let code = run_with(args, None, &mut out, &mut err);
    let _ = out.flush();
    let _ = err.flush();
    code
}

/// Run with an explicit store folder (`None` = %AppData%\Roch GPU) and output streams.
pub fn run_with(args: &[String], store_root: Option<PathBuf>, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        usage(out);
        return 0;
    }
    let opts = Opts::parse(&args[1..]);
    match dispatch(args, &opts, store_root, out, err) {
        Ok(code) => code,
        Err(CliError::BadNumber) => {
            outln!(err, "A numeric option was not a number. Values are plain integers, e.g. --core 120 --power 110.");
            2
        }
        Err(CliError::BadGpuIndex) => {
            outln!(err, "No GPU at that --gpu index. Run 'RochGPU.exe info' to list what was found.");
            2
        }
        Err(CliError::Gpu(e)) => {
            outln!(err, "ERROR: {}", e.message);
            1
        }
    }
}

fn dispatch(
    args: &[String],
    opts: &Opts,
    store_root: Option<PathBuf>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> CliResult<i32> {
    let mock = opts.contains("mock");
    // int.TryParse semantics: anything unparsable is GPU 0; a negative index is simply out of range.
    let gpu = opts.get("gpu").and_then(|s| s.trim().parse::<i32>().ok()).unwrap_or(0);
    let gpu = usize::try_from(gpu).unwrap_or(usize::MAX);

    let backend = if mock { backends::mock_variant(opts.get("mock")) } else { backends::create_and_initialize()?.0 };
    crate::trace!("cli: backend detected ({})", backend.backend_name());
    let svc = TuningService::new(backend);
    let store = ProfileStore::new(store_root)?;

    match args[0].to_lowercase().as_str() {
        "info" => {
            start(&svc, &store, gpu)?;
            info(&svc, out)
        }
        "sensors" => {
            start(&svc, &store, gpu)?;
            let telemetry = sample_telemetry(&svc)?;
            outln!(out, "{}", fmt_telemetry(&telemetry));
            for sensor in &telemetry.supplemental_sensors {
                outln!(out, "{}: {}", sensor.name, fmt_sensor(sensor.value, &sensor.unit));
            }
            Ok(0)
        }
        "monitor" => {
            start(&svc, &store, gpu)?;
            monitor(&svc, &store, opts, out)
        }
        "apply" => {
            start(&svc, &store, gpu)?;
            apply(&svc, opts, out, err)
        }
        "apply-profile" => {
            let Some(name) = args.get(1) else {
                outln!(err, "profile name required");
                return Ok(2);
            };
            start(&svc, &store, gpu)?;
            apply_profile(&svc, &store, name, out, err)
        }
        "list-profiles" => {
            for n in store.list_profile_names() {
                outln!(out, "{n}");
            }
            Ok(0)
        }
        "reset" => {
            start(&svc, &store, gpu)?;
            svc.reset_to_defaults()?;
            outln!(out, "Reset to defaults.");
            Ok(0)
        }
        "domains" => domains(&svc, gpu, opts, out, err),
        "diag" => {
            initialize(&svc, gpu)?;
            let dump = svc.with_backend(|b| b.diagnostics(gpu));
            outln!(out, "{dump}");
            if let Ok(dir) = std::env::current_dir() {
                let path = dir.join("roch-gpu-diag.txt");
                if std::fs::write(&path, &dump).is_ok() {
                    outln!(out, "(also written to {})", path.display());
                }
            }
            Ok(0)
        }
        "startup" => startup(opts, out, err),
        _ => {
            usage(out);
            Ok(2)
        }
    }
}

/// Initialise the service; an index the backend did not report is the C# out-of-range case.
fn initialize(svc: &TuningService, gpu: usize) -> CliResult<()> {
    svc.initialize(gpu).map_err(|e| if gpu >= svc.devices().len() { CliError::BadGpuIndex } else { CliError::Gpu(e) })
}

/// Every command that reasons about voltage needs the ceilings an earlier run measured; without
/// them an undervolt offset is taken from the V/F table's top rather than from where this card
/// actually runs, which can be tens of millivolts out.
fn start(svc: &TuningService, store: &ProfileStore, gpu: usize) -> CliResult<()> {
    initialize(svc, gpu)?;
    crate::trace!("cli: service initialised");
    let mut s = store.load_settings();
    let name = svc.device().name;
    seed_observed(svc, &s, &name);
    if seed_rail_defaults(svc, &mut s, &name) {
        store.save_settings(&s)?;
    }
    Ok(())
}

fn seed_observed(svc: &TuningService, s: &AppSettings, name: &str) {
    if let Some(&seen) = s.observed_max_voltage_by_gpu.get(name) {
        svc.seed_observed_max_voltage(seen);
    }
    if let Some(&seen) = s.observed_max_boosted_voltage_by_gpu.get(name) {
        svc.seed_observed_max_boosted_voltage(seen);
    }
}

/// Carry the rail ceilings forward, recording them the first time a GPU is seen. Returns true
/// when something was recorded and the settings need saving.
pub fn seed_rail_defaults(svc: &TuningService, s: &mut AppSettings, gpu_name: &str) -> bool {
    let c = svc.capabilities();
    let mut dirty = false;
    if c.can_set_voltage_rail && !s.nvvdd_default_max_by_gpu.contains_key(gpu_name) && c.voltage_rail_stock_max_mv > 0 {
        s.nvvdd_default_max_by_gpu.insert(gpu_name.to_string(), c.voltage_rail_stock_max_mv);
        dirty = true;
    }
    if c.can_set_msvdd_rail && !s.msvdd_default_max_by_gpu.contains_key(gpu_name) && c.msvdd_rail_stock_max_mv > 0 {
        s.msvdd_default_max_by_gpu.insert(gpu_name.to_string(), c.msvdd_rail_stock_max_mv);
        dirty = true;
    }
    let nv = s.nvvdd_default_max_by_gpu.get(gpu_name).copied().unwrap_or(0);
    let ms = s.msvdd_default_max_by_gpu.get(gpu_name).copied().unwrap_or(0);
    svc.seed_rail_defaults(nv, ms);
    dirty
}

fn range_or(can: bool, text: impl FnOnce() -> String) -> String {
    if can { text() } else { "not supported".into() }
}

fn info(svc: &TuningService, out: &mut dyn Write) -> CliResult<i32> {
    for d in svc.devices() {
        outln!(
            out,
            "[{}] {}  {} MB  driver {}  vBIOS {}  ({})",
            d.index,
            d.name,
            d.vram_megabytes,
            d.driver_version,
            d.bios_version,
            d.bus_id
        );
    }
    let c = svc.capabilities();
    outln!(out);
    if c.core_clock_is_absolute {
        if c.can_set_core_min_clock {
            outln!(
                out,
                "Min core clock: {}..{} MHz (stock {})",
                c.core_min_clock_min_mhz,
                c.core_min_clock_max_mhz,
                c.core_min_clock_default_mhz
            );
        }
        outln!(
            out,
            "Max core clock: {}",
            range_or(c.can_set_core_offset, || format!(
                "{}..{} MHz (stock {})",
                c.core_offset_min_mhz, c.core_offset_max_mhz, c.core_clock_default_mhz
            ))
        );
    } else {
        outln!(
            out,
            "Core offset   : {}",
            range_or(c.can_set_core_offset, || format!("{}..{} MHz", c.core_offset_min_mhz, c.core_offset_max_mhz))
        );
    }
    outln!(
        out,
        "Memory tuning : {}",
        range_or(c.can_set_memory_offset, || format!(
            "{}..{} {}",
            c.memory_offset_min_mhz, c.memory_offset_max_mhz, c.memory_clock_unit
        ))
    );
    outln!(
        out,
        "Power limit   : {}",
        range_or(c.can_set_power_limit, || format!(
            "{}..{} % (default {})",
            c.power_limit_min_percent, c.power_limit_max_percent, c.power_limit_default_percent
        ))
    );
    outln!(
        out,
        "Temp limit    : {}",
        range_or(c.can_set_temp_limit, || format!(
            "{}..{} {} (default {})",
            c.temp_limit_min_c,
            c.temp_limit_max_c,
            c.temp_limit_unit(),
            c.temp_limit_default_c
        ))
    );
    // Only where it is offered: a card with its own limit prints exactly what v1.1.0 printed.
    if c.can_set_soft_thermal_limit {
        outln!(
            out,
            "Soft temp limit: {}..{} °C (default {} °C — the card's own limit where the driver reports one, otherwise the top of the range; at or above it the card manages itself; below it the app caps clocks while it runs)",
            c.soft_thermal_limit_min_c,
            c.soft_thermal_limit_max_c,
            c.soft_thermal_card_limit_c
        );
    }
    let voltage_label = if c.voltage_style == VoltageControlStyle::Percent { "Voltage limit" } else { "Voltage boost" };
    outln!(
        out,
        "{voltage_label} : {}",
        range_or(c.can_set_voltage_boost, || format!(
            "{}..{} % (default {})",
            c.voltage_boost_min_percent, c.voltage_boost_max_percent, c.voltage_boost_default_percent
        ))
    );
    // Two undervolt mechanisms reach the same slider: NVIDIA flattens the V/F curve, AMD offsets
    // the whole curve and has no editable one. Gating on can_set_voltage_curve alone reported "not
    // supported" on a card that was actively undervolted.
    let can_undervolt = c.can_set_voltage_curve || c.voltage_style == VoltageControlStyle::Offset;
    let undervolt = if !can_undervolt {
        "not supported".to_string()
    } else if c.can_set_voltage_curve {
        format!("{}..0 mV (stock max {} mV)", c.voltage_offset_min_mv, c.stock_max_voltage_mv)
    } else if c.voltage_offset_is_absolute {
        format!("{}..{} mV (stock {})", c.voltage_offset_min_mv, c.voltage_offset_max_mv, c.voltage_offset_default_mv)
    } else {
        format!("{}..{} mV (whole-curve offset)", c.voltage_offset_min_mv, c.voltage_offset_max_mv)
    };
    if c.voltage_offset_is_absolute {
        outln!(out, "Max voltage   : {undervolt}");
    } else {
        outln!(out, "Undervolt     : {undervolt}");
    }
    if c.can_edit_vf_curve {
        outln!(
            out,
            "V/F curve     : editable, {}..{} MHz, {} MHz steps",
            c.vf_curve_min_mhz,
            c.vf_curve_max_mhz,
            c.vf_curve_step_mhz
        );
    }
    outln!(
        out,
        "Core rail     : {}",
        range_or(c.can_set_voltage_rail, || format!(
            "{}..{} mV ceiling (now {} mV), floor {}..{} mV (stock {} mV)",
            c.voltage_rail_min_mv,
            c.voltage_rail_max_mv,
            c.voltage_rail_stock_max_mv,
            c.voltage_rail_floor_min_mv,
            c.voltage_rail_floor_max_mv,
            c.voltage_rail_stock_floor_mv
        ))
    );
    outln!(
        out,
        "MSVDD rail    : {}",
        range_or(c.can_set_msvdd_rail, || format!(
            "{}..{} mV ceiling (now {} mV), floor {}..{} mV (stock {} mV)",
            c.msvdd_rail_min_mv,
            c.msvdd_rail_max_mv,
            c.msvdd_rail_stock_max_mv,
            c.msvdd_rail_floor_min_mv,
            c.msvdd_rail_floor_max_mv,
            c.msvdd_rail_stock_floor_mv
        ))
    );
    outln!(
        out,
        "Clock range   : {}",
        range_or(c.can_lock_clocks, || format!("{}..{} MHz lockable", c.clock_lock_min_mhz, c.clock_lock_max_mhz))
    );
    outln!(
        out,
        "Crossbar      : {}",
        range_or(c.can_set_xbar_offset, || format!("{}..{} MHz offset", c.xbar_offset_min_mhz, c.xbar_offset_max_mhz))
    );
    outln!(
        out,
        "SYS clock     : {}",
        range_or(c.can_set_sys_offset, || format!("{}..{} MHz offset", c.sys_offset_min_mhz, c.sys_offset_max_mhz))
    );
    outln!(
        out,
        "Video clock   : {}",
        range_or(c.can_set_video_offset, || format!(
            "{}..{} MHz offset",
            c.video_offset_min_mhz, c.video_offset_max_mhz
        ))
    );
    outln!(
        out,
        "Fans          : {}",
        range_or(c.can_set_fan_speed, || format!(
            "{} fan(s), {}..{} %",
            c.fan_count, c.fan_min_percent, c.fan_max_percent
        ))
    );

    let s = svc.read_tuning_state()?;
    outln!(out);
    // A backend that can read the three fan modes apart says so; the rest only expose a manual
    // flag, through which a hardware curve is indistinguishable from auto.
    let fan = match s.detected_fan_mode {
        Some(FanMode::Fixed) => format!("{}% fixed", s.fan_percent),
        Some(FanMode::Curve) => "hardware curve".into(),
        Some(FanMode::Auto) => "auto".into(),
        None if s.fan_manual => format!("{}% manual", s.fan_percent),
        None => "auto".into(),
    };
    // 0 means the card reported no thermal policy at all, which is not a limit of 0.
    let temp_limit =
        if s.temp_limit_c > 0 { format!("{}{}", s.temp_limit_c, c.temp_limit_unit()) } else { "n/a".into() };

    // Measure the undervolt from the same ceiling an apply measures it from. The backend reports it
    // against the V/F table's top, which on a card that never reaches that top reads tens of
    // millivolts deeper than asked: --uv -90 came back as -145.
    let mut uv_mv = s.voltage_offset_mv;
    if c.voltage_style == VoltageControlStyle::Absolute {
        let lock_mv = svc.read_voltage_lock_mv();
        let ceiling = svc.stock_ceiling_mv();
        uv_mv = if lock_mv > 0 && ceiling > 0 { lock_mv - ceiling } else { 0 };
    }
    let rail = |floor: i32, max: i32| if max > 0 { format!("{floor}-{max} mV") } else { "n/a".into() };
    // Only the controls this card has, gated as the capability lines above are: a 0 for a lever
    // the card does not offer reads as a setting.
    let core = if c.can_set_core_min_clock {
        format!("core {}-{} MHz", s.core_min_clock_mhz, s.core_offset_mhz)
    } else if c.core_clock_is_absolute {
        format!("max core {} MHz", s.core_offset_mhz)
    } else {
        format!("core {} MHz", signed(s.core_offset_mhz))
    };
    let mut current = vec![
        core,
        format!("mem {} {}", s.memory_offset_mhz, c.memory_clock_unit),
        format!("power {}%", s.power_limit_percent),
        format!("temp {temp_limit}"),
    ];
    if c.can_set_voltage_boost {
        let key = if c.voltage_style == VoltageControlStyle::Percent { "vlimit" } else { "vboost" };
        current.push(format!("{key} {}%", s.voltage_boost_percent));
    }
    if c.voltage_offset_is_absolute {
        current.push(format!("max voltage {uv_mv} mV"));
    } else if can_undervolt {
        current.push(format!("uv {uv_mv} mV"));
    }
    if c.can_set_voltage_rail {
        current.push(format!("rail {}", rail(s.voltage_rail_floor_mv, s.voltage_rail_max_mv)));
    }
    if c.can_set_msvdd_rail {
        current.push(format!("msvdd {}", rail(s.msvdd_rail_floor_mv, s.msvdd_rail_max_mv)));
    }
    if c.can_set_xbar_offset {
        current.push(format!("xbar {} MHz", signed(s.xbar_offset_mhz)));
    }
    if c.can_set_sys_offset {
        current.push(format!("sys {} MHz", signed(s.sys_offset_mhz)));
    }
    if c.can_set_video_offset {
        current.push(format!("video {} MHz", signed(s.video_offset_mhz)));
    }
    current.push(format!("fan {fan}"));
    outln!(out, "Current: {}", current.join(", "));
    let t = sample_telemetry(svc)?;
    outln!(out, "{}", fmt_telemetry(&t));
    Ok(0)
}

/// One telemetry sample for the one-shot commands. Intel derives power and load from counters,
/// so its first reading has nothing to difference against; when either is missing, a second
/// reading half a second later fills them in (the GUI and `monitor` get this from polling).
fn sample_telemetry(svc: &TuningService) -> CliResult<GpuTelemetry> {
    let g = svc.gpu_index();
    let first = svc.with_backend(|b| b.read_telemetry(g))?;
    if !first.power_watts.is_nan() && !first.gpu_load_percent.is_nan() {
        return Ok(first);
    }
    std::thread::sleep(Duration::from_millis(500));
    Ok(svc.with_backend(|b| b.read_telemetry(g))?)
}

/// A supplemental reading as `sensors` prints it: a 0/1 "state" flag as No/Yes, as the
/// Telemetry window shows it, otherwise the value and its unit.
fn fmt_sensor(value: f64, unit: &str) -> String {
    if unit == "state" && (value == 0.0 || value == 1.0) {
        return if value == 1.0 { "Yes".into() } else { "No".into() };
    }
    format!("{} {unit}", reading(value, 3))
}

fn monitor(svc: &TuningService, store: &ProfileStore, o: &Opts, out: &mut dyn Write) -> CliResult<i32> {
    let interval = o.get("interval").and_then(|s| s.trim().parse::<i32>().ok()).unwrap_or(1000);
    outln!(out, "Ctrl+C to stop.");

    // The stock-ceiling estimate is only fed while something is polling, and the GUI's monitor is
    // usually closed. Carrying it through the same store the GUI uses means a load run from here
    // teaches both front-ends what the card really tops out at.
    let gpu_name = svc.device().name;
    seed_observed(svc, &store.load_settings(), &gpu_name);
    let mut recorded = svc.observed_max_voltage_mv();
    let mut recorded_boost = svc.observed_max_boosted_voltage_mv();

    console::install_ctrl_c_handler();
    let events = svc.subscribe();
    svc.start_polling(interval.max(0) as u64)?;
    while !console::ctrl_c_pressed() {
        let t = match events.recv_timeout(Duration::from_millis(100)) {
            Ok(ServiceEvent::Telemetry(t)) => t,
            Ok(ServiceEvent::Log(_)) | Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        outln!(out, "{}", fmt_telemetry(&t));
        let stock_moved = svc.observed_max_voltage_mv() > recorded;
        let boost_moved = svc.observed_max_boosted_voltage_mv() > recorded_boost;
        if !stock_moved && !boost_moved {
            continue;
        }
        let mut cur = store.load_settings();
        if stock_moved {
            recorded = svc.observed_max_voltage_mv();
            cur.observed_max_voltage_by_gpu.insert(gpu_name.clone(), recorded);
            outln!(out, "   ceiling: recorded {recorded} mV stock, under load");
        }
        if boost_moved {
            recorded_boost = svc.observed_max_boosted_voltage_mv();
            cur.observed_max_boosted_voltage_by_gpu.insert(gpu_name.clone(), recorded_boost);
            outln!(out, "   ceiling: recorded {recorded_boost} mV boosted, under load");
        }
        store.save_settings(&cur)?;
    }
    Ok(0)
}

/// `int.Parse`: surrounding whitespace and a leading sign are fine, anything else is not.
fn int(s: &str) -> CliResult<i32> {
    s.trim().parse::<i32>().map_err(|_| CliError::BadNumber)
}

/// Fold the command-line options into a profile read from the card. Returns the CLI's own notes
/// (flags this card cannot honour, levers armed) ahead of whatever the apply reports.
pub fn build_apply_profile(
    mut p: TuningProfile,
    caps: &roch_core::GpuCapabilities,
    o: &Opts,
) -> CliResult<(TuningProfile, Vec<String>)> {
    let mut notes = Vec::new();
    if let Some(v) = o.get("core") {
        let mhz = int(v)?;
        let (lo, hi) = (caps.core_offset_min_mhz, caps.core_offset_max_mhz);
        // On an absolute card "--core 120" read as an offset would cap the clock at the bottom of
        // the range, so a value outside it is refused rather than clamped.
        if caps.core_clock_is_absolute && !(lo..=hi).contains(&mhz) {
            notes.push(format!(
                "{NOTE_PREFIX} on this card --core is the maximum core clock, {lo}..{hi} MHz (stock {}) — {v} ignored.",
                caps.core_clock_default_mhz
            ));
        } else {
            p.core_offset_mhz = mhz;
        }
    }
    if let Some(v) = o.get("core-min") {
        let mhz = int(v)?;
        let (lo, hi) = (caps.core_min_clock_min_mhz, caps.core_min_clock_max_mhz);
        if !caps.can_set_core_min_clock {
            notes.push(format!("{NOTE_PREFIX} this card exposes no minimum core clock — {v} MHz ignored."));
        } else if !(lo..=hi).contains(&mhz) {
            notes.push(format!(
                "{NOTE_PREFIX} --core-min is the minimum core clock, {lo}..{hi} MHz (stock {}) — {v} ignored.",
                caps.core_min_clock_default_mhz
            ));
        } else {
            p.core_min_clock_mhz = mhz;
        }
    }
    if let Some(v) = o.get("mem") {
        p.memory_offset_mhz = int(v)?;
    }
    if let Some(v) = o.get("power") {
        p.power_limit_percent = int(v)?;
    }
    if let Some(tp) = o.get("temp") {
        p.temp_limit_c = int(tp)?;
        // Here, not in the service: only this side knows the flag was actually typed rather than
        // carried along from the card's current state.
        if !caps.can_set_temp_limit {
            notes.push(format!("{NOTE_PREFIX} this card exposes no temperature limit — {tp}°C ignored."));
        }
    }
    if let Some(v) = o.get("soft-temp") {
        // "off" or 0 turns it off.
        let t = v.trim();
        p.soft_thermal_limit_c =
            if t.eq_ignore_ascii_case("off") || t.eq_ignore_ascii_case("card") { 0 } else { int(v)? };
        if p.soft_thermal_limit_c > 0 && !caps.can_set_soft_thermal_limit {
            notes.push(if caps.can_set_temp_limit {
                format!("{NOTE_PREFIX} this card has a temperature limit of its own (--temp) — software limit {v}°C ignored.")
            } else {
                format!("{NOTE_PREFIX} this card cannot cap its clock, so it has no software temperature limit — {v}°C ignored.")
            });
            p.soft_thermal_limit_c = 0;
        }
    }
    if let Some(v) = o.get("volt") {
        p.voltage_boost_percent = int(v)?;
    }
    if let Some(v) = o.get("uv") {
        let mv = int(v)?;
        let (lo, hi) = (caps.voltage_offset_min_mv, caps.voltage_offset_max_mv);
        // "--uv -100" on an absolute card would clamp to the lowest voltage the driver allows.
        if caps.voltage_offset_is_absolute && !(lo..=hi).contains(&mv) {
            notes.push(format!(
                "{NOTE_PREFIX} on this card --uv is the maximum core voltage, {lo}..{hi} mV (stock {}) — {v} ignored.",
                caps.voltage_offset_default_mv
            ));
        } else {
            p.voltage_offset_mv = mv;
        }
    }
    if let Some(v) = o.get("clock-min") {
        p.clock_lock_min_mhz = int(v)?;
    }
    if let Some(v) = o.get("clock-max") {
        p.clock_lock_max_mhz = int(v)?;
    }
    if let Some(v) = o.get("sys") {
        p.sys_offset_mhz = int(v)?;
        if !caps.can_set_sys_offset {
            notes.push(format!("{NOTE_PREFIX} this card exposes no SYS clock — {v} MHz ignored."));
        }
    }
    if let Some(v) = o.get("video") {
        p.video_offset_mhz = int(v)?;
        if !caps.can_set_video_offset {
            notes.push(format!("{NOTE_PREFIX} this card exposes no video clock — {v} MHz ignored."));
        }
    }
    if let Some(v) = o.get("xbar") {
        p.xbar_offset_mhz = int(v)?;
        if !caps.can_set_xbar_offset {
            notes.push(format!("{NOTE_PREFIX} this card exposes no crossbar clock — {v} MHz ignored."));
        }
    }
    if let Some(v) = o.get("nvvdd-min") {
        p.voltage_rail_floor_mv = int(v)?;
    }
    if let Some(v) = o.get("msvdd-min") {
        p.msvdd_rail_floor_mv = int(v)?;
    }
    if let Some(v) = o.get("msvdd") {
        p.msvdd_rail_max_mv = int(v)?;
        if !caps.can_set_msvdd_rail {
            notes.push(format!("{NOTE_PREFIX} this card exposes no MSVDD rail - {v} mV ignored."));
        }
    }
    if let Some(v) = o.get("nvvdd") {
        p.voltage_rail_max_mv = int(v)?;
        if !caps.can_set_voltage_rail {
            notes.push(format!("{NOTE_PREFIX} this card exposes no core voltage rail — {v} mV ignored."));
        }
    }
    // Amps on the command line, milliamps in the profile: the unit the window offers, because the
    // card quotes these figures in whole amps.
    if let Some(v) = o.get("nvvdd-ocp") {
        p.nvvdd_ocp_milliamps = int(v)?.wrapping_mul(1000);
        if !caps.can_set_ocp {
            notes.push(format!("{NOTE_PREFIX} this card exposes no OCP limits — {v} A ignored."));
        }
    }
    if let Some(v) = o.get("msvdd-ocp") {
        p.msvdd_ocp_milliamps = int(v)?.wrapping_mul(1000);
        if !caps.can_set_ocp {
            notes.push(format!("{NOTE_PREFIX} this card exposes no OCP limits — {v} A ignored."));
        }
    }

    // Typing a gated flag is the arming gesture for that lever alone. A lever nobody named stays
    // disarmed, and the apply puts it back to the driver's own value rather than leaving an earlier
    // tune half-standing underneath.
    let arms: [(XocLever, &[&str]); 8] = [
        (XocLever::NVVDD, &["nvvdd", "nvvdd-min"]),
        (XocLever::MSVDD, &["msvdd", "msvdd-min"]),
        (XocLever::XBAR, &["xbar"]),
        (XocLever::SYS, &["sys"]),
        (XocLever::VIDEO, &["video"]),
        (XocLever::NVVDD_OCP, &["nvvdd-ocp"]),
        (XocLever::MSVDD_OCP, &["msvdd-ocp"]),
        (XocLever::CLOCK_RANGE, &["clock-min", "clock-max"]),
    ];
    for (lever, flags) in arms {
        if flags.iter().any(|f| o.contains(f)) {
            p.xoc_armed |= lever;
        }
    }
    if !p.xoc_armed.is_empty() {
        notes.push(format!("{NOTE_PREFIX} armed for this apply: {}.", p.xoc_armed));
    }

    if let Some(fan) = o.get("fan") {
        if fan.eq_ignore_ascii_case("auto") {
            p.fan_mode = FanMode::Auto;
        } else {
            p.fan_mode = FanMode::Fixed;
            // "--fan 60" is every fan; "--fan 60,80,80" is one duty each, in cooler order.
            let parts: Vec<&str> = fan.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
            if parts.len() == 1 {
                p.fixed_fan_percent = int(parts[0])?;
            } else {
                p.fixed_fan_percents = parts.iter().map(|s| int(s)).collect::<CliResult<Vec<i32>>>()?;
                // An empty list ("--fan ,") was an IndexOutOfRange crash in the C#.
                p.fixed_fan_percent = *p.fixed_fan_percents.first().ok_or(CliError::BadNumber)?;
                if caps.fan_count > 0 && parts.len() > caps.fan_count as usize {
                    notes.push(format!(
                        "{NOTE_PREFIX} this card reports {} fan(s) — the extra duties are ignored.",
                        caps.fan_count
                    ));
                }
            }
        }
    }
    Ok((p, notes))
}

fn apply(svc: &TuningService, o: &Opts, out: &mut dyn Write, err: &mut dyn Write) -> CliResult<i32> {
    let current = svc.read_current_as_profile()?;
    let caps = svc.capabilities();
    let (p, mut notes) = build_apply_profile(current, &caps, o)?;
    notes.extend(svc.apply(&p));
    notes.extend(soft_thermal_note(&caps, &svc.applied_profile().unwrap_or(p)));
    Ok(report(&notes, out, err))
}

/// The software temperature limit is enforced by a running Roch GPU; this process exits as soon as
/// the apply is done, so say that the limit is not being enforced from here. `p`: what was applied.
fn soft_thermal_note(caps: &roch_core::GpuCapabilities, p: &TuningProfile) -> Option<String> {
    let off = caps.soft_thermal_off_c();
    (caps.can_set_soft_thermal_limit && p.soft_thermal_limit_c > 0 && p.soft_thermal_limit_c < off).then(|| {
        format!(
            "{NOTE_PREFIX} the software temperature limit only acts while Roch GPU is running (window or tray); \
             this command exits, so {} °C is not enforced from here. Set it in the window, or in a startup profile.",
            p.soft_thermal_limit_c
        )
    })
}

fn apply_profile(
    svc: &TuningService,
    store: &ProfileStore,
    name: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> CliResult<i32> {
    let Some(p) = store.load(name)? else {
        outln!(err, "Profile '{name}' not found in {}", store.profiles_directory().display());
        return Ok(2);
    };
    if p.fan_mode == FanMode::Curve && !svc.capabilities().fan_curve_is_hardware {
        outln!(
            err,
            "Note: fan curve mode needs a resident process; the CLI applies clocks/limits and leaves fans on auto. \
             Use the GUI with --minimized for curves."
        );
    }
    let mut errs = svc.apply(&p);
    errs.extend(soft_thermal_note(&svc.capabilities(), &svc.applied_profile().unwrap_or(p)));
    Ok(report(&errs, out, err))
}

/// Notes are what the apply did differently, not what it failed to do — stdout with a success
/// exit code. Only a real failure is stderr and 1.
pub fn report(errs: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    for n in errs.iter().filter(|e| e.starts_with(NOTE_PREFIX)) {
        outln!(out, "{n}");
    }
    let failures: Vec<&String> = errs.iter().filter(|e| !e.starts_with(NOTE_PREFIX)).collect();
    if failures.is_empty() {
        outln!(out, "Applied.");
        return 0;
    }
    for e in failures {
        outln!(err, "{e}");
    }
    1
}

fn startup(o: &Opts, out: &mut dyn Write, err: &mut dyn Write) -> CliResult<i32> {
    if !startup_task::is_windows() {
        outln!(err, "Windows only.");
        return Ok(1);
    }
    if o.contains("status") {
        outln!(out, "{}", if startup_task::exists() { "enabled" } else { "disabled" });
        return Ok(0);
    }
    if o.contains("disable") {
        startup_task::unregister()?;
        outln!(out, "Startup task removed.");
        return Ok(0);
    }
    if let Some(prof) = o.get("enable") {
        let exe = std::env::current_exe().map_err(|e| GpuError::context("cannot resolve own path", e))?;
        startup_task::register(&exe.with_extension("exe").to_string_lossy(), prof)?;
        outln!(out, "Startup task registered: apply '{prof}' at logon (elevated).");
        return Ok(0);
    }
    outln!(err, "use --enable <profile> | --disable | --status");
    Ok(2)
}

// ------------------------------------------------------------------ domains (NVIDIA only)

/// The private clock-domain family, which is where the crossbar lives and where SYS and video
/// turned out to live too. Read-only unless --slot is given, and even then the point is the
/// measurement afterwards rather than the call's return: this family will accept a value and not
/// move, which is exactly what a 4070 Ti's crossbar does.
fn domains(svc: &TuningService, gpu: usize, o: &Opts, out: &mut dyn Write, err: &mut dyn Write) -> CliResult<i32> {
    svc.with_backend(|b| {
        let name = b.backend_name();
        let Some(nv) = nv_domains::nvidia(b) else {
            outln!(err, "Clock domains are an NVIDIA private family; this card uses {name}.");
            return Ok(3);
        };
        Ok(run_domains(nv, gpu, o, out))
    })
}

/// The verb body against any [`nv_domains::DomainAccess`], so it can be tested without a card.
fn run_domains(nv: &mut dyn nv_domains::DomainAccess, gpu: usize, o: &Opts, out: &mut dyn Write) -> i32 {
    if let Some(slot) = o.get("slot").and_then(|s| s.trim().parse::<i32>().ok()) {
        let mhz = o.get("offset").and_then(|s| s.trim().parse::<i32>().ok()).unwrap_or(0);
        let before = nv.read(gpu).into_iter().find(|d| d.slot == slot).map(|d| d.measured_mhz).unwrap_or(0);
        let status = nv.set_offset(gpu, slot, mhz);
        std::thread::sleep(Duration::from_millis(300));
        let after = nv.read(gpu).into_iter().find(|d| d.slot == slot).unwrap_or_default();
        outln!(
            out,
            "slot {slot}: wrote {} MHz, status {status} ({}); measured {before} -> {} MHz, reads back {} MHz",
            signed(mhz),
            if status == 0 { "accepted" } else { "refused" },
            after.measured_mhz,
            signed(after.offset_mhz)
        );
    }
    outln!(out, "slot  type  range              measured   offset");
    for d in nv.read(gpu) {
        outln!(out, "{}", fmt_domain(&d));
    }
    0
}

fn fmt_domain(d: &nv_domains::ClockDomain) -> String {
    let range =
        if d.has_range { format!("{:>6}..{:<6} MHz", d.min_mhz, d.max_mhz) } else { "no range          ".into() };
    format!("{:>4}  {:>4}  {range}  {:>6} MHz  {:>6} MHz", d.slot, d.type_, d.measured_mhz, d.offset_mhz)
}

/// ---- NVIDIA clock domains ----------------------------------------------------------------------
///
/// The `domains` verb reaches the private clock-domain family through this small seam, so the
/// verb body can be tested against a fake without a card.
pub mod nv_domains {
    use roch_core::GpuBackend;
    use roch_nvidia::NvidiaBackend;

    /// One private clock domain as the `domains` verb prints it.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct ClockDomain {
        pub slot: i32,
        /// Printed right-aligned in four columns (the C# printed the raw type value).
        pub type_: String,
        pub has_range: bool,
        pub min_mhz: i32,
        pub max_mhz: i32,
        pub measured_mhz: i32,
        pub offset_mhz: i32,
    }

    /// What the verb needs from the backend.
    pub trait DomainAccess {
        fn read(&mut self, gpu: usize) -> Vec<ClockDomain>;
        /// The driver status of the write; 0 = accepted.
        fn set_offset(&mut self, gpu: usize, slot: i32, mhz: i32) -> i32;
    }

    /// The backend as an NVIDIA one, or `None` for any other vendor (and the mock).
    pub fn nvidia(b: &mut dyn GpuBackend) -> Option<&mut dyn DomainAccess> {
        let nv = b.as_any_mut()?.downcast_mut::<NvidiaBackend>()?;
        Some(nv)
    }

    impl DomainAccess for NvidiaBackend {
        fn read(&mut self, gpu: usize) -> Vec<ClockDomain> {
            self.read_clock_domains(gpu)
                .into_iter()
                .map(|d| ClockDomain {
                    slot: d.slot,
                    type_: d.type_.to_string(),
                    has_range: d.has_range,
                    min_mhz: d.min_mhz,
                    max_mhz: d.max_mhz,
                    measured_mhz: d.measured_mhz,
                    offset_mhz: d.offset_mhz,
                })
                .collect()
        }

        fn set_offset(&mut self, gpu: usize, slot: i32, mhz: i32) -> i32 {
            self.set_clock_domain_offset(gpu, slot, mhz)
        }
    }
}

// ------------------------------------------------------------------ formatting

/// `{v:+#;-#;0}`.
pub fn signed(v: i32) -> String {
    if v > 0 { format!("+{v}") } else { v.to_string() }
}

/// .NET custom numeric format `"0"`, `"0.#"`, `"0.##"`... with `decimals` optional digits:
/// halves round away from zero, trailing zeros are dropped, and a negative value that rounds to
/// zero keeps its sign (".NET Core 3.0+" behaviour).
pub fn num(value: f64, decimals: usize) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "∞".into() } else { "-∞".into() };
    }
    let scale = 10f64.powi(decimals as i32);
    let scaled = value * scale;
    // Rust's formatter breaks exact ties to even; .NET breaks them away from zero.
    let mut text = if scaled.fract().abs() == 0.5 {
        format!("{:.*}", decimals, scaled.round() / scale)
    } else {
        format!("{:.*}", decimals, value)
    };
    if text.contains('.') {
        text = text.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if text == "-0" && !value.is_sign_negative() {
        text = "0".into();
    }
    text
}

/// A reading, or an em dash when the sensor is not exposed (`Reading` in the C#).
pub fn reading(value: f64, decimals: usize) -> String {
    if value.is_finite() { num(value, decimals) } else { "—".into() }
}

/// One telemetry line, as `info`, `sensors` and `monitor` print it.
pub fn fmt_telemetry(t: &GpuTelemetry) -> String {
    let time = t.timestamp.with_timezone(&chrono::Local).format("%H:%M:%S");
    let hs = if t.hot_spot_c.is_nan() { String::new() } else { format!(" hs{}", num(t.hot_spot_c, 0)) };
    let mem =
        if t.memory_temperature_c.is_nan() { String::new() } else { format!(" mem{}", num(t.memory_temperature_c, 0)) };
    let volt = if t.voltage_mv.is_nan() { "   -".to_string() } else { format!("{:>4} mV", num(t.voltage_mv, 0)) };
    // A card whose memory control is a data rate (Intel) reports in the editor's unit.
    let (mem_value, mem_unit) =
        if t.memory_speed_mbps.is_finite() { (t.memory_speed_mbps, "Mbps") } else { (t.memory_clock_mhz, "MHz") };
    format!(
        "{time}  {:>5} MHz core  {:>6} {mem_unit} mem  {:>4}°C{hs}{mem}  {volt}  {:>5}% TDP  load {:>3}%  fan {:>3}% ({} rpm)  {} {}",
        num(t.core_clock_mhz, 0),
        num(mem_value, 0),
        num(t.temperature_c, 1),
        reading(t.power_percent, 1),
        reading(t.gpu_load_percent, 0),
        reading(t.fan_percent, 0),
        reading(t.fan_rpm, 0),
        t.perf_state,
        t.limit_reason
    )
}

// ------------------------------------------------------------------ options

/// `--key value` pairs, keys case-insensitive. A flag with no value maps to "". A later
/// occurrence wins.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Opts(BTreeMap<String, String>);

impl Opts {
    pub fn parse(args: &[String]) -> Opts {
        let mut d = BTreeMap::new();
        let mut key: Option<String> = None;
        for a in args {
            if let Some(k) = a.strip_prefix("--") {
                let k = k.to_lowercase();
                d.insert(k.clone(), String::new());
                key = Some(k);
            } else if let Some(k) = key.take() {
                d.insert(k, a.clone());
            }
        }
        Opts(d)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(&key.to_lowercase()).map(String::as_str)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.0.contains_key(&key.to_lowercase())
    }
}

pub const USAGE: &str = "\
RochGPU.exe — Roch GPU command line / undervolt CLI (needs admin for writes)

  RochGPU.exe info [--gpu 0]
  RochGPU.exe sensors [--gpu 0]          list supplemental native sensor readings
  RochGPU.exe monitor [--gpu 0] [--interval 1000]
  RochGPU.exe apply [--gpu 0] [--core +150] [--core-min 500] [--mem +800] [--power 90] [--temp 80] [--soft-temp 83|card] [--volt 25] [--uv -100] [--nvvdd 1100] [--nvvdd-min 800] [--msvdd 1050] [--msvdd-min 800] [--nvvdd-ocp 300] [--msvdd-ocp 120] [--xbar +100] [--sys +50] [--video +50] [--clock-min 210 --clock-max 2800] [--fan 60|auto]
    each gated flag arms its own lever for that apply; a lever you do not name goes back to the driver's own value
    --soft-temp: a software temperature limit below the card's own (RTX 50); 'card' leaves it to the card; only enforced while the app runs
    --core / --uv on AMD RDNA 3: the maximum core clock (MHz) and maximum voltage (mV), not offsets; see `info` for the range
    --core-min (AMD RDNA 3): the minimum core clock in MHz
  RochGPU.exe apply-profile <name> [--gpu 0]
  RochGPU.exe list-profiles
  RochGPU.exe reset [--gpu 0]
  RochGPU.exe diag                       dump everything the driver reports (for bug reports)
  RochGPU.exe startup --enable <profile> | --disable | --status
  (add --mock to any command to use a simulated GPU; --mock rtx50 for one without a hardware temp limit;
   --mock multi for a two-card machine)";

fn usage(out: &mut dyn Write) {
    outln!(out, "{USAGE}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use roch_core::GpuCapabilities;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    fn temp_store(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("roch-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Run a verb against the mock with a private store; returns (exit, stdout, stderr).
    fn run_mock(line: &str, store: &std::path::Path) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with(&args(line), Some(store.to_path_buf()), &mut out, &mut err);
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    #[test]
    fn option_parsing() {
        let o = Opts::parse(&args("--Core +150 --mock --uv -100 --fan 60,70 --gpu"));
        assert_eq!(o.get("core"), Some("+150"), "keys are case-insensitive");
        assert_eq!(o.get("mock"), Some(""), "a bare flag is present with no value");
        assert_eq!(o.get("uv"), Some("-100"), "a single-dash negative is a value, not a flag");
        assert_eq!(o.get("fan"), Some("60,70"));
        assert!(o.contains("gpu") && o.get("gpu") == Some(""));
        assert!(!o.contains("power"));
        // A stray value with no key before it is ignored; a later key wins.
        let o = Opts::parse(&args("stray --power 90 extra --power 95"));
        assert_eq!(o.get("power"), Some("95"));
        assert_eq!(o.0.len(), 1);
    }

    #[test]
    fn numbers_parse_like_int_parse() {
        assert_eq!(int("+150").unwrap(), 150);
        assert_eq!(int(" -90 ").unwrap(), -90);
        assert!(matches!(int(""), Err(CliError::BadNumber)));
        assert!(matches!(int("12.5"), Err(CliError::BadNumber)));
        assert!(matches!(int("abc"), Err(CliError::BadNumber)));
    }

    #[test]
    fn dotnet_number_formats() {
        assert_eq!(num(2610.0, 0), "2610");
        assert_eq!(num(39.5, 1), "39.5");
        assert_eq!(num(40.0, 1), "40");
        assert_eq!(num(36.25, 1), "36.3", "exact tie rounds away from zero");
        assert_eq!(num(2.5, 0), "3");
        assert_eq!(num(-2.5, 0), "-3");
        assert_eq!(num(0.1234, 3), "0.123");
        assert_eq!(num(-0.2, 0), "-0");
        assert_eq!(num(f64::NAN, 0), "NaN");
        assert_eq!(reading(f64::NAN, 0), "—");
        assert_eq!(reading(f64::INFINITY, 0), "—");
        assert_eq!(signed(150), "+150");
        assert_eq!(signed(-15), "-15");
        assert_eq!(signed(0), "0");
        assert_eq!(fmt_sensor(0.0, "state"), "No");
        assert_eq!(fmt_sensor(1.0, "state"), "Yes");
        assert_eq!(fmt_sensor(f64::NAN, "state"), "— state");
        assert_eq!(fmt_sensor(1.3524, "V"), "1.352 V");
    }

    #[test]
    fn telemetry_line_layout() {
        let t = GpuTelemetry {
            core_clock_mhz: 2610.0,
            memory_clock_mhz: 10501.0,
            temperature_c: 39.5,
            hot_spot_c: 51.5,
            memory_temperature_c: f64::NAN,
            voltage_mv: 998.0,
            power_percent: 36.3,
            gpu_load_percent: 8.0,
            fan_percent: 30.0,
            fan_rpm: f64::NAN,
            perf_state: "P0".into(),
            limit_reason: "None".into(),
            ..Default::default()
        };
        let line = fmt_telemetry(&t);
        let rest = &line[8..]; // after HH:mm:ss
        assert_eq!(
            rest,
            "   2610 MHz core   10501 MHz mem  39.5°C hs52   998 mV   36.3% TDP  load   8%  fan  30% (— rpm)  P0 None"
        );
        let intel = fmt_telemetry(&GpuTelemetry { memory_clock_mhz: 2375.0, memory_speed_mbps: 19000.0, ..t.clone() });
        assert!(intel.contains("   19000 Mbps mem  "), "a memory speed replaces the clock: {intel}");
        let no_volt = fmt_telemetry(&GpuTelemetry { voltage_mv: f64::NAN, ..t });
        assert!(no_volt.contains("°C hs52     -   36.3% TDP"), "{no_volt}");
    }

    #[test]
    fn domain_rows() {
        let ranged = nv_domains::ClockDomain {
            slot: 3,
            type_: "7".into(),
            has_range: true,
            min_mhz: -300,
            max_mhz: 750,
            measured_mhz: 1950,
            offset_mhz: 15,
        };
        assert_eq!(fmt_domain(&ranged), "   3     7    -300..750    MHz    1950 MHz      15 MHz");
        let bare = nv_domains::ClockDomain { slot: 12, type_: "1".into(), ..Default::default() };
        assert_eq!(fmt_domain(&bare), "  12     1  no range                 0 MHz       0 MHz");
    }

    struct FakeDomains(Vec<nv_domains::ClockDomain>);

    impl nv_domains::DomainAccess for FakeDomains {
        fn read(&mut self, _gpu: usize) -> Vec<nv_domains::ClockDomain> {
            self.0.clone()
        }
        fn set_offset(&mut self, _gpu: usize, slot: i32, mhz: i32) -> i32 {
            for d in self.0.iter_mut().filter(|d| d.slot == slot) {
                d.offset_mhz = mhz;
            }
            0
        }
    }

    #[test]
    fn domains_write_reports_and_lists() {
        let mut nv = FakeDomains(vec![nv_domains::ClockDomain { slot: 1, measured_mhz: 1000, ..Default::default() }]);
        let mut out = Vec::new();
        let code = run_domains(&mut nv, 0, &Opts::parse(&args("--slot 1 --offset -15")), &mut out);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(code, 0);
        assert!(text.starts_with(
            "slot 1: wrote -15 MHz, status 0 (accepted); measured 1000 -> 1000 MHz, reads back -15 MHz\n\
             slot  type  range              measured   offset\n"
        ));
    }

    #[test]
    fn apply_flags_arm_levers_and_note_missing_ones() {
        let caps = GpuCapabilities { can_set_voltage_rail: true, fan_count: 3, ..Default::default() };
        let o = Opts::parse(&args("--core +60 --nvvdd 1100 --xbar 30 --temp 70 --fan 60,70,80,90 --nvvdd-ocp 300"));
        let (p, notes) = build_apply_profile(TuningProfile::default(), &caps, &o).unwrap();
        assert_eq!(p.core_offset_mhz, 60);
        assert_eq!(p.voltage_rail_max_mv, 1100);
        assert_eq!(p.nvvdd_ocp_milliamps, 300_000, "amps on the command line, milliamps in the profile");
        assert_eq!(p.xoc_armed, XocLever::NVVDD | XocLever::XBAR | XocLever::NVVDD_OCP);
        assert_eq!(
            (p.fan_mode, p.fixed_fan_percent, p.fixed_fan_percents.clone()),
            (FanMode::Fixed, 60, vec![60, 70, 80, 90])
        );
        assert_eq!(
            notes,
            vec![
                "Note: this card exposes no temperature limit — 70°C ignored.",
                "Note: this card exposes no crossbar clock — 30 MHz ignored.",
                "Note: this card exposes no OCP limits — 300 A ignored.",
                "Note: armed for this apply: Nvvdd, Xbar, NvvddOcp.",
                "Note: this card reports 3 fan(s) — the extra duties are ignored.",
            ]
        );
        let (p, _) = build_apply_profile(TuningProfile::default(), &caps, &Opts::parse(&args("--fan AUTO"))).unwrap();
        assert_eq!(p.fan_mode, FanMode::Auto);
        let (p, _) = build_apply_profile(TuningProfile::default(), &caps, &Opts::parse(&args("--fan 45"))).unwrap();
        assert!(p.fixed_fan_percents.is_empty() && p.fixed_fan_percent == 45, "one duty is every fan");
        assert!(matches!(
            build_apply_profile(TuningProfile::default(), &caps, &Opts::parse(&args("--fan ,"))),
            Err(CliError::BadNumber)
        ));
        assert!(matches!(
            build_apply_profile(TuningProfile::default(), &caps, &Opts::parse(&args("--core"))),
            Err(CliError::BadNumber)
        ));
    }

    #[test]
    fn absolute_core_and_voltage_refuse_offsets() {
        let caps = GpuCapabilities {
            core_offset_min_mhz: 500,
            core_offset_max_mhz: 5000,
            core_clock_is_absolute: true,
            core_clock_default_mhz: 2835,
            voltage_style: VoltageControlStyle::Offset,
            voltage_offset_min_mv: 700,
            voltage_offset_max_mv: 1100,
            voltage_offset_is_absolute: true,
            voltage_offset_default_mv: 1100,
            ..Default::default()
        };
        let current = TuningProfile { core_offset_mhz: 2835, voltage_offset_mv: 1100, ..Default::default() };
        let (p, notes) =
            build_apply_profile(current.clone(), &caps, &Opts::parse(&args("--core +120 --uv -100"))).unwrap();
        assert_eq!((p.core_offset_mhz, p.voltage_offset_mv), (2835, 1100), "offsets leave the card at stock");
        assert_eq!(
            notes,
            vec![
                "Note: on this card --core is the maximum core clock, 500..5000 MHz (stock 2835) — +120 ignored.",
                "Note: on this card --uv is the maximum core voltage, 700..1100 mV (stock 1100) — -100 ignored.",
            ]
        );
        let (p, notes) =
            build_apply_profile(current.clone(), &caps, &Opts::parse(&args("--core 2900 --uv 1050"))).unwrap();
        assert_eq!((p.core_offset_mhz, p.voltage_offset_mv), (2900, 1050), "in-range values are the limits");
        assert!(notes.is_empty(), "{notes:?}");

        // --core-min only where the driver offers a minimum, and only inside its range.
        let (_, notes) = build_apply_profile(current.clone(), &caps, &Opts::parse(&args("--core-min 800"))).unwrap();
        assert_eq!(notes, vec!["Note: this card exposes no minimum core clock — 800 MHz ignored."]);
        let caps = GpuCapabilities {
            can_set_core_min_clock: true,
            core_min_clock_min_mhz: 500,
            core_min_clock_max_mhz: 5000,
            core_min_clock_default_mhz: 500,
            ..caps
        };
        let (p, notes) = build_apply_profile(current.clone(), &caps, &Opts::parse(&args("--core-min 800"))).unwrap();
        assert!(p.core_min_clock_mhz == 800 && notes.is_empty(), "{notes:?}");
        let (p, notes) = build_apply_profile(current, &caps, &Opts::parse(&args("--core-min 50"))).unwrap();
        assert_eq!(p.core_min_clock_mhz, 0, "out of range is left alone");
        assert_eq!(notes, vec!["Note: --core-min is the minimum core clock, 500..5000 MHz (stock 500) — 50 ignored."]);
    }

    #[test]
    fn report_splits_notes_from_failures() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(report(&["Note: x".into()], &mut out, &mut err), 0);
        assert_eq!(String::from_utf8(out).unwrap(), "Note: x\nApplied.\n");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(report(&["Note: x".into(), "Core offset: no".into()], &mut out, &mut err), 1);
        assert_eq!(String::from_utf8(out).unwrap(), "Note: x\n");
        assert_eq!(String::from_utf8(err).unwrap(), "Core offset: no\n");
    }

    #[test]
    fn verbs_against_the_mock() {
        let store = temp_store("verbs");
        let (code, out, _) = run_mock("--help", &store);
        assert_eq!(code, 0);
        assert!(out.starts_with("RochGPU.exe — Roch GPU command line"));

        let (code, out, _) = run_mock("bogus --mock", &store);
        assert_eq!(code, 2, "an unknown verb prints usage and exits 2");
        assert!(out.contains("RochGPU.exe list-profiles"));

        let (code, out, err) = run_mock("info --mock", &store);
        assert_eq!(code, 0, "{err}");
        assert!(out.starts_with("[0] Simulated RTX 4070 SUPER  12282 MB  driver 560.94  vBIOS 95.04.4E.00.6B  (PCI bus 1, slot 0)\n\nCore offset   : -1000..1000 MHz\n"));
        assert!(out.contains("Voltage boost : 0..100 % (default 0)\n"));
        assert!(out.contains("Undervolt     : -300..0 mV (stock max 1100 mV)\n"));
        assert!(out.contains("Core rail     : 800..1150 mV ceiling (now 1035 mV), floor 800..915 mV (stock 800 mV)\n"));
        assert!(out.contains(
            "Current: core 0 MHz, mem 0 MHz, power 100%, temp 83°C, vboost 0%, uv 0 mV, rail 0-1035 mV, msvdd 0-985 mV, xbar 0 MHz, sys 0 MHz, video 0 MHz, fan auto\n"
        ));
        // First sight of the GPU records its rail defaults.
        let s = ProfileStore::new(Some(store.clone())).unwrap().load_settings();
        assert_eq!(s.nvvdd_default_max_by_gpu.get("Simulated RTX 4070 SUPER"), Some(&1035));
        assert_eq!(s.msvdd_default_max_by_gpu.get("Simulated RTX 4070 SUPER"), Some(&985));

        let (code, out, err) = run_mock("apply --mock --core 5000 --temp 70 --nvvdd 1100 --fan 60,70,80,90", &store);
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            out,
            "Note: armed for this apply: Nvvdd.\n\
             Note: this card reports 3 fan(s) — the extra duties are ignored.\n\
             Note: core offset 5000 MHz is outside this card's range — applied 1000 MHz instead.\n\
             Applied.\n"
        );

        let (code, _, err) = run_mock("apply --mock --core abc", &store);
        assert_eq!(
            (code, err.as_str()),
            (2, "A numeric option was not a number. Values are plain integers, e.g. --core 120 --power 110.\n")
        );
        let (code, _, err) = run_mock("apply --mock --gpu 3", &store);
        assert_eq!(
            (code, err.as_str()),
            (2, "No GPU at that --gpu index. Run 'RochGPU.exe info' to list what was found.\n")
        );
        let (code, out, _) = run_mock("reset --mock", &store);
        assert_eq!((code, out.as_str()), (0, "Reset to defaults.\n"));
        let (code, _, err) = run_mock("apply-profile --mock", &store);
        assert_eq!(code, 2);
        assert!(err.starts_with("Profile '--mock' not found in "), "{err}");
        let (code, _, err) = run_mock("domains --mock", &store);
        assert_eq!(
            (code, err.as_str()),
            (3, "Clock domains are an NVIDIA private family; this card uses Simulated GPU.\n")
        );

        // A saved profile applies through the same engine.
        let ps = ProfileStore::new(Some(store.clone())).unwrap();
        ps.save(&mut TuningProfile {
            name: "Slot 1".into(),
            core_offset_mhz: 90,
            fan_mode: FanMode::Curve,
            ..Default::default()
        })
        .unwrap();
        let (code, out, _) = run_mock("list-profiles --mock", &store);
        assert_eq!((code, out.as_str()), (0, "Slot 1\n"));
        let (code, out, err) = run_mock("apply-profile Slot1 --mock", &store);
        assert_eq!(code, 2, "{out}{err}");
        let line = vec!["apply-profile".to_string(), "Slot 1".into(), "--mock".into()];
        let (mut o, mut e) = (Vec::new(), Vec::new());
        assert_eq!(run_with(&line, Some(store.clone()), &mut o, &mut e), 0);
        assert_eq!(String::from_utf8(o).unwrap(), "Applied.\n");
        assert!(String::from_utf8(e).unwrap().starts_with("Note: fan curve mode needs a resident process"));
        let _ = std::fs::remove_dir_all(store);
    }

    #[test]
    fn soft_temp_on_the_simulated_rtx50() {
        let store = temp_store("softtemp");
        // `info` names the window, and only on a card that offers it.
        let (code, out, err) = run_mock("info --mock rtx50", &store);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("Temp limit    : not supported\n"), "{out}");
        assert!(
            out.contains(
                "Soft temp limit: 50..105 °C (default 88 °C — the card's own limit where the driver reports one, otherwise the top of the range; at or above it the card manages itself; below it the app caps clocks while it runs)\n"
            ),
            "{out}"
        );
        let (_, out, _) = run_mock("info --mock", &store);
        assert!(!out.contains("Soft temp limit"), "a card with its own limit prints what v1.1.0 printed");

        // `apply --soft-temp` sets it, and says the CLI cannot enforce it.
        let (code, out, err) = run_mock("apply --mock rtx50 --soft-temp 83", &store);
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            out,
            "Note: the software temperature limit only acts while Roch GPU is running (window or tray); this \
             command exits, so 83 °C is not enforced from here. Set it in the window, or in a startup profile.\n\
             Applied.\n"
        );
        let (code, out, _) = run_mock("apply --mock rtx50 --soft-temp 99", &store);
        assert_eq!(code, 0);
        assert!(
            out.contains("temperature limit 99 °C is at or above this card's own limit (88 °C) — left to the card."),
            "99 °C is above the card's own limit: {out}"
        );
        assert!(!out.contains("is not enforced"), "nothing for the app to enforce: {out}");
        let (code, out, _) = run_mock("apply --mock rtx50 --soft-temp 105", &store);
        assert_eq!(code, 0);
        assert!(
            out.contains("temperature limit 105 °C is at or above this card's own limit (88 °C) — left to the card."),
            "{out}"
        );
        assert!(!out.contains("is not enforced"), "nothing for the app to enforce: {out}");
        let (code, out, _) = run_mock("apply --mock rtx50 --soft-temp off", &store);
        assert_eq!((code, out.as_str()), (0, "Applied.\n"), "off needs no note");
        let (code, _, _) = run_mock("apply --mock rtx50 --soft-temp hot", &store);
        assert_eq!(code, 2, "not a number");

        // A card with a hardware limit points at --temp instead.
        let (code, out, _) = run_mock("apply --mock --soft-temp 80", &store);
        assert_eq!(code, 0);
        assert_eq!(
            out,
            "Note: this card has a temperature limit of its own (--temp) — software limit 80°C ignored.\nApplied.\n"
        );

        // A saved profile carrying it gets the same note from apply-profile.
        let ps = ProfileStore::new(Some(store.clone())).unwrap();
        ps.save(&mut TuningProfile { name: "Cool".into(), soft_thermal_limit_c: 75, ..Default::default() }).unwrap();
        let line = vec!["apply-profile".to_string(), "Cool".into(), "--mock".into(), "rtx50".into()];
        let (mut o, mut e) = (Vec::new(), Vec::new());
        assert_eq!(run_with(&line, Some(store.clone()), &mut o, &mut e), 0);
        assert!(String::from_utf8(o).unwrap().contains("so 75 °C is not enforced from here"));
        let _ = std::fs::remove_dir_all(store);
    }

    #[test]
    fn soft_temp_flag_folds_into_the_profile() {
        let caps = GpuCapabilities {
            can_lock_clocks: true,
            can_set_soft_thermal_limit: true,
            soft_thermal_limit_min_c: 50,
            soft_thermal_limit_max_c: 105,
            ..Default::default()
        };
        let (p, notes) =
            build_apply_profile(TuningProfile::default(), &caps, &Opts::parse(&args("--soft-temp 80"))).unwrap();
        assert_eq!((p.soft_thermal_limit_c, notes.len()), (80, 0));
        let (p, _) = build_apply_profile(p, &caps, &Opts::parse(&args("--soft-temp 105"))).unwrap();
        assert_eq!(p.soft_thermal_limit_c, 105, "the top is folded in; the service leaves the card to itself");
        let (p, _) = build_apply_profile(p, &caps, &Opts::parse(&args("--soft-temp 0"))).unwrap();
        assert_eq!(p.soft_thermal_limit_c, 0);
        // A card that cannot cap its clock.
        let (p, notes) = build_apply_profile(
            TuningProfile::default(),
            &GpuCapabilities::default(),
            &Opts::parse(&args("--soft-temp 80")),
        )
        .unwrap();
        assert_eq!(p.soft_thermal_limit_c, 0);
        assert_eq!(
            notes,
            vec!["Note: this card cannot cap its clock, so it has no software temperature limit — 80°C ignored."]
        );
    }
}
