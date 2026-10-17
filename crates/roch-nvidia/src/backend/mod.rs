//! `NvApiBackend` port: the NVIDIA backend on NVAPI (plus NVML for watts and clock locks).
//!
//! Everything goes through the user-mode nvapi64.dll — no kernel driver. Writes need elevation.
//! The calls are the private NVAPI ones Afterburner / Inspector use:
//!   core/mem offset → SetPStates20 with a delta on the Graphics/Memory domain of P0;
//!   power limit     → ClientPowerPoliciesSetStatus (per-cent-mille: 100000 = 100 %);
//!   temp limit      → SetThermalPoliciesStatus (°C × 256);
//!   fans            → SetCoolerLevels (legacy) falling back to ClientFanCoolersSetControl (RTX 20+);
//!   voltage cap     → SetClockBoostLock, proved by read-back, else an NVML clock cap.
//!
//! Split by concern: [`caps`] (GetCapabilities), [`telemetry`] (telemetry, tuning state, clock
//! domains), [`curve`] (V/F curve and voltage lock), [`set`] (the other writes and reset),
//! [`diagnostics`] (the bug-report dump).

mod caps;
mod curve;
mod diagnostics;
mod set;
mod telemetry;

use std::any::Any;
use std::collections::{BTreeMap, HashMap};

use roch_core::{
    FanCurve, GpuBackend, GpuCapabilities, GpuDevice, GpuError, GpuGraphicsInfo, GpuIdentity, GpuTelemetry,
    GpuTuningState, Result, VfCurveSample,
};

use crate::nvapi::{self, GpuHandle, NvError};
use crate::nvml;

/// An internal failure: either a driver error (NVIDIAApiException / NVIDIANotSupportedException,
/// which some callers translate into their own message) or an already-worded backend error
/// (GpuBackendException).
#[derive(Debug, Clone)]
pub(crate) enum Fail {
    Nv(NvError),
    Gpu(GpuError),
    /// Any other .NET exception the C# could surface in a diagnostics section: (type, message).
    Other(&'static str, String),
}

impl From<NvError> for Fail {
    fn from(e: NvError) -> Self {
        Fail::Nv(e)
    }
}

impl From<GpuError> for Fail {
    fn from(e: GpuError) -> Self {
        Fail::Gpu(e)
    }
}

impl From<Fail> for GpuError {
    fn from(f: Fail) -> Self {
        match f {
            Fail::Nv(e) => e.into(),
            Fail::Gpu(e) => e,
            Fail::Other(_, m) => GpuError::new(m),
        }
    }
}

impl Fail {
    /// The C# exception type name, for the diagnostics' `<Type: message>` lines.
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Fail::Nv(e) => e.type_name(),
            Fail::Gpu(_) => "GpuBackendException",
            Fail::Other(t, _) => t,
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Fail::Nv(e) => e.message(),
            Fail::Gpu(e) => e.message.clone(),
            Fail::Other(_, m) => m.clone(),
        }
    }
}

/// One private clock domain, as the CLI's `domains` verb lists it (the C# value tuple of
/// `ReadClockDomains`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClockDomain {
    pub slot: i32,
    pub type_: i32,
    pub has_range: bool,
    pub min_mhz: i32,
    pub max_mhz: i32,
    pub measured_mhz: i32,
    pub offset_mhz: i32,
}

/// Marketing chip cuts and ROP counts are not exposed consistently by the driver. The device id is
/// read from the card; an unknown id stays unknown instead of borrowing a neighbour's specs.
const NVIDIA_PARTS: [(u16, &str, i32); 5] = [
    (0x2684, "AD102-300", 176),
    (0x2704, "AD103-300", 112),
    (0x2782, "AD104-400", 80),
    (0x2786, "AD104-250", 64),
    (0x2C05, "GB203-300", 96),
];

/// How much extra core voltage 100 % boost buys. Measured at ~60 mV on a 4070 Ti (1090 mV stock
/// curve top → 1150 mV observed). NVAPI exposes no way to query it.
pub(crate) const VOLTAGE_BOOST_HEADROOM_MV: i32 = 60;

/// How far above its hardware base a voltage rail may be driven, in mV.
///
/// 145 puts a 5070 Ti's rails at 1200 mV against a 1055 mV base — the figure the rail status
/// struct carries in a field that reads a flat 1200000 µV on both rails and does not move when the
/// configured ceiling does. Inference from an unlabelled offset, not a documented maximum (the
/// driver has accepted 1280 mV before clamping). Deliberately more travel than air cooling can use:
/// the reason to offer it is water and LN2. Headroom over each card's own base, not an absolute.
pub(crate) const VOLTAGE_RAIL_HEADROOM_MV: i32 = 145;

/// Fallback floor for the clock range, used only when the driver reports none. A 5070 Ti reports
/// 180 MHz and accepts it, so this is what a card that answers nothing gets.
pub(crate) const MIN_LOCKABLE_MHZ: i32 = 210;

/// Highest domain type id worth asking the frequency counter about; 31 is the top one any card has
/// answered.
pub(crate) const MAX_DOMAIN_TYPE: i32 = 31;

/// The NVIDIA backend. See the module docs.
pub struct NvidiaBackend {
    gpus: Vec<GpuHandle>,
    devices: Vec<GpuDevice>,
    caps_cache: HashMap<usize, GpuCapabilities>,
    thermal_mask: HashMap<usize, u32>,
    initialized: bool,

    /// How the voltage cap is currently being held, for the UI and diagnostics.
    voltage_lock_mechanism: String,
    /// Clock cap applied through NVML, in MHz; 0 when none. Tracked because NVML has no getter.
    nvml_clock_cap_mhz: i32,
    nvml_cap_voltage_mv: i32,
    /// The NVAPI boost-lock recipe this driver honours, found by probing; -1 = not yet.
    lock_recipe: i32,
    /// Set once NVML has held a cap: this driver's private lock is unverifiable, so stop asking.
    prefer_nvml: bool,

    /// The clock window the user asked for, so the voltage cap knows not to fight it. Remembered
    /// rather than read back: NVML will not report a lock (GetClock answers NOT_SUPPORTED for every
    /// application-clock id, and the event reasons read zero either way — measured). A display
    /// driver reset clears the lock underneath us; `forget_lock_across_driver_reset` handles that.
    locked_min_mhz: i32,
    locked_max_mhz: i32,
    /// NVML session the window was written in; a change means the driver was reset.
    lock_epoch: i32,

    nvml_index: HashMap<usize, i32>,
    /// Which power-limit payload shape this driver accepts; -1 = not yet determined.
    power_strategy: i32,

    // V/F curve state. The C# kept one of each per backend; these are per GPU so a mixed multi-GPU
    // box does not reuse one card's mask on another (same values on a single GPU).
    curve_mask: HashMap<usize, [u32; 4]>,
    curve_layout: HashMap<usize, (usize, u32)>,
    /// The delta-write recipe (mask, entry flag) this driver honours, found by probing.
    write_recipe: HashMap<usize, ([u32; 4], Option<i32>)>,
    extra_domain_types: HashMap<usize, Vec<i32>>,
    /// EnableOverclockedPStates has been sent (once per backend, as the C#).
    oc_enabled: bool,
}

impl Default for NvidiaBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NvidiaBackend {
    pub fn new() -> Self {
        NvidiaBackend {
            gpus: Vec::new(),
            devices: Vec::new(),
            caps_cache: HashMap::new(),
            thermal_mask: HashMap::new(),
            initialized: false,
            voltage_lock_mechanism: "none".into(),
            nvml_clock_cap_mhz: 0,
            nvml_cap_voltage_mv: 0,
            lock_recipe: -1,
            prefer_nvml: false,
            locked_min_mhz: 0,
            locked_max_mhz: 0,
            lock_epoch: 0,
            nvml_index: HashMap::new(),
            power_strategy: -1,
            curve_mask: HashMap::new(),
            curve_layout: HashMap::new(),
            write_recipe: HashMap::new(),
            extra_domain_types: HashMap::new(),
            oc_enabled: false,
        }
    }

    /// `Initialize`: NVAPI, the physical GPUs, and each one's identity. Read-only. Idempotent —
    /// the factory initialises while probing and the tuning service initialises again.
    fn initialize_impl(&mut self) -> Result<()> {
        if !self.devices.is_empty() {
            return Ok(());
        }
        if nvapi::initialize().is_err() {
            return Err(GpuError::new(
                "NVAPI could not be initialised. Is the NVIDIA driver installed and is this a 64-bit process?",
            ));
        }
        let gpus = nvapi::enum_physical_gpus()?;
        if gpus.is_empty() {
            return Err(GpuError::new("NVAPI initialised but reported no NVIDIA GPUs."));
        }
        let driver =
            nvapi::driver_version().map(|v| format!("{}.{:02}", v / 100, v % 100)).unwrap_or_else(|_| "unknown".into());

        self.devices.clear();
        for (i, &g) in gpus.iter().enumerate() {
            let bus = match (nvapi::bus_id(g), nvapi::bus_slot_id(g)) {
                (Ok(b), Ok(s)) => format!("PCI bus {b}, slot {s}"),
                _ => "PCI".into(),
            };
            self.devices.push(GpuDevice {
                index: i,
                name: nvapi::full_name(g).unwrap_or_else(|_| "NVIDIA GPU".into()),
                vendor: "NVIDIA".into(),
                bus_id: bus,
                driver_version: driver.clone(),
                vram_megabytes: nvapi::memory_info(g).map(|m| m.dedicated_kb as i64 / 1024).unwrap_or(0),
                bios_version: nvapi::bios_version_string(g).unwrap_or_default(),
            });
        }
        self.gpus = gpus;
        self.initialized = true;
        Ok(())
    }

    /// `Gpu(index)`: the handle, or the C#'s "not initialised" / out-of-range failures.
    fn gpu(&self, index: usize) -> Result<GpuHandle> {
        if !self.initialized {
            return Err(GpuError::new("Backend not initialised."));
        }
        self.gpus.get(index).copied().ok_or_else(|| GpuError::new(format!("NVIDIA GPU index {index} is out of range.")))
    }

    fn device_name(&self, index: usize) -> String {
        self.devices.get(index).map(|d| d.name.clone()).unwrap_or_default()
    }

    /// NVML enumerates by PCI bus and NVAPI does not, so on a multi-GPU box the ordinals disagree.
    /// Match on the adapter name and only fall back to the ordinal when that finds nothing.
    fn nvml_index_for(&mut self, gpu: usize) -> i32 {
        if let Some(&cached) = self.nvml_index.get(&gpu) {
            return cached;
        }
        let mut resolved = gpu as i32;
        let want = self.device_name(gpu);
        for i in 0..8 {
            let Some(name) = nvml::device_name(i) else { break };
            if name.trim().eq_ignore_ascii_case(want.trim()) {
                resolved = i;
                break;
            }
        }
        self.nvml_index.insert(gpu, resolved);
        resolved
    }

    /// `ReadGraphicsInfo`.
    fn read_graphics_info_impl(&mut self, gpu: usize) -> Result<GpuGraphicsInfo> {
        let g = self.gpu(gpu)?;
        let device = self.devices.get(gpu).cloned().unwrap_or_default();
        let pci = nvapi::pci_identifiers_checked(g).ok().flatten();
        let device_id = pci.map(|p| p.external_device_id).unwrap_or(0);
        let part = NVIDIA_PARTS.iter().find(|p| p.0 == device_id);

        let cores = nvapi::gpu_core_count(g).map(|v| v as i32).unwrap_or(0);
        let mut rops = nvapi::rop_count(g).map(|v| v as i32).unwrap_or(0);
        if rops <= 0 {
            rops = part.map(|p| p.2).unwrap_or(0);
        }
        let tmus = if cores > 0 { cores / 32 } else { 0 };

        let code = match part {
            Some(p) => p.1.to_string(),
            None => nvapi::short_name(g).unwrap_or_default(),
        };
        let memory_type = nvapi::ram_type(g).unwrap_or(0);
        let memory_maker = nvapi::ram_maker(g).unwrap_or(0);
        let bus_width = nvapi::ram_bus_width(g).unwrap_or(0);
        // The C# passes the NVAPI ordinal here, not the NVML-matched index.
        let pcie = nvml::pcie_link(gpu as i32);
        let rebar = nvml::resizable_bar_enabled(gpu as i32);

        Ok(GpuGraphicsInfo {
            board_manufacturer: pci
                .map(|p| GpuIdentity::board_vendor((p.sub_system_id & 0xFFFF) as i32))
                .unwrap_or_default(),
            code_name: code.clone(),
            revision: pci.map(|p| format!("{:02X}", p.revision_id & 0xFF)).unwrap_or_default(),
            cores: if cores > 0 { cores.to_string() } else { String::new() },
            rops_tmus: if rops > 0 && tmus > 0 { format!("{rops} / {tmus}") } else { String::new() },
            technology: technology_for(&code).into(),
            memory_type: memory_type_label(memory_type),
            memory_vendor: memory_maker_label(memory_maker),
            bus_width: if bus_width > 0 { format!("{bus_width} bits") } else { String::new() },
            bus_interface: GpuIdentity::pcie_bus_interface(
                pcie.max_generation,
                pcie.max_width,
                pcie.current_generation,
                pcie.current_width,
            ),
            resizable_bar: match rebar {
                Some(true) => "Enabled".into(),
                Some(false) => "Disabled".into(),
                None => String::new(),
            },
            ..GpuGraphicsInfo::from_device(&device)
        })
    }

    /// Names for the domains the driver only numbers. Checked against a 5070 Ti rather than
    /// trusted: a comparison monitor's figures and ours agree to ~2 MHz on each, and type 20 tracks
    /// the core under load as an L2 clock should. Type 31 reads a flat zero and nobody names it —
    /// a number with no name is more honest than a name with no evidence.
    pub fn domain_name(type_: i32) -> String {
        domain_name(type_)
    }

    /// See [`power_rail_name`].
    pub fn power_rail_name(index: i32, total: i32) -> String {
        power_rail_name(index, total)
    }
}

/// See [`NvidiaBackend::domain_name`].
pub fn domain_name(type_: i32) -> String {
    match type_ {
        3 => "HUBCLK".into(),
        6 => "DISPCLK".into(),
        20 => "L2CLK".into(),
        22 => "Reference clock".into(),
        _ => format!("Domain type {type_}"),
    }
}

/// What each power-monitor channel is, by position. The driver gives no names; these come from
/// matching all six currents at once against a comparison monitor on the same card under the same
/// load (board total 25.75 A to its 26.375, PCIe 0.58 to 0.585, ...). The "Rail NNN" names are that
/// monitor's own labels, carrying the driver's identifier. Positional, so only applied to a card
/// reporting exactly this many channels; anything else is numbered plainly.
pub fn power_rail_name(index: i32, total: i32) -> String {
    const KNOWN: [&str; 7] =
        ["Board total", "PCIe slot 12 V", "Rail 218", "Rail 214", "NVVDD input", "Rail 212", "Misc0 input"];
    if total == KNOWN.len() as i32 && index >= 0 && (index as usize) < KNOWN.len() {
        KNOWN[index as usize].into()
    } else {
        format!("Rail {index}")
    }
}

fn technology_for(code: &str) -> &'static str {
    let v = code.to_uppercase();
    if v.starts_with("GB") || v.starts_with("AD") {
        "4 nm"
    } else if v.starts_with("GA") {
        "8 nm"
    } else if v.starts_with("TU") {
        "12 nm"
    } else {
        ""
    }
}

fn memory_type_label(code: u32) -> String {
    match code {
        15 => "GDDR6X".into(),
        16 => "GDDR7".into(),
        1..=10 => nvapi::memory_type_name(code),
        0 => String::new(),
        _ => format!("Type {code}"),
    }
}

fn memory_maker_label(code: u32) -> String {
    match code {
        1 => "Samsung".into(),
        6 => "SK hynix".into(),
        10 => "Micron".into(),
        0 => String::new(),
        _ => format!("Maker {code}"),
    }
}

impl Drop for NvidiaBackend {
    /// `Dispose`: unload NVAPI if we initialised it, and NVML if it was ever loaded. Does NOT
    /// release a clock cap — deliberately, so a cap survives closing the app like nvidia-smi's.
    fn drop(&mut self) {
        if self.initialized {
            let _ = nvapi::unload();
            self.initialized = false;
        }
        nvml::try_shutdown();
    }
}

impl GpuBackend for NvidiaBackend {
    fn backend_name(&self) -> String {
        "NVIDIA (NVAPI)".into()
    }
    fn initialize(&mut self) -> Result<()> {
        self.initialize_impl()
    }
    fn devices(&self) -> &[GpuDevice] {
        &self.devices
    }
    fn read_graphics_info(&mut self, gpu: usize) -> GpuGraphicsInfo {
        self.read_graphics_info_impl(gpu).unwrap_or_default()
    }
    fn capabilities(&mut self, gpu: usize) -> Result<GpuCapabilities> {
        self.capabilities_impl(gpu)
    }
    fn read_telemetry(&mut self, gpu: usize) -> Result<GpuTelemetry> {
        self.read_telemetry_impl(gpu)
    }
    fn read_temperature_only(&mut self, gpu: usize) -> Result<GpuTelemetry> {
        self.read_temperature_only_impl(gpu)
    }
    fn read_tuning_state(&mut self, gpu: usize) -> Result<GpuTuningState> {
        self.read_tuning_state_impl(gpu)
    }
    fn set_core_offset(&mut self, gpu: usize, offset_mhz: i32) -> Result<()> {
        self.set_clock_delta(gpu, set::DOMAIN_GRAPHICS, offset_mhz)
    }
    fn set_memory_offset(&mut self, gpu: usize, offset_mhz: i32) -> Result<()> {
        self.set_clock_delta(gpu, set::DOMAIN_MEMORY, offset_mhz)
    }
    fn set_power_limit(&mut self, gpu: usize, percent: i32) -> Result<()> {
        self.set_power_limit_impl(gpu, percent)
    }
    fn set_temp_limit(&mut self, gpu: usize, celsius: i32) -> Result<()> {
        self.set_temp_limit_impl(gpu, celsius)
    }
    fn set_voltage_boost(&mut self, gpu: usize, percent: i32) -> Result<()> {
        self.set_voltage_boost_impl(gpu, percent)
    }
    fn set_voltage_curve_offset(&mut self, gpu: usize, offset_mv: i32, extra_clock_mhz: i32) -> Result<()> {
        self.set_voltage_curve_offset_impl(gpu, offset_mv, extra_clock_mhz)
    }
    fn set_voltage_lock(&mut self, gpu: usize, target_mv: i32) -> Result<()> {
        self.set_voltage_lock_impl(gpu, target_mv)
    }
    fn read_voltage_lock_mv(&mut self, gpu: usize) -> i32 {
        self.read_voltage_lock_mv_impl(gpu)
    }
    fn voltage_lock_mechanism(&self) -> String {
        self.voltage_lock_mechanism.clone()
    }
    fn set_voltage_rail_max(&mut self, gpu: usize, millivolts: i32) -> Result<()> {
        self.set_rail_end(gpu, crate::private::RAIL_NVVDD, millivolts, set::NVVDD, false)
    }
    fn set_msvdd_rail_max(&mut self, gpu: usize, millivolts: i32) -> Result<()> {
        self.set_rail_end(gpu, crate::private::RAIL_MSVDD, millivolts, set::MSVDD, false)
    }
    fn set_voltage_rail_floor(&mut self, gpu: usize, millivolts: i32) -> Result<()> {
        self.set_rail_end(gpu, crate::private::RAIL_NVVDD, millivolts, set::NVVDD, true)
    }
    fn set_msvdd_rail_floor(&mut self, gpu: usize, millivolts: i32) -> Result<()> {
        self.set_rail_end(gpu, crate::private::RAIL_MSVDD, millivolts, set::MSVDD, true)
    }
    fn set_xbar_offset(&mut self, gpu: usize, offset_mhz: i32) -> Result<()> {
        self.set_domain(gpu, crate::private::SLOT_XBAR, offset_mhz, "XBAR")
    }
    fn set_sys_offset(&mut self, gpu: usize, offset_mhz: i32) -> Result<()> {
        self.set_domain(gpu, crate::private::SLOT_SYS, offset_mhz, "SYS")
    }
    fn set_video_offset(&mut self, gpu: usize, offset_mhz: i32) -> Result<()> {
        self.set_domain(gpu, crate::private::SLOT_VIDEO, offset_mhz, "video")
    }
    fn set_clock_range(&mut self, gpu: usize, min_mhz: i32, max_mhz: i32) -> Result<()> {
        self.set_clock_range_impl(gpu, min_mhz, max_mhz)
    }
    /// The NVML clock cap standing in for an ignored voltage lock (see `set_voltage_lock_impl`).
    fn voltage_cap_clock_mhz(&self, _gpu: usize) -> i32 {
        self.nvml_clock_cap_mhz.max(0)
    }
    /// NVML reopens its session after a display-driver reset (`nvml::session_epoch`).
    fn driver_reset_count(&self) -> u64 {
        nvml::session_epoch().max(0) as u64
    }
    fn set_nvvdd_ocp_milliamps(&mut self, gpu: usize, milliamps: i32) -> Result<()> {
        self.write_ocp(gpu, crate::private::OCP_SLOT_NVVDD, milliamps, "NVVDD")
    }
    fn set_msvdd_ocp_milliamps(&mut self, gpu: usize, milliamps: i32) -> Result<()> {
        self.write_ocp(gpu, crate::private::OCP_SLOT_MSVDD, milliamps, "MSVDD")
    }
    fn measure_extra_clocks(&mut self, gpu: usize) -> BTreeMap<String, f64> {
        self.measure_extra_clocks_impl(gpu)
    }
    fn set_fan_speed(&mut self, gpu: usize, fan_index: i32, percent: i32) -> Result<()> {
        self.set_fan_speed_impl(gpu, fan_index, percent)
    }
    fn set_fan_auto(&mut self, gpu: usize) -> Result<()> {
        self.set_fan_auto_impl(gpu)
    }
    fn set_fan_curve(&mut self, _gpu: usize, _curve: &FanCurve) -> Result<()> {
        Err(GpuError::new("This backend has no hardware fan curve."))
    }
    fn reset_to_defaults(&mut self, gpu: usize) -> Result<()> {
        self.reset_to_defaults_impl(gpu)
    }
    fn diagnostics(&mut self, gpu: usize) -> String {
        self.diagnostics_impl(gpu)
    }
    fn read_vf_curve(&mut self, gpu: usize) -> Result<Vec<VfCurveSample>> {
        self.read_vf_curve_impl(gpu)
    }
    fn set_vf_curve_targets(&mut self, gpu: usize, targets: &[VfCurveSample]) -> Result<()> {
        self.set_vf_curve_targets_impl(gpu, targets)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(domain_name(20), "L2CLK");
        assert_eq!(domain_name(22), "Reference clock");
        assert_eq!(domain_name(31), "Domain type 31");
        assert_eq!(power_rail_name(0, 7), "Board total");
        assert_eq!(power_rail_name(6, 7), "Misc0 input");
        assert_eq!(power_rail_name(2, 6), "Rail 2", "names are positional for exactly seven channels");
        assert_eq!(power_rail_name(-1, 7), "Rail -1");
    }

    #[test]
    fn identity_labels() {
        assert_eq!(technology_for("gb203-300"), "4 nm");
        assert_eq!(technology_for("GA102"), "8 nm");
        assert_eq!(technology_for("TU104"), "12 nm");
        assert_eq!(technology_for(""), "");
        assert_eq!(memory_type_label(16), "GDDR7");
        assert_eq!(memory_type_label(15), "GDDR6X");
        assert_eq!(memory_type_label(8), "GDDR5");
        assert_eq!(memory_type_label(12), "Type 12");
        assert_eq!(memory_type_label(0), "");
        assert_eq!(memory_maker_label(6), "SK hynix");
        assert_eq!(memory_maker_label(3), "Maker 3");
        assert_eq!(memory_maker_label(0), "");
        assert!(NVIDIA_PARTS.iter().any(|p| p.0 == 0x2C05 && p.1 == "GB203-300" && p.2 == 96));
    }

    #[test]
    fn uninitialised_backend_refuses_politely() {
        let mut b = NvidiaBackend::new();
        assert!(b.devices().is_empty());
        assert_eq!(b.capabilities(0).unwrap_err().message, "Backend not initialised.");
        assert!(b.read_temperature_only(0).is_err());
        assert_eq!(b.read_voltage_lock_mv(0), 0);
        assert_eq!(b.voltage_lock_mechanism(), "none");
        assert!(b.read_clock_domains(0).is_empty());
        assert!(b.measure_extra_clocks(0).is_empty());
        assert_eq!(b.read_graphics_info(0), GpuGraphicsInfo::default());
        assert!(b.as_any_mut().is_some_and(|a| a.downcast_mut::<NvidiaBackend>().is_some()));
    }
}
