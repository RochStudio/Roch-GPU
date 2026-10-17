//! Read-only ADLX performance metrics, loaded from the installed AMD driver (amdadlx64.dll).
//!
//! ADLX is a C++ interface library with C-compatible vtables: every object pointer points at a
//! vtable pointer, and methods are called by slot index. IADLXInterface puts Acquire at slot 0,
//! Release at slot 1 and QueryInterface (by wide-string interface name) at slot 2.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::null_mut;

use roch_core::native::{self, Library};

const DLL: &str = "amdadlx64.dll";

type QueryFullVersion = unsafe extern "C" fn(version: *mut u64) -> i32;
type InitializeWithCallerAdl = unsafe extern "C" fn(
    version: u64,
    system: *mut *mut c_void,
    mapping: *mut *mut c_void,
    context: *mut c_void,
    free: FreeMemory,
) -> i32;
type Terminate = unsafe extern "C" fn() -> i32;
type FreeMemory = unsafe extern "system" fn(buffer: *mut *mut c_void);

type GetObject = unsafe extern "system" fn(this: *mut c_void, value: *mut *mut c_void) -> i32;
type MapGpu = unsafe extern "system" fn(this: *mut c_void, adapter: i32, value: *mut *mut c_void) -> i32;
type GpuObject = unsafe extern "system" fn(this: *mut c_void, gpu: *mut c_void, value: *mut *mut c_void) -> i32;
type Query = unsafe extern "system" fn(this: *mut c_void, iid: *const u16, value: *mut *mut c_void) -> i32;
type GetInt = unsafe extern "system" fn(this: *mut c_void, value: *mut i32) -> i32;
type GetDouble = unsafe extern "system" fn(this: *mut c_void, value: *mut f64) -> i32;
type GetBool = unsafe extern "system" fn(this: *mut c_void, value: *mut u8) -> i32;
type ReleaseObject = unsafe extern "system" fn(this: *mut c_void) -> i32;

/// The free callback ADLX calls for buffers ADL handed it: `buffer` points at the pointer to free,
/// which is then cleared. Those buffers came from our ADL allocator (CoTaskMem).
unsafe extern "system" fn free_memory(buffer: *mut *mut c_void) {
    if buffer.is_null() {
        return;
    }
    // SAFETY: ADLX passes the address of a pointer it received from ADL's allocator.
    unsafe {
        crate::adl::co_free(*buffer);
        *buffer = null_mut();
    }
}

/// Method `index` of the vtable behind `obj`.
///
/// # Safety
/// `obj` must be a live ADLX interface pointer whose vtable has at least `index + 1` entries, and
/// `F` must be that method's exact signature.
unsafe fn slot<F: Copy>(obj: *mut c_void, index: usize) -> F {
    debug_assert_eq!(size_of::<F>(), size_of::<*const c_void>());
    unsafe {
        let vtable = *(obj as *const *const *const c_void);
        let f = *vtable.add(index);
        std::mem::transmute_copy::<*const c_void, F>(&f)
    }
}

/// Release a refcounted interface (slot 1). Null is a no-op.
fn release(obj: *mut c_void) {
    if !obj.is_null() {
        // SAFETY: every pointer passed here was returned by ADLX as an IADLXInterface.
        unsafe { slot::<ReleaseObject>(obj, 1)(obj) };
    }
}

/// ADLX_RESULT: OK, ALREADY_ENABLED and ALREADY_INITIALIZED all count as success.
fn ok(rc: i32) -> bool {
    (0..=2).contains(&rc)
}

/// NUL-terminated UTF-16 for an interface name.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// One ADLX metric: where its value and its support flag live in the vtables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metric {
    pub key: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    /// Slot of the getter on IADLXGPUMetrics[revision].
    pub value_slot: usize,
    /// Slot of the IsSupported… getter on IADLXGPUMetricsSupport[revision].
    pub support_slot: usize,
    /// Getter writes an `adlx_int` rather than an `adlx_double`.
    pub integer: bool,
    /// 0 for the base interfaces, n for IADLXGPUMetrics{n} / IADLXGPUMetricsSupport{n}.
    pub revision: i32,
}

const fn m(
    key: &'static str,
    label: &'static str,
    unit: &'static str,
    value_slot: usize,
    support_slot: usize,
    integer: bool,
    revision: i32,
) -> Metric {
    Metric { key, label, unit, value_slot, support_slot, integer, revision }
}

/// Owns the ADLX runtime once initialised against the backend's ADL context.
pub struct AdlxTelemetry {
    lib: Option<Library>,
    terminate: Option<Terminate>,
    system: *mut c_void,
    mapping: *mut c_void,
    performance: *mut c_void,
    attempted: bool,
    owns_runtime: bool,
    disposed: bool,
    /// Last outcome, shown in diagnostics.
    pub status: String,
}

// SAFETY: the ADLX interface pointers are not thread-affine; every use goes through `&mut self`,
// which the backend's single caller lock serialises.
unsafe impl Send for AdlxTelemetry {}

impl Default for AdlxTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl AdlxTelemetry {
    /// Slots and types follow AMD's IPerformanceMonitoring[2,3].h C vtables.
    pub const METRICS: [Metric; 14] = [
        m("usage", "GPU utilization", "%", 4, 3, false, 0),
        m("core", "GPU clock", "MHz", 5, 4, true, 0),
        m("memoryclock", "Memory clock", "MHz", 6, 5, true, 0),
        m("temperature", "GPU temperature", "°C", 7, 6, false, 0),
        m("hotspot", "Hot spot", "°C", 8, 7, false, 0),
        m("power", "GPU power (ADLX)", "W", 9, 8, false, 0),
        m("board", "Board power", "W", 10, 9, false, 0),
        m("fan", "Fan speed", "RPM", 11, 10, true, 0),
        m("vram", "Dedicated VRAM used", "MB", 12, 11, true, 0),
        m("voltage", "GPU voltage", "mV", 13, 12, true, 0),
        m("intake", "Intake temperature", "°C", 14, 24, false, 0),
        m("memorytemp", "Memory junction", "°C", 15, 25, false, 1),
        m("shared", "Shared GPU memory used", "MB", 18, 31, true, 2),
        m("fanduty", "Fan duty", "%", 19, 33, true, 3),
    ];

    pub fn new() -> Self {
        Self {
            lib: None,
            terminate: None,
            system: null_mut(),
            mapping: null_mut(),
            performance: null_mut(),
            attempted: false,
            owns_runtime: false,
            disposed: false,
            status: "ADLX not sampled".into(),
        }
    }

    /// First call only: load the runtime and initialise it on the caller's ADL context.
    fn start(&mut self, context: *mut c_void) {
        let lib = match native::load_system32(DLL) {
            Ok(lib) => lib,
            Err(_) => {
                self.status = "ADLX library not installed".into();
                return;
            }
        };
        // SAFETY: signatures from ADLX's ADLXHelper (ADLX.h C exports, cdecl).
        let query_version = unsafe { native::optional_symbol::<QueryFullVersion>(&lib, "ADLXQueryFullVersion") };
        let initialize =
            unsafe { native::optional_symbol::<InitializeWithCallerAdl>(&lib, "ADLXInitializeWithCallerAdl") };
        self.terminate = unsafe { native::optional_symbol::<Terminate>(&lib, "ADLXTerminate") };
        // Kept loaded for the life of this instance: the runtime and every vtable live in it.
        self.lib = Some(lib);
        let Some(query_version) = query_version else {
            self.status = "ADLX driver interface unavailable".into();
            return;
        };
        let mut version = 0u64;
        // SAFETY: out-pointer to a local.
        let rc = unsafe { query_version(&mut version) };
        if !ok(rc) {
            self.status = format!("ADLX version query: {rc}");
            return;
        }
        let Some(initialize) = initialize else {
            self.status = "ADLX driver interface unavailable".into();
            return;
        };
        // SAFETY: out-pointers to our fields; `context` is the backend's live ADL context, and
        // `free_memory` is a plain fn so it outlives the runtime.
        let rc = unsafe { initialize(version, &mut self.system, &mut self.mapping, context, free_memory) };
        self.owns_runtime = rc == 0;
        if !ok(rc) || self.system.is_null() || self.mapping.is_null() {
            self.status = format!("ADLX initialization: {rc}");
            return;
        }
        // IADLXSystem::GetPerformanceMonitoringServices.
        // SAFETY: `system` is a live IADLXSystem.
        let rc = unsafe { slot::<GetObject>(self.system, 9)(self.system, &mut self.performance) };
        self.status = format!("ADLX performance service: {rc}");
        if !ok(rc) {
            self.performance = null_mut();
        }
    }

    /// Current metrics for one ADL adapter, keyed by [`Metric::key`]. Empty when ADLX is missing
    /// or anything along the way fails; [`status`](Self::status) says why.
    pub fn read(&mut self, context: *mut c_void, adapter: i32) -> HashMap<&'static str, f64> {
        let mut readings = HashMap::new();
        if self.disposed {
            return readings;
        }
        if !self.attempted {
            self.attempted = true;
            self.start(context);
        }
        if self.performance.is_null() {
            return readings;
        }

        let mut gpu = null_mut();
        let mut metrics = null_mut();
        let mut support = null_mut();
        let mut acquired: Vec<*mut c_void> = Vec::new();
        // SAFETY: every pointer dereferenced below is one ADLX just handed back non-null, and each
        // slot type matches the C vtable declaration for that interface.
        unsafe {
            'body: {
                // IADLMapping::GetADLXGPUFromAdlAdapterIndex
                if !ok(slot::<MapGpu>(self.mapping, 1)(self.mapping, adapter, &mut gpu)) || gpu.is_null() {
                    self.status = "ADLX adapter mapping unavailable".into();
                    break 'body;
                }
                // IADLXPerformanceMonitoringServices::GetSupportedGPUMetrics
                if !ok(slot::<GpuObject>(self.performance, 21)(self.performance, gpu, &mut support))
                    || support.is_null()
                {
                    self.status = "ADLX support query unavailable".into();
                    break 'body;
                }
                // IADLXPerformanceMonitoringServices::GetCurrentGPUMetrics
                if !ok(slot::<GpuObject>(self.performance, 18)(self.performance, gpu, &mut metrics))
                    || metrics.is_null()
                {
                    self.status = "ADLX current metrics unavailable".into();
                    break 'body;
                }
                let mut metric_ifaces: HashMap<i32, *mut c_void> = HashMap::from([(0, metrics)]);
                let mut support_ifaces: HashMap<i32, *mut c_void> = HashMap::from([(0, support)]);
                for rev in 1..=3 {
                    let mut mi = null_mut();
                    let name = wide(&format!("IADLXGPUMetrics{rev}"));
                    if ok(slot::<Query>(metrics, 2)(metrics, name.as_ptr(), &mut mi)) && !mi.is_null() {
                        metric_ifaces.insert(rev, mi);
                        acquired.push(mi);
                    }
                    let mut si = null_mut();
                    let name = wide(&format!("IADLXGPUMetricsSupport{rev}"));
                    if ok(slot::<Query>(support, 2)(support, name.as_ptr(), &mut si)) && !si.is_null() {
                        support_ifaces.insert(rev, si);
                        acquired.push(si);
                    }
                }
                for d in &Self::METRICS {
                    let (Some(&mi), Some(&si)) = (metric_ifaces.get(&d.revision), support_ifaces.get(&d.revision))
                    else {
                        continue;
                    };
                    let mut enabled = 0u8;
                    if !ok(slot::<GetBool>(si, d.support_slot)(si, &mut enabled)) || enabled == 0 {
                        continue;
                    }
                    let (rc, value) = if d.integer {
                        let mut v = 0i32;
                        let rc = slot::<GetInt>(mi, d.value_slot)(mi, &mut v);
                        (rc, v as f64)
                    } else {
                        let mut v = 0f64;
                        let rc = slot::<GetDouble>(mi, d.value_slot)(mi, &mut v);
                        (rc, v)
                    };
                    if ok(rc) && value.is_finite() && value >= 0.0 {
                        readings.insert(d.key, value);
                    }
                }
                self.status = format!("ADLX: {} live metrics", readings.len());
            }
        }
        for ptr in acquired {
            release(ptr);
        }
        release(metrics);
        release(support);
        release(gpu);
        readings
    }

    /// Release the performance service and shut the runtime down if this instance started it.
    /// Permanent: a disposed instance never samples again.
    pub fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        release(self.performance);
        self.performance = null_mut();
        // IADLXSystem and IADLMapping are runtime-owned (no Acquire/Release slots).
        if self.owns_runtime
            && let Some(terminate) = self.terminate
        {
            // SAFETY: the runtime was initialised by this instance and the library is still loaded.
            unsafe { terminate() };
        }
        self.system = null_mut();
        self.mapping = null_mut();
    }
}

impl Drop for AdlxTelemetry {
    fn drop(&mut self) {
        self.dispose();
        // Never unloaded (as with P/Invoke): runtime threads may outlive ADLXTerminate.
        if let Some(lib) = self.lib.take() {
            std::mem::forget(lib);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_codes() {
        assert!(ok(0) && ok(1) && ok(2));
        assert!(!ok(-1) && !ok(3));
    }

    #[test]
    fn wide_is_nul_terminated() {
        assert_eq!(wide("IA"), vec![b'I' as u16, b'A' as u16, 0]);
    }

    #[test]
    fn disposed_never_samples() {
        let mut t = AdlxTelemetry::new();
        t.dispose();
        assert!(t.read(null_mut(), 0).is_empty());
        assert_eq!(t.status, "ADLX not sampled");
    }

    #[test]
    fn revisions_are_queried_interfaces() {
        assert!(AdlxTelemetry::METRICS.iter().all(|m| (0..=3).contains(&m.revision)));
    }
}
