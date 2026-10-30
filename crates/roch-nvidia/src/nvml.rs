//! Minimal NVML binding: board watts, PCIe link, BAR1 size, clock limits, and locking the graphics
//! clock range and the memory clock — the same things `nvidia-smi --lock-gpu-clocks` and
//! `--lock-memory-clocks` do.
//!
//! NVML is a public, documented driver API, unlike the private clock-boost lock, so it is the
//! dependable way to hold the core at a chosen point on the V/F curve. Writes need administrator
//! rights and driver r465 or newer. nvml.dll is loaded from System32 only.
//!
//! One session per process (process-global, as the C# static class was), and it does not outlive
//! a display driver reset — see [`session_may_be_stale`] for what that looked like and how it is
//! recovered.

use std::ffi::{CStr, c_char, c_void};
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock};

const SUCCESS: i32 = 0;
const UNINITIALIZED: i32 = 1;
const GPU_IS_LOST: i32 = 15;
const RESET_REQUIRED: i32 = 16;
const UNKNOWN: i32 = 999;

/// NVML_CLOCK_GRAPHICS.
const CLOCK_GRAPHICS: i32 = 0;

type Device = *mut c_void;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct NvmlMemory {
    total: u64,
    free: u64,
    used: u64,
}
const _: () = assert!(std::mem::size_of::<NvmlMemory>() == 24);

type InitFn = unsafe extern "C" fn() -> i32;
type GetHandleFn = unsafe extern "C" fn(u32, *mut Device) -> i32;
type SetLockedFn = unsafe extern "C" fn(Device, u32, u32) -> i32;
type DeviceFn = unsafe extern "C" fn(Device) -> i32;
type ErrorStringFn = unsafe extern "C" fn(i32) -> *const c_char;
type GetNameFn = unsafe extern "C" fn(Device, *mut c_char, u32) -> i32;
type GetU32Fn = unsafe extern "C" fn(Device, *mut u32) -> i32;
type GetClockFn = unsafe extern "C" fn(Device, i32, *mut u32) -> i32;
type GetMinMaxPStateFn = unsafe extern "C" fn(Device, i32, i32, *mut u32, *mut u32) -> i32;
type GetMemoryFn = unsafe extern "C" fn(Device, *mut NvmlMemory) -> i32;
type GetThresholdFn = unsafe extern "C" fn(Device, i32, *mut u32) -> i32;
type GetSupportedClocksFn = unsafe extern "C" fn(Device, *mut u32, *mut u32) -> i32;
type GetSupportedGraphicsFn = unsafe extern "C" fn(Device, u32, *mut u32, *mut u32) -> i32;

/// Every entry point, each resolved independently: the C# DllImports bound lazily, so a missing
/// one only failed the call that needed it.
struct Api {
    _lib: roch_core::native::Library,
    init: Option<InitFn>,
    shutdown: Option<InitFn>,
    get_handle: Option<GetHandleFn>,
    set_locked: Option<SetLockedFn>,
    reset_locked: Option<DeviceFn>,
    set_mem_locked: Option<SetLockedFn>,
    reset_mem_locked: Option<DeviceFn>,
    get_supported_mem_clocks: Option<GetSupportedClocksFn>,
    get_supported_gr_clocks: Option<GetSupportedGraphicsFn>,
    error_string: Option<ErrorStringFn>,
    get_name: Option<GetNameFn>,
    get_power_usage: Option<GetU32Fn>,
    get_default_power_limit: Option<GetU32Fn>,
    get_max_clock: Option<GetClockFn>,
    get_min_max_of_pstate: Option<GetMinMaxPStateFn>,
    get_memory_info: Option<GetMemoryFn>,
    get_bar1_memory_info: Option<GetMemoryFn>,
    get_curr_pcie_gen: Option<GetU32Fn>,
    get_max_pcie_gen: Option<GetU32Fn>,
    get_curr_pcie_width: Option<GetU32Fn>,
    get_max_pcie_width: Option<GetU32Fn>,
    get_temperature_threshold: Option<GetThresholdFn>,
}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

fn api() -> Result<&'static Api, &'static String> {
    API.get_or_init(|| {
        let lib = roch_core::native::load_system32("nvml.dll")
            .map_err(|_| "nvml.dll not found (is the NVIDIA driver installed?)".to_string())?;
        // SAFETY: each signature matches the NVML header for that export.
        unsafe {
            use roch_core::native::optional_symbol as sym;
            Ok(Api {
                init: sym(&lib, "nvmlInit_v2"),
                shutdown: sym(&lib, "nvmlShutdown"),
                get_handle: sym(&lib, "nvmlDeviceGetHandleByIndex_v2"),
                set_locked: sym(&lib, "nvmlDeviceSetGpuLockedClocks"),
                reset_locked: sym(&lib, "nvmlDeviceResetGpuLockedClocks"),
                set_mem_locked: sym(&lib, "nvmlDeviceSetMemoryLockedClocks"),
                reset_mem_locked: sym(&lib, "nvmlDeviceResetMemoryLockedClocks"),
                get_supported_mem_clocks: sym(&lib, "nvmlDeviceGetSupportedMemoryClocks"),
                get_supported_gr_clocks: sym(&lib, "nvmlDeviceGetSupportedGraphicsClocks"),
                error_string: sym(&lib, "nvmlErrorString"),
                get_name: sym(&lib, "nvmlDeviceGetName"),
                get_power_usage: sym(&lib, "nvmlDeviceGetPowerUsage"),
                get_default_power_limit: sym(&lib, "nvmlDeviceGetPowerManagementDefaultLimit"),
                get_max_clock: sym(&lib, "nvmlDeviceGetMaxClockInfo"),
                get_min_max_of_pstate: sym(&lib, "nvmlDeviceGetMinMaxClockOfPState"),
                get_memory_info: sym(&lib, "nvmlDeviceGetMemoryInfo"),
                get_bar1_memory_info: sym(&lib, "nvmlDeviceGetBAR1MemoryInfo"),
                get_curr_pcie_gen: sym(&lib, "nvmlDeviceGetCurrPcieLinkGeneration"),
                get_max_pcie_gen: sym(&lib, "nvmlDeviceGetMaxPcieLinkGeneration"),
                get_curr_pcie_width: sym(&lib, "nvmlDeviceGetCurrPcieLinkWidth"),
                get_max_pcie_width: sym(&lib, "nvmlDeviceGetMaxPcieLinkWidth"),
                get_temperature_threshold: sym(&lib, "nvmlDeviceGetTemperatureThreshold"),
                _lib: lib,
            })
        }
    })
    .as_ref()
}

/// The C# `EntryPointNotFoundException` message shape.
fn missing(name: &str) -> String {
    format!("Unable to find an entry point named '{name}' in DLL 'nvml.dll'.")
}

#[derive(Default)]
struct State {
    init_tried: bool,
    init_ok: bool,
    last_error: Option<String>,
    session_epoch: i32,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

impl State {
    fn is_available(&mut self) -> bool {
        if self.init_tried {
            return self.init_ok;
        }
        self.init_tried = true;
        match api() {
            Err(e) => self.last_error = Some(e.clone()),
            Ok(a) => match a.init {
                None => self.last_error = Some(format!("nvml.dll is too old: {}", missing("nvmlInit_v2"))),
                Some(init) => {
                    // SAFETY: nvmlInit_v2 takes no arguments.
                    let r = unsafe { init() };
                    self.init_ok = r == SUCCESS;
                    if !self.init_ok {
                        self.last_error = Some(format!("nvmlInit failed: {}", describe(r)));
                    }
                }
            },
        }
        self.init_ok
    }

    fn try_shutdown(&mut self) {
        if !self.init_ok {
            return;
        }
        if let Ok(a) = api()
            && let Some(f) = a.shutdown
        {
            // SAFETY: nvmlShutdown takes no arguments.
            unsafe { f() };
        }
        self.init_ok = false;
        self.init_tried = false;
    }

    /// Drop the session and open a new one. True when the new one initialised.
    ///
    /// Nothing rate-limits this because nothing needs to: a card that is genuinely gone fails the
    /// re-initialise too, `is_available` then answers false, and every later call stops at the top
    /// of `read`/`write` without reaching here.
    fn reestablish(&mut self) -> bool {
        self.try_shutdown();
        if !self.is_available() {
            return false;
        }
        self.session_epoch += 1;
        true
    }
}

/// Fetch the device and run the call. `Reached` says whether the call itself was made.
fn attempt(gpu_index: i32, call: &mut dyn FnMut(&Api, Device) -> Result<i32, String>) -> Result<(i32, bool), String> {
    let a = api().map_err(|e| e.clone())?;
    let get = a.get_handle.ok_or_else(|| missing("nvmlDeviceGetHandleByIndex_v2"))?;
    let mut dev: Device = std::ptr::null_mut();
    // SAFETY: index by value, out-pointer valid.
    let r = unsafe { get(gpu_index as u32, &mut dev) };
    if r == SUCCESS { Ok((call(a, dev)?, true)) } else { Ok((r, false)) }
}

/// True when a return code means this process's session has gone stale rather than the request
/// being wrong — the one kind of failure a re-initialise can fix.
///
/// A display driver reset invalidates every NVML session open across it. NVML has a code for
/// that, GPU_IS_LOST, but a 5070 Ti coming back from one answers UNKNOWN instead, which is why 999
/// is here — measured: after a driver crash nvmlDeviceResetGpuLockedClocks returned 999 for as
/// long as the app stayed open while NVAPI writes kept working. Shutting NVML down and bringing it
/// back up is what turns this process into a fresh one.
pub fn session_may_be_stale(result: i32) -> bool {
    matches!(result, UNINITIALIZED | GPU_IS_LOST | RESET_REQUIRED | UNKNOWN)
}

/// Run a call that changes something, retrying once on a new session when the first failure looks
/// stale. `None` on success, else the reason.
fn write(gpu_index: i32, name: &str, mut call: impl FnMut(&Api, Device) -> Result<i32, String>) -> Option<String> {
    let mut st = state();
    if !st.is_available() {
        return Some(st.last_error.clone().unwrap_or_else(|| "NVML unavailable".into()));
    }
    let run =
        |st: &mut State, call: &mut dyn FnMut(&Api, Device) -> Result<i32, String>| -> Result<Option<String>, String> {
            let (mut r, mut reached) = attempt(gpu_index, call)?;
            if session_may_be_stale(r) && st.reestablish() {
                (r, reached) = attempt(gpu_index, call)?;
            }
            if r == SUCCESS {
                return Ok(None);
            }
            let place = if reached { name } else { "nvmlDeviceGetHandleByIndex" };
            Ok(Some(if session_may_be_stale(r) {
                format!(
                    "{place}: {} — the display driver was reset out from under this session, which a game crashing \
                 will do, and a new session could not reach the card either. Restart Roch GPU.",
                    describe(r)
                )
            } else {
                format!("{place}: {}", describe(r))
            }))
        };
    run(&mut st, &mut call).unwrap_or_else(Some)
}

/// Run a call that reads something, recovering the same way. False when there is no answer.
fn read(gpu_index: i32, mut call: impl FnMut(&Api, Device) -> Result<i32, String>) -> bool {
    let mut st = state();
    if !st.is_available() {
        return false;
    }
    let mut run = || -> Result<bool, String> {
        let mut r = attempt(gpu_index, &mut call)?.0;
        if session_may_be_stale(r) && st.reestablish() {
            r = attempt(gpu_index, &mut call)?.0;
        }
        Ok(r == SUCCESS)
    };
    run().unwrap_or(false)
}

fn describe(result: i32) -> String {
    let text = api().ok().and_then(|a| a.error_string).and_then(|f| {
        // SAFETY: nvmlErrorString returns a static NUL-terminated string or null.
        let p = unsafe { f(result) };
        if p.is_null() {
            None
        } else {
            // SAFETY: non-null, NUL-terminated, static lifetime per the NVML docs.
            Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
        }
    });
    match text {
        Some(s) if !s.is_empty() => format!("{s} ({result})"),
        _ => format!("error {result}"),
    }
}

/// True once nvml.dll has loaded and initialised. Safe to call repeatedly.
pub fn is_available() -> bool {
    state().is_available()
}

/// Last failure, for diagnostics. `None` while everything is fine.
pub fn last_error() -> Option<String> {
    state().last_error.clone()
}

/// Bumped every time the session is thrown away and reopened, which only happens when the display
/// driver has been reset under us. Anything remembered about driver state from before that point —
/// a clock lock, say — describes a card that no longer exists.
pub fn session_epoch() -> i32 {
    state().session_epoch
}

/// Pin the graphics clock to [min, max] MHz. `None` on success, else the reason.
pub fn lock_graphics_clocks(gpu_index: i32, min_mhz: i32, max_mhz: i32) -> Option<String> {
    write(gpu_index, &format!("nvmlDeviceSetGpuLockedClocks({min_mhz},{max_mhz})"), |a, dev| {
        let f = a.set_locked.ok_or_else(|| missing("nvmlDeviceSetGpuLockedClocks"))?;
        // SAFETY: device handle from GetHandleByIndex; plain integers.
        Ok(unsafe { f(dev, min_mhz.max(0) as u32, max_mhz.max(0) as u32) })
    })
}

/// Hand the clock range back to the driver. `None` on success, else the reason.
pub fn reset_graphics_clocks(gpu_index: i32) -> Option<String> {
    write(gpu_index, "nvmlDeviceResetGpuLockedClocks", |a, dev| {
        let f = a.reset_locked.ok_or_else(|| missing("nvmlDeviceResetGpuLockedClocks"))?;
        // SAFETY: device handle from GetHandleByIndex.
        Ok(unsafe { f(dev) })
    })
}

/// Pin the memory clock to [min, max] MHz. `None` on success, else the reason.
pub fn lock_memory_clocks(gpu_index: i32, min_mhz: i32, max_mhz: i32) -> Option<String> {
    write(gpu_index, &format!("nvmlDeviceSetMemoryLockedClocks({min_mhz},{max_mhz})"), |a, dev| {
        let f = a.set_mem_locked.ok_or_else(|| missing("nvmlDeviceSetMemoryLockedClocks"))?;
        // SAFETY: device handle from GetHandleByIndex; plain integers.
        Ok(unsafe { f(dev, min_mhz.max(0) as u32, max_mhz.max(0) as u32) })
    })
}

/// Hand the memory clock back to the driver. `None` on success, else the reason.
pub fn reset_memory_clocks(gpu_index: i32) -> Option<String> {
    write(gpu_index, "nvmlDeviceResetMemoryLockedClocks", |a, dev| {
        let f = a.reset_mem_locked.ok_or_else(|| missing("nvmlDeviceResetMemoryLockedClocks"))?;
        // SAFETY: device handle from GetHandleByIndex.
        Ok(unsafe { f(dev) })
    })
}

/// The memory clocks a lock can safely hold, in MHz, ascending; empty when NVML will not say.
///
/// Only the ones that leave the whole graphics clock range open. Each memory clock carries its own
/// list of graphics clocks, and the lowest does not: a 5070 Ti lists 14001, 13801, 7001, 810 and
/// 405 MHz, and pinning 405 caps the core at 885 MHz (measured: the card mined at 885). 810, the
/// figure mining tools use, still runs the core to 3090.
pub fn memory_lock_clocks_mhz(gpu_index: i32) -> Vec<i32> {
    let mems = supported(gpu_index, |a, dev, count, out| {
        let f = a.get_supported_mem_clocks.ok_or_else(|| missing("nvmlDeviceGetSupportedMemoryClocks"))?;
        // SAFETY: `out` has room for `*count` entries.
        Ok(unsafe { f(dev, count, out) })
    });
    let tops: Vec<(i32, u32)> = mems
        .iter()
        .filter(|&&m| m > 0)
        .map(|&m| {
            let gr = supported(gpu_index, |a, dev, count, out| {
                let f = a.get_supported_gr_clocks.ok_or_else(|| missing("nvmlDeviceGetSupportedGraphicsClocks"))?;
                // SAFETY: `out` has room for `*count` entries.
                Ok(unsafe { f(dev, m, count, out) })
            });
            (m as i32, gr.into_iter().max().unwrap_or(0))
        })
        .collect();
    let top = tops.iter().map(|t| t.1).max().unwrap_or(0);
    let mut safe: Vec<i32> = tops.into_iter().filter(|&(_, g)| top > 0 && g == top).map(|(m, _)| m).collect();
    safe.sort_unstable();
    safe.dedup();
    safe
}

/// A `nvmlDeviceGetSupported*Clocks` list. Empty when NVML will not say.
fn supported(
    gpu_index: i32,
    mut call: impl FnMut(&Api, Device, *mut u32, *mut u32) -> Result<i32, String>,
) -> Vec<u32> {
    let mut clocks = vec![0u32; 1024];
    let mut count = 0u32;
    let ok = read(gpu_index, |a, dev| {
        // In: the buffer's capacity, as the header asks; out: how many it filled. Set on every try,
        // because a retry after a stale session would otherwise pass the first try's answer.
        count = clocks.len() as u32;
        call(a, dev, &mut count, clocks.as_mut_ptr())
    });
    if !ok {
        return Vec::new();
    }
    clocks.truncate((count as usize).min(clocks.len()));
    clocks
}

/// `nvmlTemperatureThresholds_t`. The acoustic trio is the GPU target temperature (`nvidia-smi
/// -gtt`): the temperature the boost algorithm holds the card at by shedding clocks, and the only
/// one NVML lets an administrator move. A GeForce RTX 5070 Ti (616.92) reports none of the acoustic
/// trio — only shutdown 93, slowdown 90 and GPU max 88 °C — so no thermal limit is offered there.
pub const THRESHOLD_SHUTDOWN: i32 = 0;
pub const THRESHOLD_SLOWDOWN: i32 = 1;
pub const THRESHOLD_MEM_MAX: i32 = 2;
pub const THRESHOLD_GPU_MAX: i32 = 3;
pub const THRESHOLD_ACOUSTIC_MIN: i32 = 4;
pub const THRESHOLD_ACOUSTIC_CURR: i32 = 5;
pub const THRESHOLD_ACOUSTIC_MAX: i32 = 6;
pub const THRESHOLD_GPS_CURR: i32 = 7;

/// One temperature threshold in °C, or `None` when the card does not report it.
pub fn temperature_threshold_c(gpu_index: i32, threshold: i32) -> Option<i32> {
    let mut c = 0u32;
    let ok = read(gpu_index, |a, dev| {
        let f = a.get_temperature_threshold.ok_or_else(|| missing("nvmlDeviceGetTemperatureThreshold"))?;
        // SAFETY: plain enum value and a valid out-pointer.
        Ok(unsafe { f(dev, threshold, &mut c) })
    });
    ok.then_some(c as i32)
}

/// Board power draw in watts, or NaN when NVML cannot say.
///
/// NVAPI has no watts on this card: its power families report per-cent-mille of the limit and
/// nothing else, so this is where the draw comes from.
pub fn power_watts(gpu_index: i32) -> f64 {
    let mut mw = 0u32;
    let ok = read(gpu_index, |a, dev| {
        let f = a.get_power_usage.ok_or_else(|| missing("nvmlDeviceGetPowerUsage"))?;
        // SAFETY: valid out-pointer.
        Ok(unsafe { f(dev, &mut mw) })
    });
    if ok { mw as f64 / 1000.0 } else { f64::NAN }
}

/// The card's default (stock TDP) power limit in watts, or NaN when NVML cannot say. NVAPI's
/// power-topology percentage is board power against this figure (247 W read 82.4% of a 5070 Ti's
/// 300 W), so with [`power_watts`] it gives the same percentage without that 5 ms call.
pub fn default_power_limit_watts(gpu_index: i32) -> f64 {
    let mut mw = 0u32;
    let ok = read(gpu_index, |a, dev| {
        let f = a.get_default_power_limit.ok_or_else(|| missing("nvmlDeviceGetPowerManagementDefaultLimit"))?;
        // SAFETY: valid out-pointer.
        Ok(unsafe { f(dev, &mut mw) })
    });
    if ok && mw > 0 { mw as f64 / 1000.0 } else { f64::NAN }
}

/// Highest graphics clock the driver will report, in MHz; 0 when NVML cannot say. The top of the
/// clock-range control, so the slider stops where the card does.
pub fn max_graphics_clock_mhz(gpu_index: i32) -> i32 {
    let mut mhz = 0u32;
    let ok = read(gpu_index, |a, dev| {
        let f = a.get_max_clock.ok_or_else(|| missing("nvmlDeviceGetMaxClockInfo"))?;
        // SAFETY: valid out-pointer.
        Ok(unsafe { f(dev, CLOCK_GRAPHICS, &mut mhz) })
    });
    if ok { mhz as i32 } else { 0 }
}

/// Whether BAR1 spans nearly all VRAM. A legacy 256 MB aperture means disabled; with Resizable BAR
/// NVML reports an aperture close to the frame-buffer size.
pub fn resizable_bar_enabled(gpu_index: i32) -> Option<bool> {
    let mut vram = NvmlMemory::default();
    let mut bar = NvmlMemory::default();
    let ok = read(gpu_index, |a, dev| {
        let mem = a.get_memory_info.ok_or_else(|| missing("nvmlDeviceGetMemoryInfo"))?;
        // SAFETY: valid out-pointer to a v1 nvmlMemory_t.
        let r = unsafe { mem(dev, &mut vram) };
        if r != SUCCESS {
            return Ok(r);
        }
        let bar1 = a.get_bar1_memory_info.ok_or_else(|| missing("nvmlDeviceGetBAR1MemoryInfo"))?;
        // SAFETY: nvmlBAR1Memory_t has the same three u64 fields.
        Ok(unsafe { bar1(dev, &mut bar) })
    });
    if !ok || vram.total == 0 || bar.total == 0 {
        return None;
    }
    Some(bar.total as f64 >= vram.total as f64 * 0.9)
}

/// PCIe capability and the negotiated link.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PcieLink {
    pub max_generation: i32,
    pub max_width: i32,
    pub current_generation: i32,
    pub current_width: i32,
}

/// PCIe capability and the link negotiated right now. NVML reports both sides independently, which
/// matters because the generation commonly drops while the GPU idles.
pub fn pcie_link(gpu_index: i32) -> PcieLink {
    let one = |pick: fn(&Api) -> Option<GetU32Fn>, name: &'static str| -> i32 {
        let mut v = 0u32;
        let ok = read(gpu_index, |a, dev| {
            let f = pick(a).ok_or_else(|| missing(name))?;
            // SAFETY: valid out-pointer.
            Ok(unsafe { f(dev, &mut v) })
        });
        if ok { v as i32 } else { 0 }
    };
    PcieLink {
        max_generation: one(|a| a.get_max_pcie_gen, "nvmlDeviceGetMaxPcieLinkGeneration"),
        max_width: one(|a| a.get_max_pcie_width, "nvmlDeviceGetMaxPcieLinkWidth"),
        current_generation: one(|a| a.get_curr_pcie_gen, "nvmlDeviceGetCurrPcieLinkGeneration"),
        current_width: one(|a| a.get_curr_pcie_width, "nvmlDeviceGetCurrPcieLinkWidth"),
    }
}

/// Lowest graphics clock the driver will discuss, in MHz; 0 when it will not say.
///
/// Asked per p-state and reduced to the smallest answer, because no single call reports it: a
/// 5070 Ti says 495 MHz for P0 and 180 for P8, and refuses several p-states outright. 180 is the
/// real floor, and the driver accepts it as one.
pub fn min_graphics_clock_mhz(gpu_index: i32) -> i32 {
    let mut st = state();
    if !st.is_available() {
        return 0;
    }
    let Ok(a) = api() else { return 0 };
    let (Some(get), Some(minmax)) = (a.get_handle, a.get_min_max_of_pstate) else { return 0 };
    let mut dev: Device = std::ptr::null_mut();
    // SAFETY: valid out-pointer.
    if unsafe { get(gpu_index as u32, &mut dev) } != SUCCESS {
        return 0;
    }
    let mut best = 0i32;
    for pstate in 0..=15 {
        let (mut lo, mut hi) = (0u32, 0u32);
        // SAFETY: device handle and two valid out-pointers.
        if unsafe { minmax(dev, CLOCK_GRAPHICS, pstate, &mut lo, &mut hi) } != SUCCESS {
            continue;
        }
        if lo > 0 && (best == 0 || (lo as i32) < best) {
            best = lo as i32;
        }
    }
    drop(st);
    best
}

/// Device name as NVML sees it — used to line its index up with the NVAPI one.
pub fn device_name(gpu_index: i32) -> Option<String> {
    let mut buf = [0u8; 96];
    let ok = read(gpu_index, |a, dev| {
        let f = a.get_name.ok_or_else(|| missing("nvmlDeviceGetName"))?;
        // SAFETY: buffer and its true length.
        Ok(unsafe { f(dev, buf.as_mut_ptr() as *mut c_char, buf.len() as u32) })
    });
    if !ok {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..len]).into_owned())
}

/// Unload the session if one is open. Does NOT release a clock cap — deliberately, so a cap
/// survives closing the app, exactly like nvidia-smi's own locks.
pub fn try_shutdown() {
    state().try_shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_session_codes() {
        // The card cannot be reset on demand, so what is tested is the decision that drives the
        // recovery: a stale session is retried, a wrong request is not.
        assert!(session_may_be_stale(999), "999 UNKNOWN retries");
        assert!(session_may_be_stale(15), "15 GPU_IS_LOST retries");
        assert!(session_may_be_stale(16), "16 RESET_REQUIRED retries");
        assert!(session_may_be_stale(1), "1 UNINITIALIZED retries");
        assert!(!session_may_be_stale(0), "0 SUCCESS is not a failure");
        assert!(!session_may_be_stale(3), "3 NOT_SUPPORTED does not retry");
        assert!(!session_may_be_stale(4), "4 NO_PERMISSION does not retry");
        assert!(!session_may_be_stale(2), "2 INVALID_ARGUMENT does not retry");
    }
}
