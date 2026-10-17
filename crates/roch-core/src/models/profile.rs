//! A saved set of tuning values, serialized to JSON in the same shape the C# build wrote, so
//! existing profiles under %AppData%\Roch GPU keep loading.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

use super::fan_curve::FanCurve;
use super::gpu::GpuCapabilities;
use super::xoc_lever::XocLever;
use crate::util::clamp_i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FanMode {
    #[default]
    Auto,
    Fixed,
    Curve,
}

impl FanMode {
    pub const ALL: [FanMode; 3] = [FanMode::Auto, FanMode::Fixed, FanMode::Curve];

    pub fn name(self) -> &'static str {
        match self {
            FanMode::Auto => "Auto",
            FanMode::Fixed => "Fixed",
            FanMode::Curve => "Curve",
        }
    }

    pub fn as_number(self) -> i32 {
        self as i32
    }

    pub fn from_number(n: i64) -> Option<FanMode> {
        match n {
            0 => Some(FanMode::Auto),
            1 => Some(FanMode::Fixed),
            2 => Some(FanMode::Curve),
            _ => None,
        }
    }

    /// Case-insensitive, as .NET's `Enum.Parse(.., ignoreCase: true)`.
    pub fn parse(text: &str) -> Option<FanMode> {
        let t = text.trim();
        if let Ok(n) = t.parse::<i64>() {
            return Self::from_number(n);
        }
        Self::ALL.into_iter().find(|m| m.name().eq_ignore_ascii_case(t))
    }
}

impl fmt::Display for FanMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl Serialize for FanMode {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for FanMode {
    /// Accepts the string form (profile files) and the number form (recovery journals, which the
    /// C# build wrote without a string-enum converter).
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = FanMode;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a fan mode name or number")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<FanMode, E> {
                FanMode::from_number(v as i64).ok_or_else(|| E::custom(format!("unknown fan mode {v}")))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<FanMode, E> {
                FanMode::from_number(v).ok_or_else(|| E::custom(format!("unknown fan mode {v}")))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<FanMode, E> {
                FanMode::parse(v).ok_or_else(|| E::custom(format!("unknown fan mode '{v}'")))
            }
        }
        d.deserialize_any(V)
    }
}

/// `DateTime` (de)serialization compatible with System.Text.Json: written as
/// `2026-10-16T12:34:56.1234567Z`, read from RFC 3339 or an unzoned timestamp taken as UTC.
pub mod dotnet_datetime {
    use super::*;

    pub fn format(dt: &DateTime<Utc>) -> String {
        use chrono::Timelike;
        // .NET ticks are 100 ns, so seven fractional digits; chrono has no %.7f.
        let ticks = dt.nanosecond() % 1_000_000_000 / 100;
        format!("{}.{ticks:07}Z", dt.format("%Y-%m-%dT%H:%M:%S"))
    }

    pub fn parse(s: &str) -> Option<DateTime<Utc>> {
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return Some(dt.with_timezone(&Utc));
        }
        for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"] {
            if let Ok(n) = NaiveDateTime::parse_from_str(s, fmt) {
                return Some(n.and_utc());
            }
        }
        None
    }

    pub fn serialize<S: Serializer>(dt: &DateTime<Utc>, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&format(dt))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<DateTime<Utc>, D::Error> {
        let s = String::deserialize(d)?;
        parse(&s).ok_or_else(|| de::Error::custom(format!("invalid date '{s}'")))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct TuningProfile {
    pub name: String,
    pub version: i32,
    /// GPU this profile was created for (informational; used to warn on mismatch).
    pub gpu_name: Option<String>,

    pub core_offset_mhz: i32,
    pub memory_offset_mhz: i32,
    pub power_limit_percent: i32,
    pub temp_limit_c: i32,
    pub voltage_boost_percent: i32,
    /// Negative = undervolt. Caps the V/F curve this many mV below the stock maximum.
    pub voltage_offset_mv: i32,
    /// Target ceiling for the core voltage rail, in mV. 0 = leave the rail at its default.
    pub voltage_rail_max_mv: i32,
    /// Target ceiling for the MSVDD rail, in mV. 0 = leave it at its default.
    pub msvdd_rail_max_mv: i32,
    /// Target floors for the two rails, in mV. 0 = leave them at their defaults.
    pub voltage_rail_floor_mv: i32,
    pub msvdd_rail_floor_mv: i32,
    /// Crossbar clock offset in MHz.
    pub xbar_offset_mhz: i32,
    /// OCP current limits in milliamps; 0 means "leave the card's own alone".
    pub nvvdd_ocp_milliamps: i32,
    pub msvdd_ocp_milliamps: i32,
    /// Graphics clock window in MHz, 0/0 for unpinned. Armed by `XocLever::CLOCK_RANGE`.
    pub clock_lock_min_mhz: i32,
    pub clock_lock_max_mhz: i32,
    /// SYS and video clock offsets in MHz. Each armed separately.
    pub sys_offset_mhz: i32,
    pub video_offset_mhz: i32,

    /// Which of the gated fields above are armed. A lever that is not armed is written back to
    /// the driver's own value, so a profile always describes the whole card.
    pub xoc_armed: XocLever,

    /// The single gate that armed every lever at once in earlier builds. Read so an old profile
    /// still arms; never written back.
    #[serde(rename = "XocEnabled", skip_serializing, deserialize_with = "legacy_xoc")]
    pub legacy_xoc_enabled: bool,

    /// Absolute core-voltage target in mV. When non-zero this drives both boost and the curve
    /// flatten (see `voltage_plan`) and takes precedence over the two fields above.
    pub target_voltage_mv: i32,

    /// AMD: fans stop below the driver's idle threshold. Ignored on NVIDIA.
    pub zero_rpm: bool,
    /// AMD: index into the vendor's memory timing presets (0 = default). Ignored on NVIDIA.
    pub memory_timing_level: i32,

    pub fan_mode: FanMode,
    pub fixed_fan_percent: i32,
    /// A duty per fan. Empty means "use `fixed_fan_percent` for all of them".
    pub fixed_fan_percents: Vec<i32>,
    pub fan_curve: FanCurve,

    /// Software temperature limit in °C; 0 = off. Not a driver setting: while the app runs, the
    /// service caps the graphics clock whenever the GPU passes it (see `services::soft_thermal`).
    /// For cards whose driver offers no settable thermal limit (RTX 50). Not in v1.1.0, whose
    /// profile reader ignores the field; profiles without it load as off.
    #[serde(rename = "SoftThermalLimitC")]
    pub soft_thermal_limit_c: i32,

    #[serde(with = "dotnet_datetime")]
    pub modified_utc: DateTime<Utc>,
}

fn legacy_xoc<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    Option::<bool>::deserialize(d).map(|b| b.unwrap_or(false))
}

impl Default for TuningProfile {
    fn default() -> Self {
        Self {
            name: "Default".into(),
            version: 1,
            gpu_name: None,
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
            clock_lock_min_mhz: 0,
            clock_lock_max_mhz: 0,
            sys_offset_mhz: 0,
            video_offset_mhz: 0,
            xoc_armed: XocLever::NONE,
            legacy_xoc_enabled: false,
            target_voltage_mv: 0,
            zero_rpm: true,
            memory_timing_level: 0,
            fan_mode: FanMode::Auto,
            fixed_fan_percent: 50,
            fixed_fan_percents: Vec::new(),
            fan_curve: FanCurve::default(),
            soft_thermal_limit_c: 0,
            modified_utc: Utc::now(),
        }
    }
}

impl TuningProfile {
    /// Fold the legacy single XOC gate into the per-lever set. Called after every load, matching
    /// the C# setter that armed `All` when `XocEnabled` was true.
    pub fn migrate_legacy(&mut self) {
        if self.legacy_xoc_enabled {
            self.xoc_armed = XocLever::ALL;
        }
        self.legacy_xoc_enabled = false;
    }

    /// Parse profile JSON (string or numeric enums), applying the legacy migration.
    pub fn from_json(json: &str) -> crate::Result<TuningProfile> {
        let mut p: TuningProfile = serde_json::from_str(json)?;
        p.migrate_legacy();
        Ok(p)
    }

    /// The duty for one fan, falling back to the shared one when no per-fan list is set.
    pub fn fan_percent_for(&self, index: usize) -> i32 {
        self.fixed_fan_percents.get(index).copied().unwrap_or(self.fixed_fan_percent)
    }

    /// Deep copy with fresh fan-curve stepping state (C# `Clone`).
    pub fn clone_profile(&self) -> TuningProfile {
        let mut p = self.clone();
        p.fan_curve = self.fan_curve.clone_config();
        p
    }

    pub fn stock(caps: &GpuCapabilities, gpu_name: Option<&str>) -> TuningProfile {
        TuningProfile {
            name: "Stock".into(),
            gpu_name: gpu_name.map(str::to_string),
            core_offset_mhz: 0,
            voltage_boost_percent: caps.voltage_boost_default_percent,
            power_limit_percent: caps.power_limit_default_percent,
            memory_timing_level: caps.memory_timing_default_level,
            temp_limit_c: caps.temp_limit_default_c,
            // "Stock" for an absolute memory slider is the card's rated clock, not zero.
            memory_offset_mhz: if caps.memory_clock_is_absolute { caps.memory_clock_default_mhz } else { 0 },
            zero_rpm: caps.zero_rpm_default,
            fan_mode: FanMode::Auto,
            ..TuningProfile::default()
        }
    }

    /// Clamp every value into the ranges the driver reports.
    pub fn clamp_to(&mut self, caps: &GpuCapabilities) {
        self.core_offset_mhz = clamp_i32(self.core_offset_mhz, caps.core_offset_min_mhz, caps.core_offset_max_mhz);
        self.memory_offset_mhz =
            clamp_i32(self.memory_offset_mhz, caps.memory_offset_min_mhz, caps.memory_offset_max_mhz);
        self.power_limit_percent =
            clamp_i32(self.power_limit_percent, caps.power_limit_min_percent, caps.power_limit_max_percent);
        self.temp_limit_c = clamp_i32(self.temp_limit_c, caps.temp_limit_min_c, caps.temp_limit_max_c);
        self.voltage_boost_percent =
            clamp_i32(self.voltage_boost_percent, caps.voltage_boost_min_percent, caps.voltage_boost_max_percent);
        self.voltage_offset_mv =
            clamp_i32(self.voltage_offset_mv, caps.voltage_offset_min_mv, caps.voltage_offset_max_mv);
        // 0 means "leave the rail alone"; only a real request gets clamped into the offered range.
        if self.voltage_rail_max_mv > 0 && caps.voltage_rail_max_mv > 0 {
            self.voltage_rail_max_mv =
                clamp_i32(self.voltage_rail_max_mv, caps.voltage_rail_min_mv, caps.voltage_rail_max_mv);
        }
        if self.msvdd_rail_max_mv > 0 && caps.msvdd_rail_max_mv > 0 {
            self.msvdd_rail_max_mv = clamp_i32(self.msvdd_rail_max_mv, caps.msvdd_rail_min_mv, caps.msvdd_rail_max_mv);
        }
        if self.voltage_rail_floor_mv > 0 && caps.voltage_rail_floor_max_mv > 0 {
            self.voltage_rail_floor_mv =
                clamp_i32(self.voltage_rail_floor_mv, caps.voltage_rail_floor_min_mv, caps.voltage_rail_floor_max_mv);
        }
        if self.msvdd_rail_floor_mv > 0 && caps.msvdd_rail_floor_max_mv > 0 {
            self.msvdd_rail_floor_mv =
                clamp_i32(self.msvdd_rail_floor_mv, caps.msvdd_rail_floor_min_mv, caps.msvdd_rail_floor_max_mv);
        }
        if caps.can_set_xbar_offset {
            self.xbar_offset_mhz = clamp_i32(self.xbar_offset_mhz, caps.xbar_offset_min_mhz, caps.xbar_offset_max_mhz);
        }
        if caps.can_set_sys_offset {
            self.sys_offset_mhz = clamp_i32(self.sys_offset_mhz, caps.sys_offset_min_mhz, caps.sys_offset_max_mhz);
        }
        if caps.can_set_video_offset {
            self.video_offset_mhz =
                clamp_i32(self.video_offset_mhz, caps.video_offset_min_mhz, caps.video_offset_max_mhz);
        }
        if caps.can_lock_clocks {
            // 0 on one side means "not asked for", not "asked for zero"; clamping it anyway turned
            // a floor-only request into a window pinned at the bottom of the card.
            if self.clock_lock_min_mhz > 0 {
                self.clock_lock_min_mhz =
                    clamp_i32(self.clock_lock_min_mhz, caps.clock_lock_min_mhz, caps.clock_lock_max_mhz);
            }
            if self.clock_lock_max_mhz > 0 {
                self.clock_lock_max_mhz =
                    clamp_i32(self.clock_lock_max_mhz, caps.clock_lock_min_mhz, caps.clock_lock_max_mhz);
            }
            if self.clock_lock_min_mhz > 0
                && self.clock_lock_max_mhz > 0
                && self.clock_lock_min_mhz > self.clock_lock_max_mhz
            {
                self.clock_lock_min_mhz = self.clock_lock_max_mhz;
            }
        }
        if self.target_voltage_mv > 0 && caps.max_voltage_mv > 0 {
            self.target_voltage_mv = clamp_i32(self.target_voltage_mv, caps.min_voltage_mv, caps.max_voltage_mv);
        }
        self.fixed_fan_percent = clamp_i32(self.fixed_fan_percent, caps.fan_min_percent, caps.fan_max_percent);
        for f in self.fixed_fan_percents.iter_mut() {
            *f = clamp_i32(*f, caps.fan_min_percent, caps.fan_max_percent);
        }
        // A list longer than the card has fans would write to coolers that are not there.
        if caps.fan_count > 0 && self.fixed_fan_percents.len() > caps.fan_count as usize {
            self.fixed_fan_percents.truncate(caps.fan_count as usize);
        }
        if !caps.memory_timing_options.is_empty() {
            self.memory_timing_level =
                clamp_i32(self.memory_timing_level, 0, caps.memory_timing_options.len() as i32 - 1);
        }
        self.fan_curve.normalize();
        // 0 (or less) is off. A real request lands in the card's window, or in the generic one when
        // this card offers none, so a hand-edited 300 °C never reaches the controller.
        if self.soft_thermal_limit_c <= 0 {
            self.soft_thermal_limit_c = 0;
        } else {
            let (lo, hi) = caps.soft_thermal_limit_range();
            self.soft_thermal_limit_c = clamp_i32(self.soft_thermal_limit_c, lo, hi);
        }
    }

    /// JSON with enums as numbers, the shape System.Text.Json wrote without a string-enum
    /// converter (the recovery journal).
    pub fn to_numeric_enum_value(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(obj) = v.as_object_mut() {
            obj.insert("FanMode".into(), serde_json::json!(self.fan_mode.as_number()));
            obj.insert("XocArmed".into(), serde_json::json!(self.xoc_armed.0));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_gate_arms_everything() {
        let p = TuningProfile::from_json(r#"{"Name":"x","XocEnabled":true}"#).unwrap();
        assert_eq!(p.xoc_armed, XocLever::ALL);
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("XocEnabled"));
    }

    #[test]
    fn reads_csharp_profile() {
        let json = r#"{
          "Name": "Slot 1", "Version": 1, "GpuName": "NVIDIA GeForce RTX 5070 Ti",
          "CoreOffsetMhz": 150, "MemoryOffsetMhz": 1000, "PowerLimitPercent": 110,
          "XocArmed": "Nvvdd, Xbar", "FanMode": "Curve", "FixedFanPercents": [40, 45],
          "FanCurve": { "Points": [ { "TemperatureC": 30, "FanPercent": 30 } ], "HysteresisC": 3,
                        "MinimumStepPercent": 2, "ZeroRpmBelowFirstPoint": false },
          "ModifiedUtc": "2026-08-18T15:28:02.1234567Z"
        }"#;
        let p = TuningProfile::from_json(json).unwrap();
        assert_eq!(p.core_offset_mhz, 150);
        assert_eq!(p.xoc_armed, XocLever::NVVDD | XocLever::XBAR);
        assert_eq!(p.fan_mode, FanMode::Curve);
        assert_eq!(p.fan_percent_for(1), 45);
        assert_eq!(p.fan_percent_for(2), 50);
        assert_eq!(p.temp_limit_c, 83, "missing field keeps its default");
        assert_eq!(dotnet_datetime::format(&p.modified_utc), "2026-08-18T15:28:02.1234567Z");
        let numeric = p.to_numeric_enum_value();
        assert_eq!(numeric["FanMode"], 2);
        assert_eq!(numeric["XocArmed"], 5);
        let back: TuningProfile = serde_json::from_value(numeric).unwrap();
        assert_eq!(back.fan_mode, FanMode::Curve);
    }

    #[test]
    fn soft_thermal_limit_round_trips_and_defaults_off() {
        // A v1.1.0 profile has no such field: it loads as off.
        let old = TuningProfile::from_json(r#"{"Name":"Slot 2","TempLimitC":80}"#).unwrap();
        assert_eq!(old.soft_thermal_limit_c, 0);
        let p = TuningProfile { soft_thermal_limit_c: 78, ..Default::default() };
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains(r#""SoftThermalLimitC":78"#), "{json}");
        assert_eq!(TuningProfile::from_json(&json).unwrap().soft_thermal_limit_c, 78);
        // The recovery journal's numeric-enum shape carries it too.
        let back: TuningProfile = serde_json::from_value(p.to_numeric_enum_value()).unwrap();
        assert_eq!(back.soft_thermal_limit_c, 78);
    }

    #[test]
    fn clamp_soft_thermal_limit() {
        let caps = GpuCapabilities {
            can_lock_clocks: true,
            can_set_soft_thermal_limit: true,
            soft_thermal_limit_min_c: 50,
            soft_thermal_limit_max_c: 89,
            ..Default::default()
        };
        let clamp = |v: i32, caps: &GpuCapabilities| {
            let mut p = TuningProfile { soft_thermal_limit_c: v, ..Default::default() };
            p.clamp_to(caps);
            p.soft_thermal_limit_c
        };
        assert_eq!(clamp(0, &caps), 0, "off stays off");
        assert_eq!(clamp(-5, &caps), 0, "negative is off");
        assert_eq!(clamp(30, &caps), 50);
        assert_eq!(clamp(83, &caps), 83);
        assert_eq!(clamp(95, &caps), 89, "never above the card's window (slowdown - 1)");
        // A card without the feature still keeps the value sane.
        assert_eq!(clamp(300, &GpuCapabilities::default()), 95);
    }

    #[test]
    fn clamp_keeps_unasked_clock_side() {
        let caps = GpuCapabilities {
            can_lock_clocks: true,
            clock_lock_min_mhz: 210,
            clock_lock_max_mhz: 3090,
            ..Default::default()
        };
        let mut p = TuningProfile { clock_lock_min_mhz: 1500, ..Default::default() };
        p.clamp_to(&caps);
        assert_eq!((p.clock_lock_min_mhz, p.clock_lock_max_mhz), (1500, 0));
    }
}
