//! Audit dump: everything the window shows, as normalized `key = value` lines, for a
//! field-by-field comparison with the C# v1.1.0 build (whose twin harness lives outside the repo;
//! see `docs/rust-port-audit.md`). Test-only and ignored by default:
//!
//! ```text
//! set ROCH_AUDIT_MODE=mock|real
//! set ROCH_AUDIT_DIR=<a temp COPY of %AppData%\Roch GPU>
//! set ROCH_AUDIT_OUT=<out file>
//! cargo test -p roch-gpu --release audit_dump -- --ignored --exact audit_dump::audit_dump
//! ```
//!
//! READ-ONLY against hardware. In `real` mode the calls are: backend detection (vendor init +
//! enumeration), `TuningService::initialize` (capabilities, tuning state, voltage lock),
//! `read_tuning_state`, `read_voltage_lock_mv`, `read_graphics_info`, `read_vf_curve`,
//! `measure_extra_clocks`, one backend `read_telemetry`, `read_current_as_profile`, and
//! `MainViewModel::new` (the same reads, the profile store and `schtasks /Query`). No job is
//! queued or run, no `apply`/`set_*`/reset/fan/lever call is made, and the logon task is a stub
//! whose register/unregister panic. The recovery journal's apply closure is a tripwire on real
//! hardware. The mock-only block at the end exercises Apply, levers, slots and Reset.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use roch_core::services::{ProfileRecovery, ProfileStore, TuningService, startup_task};
use roch_core::{
    FanCurve, FanMode, FanPoint, GpuCapabilities, GpuDevice, GpuGraphicsInfo, GpuTelemetry, GpuTuningState,
    SupplementalSensor, TuningProfile, VoltageControlStyle, XocLever,
};

use crate::backends;
use crate::gui::shared::{Shared, StartupTasks};
use crate::gui::telemetry_table::TelemetryTable;
use crate::gui::vf_editor::VfEditor;
use crate::gui::view_model::{Field, MainViewModel, title_text};
use crate::gui::windows::{curve, fan, monitor};

// ------------------------------------------------------------------ normalization

/// One value in the shared text format (the C# harness's `N`).
trait Norm {
    fn norm(&self) -> String;
}

fn d(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v == f64::INFINITY {
        "inf".into()
    } else if v == f64::NEG_INFINITY {
        "-inf".into()
    } else {
        format!("{v}")
    }
}

impl Norm for bool {
    fn norm(&self) -> String {
        self.to_string()
    }
}
impl Norm for i32 {
    fn norm(&self) -> String {
        self.to_string()
    }
}
impl Norm for i64 {
    fn norm(&self) -> String {
        self.to_string()
    }
}
impl Norm for usize {
    fn norm(&self) -> String {
        self.to_string()
    }
}
impl Norm for f64 {
    fn norm(&self) -> String {
        d(*self)
    }
}
impl Norm for String {
    fn norm(&self) -> String {
        self.replace('\r', "\\r").replace('\n', "\\n")
    }
}
impl Norm for &str {
    fn norm(&self) -> String {
        self.to_string().norm()
    }
}
impl<T: Norm> Norm for Vec<T> {
    fn norm(&self) -> String {
        format!("[{}]", self.iter().map(Norm::norm).collect::<Vec<_>>().join(", "))
    }
}
impl<T: Norm> Norm for [T] {
    fn norm(&self) -> String {
        format!("[{}]", self.iter().map(Norm::norm).collect::<Vec<_>>().join(", "))
    }
}
impl<T: Norm> Norm for Option<T> {
    fn norm(&self) -> String {
        self.as_ref().map_or("null".into(), Norm::norm)
    }
}
impl Norm for XocLever {
    fn norm(&self) -> String {
        self.0.to_string()
    }
}
impl Norm for FanMode {
    fn norm(&self) -> String {
        self.name().into()
    }
}
impl Norm for VoltageControlStyle {
    fn norm(&self) -> String {
        format!("{self:?}")
    }
}
impl Norm for FanPoint {
    fn norm(&self) -> String {
        format!("({}, {})", d(self.temperature_c), d(self.fan_percent))
    }
}
impl Norm for FanCurve {
    fn norm(&self) -> String {
        format!(
            "points={} hysteresis={} min_step={} zero_below_first={}",
            self.points.norm(),
            d(self.hysteresis_c),
            d(self.minimum_step_percent),
            self.zero_rpm_below_first_point
        )
    }
}

/// The dump's lines; `prefix` goes in front of every key (the capability variants).
struct Out(Vec<String>, String);

impl Out {
    fn put(&mut self, key: impl AsRef<str>, v: impl Norm) {
        self.0.push(format!("{}{} = {}", self.1, key.as_ref(), v.norm()));
    }
}

/// `put` every listed field of a struct under `prefix`.
macro_rules! fields {
    ($o:expr, $prefix:expr, $s:expr; $($f:ident),* $(,)?) => {
        $( $o.put(format!("{}.{}", $prefix, stringify!($f)), $s.$f.clone()); )*
    };
}

fn device(o: &mut Out, prefix: &str, x: &GpuDevice) {
    fields!(o, prefix, x; index, name, vendor, bus_id, driver_version, vram_megabytes, bios_version);
}

fn caps(o: &mut Out, prefix: &str, c: &GpuCapabilities) {
    fields!(o, prefix, c;
        core_offset_step_mhz, memory_clock_unit, temp_limit_is_percent, can_edit_vf_curve,
        can_read_hardware_fan_curve, vf_curve_step_mhz, vf_curve_min_mhz, vf_curve_max_mhz,
        can_set_core_offset, can_set_memory_offset, can_set_power_limit, can_set_temp_limit,
        can_set_fan_speed, can_read_voltage, can_set_voltage_boost, voltage_boost_min_percent,
        voltage_boost_max_percent, voltage_boost_default_percent, can_set_voltage_curve,
        voltage_offset_min_mv, voltage_offset_max_mv, stock_max_voltage_mv, curve_unavailable_reason,
        min_voltage_mv, max_voltage_mv, core_offset_min_mhz, core_offset_max_mhz, memory_offset_min_mhz,
        memory_offset_max_mhz, power_limit_min_percent, power_limit_max_percent,
        power_limit_default_percent, temp_limit_min_c, temp_limit_max_c, temp_limit_default_c,
        fan_min_percent, fan_max_percent, fan_count, can_set_voltage_rail, voltage_rail_min_mv,
        voltage_rail_max_mv, voltage_rail_stock_max_mv, voltage_rail_floor_min_mv,
        voltage_rail_floor_max_mv, voltage_rail_stock_floor_mv, can_set_msvdd_rail, msvdd_rail_min_mv,
        msvdd_rail_max_mv, msvdd_rail_stock_max_mv, msvdd_rail_floor_min_mv, msvdd_rail_floor_max_mv,
        msvdd_rail_stock_floor_mv, can_set_ocp, nvvdd_ocp_stock_milliamps, msvdd_ocp_stock_milliamps,
        nvvdd_ocp_min_milliamps, nvvdd_ocp_max_milliamps, msvdd_ocp_min_milliamps,
        msvdd_ocp_max_milliamps, can_set_sys_offset, sys_offset_min_mhz, sys_offset_max_mhz,
        can_set_video_offset, video_offset_min_mhz, video_offset_max_mhz, can_lock_clocks,
        clock_lock_min_mhz, clock_lock_max_mhz, can_set_xbar_offset, xbar_offset_min_mhz,
        xbar_offset_max_mhz, voltage_style, memory_clock_is_absolute, memory_clock_default_mhz,
        power_limit_is_offset, can_set_zero_rpm, zero_rpm_default, can_set_memory_timing,
        memory_timing_options, memory_timing_default_level, fan_curve_is_hardware, fan_curve_points,
        fan_curve_min_temp_c, fan_curve_max_temp_c);
    o.put(format!("{prefix}.temp_limit_unit"), c.temp_limit_unit());
    o.put(format!("{prefix}.can_set_voltage"), c.can_set_voltage());
}

fn state(o: &mut Out, prefix: &str, s: &GpuTuningState) {
    fields!(o, prefix, s;
        hardware_fan_curve, core_offset_mhz, memory_offset_mhz, power_limit_percent, temp_limit_c,
        voltage_boost_percent, voltage_offset_mv, voltage_rail_max_mv, msvdd_rail_max_mv,
        voltage_rail_floor_mv, msvdd_rail_floor_mv, xbar_offset_mhz, nvvdd_ocp_milliamps,
        msvdd_ocp_milliamps, sys_offset_mhz, video_offset_mhz, locked_clock_min_mhz,
        locked_clock_max_mhz, fan_manual, fan_percent, detected_fan_mode, zero_rpm, memory_timing_level);
}

fn graphics_info(o: &mut Out, prefix: &str, g: &GpuGraphicsInfo) {
    fields!(o, prefix, g;
        gpu, board_manufacturer, code_name, revision, cores, rops_tmus, technology, memory_size,
        memory_type, memory_vendor, bus_width, bus_interface, resizable_bar, bios_version, bus_address,
        driver_version, driver_date);
}

fn profile(o: &mut Out, prefix: &str, p: &TuningProfile) {
    fields!(o, prefix, p;
        name, version, gpu_name, core_offset_mhz, memory_offset_mhz, power_limit_percent, temp_limit_c,
        voltage_boost_percent, voltage_offset_mv, voltage_rail_max_mv, msvdd_rail_max_mv,
        voltage_rail_floor_mv, msvdd_rail_floor_mv, xbar_offset_mhz, nvvdd_ocp_milliamps,
        msvdd_ocp_milliamps, clock_lock_min_mhz, clock_lock_max_mhz, sys_offset_mhz, video_offset_mhz,
        xoc_armed, target_voltage_mv, zero_rpm, memory_timing_level, fan_mode, fixed_fan_percent,
        fixed_fan_percents, fan_curve);
}

fn telemetry(o: &mut Out, prefix: &str, t: &GpuTelemetry) {
    let scalars = [
        ("core_clock_mhz", t.core_clock_mhz),
        ("memory_clock_mhz", t.memory_clock_mhz),
        ("fabric_clock_mhz", t.fabric_clock_mhz),
        ("soc_clock_mhz", t.soc_clock_mhz),
        ("temperature_c", t.temperature_c),
        ("hot_spot_c", t.hot_spot_c),
        ("memory_temperature_c", t.memory_temperature_c),
        ("voltage_mv", t.voltage_mv),
        ("nvvdd_mv", t.nvvdd_mv),
        ("msvdd_mv", t.msvdd_mv),
        ("power_percent", t.power_percent),
        ("power_watts", t.power_watts),
        ("gpu_load_percent", t.gpu_load_percent),
        ("memory_load_percent", t.memory_load_percent),
        ("memory_used_mb", t.memory_used_mb),
        ("fan_percent", t.fan_percent),
        ("fan_rpm", t.fan_rpm),
    ];
    for (k, v) in scalars {
        o.put(format!("{prefix}.present.{k}"), if v.is_nan() { "NaN" } else { "value" });
        o.put(format!("live.{prefix}.{k}"), v);
    }
    for (k, v) in [
        ("rail_amps", &t.rail_amps),
        ("rail_volts", &t.rail_volts),
        ("fan_percents", &t.fan_percents),
        ("fan_rpms", &t.fan_rpms),
    ] {
        o.put(format!("{prefix}.len.{k}"), v.len());
        o.put(format!("live.{prefix}.{k}"), v.clone());
    }
    o.put(format!("live.{prefix}.perf_state"), t.perf_state.clone());
    o.put(format!("live.{prefix}.limit_reason"), t.limit_reason.clone());
    o.put(format!("{prefix}.supplemental.count"), t.supplemental_sensors.len());
    for (i, s) in t.supplemental_sensors.iter().enumerate() {
        o.put(format!("{prefix}.supplemental.{i:02}"), format!("{} | {} | {}", s.key, s.name, s.unit));
        o.put(format!("live.{prefix}.supplemental.{}", s.key), s.value);
    }
}

fn ceilings(o: &mut Out, prefix: &str, svc: &TuningService) {
    o.put(format!("{prefix}.stock_ceiling_mv"), svc.stock_ceiling_mv());
    o.put(format!("{prefix}.boost_ceiling_mv"), svc.boost_ceiling_mv());
    for b in [0, 5, 25, 50, 75, 100] {
        o.put(format!("{prefix}.reachable_ceiling_mv.{b}"), svc.reachable_ceiling_mv(b));
    }
    o.put(format!("{prefix}.armed_on_card"), svc.armed_on_card());
    o.put(format!("{prefix}.supported_levers"), svc.supported_levers());
    o.put(format!("{prefix}.observed_max_voltage_mv"), svc.observed_max_voltage_mv());
    o.put(format!("{prefix}.observed_max_boosted_voltage_mv"), svc.observed_max_boosted_voltage_mv());
    o.put(format!("{prefix}.nvvdd_default_max_mv"), svc.nvvdd_default_max_mv());
    o.put(format!("{prefix}.msvdd_default_max_mv"), svc.msvdd_default_max_mv());
    o.put(format!("{prefix}.nvvdd_ocp_default_ma"), svc.nvvdd_ocp_default_ma());
    o.put(format!("{prefix}.msvdd_ocp_default_ma"), svc.msvdd_ocp_default_ma());
    o.put(format!("{prefix}.manual_curve_active"), svc.manual_curve_active());
    o.put(format!("{prefix}.applied_profile"), if svc.applied_profile().is_some() { "set" } else { "null" });
    o.put(format!("{prefix}.voltage_lock_mechanism"), svc.voltage_lock_mechanism());
}

/// "CoreOffset" → "core_offset" (the C# harness's `Snake`).
fn snake(name: &str) -> String {
    let mut s = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase() || prev.is_ascii_digit() || (prev.is_ascii_uppercase() && next_lower) {
                s.push('_');
            }
        }
        s.push(c.to_ascii_lowercase());
    }
    s
}

fn view_model(o: &mut Out, vm: &MainViewModel) {
    let mut put = |k: &str, v: String| o.put(format!("vm.{k}"), v);
    put("backend_name", vm.backend_name.clone());
    put("vram_text", vm.vram_text());
    put("telemetry_device_name", vm.telemetry_device_name());
    put("device_display_name", vm.device_display_name());
    put("driver_line", vm.driver_line());
    put("bios_line", vm.bios_line());
    put("memory_type_line", vm.memory_type_line());
    put("pcie_bus_line", vm.pcie_bus_line());
    put("resizable_bar_line", vm.resizable_bar_line());
    put("core_range_text", vm.core_range_text());
    put("mem_range_text", vm.mem_range_text());
    let flags = [
        ("is_voltage_absolute", vm.is_voltage_absolute()),
        ("is_voltage_offset", vm.is_voltage_offset()),
        ("has_voltage_boost", vm.has_voltage_boost()),
        ("has_curve_editor", vm.has_curve_editor()),
        ("has_voltage_rail", vm.has_voltage_rail()),
        ("has_msvdd_rail", vm.has_msvdd_rail()),
        ("has_xbar", vm.has_xbar()),
        ("has_clock_lock", vm.has_clock_lock()),
        ("has_sys", vm.has_sys()),
        ("has_video", vm.has_video()),
        ("has_xoc", vm.has_xoc()),
        ("has_intel_clock_range", vm.has_intel_clock_range()),
        ("intel_clock_range_enabled", vm.intel_clock_range_enabled()),
        ("has_temp_limit", vm.has_temp_limit()),
        ("has_zero_rpm", vm.has_zero_rpm()),
        ("has_memory_timing", vm.has_memory_timing()),
        ("has_ocp", vm.has_ocp()),
        ("zero_rpm", vm.zero_rpm()),
        ("is_fixed_fan", vm.is_fixed_fan()),
        ("is_curve_fan", vm.is_curve_fan()),
        ("pending_changes", vm.pending_changes()),
        ("clock_lock_is_off", vm.clock_lock_is_off()),
        ("limit_is_active", vm.limit_is_active()),
        ("status_is_error", vm.status_is_error),
        ("save_armed", vm.save_armed()),
        ("apply_on_startup", vm.apply_on_startup()),
    ];
    for (k, v) in flags {
        put(k, v.to_string());
    }
    put("memory_label", vm.memory_label().into());
    put("temp_label", vm.temp_label());
    put("voltage_boost_label", vm.voltage_boost_label().into());
    put("core_label", vm.core_label().into());
    put("amd_clock_support_text", vm.amd_clock_support_text());
    put("voltage_label", vm.voltage_label().into());
    put("power_label", vm.power_label().into());
    put("voltage_offset_range_text", vm.voltage_offset_range_text());
    put("voltage_boost_range_text", vm.voltage_boost_range_text());
    put("power_range_text", vm.power_range_text());
    put("temp_range_text", vm.temp_range_text());
    let ints = [
        ("core_step_mhz", vm.core_step_mhz()),
        ("memory_step_mhz", vm.memory_step_mhz()),
        ("power_step_percent", vm.power_step_percent()),
        ("temp_step", vm.temp_step()),
        ("voltage_boost_step_percent", roch_core::models::clock_step::VOLTAGE_BOOST_PERCENT),
        ("nvvdd_ocp_min_a", vm.nvvdd_ocp_min_a()),
        ("nvvdd_ocp_max_a", vm.nvvdd_ocp_max_a()),
        ("msvdd_ocp_min_a", vm.msvdd_ocp_min_a()),
        ("msvdd_ocp_max_a", vm.msvdd_ocp_max_a()),
        ("memory_timing_index", vm.memory_timing_index()),
        ("fan_mode_index", vm.fan_mode_index()),
        ("xoc_armed", vm.xoc_armed().0 as i32),
        ("stock_ceiling_mv", vm.stock_ceiling_mv()),
        ("reachable_ceiling_mv", vm.reachable_ceiling_mv()),
        ("boost_ceiling_mv", vm.boost_ceiling_mv()),
        ("stock_nvvdd_max_mv", vm.stock_nvvdd_max_mv()),
        ("stock_msvdd_max_mv", vm.stock_msvdd_max_mv()),
    ];
    for (k, v) in ints {
        put(k, v.to_string());
    }
    for f in Field::ALL {
        let k = snake(&format!("{f:?}"));
        put(&k, vm.get(f).to_string());
        put(&format!("{k}_input"), vm.input_text(f));
    }
    put("per_fan_percents", vm.per_fan_percents().to_vec().norm());
    put("active_slot_name", vm.active_slot_name().norm());
    put("xoc_status_text", vm.xoc_status_text());
    put("core_offset_text", vm.core_offset_text());
    put("memory_offset_text", vm.memory_offset_text());
    put("power_limit_text", vm.power_limit_text());
    put("temp_limit_text", vm.temp_limit_text());
    put("voltage_offset_text", vm.voltage_offset_text());
    put("target_voltage_text", vm.target_voltage_text());
    put("target_voltage_range_text", vm.target_voltage_range_text());
    put("voltage_cap_text", vm.voltage_cap_text());
    put("voltage_rail_max_text", vm.voltage_rail_max_text());
    put("voltage_rail_max_range_text", vm.voltage_rail_max_range_text());
    put("msvdd_rail_max_text", vm.msvdd_rail_max_text());
    put("msvdd_rail_max_range_text", vm.msvdd_rail_max_range_text());
    put("voltage_rail_floor_text", vm.voltage_rail_floor_text());
    put("voltage_rail_floor_range_text", vm.voltage_rail_floor_range_text());
    put("msvdd_rail_floor_text", vm.msvdd_rail_floor_text());
    put("msvdd_rail_floor_range_text", vm.msvdd_rail_floor_range_text());
    put("clock_lock_range_text", vm.clock_lock_range_text());
    put("sys_offset_range_text", vm.sys_offset_range_text());
    put("video_offset_range_text", vm.video_offset_range_text());
    put("nvvdd_ocp_range_text", vm.nvvdd_ocp_range_text());
    put("msvdd_ocp_range_text", vm.msvdd_ocp_range_text());
    put("xbar_offset_range_text", vm.xbar_offset_range_text());
    put("limit_reason_text", vm.limit_reason_text());
    put("status", vm.status.clone());
    put("applied_summary", vm.applied_summary().to_string());
    put("selected_profile", vm.selected_profile().map(str::to_string).norm());
    put("profile_name_input", vm.profile_name_input.clone());
    put("startup_profile", vm.startup_profile().map(str::to_string).norm());
    put("title_text", title_text());
    put("editor_curve", vm.editor_curve().norm());
    put("memory_timing_options", vm.memory_timing_options().to_vec().norm());
    put("profiles", vm.profiles().to_vec().norm());
    for s in vm.slots() {
        let sp = format!("slot.{}", s.number);
        put(&format!("{sp}.number"), s.number.to_string());
        put(&format!("{sp}.name"), s.name());
        put(&format!("{sp}.label"), s.label());
        put(&format!("{sp}.occupied"), s.occupied.to_string());
        put(&format!("{sp}.is_active"), s.is_active.to_string());
        put(&format!("{sp}.summary"), s.summary.clone());
        put(&format!("{sp}.tip"), s.tip());
    }
    for (k, l) in [
        ("nvvdd", XocLever::NVVDD),
        ("msvdd", XocLever::MSVDD),
        ("nvvdd_ocp", XocLever::NVVDD_OCP),
        ("msvdd_ocp", XocLever::MSVDD_OCP),
        ("clock_range", XocLever::CLOCK_RANGE),
    ] {
        put(&format!("gate.{k}"), if vm.is_armed(l) { "Disable" } else { "Enable" }.into());
    }
}

fn sliders(o: &mut Out, vm: &MainViewModel) {
    for (name, f) in [
        ("voltage_boost", Field::VoltageBoost),
        ("voltage_offset", Field::VoltageOffset),
        ("core_offset", Field::CoreOffset),
        ("xbar_offset", Field::XbarOffset),
        ("sys_offset", Field::SysOffset),
        ("video_offset", Field::VideoOffset),
        ("memory_offset", Field::MemoryOffset),
        ("power_limit", Field::PowerLimit),
        ("temp_limit", Field::TempLimit),
        ("voltage_rail", Field::VoltageRailMax),
        ("msvdd_rail", Field::MsvddRailMax),
        ("nvvdd_ocp_amps", Field::NvvddOcpAmps),
        ("msvdd_ocp_amps", Field::MsvddOcpAmps),
        ("clock_lock", Field::ClockLockMin),
    ] {
        let (min, max) = vm.slider_range(f);
        o.put(format!("slider.{name}.min"), min);
        o.put(format!("slider.{name}.max"), max);
        o.put(format!("slider.{name}.step"), vm.step(f));
    }
    for (name, f) in [
        ("voltage_boost", Field::VoltageBoost),
        ("voltage_offset", Field::VoltageOffset),
        ("core_offset", Field::CoreOffset),
        ("xbar_offset", Field::XbarOffset),
        ("sys_offset", Field::SysOffset),
        ("video_offset", Field::VideoOffset),
        ("memory_offset", Field::MemoryOffset),
        ("power_limit", Field::PowerLimit),
        ("temp_limit", Field::TempLimit),
        ("voltage_rail_floor", Field::VoltageRailFloor),
        ("voltage_rail_max", Field::VoltageRailMax),
        ("msvdd_rail_floor", Field::MsvddRailFloor),
        ("msvdd_rail_max", Field::MsvddRailMax),
        ("nvvdd_ocp_amps", Field::NvvddOcpAmps),
        ("msvdd_ocp_amps", Field::MsvddOcpAmps),
        ("clock_lock_min", Field::ClockLockMin),
        ("clock_lock_max", Field::ClockLockMax),
    ] {
        o.put(format!("tooltip.{name}"), vm.range_text(f));
    }
}

fn curve_window(o: &mut Out, svc: &TuningService, vm: &MainViewModel, pts: &[roch_core::VfCurveSample]) {
    let c = &vm.caps;
    // As `CurveWindow::ensure_started` configures its editor.
    let mut e = VfEditor::default();
    e.step_mhz = c.vf_curve_step_mhz.max(1);
    e.freq_min_mhz = c.vf_curve_min_mhz;
    e.freq_max_mhz = c.vf_curve_max_mhz;
    if c.can_edit_vf_curve {
        e.full_frequency_range = true;
        e.volt_min_mv = c.min_voltage_mv;
        e.volt_max_mv = c.max_voltage_mv;
    }
    o.put("curvewin.step_mhz", e.step_mhz);
    o.put("curvewin.freq_min_mhz", e.freq_min_mhz);
    o.put("curvewin.freq_max_mhz", e.freq_max_mhz);
    o.put("curvewin.full_range", e.full_frequency_range);
    if c.can_edit_vf_curve {
        o.put("curvewin.volt_min_mv", e.volt_min_mv);
        o.put("curvewin.volt_max_mv", e.volt_max_mv);
    }
    o.put("curvewin.flatten_box", "1000");
    // As `CurveWindow::flatten_clicked` bounds the box.
    let lo = if c.min_voltage_mv > 0 { c.min_voltage_mv } else { 600 };
    let boost = vm.boost_ceiling_mv();
    let hi = if c.can_edit_vf_curve {
        c.max_voltage_mv
    } else if boost > 0 {
        boost
    } else {
        1200
    };
    o.put("curvewin.flatten_lo", lo);
    o.put("curvewin.flatten_hi", hi);
    let cap = svc.read_voltage_lock_mv().max(0);
    let ceiling = curve::audit_ceiling_mv(svc);
    o.put("curvewin.cap_mv", cap);
    o.put("curvewin.ceiling_mv", ceiling);
    e.set_points(pts);
    e.set_voltage_cap(cap);
    e.set_voltage_ceiling(ceiling);
    match e.loaded_status() {
        Some(s) => o.put("curvewin.loaded", s),
        None => o.put("curvewin.loaded", "This card/driver did not return a V/F curve — nothing to edit."),
    }
    if let Some(s) = e.edit_status() {
        o.put("curvewin.edit_status", s);
    }
    let a = e.axes();
    o.put("curvewin.axes", format!("{} {} {} {}", a.v_min, a.v_max, a.f_min, a.f_max));
    o.put("curvewin.flatten_1000", e.flatten_from(1000));
    for (i, p) in e.points().iter().enumerate() {
        o.put(
            format!("curvewin.flattened.{i:03}"),
            format!("{} {} {} {}", p.index, p.voltage_mv, p.stock_mhz, p.live_mhz),
        );
    }
    if let Some(s) = e.edit_status() {
        o.put("curvewin.flattened_status", s);
    }
}

fn table(o: &mut Out, prefix: &str, t: &TelemetryTable, layout_only: bool) {
    o.put(format!("{prefix}.count"), t.rows().len());
    for (i, r) in t.rows().iter().enumerate() {
        let kind = if r.is_header { "H" } else { "S" };
        let v = if layout_only {
            format!("{kind} {} | banded={}", r.name, r.is_banded)
        } else {
            format!(
                "{kind} {} | {} | {} | {} | {} | banded={}",
                r.name, r.current, r.minimum, r.maximum, r.average, r.is_banded
            )
        };
        o.put(format!("{prefix}.{i:03}"), v);
    }
}

fn summaries(o: &mut Out, prefix: &str, t: &GpuTelemetry) {
    let [temp, clock, power] = monitor::audit_summaries(t);
    o.put(format!("{prefix}.temperature"), temp);
    o.put(format!("{prefix}.clock"), clock);
    o.put(format!("{prefix}.power"), power);
}

// ------------------------------------------------------------------ fixed samples (as the C# harness)

fn sup(key: &str, name: &str, unit: &str, v: f64) -> SupplementalSensor {
    SupplementalSensor::new(key, name, unit, v)
}

fn sample_a() -> GpuTelemetry {
    GpuTelemetry {
        core_clock_mhz: 2422.5,
        memory_clock_mhz: 14001.0,
        temperature_c: 45.25,
        hot_spot_c: 52.75,
        memory_temperature_c: 60.0,
        voltage_mv: 1012.5,
        nvvdd_mv: 1003.125,
        msvdd_mv: 950.05,
        power_percent: 37.5,
        power_watts: 110.25,
        rail_amps: vec![25.125, 0.5, 1.0, 1.0, 20.0, 1.0, 0.2],
        rail_volts: vec![12.0; 7],
        gpu_load_percent: 12.5,
        memory_load_percent: 7.0,
        memory_used_mb: 2048.5,
        fan_percent: 30.0,
        fan_rpm: 1000.0,
        fan_percents: vec![30.0, 31.5, 32.0],
        fan_rpms: vec![1000.0, 1010.5, 1020.0],
        perf_state: "P0".into(),
        limit_reason: "Power".into(),
        supplemental_sensors: vec![
            sup("nv:video-load", "Video engine", "%", 0.0),
            sup("nv:pcie-gen", "PCIe link generation", "gen", 5.0),
            sup("nv:pcie-speed", "PCIe link speed", "GT/s", 32.0),
            sup("nv:limit-power", "Power limit active", "state", 1.0),
            sup("nv:memory-free", "Available VRAM", "MB", 14000.5),
            sup("x:volt", "Some voltage", "V", 1.3525),
            sup("x:amp", "Some current", "A", 3.125),
            sup("x:bw", "Read bandwidth", "GB/s", 12.5),
            sup("x:watts", "GPU power", "W", 80.05),
            sup("x:other", "Something", "", 2.5),
        ],
        ..GpuTelemetry::default()
    }
}

fn sample_b() -> GpuTelemetry {
    GpuTelemetry {
        core_clock_mhz: 2437.5,
        memory_clock_mhz: 14001.0,
        temperature_c: 47.75,
        hot_spot_c: f64::NAN,
        memory_temperature_c: 61.5,
        voltage_mv: 1017.5,
        nvvdd_mv: 1008.5,
        msvdd_mv: f64::NAN,
        power_percent: 42.5,
        power_watts: 120.75,
        rail_amps: vec![27.5, 0.75, 1.0, 1.0, 22.5, 1.0, 0.25],
        rail_volts: vec![12.0; 7],
        gpu_load_percent: 99.5,
        memory_load_percent: 40.5,
        memory_used_mb: 4096.0,
        fan_percent: 45.0,
        fan_rpm: 1500.0,
        fan_percents: vec![45.0, 46.0, 47.0],
        fan_rpms: vec![1500.5, 1510.0, 1520.0],
        perf_state: "P0".into(),
        limit_reason: "None".into(),
        supplemental_sensors: vec![
            sup("nv:video-load", "Video engine", "%", 2.5),
            sup("nv:pcie-gen", "PCIe link generation", "gen", 5.0),
            sup("nv:limit-power", "Power limit active", "state", 0.0),
            sup("x:volt", "Some voltage", "V", 1.0005),
            sup("x:new", "Late temperature", "°C", 33.25),
        ],
        ..GpuTelemetry::default()
    }
}

fn extra_a() -> BTreeMap<String, f64> {
    [
        ("coremeasured", 2422.5),
        ("xbar", 2100.0),
        ("sys", 1850.5),
        ("video", 1650.0),
        ("domain3", 1000.5),
        ("domain20", 2227.5),
        ("domain22", 27.0),
        ("domain31", 0.0),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

fn extra_b() -> BTreeMap<String, f64> {
    [
        ("coremeasured", 2437.5),
        ("xbar", 2115.0),
        ("sys", 1850.0),
        ("video", 1650.5),
        ("domain3", 1000.0),
        ("domain20", 2242.5),
        ("domain22", 27.0),
        ("domain31", 0.0),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

// ------------------------------------------------------------------ the logon task, read-only

/// The real `schtasks /Query`, and nothing else.
struct ReadOnlyStartup;

impl StartupTasks for ReadOnlyStartup {
    fn exists(&self) -> bool {
        startup_task::exists()
    }
    fn register(&self, _exe_path: &str, _profile_name: &str) -> roch_core::Result<()> {
        panic!("audit: the logon task must not be registered");
    }
    fn unregister(&self) -> roch_core::Result<()> {
        panic!("audit: the logon task must not be removed");
    }
}

fn untime(s: &str) -> String {
    // "HH:MM:SS" in place of a clock time, as the C# harness's Untime.
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if i + 8 <= b.len()
            && b[i..i + 8]
                .iter()
                .enumerate()
                .all(|(k, c)| if k == 2 || k == 5 { *c == b':' } else { c.is_ascii_digit() })
        {
            out.push_str("HH:MM:SS");
            i += 8;
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn mock_actions(o: &mut Out, svc: &TuningService, vm: &mut MainViewModel) {
    vm.set(Field::CoreOffset, 45);
    vm.set(Field::MemoryOffset, 100);
    vm.set(Field::PowerLimit, 105);
    vm.set(Field::VoltageBoost, 25);
    vm.set(Field::XbarOffset, 30);
    o.put("act.edit.pending", vm.pending_changes());
    o.put("act.edit.core_offset_text", vm.core_offset_text());
    o.put("act.edit.voltage_cap_text", vm.voltage_cap_text());
    vm.apply();
    vm.run_jobs_inline();
    o.put("act.apply.status", untime(&vm.status));
    o.put("act.apply.status_is_error", vm.status_is_error);
    o.put("act.apply.summary", vm.applied_summary().to_string());
    o.put("act.apply.xoc_status", vm.xoc_status_text());
    o.put("act.apply.pending", vm.pending_changes());
    state(o, "act.apply.state", &svc.read_tuning_state().unwrap());
    vm.toggle_lever(XocLever::NVVDD);
    vm.run_jobs_inline();
    o.put("act.lever_on.status", vm.status.clone());
    o.put("act.lever_on.xoc_status", vm.xoc_status_text());
    o.put("act.lever_on.summary", vm.applied_summary().to_string());
    o.put("act.lever_on.armed_on_card", svc.armed_on_card());
    vm.set(Field::VoltageRailMax, vm.get(Field::VoltageRailMax) + 20);
    o.put("act.lever_edit.rail_text", vm.voltage_rail_max_text());
    o.put("act.lever_edit.xoc_status", vm.xoc_status_text());
    vm.toggle_lever(XocLever::NVVDD);
    vm.run_jobs_inline();
    o.put("act.lever_off.status", vm.status.clone());
    o.put("act.lever_off.xoc_status", vm.xoc_status_text());
    o.put("act.lever_off.rail", vm.get(Field::VoltageRailMax));
    vm.set(Field::TargetVoltage, 1000);
    o.put("act.cap.voltage_cap_text", vm.voltage_cap_text());
    vm.apply();
    vm.run_jobs_inline();
    o.put("act.cap.summary", vm.applied_summary().to_string());
    o.put("act.cap.voltage_cap_text", vm.voltage_cap_text());
    o.put("act.cap.mechanism", svc.voltage_lock_mechanism());
    vm.set_save_armed(true);
    o.put("act.save_armed.status", vm.status.clone());
    vm.on_slot_clicked(3);
    vm.run_jobs_inline();
    o.put("act.saved.status", vm.status.clone());
    let s3 = vm.slots()[2].clone();
    o.put("act.saved.slot3.number", s3.number);
    o.put("act.saved.slot3.name", s3.name());
    o.put("act.saved.slot3.label", s3.label());
    o.put("act.saved.slot3.occupied", s3.occupied);
    o.put("act.saved.slot3.is_active", s3.is_active);
    o.put("act.saved.slot3.summary", s3.summary.clone());
    o.put("act.saved.slot3.tip", s3.tip());
    vm.on_slot_clicked(4);
    vm.run_jobs_inline();
    o.put("act.empty_slot.status", vm.status.clone());
    o.put("act.empty_slot.is_error", vm.status_is_error);
    vm.reset();
    vm.run_jobs_inline();
    o.put("act.reset.status", vm.status.clone());
    o.put("act.reset.summary", vm.applied_summary().to_string());
    o.put("act.reset.xoc_status", vm.xoc_status_text());
    profile(o, "act.reset.vm_build", &vm.build_profile_from_editor("Audit"));
    vm.on_slot_clicked(3);
    vm.run_jobs_inline();
    o.put("act.slot3.status", untime(&vm.status));
    o.put("act.slot3.summary", vm.applied_summary().to_string());
    o.put("act.slot3.active", vm.active_slot_name());
}

/// The C# harness's `FmtProbes`, in the same order.
const FMT_PROBES: [f64; 38] = [
    0.0,
    0.04,
    -0.04,
    0.05,
    -0.05,
    0.15,
    0.25,
    0.35,
    0.45,
    0.5,
    -0.5,
    1.5,
    2.5,
    -2.5,
    1.005,
    2.675,
    45.25,
    110.25,
    2422.5,
    1010.5,
    12.125,
    15.9208984375,
    11.994140625,
    1e-7,
    123456789.125,
    0.95,
    99.95,
    9.995,
    1003.125,
    950.05,
    1.3525,
    25.125,
    3.125,
    1e15 + 0.5,
    0.000123456,
    33.25,
    30.5,
    1234567.891,
];

/// .NET number-format probes: custom formats through `dotnet_custom`, `F<n>` through `{:.n}`.
#[test]
#[ignore = "audit harness: needs ROCH_AUDIT_OUT"]
fn audit_fmt() {
    use roch_core::util::dotnet_custom;
    let out_file = std::env::var("ROCH_AUDIT_OUT").expect("ROCH_AUDIT_OUT");
    let mut text = String::new();
    for (i, &v) in FMT_PROBES.iter().enumerate() {
        for (f, s) in [
            ("0", dotnet_custom(v, 0, 0)),
            ("0.0", dotnet_custom(v, 1, 1)),
            ("0.#", dotnet_custom(v, 0, 1)),
            ("0.##", dotnet_custom(v, 0, 2)),
            ("F0", format!("{v:.0}")),
            ("F1", format!("{v:.1}")),
            ("F2", format!("{v:.2}")),
            ("F3", format!("{v:.3}")),
        ] {
            let _ = writeln!(text, "fmt.{i:02}.{f} = {s}");
        }
    }
    std::fs::write(out_file, text).unwrap();
}

/// Intel- and AMD-style capabilities on the mock (as the C# App tests do), to cover the labels,
/// ranges and steps no NVIDIA card shows.
fn variants(o: &mut Out, shared: &Shared) {
    let base = shared.service.capabilities();
    let intel = GpuCapabilities {
        voltage_style: VoltageControlStyle::Percent,
        core_offset_step_mhz: 1,
        core_offset_min_mhz: -300,
        core_offset_max_mhz: 500,
        memory_clock_is_absolute: true,
        memory_clock_unit: "Mbps".into(),
        memory_offset_min_mhz: 19000,
        memory_offset_max_mhz: 22000,
        memory_clock_default_mhz: 19000,
        temp_limit_min_c: 60,
        temp_limit_max_c: 100,
        temp_limit_default_c: 90,
        power_limit_max_percent: 120,
        can_edit_vf_curve: true,
        fan_curve_is_hardware: true,
        fan_curve_points: 10,
        can_set_voltage_rail: false,
        can_set_msvdd_rail: false,
        can_set_ocp: false,
        can_set_xbar_offset: false,
        can_set_sys_offset: false,
        can_set_video_offset: false,
        fan_count: 1,
        ..base.clone()
    };
    let intel_load = TuningProfile {
        core_offset_mhz: 91,
        memory_offset_mhz: 19001,
        voltage_boost_percent: 41,
        power_limit_percent: 101,
        temp_limit_c: 99,
        xoc_armed: XocLever::CLOCK_RANGE,
        clock_lock_min_mhz: 500,
        clock_lock_max_mhz: 2300,
        fan_mode: FanMode::Curve,
        ..TuningProfile::default()
    };
    let amd = GpuCapabilities {
        voltage_style: VoltageControlStyle::Offset,
        memory_clock_is_absolute: true,
        memory_offset_min_mhz: 2400,
        memory_offset_max_mhz: 2800,
        memory_clock_default_mhz: 2518,
        power_limit_is_offset: true,
        power_limit_min_percent: -10,
        power_limit_max_percent: 15,
        power_limit_default_percent: 0,
        can_set_zero_rpm: true,
        can_set_memory_timing: true,
        memory_timing_options: vec!["Default".into(), "Fast Timing".into(), "Fast Timing Level 2".into()],
        can_set_voltage_rail: false,
        can_set_msvdd_rail: false,
        can_set_ocp: false,
        can_set_xbar_offset: false,
        can_set_sys_offset: false,
        can_set_video_offset: false,
        can_lock_clocks: false,
        fan_curve_is_hardware: true,
        fan_curve_points: 5,
        can_read_hardware_fan_curve: true,
        voltage_offset_min_mv: -200,
        can_set_temp_limit: false,
        can_set_voltage_boost: false,
        fan_count: 2,
        ..base
    };
    let amd_load = TuningProfile {
        core_offset_mhz: -45,
        memory_offset_mhz: 2518,
        power_limit_percent: 5,
        voltage_offset_mv: -50,
        zero_rpm: false,
        memory_timing_level: 1,
        fan_mode: FanMode::Fixed,
        fixed_fan_percent: 47,
        fixed_fan_percents: vec![40, 55],
        ..TuningProfile::default()
    };
    // Keys the C# variant block leaves out (they describe the session, not the capabilities).
    let skip = |l: &str| {
        let k = l.split(" = ").next().unwrap_or("");
        let k = k.split_once('.').map_or(k, |x| x.1).split_once('.').map_or("", |x| x.1);
        k.starts_with("vm.slot.")
            || [
                "vm.profiles",
                "vm.title_text",
                "vm.pending_changes",
                "vm.status",
                "vm.status_is_error",
                "vm.applied_summary",
            ]
            .contains(&k)
            || (k.starts_with("vm.gate.") && k != "vm.gate.clock_range")
    };
    for (name, caps, load) in [("intel", intel, intel_load), ("amd", amd, amd_load)] {
        o.1 = format!("var.{name}.");
        let start = o.0.len();
        let mut vm = MainViewModel::new(shared.clone());
        vm.caps = caps;
        vm.load_into_editor(&load);
        view_model(o, &vm);
        profile(o, "vm.build", &vm.build_profile_from_editor("Audit"));
        sliders(o, &vm);
        let prefix = o.1.clone();
        for (k, v) in fan::audit_initial(&vm, &sample_a()) {
            o.0.push(format!("{prefix}{k} = {}", v.norm()));
        }
        for key in ["core", "mem", "voltboost", "voltoffset", "power", "temp"] {
            vm.nudge_spec(&format!("{key}:1"));
            vm.nudge_spec(&format!("{key}:1"));
        }
        profile(o, "nudged", &vm.build_profile_from_editor("Audit"));
        let mut kept: Vec<String> = o.0.drain(start..).filter(|l| !skip(l)).collect();
        o.0.append(&mut kept);
        o.1.clear();
    }
}

#[test]
#[ignore = "audit harness: needs ROCH_AUDIT_MODE/DIR/OUT; reads the real GPU in real mode"]
fn audit_dump() {
    let mode = std::env::var("ROCH_AUDIT_MODE").expect("ROCH_AUDIT_MODE");
    let dir = std::env::var("ROCH_AUDIT_DIR").expect("ROCH_AUDIT_DIR");
    let out_file = std::env::var("ROCH_AUDIT_OUT").expect("ROCH_AUDIT_OUT");
    let mock = mode == "mock";
    let mut o = Out(Vec::new(), String::new());

    let store = Arc::new(ProfileStore::new(Some(dir.into())).unwrap());
    let settings = store.load_settings();
    let backend = if mock { backends::mock() } else { backends::create_and_initialize().unwrap().0 };
    let svc = Arc::new(TuningService::with_history(backend, settings.history_seconds.max(1) as usize));
    svc.initialize(0).unwrap();
    let shared = Shared::new(svc.clone(), store.clone(), settings, Arc::new(ReadOnlyStartup));

    // The recovery journal, as startup opens it; a tripwire for its apply on real hardware.
    let dv = svc.device();
    let key = format!("{}|{}|{}", dv.vendor, dv.bus_id, dv.name);
    let stock = TuningProfile::stock(&svc.capabilities(), Some(&dv.name));
    let recovery = if mock {
        shared.open_recovery().unwrap()
    } else {
        ProfileRecovery::new(
            store.root_directory(),
            &key,
            &stock,
            Box::new(|_| panic!("audit: apply on real hardware")),
        )
        .unwrap()
    };
    o.put("recovery.has_pending", recovery.has_pending());
    assert!(!recovery.has_pending(), "pending trial in the journal copy - not reverting");
    shared.set_recovery(Some(recovery));

    // ------------------------------------------------------------------ core
    o.put("backend", svc.backend_name());
    let devices = svc.devices();
    o.put("devices.count", devices.len());
    for (i, x) in devices.iter().enumerate() {
        device(&mut o, &format!("device.{i}"), x);
    }
    let c = svc.capabilities();
    caps(&mut o, "caps", &c);
    state(&mut o, "state", &svc.read_tuning_state().unwrap());
    o.put("voltage_lock_mv", svc.read_voltage_lock_mv());
    o.put("voltage_lock_mechanism", svc.voltage_lock_mechanism());
    let gi = svc.read_graphics_info();
    graphics_info(&mut o, "graphics_info", &gi);
    let rows = gi.rows();
    o.put("graphics.count", rows.len());
    for (i, r) in rows.iter().enumerate() {
        o.put(format!("graphics.{i:02}"), format!("{} | {} | {}", r.name, r.value, r.is_banded));
    }
    let pts = svc.read_vf_curve().unwrap_or_default();
    o.put("vf.count", pts.len());
    for (i, p) in pts.iter().enumerate() {
        o.put(format!("vf.{i:03}"), format!("{} {} {} {}", p.index, p.voltage_mv, p.stock_mhz, p.live_mhz));
    }
    let extra = svc.measure_extra_clocks();
    o.put("extra.keys", format!("[{}]", extra.keys().cloned().collect::<Vec<_>>().join(", ")));
    for (k, v) in &extra {
        o.put(format!("live.extra.{k}"), *v);
    }
    let gpu = svc.gpu_index();
    let t = svc.with_backend(|b| b.read_telemetry(gpu)).unwrap();
    telemetry(&mut o, "telemetry", &t);
    ceilings(&mut o, "svc.pre", &svc);
    profile(&mut o, "current", &svc.read_current_as_profile().unwrap());

    // ------------------------------------------------------------------ main view model
    let mut vm = MainViewModel::new(shared.clone());
    assert!(!vm.busy(), "constructing the view model queued a driver job");
    ceilings(&mut o, "svc.post", &svc);
    view_model(&mut o, &vm);
    profile(&mut o, "vm.build", &vm.build_profile_from_editor("Audit"));
    sliders(&mut o, &vm);
    for (k, v) in fan::audit_initial(&vm, &sample_a()) {
        o.put(k, v);
    }
    curve_window(&mut o, &svc, &vm, &pts);
    // The limiter line with a sample (the setter's settings save goes to the temp copy).
    vm.set_telemetry(Some(sample_a()));
    o.put("vm.sample_a.limit_reason_text", vm.limit_reason_text());
    o.put("vm.sample_a.limit_is_active", vm.limit_is_active());
    vm.set_telemetry(Some(sample_b()));
    o.put("vm.sample_b.limit_reason_text", vm.limit_reason_text());
    o.put("vm.sample_b.limit_is_active", vm.limit_is_active());
    vm.set_telemetry(None);

    // ------------------------------------------------------------------ telemetry table
    // The keys as the monitor passes them: a BTreeMap's, in text order.
    let ea = extra_a();
    let mut fixed = TelemetryTable::new(&sample_a(), &c, ea.keys().map(String::as_str));
    fixed.add(&sample_a(), Some(&ea));
    table(&mut o, "table.fixed1", &fixed, false);
    fixed.add(&sample_b(), Some(&extra_b()));
    table(&mut o, "table.fixed2", &fixed, false);
    summaries(&mut o, "monitor.fixed_a", &sample_a());
    summaries(&mut o, "monitor.fixed_b", &sample_b());
    o.put("monitor.fixed2.elapsed_prefix", format!("{} sensors", fixed.sensor_count()));
    let mut live = TelemetryTable::new(&t, &c, extra.keys().map(String::as_str));
    live.add(&t, Some(&extra));
    table(&mut o, "table.live", &live, true);

    if mock {
        mock_actions(&mut o, &svc, &mut vm);
        variants(&mut o, &shared);
    }
    assert!(!vm.busy());

    let mut text = String::new();
    for l in &o.0 {
        let _ = writeln!(text, "{l}");
    }
    std::fs::write(out_file, text).unwrap();
}
