//! Windows x64 binding to Intel's Graphics Control Library (IGCL), from the public `igcl_api.h`.
//!
//! Like the original, structures are exchanged as sized byte buffers with fixed offsets (the x64
//! layouts, including nested `Size`/`Version` headers) rather than declared structs: the driver
//! validates the leading `Size` field, and the offsets below are the ones the C# was verified
//! against on an Arc Pro B60. Only `ControlLib.dll` from System32 is ever loaded — never a DLL
//! beside the elevated executable.
//!
//! Every IGCL entry point is `CTL_APICALL` = `__cdecl`, which on x64 is the one C convention.

use std::ffi::c_void;
use std::ptr;

use roch_core::GpuError;
use roch_core::native::{self, Library};

/// Why an IGCL call failed. The C# distinguished a driver failure (`GpuBackendException`, which
/// many call sites swallow to keep going) from a missing export (`EntryPointNotFoundException`,
/// which those same sites let propagate). Keeping the two apart preserves that control flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fault {
    /// A failed status, a malformed value, a guard or a readback mismatch.
    Backend(String),
    /// The installed `ControlLib.dll` lacks the entry point.
    MissingExport(String),
}

impl Fault {
    pub(crate) fn message(&self) -> &str {
        match self {
            Fault::Backend(m) | Fault::MissingExport(m) => m,
        }
    }
}

impl From<Fault> for GpuError {
    fn from(f: Fault) -> Self {
        match f {
            Fault::Backend(m) | Fault::MissingExport(m) => GpuError::new(m),
        }
    }
}

impl From<GpuError> for Fault {
    fn from(e: GpuError) -> Self {
        Fault::Backend(e.message)
    }
}

pub(crate) type R<T> = std::result::Result<T, Fault>;

/// `throw new GpuBackendException(..)`.
macro_rules! fail {
    ($($arg:tt)*) => { return Err($crate::igcl::Fault::Backend(format!($($arg)*))) };
}
pub(crate) use fail;

/// `catch (GpuBackendException) { }`: a driver failure becomes `None`; a missing export still
/// propagates, as `EntryPointNotFoundException` did.
pub(crate) fn swallow<T>(r: R<T>) -> R<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(Fault::Backend(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// An opaque IGCL handle (`ctl_*_handle_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CtlHandle(pub(crate) *mut c_void);

// SAFETY: IGCL handles are opaque driver tokens, not thread-affine; every call through them is
// serialised by the backend's `&mut self` (the tuning service holds one lock around the backend).
unsafe impl Send for CtlHandle {}

impl CtlHandle {
    pub(crate) const NULL: CtlHandle = CtlHandle(ptr::null_mut());

    pub(crate) fn is_null(self) -> bool {
        self.0.is_null()
    }
}

// ---------------------------------------------------------------------------------------------
// Entry-point signatures (all `__cdecl`).

/// `ctlInit(ctl_init_args_t*, ctl_api_handle_t*)`.
pub(crate) type InitFn = unsafe extern "C" fn(args: *mut u8, handle: *mut *mut c_void) -> u32;
/// Any `ctlX(handle)`: `ctlClose`, `ctlOverclockWaiverSet`, `ctlFanSetDefaultMode`, …
pub(crate) type HandleFn = unsafe extern "C" fn(handle: *mut c_void) -> u32;
/// Any `ctlX(handle, struct*)` taking a sized in/out structure.
pub(crate) type BufferFn = unsafe extern "C" fn(handle: *mut c_void, data: *mut u8) -> u32;
/// `ctlEnumX(parent, uint32_t* count, handle* handles)`.
pub(crate) type EnumerateFn =
    unsafe extern "C" fn(parent: *mut c_void, count: *mut u32, handles: *mut *mut c_void) -> u32;
/// `ctlOverclockXGetV2(handle, double*)`, `ctlTemperatureGetState(handle, double*)`.
pub(crate) type GetDoubleFn = unsafe extern "C" fn(handle: *mut c_void, value: *mut f64) -> u32;
/// `ctlOverclockXSetV2(handle, double)`.
pub(crate) type SetDoubleFn = unsafe extern "C" fn(handle: *mut c_void, value: f64) -> u32;
/// `ctlOverclockReadVFCurve(handle, ctl_vf_curve_type_t, ctl_vf_curve_details_t, uint32_t*,
/// ctl_voltage_frequency_point_t*)`.
pub(crate) type ReadCurveFn =
    unsafe extern "C" fn(handle: *mut c_void, kind: i32, detail: i32, count: *mut u32, points: *mut Point) -> u32;
/// `ctlOverclockWriteCustomVFCurve(handle, uint32_t, ctl_voltage_frequency_point_t*)`.
pub(crate) type WriteCurveFn = unsafe extern "C" fn(handle: *mut c_void, count: u32, points: *mut Point) -> u32;

/// `ctl_voltage_frequency_point_t`: mV, MHz.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    pub voltage: u32,
    pub frequency: u32,
}
const _: () = assert!(size_of::<Point>() == 8);

/// `ctl_vf_curve_type_t`.
pub(crate) const VF_CURVE_STOCK: i32 = 0;
pub(crate) const VF_CURVE_LIVE: i32 = 1;
/// `ctl_vf_curve_details_t::SIMPLIFIED`.
pub(crate) const VF_CURVE_SIMPLIFIED: i32 = 0;

// ---------------------------------------------------------------------------------------------
// Structure sizes (x64), as the C# passed them. Where the size follows from the layout the
// arithmetic is asserted so a typo cannot slip through.

/// `ctl_init_args_t`: Size, Version(+pad), AppVersion@8, flags@12, SupportedVersion@16,
/// ApplicationUID[16]@20.
pub(crate) const INIT_ARGS_SIZE: usize = 36;
const _: () = assert!(INIT_ARGS_SIZE == 20 + 16);
/// `CTL_MAKE_VERSION(1, 1)`.
pub(crate) const INIT_APP_VERSION: i32 = 0x10001;
/// `CTL_INIT_FLAG_USE_LEVEL_ZERO | CTL_INIT_FLAG_IGSC_FUL`: permits read-only firmware identity.
pub(crate) const INIT_FLAGS_WITH_FIRMWARE: i32 = 3;
/// `CTL_INIT_FLAG_USE_LEVEL_ZERO` only, for runtimes without firmware discovery.
pub(crate) const INIT_FLAGS_BASIC: i32 = 1;

/// `ctl_device_adapter_properties_t` version 3.
pub(crate) const DEVICE_PROPERTIES_SIZE: usize = 320;
pub(crate) const DEVICE_PROPERTIES_VERSION: u8 = 3;
/// `ctl_oc_properties_t` v1: header(8) + nine 48-byte `ctl_oc_control_info_t`.
pub(crate) const OC_PROPERTIES_SIZE: usize = 440;
pub(crate) const OC_PROPERTIES_VERSION: u8 = 1;
pub(crate) const OC_CONTROL_COUNT: usize = 9;
pub(crate) const OC_CONTROL_STRIDE: usize = 48;
const _: () = assert!(OC_PROPERTIES_SIZE == 8 + OC_CONTROL_COUNT * OC_CONTROL_STRIDE);
/// `ctl_temp_properties_t`.
pub(crate) const TEMP_PROPERTIES_SIZE: usize = 24;
/// `ctl_mem_properties_t`.
pub(crate) const MEMORY_PROPERTIES_SIZE: usize = 32;
/// `ctl_mem_state_t`.
pub(crate) const MEMORY_STATE_SIZE: usize = 24;
/// `ctl_fan_properties_t`.
pub(crate) const FAN_PROPERTIES_SIZE: usize = 24;
/// `ctl_fan_speed_t`: Size, Version, speed@8, units@12.
pub(crate) const FAN_SPEED_SIZE: usize = 16;
/// `ctl_fan_temp_speed_t`: Size, Version, temperature@8, `ctl_fan_speed_t`@12.
pub(crate) const FAN_POINT_SIZE: usize = 28;
const _: () = assert!(FAN_POINT_SIZE == 12 + FAN_SPEED_SIZE);
pub(crate) const FAN_MAX_POINTS: usize = 32;
/// `ctl_fan_speed_table_t`: Size, Version, numPoints@8, table[32]@12.
pub(crate) const FAN_TABLE_SIZE: usize = 908;
const _: () = assert!(FAN_TABLE_SIZE == 12 + FAN_MAX_POINTS * FAN_POINT_SIZE);
/// `ctl_fan_config_t`: Size, Version, mode@8, fixed `ctl_fan_speed_t`@12, table@28.
pub(crate) const FAN_CONFIG_SIZE: usize = 936;
const _: () = assert!(FAN_CONFIG_SIZE == 12 + FAN_SPEED_SIZE + FAN_TABLE_SIZE);
/// `ctl_freq_properties_t`: Size, Version, type@8, canControl@12, min@16, max@24.
pub(crate) const FREQ_PROPERTIES_SIZE: usize = 32;
/// `ctl_freq_range_t`: Size, Version, min@8, max@16.
pub(crate) const FREQ_RANGE_SIZE: usize = 24;
/// `ctl_firmware_component_properties_t`: Size, Version, name[64]@5, version[64]@69, …
pub(crate) const FIRMWARE_PROPERTIES_SIZE: usize = 156;
/// `ctl_pci_address_t` / `ctl_pci_speed_t`: both 24 bytes, nested as headers.
pub(crate) const PCI_NESTED_SIZE: i32 = 24;
/// `ctl_pci_properties_t`: Size, Version, address@8, maxSpeed@32, rebarSupported@56,
/// rebarEnabled@57.
pub(crate) const PCI_PROPERTIES_SIZE: usize = 64;
const _: () = assert!(PCI_PROPERTIES_SIZE == 8 + 24 + 24 + 8);
/// `ctl_pci_state_t`: Size, Version, speed@8.
pub(crate) const PCI_STATE_SIZE: usize = 32;
const _: () = assert!(PCI_STATE_SIZE == 8 + 24);
/// `ctl_power_telemetry2_t` version 1.
pub(crate) const POWER_TELEMETRY_SIZE: usize = 1016;
pub(crate) const POWER_TELEMETRY_VERSION: u8 = 1;
/// `ctl_oc_telemetry_item_t`: bSupported@0, units@4, type@8, value@16.
pub(crate) const TELEMETRY_ITEM_SIZE: usize = 24;

// ---------------------------------------------------------------------------------------------
// Sized, 8-byte-aligned byte buffers.

/// A zeroed IGCL structure buffer. Backed by `u64`s so doubles and pointers inside are naturally
/// aligned, as the pinned .NET `byte[]` was.
#[derive(Clone)]
pub struct Buf {
    words: Vec<u64>,
    len: usize,
}

impl Buf {
    /// `Igcl.Data(size, version)`: zeroed, `Size` at 0 and `Version` at 4.
    pub fn data(size: usize, version: u8) -> Buf {
        let mut b = Buf::zeroed(size);
        b.put(0, size as i32);
        b.put_u8(4, version);
        b
    }

    /// All zeroes, no header (like `new byte[n]`).
    pub fn zeroed(size: usize) -> Buf {
        Buf { words: vec![0u64; size.div_ceil(8)], len: size }
    }

    pub fn bytes(&self) -> &[u8] {
        // SAFETY: `words` holds at least `len` initialised bytes; u8 has no alignment needs.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast::<u8>(), self.len) }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, uniquely borrowed.
        unsafe { std::slice::from_raw_parts_mut(self.words.as_mut_ptr().cast::<u8>(), self.len) }
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut u8 {
        self.words.as_mut_ptr().cast::<u8>()
    }

    /// `Igcl.Put`: little-endian i32 at `at`. Out-of-range writes are ignored (never panics).
    pub fn put(&mut self, at: usize, value: i32) {
        put_bytes(self.bytes_mut(), at, &value.to_le_bytes());
    }

    pub fn put_u8(&mut self, at: usize, value: u8) {
        if let Some(b) = self.bytes_mut().get_mut(at) {
            *b = value;
        }
    }

    #[cfg(test)]
    pub fn put_u16(&mut self, at: usize, value: u16) {
        put_bytes(self.bytes_mut(), at, &value.to_le_bytes());
    }

    pub fn put_u64(&mut self, at: usize, value: u64) {
        put_bytes(self.bytes_mut(), at, &value.to_le_bytes());
    }

    pub fn put_f64(&mut self, at: usize, value: f64) {
        put_bytes(self.bytes_mut(), at, &value.to_le_bytes());
    }

    /// Copy raw bytes (test fixtures: strings into char arrays).
    #[cfg(test)]
    pub fn put_slice(&mut self, at: usize, data: &[u8]) {
        put_bytes(self.bytes_mut(), at, data);
    }

    pub fn int(&self, at: usize) -> i32 {
        int(self.bytes(), at)
    }

    pub fn byte(&self, at: usize) -> u8 {
        self.bytes().get(at).copied().unwrap_or(0)
    }

    pub fn f64(&self, at: usize) -> f64 {
        f64_at(self.bytes(), at)
    }

    pub fn u64(&self, at: usize) -> u64 {
        u64_at(self.bytes(), at)
    }
}

fn put_bytes(b: &mut [u8], at: usize, data: &[u8]) {
    if let Some(dst) = at.checked_add(data.len()).and_then(|end| b.get_mut(at..end)) {
        dst.copy_from_slice(data);
    }
}

fn arr<const N: usize>(b: &[u8], at: usize) -> Option<[u8; N]> {
    b.get(at..at.checked_add(N)?)?.try_into().ok()
}

/// `Igcl.Int`: little-endian i32; 0 past the end (the fixed buffers never get there).
pub fn int(b: &[u8], at: usize) -> i32 {
    arr::<4>(b, at).map(i32::from_le_bytes).unwrap_or(0)
}

pub fn u32_at(b: &[u8], at: usize) -> u32 {
    arr::<4>(b, at).map(u32::from_le_bytes).unwrap_or(0)
}

pub fn u16_at(b: &[u8], at: usize) -> u16 {
    arr::<2>(b, at).map(u16::from_le_bytes).unwrap_or(0)
}

pub fn u64_at(b: &[u8], at: usize) -> u64 {
    arr::<8>(b, at).map(u64::from_le_bytes).unwrap_or(0)
}

/// Past the end reads as NaN, which every caller already treats as "unavailable".
pub fn f64_at(b: &[u8], at: usize) -> f64 {
    arr::<8>(b, at).map(f64::from_le_bytes).unwrap_or(f64::NAN)
}

/// `Igcl.Sensor`: decode a `ctl_oc_telemetry_item_t` at `at`. Unsupported, truncated, unknown
/// data types and non-finite values are all NaN.
pub fn sensor(b: &[u8], at: usize) -> f64 {
    if at.checked_add(TELEMETRY_ITEM_SIZE).is_none_or(|end| end > b.len()) || b[at] == 0 {
        return f64::NAN;
    }
    let v = at + 16;
    let value = match int(b, at + 8) {
        0 => b[v] as i8 as f64,
        1 => b[v] as f64,
        2 => arr::<2>(b, v).map(i16::from_le_bytes).unwrap_or(0) as f64,
        3 => u16_at(b, v) as f64,
        4 => int(b, v) as f64,
        5 => u32_at(b, v) as f64,
        6 => arr::<8>(b, v).map(i64::from_le_bytes).unwrap_or(0) as f64,
        7 => u64_at(b, v) as f64,
        8 => arr::<4>(b, v).map(f32::from_le_bytes).unwrap_or(f32::NAN) as f64,
        9 => f64_at(b, v),
        _ => f64::NAN,
    };
    if value.is_finite() { value } else { f64::NAN }
}

/// `Igcl.Check`: a non-zero `ctl_result_t` is a failure.
pub(crate) fn check(result: u32, operation: &str) -> R<()> {
    if result != 0 {
        fail!("Intel {operation} failed (IGCL 0x{result:08X}).");
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The loaded library.

/// The installed `ControlLib.dll`. Every function pointer resolved from it is used only while
/// this value is alive.
pub(crate) struct Igcl {
    lib: Library,
}

impl Igcl {
    /// Load `%SystemRoot%\System32\ControlLib.dll`. The offsets in this crate are the x64 ABI, so a
    /// 32-bit or non-Windows build refuses outright.
    pub(crate) fn load() -> roch_core::Result<Igcl> {
        if !cfg!(windows) || !cfg!(target_pointer_width = "64") {
            return Err(GpuError::new("Intel IGCL requires 64-bit Windows."));
        }
        Ok(Igcl { lib: native::load_system32("ControlLib.dll")? })
    }

    /// Resolve an export; absence is [`Fault::MissingExport`].
    ///
    /// # Safety
    /// `F` must match the export's real signature.
    pub(crate) unsafe fn function<F: Copy>(&self, name: &str) -> R<F> {
        unsafe { native::symbol::<F>(&self.lib, name) }.map_err(|e| Fault::MissingExport(e.message))
    }

    /// Call a `ctlX(handle, struct*)` export on `buf` and return its raw status.
    pub(crate) fn call_buffer(&self, name: &str, handle: CtlHandle, buf: &mut Buf) -> R<u32> {
        // SAFETY: BufferFn is the signature of every sized-structure IGCL call; `buf` is a live,
        // writable allocation whose `Size` header bounds what the driver touches.
        let f = unsafe { self.function::<BufferFn>(name)? };
        Ok(unsafe { f(handle.0, buf.as_mut_ptr()) })
    }

    /// `Read(h, function, size, version)`: a fresh sized buffer, filled by the driver.
    pub(crate) fn read(&self, handle: CtlHandle, name: &str, size: usize, version: u8) -> R<Buf> {
        let mut b = Buf::data(size, version);
        check(self.call_buffer(name, handle, &mut b)?, name)?;
        Ok(b)
    }

    /// Call a `ctlX(handle)` export and return its raw status.
    pub(crate) fn call_handle(&self, name: &str, handle: CtlHandle) -> R<u32> {
        // SAFETY: HandleFn matches every single-handle IGCL call used here.
        let f = unsafe { self.function::<HandleFn>(name)? };
        Ok(unsafe { f(handle.0) })
    }

    /// `ctlX(handle, double*)`, status-checked with `operation` in the message.
    pub(crate) fn get_double(&self, name: &str, handle: CtlHandle, operation: &str) -> R<f64> {
        // SAFETY: GetDoubleFn matches the V2 overclock getters and ctlTemperatureGetState.
        let f = unsafe { self.function::<GetDoubleFn>(name)? };
        let mut value = 0.0f64;
        check(unsafe { f(handle.0, &mut value) }, operation)?;
        Ok(value)
    }

    /// `ctlX(handle, double)`, status-checked.
    pub(crate) fn set_double(&self, name: &str, handle: CtlHandle, value: f64, operation: &str) -> R<()> {
        // SAFETY: SetDoubleFn matches the V2 overclock setters.
        let f = unsafe { self.function::<SetDoubleFn>(name)? };
        check(unsafe { f(handle.0, value) }, operation)
    }

    /// `Igcl.Handles`: the two-call enumeration pattern, bounded at 128 handles.
    pub(crate) fn handles(&self, name: &str, parent: CtlHandle) -> R<Vec<CtlHandle>> {
        // SAFETY: EnumerateFn matches every ctlEnum* export.
        let f = unsafe { self.function::<EnumerateFn>(name)? };
        let mut count = 0u32;
        check(unsafe { f(parent.0, &mut count, ptr::null_mut()) }, name)?;
        if count > 128 {
            fail!("Intel returned an invalid handle count.");
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut handles = vec![ptr::null_mut::<c_void>(); count as usize];
        // SAFETY: `handles` has room for `count` entries, which is what we tell the driver.
        check(unsafe { f(parent.0, &mut count, handles.as_mut_ptr()) }, name)?;
        if count as usize > handles.len() {
            fail!("Intel handle count changed; reopen the app.");
        }
        handles.truncate(count as usize);
        Ok(handles.into_iter().map(CtlHandle).collect())
    }

    /// `ctlOverclockReadVFCurve` → (status of first call, points). See `IntelBackend::curve`.
    pub(crate) fn read_curve_fn(&self) -> R<ReadCurveFn> {
        // SAFETY: signature from igcl_api.h.
        unsafe { self.function::<ReadCurveFn>("ctlOverclockReadVFCurve") }
    }

    pub(crate) fn write_curve_fn(&self) -> R<WriteCurveFn> {
        // SAFETY: signature from igcl_api.h.
        unsafe { self.function::<WriteCurveFn>("ctlOverclockWriteCustomVFCurve") }
    }

    pub(crate) fn init_fn(&self) -> R<InitFn> {
        // SAFETY: signature from igcl_api.h.
        unsafe { self.function::<InitFn>("ctlInit") }
    }
}
