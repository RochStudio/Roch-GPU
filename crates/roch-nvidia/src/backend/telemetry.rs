//! Telemetry, the cheap temperature read, tuning-state readback and the clock-domain readers.
//! All read-only.

use std::collections::{BTreeMap, BTreeSet};

use roch_core::util::round_even_i32;
use roch_core::{GpuTelemetry, GpuTuningState, Result};

use super::caps::{DOMAIN_GRAPHICS, DOMAIN_MEMORY, PSTATE_P0};
use super::{ClockDomain, MAX_DOMAIN_TYPE, NvidiaBackend};
use crate::nvapi::{self, GpuHandle};
use crate::private;
use crate::vf_curve;
use crate::wrapper;
use crate::{nvml, supplemental};

/// ThermalSettingsTarget.GPU / .Memory.
const TARGET_GPU: i32 = 1;
const TARGET_MEMORY: i32 = 2;
/// PowerTopologyDomain.GPU.
const TOPOLOGY_GPU: u32 = 0;

/// `PerfState`: the pstate's enum name up to the first underscore ("P0", "P8").
pub(crate) fn perf_state_text(pstate: u32) -> String {
    nvapi::pstate_name(pstate).split('_').next().unwrap_or_default().to_string()
}

/// `LimitReason`: "None", else the PerformanceLimit flags with "Limit" dropped ("Power, Voltage").
pub(crate) fn limit_text(limit: u32) -> String {
    if limit == 0 { "None".into() } else { nvapi::performance_limit_name(limit).replace("Limit", "") }
}

impl NvidiaBackend {
    /// The public thermal-sensor call and nothing else — measured at 0.03 ms against 12.9 ms for
    /// the full read, of which the power-topology query alone is 8.8 ms. Used for background ticks
    /// that only feed the fan curve.
    pub(super) fn read_temperature_only_impl(&mut self, gpu: usize) -> Result<GpuTelemetry> {
        let g = self.gpu(gpu)?;
        let mut temp = 0.0;
        if let Ok(sensors) = nvapi::thermal_settings(g) {
            for s in sensors {
                if s.target == TARGET_GPU {
                    temp = s.current as f64;
                }
            }
        }
        Ok(GpuTelemetry { temperature_c: temp, ..GpuTelemetry::default() })
    }

    /// Drop the remembered clock window when the NVML session has been reopened, which only happens
    /// after a driver reset — and a driver reset clears the lock. Without this the window would keep
    /// reporting a range nothing is holding.
    pub(super) fn forget_lock_across_driver_reset(&mut self) {
        let epoch = nvml::session_epoch();
        if epoch == self.lock_epoch {
            return;
        }
        self.lock_epoch = epoch;
        self.locked_min_mhz = 0;
        self.locked_max_mhz = 0;
    }

    pub(super) fn read_telemetry_impl(&mut self, gpu: usize) -> Result<GpuTelemetry> {
        self.forget_lock_across_driver_reset();
        let g = self.gpu(gpu)?;

        // Measured rail currents: one private call, only while something is on screen to show them.
        let rails = private::read_power_rails(g);
        let rail_amps: Vec<f64> = rails.iter().map(|r| r.milliamps as f64 / 1000.0).collect();
        let rail_volts: Vec<f64> = rails.iter().map(|r| r.microvolts as f64 / 1_000_000.0).collect();

        let (mut core, mut mem) = (0.0, 0.0);
        if let Ok(c) = nvapi::clock_frequencies(g, 0) {
            core = c.clocks[DOMAIN_GRAPHICS as usize].frequency_khz as f64 / 1000.0;
            mem = c.clocks[DOMAIN_MEMORY as usize].frequency_khz as f64 / 1000.0;
        }

        let (mut temp, mut hotspot, mut mem_temp) = (0.0, f64::NAN, f64::NAN);
        if let Ok(sensors) = nvapi::thermal_settings(g) {
            for s in sensors {
                if s.target == TARGET_GPU {
                    temp = s.current as f64;
                } else if s.target == TARGET_MEMORY {
                    mem_temp = s.current as f64;
                }
            }
        }

        // Hot-spot and memory-junction come from the private thermal-sensors call (as HWiNFO/LHM).
        let mask = *self.thermal_mask.entry(gpu).or_insert_with(|| private::probe_mask(g));
        if mask != 0 {
            let slots = private::read_thermal_slots(g, mask);
            let (hs, mj) = private::interpret(&self.device_name(gpu), &slots);
            if !hs.is_nan() {
                hotspot = hs;
            }
            if !mj.is_nan() {
                mem_temp = mj;
            }
        }

        let voltage = nvapi::current_voltage_uv(g).map(|uv| uv as f64 / 1000.0).unwrap_or(f64::NAN);

        // Board watts. NVAPI's power families report per-cent-mille of the limit and no watts at
        // all, so this is the one figure that has to come from NVML.
        let nvml_index = self.nvml_index_for(gpu);
        let watts = nvml::power_watts(nvml_index);

        // Measured per-rail voltages. The rail status call reports the same number for every rail,
        // so the ADC is the only source that tells NVVDD and MSVDD apart.
        let uv = private::read_rail_voltages_uv(g);
        let nvvdd_mv = uv.get(&private::RAIL_NVVDD).map(|&v| v as f64 / 1000.0).unwrap_or(f64::NAN);
        let msvdd_mv = uv.get(&private::RAIL_MSVDD).map(|&v| v as f64 / 1000.0).unwrap_or(f64::NAN);

        let mut power = 0.0;
        if let Ok(entries) = nvapi::power_topology_status(g)
            && let Some(e) = entries.iter().find(|e| e.domain == TOPOLOGY_GPU).or(entries.first())
        {
            power = wrapper::pcm_percent(e.usage_pcm);
        }

        let (mut load, mut mem_load) = (0.0, 0.0);
        if let Ok(domains) = wrapper::utilization_domains(g) {
            load = wrapper::domain_percent(&domains, wrapper::UTIL_GPU).unwrap_or(0) as f64;
            mem_load = wrapper::domain_percent(&domains, wrapper::UTIL_FRAME_BUFFER).unwrap_or(0) as f64;
        }

        let mem_used = nvapi::memory_info(g)
            .map(|m| m.available_dedicated_kb.wrapping_sub(m.current_available_dedicated_kb) as f64 / 1024.0)
            .unwrap_or(0.0);

        let (mut fan_pcts, mut fan_rpms) = (Vec::new(), Vec::new());
        if let Ok(coolers) = wrapper::coolers(g) {
            for c in coolers {
                fan_pcts.push(c.current_level as f64);
                fan_rpms.push(c.current_rpm as f64);
            }
        }

        let pstate = nvapi::current_pstate(g).map(perf_state_text).unwrap_or_default();
        let limit = nvapi::performance_limit(g).map(limit_text).unwrap_or_default();

        Ok(GpuTelemetry {
            core_clock_mhz: core,
            memory_clock_mhz: mem,
            temperature_c: temp,
            hot_spot_c: hotspot,
            memory_temperature_c: mem_temp,
            voltage_mv: voltage,
            power_percent: power,
            power_watts: if watts.is_nan() { 0.0 } else { watts },
            rail_amps,
            rail_volts,
            nvvdd_mv,
            msvdd_mv,
            gpu_load_percent: load,
            memory_load_percent: mem_load,
            memory_used_mb: mem_used,
            fan_percent: max_or_zero(&fan_pcts),
            fan_rpm: max_or_zero(&fan_rpms),
            fan_percents: fan_pcts,
            fan_rpms,
            supplemental_sensors: supplemental::read(g, nvml_index),
            perf_state: pstate,
            limit_reason: limit,
            ..GpuTelemetry::default()
        })
    }

    pub(super) fn read_tuning_state_impl(&mut self, gpu: usize) -> Result<GpuTuningState> {
        let g = self.gpu(gpu)?;
        let (mut core, mut mem) = (0, 0);
        if let Ok(ps) = nvapi::pstates20(g)
            && let Some(clocks) = ps.clocks_for(PSTATE_P0)
        {
            for c in clocks {
                if c.domain_id == DOMAIN_GRAPHICS {
                    core = c.freq_delta_khz.value / 1000;
                } else if c.domain_id == DOMAIN_MEMORY {
                    mem = c.freq_delta_khz.value / 1000;
                }
            }
        }

        let power = match nvapi::power_policies_status(g) {
            Ok(p) => p.first().map(|e| round_even_i32(wrapper::pcm_percent(e.target_pcm))).unwrap_or(100),
            Err(_) => 100,
        };

        // 0, not a plausible-looking 83: a card with no thermal policy has no limit to report, and
        // inventing one made `info` print "temp 83°C" under "Temp limit: not supported".
        let temp = nvapi::thermal_policies_status(g).ok().and_then(|t| t.first().map(|e| e.target_c())).unwrap_or(0);

        let volt_boost = nvapi::core_voltage_boost_percent(g).map(|p| p as i32).unwrap_or(0);

        // Infer the voltage offset: from the lock if one holds, else from a flattened live curve.
        let mut volt_offset = 0;
        let lock_mv = self.read_voltage_lock_mv_impl(gpu);
        if lock_mv > 0
            && let Ok(base) = self.read_base_curve(gpu, g)
        {
            let stock_max = vf_curve::max_voltage_uv(&base);
            if stock_max > 0 {
                volt_offset = lock_mv - (stock_max / 1000) as i32;
            }
        }
        if volt_offset == 0
            && let Ok(eff) = self.read_effective_curve(gpu, g)
        {
            let cap_uv = vf_curve::infer_cap_voltage_uv(&eff);
            if cap_uv > 0
                && let Ok(base) = self.read_base_curve(gpu, g)
            {
                let stock_max = vf_curve::max_voltage_uv(&base);
                if stock_max > cap_uv {
                    volt_offset = -(((stock_max - cap_uv) / 1000) as i32);
                }
            }
        }

        let (mut fan_manual, mut fan_pct) = (false, 0);
        if let Ok(coolers) = wrapper::coolers(g)
            && !coolers.is_empty()
        {
            fan_manual = coolers.iter().any(|c| c.current_policy == wrapper::POLICY_MANUAL);
            fan_pct = coolers.iter().map(|c| c.current_level).max().unwrap_or(0);
        }

        // One pass for both rails: reading them separately cost three extra driver calls and ~16 KB
        // of marshalling per read, on a path the GUI hits after every Apply.
        let (mut rail_max, mut msvdd_max, mut rail_floor, mut msvdd_floor) = (0, 0, 0, 0);
        for rail in private::read_volt_rails(g) {
            if rail.index == private::RAIL_NVVDD {
                rail_max = rail.max_uv as i32 / 1000;
                rail_floor = rail.min_uv as i32 / 1000;
            } else if rail.index == private::RAIL_MSVDD {
                msvdd_max = rail.max_uv as i32 / 1000;
                msvdd_floor = rail.min_uv as i32 / 1000;
            }
        }

        let sys = private::read_domain_offset_mhz(g, private::SLOT_SYS);
        let video = private::read_domain_offset_mhz(g, private::SLOT_VIDEO);
        let xbar = private::read_xbar_offset_mhz(g);

        Ok(GpuTuningState {
            core_offset_mhz: core,
            memory_offset_mhz: mem,
            voltage_rail_max_mv: rail_max,
            msvdd_rail_max_mv: msvdd_max,
            voltage_rail_floor_mv: rail_floor,
            msvdd_rail_floor_mv: msvdd_floor,
            xbar_offset_mhz: xbar,
            sys_offset_mhz: sys,
            video_offset_mhz: video,
            nvvdd_ocp_milliamps: ocp_now(g, private::OCP_SLOT_NVVDD),
            msvdd_ocp_milliamps: ocp_now(g, private::OCP_SLOT_MSVDD),
            locked_clock_min_mhz: self.locked_min_mhz,
            locked_clock_max_mhz: self.locked_max_mhz,
            power_limit_percent: power,
            temp_limit_c: temp,
            voltage_boost_percent: volt_boost,
            voltage_offset_mv: volt_offset,
            fan_manual,
            fan_percent: fan_pct,
            ..GpuTuningState::default()
        })
    }

    /// Every clock domain the private family lists, with the offset range it admits to, what its
    /// own counter measures now, and the offset applied. The domains that carry a range are the ones
    /// worth offering. Empty when the GPU index is invalid or the family is absent.
    pub fn read_clock_domains(&mut self, gpu: usize) -> Vec<ClockDomain> {
        let Ok(g) = self.gpu(gpu) else { return Vec::new() };
        private::read_domain_entries(g)
            .into_iter()
            .map(|d| ClockDomain {
                slot: d.index,
                type_: d.kind,
                has_range: d.has_range,
                min_mhz: d.min_mhz,
                max_mhz: d.max_mhz,
                measured_mhz: (private::measure_clock_khz(g, d.kind) / 1000) as i32,
                offset_mhz: private::read_domain_offset_mhz(g, d.index),
            })
            .collect()
    }

    /// Offset one clock domain by slot. **Writes to the GPU.** Returns the driver's status: 0
    /// landed, anything else refused (not an error, because "this domain reports a range and will
    /// not move" is a real answer the caller must be able to report). An invalid GPU index returns
    /// `domain_offset::INVALID_REQUEST` without a call.
    pub fn set_clock_domain_offset(&mut self, gpu: usize, slot: i32, mhz: i32) -> i32 {
        match self.gpu(gpu) {
            Ok(g) => private::write_domain_offset_mhz(g, slot, mhz),
            Err(_) => crate::domain_offset::INVALID_REQUEST,
        }
    }

    /// The core-measured, crossbar, SYS and video counters plus every other domain type the card
    /// answers for, for the monitor's table. A handful of small private calls, cheap enough for the
    /// poll loop (the full domain walk in `read_clock_domains` is not).
    pub(super) fn measure_extra_clocks_impl(&mut self, gpu: usize) -> BTreeMap<String, f64> {
        let mut d = BTreeMap::new();
        let Ok(g) = self.gpu(gpu) else { return d };
        // The core's own counter, which is not the requested clock: at idle the card reports a boost
        // figure it is not running. Instantaneous, not an interval-averaged effective clock.
        d.insert("coremeasured".into(), private::measure_clock_khz(g, private::DOMAIN_CORE) as f64 / 1000.0);
        d.insert("xbar".into(), private::measure_clock_khz(g, private::DOMAIN_XBAR) as f64 / 1000.0);
        d.insert("sys".into(), private::measure_clock_khz(g, 2) as f64 / 1000.0);
        d.insert("video".into(), private::measure_clock_khz(g, 21) as f64 / 1000.0);
        // The rest, reported by the type the driver gives them rather than a guessed name: what
        // they are is a question the min/max columns answer over a session.
        for t in self.extra_domain_types(gpu, g) {
            d.insert(format!("domain{t}"), private::measure_clock_khz(g, t) as f64 / 1000.0);
        }
        d
    }

    /// Domain types worth reporting that are not already shown under a name of their own. The info
    /// walk lists nine domains on a 5070 Ti and misses the reference clock at type 22, which the
    /// counter answers for perfectly well, so the walk is a floor: both are unioned (the walk also
    /// carries type 31, which reads a flat zero and a "keep what moves" probe would drop). Cached —
    /// the info call reads a 34 KB struct and the set does not change while the card is plugged in.
    fn extra_domain_types(&mut self, gpu: usize, g: GpuHandle) -> Vec<i32> {
        if let Some(t) = self.extra_domain_types.get(&gpu) {
            return t.clone();
        }
        // Core, crossbar, SYS and video have their own rows; memory has one from the public API.
        let named = [0, 1, 2, 21, 4];
        let mut found = BTreeSet::new();
        for e in private::read_domain_entries(g) {
            if !named.contains(&e.kind) {
                found.insert(e.kind);
            }
        }
        for t in 0..=MAX_DOMAIN_TYPE {
            if !named.contains(&t) && !found.contains(&t) && private::measure_clock_khz(g, t) > 0 {
                found.insert(t);
            }
        }
        let list: Vec<i32> = found.into_iter().collect();
        self.extra_domain_types.insert(gpu, list.clone());
        list
    }
}

/// `list.Max()`, or 0 for an empty list.
fn max_or_zero(v: &[f64]) -> f64 {
    v.iter().copied().reduce(f64::max).unwrap_or(0.0)
}

/// One OCP channel's limit as the card reports it now; 0 when unreadable.
pub(super) fn ocp_now(g: GpuHandle, slot: i32) -> i32 {
    private::read_ocp_channels(g).iter().find(|c| c.slot == slot).map(|c| c.value).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perf_and_limit_text() {
        assert_eq!(perf_state_text(0), "P0");
        assert_eq!(perf_state_text(8), "P8");
        assert_eq!(perf_state_text(12), "P12");
        assert_eq!(limit_text(0), "None");
        assert_eq!(limit_text(4), "Voltage");
        assert_eq!(limit_text(1 | 4), "Power, Voltage");
        assert_eq!(limit_text(16), "NoLoad");
    }
}
