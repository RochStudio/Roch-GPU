//! Raw binding to AMD's Display Library (atiadlxx.dll), Overdrive 8 subset.
//!
//! The OD8 tables are read and written through plain integer buffers rather than declared structs.
//! Every one of them is an int array behind a small header, the sizes are known ([`OD8_COUNT`]),
//! and doing it this way means no struct-packing assumption can silently corrupt a write — the
//! probe that mapped this card used exactly this shape.

use std::ffi::c_void;

use roch_core::native::{self, Library};
use roch_core::{GpuError, Result};

/// The driver library. Loaded from System32 only.
pub const DLL: &str = "atiadlxx.dll";

pub const ADL_OK: i32 = 0;
/// ADL reports the PCI vendor as the *decimal* number 1002 (it is 0x1002 in PCI terms).
pub const AMD_VENDOR_ID: i32 = 1002;

/// ADLOD8SettingId member count. AMD's own tools size their tables at `OD8_COUNT - 2`.
pub const OD8_COUNT: usize = 77;
pub const OD8_FEATURE_COUNT: i32 = OD8_COUNT as i32 - 2;

// ---------------------------------------------------------------- allocator

/// `ADL_MAIN_MALLOC_CALLBACK`: `void* (__stdcall *)(int)`.
pub type MemAlloc = unsafe extern "system" fn(size: i32) -> *mut c_void;

/// The allocator handed to `ADL2_Main_Control_Create`. A plain `fn`, so it outlives the context —
/// the driver keeps calling it long after Create returns. Buffers it hands out are CoTaskMem
/// allocations (as `Marshal.AllocCoTaskMem` in the original) and are released with [`co_free`].
unsafe extern "system" fn adl_alloc(size: i32) -> *mut c_void {
    // A negative size cannot be satisfied; null is what a failed malloc reports.
    if size < 0 {
        return std::ptr::null_mut();
    }
    co_alloc(size as usize)
}

/// The allocator callback as a function pointer.
pub const ALLOCATOR: MemAlloc = adl_alloc;

#[cfg(windows)]
pub fn co_alloc(size: usize) -> *mut c_void {
    // SAFETY: CoTaskMemAlloc has no preconditions; it returns null on failure.
    unsafe { windows_sys::Win32::System::Com::CoTaskMemAlloc(size) }
}

#[cfg(not(windows))]
pub fn co_alloc(_size: usize) -> *mut c_void {
    std::ptr::null_mut()
}

/// Free a buffer the driver allocated through [`ALLOCATOR`]. Null is a no-op.
///
/// # Safety
/// `ptr` must be null or a CoTaskMem allocation not yet freed.
#[cfg(windows)]
pub unsafe fn co_free(ptr: *mut c_void) {
    if !ptr.is_null() {
        // SAFETY: `ptr` came from CoTaskMemAlloc (our allocator) and is freed exactly once.
        unsafe { windows_sys::Win32::System::Com::CoTaskMemFree(ptr) }
    }
}

#[cfg(not(windows))]
pub unsafe fn co_free(_ptr: *mut c_void) {}

// ---------------------------------------------------------------- exports

type MainControlCreate = unsafe extern "C" fn(cb: MemAlloc, enum_connected: i32, context: *mut *mut c_void) -> i32;
type MainControlDestroy = unsafe extern "C" fn(context: *mut c_void) -> i32;
type NumberOfAdaptersGet = unsafe extern "C" fn(context: *mut c_void, num: *mut i32) -> i32;
type AdapterInfoGet = unsafe extern "C" fn(context: *mut c_void, info: *mut c_void, input_size: i32) -> i32;
type OverdriveCaps = unsafe extern "C" fn(
    context: *mut c_void,
    adapter: i32,
    supported: *mut i32,
    enabled: *mut i32,
    version: *mut i32,
) -> i32;
// lpNumberOfFeatures is IN/OUT: it must arrive pre-set to the table size the caller expects.
// Passing 0 makes the driver return ADL_ERR_NULL_POINTER, which reads like "unsupported" but isn't.
type Od8InitSettingX2Get = unsafe extern "C" fn(
    context: *mut c_void,
    adapter: i32,
    caps: *mut i32,
    num_features: *mut i32,
    init_setting_list: *mut *mut c_void,
) -> i32;
type Od8CurrentSettingX2Get = unsafe extern "C" fn(
    context: *mut c_void,
    adapter: i32,
    num_features: *mut i32,
    current_list: *mut *mut c_void,
) -> i32;
type Od8SettingSet = unsafe extern "C" fn(
    context: *mut c_void,
    adapter: i32,
    set_setting: *mut c_void,
    current_setting: *mut c_void,
) -> i32;
type QueryPmLogDataGet = unsafe extern "C" fn(context: *mut c_void, adapter: i32, data_output: *mut c_void) -> i32;
type ContextBuffer = unsafe extern "C" fn(context: *mut c_void, buffer: *mut c_void) -> i32;
type AdapterBuffer = unsafe extern "C" fn(context: *mut c_void, adapter: i32, buffer: *mut c_void) -> i32;

/// The resolved ADL entry points. Only `ADL2_Main_Control_Create` is required up front; every other
/// export is resolved optionally, and a call site that needs a missing one reports it as an error
/// (the original surfaced those as `EntryPointNotFoundException` at call time). The identity exports
/// are genuinely optional — older or trimmed drivers may not export them — and their call sites
/// treat absence as "unknown", not an error.
pub struct AdlApi {
    pub main_control_create: MainControlCreate,
    pub main_control_destroy: Option<MainControlDestroy>,
    pub number_of_adapters_get: Option<NumberOfAdaptersGet>,
    pub adapter_info_get: Option<AdapterInfoGet>,
    pub overdrive_caps: Option<OverdriveCaps>,
    pub od8_init_setting_x2_get: Option<Od8InitSettingX2Get>,
    pub od8_current_setting_x2_get: Option<Od8CurrentSettingX2Get>,
    pub od8_setting_set: Option<Od8SettingSet>,
    pub new_query_pm_log_data_get: Option<QueryPmLogDataGet>,
    pub graphics_versions_x2_get: Option<ContextBuffer>,
    pub graphics_versions_get: Option<ContextBuffer>,
    pub adapter_video_bios_info_get: Option<AdapterBuffer>,
    pub adapter_memory_info2_get: Option<AdapterBuffer>,
    pub adapter_memory_info_get: Option<AdapterBuffer>,
    pub adapter_memory_info_x4_get: Option<AdapterBuffer>,
    /// Keeps every pointer above valid. Declared last so it drops last.
    _lib: Library,
}

impl AdlApi {
    /// Load atiadlxx.dll from System32 and resolve the exports.
    pub fn load() -> Result<AdlApi> {
        let lib = native::load_system32(DLL)
            .map_err(|_| GpuError::new("atiadlxx.dll not found — no AMD driver on this machine."))?;
        // SAFETY (all symbol lookups): the fn-pointer types above mirror the ADL SDK prototypes
        // (adl_sdk.h), every one cdecl; the pointers are kept alive by `_lib`.
        let main_control_create = unsafe { native::symbol::<MainControlCreate>(&lib, "ADL2_Main_Control_Create") }
            .map_err(|_| GpuError::new("This AMD driver is too old for the Overdrive 8 API."))?;
        unsafe {
            Ok(AdlApi {
                main_control_create,
                main_control_destroy: native::optional_symbol(&lib, "ADL2_Main_Control_Destroy"),
                number_of_adapters_get: native::optional_symbol(&lib, "ADL2_Adapter_NumberOfAdapters_Get"),
                adapter_info_get: native::optional_symbol(&lib, "ADL2_Adapter_AdapterInfo_Get"),
                overdrive_caps: native::optional_symbol(&lib, "ADL2_Overdrive_Caps"),
                od8_init_setting_x2_get: native::optional_symbol(&lib, "ADL2_Overdrive8_Init_SettingX2_Get"),
                od8_current_setting_x2_get: native::optional_symbol(&lib, "ADL2_Overdrive8_Current_SettingX2_Get"),
                od8_setting_set: native::optional_symbol(&lib, "ADL2_Overdrive8_Setting_Set"),
                new_query_pm_log_data_get: native::optional_symbol(&lib, "ADL2_New_QueryPMLogData_Get"),
                graphics_versions_x2_get: native::optional_symbol(&lib, "ADL2_Graphics_VersionsX2_Get"),
                graphics_versions_get: native::optional_symbol(&lib, "ADL2_Graphics_Versions_Get"),
                adapter_video_bios_info_get: native::optional_symbol(&lib, "ADL2_Adapter_VideoBiosInfo_Get"),
                adapter_memory_info2_get: native::optional_symbol(&lib, "ADL2_Adapter_MemoryInfo2_Get"),
                adapter_memory_info_get: native::optional_symbol(&lib, "ADL2_Adapter_MemoryInfo_Get"),
                adapter_memory_info_x4_get: native::optional_symbol(&lib, "ADL2_Adapter_MemoryInfoX4_Get"),
                _lib: lib,
            })
        }
    }
}

/// An optional export the call site cannot do without.
pub fn need<F: Copy>(f: Option<F>, name: &str) -> Result<F> {
    f.ok_or_else(|| GpuError::new(format!("Unable to find an entry point named '{name}' in '{DLL}'.")))
}

// ---------------------------------------------------------------- structs

/// `AdapterInfo` from adl_structures.h (sequential, 4-byte aligned, `char[256]` strings).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AdapterInfo {
    pub i_size: i32,
    pub i_adapter_index: i32,
    pub str_udid: [u8; 256],
    pub i_bus_number: i32,
    pub i_device_number: i32,
    pub i_function_number: i32,
    pub i_vendor_id: i32,
    pub str_adapter_name: [u8; 256],
    pub str_display_name: [u8; 256],
    pub i_present: i32,
    pub i_exist: i32,
    pub str_driver_path: [u8; 256],
    pub str_driver_path_ext: [u8; 256],
    pub str_pnp_string: [u8; 256],
    pub i_os_display_index: i32,
}

const _: () = assert!(size_of::<AdapterInfo>() == 1572);

impl AdapterInfo {
    pub const fn zeroed() -> Self {
        AdapterInfo {
            i_size: 0,
            i_adapter_index: 0,
            str_udid: [0; 256],
            i_bus_number: 0,
            i_device_number: 0,
            i_function_number: 0,
            i_vendor_id: 0,
            str_adapter_name: [0; 256],
            str_display_name: [0; 256],
            i_present: 0,
            i_exist: 0,
            str_driver_path: [0; 256],
            str_driver_path_ext: [0; 256],
            str_pnp_string: [0; 256],
            i_os_display_index: 0,
        }
    }
}

/// `ADLMemoryInfoX4` (sequential; the i64 fields force 8-byte alignment, hence 4 bytes of tail
/// padding after the three ints).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryInfoX4 {
    pub i_memory_size: i64,
    pub str_memory_type: [u8; 256],
    pub i_memory_bandwidth: i64,
    pub i_hyper_memory_size: i64,
    pub i_invisible_memory_size: i64,
    pub i_visible_memory_size: i64,
    pub i_vram_vendor_rev_id: i32,
    pub i_memory_bandwidth_x2: i32,
    pub i_memory_type: i32,
}

const _: () = assert!(size_of::<MemoryInfoX4>() == 312);

/// Human text for an ADL return code.
pub fn describe(rc: i32) -> String {
    match rc {
        0 => "OK".into(),
        -1 => "generic error".into(),
        -2 => "ADL not initialised".into(),
        -3 => "invalid parameter".into(),
        -4 => "invalid parameter size".into(),
        -5 => "invalid adapter index".into(),
        -8 => "not supported".into(),
        -9 => "null pointer".into(),
        -10 => "adapter disabled".into(),
        _ => format!("error {rc}"),
    }
}

// ---------------------------------------------------------------- enums

/// ADLOD8SettingId — index into every OD8 table. Verbatim from adl_defines.h.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Od8Id {
    GfxClkFMax = 0,
    GfxClkFMin = 1,
    UClkFMax = 8,
    PowerPercentage = 9,
    FanMinSpeed = 10,
    FanAcousticLimit = 11,
    FanTargetTemp = 12,
    OperatingTempMax = 13,
    AcTiming = 14,
    FanZeroRpmControl = 15,
    AutoUvEngineControl = 16,
    AutoOcEngineControl = 17,
    AutoOcMemoryControl = 18,
    FanCurveTemperature1 = 19,
    FanCurveSpeed1 = 20,
    FanCurveTemperature2 = 21,
    FanCurveSpeed2 = 22,
    FanCurveTemperature3 = 23,
    FanCurveSpeed3 = 24,
    FanCurveTemperature4 = 25,
    FanCurveSpeed4 = 26,
    FanCurveTemperature5 = 27,
    FanCurveSpeed5 = 28,
    UClkFMin = 34,
    OptimizedPowerMode = 36,
    OdVoltage = 37,
    TdcPercentage = 47,
}

impl Od8Id {
    /// Every member in ascending value order (`Enum.GetValues<Od8Id>()`).
    pub const ALL: [Od8Id; 27] = [
        Od8Id::GfxClkFMax,
        Od8Id::GfxClkFMin,
        Od8Id::UClkFMax,
        Od8Id::PowerPercentage,
        Od8Id::FanMinSpeed,
        Od8Id::FanAcousticLimit,
        Od8Id::FanTargetTemp,
        Od8Id::OperatingTempMax,
        Od8Id::AcTiming,
        Od8Id::FanZeroRpmControl,
        Od8Id::AutoUvEngineControl,
        Od8Id::AutoOcEngineControl,
        Od8Id::AutoOcMemoryControl,
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
        Od8Id::UClkFMin,
        Od8Id::OptimizedPowerMode,
        Od8Id::OdVoltage,
        Od8Id::TdcPercentage,
    ];

    /// Table index.
    pub const fn index(self) -> usize {
        self as i32 as usize
    }

    /// The member name, as `ToString()` printed it.
    pub fn name(self) -> String {
        format!("{self:?}")
    }
}

/// ADLOD8FeatureControl — the capability bitmask returned alongside the init table.
pub mod od8_feature {
    pub const GFX_CLK_LIMITS: i32 = 1 << 0;
    pub const GFX_CLK_CURVE: i32 = 1 << 1;
    pub const UCLK_MAX: i32 = 1 << 2;
    pub const POWER_LIMIT: i32 = 1 << 3;
    pub const ACOUSTIC_LIMIT_SCLK: i32 = 1 << 4;
    pub const FAN_SPEED_MIN: i32 = 1 << 5;
    pub const TEMPERATURE_FAN: i32 = 1 << 6;
    pub const TEMPERATURE_SYSTEM: i32 = 1 << 7;
    pub const MEMORY_TIMING_TUNE: i32 = 1 << 8;
    pub const FAN_ZERO_RPM_CONTROL: i32 = 1 << 9;
    pub const AUTO_UV_ENGINE: i32 = 1 << 10;
    pub const AUTO_OC_ENGINE: i32 = 1 << 11;
    pub const AUTO_OC_MEMORY: i32 = 1 << 12;
    pub const FAN_CURVE: i32 = 1 << 13;
    pub const OPTIMIZED_GPU_POWER_MODE: i32 = 1 << 16;
    pub const OD_VOLTAGE_LIMIT: i32 = 1 << 17;
    pub const GFX_VOLTAGE_LIMIT: i32 = 1 << 21;
    pub const TDC_LIMIT: i32 = 1 << 22;
    pub const FULL_CONTROL_MODE: i32 = 1 << 23;
    pub const POWER_GAUGE: i32 = 1 << 28;

    /// Every flag with its C# member name, ascending (`Enum.GetValues<Od8Feature>()`).
    pub const ALL: [(i32, &str); 20] = [
        (GFX_CLK_LIMITS, "GfxClkLimits"),
        (GFX_CLK_CURVE, "GfxClkCurve"),
        (UCLK_MAX, "UClkMax"),
        (POWER_LIMIT, "PowerLimit"),
        (ACOUSTIC_LIMIT_SCLK, "AcousticLimitSclk"),
        (FAN_SPEED_MIN, "FanSpeedMin"),
        (TEMPERATURE_FAN, "TemperatureFan"),
        (TEMPERATURE_SYSTEM, "TemperatureSystem"),
        (MEMORY_TIMING_TUNE, "MemoryTimingTune"),
        (FAN_ZERO_RPM_CONTROL, "FanZeroRpmControl"),
        (AUTO_UV_ENGINE, "AutoUvEngine"),
        (AUTO_OC_ENGINE, "AutoOcEngine"),
        (AUTO_OC_MEMORY, "AutoOcMemory"),
        (FAN_CURVE, "FanCurve"),
        (OPTIMIZED_GPU_POWER_MODE, "OptimizedGpuPowerMode"),
        (OD_VOLTAGE_LIMIT, "OdVoltageLimit"),
        (GFX_VOLTAGE_LIMIT, "GfxVoltageLimit"),
        (TDC_LIMIT, "TdcLimit"),
        (FULL_CONTROL_MODE, "FullControlMode"),
        (POWER_GAUGE, "PowerGauge"),
    ];
}

/// ADLSensorType — indices into the PMLog output array.
pub mod pm_log {
    pub const CORE_CLOCK_MHZ: i32 = 1;
    pub const MEMORY_CLOCK_MHZ: i32 = 2;
    pub const SOC_CLOCK_MHZ: i32 = 3;
    pub const FABRIC_CLOCK_MHZ: i32 = 44;
    pub const TEMPERATURE_EDGE: i32 = 8;
    pub const TEMPERATURE_MEMORY: i32 = 9;
    pub const FAN_RPM: i32 = 14;
    pub const FAN_PERCENT: i32 = 15;
    pub const ACTIVITY_GFX: i32 = 19;
    pub const ACTIVITY_MEM: i32 = 20;
    pub const GFX_VOLTAGE_MV: i32 = 21;
    pub const ASIC_POWER_W: i32 = 23;
    pub const TEMPERATURE_HOTSPOT: i32 = 27;
    pub const GFX_POWER_W: i32 = 30;
    pub const THROTTLER_STATUS: i32 = 35;
    pub const DGPU_POWER_LIMIT_W: i32 = 49;
    pub const BOARD_POWER_W: i32 = 73;
    pub const MAX_SENSORS: i32 = 256;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    #[test]
    fn adapter_info_layout() {
        assert_eq!(offset_of!(AdapterInfo, str_udid), 8);
        assert_eq!(offset_of!(AdapterInfo, i_bus_number), 264);
        assert_eq!(offset_of!(AdapterInfo, i_vendor_id), 276);
        assert_eq!(offset_of!(AdapterInfo, str_adapter_name), 280);
        assert_eq!(offset_of!(AdapterInfo, i_exist), 796);
        assert_eq!(offset_of!(AdapterInfo, str_driver_path), 800);
        assert_eq!(offset_of!(AdapterInfo, str_pnp_string), 1312);
        assert_eq!(offset_of!(AdapterInfo, i_os_display_index), 1568);
    }

    #[test]
    fn memory_info_x4_layout() {
        assert_eq!(offset_of!(MemoryInfoX4, str_memory_type), 8);
        assert_eq!(offset_of!(MemoryInfoX4, i_memory_bandwidth), 264);
        assert_eq!(offset_of!(MemoryInfoX4, i_vram_vendor_rev_id), 296);
        assert_eq!(offset_of!(MemoryInfoX4, i_memory_type), 304);
    }

    #[test]
    fn od8_ids_fit_the_table() {
        assert!(Od8Id::ALL.iter().all(|id| id.index() < OD8_FEATURE_COUNT as usize));
        assert!(Od8Id::ALL.windows(2).all(|w| (w[0] as i32) < (w[1] as i32)));
        assert_eq!(Od8Id::FanCurveSpeed5.index(), 28);
        assert_eq!(Od8Id::TdcPercentage.name(), "TdcPercentage");
        assert!(od8_feature::ALL.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn describe_codes() {
        assert_eq!(describe(-8), "not supported");
        assert_eq!(describe(-42), "error -42");
    }
}
