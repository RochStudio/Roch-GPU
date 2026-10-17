//! `GetCapabilities`. Read-only: every call here is a getter (PStates20, policy info, coolers,
//! voltage, boost percent, the curve reads, rails, crossbar/domain info, OCP, NVML clock bounds).

use std::collections::HashMap;

use roch_core::util::{clamp_i32, round_even_i32};
use roch_core::{GpuCapabilities, Result};

use super::{MIN_LOCKABLE_MHZ, NvidiaBackend, VOLTAGE_BOOST_HEADROOM_MV, VOLTAGE_RAIL_HEADROOM_MV};
use crate::nvapi::{self, NvError};
use crate::private::{self, VoltRail};
use crate::vf_curve;
use crate::wrapper;

/// PerformanceStateId.P0_3DPerformance and the PublicClockDomain ids read here.
pub(crate) const PSTATE_P0: u32 = 0;
pub(crate) const DOMAIN_GRAPHICS: u32 = 0;
pub(crate) const DOMAIN_MEMORY: u32 = 4;

/// One voltage rail's ranges as the UI needs them. A rail the card does not expose stays at the
/// default, whose `supported` is false, and the control hides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct RailCaps {
    pub supported: bool,
    pub min_mv: i32,
    pub max_mv: i32,
    pub stock_mv: i32,
    pub floor_min_mv: i32,
    pub floor_max_mv: i32,
    pub floor_stock_mv: i32,
}

/// The rail ranges from the rail family's own report. The reported ceiling is what the rail is
/// CONFIGURED at; subtracting the offset gives the base the hardware counts from. MSVDD ships with a
/// -50 mV offset already applied, so its configured ceiling is 985 mV against a 1035 mV base — the
/// same base as NVVDD. Calling the base "stock" made the UI claim 1035 and made Reset write offset
/// 0, raising the rail 50 mV ABOVE where the card left the factory. The floor reads the same way;
/// its travel is capped at the same headroom — enough for stability work, short of pinning the rail
/// near its top where the card would sit at high voltage even at idle.
pub(crate) fn rail_caps(rails: &[VoltRail]) -> HashMap<i32, RailCaps> {
    let mut out = HashMap::new();
    for rail in rails {
        if rail.max_uv == 0 {
            continue;
        }
        let configured = rail.max_uv as i32 / 1000;
        let hw_base = (rail.max_uv as i32).wrapping_sub(rail.max_offset_uv) / 1000;
        let min = rail.min_uv as i32 / 1000;
        let max = hw_base + VOLTAGE_RAIL_HEADROOM_MV;
        let floor_base = (rail.min_uv as i32).wrapping_sub(rail.min_offset_uv) / 1000;
        out.insert(
            rail.index,
            RailCaps {
                supported: hw_base > 0 && max > min,
                min_mv: min,
                max_mv: max,
                stock_mv: configured,
                floor_min_mv: floor_base,
                floor_max_mv: floor_base + VOLTAGE_RAIL_HEADROOM_MV,
                floor_stock_mv: min,
            },
        );
    }
    out
}

/// Depth is measured down from the curve's own top, so cap it relative to that rather than a flat
/// -300 mV which could drop below the bottom of the curve entirely.
pub(crate) fn voltage_offset_min_mv(stock_max_mv: i32) -> i32 {
    if stock_max_mv > 0 { -clamp_i32(stock_max_mv - 700, 50, 300) } else { -300 }
}

impl NvidiaBackend {
    pub(super) fn capabilities_impl(&mut self, gpu: usize) -> Result<GpuCapabilities> {
        if let Some(c) = self.caps_cache.get(&gpu) {
            return Ok(c.clone());
        }
        let g = self.gpu(gpu)?;

        // Clock offset ranges from PStates20 (P0).
        let (mut can_core, mut can_mem) = (false, false);
        let (mut core_min, mut core_max, mut mem_min, mut mem_max) = (-500, 500, -1000, 1500);
        if let Ok(ps) = nvapi::pstates20(g)
            && let Some(clocks) = ps.clocks_for(PSTATE_P0)
        {
            for c in clocks {
                let r = c.freq_delta_khz;
                if c.domain_id == DOMAIN_GRAPHICS {
                    can_core = ps.is_editable && c.editable();
                    core_min = r.min / 1000;
                    core_max = r.max / 1000;
                } else if c.domain_id == DOMAIN_MEMORY {
                    can_mem = ps.is_editable && c.editable();
                    // The driver's own maximum. This used to be forced up to +4000 on the theory
                    // that 50-series cards under-report; a 5070 Ti (610.88) reports +3000 and
                    // rejects anything above it with NotSupported rather than clamping.
                    mem_min = r.min / 1000;
                    mem_max = r.max / 1000;
                }
            }
        }

        // Power limit: a policy entry exists, so the limit is settable.
        let (mut can_power, mut p_min, mut p_max, mut p_def) = (false, 100, 100, 100);
        if let Ok(info) = nvapi::power_policies_info(g)
            && let Some(i) = info.first()
        {
            can_power = true;
            p_min = round_even_i32(wrapper::pcm_percent(i.min_pcm));
            p_max = round_even_i32(wrapper::pcm_percent(i.max_pcm));
            p_def = round_even_i32(wrapper::pcm_percent(i.default_pcm));
        }

        // Temp limit. Zeros when the card exposes no thermal policy — 65..90 with a default of 83
        // describes some other card, and the stock profile and clamp would carry it as this one's.
        let (mut can_temp, mut t_min, mut t_max, mut t_def) = (false, 0, 0, 0);
        if let Ok(info) = nvapi::thermal_policies_info(g)
            && let Some(i) = info.first()
        {
            can_temp = true;
            t_min = wrapper::thermal_info_c(i.min_c);
            t_max = wrapper::thermal_info_c(i.max_c);
            t_def = wrapper::thermal_info_c(i.default_c);
        }

        // Fans.
        let (mut can_fan, mut fan_count, mut f_min, mut f_max) = (false, 0, 0, 100);
        if let Ok(coolers) = wrapper::coolers(g) {
            fan_count = coolers.len() as i32;
            can_fan = fan_count > 0;
            if let (Some(lo), Some(hi)) =
                (coolers.iter().map(|c| c.default_min_level).min(), coolers.iter().map(|c| c.default_max_level).max())
            {
                f_min = lo;
                f_max = if hi <= 0 { 100 } else { hi };
            }
        }

        let can_volt = nvapi::current_voltage_uv(g).is_ok();
        // Voltage boost (Afterburner's "overvoltage" percent): read the current value to probe.
        let can_volt_boost = nvapi::core_voltage_boost_percent(g).is_ok();

        // V/F curve editing — the real undervolt path. Each call probed separately so a failure says
        // which one and why, instead of a silent "unavailable".
        let (mut can_curve, mut stock_max_mv, mut curve_reason) = (false, 0, String::new());
        if !private::curve_available() {
            curve_reason = "the driver does not export the V/F curve entry points".into();
        } else {
            match self.read_base_curve(gpu, g) {
                Ok(base) => {
                    let max_uv = vf_curve::max_voltage_uv(&base);
                    if max_uv > 0 {
                        can_curve = true;
                        stock_max_mv = (max_uv / 1000) as i32;
                    } else {
                        curve_reason =
                            format!("driver returned an empty curve (mask 0x{:08X})", self.curve_mask(gpu, g)[0]);
                    }
                }
                Err(NvError::Api(s)) => curve_reason = format!("NVAPI {}", nvapi::status_name(s)),
                Err(NvError::NotSupported(_)) => curve_reason = "entry point not exposed by this driver".into(),
            }
        }

        // Core voltage rail (NVVDD) and MSVDD: read rather than assumed. A card without the family
        // reports nothing and the controls hide.
        let rails = rail_caps(&private::read_volt_rails(g));
        let nvvdd = rails.get(&private::RAIL_NVVDD).copied().unwrap_or_default();
        let msvdd = rails.get(&private::RAIL_MSVDD).copied().unwrap_or_default();

        // Crossbar. Its own family reports the range, offered in full.
        let (mut can_xbar, mut xbar_min, mut xbar_max) = (false, 0, 0);
        let xi = private::read_xbar_info(g);
        if xi.supported {
            can_xbar = true;
            (xbar_min, xbar_max) = (xi.min_offset_mhz, xi.max_offset_mhz);
        }

        // SYS and video sit in the same info list as the crossbar, in their own slots. A slot that
        // reports a range is one the driver will discuss, and its range is offered in full.
        let (mut can_sys, mut can_video) = (false, false);
        let (mut sys_min, mut sys_max, mut vid_min, mut vid_max) = (0, 0, 0, 0);
        for d in private::read_domain_entries(g) {
            if !d.has_range {
                continue;
            }
            if d.index == private::SLOT_SYS {
                can_sys = true;
                (sys_min, sys_max) = (d.min_mhz, d.max_mhz);
            } else if d.index == private::SLOT_VIDEO {
                can_video = true;
                (vid_min, vid_max) = (d.min_mhz, d.max_mhz);
            }
        }

        // OCP. A card without the family reports no channels and the control hides. The driver's own
        // window for each where it offers one; otherwise zero and the caller derives one.
        let ocp = private::read_ocp_channels(g);
        let slot_value = |s: i32| ocp.iter().find(|c| c.slot == s).map(|c| c.value).unwrap_or(0);
        let ocp_nvvdd = slot_value(private::OCP_SLOT_NVVDD);
        let ocp_msvdd = slot_value(private::OCP_SLOT_MSVDD);
        let can_ocp = ocp_nvvdd > 0 && ocp_msvdd > 0;
        let ranges = private::read_ocp_ranges(g);
        let (nv_r, ms_r) = (private::range_for(&ranges, ocp_nvvdd), private::range_for(&ranges, ocp_msvdd));

        // Clock lock. NVML owns it; its reported maximum is the top of the range and its minimum the
        // floor (the 210 MHz fallback only when the driver reports none).
        let nvml_index = self.nvml_index_for(gpu);
        let max_lock = crate::nvml::max_graphics_clock_mhz(nvml_index);
        let min_lock = crate::nvml::min_graphics_clock_mhz(nvml_index);

        let caps = GpuCapabilities {
            can_set_core_offset: can_core,
            can_set_memory_offset: can_mem,
            can_set_power_limit: can_power,
            can_set_temp_limit: can_temp,
            can_set_fan_speed: can_fan,
            can_read_voltage: can_volt,
            can_set_voltage_boost: can_volt_boost,
            can_set_voltage_curve: can_curve,
            stock_max_voltage_mv: stock_max_mv,
            curve_unavailable_reason: curve_reason,
            voltage_offset_min_mv: voltage_offset_min_mv(stock_max_mv),
            voltage_offset_max_mv: 0,
            min_voltage_mv: 850,
            max_voltage_mv: if stock_max_mv > 0 { stock_max_mv + VOLTAGE_BOOST_HEADROOM_MV } else { 0 },
            // The driver's full reported travel (PStates20, P0). Earlier builds narrowed these to a
            // "practical" window; the whole range is offered now, and where a card fails inside it
            // is for the person tuning it to find.
            core_offset_min_mhz: core_min,
            core_offset_max_mhz: core_max,
            memory_offset_min_mhz: mem_min,
            memory_offset_max_mhz: mem_max,
            power_limit_min_percent: p_min,
            power_limit_max_percent: p_max,
            power_limit_default_percent: p_def,
            temp_limit_min_c: t_min,
            temp_limit_max_c: t_max,
            temp_limit_default_c: t_def,
            voltage_boost_min_percent: 0,
            voltage_boost_max_percent: 100,
            fan_min_percent: f_min,
            fan_max_percent: f_max,
            fan_count,
            can_set_voltage_rail: nvvdd.supported,
            voltage_rail_min_mv: nvvdd.min_mv,
            voltage_rail_max_mv: nvvdd.max_mv,
            voltage_rail_stock_max_mv: nvvdd.stock_mv,
            voltage_rail_floor_min_mv: nvvdd.floor_min_mv,
            voltage_rail_floor_max_mv: nvvdd.floor_max_mv,
            voltage_rail_stock_floor_mv: nvvdd.floor_stock_mv,
            can_set_msvdd_rail: msvdd.supported,
            can_set_ocp: can_ocp,
            nvvdd_ocp_stock_milliamps: ocp_nvvdd,
            msvdd_ocp_stock_milliamps: ocp_msvdd,
            nvvdd_ocp_min_milliamps: nv_r.map(|r| r.min_ma).unwrap_or(0),
            nvvdd_ocp_max_milliamps: nv_r.map(|r| r.max_ma).unwrap_or(0),
            msvdd_ocp_min_milliamps: ms_r.map(|r| r.min_ma).unwrap_or(0),
            msvdd_ocp_max_milliamps: ms_r.map(|r| r.max_ma).unwrap_or(0),
            msvdd_rail_min_mv: msvdd.min_mv,
            msvdd_rail_max_mv: msvdd.max_mv,
            msvdd_rail_stock_max_mv: msvdd.stock_mv,
            msvdd_rail_floor_min_mv: msvdd.floor_min_mv,
            msvdd_rail_floor_max_mv: msvdd.floor_max_mv,
            msvdd_rail_stock_floor_mv: msvdd.floor_stock_mv,
            can_lock_clocks: max_lock > 0,
            clock_lock_min_mhz: if min_lock > 0 { min_lock } else { MIN_LOCKABLE_MHZ },
            clock_lock_max_mhz: max_lock,
            can_set_xbar_offset: can_xbar,
            xbar_offset_min_mhz: xbar_min,
            xbar_offset_max_mhz: xbar_max,
            can_set_sys_offset: can_sys,
            sys_offset_min_mhz: sys_min,
            sys_offset_max_mhz: sys_max,
            can_set_video_offset: can_video,
            video_offset_min_mhz: vid_min,
            video_offset_max_mhz: vid_max,
            ..GpuCapabilities::default()
        };
        self.caps_cache.insert(gpu, caps.clone());
        Ok(caps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rail(index: i32, max_uv: u32, max_off: i32, min_uv: u32, min_off: i32) -> VoltRail {
        VoltRail { index, current_uv: 0, max_uv, min_uv, max_offset_uv: max_off, min_offset_uv: min_off }
    }

    #[test]
    fn rail_ranges_count_from_the_hardware_base() {
        // 5070 Ti: NVVDD configured 1035 mV with no offset; MSVDD 985 mV with -50 mV applied.
        let caps = rail_caps(&[rail(0, 1_035_000, 0, 800_000, 0), rail(1, 985_000, -50_000, 800_000, 0)]);
        let nv = caps[&0];
        assert!(nv.supported);
        assert_eq!((nv.min_mv, nv.max_mv, nv.stock_mv), (800, 1180, 1035));
        assert_eq!((nv.floor_min_mv, nv.floor_max_mv, nv.floor_stock_mv), (800, 945, 800));
        let ms = caps[&1];
        // Stock is the configured 985, not the 1035 base: Reset must not raise the rail 50 mV.
        assert_eq!((ms.min_mv, ms.max_mv, ms.stock_mv), (800, 1180, 985));
        assert!(rail_caps(&[rail(0, 0, 0, 0, 0)]).is_empty(), "a zero ceiling is no rail");
        assert!(!rail_caps(&[rail(0, 600_000, 0, 900_000, 0)])[&0].supported, "inverted window hides");
    }

    #[test]
    fn undervolt_depth_follows_the_curve_top() {
        assert_eq!(voltage_offset_min_mv(1240), -300);
        assert_eq!(voltage_offset_min_mv(900), -200);
        assert_eq!(voltage_offset_min_mv(720), -50);
        assert_eq!(voltage_offset_min_mv(0), -300);
    }
}
