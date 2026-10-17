//! Console plumbing for the command-line half of a windows-subsystem executable.
//!
//! A windows-subsystem exe owns no console, so when one launched us we have to borrow it or every
//! line the CLI prints goes nowhere. But borrowing is exactly wrong when the caller redirected us:
//! `diag > file.txt` already hands us a valid handle, and attaching a console replaces it. So the
//! redirection test comes first, before anything can change the handles under it.

use std::sync::atomic::{AtomicBool, Ordering};

static CTRL_C: AtomicBool = AtomicBool::new(false);

/// Puts the caller's console code page back when dropped (also on a panic unwinding through it).
#[must_use]
pub struct ConsoleGuard {
    restore_code_page: Option<u32>,
}

impl Drop for ConsoleGuard {
    fn drop(&mut self) {
        if let Some(cp) = self.restore_code_page.take() {
            use std::io::Write;
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            imp::set_output_code_page(cp);
        }
    }
}

/// Point stdout/stderr at wherever this run's output should go.
pub fn attach_to_output() -> ConsoleGuard {
    ConsoleGuard { restore_code_page: imp::attach_to_output() }
}

/// Route Ctrl+C / Ctrl+Break to [`ctrl_c_pressed`] instead of terminating (C# `CancelKeyPress`
/// with `e.Cancel = true`).
pub fn install_ctrl_c_handler() {
    CTRL_C.store(false, Ordering::SeqCst);
    imp::install_ctrl_c_handler();
}

pub fn ctrl_c_pressed() -> bool {
    CTRL_C.load(Ordering::SeqCst)
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::Ordering;

    use windows_sys::Win32::Foundation::{BOOL, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, CTRL_BREAK_EVENT, CTRL_C_EVENT, GetConsoleOutputCP, GetStdHandle,
        STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, SetConsoleOutputCP, SetStdHandle,
    };

    const CP_UTF8: u32 = 65001;

    pub fn attach_to_output() -> Option<u32> {
        // SAFETY: plain Win32 calls with valid arguments; handles are only compared or installed.
        unsafe {
            // Decide on the handle itself: a GUI-subsystem process launched from a prompt inherits
            // no standard output at all (null), while under `> file.txt` it gets a real handle.
            let current = GetStdHandle(STD_OUTPUT_HANDLE);
            let inherited = !current.is_null() && current != INVALID_HANDLE_VALUE;
            if inherited || AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
                return None;
            }

            // Rust writes a console handle as UTF-16, so the code page does not affect our own
            // output; it is still set (and put back afterwards, because the code page outlives
            // this process and belongs to the caller's window) for parity with the original, and
            // so any child output written as UTF-8 renders.
            let previous = GetConsoleOutputCP();
            SetConsoleOutputCP(CP_UTF8);

            // AttachConsole does not backfill the standard handles, so open the console device by
            // name. Rust's stdout asks GetStdHandle on every write, so installing it is enough.
            let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
            let h = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if h != INVALID_HANDLE_VALUE {
                SetStdHandle(STD_OUTPUT_HANDLE, h);
                SetStdHandle(STD_ERROR_HANDLE, h);
            }
            if previous != 0 { Some(previous) } else { None }
        }
    }

    pub fn set_output_code_page(cp: u32) {
        // SAFETY: no pointers involved.
        unsafe {
            SetConsoleOutputCP(cp);
        }
    }

    unsafe extern "system" fn handler(ctrl_type: u32) -> BOOL {
        if ctrl_type == CTRL_C_EVENT || ctrl_type == CTRL_BREAK_EVENT {
            super::CTRL_C.store(true, Ordering::SeqCst);
            1
        } else {
            0
        }
    }

    pub fn install_ctrl_c_handler() {
        // SAFETY: `handler` is a static function with the PHANDLER_ROUTINE signature.
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn attach_to_output() -> Option<u32> {
        None
    }
    pub fn set_output_code_page(_cp: u32) {}
    pub fn install_ctrl_c_handler() {}
}
