//! Pure mapping of supported driver samples into [`GpuTelemetry`]; never substitutes zero for a
//! missing sensor.

use std::collections::HashMap;

use roch_core::{GpuTelemetry, SupplementalSensor};

/// One extra PMLog sensor shown as a supplemental reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sensor {
    pub id: i32,
    pub name: &'static str,
    pub unit: &'static str,
    /// Zero is a placeholder for "not reported" on this sensor, not a real reading.
    pub positive_only: bool,
}

const fn s(id: i32, name: &'static str, unit: &'static str) -> Sensor {
    Sensor { id, name, unit, positive_only: false }
}

const fn p(id: i32, name: &'static str, unit: &'static str) -> Sensor {
    Sensor { id, name, unit, positive_only: true }
}

/// Namespace for the AMD telemetry mapping (`AmdTelemetryMapper` in the original).
pub struct AmdTelemetryMapper;

impl AmdTelemetryMapper {
    /// AMD adl_defines.h ADL_PMLOG_SENSORS. Reserved, CPU/APU and undocumented-unit fields are
    /// deliberately excluded. Clocks are MHz, voltage mV, power W.
    pub const ADDITIONAL: [Sensor; 33] = [
        s(10, "Core VRM", "°C"),
        s(11, "Memory VRM", "°C"),
        s(24, "SoC VRM", "°C"),
        s(25, "Memory VRM 0", "°C"),
        s(26, "Memory VRM 1", "°C"),
        s(12, "Liquid", "°C"),
        s(13, "PCIe bridge", "°C"),
        s(28, "Graphics die", "°C"),
        s(29, "SoC", "°C"),
        s(42, "Liquid 0", "°C"),
        s(43, "Liquid 1", "°C"),
        s(50, "GCD hot spot", "°C"),
        s(51, "MCD hot spot", "°C"),
        p(16, "SoC voltage", "mV"),
        p(22, "Memory voltage", "mV"),
        s(4, "UVD clock 1", "MHz"),
        s(5, "UVD clock 2", "MHz"),
        s(6, "VCE clock", "MHz"),
        s(7, "VCN clock", "MHz"),
        s(36, "VCN1 clock 1", "MHz"),
        s(37, "VCN1 clock 2", "MHz"),
        s(30, "Graphics core power", "W"),
        s(17, "SoC power", "W"),
        p(49, "dGPU power limit", "W"),
        s(52, "Graphics thermal throttling", "%"),
        s(53, "Memory thermal throttling", "%"),
        s(54, "VRM thermal throttling", "%"),
        s(55, "Power throttling", "%"),
        s(56, "Current throttling", "%"),
        s(57, "Voltage throttling", "%"),
        p(40, "PCIe link generation", "gen"),
        p(41, "PCIe link width", "lanes"),
        p(58, "PCIe maximum generation", "gen"),
    ];

    /// The value, or NaN when it is not a believable reading for `unit`: non-finite, negative,
    /// zero where zero means "not reported", above 256 °C or above 100 %.
    pub fn valid(value: f64, unit: &str, positive_only: bool) -> f64 {
        if !value.is_finite() || value < 0.0 || (positive_only && value == 0.0) {
            return f64::NAN;
        }
        if (unit == "°C" && value > 256.0) || (unit == "%" && value > 100.0) {
            return f64::NAN;
        }
        value
    }

    /// Build a sample from PMLog readings (by sensor id) and ADLX readings (by metric key).
    /// PMLog wins; ADLX fills in what PMLog does not report.
    pub fn map(pm: &HashMap<i32, f64>, adlx: &HashMap<&'static str, f64>) -> GpuTelemetry {
        let pv = |id: i32| pm.get(&id).copied().unwrap_or(f64::NAN);
        let av = |key: &str| adlx.get(key).copied().unwrap_or(f64::NAN);
        let read = |id: i32, key: &str, unit: &str, positive: bool| {
            let v = Self::valid(pv(id), unit, positive);
            if v.is_finite() { v } else { Self::valid(av(key), unit, positive) }
        };

        let mut extra = Vec::new();
        for d in &Self::ADDITIONAL {
            let value = Self::valid(pv(d.id), d.unit, d.positive_only);
            if value.is_finite() {
                extra.push(SupplementalSensor::new(format!("adl:{}", d.id), d.name, d.unit, value));
            }
        }
        let mut push_extra = |key: &str, name: &str, unit: &str, value: f64| {
            let value = Self::valid(value, unit, false);
            if value.is_finite() {
                extra.push(SupplementalSensor::new(key, name, unit, value));
            }
        };
        push_extra("amd:asic", "GPU ASIC power", "W", read(23, "power", "W", false));
        push_extra("amd:intake", "Intake", "°C", read(74, "intake", "°C", false));
        push_extra("adlx:shared", "Shared GPU memory used", "MB", av("shared"));

        let fan = read(14, "fan", "RPM", false);
        let duty = read(15, "fanduty", "%", false);
        GpuTelemetry {
            core_clock_mhz: read(1, "core", "MHz", false),
            memory_clock_mhz: read(2, "memoryclock", "MHz", false),
            fabric_clock_mhz: Self::valid(pv(44), "MHz", false),
            soc_clock_mhz: Self::valid(pv(3), "MHz", false),
            temperature_c: read(8, "temperature", "°C", false),
            hot_spot_c: read(27, "hotspot", "°C", false),
            memory_temperature_c: read(9, "memorytemp", "°C", false),
            voltage_mv: read(21, "voltage", "mV", true),
            // ASIC power is NOT board draw. Keep separate even when board power is missing.
            power_watts: read(73, "board", "W", false),
            power_percent: f64::NAN,
            gpu_load_percent: read(19, "usage", "%", false),
            memory_load_percent: Self::valid(pv(20), "%", false),
            memory_used_mb: Self::valid(av("vram"), "MB", false),
            fan_rpm: fan,
            fan_percent: duty,
            fan_rpms: vec![fan],
            fan_percents: vec![duty],
            supplemental_sensors: extra,
            ..GpuTelemetry::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adlx::AdlxTelemetry;
    use std::collections::HashSet;

    fn single<'a>(t: &'a GpuTelemetry, key: &str) -> &'a SupplementalSensor {
        let hits: Vec<_> = t.supplemental_sensors.iter().filter(|s| s.key == key).collect();
        assert_eq!(hits.len(), 1, "exactly one {key}");
        hits[0]
    }

    fn pm_sample() -> HashMap<i32, f64> {
        [
            (1, 2500.0),
            (3, 1371.0),
            (44, 2401.0),
            (10, 43.0),
            (16, 940.0),
            (17, 12.0),
            (23, 70.0),
            (49, 0.0),
            (73, 100.0),
            (14, 0.0),
            (15, 0.0),
            (40, 4.0),
            (41, 16.0),
        ]
        .into_iter()
        .collect()
    }

    fn adlx_sample() -> HashMap<&'static str, f64> {
        [
            ("core", 2400.0),
            ("temperature", 32.0),
            ("vram", 292.0),
            ("shared", 91.0),
            ("board", 99.0),
            ("power", 69.0),
            ("intake", 27.0),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn missing_values_are_not_fake_zeros() {
        let missing = AmdTelemetryMapper::map(&HashMap::new(), &HashMap::new());
        assert!(missing.core_clock_mhz.is_nan());
        assert!(missing.voltage_mv.is_nan());
        assert!(missing.power_watts.is_nan());
        assert!(missing.memory_used_mb.is_nan());
        // unsupported AMD extras hidden
        assert!(missing.supplemental_sensors.is_empty());
    }

    #[test]
    fn mapped_sample() {
        let m = AmdTelemetryMapper::map(&pm_sample(), &adlx_sample());
        assert_eq!(m.core_clock_mhz, 2500.0, "PMLog sample preferred");
        assert_eq!(m.temperature_c, 32.0, "ADLX temperature fallback");
        assert_eq!(m.fabric_clock_mhz, 2401.0, "native FCLK");
        assert_eq!(m.soc_clock_mhz, 1371.0, "native SoC clock");
        assert_eq!(m.memory_used_mb, 292.0, "ADLX VRAM allocation");
        assert_eq!(m.power_watts, 100.0, "native board watts");
        assert!(m.fan_rpm == 0.0 && m.fan_percent == 0.0, "real fan stop remains zero");
        assert!(!m.supplemental_sensors.iter().any(|s| s.key == "adl:49"), "zero power limit placeholder hidden");
        assert_eq!(single(&m, "adl:16").value, 940.0, "SoC voltage units mV");
        assert_eq!(single(&m, "adl:10").value, 43.0, "native VRM temperature");
        assert_eq!(single(&m, "adlx:shared").value, 91.0, "ADLX shared memory");
        assert_eq!(m.supplemental_sensors.iter().filter(|s| s.key == "amd:asic").count(), 1, "ASIC deduplicated");
        assert!(
            !m.supplemental_sensors.iter().any(|s| matches!(s.key.as_str(), "adlx:core" | "adlx:board" | "adlx:vram")),
            "base ADLX readings not duplicated as extras"
        );
    }

    #[test]
    fn asic_watts_never_mislabeled_board_watts() {
        let pm: HashMap<i32, f64> = [(23, 70.0)].into_iter().collect();
        let t = AmdTelemetryMapper::map(&pm, &HashMap::new());
        assert!(t.power_watts.is_nan());
        assert_eq!(t.supplemental_sensors.len(), 1);
        assert_eq!(t.supplemental_sensors[0].value, 70.0);
    }

    #[test]
    fn adlx_fallback_when_pmlog_fails() {
        let t = AmdTelemetryMapper::map(&HashMap::new(), &adlx_sample());
        assert_eq!(t.core_clock_mhz, 2400.0, "ADLX works when PMLog fails");
        assert!(t.fabric_clock_mhz.is_nan(), "fallback does not fabricate FCLK");
    }

    #[test]
    fn invalid_sensor_values_rejected() {
        assert!(AmdTelemetryMapper::valid(f64::INFINITY, "W", false).is_nan());
        assert!(AmdTelemetryMapper::valid(-1.0, "MHz", false).is_nan());
        assert!(AmdTelemetryMapper::valid(54000.0, "°C", false).is_nan());
        assert!(AmdTelemetryMapper::valid(101.0, "%", false).is_nan());
        assert!(AmdTelemetryMapper::valid(0.0, "W", true).is_nan());
        assert_eq!(AmdTelemetryMapper::valid(0.0, "W", false), 0.0);
    }

    #[test]
    fn sensor_ids_and_adlx_keys_unique() {
        let ids: HashSet<_> = AmdTelemetryMapper::ADDITIONAL.iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), AmdTelemetryMapper::ADDITIONAL.len());
        let keys: HashSet<_> = AdlxTelemetry::METRICS.iter().map(|m| m.key).collect();
        assert_eq!(keys.len(), AdlxTelemetry::METRICS.len());
    }
}
