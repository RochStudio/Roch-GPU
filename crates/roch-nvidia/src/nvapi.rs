//! The subset of NVAPI the backend uses, in place of the vendored NvAPIWrapper.
//!
//! Everything goes through `nvapi_QueryInterface` in nvapi64.dll (System32 only). Function ids come
//! from NvAPIWrapper's `FunctionId.cs`; struct layouts mirror its `Native/**/Structures` exactly
//! (all `Pack = 8`, which for these all-`u32` structs is plain `repr(C)`), with a compile-time size
//! assert on each one. The version word of every struct is `size | (version << 16)`.
//!
//! Two failure kinds are kept apart because the original treated them differently:
//! [`NvError::Api`] is `NVIDIAApiException` (the driver answered with a status), and
//! [`NvError::NotSupported`] is `NVIDIANotSupportedException` (the entry point is not exported, or
//! no struct version was accepted). Several call sites catch only the first.

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::{Mutex, OnceLock};

use roch_core::GpuError;

/// NVAPI status code (`NvAPI_Status`).
pub type Status = i32;

pub const OK: Status = 0;
pub const INCOMPATIBLE_STRUCT_VERSION: Status = -9;
pub const NOT_SUPPORTED: Status = -104;

/// NVAPI_MAX_PHYSICAL_GPUS.
pub const MAX_PHYSICAL_GPUS: usize = 64;
/// NvAPI_ShortString.
pub const SHORT_STRING: usize = 64;
/// ClockFrequenciesV1.MaxClocksPerGPU.
pub const MAX_CLOCKS_PER_GPU: usize = 32;

/// The two exception types the wrapper threw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NvError {
    /// `NVIDIAApiException`: the call ran and returned this status.
    Api(Status),
    /// `NVIDIANotSupportedException`: the entry point is missing or no struct version fitted.
    NotSupported(&'static str),
}

pub type NvResult<T> = Result<T, NvError>;

/// DelegateFactory's message for an entry point the driver does not export.
pub const NOT_EXPORTED: &str = "Function identification number is invalid or not supported.";
/// The message when every accepted struct version was refused.
pub const NO_VERSION: &str = "This operation is not supported.";

impl NvError {
    /// The exception's `Message`: the driver's own text for a status, else the status name.
    pub fn message(&self) -> String {
        match self {
            NvError::Api(s) => error_message(*s).unwrap_or_else(|| status_name(*s)),
            NvError::NotSupported(m) => (*m).to_string(),
        }
    }

    /// `ex.Status` as the C# formatted it (the enum name); for a missing entry point, the message.
    pub fn status_text(&self) -> String {
        match self {
            NvError::Api(s) => status_name(*s),
            NvError::NotSupported(m) => (*m).to_string(),
        }
    }

    /// The exception's type name, for the diagnostics' `<Type: message>` lines.
    pub fn type_name(&self) -> &'static str {
        match self {
            NvError::Api(_) => "NVIDIAApiException",
            NvError::NotSupported(_) => "NVIDIANotSupportedException",
        }
    }

    pub fn is_api(&self) -> bool {
        matches!(self, NvError::Api(_))
    }
}

impl From<NvError> for GpuError {
    fn from(e: NvError) -> Self {
        GpuError::new(e.message())
    }
}

/// `NvAPIWrapper.Native.General.Status` names, for messages that printed `ex.Status`.
pub fn status_name(s: Status) -> String {
    let name = match s {
        0 => "Ok",
        -1 => "Error",
        -2 => "LibraryNotFound",
        -3 => "NoImplementation",
        -4 => "ApiNotInitialized",
        -5 => "InvalidArgument",
        -6 => "NvidiaDeviceNotFound",
        -7 => "EndEnumeration",
        -8 => "InvalidHandle",
        -9 => "IncompatibleStructureVersion",
        -10 => "HandleInvalidated",
        -11 => "OpenGLContextNotCurrent",
        -12 => "NoGLExpert",
        -13 => "InstrumentationDisabled",
        -14 => "InvalidPointer",
        -100 => "ExpectedLogicalGPUHandle",
        -101 => "ExpectedPhysicalGPUHandle",
        -102 => "ExpectedDisplayHandle",
        -103 => "InvalidCombination",
        -104 => "NotSupported",
        -105 => "PortIdNotFound",
        -106 => "ExpectedUnattachedDisplayHandle",
        -107 => "InvalidPerformanceLevel",
        -108 => "DeviceBusy",
        -109 => "NvPersistFileNotFound",
        -110 => "PersistDataNotFound",
        -111 => "ExpectedTVDisplay",
        -112 => "ExpectedTVDisplayOnDConnector",
        -113 => "NoActiveSLITopology",
        -114 => "SLIRenderingModeNotAllowed",
        -115 => "ExpectedDigitalFlatPanel",
        -116 => "ArgumentExceedMaxSize",
        -117 => "DeviceSwitchingNotAllowed",
        -118 => "TestingClocksNotSupported",
        -119 => "UnknownUnderScanConfig",
        -120 => "TimeoutReConfiguringGPUTopology",
        -121 => "DataNotFound",
        -122 => "ExpectedAnalogDisplay",
        -123 => "NoVideoLink",
        -124 => "RequiresReboot",
        -125 => "InvalidHybridMode",
        -126 => "MixedTargetTypes",
        -127 => "SYSWOW64NotSupported",
        -128 => "ImplicitSetGPUTopologyChangeNotAllowed",
        -129 => "RequestUserToCloseNonMigratableApps",
        -130 => "OutOfMemory",
        -131 => "WasStillDrawing",
        -132 => "FileNotFound",
        -133 => "TooManyUniqueStateObjects",
        -134 => "InvalidCall",
        -135 => "D3D101LibraryNotFound",
        -136 => "FunctionNotFound",
        -137 => "InvalidUserPrivilege",
        -138 => "ExpectedNonPrimaryDisplayHandle",
        -139 => "ExpectedComputeGPUHandle",
        -140 => "StereoNotInitialized",
        -141 => "StereoRegistryAccessFailed",
        -142 => "StereoRegistryProfileTypeNotSupported",
        -143 => "StereoRegistryValueNotSupported",
        -144 => "StereoNotEnabled",
        -145 => "StereoNotTurnedOn",
        -146 => "StereoInvalidDeviceInterface",
        -147 => "StereoParameterOutOfRange",
        -148 => "StereoFrustumAdjustModeNotSupported",
        -149 => "TopologyNotPossible",
        -150 => "ModeChangeFailed",
        -151 => "D3D11LibraryNotFound",
        -152 => "InvalidAddress",
        -153 => "StringTooSmall",
        -154 => "MatchingDeviceNotFound",
        -155 => "DriverRunning",
        -156 => "DriverNotRunning",
        -157 => "ErrorDriverReloadRequired",
        -158 => "SetNotAllowed",
        -159 => "AdvancedDisplayTopologyRequired",
        -160 => "SettingNotFound",
        -161 => "SettingSizeTooLarge",
        -162 => "TooManySettingsInProfile",
        -163 => "ProfileNotFound",
        -164 => "ProfileNameInUse",
        -165 => "ProfileNameEmpty",
        -166 => "ExecutableNotFound",
        -167 => "ExecutableAlreadyInUse",
        -168 => "DataTypeMismatch",
        -169 => "ProfileRemoved",
        -170 => "UnregisteredResource",
        -171 => "IdOutOfRange",
        -172 => "DisplayConfigValidationFailed",
        -173 => "DPMSTChanged",
        -174 => "InsufficientBuffer",
        -175 => "AccessDenied",
        -176 => "MosaicNotActive",
        -177 => "ShareResourceRelocated",
        -178 => "RequestUserToDisableDWM",
        -179 => "D3DDeviceLost",
        -180 => "InvalidConfiguration",
        -181 => "StereoHandshakeNotDone",
        -182 => "ExecutablePathIsAmbiguous",
        -183 => "DefaultStereoProfileIsNotDefined",
        -184 => "DefaultStereoProfileDoesNotExist",
        -185 => "ClusterAlreadyExists",
        -186 => "DPMSTDisplayIdExpected",
        -187 => "InvalidDisplayId",
        -188 => "StreamIsOutOfSync",
        -189 => "IncompatibleAudioDriver",
        -190 => "ValueAlreadySet",
        -191 => "Timeout",
        -192 => "GPUWorkstationFeatureIncomplete",
        -193 => "StereoInitActivationNotDone",
        -194 => "SyncNotActive",
        -195 => "SyncMasterNotFound",
        -196 => "InvalidSyncTopology",
        -197 => "ECIDSignAlgoUnsupported",
        -198 => "ECIDKeyVerificationFailed",
        -199 => "FirmwareOutOfDate",
        -200 => "FirmwareRevisionNotSupported",
        -201 => "LicenseCallerAuthenticationFailed",
        -202 => "D3DDeviceNotRegistered",
        -203 => "ResourceNotAcquired",
        -204 => "TimingNotSupported",
        -205 => "HDCPEncryptionFailed",
        -206 => "PCLKLimitationFailed",
        -207 => "NoConnectorFound",
        -208 => "HDCPDisabled",
        -209 => "ApiInUse",
        -210 => "NVIDIADisplayNotFound",
        _ => return s.to_string(),
    };
    name.to_string()
}

// ------------------------------------------------------------------ function ids (FunctionId.cs)

pub mod id {
    pub const INITIALIZE: u32 = 0x0150_E828;
    pub const UNLOAD: u32 = 0xD22B_DD7E;
    pub const GET_ERROR_MESSAGE: u32 = 0x6C2D_048C;
    pub const SYS_GET_DRIVER_AND_BRANCH_VERSION: u32 = 0x2926_AAAD;
    pub const ENUM_PHYSICAL_GPUS: u32 = 0xE5AC_921F;
    pub const GPU_GET_FULL_NAME: u32 = 0xCEEE_8E9F;
    pub const GPU_GET_SHORT_NAME: u32 = 0xD988_F0F3;
    pub const GPU_GET_BUS_ID: u32 = 0x1BE0_B8E5;
    pub const GPU_GET_BUS_SLOT_ID: u32 = 0x2A0A_350F;
    pub const GPU_GET_BUS_TYPE: u32 = 0x1BB1_8724;
    pub const GPU_GET_MEMORY_INFO: u32 = 0x07F9_B368;
    pub const GPU_GET_VBIOS_REVISION: u32 = 0xACC3_DA0A;
    pub const GPU_GET_VBIOS_OEM_REVISION: u32 = 0x2D43_FB31;
    pub const GPU_GET_VBIOS_VERSION_STRING: u32 = 0xA561_FD7D;
    pub const GPU_GET_PCI_IDENTIFIERS: u32 = 0x2DDF_B66E;
    pub const GPU_GET_GPU_CORE_COUNT: u32 = 0xC702_6A87;
    pub const GPU_GET_ROP_COUNT: u32 = 0xFDC1_29FA;
    pub const GPU_GET_RAM_TYPE: u32 = 0x57F7_CAAC;
    pub const GPU_GET_RAM_MAKER: u32 = 0x42AE_A16A;
    pub const GPU_GET_RAM_BUS_WIDTH: u32 = 0x7975_C581;
    pub const GPU_GET_PSTATES20: u32 = 0x6FF8_1213;
    pub const GPU_SET_PSTATES20: u32 = 0x0F4D_AE6B;
    pub const GPU_GET_CURRENT_PSTATE: u32 = 0x927D_A4F6;
    pub const GPU_GET_ALL_CLOCK_FREQUENCIES: u32 = 0xDCB6_16C3;
    pub const GPU_GET_THERMAL_SETTINGS: u32 = 0xE364_0A56;
    pub const GPU_GET_DYNAMIC_PSTATES_INFO_EX: u32 = 0x60DE_D2ED;
    pub const GPU_GET_USAGES: u32 = 0x189A_1FDF;
    pub const GPU_GET_TACH_READING: u32 = 0x5F60_8315;
    pub const GPU_GET_COOLER_SETTINGS: u32 = 0xDA14_1340;
    pub const GPU_SET_COOLER_LEVELS: u32 = 0x891F_A0AE;
    pub const GPU_RESTORE_COOLER_SETTINGS: u32 = 0x8F6E_D0FB;
    pub const GPU_CLIENT_FAN_COOLERS_GET_INFO: u32 = 0xFB85_B01E;
    pub const GPU_CLIENT_FAN_COOLERS_GET_STATUS: u32 = 0x35AE_D5E8;
    pub const GPU_CLIENT_FAN_COOLERS_GET_CONTROL: u32 = 0x814B_209F;
    pub const GPU_CLIENT_FAN_COOLERS_SET_CONTROL: u32 = 0xA589_71A5;
    pub const GPU_CLIENT_POWER_POLICIES_GET_INFO: u32 = 0x3420_6D86;
    pub const GPU_CLIENT_POWER_POLICIES_GET_STATUS: u32 = 0x7091_6171;
    pub const GPU_CLIENT_POWER_POLICIES_SET_STATUS: u32 = 0xAD95_F5ED;
    pub const GPU_CLIENT_POWER_TOPOLOGY_GET_STATUS: u32 = 0xEDCF_624E;
    pub const GPU_GET_THERMAL_POLICIES_INFO: u32 = 0x0D25_8BB5;
    pub const GPU_GET_THERMAL_POLICIES_STATUS: u32 = 0xE9C4_25A1;
    pub const GPU_SET_THERMAL_POLICIES_STATUS: u32 = 0x34C0_B13D;
    pub const GPU_PERF_POLICIES_GET_STATUS: u32 = 0x3D35_8A0C;
    pub const GPU_GET_CURRENT_VOLTAGE: u32 = 0x465F_9BCF;
    pub const GPU_GET_CORE_VOLTAGE_BOOST_PERCENT: u32 = 0x9DF2_3CA1;
    pub const GPU_SET_CORE_VOLTAGE_BOOST_PERCENT: u32 = 0xB930_6D9B;
    pub const GPU_GET_CLOCK_BOOST_MASK: u32 = 0x507B_4B59;
    pub const GPU_GET_CLOCK_BOOST_TABLE: u32 = 0x23F1_B133;
    pub const GPU_SET_CLOCK_BOOST_TABLE: u32 = 0x0733_E009;
    pub const GPU_GET_VFP_CURVE: u32 = 0x2153_7AD4;
    pub const GPU_GET_CLOCK_BOOST_RANGES: u32 = 0x64B4_3A6A;
    pub const GPU_GET_CLOCK_BOOST_LOCK: u32 = 0xE440_B867;
    pub const GPU_SET_CLOCK_BOOST_LOCK: u32 = 0x3944_2CFB;
    pub const GPU_ENABLE_OVERCLOCKED_PSTATES: u32 = 0xB23B_70EE;
}

// ------------------------------------------------------------------ library

/// A physical GPU handle (`NvPhysicalGpuHandle`), pointer-sized and passed by value.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuHandle(pub *mut c_void);

// SAFETY: the handle is an opaque token the driver hands out for the life of the NVAPI session;
// it is never dereferenced here, only passed back to the driver.
unsafe impl Send for GpuHandle {}
// SAFETY: as above; sharing the token is as harmless as copying it.
unsafe impl Sync for GpuHandle {}

impl GpuHandle {
    pub const NULL: GpuHandle = GpuHandle(std::ptr::null_mut());
}

type QueryInterfaceFn = unsafe extern "C" fn(u32) -> *mut c_void;

struct Lib {
    _lib: roch_core::native::Library,
    query: QueryInterfaceFn,
    /// Resolved entry points, as DelegateFactory caches its delegates. Absent ids are not cached.
    cache: Mutex<HashMap<u32, usize>>,
}

static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();

fn lib() -> Result<&'static Lib, String> {
    LIB.get_or_init(|| {
        let lib = roch_core::native::load_system32("nvapi64.dll").map_err(|e| e.message)?;
        // SAFETY: nvapi_QueryInterface is `void* __cdecl nvapi_QueryInterface(unsigned int)`.
        let query = unsafe { roch_core::native::symbol::<QueryInterfaceFn>(&lib, "nvapi_QueryInterface") }
            .map_err(|e| e.message)?;
        Ok(Lib { _lib: lib, query, cache: Mutex::new(HashMap::new()) })
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// True once nvapi64.dll is loaded (loading it on first call).
pub fn library_loaded() -> Result<(), String> {
    lib().map(|_| ())
}

/// `nvapi_QueryInterface(id)`; null when the library is missing or the id is not exported.
pub fn query_interface(id: u32) -> *mut c_void {
    let Ok(lib) = lib() else { return std::ptr::null_mut() };
    if let Ok(cache) = lib.cache.lock()
        && let Some(&p) = cache.get(&id)
    {
        return p as *mut c_void;
    }
    // SAFETY: the export takes a plain integer id and returns a pointer or null.
    let p = unsafe { (lib.query)(id) };
    if !p.is_null()
        && let Ok(mut cache) = lib.cache.lock()
    {
        cache.insert(id, p as usize);
    }
    p
}

/// Whether the driver exports this entry point.
pub fn exposes(id: u32) -> bool {
    !query_interface(id).is_null()
}

/// Resolve an entry point as a function pointer of type `F`.
///
/// # Safety
/// `F` must be the `extern "C"` signature the entry point really has.
pub unsafe fn resolve<F: Copy>(id: u32) -> NvResult<F> {
    const { assert!(size_of::<F>() == size_of::<*mut c_void>()) };
    let p = query_interface(id);
    if p.is_null() {
        return Err(NvError::NotSupported(NOT_EXPORTED));
    }
    // SAFETY: pointer-sized, and the caller vouches for the signature.
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&p) })
}

/// `(GpuHandle, void*) -> status`, the shape of every struct-taking GPU call.
pub type RawFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> Status;
type NoArgFn = unsafe extern "C" fn() -> Status;
type HandleFn = unsafe extern "C" fn(*mut c_void) -> Status;
type HandleU32Fn = unsafe extern "C" fn(*mut c_void, *mut u32) -> Status;
type HandleStringFn = unsafe extern "C" fn(*mut c_void, *mut u8) -> Status;

fn check(status: Status) -> NvResult<()> {
    if status == OK { Ok(()) } else { Err(NvError::Api(status)) }
}

/// Call a `(handle, struct*)` entry point.
///
/// # Safety
/// `T` must be the struct (or a buffer at least as large as the version word declares) the entry
/// point expects.
pub unsafe fn call_struct<T>(id: u32, h: GpuHandle, s: &mut T) -> NvResult<()> {
    // SAFETY: every id passed here is a `(NvPhysicalGpuHandle, NV_*_STRUCT*)` entry point.
    let f: RawFn = unsafe { resolve(id)? };
    // SAFETY: `s` is a live, exclusively borrowed struct of the declared size.
    check(unsafe { f(h.0, s as *mut T as *mut c_void) })
}

/// Size-and-version word for a struct.
pub const fn version_word(size: usize, version: u32) -> u32 {
    size as u32 | (version << 16)
}

/// Zeroed struct on the heap (several are 7-9 KB).
pub fn zeroed<T: Zeroable>() -> Box<T> {
    // SAFETY: `Zeroable` is only implemented for plain-integer `repr(C)` structs, for which the
    // all-zero bit pattern is valid.
    unsafe { Box::<T>::new_zeroed().assume_init() }
}

/// Marker for FFI structs made only of integers (and arrays of them), valid when all-zero.
///
/// # Safety
/// Only implement for types where every bit pattern of zeros is a valid value.
pub unsafe trait Zeroable {}

/// Decode a C `char[]` up to the first NUL (ByValTStr / ANSI), without trimming.
pub fn ansi(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

// ------------------------------------------------------------------ general

pub fn initialize() -> NvResult<()> {
    // SAFETY: NvAPI_Initialize takes no arguments.
    let f: NoArgFn = unsafe { resolve(id::INITIALIZE)? };
    // SAFETY: as above.
    check(unsafe { f() })
}

pub fn unload() -> NvResult<()> {
    // SAFETY: NvAPI_Unload takes no arguments.
    let f: NoArgFn = unsafe { resolve(id::UNLOAD)? };
    // SAFETY: as above.
    check(unsafe { f() })
}

/// `GeneralApi.GetErrorMessage`: the driver's text for a status, `None` when it has none.
pub fn error_message(status: Status) -> Option<String> {
    type F = unsafe extern "C" fn(Status, *mut u8) -> Status;
    // SAFETY: NvAPI_GetErrorMessage(NvAPI_Status, NvAPI_ShortString).
    let f: F = unsafe { resolve(id::GET_ERROR_MESSAGE).ok()? };
    let mut buf = [0u8; SHORT_STRING];
    // SAFETY: the buffer is a full NvAPI_ShortString.
    if unsafe { f(status, buf.as_mut_ptr()) } != OK {
        return None;
    }
    Some(ansi(&buf))
}

/// `NVIDIA.DriverVersion`: e.g. 61692 for 616.92.
pub fn driver_version() -> NvResult<u32> {
    type F = unsafe extern "C" fn(*mut u32, *mut u8) -> Status;
    // SAFETY: NvAPI_SYS_GetDriverAndBranchVersion(NvU32*, NvAPI_ShortString).
    let f: F = unsafe { resolve(id::SYS_GET_DRIVER_AND_BRANCH_VERSION)? };
    let mut v = 0u32;
    let mut branch = [0u8; SHORT_STRING];
    // SAFETY: both out-pointers are valid for their full size.
    check(unsafe { f(&mut v, branch.as_mut_ptr()) })?;
    Ok(v)
}

pub fn enum_physical_gpus() -> NvResult<Vec<GpuHandle>> {
    type F = unsafe extern "C" fn(*mut GpuHandle, *mut u32) -> Status;
    // SAFETY: NvAPI_EnumPhysicalGPUs(NvPhysicalGpuHandle[NVAPI_MAX_PHYSICAL_GPUS], NvU32*).
    let f: F = unsafe { resolve(id::ENUM_PHYSICAL_GPUS)? };
    let mut handles = [GpuHandle::NULL; MAX_PHYSICAL_GPUS];
    let mut count = 0u32;
    // SAFETY: the array holds the documented maximum.
    check(unsafe { f(handles.as_mut_ptr(), &mut count) })?;
    Ok(handles.iter().take((count as usize).min(MAX_PHYSICAL_GPUS)).copied().collect())
}

// ------------------------------------------------------------------ simple GPU getters

fn get_u32(fid: u32, h: GpuHandle) -> NvResult<u32> {
    // SAFETY: every id passed here is `(NvPhysicalGpuHandle, NvU32*)` (or a 4-byte enum out).
    let f: HandleU32Fn = unsafe { resolve(fid)? };
    let mut v = 0u32;
    // SAFETY: `v` is a valid out-pointer.
    check(unsafe { f(h.0, &mut v) })?;
    Ok(v)
}

fn get_string(fid: u32, h: GpuHandle) -> NvResult<String> {
    // SAFETY: every id passed here is `(NvPhysicalGpuHandle, NvAPI_ShortString)`.
    let f: HandleStringFn = unsafe { resolve(fid)? };
    let mut buf = [0u8; SHORT_STRING];
    // SAFETY: a full NvAPI_ShortString.
    check(unsafe { f(h.0, buf.as_mut_ptr()) })?;
    Ok(ansi(&buf))
}

pub fn full_name(h: GpuHandle) -> NvResult<String> {
    get_string(id::GPU_GET_FULL_NAME, h)
}
pub fn short_name(h: GpuHandle) -> NvResult<String> {
    get_string(id::GPU_GET_SHORT_NAME, h)
}
pub fn vbios_version_string(h: GpuHandle) -> NvResult<String> {
    get_string(id::GPU_GET_VBIOS_VERSION_STRING, h)
}
pub fn bus_id(h: GpuHandle) -> NvResult<i32> {
    get_u32(id::GPU_GET_BUS_ID, h).map(|v| v as i32)
}
pub fn bus_slot_id(h: GpuHandle) -> NvResult<i32> {
    get_u32(id::GPU_GET_BUS_SLOT_ID, h).map(|v| v as i32)
}
/// GPUBusType: 0 Undefined, 1 PCI, 2 AGP, 3 PCIExpress, 4 FPCI, 5 AXI.
pub fn bus_type(h: GpuHandle) -> NvResult<i32> {
    get_u32(id::GPU_GET_BUS_TYPE, h).map(|v| v as i32)
}
pub fn vbios_revision(h: GpuHandle) -> NvResult<u32> {
    get_u32(id::GPU_GET_VBIOS_REVISION, h)
}
pub fn vbios_oem_revision(h: GpuHandle) -> NvResult<u32> {
    get_u32(id::GPU_GET_VBIOS_OEM_REVISION, h)
}
pub fn gpu_core_count(h: GpuHandle) -> NvResult<u32> {
    get_u32(id::GPU_GET_GPU_CORE_COUNT, h)
}
pub fn rop_count(h: GpuHandle) -> NvResult<u32> {
    get_u32(id::GPU_GET_ROP_COUNT, h)
}
pub fn ram_type(h: GpuHandle) -> NvResult<u32> {
    get_u32(id::GPU_GET_RAM_TYPE, h)
}
pub fn ram_maker(h: GpuHandle) -> NvResult<u32> {
    get_u32(id::GPU_GET_RAM_MAKER, h)
}
pub fn ram_bus_width(h: GpuHandle) -> NvResult<u32> {
    get_u32(id::GPU_GET_RAM_BUS_WIDTH, h)
}
pub fn tach_reading(h: GpuHandle) -> NvResult<u32> {
    get_u32(id::GPU_GET_TACH_READING, h)
}
/// PerformanceStateId of the current pstate.
pub fn current_pstate(h: GpuHandle) -> NvResult<u32> {
    get_u32(id::GPU_GET_CURRENT_PSTATE, h)
}

/// `(deviceId, subSystemId, revisionId, extDeviceId)`.
pub fn pci_identifiers(h: GpuHandle) -> NvResult<(u32, u32, u32, u32)> {
    type F = unsafe extern "C" fn(*mut c_void, *mut u32, *mut u32, *mut u32, *mut u32) -> Status;
    // SAFETY: NvAPI_GPU_GetPCIIdentifiers(handle, NvU32*, NvU32*, NvU32*, NvU32*).
    let f: F = unsafe { resolve(id::GPU_GET_PCI_IDENTIFIERS)? };
    let (mut a, mut b, mut c, mut d) = (0u32, 0u32, 0u32, 0u32);
    // SAFETY: four valid out-pointers.
    check(unsafe { f(h.0, &mut a, &mut b, &mut c, &mut d) })?;
    Ok((a, b, c, d))
}

/// `PCIIdentifiers` as the wrapper derived it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciIdentifiers {
    pub device_id: u32,
    pub sub_system_id: u32,
    pub revision_id: u32,
    pub external_device_id: u16,
}

/// `GPUBusInformation.PCIIdentifiers`: `None` unless the bus is PCI, PCIe or FPCI.
pub fn pci_identifiers_checked(h: GpuHandle) -> NvResult<Option<PciIdentifiers>> {
    let bt = bus_type(h)?;
    if !(bt == 4 || bt == 1 || bt == 3) {
        return Ok(None);
    }
    let (device_id, sub_system_id, revision_id, ext) = pci_identifiers(h)?;
    let ext = ext as i32;
    let external_device_id = if ext > 0 { ext as u16 } else { (device_id >> 16) as u16 };
    Ok(Some(PciIdentifiers { device_id, sub_system_id, revision_id, external_device_id }))
}

/// `PhysicalGPU.Bios.VersionString`: all three VBIOS calls must succeed; upper-cased.
pub fn bios_version_string(h: GpuHandle) -> NvResult<String> {
    vbios_revision(h)?;
    vbios_oem_revision(h)?;
    Ok(vbios_version_string(h)?.to_uppercase())
}

// ------------------------------------------------------------------ structures

/// Implement `Zeroable` and pin the size for a list of structs.
macro_rules! nv_struct {
    ($($t:ty = $size:expr),* $(,)?) => {
        $(
            // SAFETY: integers and arrays of integers only.
            unsafe impl Zeroable for $t {}
            const _: () = assert!(size_of::<$t>() == $size);
        )*
    };
}

/// ClockDomainInfo.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ClockDomainInfo {
    pub is_present: u32,
    pub frequency_khz: u32,
}

impl ClockDomainInfo {
    pub fn present(&self) -> bool {
        self.is_present & 1 != 0
    }
}

/// ClockFrequenciesV1/V2/V3 (one layout, three versions).
#[repr(C)]
pub struct ClockFrequencies {
    pub version: u32,
    pub clock_type_and_reserve: u32,
    pub clocks: [ClockDomainInfo; MAX_CLOCKS_PER_GPU],
}

/// ThermalSettingsV1/V2 sensor (`controller`, defaults, current, `target`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ThermalSensor {
    pub controller: i32,
    pub default_min: i32,
    pub default_max: i32,
    pub current: i32,
    pub target: i32,
}

/// ThermalSettingsV1/V2.
#[repr(C)]
pub struct ThermalSettings {
    pub version: u32,
    pub count: u32,
    pub sensors: [ThermalSensor; 3],
}

/// DisplayDriverMemoryInfoV3 (V2 drops the last two fields, V1 the last three).
#[repr(C)]
pub struct MemoryInfoV3 {
    pub version: u32,
    pub dedicated_kb: u32,
    pub available_dedicated_kb: u32,
    pub system_video_kb: u32,
    pub shared_system_kb: u32,
    pub current_available_dedicated_kb: u32,
    pub evictions_size: u32,
    pub eviction_count: u32,
}

/// PerformanceStates20ParameterDelta.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PStates20Delta {
    pub value: i32,
    pub min: i32,
    pub max: i32,
}

/// PerformanceStates20ClockEntryV1. `dependent` is the 20-byte single/range union.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PStates20ClockEntry {
    pub domain_id: u32,
    pub clock_type: u32,
    pub flags: u32,
    pub freq_delta_khz: PStates20Delta,
    pub dependent: [u32; 5],
}

impl PStates20ClockEntry {
    pub fn editable(&self) -> bool {
        self.flags & 1 != 0
    }
}

/// PerformanceStates20BaseVoltageEntryV1.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PStates20BaseVoltageEntry {
    pub domain_id: u32,
    pub flags: u32,
    pub value_uv: u32,
    pub value_delta: PStates20Delta,
}

/// PerformanceStates20InfoV1.PerformanceState20.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PState20 {
    pub id: u32,
    pub flags: u32,
    pub clocks: [PStates20ClockEntry; 8],
    pub base_voltages: [PStates20BaseVoltageEntry; 4],
}

/// PerformanceStates20InfoV1.
#[repr(C)]
pub struct PStates20InfoV1 {
    pub version: u32,
    pub flags: u32,
    pub num_pstates: u32,
    pub num_clocks: u32,
    pub num_base_voltages: u32,
    pub pstates: [PState20; 16],
}

/// PerformanceStates20OverVoltingSetting.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PStates20OverVolting {
    pub num_voltages: u32,
    pub voltages: [PStates20BaseVoltageEntry; 4],
}

/// PerformanceStates20InfoV2 and V3 (same layout).
#[repr(C)]
pub struct PStates20InfoV2 {
    pub version: u32,
    pub flags: u32,
    pub num_pstates: u32,
    pub num_clocks: u32,
    pub num_base_voltages: u32,
    pub pstates: [PState20; 16],
    pub over_volting: PStates20OverVolting,
}

/// PrivateClockBoostLockV2.ClockBoostLock.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ClockBoostLock {
    pub clock_domain: u32,
    pub unknown1: u32,
    pub lock_mode: u32,
    pub unknown2: u32,
    pub voltage_uv: u32,
    pub unknown3: u32,
}

/// PrivateClockBoostLockV2.
#[repr(C)]
pub struct ClockBoostLockV2 {
    pub version: u32,
    pub unknown: u32,
    pub count: u32,
    pub locks: [ClockBoostLock; MAX_CLOCKS_PER_GPU],
}

/// PrivateClockBoostMasksV1.ClockBoostMask.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ClockBoostMask {
    pub unknown: [u32; 4],
    pub memory_delta: i32,
    pub gpu_delta: i32,
}

/// PrivateClockBoostMasksV1.
#[repr(C)]
pub struct ClockBoostMasksV1 {
    pub version: u32,
    pub masks: [u32; 4],
    pub unknown1: [u32; 8],
    pub boost_masks: [ClockBoostMask; 103],
    pub unknown2: [u32; 916],
}

/// PrivateClockBoostRangesV1.ClockBoostRange.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ClockBoostRange {
    pub unknown1: u32,
    pub clock_type: u32,
    pub unknown2_9: [u32; 8],
    pub range_max_khz: i32,
    pub range_min_khz: i32,
    pub max_temperature: i32,
    pub unknown10_14: [u32; 5],
}

/// PrivateClockBoostRangesV1.
#[repr(C)]
pub struct ClockBoostRangesV1 {
    pub version: u32,
    pub count: u32,
    pub unknown: [u32; 8],
    pub ranges: [ClockBoostRange; MAX_CLOCKS_PER_GPU],
}

/// PrivateVoltageBoostPercentV1.
#[repr(C)]
pub struct VoltageBoostPercentV1 {
    pub version: u32,
    pub percent: u32,
    pub unknown: [u32; 8],
}

/// PrivateVoltageStatusV1.
#[repr(C)]
pub struct VoltageStatusV1 {
    pub version: u32,
    pub unknown1: u32,
    pub unknown2: [u32; 8],
    pub value_uv: u32,
    pub unknown3: [u32; 8],
}

/// PrivatePowerPoliciesInfoV1.PowerPolicyInfoEntry.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PowerPolicyInfoEntry {
    pub pstate: u32,
    pub unknown1: u32,
    pub unknown2: u32,
    pub min_pcm: u32,
    pub unknown3: u32,
    pub unknown4: u32,
    pub default_pcm: u32,
    pub unknown5: u32,
    pub unknown6: u32,
    pub max_pcm: u32,
    pub unknown7: u32,
}

/// PrivatePowerPoliciesInfoV1.
#[repr(C)]
pub struct PowerPoliciesInfoV1 {
    pub version: u32,
    pub valid: u8,
    pub count: u8,
    pub entries: [PowerPolicyInfoEntry; 4],
}

/// PrivatePowerPoliciesStatusV1.PowerPolicyStatusEntry.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PowerPolicyStatusEntry {
    pub pstate: u32,
    pub unknown1: u32,
    pub target_pcm: u32,
    pub unknown2: u32,
}

/// PrivatePowerPoliciesStatusV1.
#[repr(C)]
pub struct PowerPoliciesStatusV1 {
    pub version: u32,
    pub count: u32,
    pub entries: [PowerPolicyStatusEntry; 4],
}

/// PrivatePowerTopologiesStatusV1.PowerTopologiesStatusEntry.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PowerTopologyEntry {
    pub domain: u32,
    pub unknown2: u32,
    pub usage_pcm: u32,
    pub unknown3: u32,
}

/// PrivatePowerTopologiesStatusV1.
#[repr(C)]
pub struct PowerTopologiesStatusV1 {
    pub version: u32,
    pub count: u32,
    pub entries: [PowerTopologyEntry; 4],
}

/// PrivateThermalPoliciesInfoV2.ThermalPoliciesInfoEntry.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ThermalPolicyInfoEntry {
    pub controller: i32,
    pub unknown1: u32,
    pub min_c: i32,
    pub default_c: i32,
    pub max_c: i32,
    pub unknown2: u32,
}

/// PrivateThermalPoliciesInfoV2.
#[repr(C)]
pub struct ThermalPoliciesInfoV2 {
    pub version: u32,
    pub count: u8,
    pub unknown: u8,
    pub entries: [ThermalPolicyInfoEntry; 4],
}

/// PrivateThermalPoliciesStatusV2.ThermalPoliciesStatusEntry. `target` is °C × 256.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ThermalPolicyStatusEntry {
    pub controller: i32,
    pub target: i32,
    pub pstate: u32,
}

impl ThermalPolicyStatusEntry {
    /// `TargetTemperature`: the raw value shifted down by 8.
    pub fn target_c(&self) -> i32 {
        self.target >> 8
    }
}

/// PrivateThermalPoliciesStatusV2.
#[repr(C)]
pub struct ThermalPoliciesStatusV2 {
    pub version: u32,
    pub count: u32,
    pub entries: [ThermalPolicyStatusEntry; 4],
}

/// PrivateVFPCurveV1.VFPCurveEntry (stride 7: frequency at +1, voltage at +2).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VfpCurveEntry {
    pub unknown1: u32,
    pub frequency_khz: u32,
    pub voltage_uv: u32,
    pub unknown2_5: [u32; 4],
}

/// PrivateVFPCurveV1.
#[repr(C)]
pub struct VfpCurveV1 {
    pub version: u32,
    pub masks: [u32; 4],
    pub unknown1: [u32; 12],
    pub gpu_entries: [VfpCurveEntry; 80],
    pub memory_entries: [VfpCurveEntry; 23],
    pub unknown2: [u32; 1064],
}

/// PrivateClockBoostTableV1.GPUDelta (stride 9: delta at +5).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ClockBoostDelta {
    pub unknown1_5: [u32; 5],
    pub frequency_delta_khz: i32,
    pub unknown7_9: [u32; 3],
}

/// PrivateClockBoostTableV1.
#[repr(C)]
pub struct ClockBoostTableV1 {
    pub version: u32,
    pub masks: [u32; 4],
    pub unknown1: [u32; 12],
    pub gpu_deltas: [ClockBoostDelta; 80],
    pub memory_filled: [u32; 23],
    pub memory_deltas: [i32; 23],
    pub unknown2: [u32; 1529],
}

/// PrivateCoolerSettingsV1.CoolerSetting.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CoolerSetting {
    pub cooler_type: u32,
    pub controller: u32,
    pub default_min: u32,
    pub default_max: u32,
    pub current_min: u32,
    pub current_max: u32,
    pub current_level: u32,
    pub default_policy: u32,
    pub current_policy: u32,
    pub target: u32,
    pub control_mode: u32,
    pub is_active: u32,
}

/// PrivateCoolerSettingsV1.
#[repr(C)]
pub struct CoolerSettingsV1 {
    pub version: u32,
    pub count: u32,
    pub settings: [CoolerSetting; 3],
}

/// PrivateCoolerLevelsV1.CoolerLevel.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CoolerLevel {
    pub level: u32,
    pub policy: u32,
}

/// PrivateCoolerLevelsV1.
#[repr(C)]
pub struct CoolerLevelsV1 {
    pub version: u32,
    pub levels: [CoolerLevel; 3],
}

/// PrivateFanCoolersInfoV1.FanCoolersInfoEntry.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FanCoolerInfoEntry {
    pub cooler_id: u32,
    pub unknown3: u32,
    pub unknown4: u32,
    pub max_rpm: u32,
    pub reserved: [u32; 8],
}

/// PrivateFanCoolersInfoV1.
#[repr(C)]
pub struct FanCoolersInfoV1 {
    pub version: u32,
    pub unknown1: u32,
    pub count: u32,
    pub reserved: [u32; 8],
    pub entries: [FanCoolerInfoEntry; 32],
}

/// PrivateFanCoolersStatusV1.FanCoolersStatusEntry.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FanCoolerStatusEntry {
    pub cooler_id: u32,
    pub current_rpm: u32,
    pub current_min: u32,
    pub current_max: u32,
    pub current_level: u32,
    pub reserved: [u32; 8],
}

/// PrivateFanCoolersStatusV1.
#[repr(C)]
pub struct FanCoolersStatusV1 {
    pub version: u32,
    pub count: u32,
    pub reserved: [u32; 8],
    pub entries: [FanCoolerStatusEntry; 32],
}

/// PrivateFanCoolersControlV1.FanCoolersControlEntry.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FanCoolerControlEntry {
    pub cooler_id: u32,
    pub level: u32,
    pub control_mode: u32,
    pub reserved: [u32; 8],
}

/// PrivateFanCoolersControlV1.
#[repr(C)]
pub struct FanCoolersControlV1 {
    pub version: u32,
    pub unknown_uint: u32,
    pub count: u32,
    pub reserved: [u32; 8],
    pub entries: [FanCoolerControlEntry; 32],
}

/// PrivateUsagesInfoV1.UsagesInfoEntry.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UsageEntry {
    pub is_present: u32,
    pub percentage: u32,
    pub unknown1: u32,
    pub unknown2: u32,
}

/// PrivateUsagesInfoV1.
#[repr(C)]
pub struct UsagesInfoV1 {
    pub version: u32,
    pub unknown: u32,
    pub entries: [UsageEntry; 8],
}

/// DynamicPerformanceStatesInfoV1.UtilizationDomainInfo.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UtilizationDomainInfo {
    pub is_present: u32,
    pub percentage: u32,
}

/// DynamicPerformanceStatesInfoV1.
#[repr(C)]
pub struct DynamicPStatesInfoV1 {
    pub version: u32,
    pub flags: u32,
    pub domains: [UtilizationDomainInfo; 8],
}

/// PrivatePerformanceStatusV1.
#[repr(C)]
pub struct PerformanceStatusV1 {
    pub version: u32,
    pub unknown1: u32,
    pub timer_ns: u64,
    pub performance_limit: u32,
    pub unknown2: u32,
    pub unknown3: u32,
    pub unknown4: u32,
    pub timers_ns: [u64; 3],
    pub unknown5: [u32; 326],
}

/// NvApiPrivate's NV_THERMAL_SENSORS v2 (hot spot / memory junction).
#[repr(C)]
pub struct ThermalSensorsV2 {
    pub version: u32,
    pub mask: u32,
    pub reserved: [i32; 8],
    pub temperatures: [i32; 32],
}

nv_struct! {
    ClockFrequencies = 264,
    ThermalSettings = 68,
    MemoryInfoV3 = 32,
    PStates20InfoV1 = 7316,
    PStates20InfoV2 = 7416,
    ClockBoostLockV2 = 780,
    ClockBoostMasksV1 = 6188,
    ClockBoostRangesV1 = 2344,
    VoltageBoostPercentV1 = 40,
    VoltageStatusV1 = 76,
    PowerPoliciesInfoV1 = 184,
    PowerPoliciesStatusV1 = 72,
    PowerTopologiesStatusV1 = 72,
    ThermalPoliciesInfoV2 = 104,
    ThermalPoliciesStatusV2 = 56,
    VfpCurveV1 = 7208,
    ClockBoostTableV1 = 9248,
    CoolerSettingsV1 = 152,
    CoolerLevelsV1 = 28,
    FanCoolersInfoV1 = 1580,
    FanCoolersStatusV1 = 1704,
    FanCoolersControlV1 = 1452,
    UsagesInfoV1 = 136,
    DynamicPStatesInfoV1 = 72,
    PerformanceStatusV1 = 1360,
    ThermalSensorsV2 = 168,
}

const _: () = assert!(size_of::<PStates20ClockEntry>() == 44);
const _: () = assert!(size_of::<PStates20BaseVoltageEntry>() == 24);
const _: () = assert!(size_of::<PState20>() == 456);
const _: () = assert!(size_of::<ClockBoostLock>() == 24);
const _: () = assert!(size_of::<ClockBoostRange>() == 72);
const _: () = assert!(size_of::<VfpCurveEntry>() == 28);
const _: () = assert!(size_of::<ClockBoostDelta>() == 36);
const _: () = assert!(size_of::<CoolerSetting>() == 48);
const _: () = assert!(size_of::<GpuHandle>() == size_of::<usize>());

/// Allocate a zeroed struct with its version word set.
pub fn new_versioned<T: Zeroable>(version: u32) -> Box<T> {
    let mut b = zeroed::<T>();
    let word = version_word(size_of::<T>(), version);
    // SAFETY: every versioned struct starts with its u32 version word.
    unsafe { *(b.as_mut() as *mut T as *mut u32) = word };
    b
}

// ------------------------------------------------------------------ versioned getters

/// GPUApi.GetPerformanceStates20: V3, V2, V1 in that order, skipping IncompatibleStructureVersion.
pub fn pstates20(h: GpuHandle) -> NvResult<PStates20> {
    // SAFETY: probing the entry point's existence first, as GetDelegate does before the loop.
    unsafe { resolve::<RawFn>(id::GPU_GET_PSTATES20)? };
    for ver in [3u32, 2] {
        let mut s = new_versioned::<PStates20InfoV2>(ver);
        // SAFETY: V2/V3 layout, version word matches its size.
        match unsafe { call_struct(id::GPU_GET_PSTATES20, h, s.as_mut()) } {
            Err(NvError::Api(INCOMPATIBLE_STRUCT_VERSION)) => continue,
            Err(e) => return Err(e),
            Ok(()) => {
                return Ok(PStates20::from_parts(s.flags, s.num_pstates, s.num_clocks, &s.pstates));
            }
        }
    }
    let mut s = new_versioned::<PStates20InfoV1>(1);
    // SAFETY: V1 layout.
    match unsafe { call_struct(id::GPU_GET_PSTATES20, h, s.as_mut()) } {
        Err(NvError::Api(INCOMPATIBLE_STRUCT_VERSION)) => Err(NvError::NotSupported(NO_VERSION)),
        Err(e) => Err(e),
        Ok(()) => Ok(PStates20::from_parts(s.flags, s.num_pstates, s.num_clocks, &s.pstates)),
    }
}

/// The parsed parts of IPerformanceStates20Info the backend reads.
#[derive(Debug, Clone, Default)]
pub struct PStates20 {
    pub is_editable: bool,
    /// Per pstate in driver order: `(pstate id, its first num_clocks clock entries)`.
    pub clocks: Vec<(u32, Vec<PStates20ClockEntry>)>,
}

impl PStates20 {
    fn from_parts(flags: u32, num_pstates: u32, num_clocks: u32, pstates: &[PState20; 16]) -> Self {
        let n = (num_pstates as usize).min(16);
        let c = (num_clocks as usize).min(8);
        PStates20 {
            is_editable: flags & 1 != 0,
            clocks: pstates[..n].iter().map(|p| (p.id, p.clocks[..c].to_vec())).collect(),
        }
    }

    /// `ps.Clocks.TryGetValue(id, ...)` — the first pstate with that id.
    pub fn clocks_for(&self, pstate: u32) -> Option<&[PStates20ClockEntry]> {
        self.clocks.iter().find(|(id, _)| *id == pstate).map(|(_, c)| c.as_slice())
    }
}

/// GPUApi.SetPerformanceStates20 with a V1 payload.
pub fn set_pstates20_v1(h: GpuHandle, s: &mut PStates20InfoV1) -> NvResult<()> {
    // SAFETY: V1 struct with its own version word.
    unsafe { call_struct(id::GPU_SET_PSTATES20, h, s) }
}

/// GPUApi.GetMemoryInfo: V3, V2, V1. Returns `(dedicated, available, current_available)` in kB.
pub fn memory_info(h: GpuHandle) -> NvResult<MemoryInfo> {
    // SAFETY: existence probe.
    unsafe { resolve::<RawFn>(id::GPU_GET_MEMORY_INFO)? };
    for (ver, size) in [(3u32, 32usize), (2, 24), (1, 20)] {
        let mut s = zeroed::<MemoryInfoV3>();
        s.version = version_word(size, ver);
        // SAFETY: the V3 buffer is at least as large as every declared size.
        match unsafe { call_struct(id::GPU_GET_MEMORY_INFO, h, s.as_mut()) } {
            Err(NvError::Api(INCOMPATIBLE_STRUCT_VERSION)) => continue,
            Err(e) => return Err(e),
            Ok(()) => {
                return Ok(MemoryInfo {
                    dedicated_kb: s.dedicated_kb,
                    available_dedicated_kb: s.available_dedicated_kb,
                    current_available_dedicated_kb: if ver == 1 {
                        s.available_dedicated_kb
                    } else {
                        s.current_available_dedicated_kb
                    },
                });
            }
        }
    }
    Err(NvError::NotSupported(NO_VERSION))
}

/// IDisplayDriverMemoryInfo, the three fields used.
#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryInfo {
    pub dedicated_kb: u32,
    pub available_dedicated_kb: u32,
    pub current_available_dedicated_kb: u32,
}

/// GPUApi.GetAllClockFrequencies with an explicit V2 payload of the given clock type
/// (0 current, 1 base, 2 boost) — what `CurrentClockFrequencies` & co. pass.
pub fn clock_frequencies(h: GpuHandle, clock_type: u32) -> NvResult<Box<ClockFrequencies>> {
    let mut s = new_versioned::<ClockFrequencies>(2);
    s.clock_type_and_reserve = clock_type & 0b11;
    // SAFETY: V2 layout.
    unsafe { call_struct(id::GPU_GET_ALL_CLOCK_FREQUENCIES, h, s.as_mut())? };
    Ok(s)
}

/// GPUApi.GetThermalSettings(All): V2 then V1. Returns the first `count` sensors.
pub fn thermal_settings(h: GpuHandle) -> NvResult<Vec<ThermalSensor>> {
    type F = unsafe extern "C" fn(*mut c_void, i32, *mut ThermalSettings) -> Status;
    // SAFETY: NvAPI_GPU_GetThermalSettings(handle, NvU32 sensorIndex, NV_GPU_THERMAL_SETTINGS*).
    let f: F = unsafe { resolve(id::GPU_GET_THERMAL_SETTINGS)? };
    for ver in [2u32, 1] {
        let mut s = new_versioned::<ThermalSettings>(ver);
        // SAFETY: valid struct of the declared size; 15 = ThermalSettingsTarget.All.
        let st = unsafe { f(h.0, 15, s.as_mut()) };
        if st == INCOMPATIBLE_STRUCT_VERSION {
            continue;
        }
        check(st)?;
        let n = (s.count as usize).min(3);
        return Ok(s.sensors[..n].to_vec());
    }
    Err(NvError::NotSupported(NO_VERSION))
}

/// A simple single-version struct getter.
pub fn get_versioned<T: Zeroable>(fid: u32, h: GpuHandle, version: u32) -> NvResult<Box<T>> {
    let mut s = new_versioned::<T>(version);
    // SAFETY: T is the struct this id takes; its version word is set.
    unsafe { call_struct(fid, h, s.as_mut())? };
    Ok(s)
}

pub fn power_policies_info(h: GpuHandle) -> NvResult<Vec<PowerPolicyInfoEntry>> {
    let s = get_versioned::<PowerPoliciesInfoV1>(id::GPU_CLIENT_POWER_POLICIES_GET_INFO, h, 1)?;
    Ok(s.entries.iter().take((s.count as usize).min(4)).copied().collect())
}

pub fn power_policies_status(h: GpuHandle) -> NvResult<Vec<PowerPolicyStatusEntry>> {
    let s = get_versioned::<PowerPoliciesStatusV1>(id::GPU_CLIENT_POWER_POLICIES_GET_STATUS, h, 1)?;
    Ok(s.entries.iter().take((s.count as usize).min(4)).copied().collect())
}

pub fn set_power_policies_status(h: GpuHandle, entries: &[PowerPolicyStatusEntry]) -> NvResult<()> {
    if entries.is_empty() || entries.len() > 4 {
        return Err(NvError::NotSupported("Array is null or empty."));
    }
    let mut s = new_versioned::<PowerPoliciesStatusV1>(1);
    s.count = entries.len() as u32;
    s.entries[..entries.len()].copy_from_slice(entries);
    // SAFETY: V1 struct.
    unsafe { call_struct(id::GPU_CLIENT_POWER_POLICIES_SET_STATUS, h, s.as_mut()) }
}

pub fn power_topology_status(h: GpuHandle) -> NvResult<Vec<PowerTopologyEntry>> {
    let s = get_versioned::<PowerTopologiesStatusV1>(id::GPU_CLIENT_POWER_TOPOLOGY_GET_STATUS, h, 1)?;
    Ok(s.entries.iter().take((s.count as usize).min(4)).copied().collect())
}

pub fn thermal_policies_info(h: GpuHandle) -> NvResult<Vec<ThermalPolicyInfoEntry>> {
    let s = get_versioned::<ThermalPoliciesInfoV2>(id::GPU_GET_THERMAL_POLICIES_INFO, h, 2)?;
    Ok(s.entries.iter().take((s.count as usize).min(4)).copied().collect())
}

pub fn thermal_policies_status(h: GpuHandle) -> NvResult<Vec<ThermalPolicyStatusEntry>> {
    let s = get_versioned::<ThermalPoliciesStatusV2>(id::GPU_GET_THERMAL_POLICIES_STATUS, h, 2)?;
    Ok(s.entries.iter().take((s.count as usize).min(4)).copied().collect())
}

pub fn set_thermal_policies_status(h: GpuHandle, entries: &[ThermalPolicyStatusEntry]) -> NvResult<()> {
    if entries.is_empty() || entries.len() > 4 {
        return Err(NvError::NotSupported("Array is null or empty."));
    }
    let mut s = new_versioned::<ThermalPoliciesStatusV2>(2);
    s.count = entries.len() as u32;
    s.entries[..entries.len()].copy_from_slice(entries);
    // SAFETY: V2 struct.
    unsafe { call_struct(id::GPU_SET_THERMAL_POLICIES_STATUS, h, s.as_mut()) }
}

/// GPUPerformanceControl.CurrentActiveLimit (PerformanceLimit flags).
pub fn performance_limit(h: GpuHandle) -> NvResult<u32> {
    Ok(get_versioned::<PerformanceStatusV1>(id::GPU_PERF_POLICIES_GET_STATUS, h, 1)?.performance_limit)
}

pub fn current_voltage_uv(h: GpuHandle) -> NvResult<u32> {
    Ok(get_versioned::<VoltageStatusV1>(id::GPU_GET_CURRENT_VOLTAGE, h, 1)?.value_uv)
}

pub fn core_voltage_boost_percent(h: GpuHandle) -> NvResult<u32> {
    Ok(get_versioned::<VoltageBoostPercentV1>(id::GPU_GET_CORE_VOLTAGE_BOOST_PERCENT, h, 1)?.percent)
}

pub fn set_core_voltage_boost_percent(h: GpuHandle, percent: u32) -> NvResult<()> {
    let mut s = new_versioned::<VoltageBoostPercentV1>(1);
    s.percent = percent;
    // SAFETY: V1 struct.
    unsafe { call_struct(id::GPU_SET_CORE_VOLTAGE_BOOST_PERCENT, h, s.as_mut()) }
}

/// GetClockBoostMask's own `_Masks` field.
pub fn clock_boost_mask(h: GpuHandle) -> NvResult<[u32; 4]> {
    Ok(get_versioned::<ClockBoostMasksV1>(id::GPU_GET_CLOCK_BOOST_MASK, h, 1)?.masks)
}

/// GetClockBoostTable with the wrapper's zero mask: the 80 GPU deltas.
pub fn clock_boost_table_gpu_deltas(h: GpuHandle) -> NvResult<Vec<i32>> {
    let s = get_versioned::<ClockBoostTableV1>(id::GPU_GET_CLOCK_BOOST_TABLE, h, 1)?;
    Ok(s.gpu_deltas.iter().map(|d| d.frequency_delta_khz).collect())
}

/// GetVFPCurve with the wrapper's zero mask: both entry regions, GPU then memory.
pub fn vfp_curve_entries(h: GpuHandle) -> NvResult<Vec<VfpCurveEntry>> {
    let s = get_versioned::<VfpCurveV1>(id::GPU_GET_VFP_CURVE, h, 1)?;
    Ok(s.gpu_entries.iter().chain(s.memory_entries.iter()).copied().collect())
}

pub fn clock_boost_ranges(h: GpuHandle) -> NvResult<Vec<ClockBoostRange>> {
    let s = get_versioned::<ClockBoostRangesV1>(id::GPU_GET_CLOCK_BOOST_RANGES, h, 1)?;
    Ok(s.ranges.iter().take((s.count as usize).min(MAX_CLOCKS_PER_GPU)).copied().collect())
}

pub fn clock_boost_locks(h: GpuHandle) -> NvResult<Vec<ClockBoostLock>> {
    let s = get_versioned::<ClockBoostLockV2>(id::GPU_GET_CLOCK_BOOST_LOCK, h, 2)?;
    Ok(s.locks.iter().take((s.count as usize).min(MAX_CLOCKS_PER_GPU)).copied().collect())
}

pub fn set_clock_boost_locks(h: GpuHandle, locks: &[ClockBoostLock]) -> NvResult<()> {
    if locks.is_empty() || locks.len() > MAX_CLOCKS_PER_GPU {
        return Err(NvError::NotSupported("Array is null or empty."));
    }
    let mut s = new_versioned::<ClockBoostLockV2>(2);
    s.count = locks.len() as u32;
    s.locks[..locks.len()].copy_from_slice(locks);
    // SAFETY: V2 struct.
    unsafe { call_struct(id::GPU_SET_CLOCK_BOOST_LOCK, h, s.as_mut()) }
}

pub fn enable_overclocked_pstates(h: GpuHandle) -> NvResult<()> {
    // SAFETY: NvAPI_GPU_EnableOverclockedPstates(handle).
    let f: HandleFn = unsafe { resolve(id::GPU_ENABLE_OVERCLOCKED_PSTATES)? };
    // SAFETY: as above.
    check(unsafe { f(h.0) })
}

/// GetDynamicPstatesInfoEx (V1 only).
pub fn dynamic_pstates(h: GpuHandle) -> NvResult<Box<DynamicPStatesInfoV1>> {
    get_versioned::<DynamicPStatesInfoV1>(id::GPU_GET_DYNAMIC_PSTATES_INFO_EX, h, 1)
}

pub fn usages(h: GpuHandle) -> NvResult<Box<UsagesInfoV1>> {
    get_versioned::<UsagesInfoV1>(id::GPU_GET_USAGES, h, 1)
}

/// GetCoolerSettings(All): the first `count` settings.
pub fn cooler_settings(h: GpuHandle) -> NvResult<Vec<CoolerSetting>> {
    type F = unsafe extern "C" fn(*mut c_void, u32, *mut CoolerSettingsV1) -> Status;
    // SAFETY: NvAPI_GPU_GetCoolerSettings(handle, NvU32 coolerIndex, NV_GPU_COOLER_SETTINGS*).
    let f: F = unsafe { resolve(id::GPU_GET_COOLER_SETTINGS)? };
    let mut s = new_versioned::<CoolerSettingsV1>(1);
    // SAFETY: V1 struct; 7 = CoolerTarget.All.
    check(unsafe { f(h.0, 7, s.as_mut()) })?;
    Ok(s.settings.iter().take((s.count as usize).min(3)).copied().collect())
}

pub fn fan_coolers_info(h: GpuHandle) -> NvResult<Vec<FanCoolerInfoEntry>> {
    let s = get_versioned::<FanCoolersInfoV1>(id::GPU_CLIENT_FAN_COOLERS_GET_INFO, h, 1)?;
    Ok(s.entries.iter().take((s.count as usize).min(32)).copied().collect())
}

pub fn fan_coolers_status(h: GpuHandle) -> NvResult<Vec<FanCoolerStatusEntry>> {
    let s = get_versioned::<FanCoolersStatusV1>(id::GPU_CLIENT_FAN_COOLERS_GET_STATUS, h, 1)?;
    Ok(s.entries.iter().take((s.count as usize).min(32)).copied().collect())
}

/// `(entries, UnknownUInt)` of GetClientFanCoolersControl.
pub fn fan_coolers_control(h: GpuHandle) -> NvResult<(Vec<FanCoolerControlEntry>, u32)> {
    let s = get_versioned::<FanCoolersControlV1>(id::GPU_CLIENT_FAN_COOLERS_GET_CONTROL, h, 1)?;
    Ok((s.entries.iter().take((s.count as usize).min(32)).copied().collect(), s.unknown_uint))
}

/// SetClientFanCoolersControl; entries are sorted by cooler id first, as the wrapper's ctor did.
pub fn set_fan_coolers_control(h: GpuHandle, entries: &[FanCoolerControlEntry], unknown: u32) -> NvResult<()> {
    if entries.is_empty() || entries.len() > 32 {
        return Err(NvError::NotSupported("Array is null or empty."));
    }
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|e| e.cooler_id); // stable, as OrderBy
    let mut s = new_versioned::<FanCoolersControlV1>(1);
    s.unknown_uint = unknown;
    s.count = sorted.len() as u32;
    s.entries[..sorted.len()].copy_from_slice(&sorted);
    // SAFETY: V1 struct.
    unsafe { call_struct(id::GPU_CLIENT_FAN_COOLERS_SET_CONTROL, h, s.as_mut()) }
}

/// SetCoolerLevels(index, one level, count 1).
pub fn set_cooler_level(h: GpuHandle, cooler: u32, policy: u32, level: u32) -> NvResult<()> {
    type F = unsafe extern "C" fn(*mut c_void, u32, *mut CoolerLevelsV1, u32) -> Status;
    // SAFETY: NvAPI_GPU_SetCoolerLevels(handle, NvU32 index, NV_GPU_COOLER_LEVELS*, NvU32 count).
    let f: F = unsafe { resolve(id::GPU_SET_COOLER_LEVELS)? };
    let mut s = new_versioned::<CoolerLevelsV1>(1);
    s.levels[0] = CoolerLevel { level, policy };
    // SAFETY: V1 struct with one populated level.
    check(unsafe { f(h.0, cooler, s.as_mut(), 1) })
}

pub fn restore_cooler_settings(h: GpuHandle, ids: &[u32]) -> NvResult<()> {
    type F = unsafe extern "C" fn(*mut c_void, *const u32, u32) -> Status;
    // SAFETY: NvAPI_GPU_RestoreCoolerSettings(handle, NvU32* indexes, NvU32 count).
    let f: F = unsafe { resolve(id::GPU_RESTORE_COOLER_SETTINGS)? };
    let ptr = if ids.is_empty() { std::ptr::null() } else { ids.as_ptr() };
    // SAFETY: `ids` outlives the call.
    check(unsafe { f(h.0, ptr, ids.len() as u32) })
}

// ------------------------------------------------------------------ enum names (for messages)

/// PerformanceStateId.ToString().
pub fn pstate_name(id: u32) -> String {
    let n = match id {
        0 => "P0_3DPerformance",
        1 => "P1_3DPerformance",
        2 => "P2_Balanced",
        3 => "P3_Balanced",
        4 => "P4",
        5 => "P5",
        6 => "P6",
        7 => "P7",
        8 => "P8_HDVideoPlayback",
        9 => "P9",
        10 => "P10_DVDPlayback",
        11 => "P11",
        12 => "P12_Idle",
        13 => "P13",
        14 => "P14",
        15 => "P15",
        16 => "Undefined",
        17 => "All",
        _ => return id.to_string(),
    };
    n.to_string()
}

/// PublicClockDomain.ToString().
pub fn clock_domain_name(id: u32) -> String {
    match id {
        0 => "Graphics".into(),
        4 => "Memory".into(),
        7 => "Processor".into(),
        8 => "Video".into(),
        32 => "Undefined".into(),
        _ => (id as i32).to_string(),
    }
}

/// Whether a PublicClockDomain value is a named member (`Enum.IsDefined`).
pub fn clock_domain_defined(id: u32) -> bool {
    matches!(id, 0 | 4 | 7 | 8 | 32)
}

/// ClockLockMode.ToString().
pub fn lock_mode_name(m: u32) -> String {
    match m {
        0 => "None".into(),
        3 => "Manual".into(),
        _ => m.to_string(),
    }
}

/// ThermalController.ToString().
pub fn thermal_controller_name(c: i32) -> String {
    let n = match c {
        0 => "None",
        1 => "GPU",
        2 => "ADM1032",
        3 => "MAX6649",
        4 => "MAX1617",
        5 => "LM99",
        6 => "LM89",
        7 => "LM64",
        8 => "ADT7473",
        9 => "SBMAX6649",
        10 => "VideoBiosEvent",
        11 => "OperatingSystem",
        -1 => "Unknown",
        _ => return c.to_string(),
    };
    n.to_string()
}

/// .NET `[Flags]` enum formatting: an exact name, else the set bits' names joined in ascending
/// order, else the number.
pub fn flags_name(value: u64, names: &[(u64, &str)], signed_text: impl FnOnce() -> String) -> String {
    if let Some((_, n)) = names.iter().find(|(v, _)| *v == value) {
        return (*n).to_string();
    }
    if value == 0 {
        return "0".into();
    }
    let mut sorted: Vec<(u64, &str)> = names.iter().copied().filter(|(v, _)| *v != 0).collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut rest = value;
    let mut picked = Vec::new();
    for (v, n) in sorted {
        if rest & v == v && v & value == v {
            rest &= !v;
            picked.push(n);
        }
    }
    if rest != 0 {
        return signed_text();
    }
    picked.reverse();
    picked.join(", ")
}

/// ThermalSettingsTarget.ToString().
pub fn thermal_target_name(t: i32) -> String {
    if t == -1 {
        return "Unknown".into();
    }
    flags_name(
        t as u32 as u64,
        &[
            (0, "None"),
            (1, "GPU"),
            (2, "Memory"),
            (4, "PowerSupply"),
            (8, "Board"),
            (9, "VisualComputingBoard"),
            (10, "VisualComputingInlet"),
            (11, "VisualComputingOutlet"),
            (15, "All"),
        ],
        || t.to_string(),
    )
}

/// CoolerPolicy.ToString().
pub fn cooler_policy_name(p: u32) -> String {
    flags_name(
        p as u64,
        &[
            (0, "None"),
            (1, "Manual"),
            (2, "Performance"),
            (4, "TemperatureDiscrete"),
            (8, "TemperatureContinuous"),
            (16, "Silent"),
        ],
        || p.to_string(),
    )
}

/// PerformanceLimit.ToString().
pub fn performance_limit_name(l: u32) -> String {
    flags_name(
        l as u64,
        &[
            (0, "None"),
            (1, "PowerLimit"),
            (2, "TemperatureLimit"),
            (4, "VoltageLimit"),
            (8, "Unknown8"),
            (16, "NoLoadLimit"),
        ],
        || l.to_string(),
    )
}

/// GPUMemoryType.ToString() for the named members.
pub fn memory_type_name(code: u32) -> String {
    let n = match code {
        0 => "Unknown",
        1 => "SDRAM",
        2 => "DDR1",
        3 => "DDR2",
        4 => "GDDR2",
        5 => "GDDR3",
        6 => "GDDR4",
        7 => "DDR3",
        8 => "GDDR5",
        9 => "LPDDR2",
        10 => "GDDR5X",
        _ => return code.to_string(),
    };
    n.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    #[test]
    fn version_words() {
        assert_eq!(version_word(size_of::<PStates20InfoV1>(), 1), 0x0001_1C94);
        assert_eq!(version_word(size_of::<PStates20InfoV2>(), 3), 0x0003_1CF8);
        assert_eq!(version_word(size_of::<ClockBoostLockV2>(), 2), 0x0002_030C);
        assert_eq!(version_word(size_of::<PowerPoliciesStatusV1>(), 1), 0x0001_0048);
        assert_eq!(version_word(size_of::<PowerPoliciesInfoV1>(), 1), 0x0001_00B8);
        let b = new_versioned::<ThermalSensorsV2>(2);
        assert_eq!(b.version, 168 | (2 << 16));
    }

    #[test]
    fn layout_offsets() {
        // The byte fields of the info structs are followed by entries aligned to 4.
        assert_eq!(offset_of!(PowerPoliciesInfoV1, entries), 8);
        assert_eq!(offset_of!(ThermalPoliciesInfoV2, entries), 8);
        // Header of the curve and boost table: version + 4 masks + 12 unknowns = 68 bytes.
        assert_eq!(offset_of!(VfpCurveV1, gpu_entries), 68);
        assert_eq!(offset_of!(ClockBoostTableV1, gpu_deltas), 68);
        assert_eq!(offset_of!(ClockBoostTableV1, memory_filled), 68 + 80 * 36);
        assert_eq!(offset_of!(ClockBoostDelta, frequency_delta_khz), 20);
        assert_eq!(offset_of!(VfpCurveEntry, frequency_khz), 4);
        assert_eq!(offset_of!(VfpCurveEntry, voltage_uv), 8);
        assert_eq!(offset_of!(PerformanceStatusV1, performance_limit), 16);
        assert_eq!(offset_of!(PerformanceStatusV1, timers_ns), 32);
        assert_eq!(offset_of!(PStates20InfoV2, over_volting), 7316);
        assert_eq!(offset_of!(ClockBoostRange, range_max_khz), 40);
        assert_eq!(offset_of!(VoltageStatusV1, value_uv), 40);
        assert_eq!(offset_of!(FanCoolersControlV1, entries), 44);
        assert_eq!(offset_of!(FanCoolersStatusV1, entries), 40);
        assert_eq!(offset_of!(FanCoolersInfoV1, entries), 44);
    }

    #[test]
    fn enum_text() {
        assert_eq!(pstate_name(0).split('_').next(), Some("P0"));
        assert_eq!(pstate_name(8).split('_').next(), Some("P8"));
        assert_eq!(performance_limit_name(0), "None");
        assert_eq!(performance_limit_name(3).replace("Limit", ""), "Power, Temperature");
        assert_eq!(performance_limit_name(16).replace("Limit", ""), "NoLoad");
        assert_eq!(performance_limit_name(32), "32");
        assert_eq!(thermal_target_name(1), "GPU");
        assert_eq!(thermal_target_name(-1), "Unknown");
        assert_eq!(cooler_policy_name(1), "Manual");
        assert_eq!(status_name(-104), "NotSupported");
        assert_eq!(status_name(-9), "IncompatibleStructureVersion");
        assert_eq!(status_name(-999), "-999");
        assert_eq!(memory_type_name(8), "GDDR5");
    }
}
