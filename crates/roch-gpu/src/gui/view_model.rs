//! The main window's logic (C# `MainViewModel`), free of any UI toolkit so it can be unit-tested.
//!
//! WPF bound properties and raised change notifications; egui redraws from state every frame, so
//! here a property is a getter, a setter that clamps/snaps and marks the editor dirty, and the
//! derived texts are plain methods. The numeric editor values are addressed through [`Field`], so
//! one generic slider/value-box widget serves every row.
//!
//! Driver writes never run on the UI thread: an action (Apply, Reset, a slot click, an XOC
//! Enable/Disable, the Startup toggle) does its UI-side preparation here, queues a [`Job`], and
//! the app runs queued jobs on a worker thread and hands each [`Outcome`] back to [`MainViewModel::finish`].
//! Tests use [`MainViewModel::run_jobs_inline`] to run the same jobs synchronously.

// This is the API every window is written against, including the Monitor/Fan/Curve windows that
// part 2 of the port fills in; until those land, some of it has no caller outside the tests.
#![allow(dead_code)]

use roch_core::models::{clock_step, ocp_bounds};
use roch_core::{
    FanCurve, FanMode, GpuCapabilities, GpuDevice, GpuGraphicsInfo, GpuTelemetry, GpuTuningState, Result,
    TuningProfile, VoltageControlStyle, XocLever, only_notes,
};

use super::format::signed;
use super::shared::Shared;

/// Number of profile slots in the bar.
pub const SLOT_COUNT: i32 = 5;

/// Social links in the footer, matching Roch CPU and Viewer.
pub const FOOTER_LINKS: [(&str, &str); 3] = [
    ("YouTube", "https://www.youtube.com/@MateoPcTech"),
    ("X", "https://x.com/MateoPCTech"),
    ("Discord", "https://discord.gg/KfzExpKQHB"),
];

/// "Roch GPU 1.1.0" for the title strip, from the crate version so it cannot go stale.
pub fn title_text() -> String {
    format!("Roch GPU {}", roch_core::VERSION.split(['+', '-']).next().unwrap_or(""))
}

// ------------------------------------------------------------------ fields

/// Every numeric editor value. Each has a range, a snapping grid and a value-box format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    VoltageBoost,
    VoltageOffset,
    TargetVoltage,
    CoreOffset,
    MemoryOffset,
    PowerLimit,
    TempLimit,
    VoltageRailMax,
    VoltageRailFloor,
    MsvddRailMax,
    MsvddRailFloor,
    XbarOffset,
    SysOffset,
    VideoOffset,
    ClockLockMin,
    ClockLockMax,
    FixedFan,
    NvvddOcpAmps,
    MsvddOcpAmps,
    /// The software temperature limit, in °C; the card's own limit (88 °C on a 5070 Ti) is the
    /// default/off point, and the window runs on to 105 °C.
    SoftTempLimit,
}

const FIELD_COUNT: usize = 20;

impl Field {
    pub const ALL: [Field; FIELD_COUNT] = [
        Field::VoltageBoost,
        Field::VoltageOffset,
        Field::TargetVoltage,
        Field::CoreOffset,
        Field::MemoryOffset,
        Field::PowerLimit,
        Field::TempLimit,
        Field::VoltageRailMax,
        Field::VoltageRailFloor,
        Field::MsvddRailMax,
        Field::MsvddRailFloor,
        Field::XbarOffset,
        Field::SysOffset,
        Field::VideoOffset,
        Field::ClockLockMin,
        Field::ClockLockMax,
        Field::FixedFan,
        Field::NvvddOcpAmps,
        Field::MsvddOcpAmps,
        Field::SoftTempLimit,
    ];

    /// The nudge-command name the WPF arrows used ("core:-1").
    pub fn nudge_name(self) -> Option<&'static str> {
        Some(match self {
            Field::TargetVoltage => "volt",
            Field::NvvddOcpAmps => "nvocp",
            Field::MsvddOcpAmps => "msocp",
            Field::VoltageBoost => "voltboost",
            Field::VoltageOffset => "voltoffset",
            Field::CoreOffset => "core",
            Field::MemoryOffset => "mem",
            Field::PowerLimit => "power",
            Field::TempLimit => "temp",
            Field::SoftTempLimit => "softtemp",
            Field::VoltageRailMax => "rail",
            Field::MsvddRailMax => "msvdd",
            Field::VoltageRailFloor => "railfloor",
            Field::MsvddRailFloor => "msvddfloor",
            Field::XbarOffset => "xbar",
            Field::SysOffset => "sys",
            Field::VideoOffset => "video",
            _ => return None,
        })
    }
}

// ------------------------------------------------------------------ profile slots

/// One numbered button in the Afterburner-style profile bar. A slot is a named profile on disk
/// ("Slot 1" … "Slot 5"), so slots and legacy named profiles share one store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSlot {
    pub number: i32,
    pub occupied: bool,
    /// The slot last loaded or saved; the bar highlights it in red.
    pub is_active: bool,
    pub summary: String,
}

impl ProfileSlot {
    pub fn new(number: i32) -> Self {
        Self { number, occupied: false, is_active: false, summary: String::new() }
    }
    /// Profile name used on disk. Kept stable so the logon task can target a slot.
    pub fn name(&self) -> String {
        slot_name(self.number)
    }
    pub fn label(&self) -> String {
        self.number.to_string()
    }
    pub fn tip(&self) -> String {
        if self.occupied {
            format!("Slot {}\n{}\n\nClick to load and apply · right-click to clear", self.number, self.summary)
        } else {
            format!("Slot {} is empty\n\nPress Save, then this number, to store the current settings", self.number)
        }
    }
}

pub fn slot_name(number: i32) -> String {
    format!("Slot {number}")
}

// ------------------------------------------------------------------ worker jobs

/// A driver-touching piece of an action, run off the UI thread.
pub struct Job {
    /// What the UI shows while it runs ("Applying…").
    pub label: &'static str,
    run: Box<dyn FnOnce(&Shared) -> Outcome + Send>,
}

impl Job {
    fn new(label: &'static str, run: impl FnOnce(&Shared) -> Outcome + Send + 'static) -> Self {
        Self { label, run: Box::new(run) }
    }
    /// Run it (on whatever thread the caller is on).
    pub fn run(self, shared: &Shared) -> Outcome {
        (self.run)(shared)
    }
}

/// A job's result, handed back to [`MainViewModel::finish`] on the UI thread.
pub struct Outcome(OutcomeKind);

/// What the card reads after a write: the live tuning state (for the driver-owned rail ceilings
/// and the summary line) and the held voltage cap.
struct Readback {
    state: Result<GpuTuningState>,
    lock_mv: i32,
}

impl Readback {
    fn gather(shared: &Shared) -> Self {
        let state = shared.service.read_tuning_state();
        let lock_mv = shared.service.read_voltage_lock_mv();
        Self { state, lock_mv }
    }
}

/// The UI-thread half of a [`MainViewModel::queue_custom`] job.
pub type ApplyOnUi = Box<dyn FnOnce(&mut MainViewModel) + Send>;

enum OutcomeKind {
    /// A job queued by a window (part 2: fan writes, curve edits): the closure the worker
    /// returned runs on the UI thread against the view model.
    Custom(ApplyOnUi),
    Apply {
        profile_name: String,
        errors: Vec<String>,
        slot: Option<i32>,
        name_saved: bool,
        recovery_idle: bool,
        applied: Option<TuningProfile>,
        readback: Readback,
    },
    Reset {
        result: Result<()>,
        readback: Readback,
    },
    Lever {
        lever: XocLever,
        on: bool,
        errors: Vec<String>,
        readback: Readback,
    },
    Startup {
        /// Ok(status) on success, Err(message) when the register/unregister did not go through,
        /// `None` when Startup was ticked with no profile picked.
        result: Option<std::result::Result<String, String>>,
        registered: bool,
        recovery_idle: bool,
        applied: Option<TuningProfile>,
    },
}

/// What toggling Startup has to do, decided on the UI thread.
enum StartupPlan {
    NoProfile,
    Register { name: String, candidate: Box<std::result::Result<TuningProfile, String>> },
    Unregister,
}

fn now_hms() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

fn ok(errors: &[String]) -> bool {
    errors.is_empty() || only_notes(errors)
}

// ------------------------------------------------------------------ the view model

pub struct MainViewModel {
    shared: Shared,
    pub caps: GpuCapabilities,
    pub device: GpuDevice,
    pub backend_name: String,
    /// Every card the active backend reported, and the one this editor is bound to.
    devices: Vec<GpuDevice>,
    gpu_index: usize,
    graphics_info: GpuGraphicsInfo,

    vals: [i32; FIELD_COUNT],
    zero_rpm: bool,
    memory_timing_index: i32,
    fan_mode_index: i32,
    per_fan_percents: Vec<i32>,
    editor_curve: FanCurve,
    xoc_armed: XocLever,
    pending_changes: bool,
    loading_editor: bool,

    telemetry: Option<GpuTelemetry>,
    last_ceiling: i32,
    last_boost_ceiling: i32,

    pub status: String,
    pub status_is_error: bool,
    applied_summary: String,

    slots: Vec<ProfileSlot>,
    save_armed: bool,
    profiles: Vec<String>,
    selected_profile: Option<String>,
    pub profile_name_input: String,
    apply_on_startup: bool,
    startup_profile: Option<String>,
    accepted_startup_profile: Option<String>,

    queued: Vec<Job>,
    in_flight: usize,
    busy_label: Option<&'static str>,
}

impl MainViewModel {
    /// Build the editor from the card, the settings and the saved profiles (C# constructor).
    pub fn new(shared: Shared) -> Self {
        let svc = shared.service.clone();
        let caps = svc.capabilities();
        let device = svc.device();
        let backend_name = svc.backend_name();
        let devices = svc.devices();
        let gpu_index = svc.gpu_index();
        let graphics_info = svc.read_graphics_info();

        let mut vm = Self {
            shared: shared.clone(),
            caps,
            device,
            backend_name,
            devices,
            gpu_index,
            graphics_info,
            vals: [0; FIELD_COUNT],
            zero_rpm: true,
            memory_timing_index: 0,
            fan_mode_index: 0,
            per_fan_percents: Vec::new(),
            editor_curve: FanCurve::default(),
            xoc_armed: XocLever::NONE,
            pending_changes: false,
            loading_editor: false,
            telemetry: None,
            last_ceiling: 0,
            last_boost_ceiling: 0,
            status: "Ready".into(),
            status_is_error: false,
            applied_summary: NOTHING_APPLIED.into(),
            slots: (1..=SLOT_COUNT).map(ProfileSlot::new).collect(),
            save_armed: false,
            profiles: Vec::new(),
            selected_profile: None,
            profile_name_input: String::new(),
            apply_on_startup: false,
            startup_profile: None,
            accepted_startup_profile: None,
            queued: Vec::new(),
            in_flight: 0,
            busy_label: None,
        };
        let name = vm.device.name.clone();
        let caps = vm.caps.clone();

        // Seed the measured ceilings BEFORE loading the editor: the cap slider defaults to the stock
        // ceiling, which until an observation is in hand falls back to the V/F table's top.
        let (nv_default, ms_default, nv_ocp, ms_ocp, rails_recorded) = {
            let mut s = shared.settings();
            if let Some(&mv) = s.observed_max_voltage_by_gpu.get(&name) {
                svc.seed_observed_max_voltage(mv);
            }
            if let Some(&mv) = s.observed_max_boosted_voltage_by_gpu.get(&name) {
                svc.seed_observed_max_boosted_voltage(mv);
            }
            // Rail ceilings and OCP limits: recorded the first time this GPU is seen, then carried
            // forward. Reset has nothing else to restore them to (see AppSettings).
            let mut recorded = false;
            let mut record = |map: &mut std::collections::BTreeMap<String, i32>, can: bool, value: i32| {
                if can && value > 0 && !map.contains_key(&name) {
                    map.insert(name.clone(), value);
                    recorded = true;
                }
            };
            record(&mut s.nvvdd_default_max_by_gpu, caps.can_set_voltage_rail, caps.voltage_rail_stock_max_mv);
            record(&mut s.msvdd_default_max_by_gpu, caps.can_set_msvdd_rail, caps.msvdd_rail_stock_max_mv);
            record(&mut s.nvvdd_ocp_default_by_gpu, caps.can_set_ocp, caps.nvvdd_ocp_stock_milliamps);
            record(&mut s.msvdd_ocp_default_by_gpu, caps.can_set_ocp, caps.msvdd_ocp_stock_milliamps);
            let get = |m: &std::collections::BTreeMap<String, i32>| m.get(&name).copied().unwrap_or(0);
            (
                get(&s.nvvdd_default_max_by_gpu),
                get(&s.msvdd_default_max_by_gpu),
                get(&s.nvvdd_ocp_default_by_gpu),
                get(&s.msvdd_ocp_default_by_gpu),
                recorded,
            )
        };
        svc.seed_ocp_defaults(nv_ocp, ms_ocp);
        svc.seed_rail_defaults(nv_default, ms_default);
        if rails_recorded {
            shared.save_settings();
        }

        // Seed the sliders with what the driver currently has.
        match svc.read_current_as_profile() {
            Ok(mut cur) => {
                cur.clamp_to(&vm.caps);
                vm.load_into_editor(&cur);
            }
            Err(e) => {
                vm.load_into_editor(&TuningProfile::stock(&vm.caps, Some(&name)));
                vm.set_status_error(format!("Could not read the card's current settings: {e}"));
            }
        }

        // ...except the fans, which the card cannot describe: the driver knows only "auto" or
        // "manual at N%" and stores no curve. The saved profile is the only thing that knows, so
        // the fan half comes from the last slot applied on this card, or the logon profile.
        let (last_name, startup_name) = {
            let s = shared.settings();
            (s.last_profile_by_gpu.get(&name).cloned(), s.startup_profile.clone())
        };
        let load = |n: &Option<String>| -> Option<TuningProfile> {
            n.as_deref().filter(|n| !n.is_empty()).and_then(|n| shared.store.load(n).ok().flatten())
        };
        let fan_source = load(&last_name).or_else(|| load(&startup_name));
        if !vm.caps.can_read_hardware_fan_curve
            && let Some(saw) = fan_source
            && saw.gpu_name.as_deref().is_none_or(|g| g.is_empty() || g == name)
        {
            vm.set_fan_mode_index(saw.fan_mode.as_number());
            vm.set(Field::FixedFan, saw.fixed_fan_percent);
            vm.per_fan_percents = saw.fixed_fan_percents.clone();
            vm.editor_curve = saw.fan_curve.clone_config();
        }
        vm.pending_changes = false;

        vm.refresh_profiles();
        vm.refresh_slots();

        // The registered task is the truth, not the saved flag.
        vm.apply_on_startup = shared.startup.exists();
        vm.startup_profile = startup_name.clone();
        vm.accepted_startup_profile = startup_name.clone();
        if let Some(notice) = &shared.recovery_notice {
            vm.status = notice.clone();
        } else if vm.apply_on_startup
            && let Some(sp) = &startup_name
            && shared.recovery().as_ref().is_some_and(|r| r.startup_profile(sp).is_none())
        {
            vm.status = "Refresh startup settings by toggling Startup off/on.".into();
        }

        // Light up the slot applied last session, so the bar reflects what the card is running.
        if let Some(last) = last_name
            && let Some(n) = vm.slots.iter().find(|s| s.name() == last && s.occupied).map(|s| s.number)
        {
            vm.set_active_slot(n);
        }
        vm
    }

    /// Switch to another card of this backend. The service hands the old card's software fan
    /// curve and temperature limit back; this rebuilds the editor from the new card's
    /// capabilities, current settings and last profile.
    pub fn switch_gpu(&mut self, index: usize) -> Result<()> {
        if index == self.gpu_index {
            return Ok(());
        }
        let mut shared = self.shared.clone();
        // The startup notice belonged to the card selected when the window opened.
        shared.recovery_notice = None;
        shared.service.select_gpu(index)?;
        // Recovery, and its startup confirmation, are per card.
        match shared.open_recovery() {
            Ok(recovery) => shared.set_recovery(Some(recovery)),
            Err(e) => {
                shared.set_recovery(None);
                shared.log(&format!("Recovery unavailable for the selected GPU: {}", e.message));
            }
        }
        shared.settings().gpu_index = index;
        shared.save_settings();
        *self = Self::new(shared);
        Ok(())
    }

    pub fn shared(&self) -> &Shared {
        &self.shared
    }

    // ------------------------------------------------------------------ identity lines

    /// How many cards the active backend reported (normally 1).
    pub fn gpu_count(&self) -> usize {
        self.devices.len()
    }

    pub fn gpu_index(&self) -> usize {
        self.gpu_index
    }

    /// A menu line that distinguishes two cards of the same model by their bus address.
    pub fn gpu_choice_label(&self, index: usize) -> String {
        let Some(d) = self.devices.get(index) else { return String::new() };
        let number = index + 1;
        if d.bus_id.trim().is_empty() {
            format!("{number}  ·  {}", d.name)
        } else {
            format!("{number}  ·  {}  ({})", d.name, d.bus_id)
        }
    }

    pub fn vram_text(&self) -> String {
        if self.device.vram_megabytes > 0 {
            format!("{} GB", super::format::one_decimal(self.device.vram_megabytes as f64 / 1024.0))
        } else {
            String::new()
        }
    }

    pub fn device_display_name(&self) -> String {
        if self.device.vram_megabytes > 0 {
            format!(
                "{} ({}GB)",
                self.device.name,
                super::format::one_decimal(self.device.vram_megabytes as f64 / 1024.0)
            )
        } else {
            self.device.name.clone()
        }
    }

    pub fn telemetry_device_name(&self) -> String {
        let n = self.device.name.to_ascii_lowercase();
        let suffix = if n.contains("arc") && n.contains("pro b60") { " · Battlemage BMG-G21 WKSTN" } else { "" };
        format!("{}{suffix}", self.device_display_name())
    }

    pub fn driver_line(&self) -> String {
        if !self.device.driver_version.trim().is_empty() {
            format!("Driver: {}", self.device.driver_version)
        } else {
            format!("Backend: {}", self.backend_name)
        }
    }

    pub fn bios_line(&self) -> String {
        if !self.device.bios_version.trim().is_empty() {
            format!("vBIOS: {}", self.device.bios_version)
        } else if self.device.vendor == "Intel" {
            "vBIOS: Unavailable".into()
        } else {
            String::new()
        }
    }

    pub fn memory_type_line(&self) -> String {
        let parts: Vec<&str> = [self.graphics_info.memory_type.as_str(), self.graphics_info.memory_vendor.as_str()]
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .collect();
        if parts.is_empty() { String::new() } else { format!("Memory Type: {}", parts.join(" ")) }
    }

    pub fn pcie_bus_line(&self) -> String {
        if self.graphics_info.bus_interface.trim().is_empty() {
            if self.device.vendor == "Intel" { "PCIe Bus: Unavailable".into() } else { String::new() }
        } else {
            format!("PCIe Bus: {}", self.graphics_info.bus_interface)
        }
    }

    pub fn resizable_bar_line(&self) -> String {
        if self.graphics_info.resizable_bar.trim().is_empty() {
            String::new()
        } else {
            format!("Resizable BAR: {}", self.graphics_info.resizable_bar)
        }
    }

    pub fn graphics_info(&self) -> &GpuGraphicsInfo {
        &self.graphics_info
    }

    // ------------------------------------------------------------------ which controls exist

    fn intel_tuning(&self) -> bool {
        self.caps.voltage_style == VoltageControlStyle::Percent
    }
    /// NVIDIA: one absolute mV slider driven by the V/F curve.
    pub fn is_voltage_absolute(&self) -> bool {
        self.caps.voltage_style == VoltageControlStyle::Absolute && self.caps.can_set_voltage()
    }
    /// AMD: a single signed offset applied to the whole curve.
    pub fn is_voltage_offset(&self) -> bool {
        self.caps.voltage_style == VoltageControlStyle::Offset
    }
    /// NVIDIA's over-voltage percentage (Afterburner's "Core Voltage (%)"), or Intel's limit.
    pub fn has_voltage_boost(&self) -> bool {
        (self.is_voltage_absolute() || self.caps.voltage_style == VoltageControlStyle::Percent)
            && self.caps.can_set_voltage_boost
    }
    pub fn has_curve_editor(&self) -> bool {
        self.caps.can_set_voltage_curve || self.caps.can_edit_vf_curve
    }
    pub fn has_voltage_rail(&self) -> bool {
        self.caps.can_set_voltage_rail
    }
    pub fn has_msvdd_rail(&self) -> bool {
        self.caps.can_set_msvdd_rail
    }
    pub fn has_xbar(&self) -> bool {
        self.caps.can_set_xbar_offset
    }
    pub fn has_clock_lock(&self) -> bool {
        self.caps.can_lock_clocks
    }
    pub fn has_sys(&self) -> bool {
        self.caps.can_set_sys_offset
    }
    pub fn has_video(&self) -> bool {
        self.caps.can_set_video_offset
    }
    pub fn has_ocp(&self) -> bool {
        self.caps.can_set_ocp
    }
    /// Any lever still behind the XOC gate. XBAR, SYS and video are main-window clocks now.
    pub fn has_xoc(&self) -> bool {
        self.has_voltage_rail()
            || self.has_msvdd_rail()
            || self.has_ocp()
            || (self.has_clock_lock() && !self.intel_tuning())
    }
    pub fn has_intel_clock_range(&self) -> bool {
        self.intel_tuning() && self.has_clock_lock()
    }
    pub fn has_temp_limit(&self) -> bool {
        self.caps.can_set_temp_limit
    }
    /// The software temperature limit stands in for a hardware one the driver does not offer.
    pub fn has_soft_temp_limit(&self) -> bool {
        !self.caps.can_set_temp_limit && self.caps.can_set_soft_thermal_limit
    }
    pub fn has_zero_rpm(&self) -> bool {
        self.caps.can_set_zero_rpm
    }
    pub fn has_memory_timing(&self) -> bool {
        self.caps.can_set_memory_timing && !self.caps.memory_timing_options.is_empty()
    }

    pub fn intel_clock_range_enabled(&self) -> bool {
        self.xoc_armed.has(XocLever::CLOCK_RANGE)
    }
    pub fn set_intel_clock_range_enabled(&mut self, value: bool) {
        if value != self.intel_clock_range_enabled() {
            self.xoc_armed = self.xoc_armed.with(XocLever::CLOCK_RANGE, value);
            self.dirty();
        }
    }

    /// Clock domains shown beside core and memory. No Enable/Disable state: Apply always writes
    /// their displayed offset, including zero.
    fn main_clock_levers(&self) -> XocLever {
        let mut bits = 0;
        if self.has_xbar() {
            bits |= XocLever::XBAR.0;
        }
        if self.has_sys() {
            bits |= XocLever::SYS.0;
        }
        if self.has_video() {
            bits |= XocLever::VIDEO.0;
        }
        XocLever(bits)
    }

    // ------------------------------------------------------------------ labels

    pub fn memory_label(&self) -> &'static str {
        if self.caps.memory_clock_unit == "Mbps" {
            "Memory Speed (Mbps)"
        } else if self.caps.memory_clock_is_absolute {
            "Memory Clock"
        } else {
            "Memory Clock Offset"
        }
    }
    pub fn temp_label(&self) -> String {
        "Temperature Limit".into()
    }
    pub fn soft_temp_label(&self) -> &'static str {
        "Temperature Limit"
    }
    pub fn voltage_boost_label(&self) -> &'static str {
        if self.caps.voltage_style == VoltageControlStyle::Percent { "Voltage Limit" } else { "Voltage Boost" }
    }
    pub fn core_label(&self) -> &'static str {
        "Core Offset"
    }
    pub fn voltage_label(&self) -> &'static str {
        if self.is_voltage_offset() { "Voltage Offset (mV)" } else { "Core Voltage (mV)" }
    }
    pub fn power_label(&self) -> &'static str {
        if self.caps.power_limit_is_offset { "Power Limit Offset" } else { "Power Limit" }
    }
    /// The unit drawn beside the value box for fields whose unit is not in the label. `""` keeps
    /// the unit in the label (mV, A, Mbps).
    pub fn field_unit(&self, f: Field) -> &'static str {
        match f {
            Field::CoreOffset
            | Field::XbarOffset
            | Field::SysOffset
            | Field::VideoOffset
            | Field::ClockLockMin
            | Field::ClockLockMax => "MHz",
            Field::MemoryOffset => {
                if self.caps.memory_clock_is_absolute {
                    if self.caps.memory_clock_unit == "MHz" { "MHz" } else { "" }
                } else {
                    "MHz"
                }
            }
            Field::PowerLimit | Field::VoltageBoost => "%",
            Field::TempLimit => self.caps.temp_limit_unit(),
            Field::SoftTempLimit => "°C",
            _ => "",
        }
    }
    pub fn memory_timing_options(&self) -> &[String] {
        &self.caps.memory_timing_options
    }

    pub fn core_range_text(&self) -> String {
        format!("{} … {} MHz", signed(self.caps.core_offset_min_mhz), signed(self.caps.core_offset_max_mhz))
    }
    pub fn mem_range_text(&self) -> String {
        let c = &self.caps;
        if c.memory_clock_is_absolute {
            format!(
                "{} … {} {} (stock {})",
                c.memory_offset_min_mhz, c.memory_offset_max_mhz, c.memory_clock_unit, c.memory_clock_default_mhz
            )
        } else {
            format!("{} … {} MHz", signed(c.memory_offset_min_mhz), signed(c.memory_offset_max_mhz))
        }
    }
    pub fn amd_clock_support_text(&self) -> String {
        format!(
            "Offset range: {}. Absolute core limits and FCLK adjustment are unavailable in this backend. FCLK appears in Telemetry when reported.",
            self.core_range_text()
        )
    }
    pub fn voltage_offset_range_text(&self) -> String {
        format!("{} … {} mV (0 = stock)", self.caps.voltage_offset_min_mv, self.caps.voltage_offset_max_mv)
    }
    pub fn voltage_boost_range_text(&self) -> String {
        let c = &self.caps;
        if c.voltage_style == VoltageControlStyle::Percent {
            format!("{} … {}% of Intel voltage headroom.", c.voltage_boost_min_percent, c.voltage_boost_max_percent)
        } else {
            format!(
                "{} … {} % (0 = stock ceiling). Raises how far the core may be driven above the top of the V/F table; it does not add curve points.",
                c.voltage_boost_min_percent, c.voltage_boost_max_percent
            )
        }
    }
    pub fn power_range_text(&self) -> String {
        let c = &self.caps;
        format!(
            "{} … {} % (default {})",
            c.power_limit_min_percent, c.power_limit_max_percent, c.power_limit_default_percent
        )
    }
    pub fn temp_range_text(&self) -> String {
        let c = &self.caps;
        format!(
            "{} … {} {} (default {})",
            c.temp_limit_min_c,
            c.temp_limit_max_c,
            c.temp_limit_unit(),
            c.temp_limit_default_c
        )
    }

    /// The software limit's tooltip: what it does, and what it cannot.
    pub fn soft_temp_range_text(&self) -> String {
        let (lo, hi) = self.caps.soft_thermal_limit_range();
        let card = self.soft_temp_card_limit();
        format!(
            "{lo} … {hi} °C. The default is {card} °C — this card's own limit where the driver reports \
             one, otherwise the top of the range. At or above it the card manages itself and Roch GPU \
             does nothing. The slider runs on to {hi} °C; values from {card} °C up also leave it to the \
             card. Below {card} °C Roch GPU enforces the limit in software: when the GPU reaches it, the \
             core clock is capped a step at a time and allowed to climb back as the card cools. That \
             works only while Roch GPU is running, including from the tray; closing the app hands the \
             clock back. The card's own slowdown at the hardware threshold always applies."
        )
    }

    /// The card's own limit (88 °C on a 5070 Ti), or the top of the range when the driver reports
    /// none, for the tooltip.
    pub fn soft_temp_card_limit(&self) -> i32 {
        self.caps.soft_thermal_card_limit_c
    }
    /// The default/off value shown on the slider: the card's own limit (88 °C on a 5070 Ti),
    /// falling back to the top of the window if the backend did not report one.
    pub fn soft_temp_off_c(&self) -> i32 {
        self.caps.soft_thermal_off_c()
    }
    /// The editor's software limit as a profile stores it: 0 at the card's own limit.
    pub fn soft_temp_limit_c(&self) -> i32 {
        let v = self.get(Field::SoftTempLimit);
        if self.has_soft_temp_limit() && v != self.soft_temp_off_c() { v } else { 0 }
    }

    // ------------------------------------------------------------------ steps and ranges

    /// Off-grid values a profile carries (Intel) survive loading: snapping only applies to edits.
    fn editor_step(&self, step: i32) -> i32 {
        if self.intel_tuning() && self.loading_editor { 1 } else { step }
    }
    pub fn core_step_mhz(&self) -> i32 {
        if self.intel_tuning() { 15 } else { self.caps.core_offset_step_mhz }
    }
    /// Grid the memory value snaps to. An absolute AMD clock is left alone (1 = no snapping): its
    /// stock value sits wherever the driver puts it, and rounding it would overclock the card.
    fn memory_snap_mhz(&self) -> i32 {
        if self.intel_tuning() {
            self.editor_step(100)
        } else if self.caps.memory_clock_is_absolute {
            1
        } else {
            clock_step::MEMORY_MHZ
        }
    }
    /// How far one nudge or slider step moves memory.
    pub fn memory_step_mhz(&self) -> i32 {
        if self.intel_tuning() {
            100
        } else if self.caps.memory_clock_is_absolute {
            5
        } else {
            clock_step::MEMORY_MHZ
        }
    }
    pub fn power_step_percent(&self) -> i32 {
        if self.intel_tuning() { 5 } else { 1 }
    }
    pub fn temp_step(&self) -> i32 {
        if self.intel_tuning() { 5 } else { 1 }
    }

    /// OCP windows in whole amps: the driver's own where it offers one, else derived from stock.
    pub fn nvvdd_ocp_min_a(&self) -> i32 {
        let c = &self.caps;
        if c.nvvdd_ocp_min_milliamps > 0 {
            c.nvvdd_ocp_min_milliamps / 1000
        } else {
            ocp_bounds::min_a(c.nvvdd_ocp_stock_milliamps)
        }
    }
    pub fn nvvdd_ocp_max_a(&self) -> i32 {
        let c = &self.caps;
        if c.nvvdd_ocp_max_milliamps > 0 {
            c.nvvdd_ocp_max_milliamps / 1000
        } else {
            ocp_bounds::max_a(c.nvvdd_ocp_stock_milliamps)
        }
    }
    pub fn msvdd_ocp_min_a(&self) -> i32 {
        let c = &self.caps;
        if c.msvdd_ocp_min_milliamps > 0 {
            c.msvdd_ocp_min_milliamps / 1000
        } else {
            ocp_bounds::min_a(c.msvdd_ocp_stock_milliamps)
        }
    }
    pub fn msvdd_ocp_max_a(&self) -> i32 {
        let c = &self.caps;
        if c.msvdd_ocp_max_milliamps > 0 {
            c.msvdd_ocp_max_milliamps / 1000
        } else {
            ocp_bounds::max_a(c.msvdd_ocp_stock_milliamps)
        }
    }

    /// The range a value is clamped into on set.
    pub fn bounds(&self, f: Field) -> (i32, i32) {
        let c = &self.caps;
        match f {
            Field::CoreOffset => (c.core_offset_min_mhz, c.core_offset_max_mhz),
            Field::MemoryOffset => (c.memory_offset_min_mhz, c.memory_offset_max_mhz),
            Field::PowerLimit => (c.power_limit_min_percent, c.power_limit_max_percent),
            Field::TempLimit => (c.temp_limit_min_c, c.temp_limit_max_c),
            Field::VoltageBoost => (c.voltage_boost_min_percent, c.voltage_boost_max_percent),
            Field::VoltageOffset => (c.voltage_offset_min_mv, c.voltage_offset_max_mv),
            Field::TargetVoltage => (c.min_voltage_mv, c.min_voltage_mv.max(self.boost_ceiling_mv())),
            Field::VoltageRailMax => (c.voltage_rail_min_mv, c.voltage_rail_min_mv.max(c.voltage_rail_max_mv)),
            Field::VoltageRailFloor => {
                (c.voltage_rail_floor_min_mv, c.voltage_rail_floor_min_mv.max(c.voltage_rail_floor_max_mv))
            }
            Field::MsvddRailMax => (c.msvdd_rail_min_mv, c.msvdd_rail_min_mv.max(c.msvdd_rail_max_mv)),
            Field::MsvddRailFloor => {
                (c.msvdd_rail_floor_min_mv, c.msvdd_rail_floor_min_mv.max(c.msvdd_rail_floor_max_mv))
            }
            Field::XbarOffset => (c.xbar_offset_min_mhz, c.xbar_offset_max_mhz),
            Field::SysOffset => (c.sys_offset_min_mhz, c.sys_offset_max_mhz),
            Field::VideoOffset => (c.video_offset_min_mhz, c.video_offset_max_mhz),
            Field::ClockLockMin | Field::ClockLockMax => {
                (c.clock_lock_min_mhz, c.clock_lock_min_mhz.max(c.clock_lock_max_mhz))
            }
            Field::FixedFan => (c.fan_min_percent, c.fan_max_percent),
            Field::NvvddOcpAmps => (self.nvvdd_ocp_min_a(), self.nvvdd_ocp_max_a()),
            Field::MsvddOcpAmps => (self.msvdd_ocp_min_a(), self.msvdd_ocp_max_a()),
            Field::SoftTempLimit => c.soft_thermal_limit_range(),
        }
    }

    /// The range a slider spans (the XAML Minimum/Maximum). A rail's two thumbs share one track
    /// from the floor's minimum to the ceiling's maximum.
    pub fn slider_range(&self, f: Field) -> (i32, i32) {
        let c = &self.caps;
        match f {
            Field::VoltageRailMax | Field::VoltageRailFloor => (c.voltage_rail_floor_min_mv, c.voltage_rail_max_mv),
            Field::MsvddRailMax | Field::MsvddRailFloor => (c.msvdd_rail_floor_min_mv, c.msvdd_rail_max_mv),
            Field::ClockLockMin | Field::ClockLockMax => (c.clock_lock_min_mhz, c.clock_lock_max_mhz),
            _ => self.bounds(f),
        }
    }

    /// One slider tick / one arrow press.
    pub fn step(&self, f: Field) -> i32 {
        match f {
            Field::CoreOffset | Field::XbarOffset | Field::SysOffset | Field::VideoOffset => self.core_step_mhz(),
            Field::MemoryOffset => self.memory_step_mhz(),
            Field::PowerLimit => self.power_step_percent(),
            Field::TempLimit => self.temp_step(),
            Field::SoftTempLimit => 1,
            Field::VoltageBoost => clock_step::VOLTAGE_BOOST_PERCENT,
            Field::FixedFan => clock_step::FAN_PERCENT,
            Field::ClockLockMin | Field::ClockLockMax => 15,
            _ => 5,
        }
    }

    /// The snapped value a set would store, before clamping.
    fn snapped(&self, f: Field, v: i32) -> i32 {
        let (min, max) = self.bounds(f);
        let within = |step: i32| clock_step::snap_within(v, step, min, max);
        match f {
            Field::CoreOffset => within(self.editor_step(self.core_step_mhz())),
            Field::MemoryOffset => within(self.memory_snap_mhz()),
            Field::PowerLimit => within(self.editor_step(self.power_step_percent())),
            Field::TempLimit => within(self.editor_step(self.temp_step())),
            Field::VoltageBoost => within(self.editor_step(clock_step::VOLTAGE_BOOST_PERCENT)),
            Field::XbarOffset | Field::SysOffset | Field::VideoOffset => within(self.core_step_mhz()),
            Field::FixedFan => within(clock_step::FAN_PERCENT),
            _ => v,
        }
    }

    // ------------------------------------------------------------------ values

    pub fn get(&self, f: Field) -> i32 {
        self.vals[f as usize]
    }

    /// Snap, clamp, store; marks the editor dirty when the value moved (C# `SetTuned`).
    pub fn set(&mut self, f: Field, value: i32) -> bool {
        let (min, max) = self.bounds(f);
        let v = roch_core::util::clamp_i32(self.snapped(f, value), min, max);
        if self.vals[f as usize] == v {
            return false;
        }
        self.vals[f as usize] = v;
        self.dirty();
        true
    }

    /// The value box's text: offsets signed, absolute values plain.
    pub fn input_text(&self, f: Field) -> String {
        let v = self.get(f);
        match f {
            Field::CoreOffset | Field::VoltageOffset | Field::XbarOffset | Field::SysOffset | Field::VideoOffset => {
                signed(v)
            }
            Field::MemoryOffset if !self.caps.memory_clock_is_absolute => signed(v),
            _ => v.to_string(),
        }
    }

    /// Commit a typed value. Garbage leaves the value unchanged; the caller re-reads
    /// [`Self::input_text`] either way, so a clamped value replaces what was typed.
    pub fn commit_input(&mut self, f: Field, text: &str) {
        if f == Field::SoftTempLimit {
            // "Off", "card" or 0 go to the card's own limit (88 °C); any number lands in the window.
            let off = self.soft_temp_off_c();
            let t = text.trim();
            if t.eq_ignore_ascii_case("off") || t.eq_ignore_ascii_case("card") {
                self.set(f, off);
            } else if let Some(v) = super::format::parse_signed(text) {
                self.set(f, if v <= 0 { off } else { v });
            }
            return;
        }
        if let Some(v) = super::format::parse_signed(text) {
            self.set(f, v);
        }
    }

    /// Step one value by one increment (the arrow buttons, and the WPF `NudgeCommand`).
    pub fn nudge(&mut self, f: Field, dir: i32) {
        if dir == 0 {
            return;
        }
        let step = match f {
            Field::CoreOffset | Field::XbarOffset | Field::SysOffset | Field::VideoOffset => self.core_step_mhz(),
            Field::MemoryOffset => self.memory_step_mhz(),
            Field::PowerLimit => self.power_step_percent(),
            Field::TempLimit => self.temp_step(),
            Field::SoftTempLimit => 1,
            Field::VoltageBoost => clock_step::VOLTAGE_BOOST_PERCENT,
            _ => 5,
        };
        self.set(f, self.get(f).saturating_add(step.saturating_mul(dir)));
    }

    /// The WPF command form, "name:direction" (e.g. "core:-1").
    pub fn nudge_spec(&mut self, spec: &str) {
        let Some((name, dir)) = spec.rsplit_once(':') else { return };
        let Ok(dir) = dir.parse::<i32>() else { return };
        if let Some(f) = Field::ALL.iter().copied().find(|f| f.nudge_name() == Some(name)) {
            self.nudge(f, dir);
        }
    }

    pub fn zero_rpm(&self) -> bool {
        self.zero_rpm
    }
    pub fn set_zero_rpm(&mut self, v: bool) {
        if self.zero_rpm != v {
            self.zero_rpm = v;
            self.dirty();
        }
    }
    pub fn memory_timing_index(&self) -> i32 {
        self.memory_timing_index
    }
    pub fn set_memory_timing_index(&mut self, v: i32) {
        if self.memory_timing_index != v {
            self.memory_timing_index = v;
            self.dirty();
        }
    }

    /// 0 = Auto, 1 = Fixed, 2 = Curve.
    pub fn fan_mode_index(&self) -> i32 {
        self.fan_mode_index
    }
    pub fn set_fan_mode_index(&mut self, v: i32) {
        if self.fan_mode_index != v {
            self.fan_mode_index = v;
            self.dirty();
        }
    }
    pub fn is_fixed_fan(&self) -> bool {
        self.fan_mode_index == 1
    }
    pub fn is_curve_fan(&self) -> bool {
        self.fan_mode_index == 2
    }
    /// Per-fan duties, empty when they all run together. Owned here so the fan window and the
    /// main window cannot drift apart.
    pub fn per_fan_percents(&self) -> &[i32] {
        &self.per_fan_percents
    }
    /// The fan-curve editor's working copy; the fan window edits it in place, then calls
    /// [`Self::mark_curve_dirty`].
    pub fn editor_curve(&self) -> &FanCurve {
        &self.editor_curve
    }
    pub fn editor_curve_mut(&mut self) -> &mut FanCurve {
        &mut self.editor_curve
    }
    pub fn mark_curve_dirty(&mut self) {
        self.dirty();
    }

    /// The fan window wrote to the card; bring this window's controls into line with it.
    pub fn sync_fans_from_service(&mut self, mode: FanMode, percent: i32, per_fan: &[i32]) {
        self.set(Field::FixedFan, percent);
        self.per_fan_percents = per_fan.to_vec();
        self.set_fan_mode_index(mode.as_number());
        self.refresh_applied_summary();
    }

    /// The slot lit up in the profile bar, if any.
    pub fn active_slot_name(&self) -> Option<String> {
        self.slots.iter().find(|s| s.is_active && s.occupied).map(ProfileSlot::name)
    }

    /// Write the fan settings into the saved profile behind the active slot, leaving everything
    /// else in it alone. Fans are saved as they are applied: the point of a curve is for the machine
    /// to run it, including after a reboot.
    pub fn save_fans_to_active_slot(
        &mut self,
        mode: FanMode,
        percent: i32,
        per_fan: &[i32],
        curve: &FanCurve,
    ) -> String {
        let Some(name) = self.active_slot_name() else {
            return "not saved - no profile slot is active".into();
        };
        let Some(mut p) = self.shared.store.load(&name).ok().flatten() else {
            return format!("not saved - '{name}' could not be read");
        };
        p.fan_mode = mode;
        p.fixed_fan_percent = percent;
        p.fixed_fan_percents = per_fan.to_vec();
        p.fan_curve = curve.clone_config();
        p.zero_rpm = self.zero_rpm;
        if let Err(e) = self.shared.store.save(&mut p) {
            return format!("not saved - {e}");
        }
        self.refresh_slots();
        format!("saved to {name}{}", self.startup_save_notice(&name))
    }

    // ------------------------------------------------------------------ XOC

    pub fn xoc_armed(&self) -> XocLever {
        self.xoc_armed
    }
    pub fn is_armed(&self, lever: XocLever) -> bool {
        self.xoc_armed.has(lever)
    }

    /// The rail ceilings the driver shipped with: the figure recorded the first time this GPU was
    /// seen, which Disable restores to — not whatever the rail reads now.
    pub fn stock_nvvdd_max_mv(&self) -> i32 {
        let v = self.shared.service.nvvdd_default_max_mv();
        if v > 0 { v } else { self.caps.voltage_rail_stock_max_mv }
    }
    pub fn stock_msvdd_max_mv(&self) -> i32 {
        let v = self.shared.service.msvdd_default_max_mv();
        if v > 0 { v } else { self.caps.msvdd_rail_stock_max_mv }
    }

    /// What the levers are doing, told apart from what they are only asked to do.
    pub fn xoc_status_text(&self) -> String {
        let mut names = Vec::new();
        for (lever, name) in [
            (XocLever::NVVDD, "NVVDD"),
            (XocLever::MSVDD, "MSVDD"),
            (XocLever::XBAR, "XBAR"),
            (XocLever::SYS, "SYS"),
            (XocLever::VIDEO, "video"),
            (XocLever::CLOCK_RANGE, "clock range"),
        ] {
            if self.xoc_armed.has(lever) {
                names.push(name);
            }
        }
        // What an earlier session left on the card: rail ceilings outlive the process that set them.
        let mut raised = Vec::new();
        if self.has_voltage_rail()
            && !self.xoc_armed.has(XocLever::NVVDD)
            && self.get(Field::VoltageRailMax) > self.stock_nvvdd_max_mv()
        {
            raised.push("NVVDD");
        }
        if self.has_msvdd_rail()
            && !self.xoc_armed.has(XocLever::MSVDD)
            && self.get(Field::MsvddRailMax) > self.stock_msvdd_max_mv()
        {
            raised.push("MSVDD");
        }
        let carried = if raised.is_empty() {
            String::new()
        } else {
            format!(
                " {} {} above the driver's default, raised before this session started.",
                raised.join(" and "),
                if raised.len() > 1 { "are" } else { "is" }
            )
        };
        if names.is_empty() {
            return if carried.is_empty() {
                "Nothing armed - the card is running the driver's own values.".into()
            } else {
                format!("Nothing armed here, but{carried} Enable, then Disable, to put it back.")
            };
        }
        let list = names.join(", ");
        let head = if self.shared.service.armed_on_card().has(self.xoc_armed) {
            format!("Live on the card: {list}.")
        } else {
            format!("Armed: {list}. Press Apply to send them to the card.")
        };
        head + &carried
    }

    pub fn pending_changes(&self) -> bool {
        self.pending_changes
    }
    fn dirty(&mut self) {
        self.pending_changes = true;
    }

    // ------------------------------------------------------------------ value texts

    pub fn core_offset_text(&self) -> String {
        format!("{} MHz", signed(self.get(Field::CoreOffset)))
    }
    pub fn memory_offset_text(&self) -> String {
        format!("{} MHz", signed(self.get(Field::MemoryOffset)))
    }
    pub fn power_limit_text(&self) -> String {
        format!("{} %", self.get(Field::PowerLimit))
    }
    pub fn temp_limit_text(&self) -> String {
        format!("{} {}", self.get(Field::TempLimit), self.caps.temp_limit_unit())
    }
    pub fn voltage_offset_text(&self) -> String {
        format!("{} mV", signed(self.get(Field::VoltageOffset)))
    }
    pub fn target_voltage_text(&self) -> String {
        format!("{} mV", self.get(Field::TargetVoltage))
    }

    /// What this card really tops out at; the curve's last point is a step below it.
    pub fn stock_ceiling_mv(&self) -> i32 {
        self.shared.service.stock_ceiling_mv()
    }
    /// The highest voltage the card can select with the boost shown here. A cap at or above it
    /// means "no cap".
    pub fn reachable_ceiling_mv(&self) -> i32 {
        self.shared.service.reachable_ceiling_mv(self.get(Field::VoltageBoost))
    }
    /// Top of the cap slider.
    pub fn boost_ceiling_mv(&self) -> i32 {
        self.shared.service.boost_ceiling_mv()
    }

    pub fn target_voltage_range_text(&self) -> String {
        let stock = self.stock_ceiling_mv();
        let curve_top = self.caps.stock_max_voltage_mv;
        let tail = if stock < curve_top {
            format!(
                "This card has not been seen above {stock} mV under load, against a V/F curve whose last point is {curve_top} mV — a cap above {stock} mV caps nothing."
            )
        } else {
            format!(
                "This card tops out at {stock} mV (the V/F curve's last point is {curve_top} mV; the VID sits a step above it)."
            )
        };
        format!(
            "{} … {} mV. Holds the core at or below this voltage; set it at or above the ceiling to cap nothing. {tail}",
            self.caps.min_voltage_mv,
            self.boost_ceiling_mv()
        )
    }

    pub fn voltage_cap_text(&self) -> String {
        if !self.caps.can_set_voltage() {
            return if self.caps.curve_unavailable_reason.is_empty() {
                "voltage control unavailable on this driver".into()
            } else {
                format!("voltage control unavailable — {}", self.caps.curve_unavailable_reason)
            };
        }
        let reachable = self.reachable_ceiling_mv();
        let target = self.get(Field::TargetVoltage);
        if target >= reachable {
            return format!("no cap — free to the ceiling ({reachable} mV)");
        }
        let how = self.shared.service.voltage_lock_mechanism();
        let via = if matches!(how.as_str(), "none" | "n/a" | "") { String::new() } else { format!(" · via {how}") };
        format!("undervolt — capped at {target} mV (stock {} mV){via}", self.stock_ceiling_mv())
    }

    pub fn voltage_rail_max_text(&self) -> String {
        let v = self.get(Field::VoltageRailMax);
        let stock = self.caps.voltage_rail_stock_max_mv;
        if stock > 0 && v == stock {
            format!("current ceiling ({v} mV)")
        } else {
            format!("{v} mV — {} mV on the core rail's ceiling", signed(v - stock))
        }
    }
    pub fn voltage_rail_max_range_text(&self) -> String {
        let c = &self.caps;
        format!(
            "{} … {} mV (stock {} mV). Moves the core rail's own ceiling. The driver accepts more than this range offers; it is narrowed to the part worth using.",
            c.voltage_rail_min_mv, c.voltage_rail_max_mv, c.voltage_rail_stock_max_mv
        )
    }
    pub fn msvdd_rail_max_text(&self) -> String {
        let v = self.get(Field::MsvddRailMax);
        if self.caps.msvdd_rail_stock_max_mv > 0 && v == self.caps.msvdd_rail_stock_max_mv {
            format!("current ceiling ({v} mV)")
        } else {
            format!("{v} mV on the MSVDD rail")
        }
    }
    pub fn msvdd_rail_max_range_text(&self) -> String {
        let c = &self.caps;
        format!(
            "{} ... {} mV (stock {} mV). Feeds the crossbar, SYS and video domains rather than the shader core.",
            c.msvdd_rail_min_mv, c.msvdd_rail_max_mv, c.msvdd_rail_stock_max_mv
        )
    }
    pub fn voltage_rail_floor_text(&self) -> String {
        let v = self.get(Field::VoltageRailFloor);
        if self.caps.voltage_rail_stock_floor_mv > 0 && v == self.caps.voltage_rail_stock_floor_mv {
            format!("stock floor ({v} mV)")
        } else {
            format!("{v} mV floor on the core rail")
        }
    }
    pub fn voltage_rail_floor_range_text(&self) -> String {
        let c = &self.caps;
        format!(
            "{} ... {} mV (stock {} mV). The lowest the rail may drop to; raising it holds more voltage at idle as well as under load.",
            c.voltage_rail_floor_min_mv, c.voltage_rail_floor_max_mv, c.voltage_rail_stock_floor_mv
        )
    }
    pub fn msvdd_rail_floor_text(&self) -> String {
        let v = self.get(Field::MsvddRailFloor);
        if self.caps.msvdd_rail_stock_floor_mv > 0 && v == self.caps.msvdd_rail_stock_floor_mv {
            format!("stock floor ({v} mV)")
        } else {
            format!("{v} mV floor on the MSVDD rail")
        }
    }
    pub fn msvdd_rail_floor_range_text(&self) -> String {
        let c = &self.caps;
        format!(
            "{} ... {} mV (stock {} mV).",
            c.msvdd_rail_floor_min_mv, c.msvdd_rail_floor_max_mv, c.msvdd_rail_stock_floor_mv
        )
    }

    /// A window covering the whole range constrains nothing, so it means unpinned.
    pub fn clock_lock_is_off(&self) -> bool {
        if self.intel_tuning() {
            !self.intel_clock_range_enabled()
        } else {
            self.get(Field::ClockLockMin) <= self.caps.clock_lock_min_mhz
                && self.get(Field::ClockLockMax) >= self.caps.clock_lock_max_mhz
        }
    }
    pub fn clock_lock_range_text(&self) -> String {
        format!(
            "{} … {} MHz. Holds the graphics clock inside a window; Disable to restore driver defaults.",
            self.caps.clock_lock_min_mhz, self.caps.clock_lock_max_mhz
        )
    }
    pub fn sys_offset_range_text(&self) -> String {
        format!(
            "{} \u{2026} {} MHz. The SYS domain, from the same private family as the crossbar.",
            signed(self.caps.sys_offset_min_mhz),
            signed(self.caps.sys_offset_max_mhz)
        )
    }
    pub fn video_offset_range_text(&self) -> String {
        format!(
            "{} \u{2026} {} MHz. Drives the video encode/decode clock.",
            signed(self.caps.video_offset_min_mhz),
            signed(self.caps.video_offset_max_mhz)
        )
    }
    pub fn nvvdd_ocp_range_text(&self) -> String {
        format!(
            "{} … {} A, against a stock {} A{}Over-current protection for the core rail: the current at which the card cuts in to save itself.",
            self.nvvdd_ocp_min_a(),
            self.nvvdd_ocp_max_a(),
            self.caps.nvvdd_ocp_stock_milliamps / 1000,
            if self.caps.nvvdd_ocp_min_milliamps > 0 { " — the window this card's driver reports. " } else { ". " }
        )
    }
    pub fn msvdd_ocp_range_text(&self) -> String {
        format!(
            "{} … {} A, against a stock {} A{}Over-current protection for the rail behind the crossbar, SYS and video domains.",
            self.msvdd_ocp_min_a(),
            self.msvdd_ocp_max_a(),
            self.caps.msvdd_ocp_stock_milliamps / 1000,
            if self.caps.msvdd_ocp_min_milliamps > 0 { " — the window this card's driver reports. " } else { ". " }
        )
    }
    pub fn xbar_offset_range_text(&self) -> String {
        format!(
            "{} … {} MHz. Offsets the crossbar, which no public NVAPI surface exposes; the GPU's own frequency counter is used to verify the write landed.",
            signed(self.caps.xbar_offset_min_mhz),
            signed(self.caps.xbar_offset_max_mhz)
        )
    }

    /// The value box's tooltip for a field.
    pub fn range_text(&self, f: Field) -> String {
        match f {
            Field::VoltageBoost => self.voltage_boost_range_text(),
            Field::VoltageOffset => self.voltage_offset_range_text(),
            Field::TargetVoltage => self.target_voltage_range_text(),
            Field::CoreOffset => self.core_range_text(),
            Field::MemoryOffset => self.mem_range_text(),
            Field::PowerLimit => self.power_range_text(),
            Field::TempLimit => self.temp_range_text(),
            Field::VoltageRailMax | Field::VoltageRailFloor => self.voltage_rail_max_range_text(),
            Field::MsvddRailMax | Field::MsvddRailFloor => self.msvdd_rail_max_range_text(),
            Field::XbarOffset => self.xbar_offset_range_text(),
            Field::SysOffset => self.sys_offset_range_text(),
            Field::VideoOffset => self.video_offset_range_text(),
            Field::ClockLockMin | Field::ClockLockMax => self.clock_lock_range_text(),
            Field::FixedFan => format!("{} … {} %", self.caps.fan_min_percent, self.caps.fan_max_percent),
            Field::NvvddOcpAmps => self.nvvdd_ocp_range_text(),
            Field::MsvddOcpAmps => self.msvdd_ocp_range_text(),
            Field::SoftTempLimit => self.soft_temp_range_text(),
        }
    }

    // ------------------------------------------------------------------ live telemetry

    pub fn telemetry(&self) -> Option<&GpuTelemetry> {
        self.telemetry.as_ref()
    }

    /// A new foreground sample, or `None` when polling went to the background. Persists a newly
    /// observed voltage ceiling, which moves rarely, so this writes rarely.
    pub fn set_telemetry(&mut self, t: Option<GpuTelemetry>) {
        self.telemetry = t;
        let svc = &self.shared.service;
        let (stock, boost) = (svc.stock_ceiling_mv(), svc.boost_ceiling_mv());
        if stock != self.last_ceiling || boost != self.last_boost_ceiling {
            self.last_ceiling = stock;
            self.last_boost_ceiling = boost;
            let (seen, seen_boost) = (svc.observed_max_voltage_mv(), svc.observed_max_boosted_voltage_mv());
            if seen > 0 || seen_boost > 0 {
                {
                    let mut s = self.shared.settings();
                    if seen > 0 {
                        s.observed_max_voltage_by_gpu.insert(self.device.name.clone(), seen);
                    }
                    if seen_boost > 0 {
                        s.observed_max_boosted_voltage_by_gpu.insert(self.device.name.clone(), seen_boost);
                    }
                }
                self.shared.save_settings();
            }
        }
    }

    /// The software temperature limit's live state, from the service (it runs in the background
    /// too, so this needs no sample). `Some` while it is holding the clock down.
    pub fn soft_thermal_active_text(&self) -> Option<String> {
        if !self.has_soft_temp_limit() {
            return None;
        }
        let s = self.shared.service.soft_thermal_status();
        (s.enabled && s.active && s.ceiling_mhz > 0)
            .then(|| format!("Thermal limit active: capped at {} MHz", s.ceiling_mhz))
    }

    /// One line for the XOC window's clock range while the software limit holds the clock inside it.
    pub fn soft_thermal_clock_range_note(&self) -> Option<String> {
        let s = self.shared.service.soft_thermal_status();
        (self.has_soft_temp_limit() && s.enabled && s.active).then(|| {
            format!(
                "The software temperature limit ({} °C) is holding the clock at or below {} MHz, inside this range; \
                 the range comes back in full as the GPU cools.",
                s.limit_c, s.ceiling_mhz
            )
        })
    }

    /// No sample means nothing is being polled — say so rather than claim "no limiter". The
    /// software temperature limit holding the clock down takes the line: it is the limiter then.
    pub fn limit_reason_text(&self) -> String {
        if let Some(text) = self.soft_thermal_active_text() {
            return text;
        }
        match &self.telemetry {
            None => "Limiter — open the monitor to sample".into(),
            Some(t) if t.limit_reason == "Unavailable" => "Limiter status not reported".into(),
            Some(t) if t.limit_reason.is_empty() || t.limit_reason == "None" => "No active limiter".into(),
            Some(t) => format!("Limited by {}", t.limit_reason.to_lowercase()),
        }
    }
    pub fn limit_is_active(&self) -> bool {
        self.soft_thermal_active_text().is_some()
            || self.telemetry.as_ref().is_some_and(|t| {
                !t.limit_reason.is_empty() && t.limit_reason != "None" && t.limit_reason != "Unavailable"
            })
    }

    // ------------------------------------------------------------------ status

    fn set_status(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.status_is_error = false;
    }
    fn set_status_error(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.status_is_error = true;
    }
    /// For the windows: show a message in the status line.
    pub fn report(&mut self, message: impl Into<String>, is_error: bool) {
        self.status = message.into();
        self.status_is_error = is_error;
    }

    /// What the card is actually carrying, read back rather than echoed. Cached: it costs two
    /// driver calls.
    pub fn applied_summary(&self) -> &str {
        &self.applied_summary
    }

    /// Re-read the card and rebuild the summary on this thread (two short driver calls).
    pub fn refresh_applied_summary(&mut self) {
        let readback = if self.shared.service.applied_profile().is_some() {
            Readback::gather(&self.shared)
        } else {
            Readback { state: Err("not read".into()), lock_mv: 0 }
        };
        self.applied_summary = self.build_applied_summary(&readback);
    }

    fn build_applied_summary(&self, rb: &Readback) -> String {
        let Some(p) = self.shared.service.applied_profile() else {
            return NOTHING_APPLIED.into();
        };
        let c = &self.caps;
        let a = match &rb.state {
            Ok(a) => a,
            Err(_) => {
                return format!(
                    "Applied: core {} · mem {} · power {}% · temp {}{} · fan {}",
                    signed(p.core_offset_mhz),
                    signed(p.memory_offset_mhz),
                    p.power_limit_percent,
                    p.temp_limit_c,
                    c.temp_limit_unit(),
                    p.fan_mode.name()
                );
            }
        };
        let volts = match c.voltage_style {
            VoltageControlStyle::Offset => {
                if a.voltage_offset_mv == 0 {
                    "stock".to_string()
                } else {
                    format!("{} mV", signed(a.voltage_offset_mv))
                }
            }
            VoltageControlStyle::Percent => format!("limit {}%", a.voltage_boost_percent),
            _ => {
                if rb.lock_mv > 0 {
                    let how = self.shared.service.voltage_lock_mechanism();
                    let via =
                        if matches!(how.as_str(), "none" | "n/a" | "") { String::new() } else { format!(" ({how})") };
                    format!("{} mV{via}", rb.lock_mv)
                } else if a.voltage_boost_percent > 0 {
                    format!("boost +{}%", a.voltage_boost_percent)
                } else {
                    "stock".into()
                }
            }
        };
        let mem = if c.memory_clock_is_absolute {
            format!("{} {}", a.memory_offset_mhz, c.memory_clock_unit)
        } else {
            signed(a.memory_offset_mhz)
        };
        let temp = if c.can_set_temp_limit {
            format!(" · temp {}{}", a.temp_limit_c, c.temp_limit_unit())
        } else if self.has_soft_temp_limit()
            && p.soft_thermal_limit_c > 0
            && p.soft_thermal_limit_c < self.soft_temp_off_c()
        {
            // Not on the card: the limit this process enforces. At or above the card's own limit the
            // value is only the "leave it to the card" target, so it is not shown here.
            format!(" · temp {}°C (software)", p.soft_thermal_limit_c)
        } else {
            String::new()
        };
        let mut extras = String::new();
        if c.can_set_memory_timing
            && a.memory_timing_level > 0
            && let Some(opt) = c.memory_timing_options.get(a.memory_timing_level as usize)
        {
            extras += &format!(" · {}", opt.to_lowercase());
        }
        if c.can_set_zero_rpm && !a.zero_rpm {
            extras += " · zero-rpm off";
        }
        let fan = if a.detected_fan_mode == Some(FanMode::Curve) {
            "curve".to_string()
        } else if a.fan_manual {
            format!("{}%", a.fan_percent)
        } else {
            "auto".into()
        };
        format!(
            "On GPU: volt {volts} · core {} · mem {mem} · power {}%{temp} · fan {fan}{extras}",
            signed(a.core_offset_mhz),
            a.power_limit_percent
        )
    }

    // ------------------------------------------------------------------ profile slots

    pub fn slots(&self) -> &[ProfileSlot] {
        &self.slots
    }

    /// Afterburner's two-step save: press Save, then a number.
    pub fn save_armed(&self) -> bool {
        self.save_armed
    }
    pub fn set_save_armed(&mut self, value: bool) {
        if self.save_armed == value {
            return;
        }
        self.save_armed = value;
        if value {
            self.set_status("Pick a slot (1-5) to store the current settings");
        }
    }

    /// Left-click on a slot: store into it when Save is armed, else load and apply it.
    pub fn on_slot_clicked(&mut self, number: i32) {
        let Some(slot) = self.slots.iter().find(|s| s.number == number) else { return };
        let name = slot.name();

        if self.save_armed {
            let mut p = self.build_profile_from_editor(&name);
            if let Err(e) = self.shared.store.save(&mut p) {
                self.set_status_error(format!("Could not save slot {number}: {e}"));
                return;
            }
            self.set_save_armed(false);
            self.refresh_profiles();
            self.refresh_slots();
            self.set_active_slot(number);
            self.set_selected_profile(Some(name.clone()));
            let notice = self.startup_save_notice(&name);
            self.set_status(format!("Saved current settings to slot {number}{notice}"));
            return;
        }

        let Some(mut p) = self.shared.store.load(&name).ok().flatten() else {
            self.set_status_error(format!(
                "Slot {number} is empty — press Save, then {number}, to store the current settings"
            ));
            return;
        };
        if !self.can_load_profile(&p) {
            return;
        }
        p.clamp_to(&self.caps);
        self.load_into_editor(&p);
        self.set_selected_profile(Some(name));
        self.set_active_slot(number);
        self.queue_apply(Some(number)); // Afterburner applies the moment you click a slot
    }

    /// Right-click on a slot, after the UI has confirmed. Wipes the stored profile.
    pub fn clear_slot(&mut self, number: i32) {
        let Some(slot) = self.slots.iter().find(|s| s.number == number && s.occupied).cloned() else { return };
        let name = slot.name();
        let _ = self.shared.store.delete(&name);
        if self.startup_profile.as_deref() == Some(name.as_str()) {
            self.set_startup_profile(None);
            self.set_apply_on_startup(false);
        }
        if self.selected_profile.as_deref() == Some(name.as_str()) {
            self.set_selected_profile(None);
        }
        self.refresh_profiles();
        self.refresh_slots();
        if slot.is_active {
            self.set_active_slot(0);
        }
        self.set_status(format!("Slot {number} cleared"));
    }

    /// True when a right-click on this slot has something to clear (the UI asks first).
    pub fn slot_is_occupied(&self, number: i32) -> bool {
        self.slots.iter().any(|s| s.number == number && s.occupied)
    }

    fn set_active_slot(&mut self, number: i32) {
        for s in &mut self.slots {
            s.is_active = s.number == number;
        }
    }

    /// Re-read which slots have a profile behind them, and summarise each for its tooltip.
    pub fn refresh_slots(&mut self) {
        let loaded: Vec<Option<TuningProfile>> =
            self.slots.iter().map(|s| self.shared.store.load(&s.name()).ok().flatten()).collect();
        let summaries: Vec<String> =
            loaded.iter().map(|p| p.as_ref().map(|p| self.describe_profile(p)).unwrap_or_default()).collect();
        for ((s, p), summary) in self.slots.iter_mut().zip(loaded).zip(summaries) {
            s.occupied = p.is_some();
            s.summary = summary;
        }
    }

    fn describe_profile(&self, p: &TuningProfile) -> String {
        if self.intel_tuning()
            && let Some(g) = p.gpu_name.as_deref().filter(|g| !g.is_empty() && *g != self.device.name)
        {
            return format!("Saved for {g}. Save a profile for the current GPU before applying.");
        }
        let mut bits = vec![
            format!("core {} MHz", signed(p.core_offset_mhz)),
            format!("mem {} {}", p.memory_offset_mhz, self.caps.memory_clock_unit),
            format!("power {}%", p.power_limit_percent),
            format!("temp {} {}", p.temp_limit_c, self.caps.temp_limit_unit()),
        ];
        if self.has_soft_temp_limit() && p.soft_thermal_limit_c > 0 && p.soft_thermal_limit_c < self.soft_temp_off_c() {
            bits.push(format!("soft temp {} °C", p.soft_thermal_limit_c));
        }
        if p.target_voltage_mv > 0 {
            bits.insert(0, format!("{} mV", p.target_voltage_mv));
        }
        bits.push(match p.fan_mode {
            FanMode::Fixed => format!("fan {}%", p.fixed_fan_percent),
            FanMode::Curve => "fan curve".into(),
            FanMode::Auto => "fan auto".into(),
        });
        bits.join(" · ")
    }

    // ------------------------------------------------------------------ named profiles

    pub fn profiles(&self) -> &[String] {
        &self.profiles
    }
    pub fn selected_profile(&self) -> Option<&str> {
        self.selected_profile.as_deref()
    }
    pub fn set_selected_profile(&mut self, value: Option<String>) {
        if self.selected_profile != value {
            self.selected_profile = value.clone();
            if let Some(v) = value {
                self.profile_name_input = v;
            }
        }
    }

    pub fn refresh_profiles(&mut self) {
        let keep = self.selected_profile.clone();
        self.profiles = self.shared.store.list_profile_names();
        let keep = keep.filter(|k| self.profiles.contains(k));
        self.set_selected_profile(keep);
    }

    /// Store the editor under the typed name.
    pub fn save_profile(&mut self) {
        let name = self.profile_name_input.trim().to_string();
        if name.is_empty() {
            return;
        }
        let mut p = self.build_profile_from_editor(&name);
        if let Err(e) = self.shared.store.save(&mut p) {
            self.set_status_error(format!("Could not save '{name}': {e}"));
            return;
        }
        self.refresh_profiles();
        self.set_selected_profile(Some(name.clone()));
        let notice = self.startup_save_notice(&name);
        self.set_status(format!("Saved profile '{name}'{notice}"));
    }

    fn can_load_profile(&mut self, profile: &TuningProfile) -> bool {
        match profile.gpu_name.as_deref() {
            Some(g) if self.intel_tuning() && !g.is_empty() && g != self.device.name => {
                self.set_status_error(format!(
                    "Profile belongs to {g}. Save a new profile for this GPU; Intel tuning units differ."
                ));
                false
            }
            _ => true,
        }
    }

    /// Load the selected named profile into the editor without applying it.
    pub fn load_selected_profile(&mut self) {
        let Some(sel) = self.selected_profile.clone() else { return };
        let Some(mut p) = self.shared.store.load(&sel).ok().flatten() else {
            self.set_status_error("Profile not found");
            self.refresh_profiles();
            return;
        };
        if !self.can_load_profile(&p) {
            return;
        }
        match p.gpu_name.as_deref() {
            Some(g) if !g.is_empty() && g != self.device.name => {
                // The C# set only the text here, keeping whatever colour the line already had.
                self.status = format!("Note: profile was made for {g}, values will be clamped to this GPU's ranges");
            }
            _ => self.set_status(format!("Loaded '{}' — press Apply to send to GPU", p.name)),
        }
        p.clamp_to(&self.caps);
        self.load_into_editor(&p);
        self.pending_changes = true;
    }

    /// Delete the selected named profile (after the UI has confirmed).
    pub fn delete_selected_profile(&mut self) {
        let Some(sel) = self.selected_profile.clone() else { return };
        let _ = self.shared.store.delete(&sel);
        if self.startup_profile.as_deref() == Some(sel.as_str()) {
            self.set_startup_profile(None);
            self.set_apply_on_startup(false);
        }
        self.refresh_profiles();
    }

    fn startup_save_notice(&self, name: &str) -> String {
        if self.apply_on_startup && self.startup_profile.as_deref() == Some(name) {
            "; startup copy unchanged — toggle Startup off/on to update it".into()
        } else {
            String::new()
        }
    }

    // ------------------------------------------------------------------ startup task

    pub fn apply_on_startup(&self) -> bool {
        self.apply_on_startup
    }
    /// Tick or untick Startup: registers (after a confirming trial) or removes the logon task.
    pub fn set_apply_on_startup(&mut self, value: bool) {
        if self.apply_on_startup != value {
            self.apply_on_startup = value;
            self.queue_startup_update();
        }
    }
    pub fn startup_profile(&self) -> Option<&str> {
        self.startup_profile.as_deref()
    }
    pub fn set_startup_profile(&mut self, value: Option<String>) {
        if self.startup_profile != value {
            self.startup_profile = value.clone();
            self.shared.settings().startup_profile = value;
            if self.apply_on_startup {
                self.queue_startup_update();
            }
        }
    }

    fn queue_startup_update(&mut self) {
        let plan = if self.apply_on_startup {
            match self.startup_profile.clone().filter(|n| !n.is_empty()) {
                None => StartupPlan::NoProfile,
                Some(name) => {
                    let candidate = match self.shared.store.load(&name).ok().flatten() {
                        None => Err("Profile not found".to_string()),
                        Some(p) if !self.can_load_profile(&p) => Err(self.status.clone()),
                        Some(p) => Ok(p),
                    };
                    StartupPlan::Register { name, candidate: Box::new(candidate) }
                }
            }
        } else {
            StartupPlan::Unregister
        };
        self.queue(Job::new("Updating the startup task…", move |sh: &Shared| {
            let result = match plan {
                StartupPlan::NoProfile => None,
                StartupPlan::Register { name, candidate } => Some((|| {
                    let candidate = (*candidate)?;
                    let errors = sh.trial_profile(&candidate);
                    if !ok(&errors) {
                        return Err(errors.join("; "));
                    }
                    let exe = std::env::current_exe().map_err(|_| "Cannot resolve exe path".to_string())?;
                    // Registration only points at a confirmed snapshot, never the editable slot.
                    match sh.recovery().as_mut() {
                        Some(r) => r.approve_startup(&name).map_err(|e| e.message)?,
                        None => return Err("Profile recovery is unavailable".into()),
                    }
                    sh.startup.register(&exe.to_string_lossy(), &name).map_err(|e| e.message)?;
                    Ok(format!("Startup task registered for '{name}'"))
                })()),
                StartupPlan::Unregister => {
                    Some(sh.startup.unregister().map(|_| "Startup task removed".to_string()).map_err(|e| e.message))
                }
            };
            Outcome(OutcomeKind::Startup {
                result,
                registered: sh.startup.exists(),
                recovery_idle: sh.recovery_idle(),
                applied: sh.service.applied_profile(),
            })
        }));
    }

    // ------------------------------------------------------------------ build / load

    pub fn build_profile_from_editor(&self, name: &str) -> TuningProfile {
        let g = |f| self.get(f);
        let lock_on = self.has_clock_lock() && !self.clock_lock_is_off();
        TuningProfile {
            name: name.to_string(),
            gpu_name: Some(self.device.name.clone()),
            core_offset_mhz: g(Field::CoreOffset),
            memory_offset_mhz: g(Field::MemoryOffset),
            power_limit_percent: g(Field::PowerLimit),
            temp_limit_c: g(Field::TempLimit),
            voltage_boost_percent: g(Field::VoltageBoost),
            voltage_offset_mv: g(Field::VoltageOffset),
            voltage_rail_max_mv: if self.has_voltage_rail() { g(Field::VoltageRailMax) } else { 0 },
            msvdd_rail_max_mv: if self.has_msvdd_rail() { g(Field::MsvddRailMax) } else { 0 },
            voltage_rail_floor_mv: if self.has_voltage_rail() { g(Field::VoltageRailFloor) } else { 0 },
            msvdd_rail_floor_mv: if self.has_msvdd_rail() { g(Field::MsvddRailFloor) } else { 0 },
            xbar_offset_mhz: if self.has_xbar() { g(Field::XbarOffset) } else { 0 },
            nvvdd_ocp_milliamps: if self.has_ocp() { g(Field::NvvddOcpAmps) * 1000 } else { 0 },
            msvdd_ocp_milliamps: if self.has_ocp() { g(Field::MsvddOcpAmps) * 1000 } else { 0 },
            sys_offset_mhz: if self.has_sys() { g(Field::SysOffset) } else { 0 },
            clock_lock_min_mhz: if lock_on { g(Field::ClockLockMin) } else { 0 },
            clock_lock_max_mhz: if lock_on { g(Field::ClockLockMax) } else { 0 },
            video_offset_mhz: if self.has_video() { g(Field::VideoOffset) } else { 0 },
            xoc_armed: XocLever(self.xoc_armed.0 | self.main_clock_levers().0),
            // The cap has no slider here (the curve editor's flatten owns it), but it has a value:
            // at or above the reachable ceiling it caps nothing and is stored as 0.
            target_voltage_mv: if g(Field::TargetVoltage) < self.reachable_ceiling_mv() {
                g(Field::TargetVoltage)
            } else {
                0
            },
            zero_rpm: self.zero_rpm,
            memory_timing_level: self.memory_timing_index,
            fan_mode: FanMode::from_number(self.fan_mode_index as i64).unwrap_or(FanMode::Auto),
            fixed_fan_percent: g(Field::FixedFan),
            fixed_fan_percents: self.per_fan_percents.clone(),
            fan_curve: self.editor_curve.clone_config(),
            soft_thermal_limit_c: self.soft_temp_limit_c(),
            ..TuningProfile::default()
        }
    }

    pub fn load_into_editor(&mut self, p: &TuningProfile) {
        self.loading_editor = true;
        let c = self.caps.clone();
        self.set(Field::CoreOffset, p.core_offset_mhz);
        self.set(Field::MemoryOffset, p.memory_offset_mhz);
        self.set(Field::PowerLimit, p.power_limit_percent);
        self.set(Field::TempLimit, p.temp_limit_c);
        // 0 is off: show the card's own limit (88 °C on a 5070 Ti), the default.
        let soft = if p.soft_thermal_limit_c > 0 { p.soft_thermal_limit_c } else { self.soft_temp_off_c() };
        self.set(Field::SoftTempLimit, soft);
        self.set(Field::VoltageBoost, p.voltage_boost_percent);
        self.set(Field::VoltageOffset, p.voltage_offset_mv);
        let rail = if p.voltage_rail_max_mv > 0 { p.voltage_rail_max_mv } else { self.stock_nvvdd_max_mv() };
        self.set(Field::VoltageRailMax, rail);
        let msvdd = if p.msvdd_rail_max_mv > 0 { p.msvdd_rail_max_mv } else { self.stock_msvdd_max_mv() };
        self.set(Field::MsvddRailMax, msvdd);
        let pick = |v: i32, fallback: i32| if v > 0 { v } else { fallback };
        self.set(Field::VoltageRailFloor, pick(p.voltage_rail_floor_mv, c.voltage_rail_stock_floor_mv));
        self.set(Field::MsvddRailFloor, pick(p.msvdd_rail_floor_mv, c.msvdd_rail_stock_floor_mv));
        self.set(Field::XbarOffset, p.xbar_offset_mhz);
        self.set(Field::NvvddOcpAmps, pick(p.nvvdd_ocp_milliamps, c.nvvdd_ocp_stock_milliamps) / 1000);
        self.set(Field::MsvddOcpAmps, pick(p.msvdd_ocp_milliamps, c.msvdd_ocp_stock_milliamps) / 1000);
        self.set(Field::SysOffset, p.sys_offset_mhz);
        // 0/0 in a profile is "unpinned", which the slider shows as the full range.
        self.set(Field::ClockLockMin, pick(p.clock_lock_min_mhz, c.clock_lock_min_mhz));
        self.set(Field::ClockLockMax, pick(p.clock_lock_max_mhz, c.clock_lock_max_mhz));
        self.set(Field::VideoOffset, p.video_offset_mhz);
        // Only the genuinely gated XOC bits live in editor state; the main clocks are added back
        // by build_profile_from_editor for every Apply.
        self.xoc_armed = XocLever(p.xoc_armed.0 & !self.main_clock_levers().0);
        // "No cap" is the top of what the card can reach, not the stock ceiling.
        let target = if p.target_voltage_mv > 0 { p.target_voltage_mv } else { self.reachable_ceiling_mv() };
        self.set(Field::TargetVoltage, target);
        self.set_zero_rpm(p.zero_rpm);
        self.set_memory_timing_index(p.memory_timing_level);
        self.set_fan_mode_index(p.fan_mode.as_number());
        self.set(Field::FixedFan, p.fixed_fan_percent);
        self.per_fan_percents = p.fixed_fan_percents.clone();
        self.editor_curve = p.fan_curve.clone_config();
        self.loading_editor = false;
    }

    // ------------------------------------------------------------------ actions

    /// Apply button: trial the editor as a profile.
    pub fn apply(&mut self) {
        self.queue_apply(None);
    }

    fn queue_apply(&mut self, slot: Option<i32>) {
        let p = self.build_profile_from_editor(self.selected_profile.as_deref().unwrap_or("Session"));
        self.queue(Job::new("Applying…", move |sh: &Shared| {
            let errors = sh.trial_profile(&p);
            let name_saved = sh.store.load(&p.name).ok().flatten().is_some();
            Outcome(OutcomeKind::Apply {
                profile_name: p.name.clone(),
                errors,
                slot,
                name_saved,
                recovery_idle: sh.recovery_idle(),
                applied: sh.service.applied_profile(),
                readback: Readback::gather(sh),
            })
        }));
    }

    /// Enable / Disable one XOC lever: writes that one lever now and nothing else.
    pub fn toggle_lever(&mut self, lever: XocLever) {
        let on = !self.xoc_armed.has(lever);
        self.set_lever(lever, on);
    }

    pub fn set_lever(&mut self, lever: XocLever, on: bool) {
        let p = self.build_profile_from_editor(self.selected_profile.as_deref().unwrap_or("Session"));
        self.queue(Job::new(if on { "Arming…" } else { "Disarming…" }, move |sh: &Shared| {
            let errors = sh.service.set_xoc_lever(&p, lever, on);
            Outcome(OutcomeKind::Lever { lever, on, errors, readback: Readback::gather(sh) })
        }));
    }

    /// Reset to Defaults: the driver's own values, every gated lever included.
    pub fn reset(&mut self) {
        self.queue(Job::new("Resetting…", |sh: &Shared| {
            let result = sh.service.reset_to_defaults();
            Outcome(OutcomeKind::Reset { result, readback: Readback::gather(sh) })
        }));
    }

    /// Refresh only rail ceilings whose XOC control is disabled: those are read-only views of the
    /// driver's live policy. Assigned directly so a read-back never marks the editor dirty.
    fn refresh_driver_owned_rail_ceilings(&mut self, live: &Result<GpuTuningState>) {
        let Ok(live) = live else { return };
        let c = &self.caps;
        if !self.xoc_armed.has(XocLever::NVVDD) && live.voltage_rail_max_mv > 0 {
            self.vals[Field::VoltageRailMax as usize] =
                roch_core::util::clamp_i32(live.voltage_rail_max_mv, c.voltage_rail_min_mv, c.voltage_rail_max_mv);
        }
        if !self.xoc_armed.has(XocLever::MSVDD) && live.msvdd_rail_max_mv > 0 {
            self.vals[Field::MsvddRailMax as usize] =
                roch_core::util::clamp_i32(live.msvdd_rail_max_mv, c.msvdd_rail_min_mv, c.msvdd_rail_max_mv);
        }
    }

    fn show_disarmed_values(&mut self, lever: XocLever, live: &Result<GpuTuningState>) {
        let c = self.caps.clone();
        if lever == XocLever::NVVDD {
            self.refresh_driver_owned_rail_ceilings(live);
            self.set(Field::VoltageRailFloor, c.voltage_rail_stock_floor_mv);
        } else if lever == XocLever::MSVDD {
            self.refresh_driver_owned_rail_ceilings(live);
            self.set(Field::MsvddRailFloor, c.msvdd_rail_stock_floor_mv);
        } else if lever == XocLever::XBAR {
            self.set(Field::XbarOffset, 0);
        } else if lever == XocLever::SYS {
            self.set(Field::SysOffset, 0);
        } else if lever == XocLever::VIDEO {
            self.set(Field::VideoOffset, 0);
        } else if lever == XocLever::CLOCK_RANGE {
            // Full range is how this slider says "unpinned"; its floor cannot reach zero.
            self.set(Field::ClockLockMin, c.clock_lock_min_mhz);
            self.set(Field::ClockLockMax, c.clock_lock_max_mhz);
        }
    }

    // ------------------------------------------------------------------ job plumbing

    fn queue(&mut self, job: Job) {
        self.queued.push(job);
    }

    /// Queue a driver call from a window (the fan window's `set_fans`, the curve editor's writes):
    /// `run` executes on the worker thread and returns what to do with the view model once back on
    /// the UI thread. Write buttons stay disabled while it runs ([`Self::busy`]).
    pub fn queue_custom(&mut self, label: &'static str, run: impl FnOnce(&Shared) -> ApplyOnUi + Send + 'static) {
        self.queue(Job::new(label, move |sh: &Shared| Outcome(OutcomeKind::Custom(run(sh)))));
    }

    /// Jobs waiting to be run. The app moves them to its worker thread and later hands each
    /// [`Outcome`] to [`Self::finish`].
    pub fn take_jobs(&mut self) -> Vec<Job> {
        let jobs = std::mem::take(&mut self.queued);
        if let Some(last) = jobs.last() {
            self.busy_label = Some(last.label);
        }
        self.in_flight += jobs.len();
        jobs
    }

    /// Something is writing to the card; write buttons stay disabled until it finishes.
    pub fn busy(&self) -> bool {
        self.in_flight > 0 || !self.queued.is_empty()
    }
    pub fn busy_label(&self) -> Option<&'static str> {
        if let Some(job) = self.queued.last() {
            return Some(job.label);
        }
        if self.in_flight > 0 { self.busy_label.or(Some("Working…")) } else { None }
    }

    /// Run every queued job on this thread (tests, and the headless startup path).
    pub fn run_jobs_inline(&mut self) {
        for job in self.take_jobs() {
            let outcome = job.run(&self.shared.clone());
            self.finish(outcome);
        }
    }

    /// Take a finished job's result back on the UI thread.
    pub fn finish(&mut self, outcome: Outcome) {
        self.in_flight = self.in_flight.saturating_sub(1);
        match outcome.0 {
            OutcomeKind::Custom(apply) => apply(self),
            OutcomeKind::Apply { profile_name, errors, slot, name_saved, recovery_idle, applied, readback } => {
                // Notes say what the apply did differently, not that it failed.
                if ok(&errors) {
                    if errors.is_empty() {
                        self.set_status(format!("Applied at {}", now_hms()));
                    } else {
                        self.set_status(errors.join("  |  "));
                    }
                    self.pending_changes = false;
                    // Disabled rail editors show the driver-owned live ceilings; the boost can
                    // move them, so do not leave startup values on screen.
                    self.refresh_driver_owned_rail_ceilings(&readback.state);
                    // Only a name that can be loaded again ("Session" is never written).
                    if name_saved {
                        self.shared.settings().last_profile_by_gpu.insert(self.device.name.clone(), profile_name);
                    }
                    if let Some(n) = slot {
                        self.set_status(format!("Slot {n} applied at {}", now_hms()));
                    }
                } else {
                    self.set_status_error(errors.join("  |  "));
                    if recovery_idle && let Some(a) = applied {
                        self.load_into_editor(&a);
                        self.pending_changes = false;
                    }
                }
                self.applied_summary = self.build_applied_summary(&readback);
            }
            OutcomeKind::Reset { result, readback } => {
                match result {
                    Ok(()) => {
                        let stock = TuningProfile::stock(&self.caps, Some(&self.device.name));
                        self.load_into_editor(&stock);
                        self.xoc_armed = XocLever::NONE;
                        self.pending_changes = false;
                        self.set_status("Reset to driver defaults");
                    }
                    Err(e) => self.set_status_error(e.message),
                }
                self.applied_summary = self.build_applied_summary(&readback);
            }
            OutcomeKind::Lever { lever, on, errors, readback } => {
                let success = ok(&errors);
                // The button only moves if the write behind it landed.
                if success {
                    self.xoc_armed = self.xoc_armed.with(lever, on);
                }
                self.status = if errors.is_empty() {
                    format!("{} {}", lever_label(lever), if on { "armed" } else { "disarmed" })
                } else {
                    errors.join("  |  ")
                };
                self.status_is_error = !success;
                if !on && success {
                    self.show_disarmed_values(lever, &readback.state);
                }
                self.applied_summary = self.build_applied_summary(&readback);
            }
            OutcomeKind::Startup { result, registered, recovery_idle, applied } => {
                match result {
                    None => self.set_status_error("Pick a startup profile first"),
                    Some(Ok(status)) => {
                        if self.apply_on_startup {
                            self.accepted_startup_profile = self.startup_profile.clone();
                        }
                        self.set_status(status);
                    }
                    Some(Err(message)) => {
                        self.set_status_error(format!("Startup task: {message}"));
                        self.startup_profile = self.accepted_startup_profile.clone();
                        self.shared.settings().startup_profile = self.startup_profile.clone();
                        if recovery_idle && let Some(a) = applied {
                            self.load_into_editor(&a);
                        }
                    }
                }
                // Settle the tick on what is actually registered, not on what was asked for.
                self.apply_on_startup = registered;
                self.shared.settings().apply_on_startup = registered;
                self.shared.save_settings();
            }
        }
    }
}

const NOTHING_APPLIED: &str = "Nothing applied this session (driver state shown)";

fn lever_label(lever: XocLever) -> String {
    match lever {
        XocLever::NVVDD => "NVVDD range".into(),
        XocLever::MSVDD => "MSVDD range".into(),
        XocLever::XBAR => "XBAR clock offset".into(),
        XocLever::SYS => "SYS clock offset".into(),
        XocLever::VIDEO => "Video clock offset".into(),
        XocLever::CLOCK_RANGE => "Clock range".into(),
        other => other.to_string(),
    }
}
