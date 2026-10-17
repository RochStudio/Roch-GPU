//! Opt-in timing log for startup and the window lifecycle (`ROCH_TRACE=1`).
//!
//! Off unless the variable is set, and then free: every call site is one `OnceLock` load and the
//! label is never formatted. On, each [`trace!`] line is stamped with the milliseconds since the
//! *process was created* (not since `main`, so loader and CRT time count) and written to stderr
//! and appended to `%TEMP%\roch-gpu-trace.log` (or to the file `ROCH_TRACE` names, when it is not
//! `1`). A windows-subsystem exe usually has no stderr, hence the file.
//!
//! `ROCH_TRACE_BENCH=1` (with `ROCH_TRACE`) also makes the window run a fixed open/close sequence
//! and exit — see `gui::app::Bench`. Only use it with `--mock`. Numbers are recorded in `PERF.md`.

use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

struct Tracer {
    origin: SystemTime,
    file: Mutex<Option<File>>,
    bench: Option<Bench>,
}

/// Which scripted run `ROCH_TRACE_BENCH` asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bench {
    /// `1`: the whole open/close sequence (see `gui::bench`).
    Full,
    /// `startup`: exit as soon as the first frame is on screen.
    Startup,
}

static TRACER: OnceLock<Option<Tracer>> = OnceLock::new();

/// Read the environment once. Call first thing in `main`; until then (and in tests) tracing is off.
pub fn init() {
    TRACER.get_or_init(|| {
        let value = std::env::var_os("ROCH_TRACE").filter(|v| !v.is_empty() && v != "0")?;
        let path = if value == "1" {
            std::env::temp_dir().join("roch-gpu-trace.log")
        } else {
            std::path::PathBuf::from(value)
        };
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok();
        let bench = match std::env::var("ROCH_TRACE_BENCH").as_deref() {
            Ok("1") => Some(Bench::Full),
            Ok("startup") => Some(Bench::Startup),
            _ => None,
        };
        Some(Tracer { origin: imp::process_created().unwrap_or_else(SystemTime::now), file: Mutex::new(file), bench })
    });
}

/// Is `ROCH_TRACE` on?
#[inline]
pub fn enabled() -> bool {
    matches!(TRACER.get(), Some(Some(_)))
}

/// The scripted window benchmark, when `ROCH_TRACE_BENCH` asks for one (with `ROCH_TRACE` on).
pub fn bench() -> Option<Bench> {
    match TRACER.get() {
        Some(Some(t)) => t.bench,
        _ => None,
    }
}

/// A benchmark run against the simulated GPU stays out of the single-instance handshake, so it
/// can run beside a resident copy of the app without signalling it or being turned away by it.
pub fn isolated(args: &[String]) -> bool {
    bench().is_some() && args.iter().any(|a| a == "--mock")
}

/// Milliseconds since the process was created.
pub fn since_start_ms() -> f64 {
    match TRACER.get() {
        Some(Some(t)) => SystemTime::now().duration_since(t.origin).unwrap_or_default().as_secs_f64() * 1000.0,
        _ => 0.0,
    }
}

/// Write one stamped line. Use [`trace!`], which skips the formatting when tracing is off.
pub fn write(line: &str) {
    let Some(Some(t)) = TRACER.get() else { return };
    let (ws, _) = memory_mib();
    let text = format!("{:>9.1} ms  {ws:>6.1} MiB  {line}\n", since_start_ms());
    let _ = std::io::stderr().write_all(text.as_bytes());
    if let Ok(mut guard) = t.file.lock()
        && let Some(f) = guard.as_mut()
    {
        let _ = f.write_all(text.as_bytes());
    }
}

/// CPU time (user + kernel) this process has used so far.
pub fn cpu_time() -> Duration {
    imp::cpu_time()
}

/// Working set and private bytes, in MiB.
pub fn memory_mib() -> (f64, f64) {
    imp::memory()
}

/// `trace!("opened {}", name)`: a stamped line in the trace log, formatted only when tracing is on.
#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {
        if $crate::trace::enabled() {
            $crate::trace::write(&format!($($arg)*));
        }
    };
}

#[cfg(windows)]
mod imp {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    fn ticks(ft: &FILETIME) -> u64 {
        (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
    }

    fn times() -> Option<[FILETIME; 4]> {
        let mut t = [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 4];
        let [a, b, c, d] = &mut t;
        // SAFETY: the pseudo-handle needs no closing; all four out-pointers are valid.
        let ok = unsafe { GetProcessTimes(GetCurrentProcess(), a, b, c, d) };
        (ok != 0).then_some(t)
    }

    pub fn process_created() -> Option<SystemTime> {
        // FILETIME counts 100 ns ticks since 1601-01-01.
        const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
        let created = ticks(&times()?[0]).checked_sub(UNIX_EPOCH_TICKS)?;
        Some(UNIX_EPOCH + Duration::from_nanos(created * 100))
    }

    pub fn cpu_time() -> Duration {
        times().map(|t| Duration::from_nanos((ticks(&t[2]) + ticks(&t[3])) * 100)).unwrap_or_default()
    }

    pub fn memory() -> (f64, f64) {
        // SAFETY: plain out-struct of the size we pass.
        unsafe {
            let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
            let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            if K32GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut c).cast(), size) == 0 {
                return (0.0, 0.0);
            }
            let mib = |b: usize| b as f64 / (1024.0 * 1024.0);
            (mib(c.WorkingSetSize), mib(c.PrivateUsage))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::time::{Duration, SystemTime};

    pub fn process_created() -> Option<SystemTime> {
        None
    }
    pub fn cpu_time() -> Duration {
        Duration::ZERO
    }
    pub fn memory() -> (f64, f64) {
        (0.0, 0.0)
    }
}
