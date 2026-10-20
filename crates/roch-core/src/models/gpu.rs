//! Device identity, capabilities, telemetry and tuning state as reported by a backend.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::fan_curve::FanCurve;
use super::profile::FanMode;

/// One editable point of the voltage/frequency curve, in display units (mV, MHz).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct VfCurveSample {
    pub index: i32,
    pub voltage_mv: i32,
    pub stock_mhz: i32,
    pub live_mhz: i32,
}

impl VfCurveSample {
    pub const fn new(index: i32, voltage_mv: i32, stock_mhz: i32, live_mhz: i32) -> Self {
        Self { index, voltage_mv, stock_mhz, live_mhz }
    }
}

/// Static identity of a GPU as discovered by a backend.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GpuDevice {
    pub index: usize,
    pub name: String,
    pub vendor: String,
    pub bus_id: String,
    pub driver_version: String,
    pub vram_megabytes: i64,
    pub bios_version: String,
}

/// Which shape the core-voltage control takes on this vendor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoltageControlStyle {
    /// No voltage control at all.
    None,
    /// Absolute target in mV (NVIDIA: cap or boost against the V/F curve).
    #[default]
    Absolute,
    /// Signed offset in mV applied to the whole curve (AMD).
    Offset,
    /// Intel's driver-defined voltage limit percentage.
    Percent,
}

/// What the backend allows us to change, plus the driver-imposed ranges.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuCapabilities {
    pub core_offset_step_mhz: i32,
    pub memory_clock_unit: String,
    pub temp_limit_is_percent: bool,
    /// Independent per-point editing without NVIDIA's voltage-cap/flatten path.
    pub can_edit_vf_curve: bool,
    pub can_read_hardware_fan_curve: bool,
    pub vf_curve_step_mhz: i32,
    pub vf_curve_min_mhz: i32,
    pub vf_curve_max_mhz: i32,
    pub can_set_core_offset: bool,
    pub can_set_memory_offset: bool,
    pub can_set_power_limit: bool,
    pub can_set_temp_limit: bool,
    pub can_set_fan_speed: bool,
    pub can_read_voltage: bool,
    pub can_set_voltage_boost: bool,

    /// Core voltage boost range in percent (0 = stock V/F curve).
    pub voltage_boost_min_percent: i32,
    pub voltage_boost_max_percent: i32,
    /// Driver default: NVIDIA boost is normally 0%; Intel voltage-limit defaults differ.
    pub voltage_boost_default_percent: i32,

    /// True when the V/F curve can be edited — the only real undervolt path on NVIDIA.
    pub can_set_voltage_curve: bool,
    /// Negative-only voltage offset range in mV (0 = stock, -150 = cap 150 mV below stock max).
    pub voltage_offset_min_mv: i32,
    pub voltage_offset_max_mv: i32,
    /// Highest voltage on the stock V/F curve, in mV. 0 when unknown.
    pub stock_max_voltage_mv: i32,
    /// Why the V/F curve is unavailable, when `can_set_voltage_curve` is false.
    pub curve_unavailable_reason: String,

    /// Absolute core-voltage window for the single voltage slider, in mV.
    pub min_voltage_mv: i32,
    /// Stock curve top plus full voltage-boost headroom — the highest the card will go.
    pub max_voltage_mv: i32,

    /// Core clock offset range in MHz (driver limits).
    pub core_offset_min_mhz: i32,
    pub core_offset_max_mhz: i32,
    /// Memory clock offset range in MHz (driver limits).
    pub memory_offset_min_mhz: i32,
    pub memory_offset_max_mhz: i32,
    /// Power limit range in percent of TDP.
    pub power_limit_min_percent: i32,
    pub power_limit_max_percent: i32,
    pub power_limit_default_percent: i32,
    /// Temperature limit range in °C (or percent, see `temp_limit_is_percent`).
    pub temp_limit_min_c: i32,
    pub temp_limit_max_c: i32,
    pub temp_limit_default_c: i32,
    /// Fan speed range in percent.
    pub fan_min_percent: i32,
    pub fan_max_percent: i32,
    /// Number of independently controllable fans (0 if fan control unsupported).
    pub fan_count: i32,

    /// The core rail's own voltage ceiling can be moved (distinct from boost and V/F cap).
    pub can_set_voltage_rail: bool,
    pub voltage_rail_min_mv: i32,
    pub voltage_rail_max_mv: i32,
    /// The rail's ceiling with no offset applied, i.e. what "default" restores.
    pub voltage_rail_stock_max_mv: i32,
    pub voltage_rail_floor_min_mv: i32,
    pub voltage_rail_floor_max_mv: i32,
    pub voltage_rail_stock_floor_mv: i32,

    /// MSVDD feeds the crossbar, SYS and video domains — a separate supply from NVVDD.
    pub can_set_msvdd_rail: bool,
    pub msvdd_rail_min_mv: i32,
    pub msvdd_rail_max_mv: i32,
    pub msvdd_rail_stock_max_mv: i32,
    pub msvdd_rail_floor_min_mv: i32,
    pub msvdd_rail_floor_max_mv: i32,
    pub msvdd_rail_stock_floor_mv: i32,

    /// Over-current protection limits, in milliamps. Stock figures are what a disarmed lever
    /// restores, recorded the first time the GPU is seen.
    pub can_set_ocp: bool,
    pub nvvdd_ocp_stock_milliamps: i32,
    pub msvdd_ocp_stock_milliamps: i32,
    /// The window the driver itself offers for each OCP limit, in milliamps; 0 when none.
    pub nvvdd_ocp_min_milliamps: i32,
    pub nvvdd_ocp_max_milliamps: i32,
    pub msvdd_ocp_min_milliamps: i32,
    pub msvdd_ocp_max_milliamps: i32,

    /// SYS and video clocks: same private family as the crossbar.
    pub can_set_sys_offset: bool,
    pub sys_offset_min_mhz: i32,
    pub sys_offset_max_mhz: i32,
    pub can_set_video_offset: bool,
    pub video_offset_min_mhz: i32,
    pub video_offset_max_mhz: i32,

    /// Pinning the graphics clock to a min/max window (NVML locked clocks).
    pub can_lock_clocks: bool,
    pub clock_lock_min_mhz: i32,
    pub clock_lock_max_mhz: i32,

    /// A software temperature limit can be offered: the clock can be capped (`can_lock_clocks`)
    /// and the driver has no settable thermal limit of its own (RTX 50). It acts only while the app
    /// runs; see `services::soft_thermal`.
    pub can_set_soft_thermal_limit: bool,
    /// Its window in °C, `50..=105`. Values below the card's own limit are enforced by the app;
    /// at or above it (see `soft_thermal_card_limit_c`) the value is kept but the card manages
    /// itself, and 105 °C is the top of the selectable range.
    pub soft_thermal_limit_min_c: i32,
    pub soft_thermal_limit_max_c: i32,
    /// The card's own limit (its GPU-max threshold, 88 °C on a 5070 Ti), shown in the tooltip.
    /// Above it the card manages itself and the driver's slowdown still applies. 0 when unknown.
    pub soft_thermal_card_limit_c: i32,

    pub can_set_xbar_offset: bool,
    pub xbar_offset_min_mhz: i32,
    pub xbar_offset_max_mhz: i32,

    // ---------------- vendor differences the UI has to reflect ----------------
    pub voltage_style: VoltageControlStyle,
    /// True when the memory slider is an absolute clock (AMD) rather than an offset (NVIDIA).
    pub memory_clock_is_absolute: bool,
    /// Stock memory clock in MHz, when the slider is absolute.
    pub memory_clock_default_mhz: i32,
    /// True when the power limit is a +/- offset around stock rather than a percentage of TDP.
    pub power_limit_is_offset: bool,
    /// Zero RPM (fan stop at idle) can be turned on and off.
    pub can_set_zero_rpm: bool,
    pub zero_rpm_default: bool,
    /// Memory timing presets are selectable (AMD's "fast timing").
    pub can_set_memory_timing: bool,
    pub memory_timing_options: Vec<String>,
    pub memory_timing_default_level: i32,
    /// The fan curve is executed by the driver rather than this app's polling loop.
    pub fan_curve_is_hardware: bool,
    pub fan_curve_points: i32,
    pub fan_curve_min_temp_c: i32,
    pub fan_curve_max_temp_c: i32,
}

impl Default for GpuCapabilities {
    fn default() -> Self {
        Self {
            core_offset_step_mhz: 15,
            memory_clock_unit: "MHz".into(),
            temp_limit_is_percent: false,
            can_edit_vf_curve: false,
            can_read_hardware_fan_curve: false,
            vf_curve_step_mhz: 5,
            vf_curve_min_mhz: 0,
            vf_curve_max_mhz: 10000,
            can_set_core_offset: false,
            can_set_memory_offset: false,
            can_set_power_limit: false,
            can_set_temp_limit: false,
            can_set_fan_speed: false,
            can_read_voltage: false,
            can_set_voltage_boost: false,
            voltage_boost_min_percent: 0,
            voltage_boost_max_percent: 100,
            voltage_boost_default_percent: 0,
            can_set_voltage_curve: false,
            voltage_offset_min_mv: -300,
            voltage_offset_max_mv: 0,
            stock_max_voltage_mv: 0,
            curve_unavailable_reason: String::new(),
            min_voltage_mv: 850,
            max_voltage_mv: 0,
            core_offset_min_mhz: -500,
            core_offset_max_mhz: 500,
            memory_offset_min_mhz: -1000,
            memory_offset_max_mhz: 1500,
            power_limit_min_percent: 100,
            power_limit_max_percent: 100,
            power_limit_default_percent: 100,
            temp_limit_min_c: 65,
            temp_limit_max_c: 90,
            temp_limit_default_c: 83,
            fan_min_percent: 0,
            fan_max_percent: 100,
            fan_count: 0,
            can_set_voltage_rail: false,
            voltage_rail_min_mv: 0,
            voltage_rail_max_mv: 0,
            voltage_rail_stock_max_mv: 0,
            voltage_rail_floor_min_mv: 0,
            voltage_rail_floor_max_mv: 0,
            voltage_rail_stock_floor_mv: 0,
            can_set_msvdd_rail: false,
            msvdd_rail_min_mv: 0,
            msvdd_rail_max_mv: 0,
            msvdd_rail_stock_max_mv: 0,
            msvdd_rail_floor_min_mv: 0,
            msvdd_rail_floor_max_mv: 0,
            msvdd_rail_stock_floor_mv: 0,
            can_set_ocp: false,
            nvvdd_ocp_stock_milliamps: 0,
            msvdd_ocp_stock_milliamps: 0,
            nvvdd_ocp_min_milliamps: 0,
            nvvdd_ocp_max_milliamps: 0,
            msvdd_ocp_min_milliamps: 0,
            msvdd_ocp_max_milliamps: 0,
            can_set_sys_offset: false,
            sys_offset_min_mhz: 0,
            sys_offset_max_mhz: 0,
            can_set_video_offset: false,
            video_offset_min_mhz: 0,
            video_offset_max_mhz: 0,
            can_lock_clocks: false,
            clock_lock_min_mhz: 0,
            clock_lock_max_mhz: 0,
            can_set_soft_thermal_limit: false,
            soft_thermal_limit_min_c: SOFT_THERMAL_MIN_C,
            soft_thermal_limit_max_c: SOFT_THERMAL_MAX_C,
            soft_thermal_card_limit_c: 0,
            can_set_xbar_offset: false,
            xbar_offset_min_mhz: 0,
            xbar_offset_max_mhz: 0,
            voltage_style: VoltageControlStyle::Absolute,
            memory_clock_is_absolute: false,
            memory_clock_default_mhz: 0,
            power_limit_is_offset: false,
            can_set_zero_rpm: false,
            zero_rpm_default: true,
            can_set_memory_timing: false,
            memory_timing_options: Vec::new(),
            memory_timing_default_level: 0,
            fan_curve_is_hardware: false,
            fan_curve_points: 0,
            fan_curve_min_temp_c: 0,
            fan_curve_max_temp_c: 110,
        }
    }
}

/// The window a software temperature limit is offered in, in °C. The top is 105 °C; at or above
/// the card's own limit (see [`soft_thermal_card_limit`]) the card manages itself, so the app caps
/// nothing.
pub const SOFT_THERMAL_MIN_C: i32 = 50;
pub const SOFT_THERMAL_MAX_C: i32 = 105;

/// The temperature where the card already manages itself: its reported GPU-max operating
/// temperature (88 °C on a 5070 Ti), else one below its hardware slowdown. Every reported value is
/// used as-is, capped only at the slider's 105 °C top, so cards with different limits get different
/// defaults. When the driver reports no threshold at all the top is used, which means "leave it to
/// the card": no limit is invented for it. The setting may go above the card's limit, but the
/// card's own slowdown is never bypassed from user mode.
pub fn soft_thermal_card_limit(gpu_max_c: Option<i32>, slowdown_c: Option<i32>) -> i32 {
    let below_slowdown = slowdown_c.filter(|s| *s > 0).map(|s| s - 1);
    let card = match gpu_max_c.filter(|g| *g > 0) {
        Some(g) => g,
        None => below_slowdown.unwrap_or(SOFT_THERMAL_MAX_C),
    };
    let card = below_slowdown.map_or(card, |below| card.min(below));
    card.min(SOFT_THERMAL_MAX_C)
}

/// The software temperature limit's selectable window. The whole `50..=105` range is offered
/// whenever the card has room to hold a temperature of its own; [`soft_thermal_card_limit`]
/// reports where the card takes over, and values from there up leave it to the card. `None` when
/// the card reports no room at all.
pub fn soft_thermal_window(gpu_max_c: Option<i32>, slowdown_c: Option<i32>) -> Option<(i32, i32)> {
    (soft_thermal_card_limit(gpu_max_c, slowdown_c) > SOFT_THERMAL_MIN_C)
        .then_some((SOFT_THERMAL_MIN_C, SOFT_THERMAL_MAX_C))
}

impl GpuCapabilities {
    /// The window a software limit is clamped into: the card's own, or the generic one.
    pub fn soft_thermal_limit_range(&self) -> (i32, i32) {
        let (lo, hi) = (self.soft_thermal_limit_min_c, self.soft_thermal_limit_max_c);
        if self.can_set_soft_thermal_limit && lo > 0 && hi >= lo {
            (lo, hi)
        } else {
            (SOFT_THERMAL_MIN_C, SOFT_THERMAL_MAX_C)
        }
    }

    /// The software limit's default/off value: the card's own reported limit, or the top of its
    /// window when the backend did not report one. At or above it the card manages itself, so the
    /// controller is not armed.
    pub fn soft_thermal_off_c(&self) -> i32 {
        if self.soft_thermal_card_limit_c > 0 {
            self.soft_thermal_card_limit_c
        } else {
            self.soft_thermal_limit_range().1
        }
    }

    pub fn temp_limit_unit(&self) -> &'static str {
        if self.temp_limit_is_percent { "%" } else { "°C" }
    }

    /// True when either lever (boost up, curve flatten down) is available.
    pub fn can_set_voltage(&self) -> bool {
        (self.can_set_voltage_curve || self.can_set_voltage_boost) && self.stock_max_voltage_mv > 0
    }
}

/// A supported native sensor reading; missing readings are omitted from each sample.
#[derive(Debug, Clone, PartialEq)]
pub struct SupplementalSensor {
    pub key: String,
    pub name: String,
    pub unit: String,
    pub value: f64,
}

impl SupplementalSensor {
    pub fn new(key: impl Into<String>, name: impl Into<String>, unit: impl Into<String>, value: f64) -> Self {
        Self { key: key.into(), name: name.into(), unit: unit.into(), value }
    }
}

/// Instantaneous sensor readout. NaN marks a reading the card does not expose.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuTelemetry {
    pub timestamp: DateTime<Utc>,
    pub core_clock_mhz: f64,
    pub memory_clock_mhz: f64,
    /// Effective memory data rate per pin, in Mbps, for cards whose memory control is a speed
    /// (Intel). Shown in place of the memory clock so it matches the editor. NaN when unavailable.
    pub memory_speed_mbps: f64,
    /// NaN when unavailable.
    pub fabric_clock_mhz: f64,
    /// NaN when unavailable.
    pub soc_clock_mhz: f64,
    pub supplemental_sensors: Vec<SupplementalSensor>,
    pub temperature_c: f64,
    /// NaN when unavailable (backends set it explicitly).
    pub hot_spot_c: f64,
    /// NaN when unavailable (backends set it explicitly).
    pub memory_temperature_c: f64,
    /// NaN when unavailable (backends set it explicitly).
    pub voltage_mv: f64,
    /// Measured rail voltages in mV; NaN when the card exposes no ADC for them.
    pub nvvdd_mv: f64,
    pub msvdd_mv: f64,
    /// % of TDP (0 when the vendor reports watts instead).
    pub power_percent: f64,
    /// Board power draw; 0 when unavailable.
    pub power_watts: f64,
    /// Measured 12 V rail currents, in amps. Index 0 is the board total; the rest are rails.
    pub rail_amps: Vec<f64>,
    pub rail_volts: Vec<f64>,
    pub gpu_load_percent: f64,
    pub memory_load_percent: f64,
    pub memory_used_mb: f64,
    pub fan_percent: f64,
    pub fan_rpm: f64,
    pub fan_percents: Vec<f64>,
    pub fan_rpms: Vec<f64>,
    pub perf_state: String,
    /// Power / Thermal / Voltage / None.
    pub limit_reason: String,
}

impl Default for GpuTelemetry {
    /// The C# record's defaults: zeros except the memory speed, fabric/SoC clocks and rail
    /// voltages, which are NaN until a backend reads them.
    fn default() -> Self {
        Self {
            timestamp: Utc::now(),
            core_clock_mhz: 0.0,
            memory_clock_mhz: 0.0,
            memory_speed_mbps: f64::NAN,
            fabric_clock_mhz: f64::NAN,
            soc_clock_mhz: f64::NAN,
            supplemental_sensors: Vec::new(),
            temperature_c: 0.0,
            hot_spot_c: 0.0,
            memory_temperature_c: 0.0,
            voltage_mv: 0.0,
            nvvdd_mv: f64::NAN,
            msvdd_mv: f64::NAN,
            power_percent: 0.0,
            power_watts: 0.0,
            rail_amps: Vec::new(),
            rail_volts: Vec::new(),
            gpu_load_percent: 0.0,
            memory_load_percent: 0.0,
            memory_used_mb: 0.0,
            fan_percent: 0.0,
            fan_rpm: 0.0,
            fan_percents: Vec::new(),
            fan_rpms: Vec::new(),
            perf_state: String::new(),
            limit_reason: String::new(),
        }
    }
}

/// Current applied tuning as read back from the driver.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuTuningState {
    pub hardware_fan_curve: Option<FanCurve>,
    pub core_offset_mhz: i32,
    pub memory_offset_mhz: i32,
    pub power_limit_percent: i32,
    pub temp_limit_c: i32,
    pub voltage_boost_percent: i32,
    /// Inferred from the live curve: how far below stock max the curve is flattened. 0 = not.
    pub voltage_offset_mv: i32,
    /// Current ceiling of the core voltage rail in mV; 0 when the card doesn't expose it.
    pub voltage_rail_max_mv: i32,
    /// Current ceiling of the MSVDD rail in mV; 0 when the card doesn't expose it.
    pub msvdd_rail_max_mv: i32,
    /// Current floors of the two rails in mV; 0 when unavailable.
    pub voltage_rail_floor_mv: i32,
    pub msvdd_rail_floor_mv: i32,
    /// Crossbar clock offset currently applied, in MHz.
    pub xbar_offset_mhz: i32,
    /// OCP current limits actually on the card, in milliamps; 0 when unreadable.
    pub nvvdd_ocp_milliamps: i32,
    pub msvdd_ocp_milliamps: i32,
    pub sys_offset_mhz: i32,
    pub video_offset_mhz: i32,
    /// Clock lock currently applied, in MHz; 0/0 when the clock is unpinned.
    pub locked_clock_min_mhz: i32,
    pub locked_clock_max_mhz: i32,
    pub fan_manual: bool,
    pub fan_percent: i32,
    /// Fan mode as read back from hardware, for a backend that can tell the three apart.
    pub detected_fan_mode: Option<FanMode>,
    /// AMD: fans stop entirely below the driver's idle threshold.
    pub zero_rpm: bool,
    /// AMD: index into `GpuCapabilities::memory_timing_options`.
    pub memory_timing_level: i32,
}

impl Default for GpuTuningState {
    fn default() -> Self {
        Self {
            hardware_fan_curve: None,
            core_offset_mhz: 0,
            memory_offset_mhz: 0,
            power_limit_percent: 100,
            temp_limit_c: 83,
            voltage_boost_percent: 0,
            voltage_offset_mv: 0,
            voltage_rail_max_mv: 0,
            msvdd_rail_max_mv: 0,
            voltage_rail_floor_mv: 0,
            msvdd_rail_floor_mv: 0,
            xbar_offset_mhz: 0,
            nvvdd_ocp_milliamps: 0,
            msvdd_ocp_milliamps: 0,
            sys_offset_mhz: 0,
            video_offset_mhz: 0,
            locked_clock_min_mhz: 0,
            locked_clock_max_mhz: 0,
            fan_manual: false,
            fan_percent: 0,
            detected_fan_mode: None,
            zero_rpm: true,
            memory_timing_level: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_thermal_window_offers_the_105_ceiling() {
        assert_eq!(soft_thermal_window(Some(88), Some(90)), Some((50, 105)), "RTX 5070 Ti: the 105 °C top");
        assert_eq!(soft_thermal_card_limit(Some(88), Some(90)), 88, "its own limit is still 88 °C");
        assert_eq!(soft_thermal_window(None, Some(90)), Some((50, 105)), "no GPU max: one below the slowdown");
        assert_eq!(soft_thermal_card_limit(None, Some(90)), 89);
        assert_eq!(soft_thermal_card_limit(Some(92), Some(90)), 89, "never at or above the slowdown");
        assert_eq!(soft_thermal_card_limit(None, Some(105)), 104);
        assert_eq!(soft_thermal_card_limit(None, None), SOFT_THERMAL_MAX_C, "no reading: leave it to the card");
        assert_eq!(soft_thermal_window(Some(0), Some(0)), Some((50, 105)), "0 is no reading");
        assert_eq!(soft_thermal_window(None, Some(50)), None, "no room left");
    }

    #[test]
    fn soft_thermal_card_limit_is_per_gpu() {
        // Reported thresholds win, up to the slider's top; with none, the top is the off point.
        for (gpu_max, slowdown, want) in [
            (Some(85), Some(90), 85),
            (Some(88), Some(90), 88),
            (Some(93), Some(95), 93),
            (Some(100), Some(105), 100),
            (Some(110), Some(115), SOFT_THERMAL_MAX_C),
            (Some(88), None, 88),
            (Some(100), None, 100),
            (None, Some(90), 89),
            (None, Some(105), 104),
            (None, None, SOFT_THERMAL_MAX_C),
        ] {
            assert_eq!(soft_thermal_card_limit(gpu_max, slowdown), want, "GPU max {gpu_max:?}, slowdown {slowdown:?}");
        }
    }

    #[test]
    fn soft_thermal_off_follows_the_card_or_the_window_top() {
        let card = GpuCapabilities {
            can_set_soft_thermal_limit: true,
            soft_thermal_limit_min_c: 50,
            soft_thermal_limit_max_c: 105,
            soft_thermal_card_limit_c: 88,
            ..Default::default()
        };
        assert_eq!(card.soft_thermal_off_c(), 88, "a reported limit is the off point");
        let unknown = GpuCapabilities { soft_thermal_card_limit_c: 0, ..card.clone() };
        assert_eq!(unknown.soft_thermal_off_c(), 105, "with no reported limit the top is off");
    }

    #[test]
    fn unavailable_fabric_sensor_is_not_zero() {
        assert!(GpuTelemetry::default().fabric_clock_mhz.is_nan());
    }
}
