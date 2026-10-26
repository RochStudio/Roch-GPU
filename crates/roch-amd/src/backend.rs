//! AMD backend, Overdrive 8 over ADL. Covers RDNA and newer.
//!
//! Everything it can do is discovered at runtime rather than hardcoded: the driver hands back a
//! capability bitmask plus a per-feature table of min/max/default, and a feature whose min equals
//! its max is one this card does not expose. On an RX 9070 XT that leaves core-clock offset,
//! voltage offset, memory clock, power limit, memory timing, zero RPM and a 5-point fan curve —
//! and correctly hides the V/F curve and temperature limit, which RDNA 4 does not offer.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::fmt::Write as _;
use std::ptr::null_mut;

use roch_core::models::graphics_info::trim_decimals;
use roch_core::native::c_str;
use roch_core::util::{clamp_i32, round_even_i32};
use roch_core::{
    FanCurve, FanMode, GpuBackend, GpuCapabilities, GpuDevice, GpuError, GpuGraphicsInfo, GpuIdentity, GpuTelemetry,
    GpuTuningState, Result, VoltageControlStyle, bail,
};

use crate::adl::{
    self, ADL_OK, ALLOCATOR, AMD_VENDOR_ID, AdapterInfo, AdlApi, MemoryInfoX4, OD8_COUNT, OD8_FEATURE_COUNT, Od8Id,
    describe, need, od8_feature, pm_log,
};
use crate::adlx::AdlxTelemetry;
use crate::mapper::AmdTelemetryMapper;

/// min/max/default for one OD8 feature. Unsupported features report min == max.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Od8Range {
    min: i32,
    max: i32,
    default: i32,
}

impl Od8Range {
    fn supported(&self) -> bool {
        self.min != self.max
    }
}

/// Size of the PMLog payload: `int size; { int supported; int value; }[256]`.
const PM_LOG_BYTES: usize = 4 + 256 * 8;

const FAN_CURVE_TEMP_IDS: [Od8Id; 5] = [
    Od8Id::FanCurveTemperature1,
    Od8Id::FanCurveTemperature2,
    Od8Id::FanCurveTemperature3,
    Od8Id::FanCurveTemperature4,
    Od8Id::FanCurveTemperature5,
];

const FAN_CURVE_SPEED_IDS: [Od8Id; 5] =
    [Od8Id::FanCurveSpeed1, Od8Id::FanCurveSpeed2, Od8Id::FanCurveSpeed3, Od8Id::FanCurveSpeed4, Od8Id::FanCurveSpeed5];

/// The AMD backend (`AdlBackend` in the original).
pub struct AmdBackend {
    api: Option<AdlApi>,
    context: *mut c_void,
    devices: Vec<GpuDevice>,
    /// Device index -> ADL adapter index.
    adapter_index: Vec<i32>,
    /// Same index -> PCI identity.
    pnp_ids: Vec<String>,
    adapter_failures: Vec<String>,
    // Per adapter: a second AMD card has its own limits, and clamping one card's writes to the
    // other's ranges would silently truncate them.
    caps: HashMap<usize, i32>,
    ranges_by_gpu: HashMap<usize, [Od8Range; OD8_COUNT]>,
    adlx: AdlxTelemetry,
}

/// The original's class name.
pub type AdlBackend = AmdBackend;

// SAFETY: the ADL context is not thread-affine; every call goes through `&mut self`, and the
// tuning service serialises all driver calls behind one lock.
unsafe impl Send for AmdBackend {}

impl Default for AmdBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AmdBackend {
    fn drop(&mut self) {
        self.dispose();
        // Like a P/Invoke library, the driver DLL is never unloaded: driver worker threads may
        // outlive Main_Control_Destroy, and unloading their code under them would crash.
        if let Some(api) = self.api.take() {
            std::mem::forget(api);
        }
    }
}

// ------------------------------------------------------------------ pure helpers

/// Memory timing choices for an OD8 AcTiming range. Keeps UI/profile indices separate from
/// driver preset IDs; does not invent timing names for undocumented levels, or allocate an
/// unbounded list from a malformed driver range.
pub fn build_timing_options(min: i32, max: i32) -> Vec<String> {
    if min < 0 || max <= min || max > 255 {
        return Vec::new();
    }
    (min..=max)
        .map(|id| match id {
            0 => "Default".to_string(),
            1 => "Fast timing".to_string(),
            _ => format!("Driver preset {id}"),
        })
        .collect()
}

/// There is no "manual fan" flag on this driver — the five-point hardware curve is the only fan
/// mechanism, so the mode has to be inferred from the table itself:
///
/// - the driver's own default table means nothing has overridden it — auto;
/// - five equal duties is what `set_fan_speed` writes for a fixed speed;
/// - anything else is a curve.
///
/// A hand-drawn flat curve therefore reads back as fixed, which is precisely what it is once it
/// reaches the hardware.
pub fn infer_fan_mode(temps: &[i32], speeds: &[i32], temp_defaults: &[i32], speed_defaults: &[i32]) -> (FanMode, i32) {
    if speeds.is_empty() {
        return (FanMode::Auto, 0);
    }
    let untouched = speeds.len() == speed_defaults.len()
        && temps.len() == temp_defaults.len()
        && speeds == speed_defaults
        && temps == temp_defaults;
    if untouched {
        return (FanMode::Auto, 0);
    }
    if speeds.iter().any(|&s| s != speeds[0]) {
        return (FanMode::Curve, 0);
    }
    (FanMode::Fixed, speeds[0])
}

/// `DEV_xxxx&SUBSYS_ssssvvvv&REV_rr` (case-insensitive), first match anywhere in the PnP string.
/// Returns (device id, subsystem vendor, revision).
fn parse_pnp_identity(pnp: &str) -> Option<(i32, i32, i32)> {
    fn hex(b: &[u8]) -> Option<i32> {
        if b.is_empty() || !b.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        i32::from_str_radix(std::str::from_utf8(b).ok()?, 16).ok()
    }
    fn tag(b: &[u8], at: usize, text: &str) -> bool {
        b.get(at..at + text.len()).is_some_and(|s| s.eq_ignore_ascii_case(text.as_bytes()))
    }
    let b = pnp.as_bytes();
    for start in 0..b.len() {
        let mut i = start;
        if !tag(b, i, "DEV_") {
            continue;
        }
        i += 4;
        let Some(dev) = b.get(i..i + 4).and_then(hex) else { continue };
        i += 4;
        if !tag(b, i, "&SUBSYS_") {
            continue;
        }
        i += 8;
        let Some(_subsys_device) = b.get(i..i + 4).and_then(hex) else { continue };
        i += 4;
        let Some(board_vendor) = b.get(i..i + 4).and_then(hex) else { continue };
        i += 4;
        if !tag(b, i, "&REV_") {
            continue;
        }
        i += 5;
        let Some(rev) = b.get(i..i + 2).and_then(hex) else { continue };
        return Some((dev, board_vendor, rev));
    }
    None
}

/// Throttler status bits → text.
fn throttle_text(status: f64) -> String {
    if status.is_nan() || status == 0.0 {
        return "None".into();
    }
    let s = status as i32;
    let mut hits = Vec::new();
    if s & 1 != 0 {
        hits.push("Power");
    }
    if s & 2 != 0 {
        hits.push("Thermal");
    }
    if s & 4 != 0 {
        hits.push("Current");
    }
    if hits.is_empty() { "Other".into() } else { hits.join(" + ") }
}

/// One sensor out of a filled PMLog buffer (as i32 words), NaN when the driver marks it
/// unsupported.
fn pm_value(buf: &[i32], id: i32) -> f64 {
    let o = 1 + id as usize * 2;
    match (buf.get(o), buf.get(o + 1)) {
        (Some(&supported), Some(&value)) if supported != 0 => value as f64,
        _ => f64::NAN,
    }
}

/// The string at `offset` in a zeroed driver buffer, up to its NUL; "" when blank.
fn ansi_at(buf: &[u8], offset: usize) -> String {
    c_str(buf.get(offset..).unwrap_or(&[]))
}

/// .NET `{x:0}`.
fn f0(v: f64) -> String {
    if v.is_nan() { "NaN".into() } else { format!("{:.0}", v.round()) }
}

fn missing_adapter(gpu: usize) -> GpuError {
    GpuError::new(format!("No AMD adapter at index {gpu}."))
}

impl AmdBackend {
    pub fn new() -> Self {
        Self {
            api: None,
            context: null_mut(),
            devices: Vec::new(),
            adapter_index: Vec::new(),
            pnp_ids: Vec::new(),
            adapter_failures: Vec::new(),
            caps: HashMap::new(),
            ranges_by_gpu: HashMap::new(),
            adlx: AdlxTelemetry::new(),
        }
    }

    /// See [`build_timing_options`].
    pub fn build_timing_options(min: i32, max: i32) -> Vec<String> {
        build_timing_options(min, max)
    }

    /// See [`infer_fan_mode`].
    pub fn infer_fan_mode(
        temps: &[i32],
        speeds: &[i32],
        temp_defaults: &[i32],
        speed_defaults: &[i32],
    ) -> (FanMode, i32) {
        infer_fan_mode(temps, speeds, temp_defaults, speed_defaults)
    }

    fn api(&self) -> Result<&AdlApi> {
        self.api.as_ref().ok_or_else(|| GpuError::new("The AMD backend has not been initialised."))
    }

    /// Fails once the context is gone. Every telemetry call checks this first, so a poll still in
    /// flight after dispose fails cleanly instead of handing ADL a dead pointer.
    fn live(&self) -> Result<*mut c_void> {
        if self.context.is_null() {
            bail!("The AMD backend has been disposed.");
        }
        Ok(self.context)
    }

    fn adapter(&self, gpu: usize) -> Result<i32> {
        self.adapter_index.get(gpu).copied().ok_or_else(|| missing_adapter(gpu))
    }

    /// Release ADLX, destroy the ADL context and forget every adapter. Note that the ADLX helper
    /// is disposed permanently, as in the original.
    fn dispose(&mut self) {
        if self.context.is_null() {
            return;
        }
        // ADLX borrows the caller-owned ADL context, so it goes first.
        self.adlx.dispose();
        if let Some(destroy) = self.api.as_ref().and_then(|a| a.main_control_destroy) {
            // SAFETY: the context came from Main_Control_Create and is destroyed once.
            unsafe { destroy(self.context) };
        }
        self.context = null_mut();
        self.devices.clear();
        self.adapter_index.clear();
        self.pnp_ids.clear();
        self.caps.clear();
        self.ranges_by_gpu.clear();
        self.adapter_failures.clear();
    }

    fn enumerate_adapters(&mut self) -> Result<()> {
        let ctx = self.context;
        let num_get = need(self.api()?.number_of_adapters_get, "ADL2_Adapter_NumberOfAdapters_Get")?;
        let mut num = 0i32;
        // SAFETY: out-pointer to a local.
        if unsafe { num_get(ctx, &mut num) } != ADL_OK || num <= 0 {
            return Ok(());
        }
        let info_get = need(self.api()?.adapter_info_get, "ADL2_Adapter_AdapterInfo_Get")?;
        let sz = size_of::<AdapterInfo>() as i32;
        let Some(total) = sz.checked_mul(num) else { return Ok(()) };
        let mut infos = vec![AdapterInfo::zeroed(); num as usize];
        // SAFETY: `infos` holds exactly `total` bytes of AdapterInfo, the size we report.
        if unsafe { info_get(ctx, infos.as_mut_ptr().cast(), total) } != ADL_OK {
            return Ok(());
        }

        // ADL reports one entry per display output, so the same GPU appears several times.
        let mut seen = HashSet::new();
        for ai in &infos {
            if ai.i_vendor_id != AMD_VENDOR_ID || ai.i_exist == 0 {
                continue;
            }
            let key = format!("{}.{}.{}", ai.i_bus_number, ai.i_device_number, ai.i_function_number);
            if seen.contains(&key) {
                continue;
            }
            // A missing OD8 export is not an adapter failure: the original let it escape.
            need(self.api()?.od8_init_setting_x2_get, "ADL2_Overdrive8_Init_SettingX2_Get")?;

            // Integrated graphics can enumerate before a supported discrete card. Probe every
            // candidate, retaining the native ADL index only for usable adapters. Mark the PCI
            // device seen only after success: another display entry may work.
            let gpu = self.devices.len();
            let name = c_str(&ai.str_adapter_name);
            self.adapter_index.push(ai.i_adapter_index);
            self.pnp_ids.push(c_str(&ai.str_pnp_string));
            if let Err(e) = self.load_ranges(gpu) {
                self.adapter_index.truncate(gpu);
                self.pnp_ids.truncate(gpu);
                self.caps.remove(&gpu);
                self.ranges_by_gpu.remove(&gpu);
                self.adapter_failures.push(format!("{name} (ADL {}): {}", ai.i_adapter_index, e.message));
                continue;
            }
            seen.insert(key);

            let driver_version = self.read_driver_version(&c_str(&ai.str_driver_path));
            let vram_megabytes = self.read_vram_megabytes(ai.i_adapter_index);
            let bios_version = self.read_bios_version(ai.i_adapter_index);
            self.devices.push(GpuDevice {
                index: self.devices.len(),
                // A blank ADL name stays blank (the original's "AMD GPU" fallback only caught null).
                name,
                vendor: "AMD".into(),
                bus_id: format!("PCI bus {}, device {}", ai.i_bus_number, ai.i_device_number),
                driver_version,
                vram_megabytes,
                bios_version,
            });
        }
        Ok(())
    }

    // ---- identity ---------------------------------------------------------
    //
    // These are best-effort. Each entry point is optional on a given driver, so a missing one
    // simply yields "unknown", which the UI then omits instead of printing "0 GB".

    /// Driver version. ADL is asked first, and the adapter's own driver registry key is the
    /// fallback — that key is where AMD records both the Windows driver version and the Adrenalin
    /// release number, and it answers on drivers whose ADL version export returns nothing.
    fn read_driver_version(&self, driver_path: &str) -> String {
        let from_adl = (|| {
            let api = self.api.as_ref()?;
            let mut buf = vec![0u8; 4096];
            // ADLVersionsInfoX2 and ADLVersionsInfo both open with char strDriverVer[256], so the
            // layout difference after that field does not matter here.
            let f = api.graphics_versions_x2_get.or(api.graphics_versions_get)?;
            // SAFETY: 4096 zeroed bytes is larger than either versions struct.
            if unsafe { f(self.context, buf.as_mut_ptr().cast()) } != ADL_OK {
                return None;
            }
            let driver = ansi_at(&buf, 0);
            let catalyst = ansi_at(&buf, 256);
            Some(if !catalyst.is_empty() { catalyst } else { driver })
        })()
        .unwrap_or_default();
        if !from_adl.is_empty() {
            return from_adl;
        }
        read_driver_version_from_registry(driver_path)
    }

    /// vBIOS version string. Empty when unavailable.
    fn read_bios_version(&self, adapter: i32) -> String {
        let Some(f) = self.api.as_ref().and_then(|a| a.adapter_video_bios_info_get) else { return String::new() };
        let mut buf = vec![0u8; 1024];
        // SAFETY: ADLBiosInfo is {partNumber[256], version[256], date[256]} — 768 of our 1024 bytes.
        if unsafe { f(self.context, adapter, buf.as_mut_ptr().cast()) } == ADL_OK {
            ansi_at(&buf, 256)
        } else {
            String::new()
        }
    }

    /// VRAM in MB, or 0 when the driver does not report a believable figure.
    fn read_vram_megabytes(&self, adapter: i32) -> i64 {
        let Some(f) = self.api.as_ref().and_then(|a| a.adapter_memory_info2_get.or(a.adapter_memory_info_get)) else {
            return 0;
        };
        let mut buf = vec![0u8; 2048];
        // SAFETY: 2048 zeroed bytes is larger than ADLMemoryInfo2 / ADLMemoryInfo.
        if unsafe { f(self.context, adapter, buf.as_mut_ptr().cast()) } != ADL_OK {
            return 0;
        }
        // Both structs open with long long iMemorySize.
        let bytes = buf.first_chunk::<8>().map(|b| i64::from_le_bytes(*b)).unwrap_or(0);
        let mb = bytes / (1024 * 1024);
        // Sanity-check rather than trust it: a wrong struct shape shows up as a nonsense number,
        // and "unknown" is a better answer than "1048576 GB".
        if (256..=262144).contains(&mb) { mb } else { 0 }
    }

    fn read_memory_identity(&self, adapter: i32) -> (String, String) {
        let Some(f) = self.api.as_ref().and_then(|a| a.adapter_memory_info_x4_get) else {
            return (String::new(), String::new());
        };
        let mut buf = vec![0u8; size_of::<MemoryInfoX4>()];
        // SAFETY: the buffer is exactly one zeroed ADLMemoryInfoX4.
        if unsafe { f(self.context, adapter, buf.as_mut_ptr().cast()) } != ADL_OK {
            return (String::new(), String::new());
        }
        // SAFETY: the buffer is size_of::<MemoryInfoX4>() bytes and the struct is plain data.
        let info: MemoryInfoX4 = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
        let vendor = match info.i_vram_vendor_rev_id {
            1 => "Samsung".to_string(),
            v if v > 0 => format!("Vendor {v}"),
            _ => String::new(),
        };
        (c_str(&info.str_memory_type), vendor)
    }

    // ------------------------------------------------------------------ OD8 tables

    /// Read the capability mask and the per-feature ranges. Cached; they do not change.
    fn load_ranges(&mut self, gpu: usize) -> Result<()> {
        let adapter = self.adapter(gpu)?;
        let f = need(self.api()?.od8_init_setting_x2_get, "ADL2_Overdrive8_Init_SettingX2_Get")?;
        let mut caps = 0i32;
        let mut count = OD8_FEATURE_COUNT;
        let mut list: *mut c_void = null_mut();
        // SAFETY: out-pointers to locals; `count` arrives pre-set to the table size we expect.
        let rc = unsafe { f(self.context, adapter, &mut caps, &mut count, &mut list) };
        let result = if rc != ADL_OK || list.is_null() {
            Err(GpuError::new(format!("Could not read the Overdrive 8 feature table: {}.", describe(rc))))
        } else {
            let n = (if count <= 0 { OD8_FEATURE_COUNT } else { count }).min(OD8_COUNT as i32) as usize;
            let mut ranges = [Od8Range::default(); OD8_COUNT];
            for (i, range) in ranges.iter_mut().enumerate().take(n) {
                // { featureID, min, max, default } per entry.
                // SAFETY: the driver allocated `count` 16-byte entries; n never exceeds that (or the
                // 75-entry table it was asked for when it reports no count).
                unsafe {
                    let e = (list as *const u8).add(i * 16);
                    *range = Od8Range {
                        min: std::ptr::read_unaligned(e.add(4).cast::<i32>()),
                        max: std::ptr::read_unaligned(e.add(8).cast::<i32>()),
                        default: std::ptr::read_unaligned(e.add(12).cast::<i32>()),
                    };
                }
            }
            self.caps.insert(gpu, caps);
            self.ranges_by_gpu.insert(gpu, ranges);
            Ok(())
        };
        // SAFETY: the driver allocated it through our allocator; freed once.
        unsafe { adl::co_free(list) };
        result
    }

    fn range(&mut self, gpu: usize, id: Od8Id) -> Result<Od8Range> {
        if !self.ranges_by_gpu.contains_key(&gpu) {
            self.load_ranges(gpu)?;
        }
        Ok(self.ranges_by_gpu.get(&gpu).and_then(|r| r.get(id.index())).copied().unwrap_or_default())
    }

    /// The driver's range for each of the five fan-curve points: temperatures, then speeds.
    fn fan_ranges(&mut self, gpu: usize) -> Result<([Od8Range; 5], [Od8Range; 5])> {
        let mut t = [Od8Range::default(); 5];
        let mut s = [Od8Range::default(); 5];
        for (r, id) in t.iter_mut().zip(FAN_CURVE_TEMP_IDS).chain(s.iter_mut().zip(FAN_CURVE_SPEED_IDS)) {
            *r = self.range(gpu, id)?;
        }
        Ok((t, s))
    }

    fn has(&mut self, gpu: usize, feature: i32) -> Result<bool> {
        self.range(gpu, Od8Id::GfxClkFMax)?; // ensures caps is populated
        Ok(self.caps.get(&gpu).is_some_and(|&c| c & feature == feature))
    }

    /// Current values for every OD8 feature, indexed by [`Od8Id`]. Zeros when the read fails.
    fn read_current(&self, gpu: usize) -> Result<[i32; OD8_COUNT]> {
        let adapter = self.adapter(gpu)?;
        let f = need(self.api()?.od8_current_setting_x2_get, "ADL2_Overdrive8_Current_SettingX2_Get")?;
        let mut count = OD8_FEATURE_COUNT;
        let mut list: *mut c_void = null_mut();
        // SAFETY: out-pointers to locals; `count` pre-set to the table size we expect.
        let rc = unsafe { f(self.context, adapter, &mut count, &mut list) };
        let mut values = [0i32; OD8_COUNT];
        if rc != ADL_OK || list.is_null() {
            return Ok(values);
        }
        let n = count.clamp(0, OD8_COUNT as i32) as usize;
        for (i, v) in values.iter_mut().enumerate().take(n) {
            // SAFETY: the driver allocated `count` ints.
            *v = unsafe { std::ptr::read_unaligned((list as *const i32).add(i)) };
        }
        // SAFETY: as above.
        unsafe { adl::co_free(list) };
        Ok(values)
    }

    /// Write one or more OD8 settings. Every slot is sent every time — the driver takes the whole
    /// table — but only the ones named here are flagged "requested", so the rest keep their values.
    fn write(&mut self, gpu: usize, settings: &[(Od8Id, i32)]) -> Result<()> {
        if settings.is_empty() {
            return Ok(());
        }
        // ADLOD8SetSetting: int count; { int value; int requested; int reset; } table[OD8_COUNT]
        let mut set = vec![0i32; 1 + OD8_COUNT * 3];
        // ADLOD8CurrentSetting: int count; int value[OD8_COUNT]
        let mut cur = vec![0i32; 1 + OD8_COUNT];
        set[0] = OD8_FEATURE_COUNT;
        cur[0] = OD8_FEATURE_COUNT;
        for &(id, value) in settings {
            let o = 1 + id.index() * 3;
            set[o] = value; // value
            set[o + 1] = 1; // requested
            set[o + 2] = 0; // reset — leaving this 0 is deliberate: some cards refuse the write
            // when both reset and requested are set.
        }
        let adapter = self.adapter(gpu)?;
        let f = need(self.api()?.od8_setting_set, "ADL2_Overdrive8_Setting_Set")?;
        // SAFETY: both buffers are the full 4 + 77*12 / 4 + 77*4 byte tables the driver expects.
        let rc = unsafe { f(self.context, adapter, set.as_mut_ptr().cast(), cur.as_mut_ptr().cast()) };
        if rc != ADL_OK {
            bail!(
                "The driver rejected the tuning write: {}. AMD Software must be set to a Default or Custom tuning preset for Overdrive writes to apply.",
                describe(rc)
            );
        }
        Ok(())
    }

    /// Pull the live fan table and its defaults, skipping points this card doesn't expose.
    fn read_fan_mode(&mut self, gpu: usize, cur: &[i32; OD8_COUNT]) -> Result<(FanMode, i32)> {
        let (mut temps, mut temp_defaults, mut speeds, mut speed_defaults) = (vec![], vec![], vec![], vec![]);
        for i in 0..FAN_CURVE_SPEED_IDS.len() {
            let sr = self.range(gpu, FAN_CURVE_SPEED_IDS[i])?;
            if !sr.supported() {
                continue;
            }
            speeds.push(cur[FAN_CURVE_SPEED_IDS[i].index()]);
            speed_defaults.push(sr.default);

            let tr = self.range(gpu, FAN_CURVE_TEMP_IDS[i])?;
            if !tr.supported() {
                continue;
            }
            temps.push(cur[FAN_CURVE_TEMP_IDS[i].index()]);
            temp_defaults.push(tr.default);
        }
        Ok(infer_fan_mode(&temps, &speeds, &temp_defaults, &speed_defaults))
    }

    fn diagnostics_text(&mut self, gpu: usize) -> Result<String> {
        let d = self.devices.get(gpu).cloned().ok_or_else(|| missing_adapter(gpu))?;
        let mut sb = String::new();
        let adapter = self.adapter(gpu)?;
        let _ = writeln!(sb, "GPU  : {}   ({})", d.name, d.bus_id);
        let _ = writeln!(sb, "ADL adapter index: {adapter}");
        sb.push('\n');

        let (mut sup, mut en, mut ver) = (0i32, 0i32, 0i32);
        let caps_fn = need(self.api()?.overdrive_caps, "ADL2_Overdrive_Caps")?;
        // SAFETY: out-pointers to locals.
        let rc = unsafe { caps_fn(self.context, adapter, &mut sup, &mut en, &mut ver) };
        sb.push_str("--- Overdrive ---\n");
        let _ = writeln!(sb, "  Overdrive_Caps -> {}  supported={sup} enabled={en} version={ver}", describe(rc));
        let _ = writeln!(sb, "  capability mask = 0x{:08X}", self.caps.get(&gpu).copied().unwrap_or(0));
        for (flag, name) in od8_feature::ALL {
            if self.has(gpu, flag)? {
                let _ = writeln!(sb, "    {name}");
            }
        }
        sb.push('\n');

        let cur = self.read_current(gpu)?;
        sb.push_str("--- OD8 features (supported only) ---\n");
        for id in Od8Id::ALL {
            let r = self.range(gpu, id)?;
            let mark = if r.supported() { "" } else { "   (locked)" };
            let _ = writeln!(
                sb,
                "  {:<24} [{:>7} .. {:>7}]  default={:<7} current={}{mark}",
                id.name(),
                r.min,
                r.max,
                r.default,
                cur[id.index()]
            );
        }
        sb.push('\n');

        sb.push_str("--- Native AMD telemetry (PMLog + ADLX; no external monitor) ---\n");
        let t = self.read_telemetry(gpu)?;
        let _ = writeln!(
            sb,
            "  core {} MHz · mem {} MHz · {} mV",
            f0(t.core_clock_mhz),
            f0(t.memory_clock_mhz),
            f0(t.voltage_mv)
        );
        let not_reported = |v: f64| if v.is_nan() { "not reported".to_string() } else { format!("{} MHz", f0(v)) };
        let _ = writeln!(sb, "  FCLK: {} · SoC: {}", not_reported(t.fabric_clock_mhz), not_reported(t.soc_clock_mhz));
        sb.push_str("  FCLK adjustment: unavailable through this ADL backend (sensor is read-only).\n");
        let core_min = if self.capabilities(gpu)?.can_set_core_min_clock {
            "writable (Min Core Clock)"
        } else if self.range(gpu, Od8Id::GfxClkFMin)?.supported() {
            "driver range available, but the maximum is an offset on this card, so no absolute pair"
        } else {
            "locked by driver"
        };
        let _ = writeln!(sb, "  Core minimum: {core_min}.");
        let _ = writeln!(
            sb,
            "  edge {}°C · hotspot {}°C · memory {}°C",
            f0(t.temperature_c),
            f0(t.hot_spot_c),
            f0(t.memory_temperature_c)
        );
        let _ = writeln!(
            sb,
            "  fan {}% / {} rpm · board {} W · load {}%",
            f0(t.fan_percent),
            f0(t.fan_rpm),
            f0(t.power_watts),
            f0(t.gpu_load_percent)
        );
        let _ = writeln!(sb, "  limit reason: {}", t.limit_reason);
        let _ = writeln!(sb, "  dedicated VRAM used: {} MB", f0(t.memory_used_mb));
        for s in &t.supplemental_sensors {
            let _ = writeln!(sb, "  [{}] {}: {} {}", s.key, s.name, trim_decimals(s.value, 3), s.unit);
        }
        let _ = writeln!(sb, "{}", self.adlx.status);
        Ok(sb)
    }
}

/// `AdapterInfo.strDriverPath` is an NT-style path to the adapter's driver key, e.g.
/// `\Registry\Machine\System\CurrentControlSet\Control\Class\{4d36e968-...}\0000`. AMD writes
/// RadeonSoftwareVersion ("25.3.1") and DriverVersion ("32.0.23033.1002") there.
#[cfg(windows)]
fn read_driver_version_from_registry(driver_path: &str) -> String {
    use winreg::RegKey;
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let read = |sub_key: &str| -> Option<String> {
        let key = hklm.open_subkey_with_flags(sub_key, KEY_READ).ok()?;
        let radeon: Option<String> = key.get_value("RadeonSoftwareVersion").ok(); // the friendly one
        if let Some(r) = radeon.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            return Some(r.to_string());
        }
        let driver: Option<String> = key.get_value("DriverVersion").ok();
        driver.as_deref().map(str::trim).filter(|d| !d.is_empty()).map(str::to_string)
    };

    if !driver_path.trim().is_empty() {
        let mut p = driver_path.replace('/', "\\");
        p = p.trim_start_matches('\\').to_string();
        const PREFIX: &str = "Registry\\Machine\\";
        if p.get(..PREFIX.len()).is_some_and(|s| s.eq_ignore_ascii_case(PREFIX)) {
            p = p[PREFIX.len()..].to_string();
        }
        if let Some(hit) = read(&p) {
            return hit;
        }
    }

    // Fallback: walk the display class and take the first AMD adapter that names itself.
    const DISPLAY_CLASS: &str = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    if let Ok(cls) = hklm.open_subkey_with_flags(DISPLAY_CLASS, KEY_READ) {
        for name in cls.enum_keys().filter_map(|k| k.ok()) {
            if name.chars().count() != 4 {
                continue; // 0000, 0001, ...
            }
            let provider: String = cls
                .open_subkey_with_flags(&name, KEY_READ)
                .and_then(|s| s.get_value("ProviderName"))
                .unwrap_or_default();
            let lower = provider.to_lowercase();
            if !lower.contains("advanced micro devices") && !lower.contains("amd") {
                continue;
            }
            if let Some(hit) = read(&format!("{DISPLAY_CLASS}\\{name}")) {
                return hit;
            }
        }
    }
    String::new()
}

#[cfg(not(windows))]
fn read_driver_version_from_registry(_driver_path: &str) -> String {
    String::new()
}

impl GpuBackend for AmdBackend {
    fn backend_name(&self) -> String {
        "AMD (ADL Overdrive 8)".into()
    }

    fn initialize(&mut self) -> Result<()> {
        // Idempotent: the factory initialises while probing, then the tuning service initialises
        // again. A second ADL2_Main_Control_Create would leak a context.
        if !self.context.is_null() {
            return Ok(());
        }
        if self.api.is_none() {
            self.api = Some(AdlApi::load()?);
        }
        let create = self.api()?.main_control_create;
        let mut ctx: *mut c_void = null_mut();
        // SAFETY: ALLOCATOR is a plain fn that lives for the whole process; out-pointer to a local.
        let rc = unsafe { create(ALLOCATOR, 1, &mut ctx) };
        if rc != ADL_OK || ctx.is_null() {
            // A context from a failed Create is not adopted (the original kept it, which made the
            // next initialize() a silent no-op on a context the driver said was bad).
            bail!("ADL2_Main_Control_Create failed: {}.", describe(rc));
        }
        self.context = ctx;

        let result = self.enumerate_adapters().and_then(|()| {
            if self.devices.is_empty() {
                bail!("ADL found no adapter with usable Overdrive 8 controls. {}", self.adapter_failures.join("; "));
            }
            Ok(())
        });
        if result.is_err() {
            self.dispose();
        }
        result
    }

    fn devices(&self) -> &[GpuDevice] {
        &self.devices
    }

    fn read_graphics_info(&mut self, gpu: usize) -> GpuGraphicsInfo {
        let Some(device) = self.devices.get(gpu).cloned() else { return GpuGraphicsInfo::default() };
        let identity = self.pnp_ids.get(gpu).and_then(|p| parse_pnp_identity(p));
        let device_id = identity.map_or(0, |i| i.0);
        let board_vendor = identity.map_or(0, |i| i.1);
        let revision = identity.map_or(-1, |i| i.2);
        let (memory_type, memory_vendor) = match self.adapter(gpu) {
            Ok(a) => self.read_memory_identity(a),
            Err(_) => (String::new(), String::new()),
        };
        let telemetry = self.read_telemetry(gpu).unwrap_or_default();
        let pcie = |key: &str| {
            telemetry
                .supplemental_sensors
                .iter()
                .find(|s| s.key == key)
                .filter(|s| s.value.is_finite())
                .map_or(0, |s| round_even_i32(s.value))
        };
        let current_generation = pcie("adl:40");
        let current_width = pcie("adl:41");
        let maximum_generation = pcie("adl:58");

        // Device plus revision identifies the exact silicon cut. Unknown cards keep those fields
        // empty rather than borrowing a neighbouring model's unit counts.
        let navi48 = device_id == 0x7550 && revision == 0xC0;
        let pick = |s: &str| if navi48 { s.to_string() } else { String::new() };
        GpuGraphicsInfo {
            board_manufacturer: GpuIdentity::board_vendor(board_vendor),
            code_name: pick("Navi 48"),
            revision: if revision >= 0 { format!("{revision:02X}") } else { String::new() },
            cores: pick("4096"),
            rops_tmus: pick("128 / 256"),
            technology: pick("4 nm"),
            memory_type: if !memory_type.is_empty() { memory_type } else { pick("GDDR6") },
            memory_vendor,
            bus_width: pick("256 bits"),
            bus_interface: GpuIdentity::pcie_bus_interface(maximum_generation, 0, current_generation, current_width),
            ..GpuGraphicsInfo::from_device(&device)
        }
    }

    fn capabilities(&mut self, gpu: usize) -> Result<GpuCapabilities> {
        let core = self.range(gpu, Od8Id::GfxClkFMax)?;
        let core_min = self.range(gpu, Od8Id::GfxClkFMin)?;
        let mem = self.range(gpu, Od8Id::UClkFMax)?;
        let power = self.range(gpu, Od8Id::PowerPercentage)?;
        let volt = self.range(gpu, Od8Id::OdVoltage)?;
        let (fan_t, fan_s) = self.fan_ranges(gpu)?;
        let fan_ok = fan_t[0].supported() && fan_s[0].supported();
        let (fan_lo, fan_hi) = flat_duty_range(&fan_s);
        let timing = self.range(gpu, Od8Id::AcTiming)?;
        let temp = self.range(gpu, Od8Id::OperatingTempMax)?;
        let zero_rpm = self.range(gpu, Od8Id::FanZeroRpmControl)?;
        let timing_options = if self.has(gpu, od8_feature::MEMORY_TIMING_TUNE)? {
            build_timing_options(timing.min, timing.max)
        } else {
            Vec::new()
        };
        let n_options = timing_options.len() as i32;
        // The same OD8 ids mean different things per generation. RDNA 4 reports GfxClkFMax as an
        // offset (-500..1000, default 0) and OdVoltage as one too; RDNA 3 reports the absolute
        // maximum clock (500..5000, default 2835 on a 7900 XT) and maximum voltage (700..1100,
        // default 1100). An offset window always spans 0, so a minimum above it means absolute.
        let core_absolute = core.supported() && core.min > 0;
        let volt_absolute = volt.supported() && volt.min > 0;
        // A minimum only pairs with an absolute maximum; RDNA 4 locks GfxClkFMin at 0..0 anyway.
        let min_clock = core_absolute && core_min.supported() && self.has(gpu, od8_feature::GFX_CLK_LIMITS)?;

        Ok(GpuCapabilities {
            can_set_core_offset: self.has(gpu, od8_feature::GFX_CLK_LIMITS)? && core.supported(),
            core_offset_min_mhz: if core.supported() { core.min } else { 0 },
            core_offset_max_mhz: if core.supported() { core.max } else { 0 },
            core_clock_is_absolute: core_absolute,
            core_clock_default_mhz: if core_absolute { core.default } else { 0 },
            can_set_core_min_clock: min_clock,
            core_min_clock_min_mhz: if min_clock { core_min.min } else { 0 },
            core_min_clock_max_mhz: if min_clock { core_min.max } else { 0 },
            core_min_clock_default_mhz: if min_clock { core_min.default } else { 0 },

            // RDNA reports an absolute memory clock, not an offset — the UI reads
            // memory_clock_is_absolute.
            can_set_memory_offset: self.has(gpu, od8_feature::UCLK_MAX)? && mem.supported(),
            memory_offset_min_mhz: if mem.supported() { mem.min } else { 0 },
            memory_offset_max_mhz: if mem.supported() { mem.max } else { 0 },
            memory_clock_is_absolute: true,
            memory_clock_default_mhz: mem.default,

            can_set_power_limit: self.has(gpu, od8_feature::POWER_LIMIT)? && power.supported(),
            power_limit_min_percent: if power.supported() { power.min } else { 0 },
            power_limit_max_percent: if power.supported() { power.max } else { 0 },
            power_limit_default_percent: power.default,
            power_limit_is_offset: true,

            // Locked on RDNA 4: the driver owns the thermal limit.
            can_set_temp_limit: self.has(gpu, od8_feature::TEMPERATURE_SYSTEM)? && temp.supported(),
            temp_limit_min_c: temp.min,
            temp_limit_max_c: temp.max,
            temp_limit_default_c: temp.default,

            // Voltage is a negative offset here, not a point on an editable curve.
            voltage_style: if self.has(gpu, od8_feature::OD_VOLTAGE_LIMIT)? && volt.supported() {
                VoltageControlStyle::Offset
            } else {
                VoltageControlStyle::None
            },
            voltage_offset_min_mv: if volt.supported() { volt.min } else { 0 },
            voltage_offset_max_mv: if volt.supported() { volt.max } else { 0 },
            voltage_offset_is_absolute: volt_absolute,
            voltage_offset_default_mv: if volt_absolute { volt.default } else { 0 },
            can_set_voltage_curve: false,
            curve_unavailable_reason: if self.has(gpu, od8_feature::GFX_CLK_CURVE)? {
                String::new()
            } else {
                "this architecture exposes a voltage offset instead of an editable V/F curve".into()
            },
            can_read_voltage: true,

            can_set_zero_rpm: self.has(gpu, od8_feature::FAN_ZERO_RPM_CONTROL)? && zero_rpm.supported(),
            zero_rpm_default: zero_rpm.default != 0,

            can_set_memory_timing: n_options > 1,
            memory_timing_default_level: if n_options > 0 {
                clamp_i32(timing.default.wrapping_sub(timing.min), 0, n_options - 1)
            } else {
                0
            },
            memory_timing_options: timing_options,

            // A hardware curve: exactly five points, and the driver runs it — no polling loop.
            // Every window below is the driver's, per point; the fallbacks only fill a card that
            // offers no fan control, where nothing reads them.
            can_set_fan_speed: self.has(gpu, od8_feature::FAN_CURVE)? && fan_ok,
            fan_curve_is_hardware: true,
            fan_curve_points: 5,
            fan_curve_min_temp_c: if fan_ok { fan_t[0].min } else { 25 },
            fan_curve_max_temp_c: if fan_ok { fan_t[4].max } else { 100 },
            fan_min_percent: if fan_ok { fan_lo } else { 0 },
            fan_max_percent: if fan_ok { fan_hi } else { 100 },
            fan_count: 1,
            ..GpuCapabilities::default()
        })
    }

    fn read_telemetry(&mut self, gpu: usize) -> Result<GpuTelemetry> {
        // The caller-owned ADL context stays alive until ADLX has finished using it (&mut self).
        let context = self.live()?;
        let adapter = self.adapter(gpu)?;
        let query = need(self.api()?.new_query_pm_log_data_get, "ADL2_New_QueryPMLogData_Get")?;
        let mut buf = vec![0i32; PM_LOG_BYTES / 4];
        let mut pm = HashMap::new();
        // SAFETY: `buf` is the full zeroed ADLPMLogDataOutput.
        if unsafe { query(context, adapter, buf.as_mut_ptr().cast()) } == ADL_OK {
            for id in 1..256 {
                let value = pm_value(&buf, id);
                if value.is_finite() {
                    pm.insert(id, value);
                }
            }
        }
        // A failed PMLog query must not prevent the independent ADLX fallback.
        let adlx = self.adlx.read(context, adapter);
        let mut t = AmdTelemetryMapper::map(&pm, &adlx);
        t.limit_reason = match pm.get(&pm_log::THROTTLER_STATUS) {
            Some(&flags) => throttle_text(flags),
            None => "Unavailable".into(),
        };
        Ok(t)
    }

    /// Temperature-only sample for background ticks that exist purely to step a fan curve.
    ///
    /// Worth being straight about what this saves on AMD: the driver returns every sensor from a
    /// single QueryPMLogData call, so unlike NVIDIA the call itself is the cost here and a running
    /// curve still needs it. What it does avoid is building a whole sample for one number. Never
    /// fails: any problem yields an empty sample, as in the original.
    fn read_temperature_only(&mut self, gpu: usize) -> Result<GpuTelemetry> {
        let sample = (|| {
            let context = self.live().ok()?;
            let adapter = self.adapter(gpu).ok()?;
            let query = self.api.as_ref()?.new_query_pm_log_data_get?;
            let mut buf = vec![0i32; PM_LOG_BYTES / 4];
            // SAFETY: `buf` is the full zeroed ADLPMLogDataOutput.
            if unsafe { query(context, adapter, buf.as_mut_ptr().cast()) } != ADL_OK {
                return None;
            }
            let edge = pm_value(&buf, pm_log::TEMPERATURE_EDGE);
            Some(GpuTelemetry { temperature_c: if edge.is_nan() { 0.0 } else { edge }, ..GpuTelemetry::default() })
        })();
        Ok(sample.unwrap_or_default())
    }

    fn read_tuning_state(&mut self, gpu: usize) -> Result<GpuTuningState> {
        let v = self.read_current(gpu)?;
        let (mode, percent) = self.read_fan_mode(gpu, &v)?;
        let timing_min = self.range(gpu, Od8Id::AcTiming)?.min;
        Ok(GpuTuningState {
            core_offset_mhz: v[Od8Id::GfxClkFMax.index()],
            core_min_clock_mhz: v[Od8Id::GfxClkFMin.index()],
            memory_offset_mhz: v[Od8Id::UClkFMax.index()],
            power_limit_percent: v[Od8Id::PowerPercentage.index()],
            temp_limit_c: v[Od8Id::OperatingTempMax.index()],
            voltage_offset_mv: v[Od8Id::OdVoltage.index()],
            zero_rpm: v[Od8Id::FanZeroRpmControl.index()] != 0,
            memory_timing_level: v[Od8Id::AcTiming.index()].wrapping_sub(timing_min),
            detected_fan_mode: Some(mode),
            // Reported as manual only for a fixed duty. A hardware curve is the driver's to run,
            // and callers that only see this flag treat manual as "a single percentage is in force".
            fan_manual: mode == FanMode::Fixed,
            fan_percent: percent,
            ..GpuTuningState::default()
        })
    }

    fn set_core_offset(&mut self, gpu: usize, offset_mhz: i32) -> Result<()> {
        let r = self.range(gpu, Od8Id::GfxClkFMax)?;
        if !r.supported() {
            bail!("This card does not expose a core clock offset.");
        }
        self.write(gpu, &[(Od8Id::GfxClkFMax, clamp_i32(offset_mhz, r.min, r.max))])
    }

    fn set_core_min_clock(&mut self, gpu: usize, mhz: i32) -> Result<()> {
        if !self.capabilities(gpu)?.can_set_core_min_clock {
            bail!("This card does not expose a minimum core clock.");
        }
        let r = self.range(gpu, Od8Id::GfxClkFMin)?;
        self.write(gpu, &[(Od8Id::GfxClkFMin, clamp_i32(mhz, r.min, r.max))])
    }

    fn set_memory_offset(&mut self, gpu: usize, mhz: i32) -> Result<()> {
        let r = self.range(gpu, Od8Id::UClkFMax)?;
        if !r.supported() {
            bail!("This card does not expose a memory clock control.");
        }
        self.write(gpu, &[(Od8Id::UClkFMax, clamp_i32(mhz, r.min, r.max))])
    }

    fn set_power_limit(&mut self, gpu: usize, percent: i32) -> Result<()> {
        let r = self.range(gpu, Od8Id::PowerPercentage)?;
        if !r.supported() {
            bail!("This card does not expose a power limit.");
        }
        self.write(gpu, &[(Od8Id::PowerPercentage, clamp_i32(percent, r.min, r.max))])
    }

    fn set_temp_limit(&mut self, gpu: usize, celsius: i32) -> Result<()> {
        let r = self.range(gpu, Od8Id::OperatingTempMax)?;
        if !r.supported() {
            bail!("This driver keeps the temperature limit to itself.");
        }
        self.write(gpu, &[(Od8Id::OperatingTempMax, clamp_i32(celsius, r.min, r.max))])
    }

    fn set_voltage_boost(&mut self, _gpu: usize, percent: i32) -> Result<()> {
        if percent != 0 {
            bail!("AMD cards have no voltage boost — use the voltage offset.");
        }
        Ok(())
    }

    /// Negative mV offset applied to the whole curve. 0 restores stock.
    fn set_voltage_curve_offset(&mut self, gpu: usize, offset_mv: i32, _extra_clock_mhz: i32) -> Result<()> {
        let r = self.range(gpu, Od8Id::OdVoltage)?;
        if !r.supported() {
            if offset_mv == 0 {
                return Ok(());
            }
            bail!("This card does not expose a voltage offset.");
        }
        self.write(gpu, &[(Od8Id::OdVoltage, clamp_i32(offset_mv, r.min, r.max))])
    }

    fn set_zero_rpm(&mut self, gpu: usize, enabled: bool) -> Result<()> {
        if !self.range(gpu, Od8Id::FanZeroRpmControl)?.supported() {
            bail!("This card does not expose zero RPM control.");
        }
        self.write(gpu, &[(Od8Id::FanZeroRpmControl, i32::from(enabled))])
    }

    fn set_memory_timing(&mut self, gpu: usize, level: i32) -> Result<()> {
        let r = self.range(gpu, Od8Id::AcTiming)?;
        let options = self.capabilities(gpu)?.memory_timing_options;
        if options.is_empty() {
            bail!("This card does not expose memory timing tuning.");
        }
        if level < 0 || level >= options.len() as i32 {
            bail!("Memory timing preset is outside the driver-supported range.");
        }
        self.write(gpu, &[(Od8Id::AcTiming, r.min.wrapping_add(level))])
    }

    /// Fixed speed is expressed as a flat hardware curve: five points at the same duty. The driver
    /// still owns fan control, so it keeps working when this app is closed.
    fn set_fan_speed(&mut self, gpu: usize, _fan_index: i32, percent: i32) -> Result<()> {
        let (t, s) = self.fan_ranges(gpu)?;
        if !s[0].supported() {
            bail!("This card does not expose fan control.");
        }
        let (lo, hi) = flat_duty_range(&s);
        let duty = clamp_i32(percent, lo, hi);
        let writes = fan_curve_writes(|_| f64::from(duty), &t, &s);
        self.write(gpu, &writes)
    }

    /// Push a fan curve into the hardware, sampled down to the five points the driver takes.
    fn set_fan_curve(&mut self, gpu: usize, curve: &FanCurve) -> Result<()> {
        let (t, s) = self.fan_ranges(gpu)?;
        if !s[0].supported() {
            bail!("This card does not expose fan control.");
        }
        let writes = fan_curve_writes(|c| curve.evaluate(c), &t, &s);
        self.write(gpu, &writes)
    }

    fn set_fan_auto(&mut self, gpu: usize) -> Result<()> {
        let ids = [
            Od8Id::FanCurveTemperature1,
            Od8Id::FanCurveSpeed1,
            Od8Id::FanCurveTemperature2,
            Od8Id::FanCurveSpeed2,
            Od8Id::FanCurveTemperature3,
            Od8Id::FanCurveSpeed3,
            Od8Id::FanCurveTemperature4,
            Od8Id::FanCurveSpeed4,
            Od8Id::FanCurveTemperature5,
            Od8Id::FanCurveSpeed5,
        ];
        let mut writes = Vec::new();
        for id in ids {
            let r = self.range(gpu, id)?;
            if r.supported() {
                writes.push((id, r.default));
            }
        }
        if !writes.is_empty() {
            self.write(gpu, &writes)?;
        }
        Ok(())
    }

    fn reset_to_defaults(&mut self, gpu: usize) -> Result<()> {
        let mut errors = Vec::new();
        let mut writes = Vec::new();
        for id in Od8Id::ALL {
            let r = self.range(gpu, id)?;
            if r.supported() {
                writes.push((id, r.default));
            }
        }
        if !writes.is_empty()
            && let Err(e) = self.write(gpu, &writes)
        {
            errors.push(e.message);
        }
        if let Err(e) = self.set_fan_auto(gpu) {
            errors.push(e.message);
        }
        if !errors.is_empty() {
            bail!("Reset partially failed: {}", errors.join(" | "));
        }
        Ok(())
    }

    fn diagnostics(&mut self, gpu: usize) -> String {
        self.diagnostics_text(gpu).unwrap_or_else(|e| e.message)
    }
}

/// Five evenly spaced temperatures across the allowed window, each taking its speed from the
/// user's curve. Temperatures must stay strictly increasing or the driver rejects the table, and
/// the speeds must be monotonic.
/// The duty a flat curve may hold: inside every point's own speed range.
fn flat_duty_range(s: &[Od8Range; 5]) -> (i32, i32) {
    let lo = s.iter().map(|r| r.min).max().unwrap_or(0);
    let hi = s.iter().map(|r| r.max).min().unwrap_or(100);
    (lo, hi.max(lo))
}

/// The five hardware points: temperatures spread from the first point's minimum to the last
/// point's maximum, speeds from `speed_at`. The driver reports a range per point, so each is
/// clamped to its own rather than to the first point's.
fn fan_curve_writes(speed_at: impl Fn(f64) -> f64, t: &[Od8Range; 5], s: &[Od8Range; 5]) -> Vec<(Od8Id, i32)> {
    let mut writes = Vec::with_capacity(10);
    let (lo, hi) = (t[0].min, t[4].max);
    let (mut last_temp, mut last_speed) = (i32::MIN, s[0].min);
    let n = FAN_CURVE_TEMP_IDS.len() as i32;
    for i in 0..FAN_CURVE_TEMP_IDS.len() {
        let spread = lo.wrapping_add(hi.wrapping_sub(lo).wrapping_mul(i as i32) / (n - 1));
        let mut temp = clamp_i32(spread, t[i].min, t[i].max);
        if temp <= last_temp {
            temp = last_temp.wrapping_add(1);
        }
        let mut speed = round_even_i32(speed_at(temp as f64));
        speed = clamp_i32(speed, s[i].min, s[i].max);
        if speed < last_speed {
            speed = last_speed; // the hardware curve must be monotonic
        }
        writes.push((FAN_CURVE_TEMP_IDS[i], temp));
        writes.push((FAN_CURVE_SPEED_IDS[i], speed));
        last_temp = temp;
        last_speed = speed;
    }
    writes
}

#[cfg(test)]
mod tests {
    use super::*;
    use roch_core::FanPoint;

    #[test]
    fn timing_options() {
        assert_eq!(build_timing_options(0, 1), vec!["Default", "Fast timing"], "RDNA4 exposes only driver choices");
        assert_eq!(build_timing_options(0, 2)[2], "Driver preset 2", "extra IDs are not invented fast presets");
        assert_eq!(build_timing_options(2, 3)[0], "Driver preset 2", "nonzero range retains raw ID labels");
        assert!(build_timing_options(0, 0).is_empty(), "locked range hidden");
        assert!(build_timing_options(2, 1).is_empty(), "reversed range rejected");
        assert!(build_timing_options(-1, 1).is_empty(), "negative range rejected");
        assert!(build_timing_options(0, i32::MAX).is_empty(), "oversized range rejected");
        assert_eq!(AdlBackend::build_timing_options(0, 255).len(), 256);
    }

    #[test]
    fn fan_mode_inference() {
        let (def_t, def_s) = ([30, 50, 64, 79, 88], [30, 42, 54, 66, 90]);

        assert_eq!(infer_fan_mode(&def_t, &def_s, &def_t, &def_s), (FanMode::Auto, 0), "untouched table is auto");

        // What set_fan_speed writes: five equal duties spread across the temperature window.
        let fixed = AmdBackend::infer_fan_mode(&[25, 43, 62, 81, 100], &[50; 5], &def_t, &def_s);
        assert_eq!(fixed, (FanMode::Fixed, 50), "flat table reads as fixed 50%");

        let rising = infer_fan_mode(&[25, 43, 62, 81, 100], &[30, 40, 55, 70, 100], &def_t, &def_s);
        assert_eq!(rising.0, FanMode::Curve, "rising table reads as a curve");

        // Default duties at non-default temperatures is still a user table, not auto.
        let moved = infer_fan_mode(&[25, 45, 60, 75, 95], &def_s, &def_t, &def_s);
        assert_eq!(moved.0, FanMode::Curve, "shifted temperatures are not auto");

        // A card exposing no fan points at all must not be reported as manual.
        assert_eq!(infer_fan_mode(&[], &[], &def_t, &def_s).0, FanMode::Auto, "no fan points reads as auto");

        // Regression: a fixed duty used to read back as auto.
        assert_ne!(fixed.0, FanMode::Auto);
    }

    #[test]
    fn pnp_identity() {
        let pnp = r"PCI\VEN_1002&DEV_7550&SUBSYS_24221DA2&REV_C0\6&1A2B3C4D&0&00000019";
        assert_eq!(parse_pnp_identity(pnp), Some((0x7550, 0x1DA2, 0xC0)));
        assert_eq!(parse_pnp_identity(&pnp.to_lowercase()), Some((0x7550, 0x1DA2, 0xC0)), "case-insensitive");
        assert_eq!(parse_pnp_identity("DEV_75&DEV_7480&SUBSYS_00000000&REV_C7"), Some((0x7480, 0, 0xC7)));
        assert_eq!(parse_pnp_identity("PCI\\VEN_1002&DEV_7550&REV_C0"), None);
        assert_eq!(parse_pnp_identity(""), None);
        assert_eq!(parse_pnp_identity("DEV_"), None);
    }

    #[test]
    fn throttle_text_bits() {
        assert_eq!(throttle_text(0.0), "None");
        assert_eq!(throttle_text(f64::NAN), "None");
        assert_eq!(throttle_text(1.0), "Power");
        assert_eq!(throttle_text(7.0), "Power + Thermal + Current");
        assert_eq!(throttle_text(8.0), "Other");
    }

    #[test]
    fn pm_value_reads_supported_pairs() {
        let mut buf = vec![0i32; PM_LOG_BYTES / 4];
        assert_eq!(buf.len(), 513);
        buf[1 + 8 * 2] = 1;
        buf[1 + 8 * 2 + 1] = 54;
        assert_eq!(pm_value(&buf, 8), 54.0);
        assert!(pm_value(&buf, 9).is_nan());
        assert!(pm_value(&buf, 255).is_nan());
        assert!(pm_value(&buf, 256).is_nan(), "out of range is unsupported, not a panic");
    }

    #[test]
    fn fan_curve_is_strictly_increasing_and_monotonic() {
        let curve = FanCurve::with_points(vec![
            FanPoint::new(30.0, 60.0),
            FanPoint::new(60.0, 40.0),
            FanPoint::new(90.0, 100.0),
        ]);
        let t = [Od8Range { min: 25, max: 100, default: 0 }; 5];
        let s = [Od8Range { min: 0, max: 100, default: 0 }; 5];
        let w = fan_curve_writes(|c| curve.evaluate(c), &t, &s);
        let temps: Vec<i32> = w.iter().step_by(2).map(|x| x.1).collect();
        let speeds: Vec<i32> = w.iter().skip(1).step_by(2).map(|x| x.1).collect();
        assert_eq!(temps, vec![25, 43, 62, 81, 100]);
        assert!(speeds.windows(2).all(|p| p[0] <= p[1]));
        assert_eq!(w[0].0, Od8Id::FanCurveTemperature1);
        assert_eq!(w[9].0, Od8Id::FanCurveSpeed5);

        // A collapsed window still yields strictly increasing temperatures.
        let flat = [Od8Range { min: 50, max: 50, default: 0 }; 5];
        let w = fan_curve_writes(|c| curve.evaluate(c), &flat, &s);
        let temps: Vec<i32> = w.iter().step_by(2).map(|x| x.1).collect();
        assert_eq!(temps, vec![50, 51, 52, 53, 54]);
    }

    #[test]
    fn fan_points_use_their_own_driver_ranges() {
        // A card whose points differ: the curve spans the first minimum to the last maximum, and
        // each point stays inside the window the driver reported for it.
        let r = |min, max| Od8Range { min, max, default: 0 };
        let t = [r(20, 40), r(30, 60), r(40, 80), r(50, 90), r(60, 110)];
        let s = [r(0, 60), r(20, 80), r(30, 100), r(30, 100), r(40, 100)];
        let w = fan_curve_writes(|_| 10.0, &t, &s);
        let temps: Vec<i32> = w.iter().step_by(2).map(|x| x.1).collect();
        let speeds: Vec<i32> = w.iter().skip(1).step_by(2).map(|x| x.1).collect();
        assert_eq!(temps, vec![20, 42, 65, 87, 110], "spread over 20..110, each inside its own range");
        assert_eq!(speeds, vec![10, 20, 30, 30, 40], "each speed raised to its point's own minimum");

        // A fixed duty must fit every point: the highest minimum to the lowest maximum.
        assert_eq!(flat_duty_range(&s), (40, 60));
    }

    #[test]
    fn uninitialised_backend_fails_cleanly() {
        let mut b = AmdBackend::new();
        assert_eq!(b.backend_name(), "AMD (ADL Overdrive 8)");
        assert!(b.devices().is_empty());
        assert!(b.capabilities(0).is_err());
        assert!(b.read_telemetry(0).is_err());
        assert!(b.read_tuning_state(0).is_err());
        assert!(b.set_core_offset(0, 100).is_err());
        assert!(b.set_fan_auto(0).is_err());
        assert!(b.reset_to_defaults(0).is_err());
        assert_eq!(b.read_graphics_info(3), GpuGraphicsInfo::default());
        assert!(b.diagnostics(0).contains("No AMD adapter"));
        // Voltage boost is rejected before any driver access.
        assert!(b.set_voltage_boost(0, 5).is_err());
        assert!(b.set_voltage_boost(0, 0).is_ok());
    }

    /// The trait's default temperature read falls back to a full sample; this backend declares its
    /// own, which never fails (an empty sample instead), while the full read does fail here.
    #[test]
    fn declares_its_own_temperature_only_read() {
        let mut b = AmdBackend::new();
        let t = b.read_temperature_only(0).expect("temperature-only never fails");
        assert_eq!(t.temperature_c, 0.0);
        assert!(b.read_telemetry(0).is_err());
    }
}
