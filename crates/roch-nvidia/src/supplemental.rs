//! `NvidiaSupplementalTelemetry`: the extra native readings shown under "Sensors" (video/bus load,
//! VRAM headroom, the reported video clock, the thermal limit, the limit flags, the PCIe link).
//! Every reading is optional; a missing or failed one is simply left out. Read-only.

use roch_core::SupplementalSensor;

use crate::nvapi::{self, GpuHandle};
use crate::{nvml, wrapper};

/// PCIe link rate per generation, GT/s; NaN for an unknown generation (and so never shown).
pub fn pcie_speed(generation: i32) -> f64 {
    match generation {
        1 => 2.5,
        2 => 5.0,
        3 => 8.0,
        4 => 16.0,
        5 => 32.0,
        6 => 64.0,
        _ => f64::NAN,
    }
}

/// PerformanceLimit flags.
const LIMIT_POWER: u32 = 1;
const LIMIT_TEMPERATURE: u32 = 2;
const LIMIT_VOLTAGE: u32 = 4;
const LIMIT_NO_LOAD: u32 = 16;
/// PublicClockDomain.Video.
const DOMAIN_VIDEO: usize = 8;

/// The sensor list being built: only finite, non-negative values make it in, keyed "nv:<key>".
#[derive(Default)]
pub(crate) struct Sensors(pub Vec<SupplementalSensor>);

impl Sensors {
    pub(crate) fn add(&mut self, key: &str, name: &str, unit: &str, value: f64) {
        if value.is_finite() && value >= 0.0 {
            self.0.push(SupplementalSensor::new(format!("nv:{key}"), name, unit, value));
        }
    }

    /// The VRAM pair, from the dedicated total and what is currently free (kB).
    pub(crate) fn add_memory(&mut self, total_kb: f64, free_kb: f64) {
        if total_kb > 0.0 && free_kb <= total_kb {
            self.add("memory-free", "Available VRAM", "MB", free_kb / 1024.0);
            self.add("memory-utilization", "VRAM usage", "%", (total_kb - free_kb) * 100.0 / total_kb);
        }
    }

    pub(crate) fn add_limits(&mut self, flags: u32) {
        for (flag, key, name) in [
            (LIMIT_POWER, "power", "Power limit active"),
            (LIMIT_TEMPERATURE, "thermal", "Thermal limit active"),
            (LIMIT_VOLTAGE, "voltage", "Voltage limit active"),
            (LIMIT_NO_LOAD, "idle", "No-load limit active"),
        ] {
            self.add(&format!("limit-{key}"), name, "state", if flags & flag != 0 { 1.0 } else { 0.0 });
        }
    }

    pub(crate) fn add_pcie(&mut self, pcie: &nvml::PcieLink) {
        if pcie.current_generation > 0 {
            self.add("pcie-generation", "PCIe generation", "gen", pcie.current_generation as f64);
            self.add("pcie-speed", "PCIe link speed", "GT/s", pcie_speed(pcie.current_generation));
        }
        if pcie.current_width > 0 {
            self.add("pcie-width", "PCIe link width", "lanes", pcie.current_width as f64);
        }
    }
}

/// Readings the telemetry sample has already taken this tick (`None` where its call failed), so
/// the sensors below do not ask the driver for them a second time.
#[derive(Default)]
pub struct Already<'a> {
    pub utilization: Option<&'a [(u32, i32)]>,
    pub memory: Option<nvapi::MemoryInfo>,
    pub clocks: Option<&'a nvapi::ClockFrequencies>,
    pub limit_flags: Option<u32>,
}

/// `NvidiaSupplementalTelemetry.Read`.
pub fn read(g: GpuHandle, nvml_index: i32, already: Already) -> Vec<SupplementalSensor> {
    let mut s = Sensors::default();
    for &(domain, pct) in already.utilization.unwrap_or_default() {
        if domain == wrapper::UTIL_VIDEO_ENGINE {
            s.add("video-load", "Video engine", "%", pct as f64);
        }
        if domain == wrapper::UTIL_BUS_INTERFACE {
            s.add("bus-load", "Bus interface", "%", pct as f64);
        }
    }
    if let Some(m) = already.memory {
        s.add_memory(m.available_dedicated_kb as f64, m.current_available_dedicated_kb as f64);
    }
    if let Some(c) = already.clocks {
        let video = c.clocks[DOMAIN_VIDEO];
        if video.present() {
            s.add("video-clock", "Video clock (reported)", "MHz", video.frequency_khz as f64 / 1000.0);
        }
    }
    if let Ok(t) = nvapi::thermal_policies_status(g)
        && let Some(first) = t.first()
    {
        s.add("thermal-limit", "GPU thermal limit", "°C", first.target_c() as f64);
    }
    if let Some(flags) = already.limit_flags {
        s.add_limits(flags);
    }
    s.add_pcie(&nvml::pcie_link(nvml_index));
    s.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcie_rates() {
        assert_eq!(pcie_speed(1), 2.5);
        assert_eq!(pcie_speed(5), 32.0);
        assert!(pcie_speed(0).is_nan());
        assert!(pcie_speed(7).is_nan());
    }

    #[test]
    fn only_real_readings_are_kept() {
        let mut s = Sensors::default();
        s.add("a", "A", "%", f64::NAN);
        s.add("b", "B", "%", -1.0);
        s.add("c", "C", "%", f64::INFINITY);
        s.add("d", "D", "%", 0.0);
        assert_eq!(s.0.len(), 1);
        assert_eq!(s.0[0].key, "nv:d");

        let mut m = Sensors::default();
        m.add_memory(16_000.0 * 1024.0, 15_600.0 * 1024.0);
        assert_eq!(m.0[0].key, "nv:memory-free");
        assert!((m.0[0].value - 15_600.0).abs() < 1e-9);
        assert!((m.0[1].value - 2.5).abs() < 1e-9);
        let mut bad = Sensors::default();
        bad.add_memory(100.0, 200.0); // free above total: driver nonsense, skipped
        bad.add_memory(0.0, 0.0);
        assert!(bad.0.is_empty());
    }

    #[test]
    fn limit_flags_and_link() {
        let mut s = Sensors::default();
        s.add_limits(LIMIT_VOLTAGE);
        let v: Vec<(String, f64)> = s.0.iter().map(|x| (x.key.clone(), x.value)).collect();
        assert_eq!(
            v,
            [
                ("nv:limit-power".into(), 0.0),
                ("nv:limit-thermal".into(), 0.0),
                ("nv:limit-voltage".into(), 1.0),
                ("nv:limit-idle".into(), 0.0)
            ]
        );
        let mut p = Sensors::default();
        p.add_pcie(&nvml::PcieLink { max_generation: 5, max_width: 16, current_generation: 1, current_width: 16 });
        let keys: Vec<&str> = p.0.iter().map(|x| x.key.as_str()).collect();
        assert_eq!(keys, ["nv:pcie-generation", "nv:pcie-speed", "nv:pcie-width"]);
        assert_eq!(p.0[1].value, 2.5);
        let mut none = Sensors::default();
        none.add_pcie(&nvml::PcieLink { max_generation: 0, max_width: 0, current_generation: 0, current_width: 0 });
        assert!(none.0.is_empty());
    }
}
