//! The NvAPIWrapper aggregation objects `NvApiBackend` leaned on, rebuilt on the raw calls in
//! [`crate::nvapi`]: `GPUCoolerInformation` (cooler list, set, restore — the choice between the legacy
//! cooler family and the RTX 20+ "client fan coolers" family), `GPUUsageInformation`'s domain list,
//! and the `PerformanceControl` views. Error behaviour follows the wrapper: a family answering
//! `NotSupported` falls through to the next one; any other status propagates.

use crate::nvapi::{self, FanCoolerControlEntry, GpuHandle, NOT_SUPPORTED, NvError, NvResult};

/// CoolerPolicy.None / .Manual.
pub const POLICY_NONE: u32 = 0;
pub const POLICY_MANUAL: u32 = 1;
/// FanCoolersControlMode.Auto / .Manual.
const MODE_AUTO: u32 = 0;
const MODE_MANUAL: u32 = 1;

/// `GPUCooler`: the fields the backend reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cooler {
    pub cooler_id: i32,
    pub current_level: i32,
    pub default_min_level: i32,
    pub default_max_level: i32,
    /// CoolerPolicy flags; the client-fan-coolers family only ever reports None or Manual.
    pub current_policy: u32,
    /// -1 when the legacy family's tach reading failed.
    pub current_rpm: i32,
}

fn is_not_supported_status(e: &NvError) -> bool {
    matches!(e, NvError::Api(s) if *s == NOT_SUPPORTED)
}

/// `GPUCoolerInformation.Coolers`.
///
/// The legacy family first (index = position, the first cooler carrying the tach reading), then the
/// client fan coolers (id from the driver). Both answering NotSupported is an `NvError::Api`
/// NotSupported, as the wrapper threw. Deviation: the wrapper threw `ArgumentException` when the
/// three fan-cooler lists disagreed on a cooler id; that surfaces here as a NotSupported error
/// instead, which every caller already treats as "no fans" rather than crashing.
pub fn coolers(h: GpuHandle) -> NvResult<Vec<Cooler>> {
    match nvapi::cooler_settings(h) {
        Ok(settings) => {
            let mut out = Vec::with_capacity(settings.len());
            for (i, s) in settings.iter().enumerate() {
                let mut rpm = -1;
                if i == 0 {
                    match nvapi::tach_reading(h) {
                        Ok(v) => rpm = v as i32,
                        Err(NvError::Api(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
                out.push(Cooler {
                    cooler_id: i as i32,
                    current_level: s.current_level as i32,
                    default_min_level: s.default_min as i32,
                    default_max_level: s.default_max as i32,
                    current_policy: s.current_policy,
                    current_rpm: if i == 0 && rpm >= 0 { rpm } else { -1 },
                });
            }
            return Ok(out);
        }
        Err(e) if is_not_supported_status(&e) => {}
        Err(e) => return Err(e),
    }

    let fetch = || -> NvResult<Option<(Vec<_>, Vec<_>, Vec<FanCoolerControlEntry>)>> {
        let status = match nvapi::fan_coolers_status(h) {
            Ok(v) => v,
            Err(e) if is_not_supported_status(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        let info = match nvapi::fan_coolers_info(h) {
            Ok(v) => v,
            Err(e) if is_not_supported_status(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        let control = match nvapi::fan_coolers_control(h) {
            Ok(v) => v.0,
            Err(e) if is_not_supported_status(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(Some((status, info, control)))
    };
    let Some((status, info, control)) = fetch()? else { return Err(NvError::Api(NOT_SUPPORTED)) };

    let mut out = Vec::new();
    for (i, st) in status.iter().enumerate() {
        let (Some(inf), Some(ctl)) = (info.get(i), control.get(i)) else { continue };
        if inf.cooler_id != st.cooler_id || st.cooler_id != ctl.cooler_id {
            return Err(NvError::NotSupported("Passed arguments are meant to be for different coolers."));
        }
        out.push(Cooler {
            cooler_id: st.cooler_id as i32,
            current_level: st.current_level as i32,
            default_min_level: st.current_min as i32,
            default_max_level: st.current_max as i32,
            current_policy: if ctl.control_mode == MODE_MANUAL { POLICY_MANUAL } else { POLICY_NONE },
            current_rpm: st.current_rpm as i32,
        });
    }
    Ok(out)
}

/// Why a wrapper-level cooler write failed: a driver error, or a cooler id the card does not have
/// (`ArgumentException` in the wrapper).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoolerWriteError {
    Nv(NvError),
    InvalidCooler,
}

impl From<NvError> for CoolerWriteError {
    fn from(e: NvError) -> Self {
        CoolerWriteError::Nv(e)
    }
}

impl CoolerWriteError {
    pub const INVALID_COOLER: &'static str = "Invalid cooler identification number provided.";
}

/// `GPUCoolerInformation.SetCoolerSettings(coolerId, policy[, level])`. `level` is `None` for the
/// policy-only overload (which sends level 0 and, on the fan-coolers path, level 0 too).
pub fn set_cooler_settings(
    h: GpuHandle,
    cooler_id: i32,
    policy: u32,
    level: Option<i32>,
) -> Result<(), CoolerWriteError> {
    if !coolers(h)?.iter().any(|c| c.cooler_id == cooler_id) {
        return Err(CoolerWriteError::InvalidCooler);
    }
    let lvl = level.unwrap_or(0).max(0) as u32;
    match nvapi::set_cooler_level(h, cooler_id as u32, policy, lvl) {
        Ok(()) => return Ok(()),
        Err(e) if is_not_supported_status(&e) => {}
        Err(e) => return Err(e.into()),
    }
    let (entries, unknown) = nvapi::fan_coolers_control(h)?;
    let manual = policy == POLICY_MANUAL;
    let updated: Vec<FanCoolerControlEntry> = entries
        .iter()
        .map(|e| {
            if e.cooler_id as i32 == cooler_id {
                FanCoolerControlEntry {
                    cooler_id: e.cooler_id,
                    level: if manual && level.is_some() { lvl } else { 0 },
                    control_mode: if manual { MODE_MANUAL } else { MODE_AUTO },
                    reserved: [0; 8],
                }
            } else {
                *e
            }
        })
        .collect();
    nvapi::set_fan_coolers_control(h, &updated, unknown)?;
    Ok(())
}

/// `GPUCoolerInformation.RestoreCoolerSettingsToDefault()`: the legacy restore for every cooler,
/// else every fan-cooler entry back to Auto.
pub fn restore_coolers_to_default(h: GpuHandle) -> NvResult<()> {
    let ids: Vec<i32> = coolers(h)?.iter().map(|c| c.cooler_id).collect();
    let raw: Vec<u32> = ids.iter().map(|&i| i as u32).collect();
    match nvapi::restore_cooler_settings(h, &raw) {
        Ok(()) => return Ok(()),
        Err(e) if is_not_supported_status(&e) => {}
        Err(e) => return Err(e),
    }
    let (entries, unknown) = nvapi::fan_coolers_control(h)?;
    let updated: Vec<FanCoolerControlEntry> = entries
        .iter()
        .map(|e| {
            if ids.contains(&(e.cooler_id as i32)) {
                FanCoolerControlEntry { cooler_id: e.cooler_id, level: 0, control_mode: MODE_AUTO, reserved: [0; 8] }
            } else {
                *e
            }
        })
        .collect();
    nvapi::set_fan_coolers_control(h, &updated, unknown)
}

/// UtilizationDomain values.
pub const UTIL_GPU: u32 = 0;
pub const UTIL_FRAME_BUFFER: u32 = 1;
pub const UTIL_VIDEO_ENGINE: u32 = 2;
pub const UTIL_BUS_INTERFACE: u32 = 3;

/// `GPUUsageInformation.UtilizationDomainsStatus`: `(domain, percent)` for each present, named
/// domain. DynamicPstatesInfoEx when it succeeds with dynamic pstates enabled, else GetUsages.
pub fn utilization_domains(h: GpuHandle) -> NvResult<Vec<(u32, i32)>> {
    if let Ok(d) = nvapi::dynamic_pstates(h)
        && d.flags & 1 != 0
    {
        return Ok(d
            .domains
            .iter()
            .enumerate()
            .filter(|(i, v)| *i as u32 <= UTIL_BUS_INTERFACE && v.is_present & 1 != 0)
            .map(|(i, v)| (i as u32, v.percentage as i32))
            .collect());
    }
    let u = nvapi::usages(h)?;
    Ok(u.entries
        .iter()
        .enumerate()
        .filter(|(i, v)| *i as u32 <= UTIL_BUS_INTERFACE && v.is_present > 0)
        .map(|(i, v)| (i as u32, v.percentage as i32))
        .collect())
}

/// The first `(domain, percent)` for `domain`, as `UsageInformation.GPU` & co.
pub fn domain_percent(domains: &[(u32, i32)], domain: u32) -> Option<i32> {
    domains.iter().find(|(d, _)| *d == domain).map(|(_, p)| *p)
}

/// `GPUPowerLimitInfo.*InPercent` / `GPUPowerLimitPolicy.PowerTargetInPercent`: PCM / 1000f, a
/// float in the wrapper — kept as f32 so `Math.Round` sees the same value.
pub fn pcm_percent(pcm: u32) -> f64 {
    (pcm as f32 / 1000f32) as f64
}

/// `GPUThermalLimitInfo` temperatures: the raw value shifted down by 8.
pub fn thermal_info_c(raw: i32) -> i32 {
    raw >> 8
}

#[cfg(test)]
mod tests {
    use super::*;
    use roch_core::util::round_even_i32;

    #[test]
    fn power_percent_rounding() {
        // 5070 Ti: 83333 / 100000 / 116667 PCM -> 83 / 100 / 117 %.
        assert_eq!(round_even_i32(pcm_percent(83_333)), 83);
        assert_eq!(round_even_i32(pcm_percent(100_000)), 100);
        assert_eq!(round_even_i32(pcm_percent(116_667)), 117);
        assert_eq!(thermal_info_c(83 << 8), 83);
    }

    #[test]
    fn domain_lookup() {
        let d = [(UTIL_GPU, 7), (UTIL_FRAME_BUFFER, 3)];
        assert_eq!(domain_percent(&d, UTIL_FRAME_BUFFER), Some(3));
        assert_eq!(domain_percent(&d, UTIL_BUS_INTERFACE), None);
    }
}
