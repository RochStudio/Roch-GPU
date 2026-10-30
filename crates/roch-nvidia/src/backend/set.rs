//! The writes other than the V/F curve and voltage lock (those live in `curve`), and reset.
//! Everything in this file changes GPU state.

use roch_core::util::clamp_i32;
use roch_core::{GpuError, Result};

use super::{MIN_LOCKABLE_MHZ, NvidiaBackend};
use crate::nvapi::{self, NvError, PStates20ClockEntry, PStates20Delta, PStates20InfoV1, PowerPolicyStatusEntry};
use crate::wrapper::{self, CoolerWriteError};
use crate::{domain_offset, nvml, private};

pub(super) const DOMAIN_GRAPHICS: u32 = 0;
pub(super) const DOMAIN_MEMORY: u32 = 4;
pub(super) const NVVDD: &str = "core (NVVDD)";
pub(super) const MSVDD: &str = "MSVDD";

/// .NET `{0:+#;-#;0}`.
pub(crate) fn signed(v: i32) -> String {
    if v > 0 { format!("+{v}") } else { v.to_string() }
}

/// `{0:0.##}`: at most two decimals, trailing zeros dropped, halves away from zero.
pub(crate) fn two_dp(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    roch_core::models::graphics_info::trim_decimals(r, 2)
}

fn cooler_error(e: CoolerWriteError, wrap: impl FnOnce(&NvError) -> String) -> GpuError {
    match e {
        CoolerWriteError::Nv(n @ NvError::Api(_)) => GpuError::new(wrap(&n)),
        CoolerWriteError::Nv(n) => n.into(),
        CoolerWriteError::InvalidCooler => GpuError::new(CoolerWriteError::INVALID_COOLER),
    }
}

impl NvidiaBackend {
    /// Move one end of a voltage rail to an absolute mV target; 0 restores the card's default. Both
    /// ends work alike: the driver reports the end it is currently configured at, offset included,
    /// so the base the hardware counts from is that minus the offset and the write is a new offset
    /// from that base.
    pub(super) fn set_rail_end(
        &mut self,
        gpu: usize,
        rail_index: i32,
        millivolts: i32,
        name: &str,
        floor: bool,
    ) -> Result<()> {
        let g = self.gpu(gpu)?;
        let base_uv = private::read_volt_rails(g)
            .iter()
            .find(|r| r.index == rail_index)
            .map(|r| {
                if floor {
                    (r.min_uv as i32).wrapping_sub(r.min_offset_uv)
                } else {
                    (r.max_uv as i32).wrapping_sub(r.max_offset_uv)
                }
            })
            .unwrap_or(0);
        if base_uv <= 0 {
            roch_core::bail!("This card does not expose a {name} voltage rail.");
        }
        let offset_uv = if millivolts <= 0 { 0 } else { millivolts.wrapping_mul(1000).wrapping_sub(base_uv) };
        let status = if floor {
            private::write_volt_rail_min_offset(g, rail_index, offset_uv)
        } else {
            private::write_volt_rail_max_offset(g, rail_index, offset_uv)
        };
        if status != 0 {
            roch_core::bail!(
                "Failed to set the {name} rail {} to {millivolts} mV (offset {} mV): status {status}.",
                if floor { "floor" } else { "ceiling" },
                signed(offset_uv / 1000)
            );
        }
        Ok(())
    }

    /// Pin the graphics clock (NVML); 0/0 releases it.
    pub(super) fn set_clock_range_impl(&mut self, gpu: usize, min_mhz: i32, max_mhz: i32) -> Result<()> {
        let nvml_index = self.nvml_index_for(gpu);
        if min_mhz <= 0 && max_mhz <= 0 {
            // Always ask the driver to release it, even when this process never pinned anything. The
            // lock lives in the driver and outlives us: skipping the call when the remembered state
            // said "already unpinned" left a lock standing whenever an earlier run had set one.
            if let Some(e) = nvml::reset_graphics_clocks(nvml_index) {
                roch_core::bail!("Failed to release the clock range: {e}");
            }
            self.locked_min_mhz = 0;
            self.locked_max_mhz = 0;
            self.lock_epoch = nvml::session_epoch();
            return Ok(());
        }

        let lo = MIN_LOCKABLE_MHZ.max(if min_mhz > 0 { min_mhz } else { MIN_LOCKABLE_MHZ });
        let mut hi = if max_mhz > 0 { max_mhz } else { min_mhz };
        if hi < lo {
            hi = lo;
        }
        if let Some(e) = nvml::lock_graphics_clocks(nvml_index, lo, hi) {
            roch_core::bail!("Failed to pin the clock to {lo}-{hi} MHz: {e}");
        }
        self.locked_min_mhz = lo;
        self.locked_max_mhz = hi;
        self.lock_epoch = nvml::session_epoch();
        // The voltage cap's NVML fallback writes the same lock. This one is explicit, so it wins; the
        // cap has the NVAPI boost lock to fall back on and will say so if it cannot use it.
        self.nvml_clock_cap_mhz = 0;
        self.nvml_cap_voltage_mv = 0;
        Ok(())
    }

    /// Pin the memory clock (NVML) inside [min, max]; equal ends hold one figure, as mining tools do.
    /// 0/0 releases it.
    pub(super) fn set_memory_lock_impl(&mut self, gpu: usize, min_mhz: i32, max_mhz: i32) -> Result<()> {
        let nvml_index = self.nvml_index_for(gpu);
        if min_mhz <= 0 && max_mhz <= 0 {
            // Always asked, like the graphics clock: the lock lives in the driver and outlives us.
            if let Some(e) = nvml::reset_memory_clocks(nvml_index) {
                roch_core::bail!("Failed to release the memory clock: {e}");
            }
            return Ok(());
        }
        let hi = if max_mhz > 0 { max_mhz } else { min_mhz };
        let lo = if min_mhz > 0 { min_mhz.min(hi) } else { hi };
        if let Some(e) = nvml::lock_memory_clocks(nvml_index, lo, hi) {
            roch_core::bail!("Failed to pin the memory clock to {lo}-{hi} MHz: {e}");
        }
        Ok(())
    }

    /// Offset a private clock domain (crossbar, SYS, video). Verified by the private layer reading
    /// the offset back; the crossbar was also confirmed against the GPU's own frequency counter
    /// (+100 MHz moved it 2414 → 2514 MHz with the core unchanged).
    pub(super) fn set_domain(&mut self, gpu: usize, slot: i32, offset_mhz: i32, name: &str) -> Result<()> {
        let status = private::write_domain_offset_mhz(self.gpu(gpu)?, slot, offset_mhz);
        if status == 0 {
            return Ok(());
        }
        let detail = match status {
            domain_offset::INVALID_REQUEST => "Invalid domain or offset; no write attempted.",
            domain_offset::INVALID_LAYOUT => "The driver returned an unrecognized control layout.",
            domain_offset::VERIFICATION_FAILED => "The requested offset did not read back after the write.",
            _ => "The driver operation failed; a reported range does not establish write support.",
        };
        roch_core::bail!(
            "Failed to set the {name} clock offset to {} MHz: status {status}. {detail}",
            signed(offset_mhz)
        )
    }

    /// Core/memory offset: a minimal PSTATES20 V1 payload — one pstate (P0), one clock entry, no
    /// voltages.
    pub(super) fn set_clock_delta(&mut self, gpu: usize, domain: u32, offset_mhz: i32) -> Result<()> {
        let g = self.gpu(gpu)?;
        let mut info = nvapi::new_versioned::<PStates20InfoV1>(1);
        info.num_pstates = 1;
        info.num_clocks = 1;
        info.num_base_voltages = 0;
        info.pstates[0].id = 0;
        info.pstates[0].clocks[0] = PStates20ClockEntry {
            domain_id: domain,
            freq_delta_khz: PStates20Delta { value: offset_mhz.wrapping_mul(1000), min: 0, max: 0 },
            ..PStates20ClockEntry::default()
        };
        match nvapi::set_pstates20_v1(g, &mut info) {
            Ok(()) => Ok(()),
            Err(e @ NvError::Api(_)) => Err(GpuError::new(format!(
                "Failed to set {} clock offset to {} MHz: {}. Run as administrator and make sure no other OC tool is \
                 fighting for control.",
                nvapi::clock_domain_name(domain),
                signed(offset_mhz),
                e.status_text()
            ))),
            Err(e) => Err(e.into()),
        }
    }

    pub(super) fn set_power_limit_impl(&mut self, gpu: usize, percent: i32) -> Result<()> {
        let g = self.gpu(gpu)?;

        // Clamp to the PCM range the driver itself reported rather than trusting percent × 1000:
        // some cards report a min/max that doesn't land on a round percentage.
        let mut pcm = percent.wrapping_mul(1000) as u32;
        if let Ok(info) = nvapi::power_policies_info(g)
            && let Some(i) = info.first()
            && i.min_pcm <= i.max_pcm
        {
            pcm = pcm.clamp(i.min_pcm, i.max_pcm);
        }

        let current = match nvapi::power_policies_status(g) {
            Ok(c) => c,
            Err(e @ NvError::Api(_)) => roch_core::bail!("Cannot read power policy: {}.", e.status_text()),
            Err(e) => return Err(e.into()),
        };
        if current.is_empty() {
            roch_core::bail!("GPU reports no power policy entries.");
        }

        // Payload shapes seen in the wild; different driver branches accept different ones.
        let shape = |i: i32| -> Vec<PowerPolicyStatusEntry> {
            let fresh = PowerPolicyStatusEntry { pstate: 0, unknown1: 0, target_pcm: pcm, unknown2: 0 };
            match i {
                // 0: one fresh entry — pstate 0, unknown fields zeroed. What nvapioc / Afterburner send.
                0 => vec![fresh],
                // 1: the driver's own entry 0, only the target rewritten (keeps pstate + unknowns).
                1 => vec![PowerPolicyStatusEntry { target_pcm: pcm, ..current[0] }],
                // 2: every entry back, the target rewritten in all of them.
                2 => current.iter().map(|e| PowerPolicyStatusEntry { target_pcm: pcm, ..*e }).collect(),
                // 3: fresh entry pinned to P0 with the leading unknown set to 1 (some branches treat
                //    it as a "this field is valid" flag rather than padding).
                _ => vec![PowerPolicyStatusEntry { pstate: 0, unknown1: 1, ..fresh }],
            }
        };
        const STRATEGIES: i32 = 4;

        // If we already know which shape works, use it directly.
        if (0..STRATEGIES).contains(&self.power_strategy) {
            match nvapi::set_power_policies_status(g, &shape(self.power_strategy)) {
                Ok(()) => return Ok(()),
                Err(NvError::Api(_)) => self.power_strategy = -1, // the driver changed its mind; re-probe
                Err(e) => return Err(e.into()),
            }
        }

        let mut failures = Vec::new();
        for i in 0..STRATEGIES {
            match nvapi::set_power_policies_status(g, &shape(i)) {
                Ok(()) => {
                    self.power_strategy = i;
                    return Ok(());
                }
                Err(e @ NvError::Api(_)) => failures.push(format!("#{i}:{}", e.status_text())),
                Err(e) => return Err(e.into()),
            }
        }
        roch_core::bail!(
            "Failed to set power limit to {percent}% ({pcm} PCM) — the driver rejected every payload shape ({}). Run \
             'gputuner-cli diag' and send the output.",
            failures.join(", ")
        )
    }

    pub(super) fn set_voltage_boost_impl(&mut self, gpu: usize, percent: i32) -> Result<()> {
        let g = self.gpu(gpu)?;
        match nvapi::set_core_voltage_boost_percent(g, clamp_i32(percent, 0, 100) as u32) {
            Ok(()) => Ok(()),
            Err(e @ NvError::Api(_)) => {
                Err(GpuError::new(format!("Failed to set voltage boost to {percent}%: {}.", e.status_text())))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub(super) fn set_temp_limit_impl(&mut self, gpu: usize, celsius: i32) -> Result<()> {
        let g = self.gpu(gpu)?;
        let fail = |e: NvError| -> GpuError {
            match e {
                NvError::Api(_) => {
                    GpuError::new(format!("Failed to set temperature limit to {celsius} °C: {}.", e.status_text()))
                }
                e => e.into(),
            }
        };
        let entries = nvapi::thermal_policies_status(g).map_err(fail)?;
        let Some(first) = entries.first() else { roch_core::bail!("GPU reports no thermal policy entries.") };
        // The same controller and pstate, the new target (°C × 256).
        let updated = nvapi::ThermalPolicyStatusEntry {
            controller: first.controller,
            target: celsius.wrapping_mul(256),
            pstate: first.pstate,
        };
        nvapi::set_thermal_policies_status(g, &[updated]).map_err(fail)
    }

    pub(super) fn write_ocp(&mut self, gpu: usize, slot: i32, milliamps: i32, rail: &str) -> Result<()> {
        if let Some(e) = private::write_ocp_limit(self.gpu(gpu)?, slot, milliamps) {
            roch_core::bail!("Failed to set the {rail} OCP limit to {} A: {e}.", two_dp(milliamps as f64 / 1000.0));
        }
        Ok(())
    }

    /// `fan_index` -1 = every fan, else the cooler id (1-based on the fan-coolers family).
    pub(super) fn set_fan_speed_impl(&mut self, gpu: usize, fan_index: i32, percent: i32) -> Result<()> {
        let g = self.gpu(gpu)?;
        let wrap = |e: &NvError| format!("Failed to set fan speed to {percent}%: {}.", e.status_text());
        let coolers = match wrapper::coolers(g) {
            Ok(c) => c,
            Err(e) => return Err(cooler_error(e.into(), wrap)),
        };
        if coolers.is_empty() {
            roch_core::bail!("No controllable fans reported by NVAPI.");
        }
        for c in coolers {
            if fan_index >= 0 && c.cooler_id != fan_index {
                continue;
            }
            let hi = if c.default_max_level <= 0 { 100 } else { c.default_max_level };
            let lvl = clamp_i32(percent, c.default_min_level, hi);
            wrapper::set_cooler_settings(g, c.cooler_id, wrapper::POLICY_MANUAL, Some(lvl))
                .map_err(|e| cooler_error(e, wrap))?;
        }
        Ok(())
    }

    pub(super) fn set_fan_auto_impl(&mut self, gpu: usize) -> Result<()> {
        let g = self.gpu(gpu)?;
        match wrapper::restore_coolers_to_default(g) {
            Ok(()) => Ok(()),
            // Some RTX cards reject the legacy restore; fall back to per-cooler Auto policy.
            Err(NvError::Api(_)) => {
                let wrap = |e: &NvError| format!("Failed to return fans to automatic control: {}.", e.status_text());
                let coolers = wrapper::coolers(g).map_err(|e| cooler_error(e.into(), wrap))?;
                for c in coolers {
                    wrapper::set_cooler_settings(g, c.cooler_id, wrapper::POLICY_NONE, None)
                        .map_err(|e| cooler_error(e, wrap))?;
                }
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    pub(super) fn reset_to_defaults_impl(&mut self, gpu: usize) -> Result<()> {
        // The rail ceiling survives a driver-level reset, so the private domains are cleared
        // explicitly. The crossbar/SYS/video offsets have a real default of zero. The voltage rails
        // do not: the driver reports no factory value and this layer cannot tell an untouched rail
        // from one a previous run raised, so TuningService restores those from the figure recorded
        // on first sight.
        let _ = self.set_domain(gpu, private::SLOT_XBAR, 0, "XBAR");
        let _ = self.set_domain(gpu, private::SLOT_SYS, 0, "SYS");
        let _ = self.set_domain(gpu, private::SLOT_VIDEO, 0, "video");

        let caps = self.capabilities_impl(gpu)?;
        let mut errors: Vec<String> = Vec::new();
        let mut attempt = |r: Result<()>| {
            if let Err(e) = r {
                errors.push(e.message);
            }
        };

        if caps.can_set_core_offset {
            attempt(self.set_clock_delta(gpu, DOMAIN_GRAPHICS, 0));
        }
        if caps.can_set_memory_offset {
            attempt(self.set_clock_delta(gpu, DOMAIN_MEMORY, 0));
        }
        if caps.can_set_power_limit {
            attempt(self.set_power_limit_impl(gpu, caps.power_limit_default_percent));
        }
        if caps.can_set_temp_limit {
            attempt(self.set_temp_limit_impl(gpu, caps.temp_limit_default_c));
        }
        if caps.can_set_voltage_boost {
            attempt(self.set_voltage_boost_impl(gpu, 0));
        }
        if caps.can_set_voltage_curve {
            attempt(self.set_voltage_curve_offset_impl(gpu, 0, 0)); // clears the whole delta table
        }
        // Both cap mechanisms unconditionally: an NVML clock lock outlives the process that set it,
        // so a fresh launch has to be able to clear one it doesn't remember applying.
        let boost = self.gpu(gpu).and_then(|g| self.try_write_boost_lock(g, 0, 0));
        attempt(boost);
        let nvml_err = nvml::reset_graphics_clocks(self.nvml_index_for(gpu));
        // A memory clock lock outlives the process too. Not reported: a card that cannot lock its
        // memory refuses the reset as well, and the service's disarmed write reports a real failure.
        if caps.can_lock_memory_clock {
            let _ = nvml::reset_memory_clocks(self.nvml_index_for(gpu));
        }
        self.nvml_clock_cap_mhz = 0;
        self.nvml_cap_voltage_mv = 0;
        self.voltage_lock_mechanism = "none".into();
        // NVML is optional: only complain if it is actually there and still refused.
        if let Some(e) = nvml_err
            && nvml::is_available()
        {
            attempt(Err(GpuError::new(format!("Clock cap: {e}"))));
        }
        if caps.can_set_fan_speed {
            attempt(self.set_fan_auto_impl(gpu));
        }

        if !errors.is_empty() {
            roch_core::bail!("Reset partially failed: {}", errors.join(" | "));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_formats() {
        assert_eq!(signed(150), "+150");
        assert_eq!(signed(-50), "-50");
        assert_eq!(signed(0), "0");
        assert_eq!(two_dp(300.0), "300");
        assert_eq!(two_dp(312.5), "312.5");
        assert_eq!(two_dp(1.005), "1");
        assert_eq!(two_dp(0.125), "0.13");
    }
}
