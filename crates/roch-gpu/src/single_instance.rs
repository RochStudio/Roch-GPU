//! One window at a time.
//!
//! A resident startup run sits in the tray when you double-click the desktop icon; without a guard
//! you get two elevated processes writing clocks and voltages to the same card, each unaware of the
//! other's applies. The check runs before the elevation prompt, so a redundant launch costs no UAC
//! dialog.
//!
//! Two integrity levels are in play: the logon instance is elevated (the task asks for the highest
//! privileges), while a launch from the shell starts medium and only elevates afterwards. A medium
//! process cannot open a high-integrity object for modify, so "access denied" here means the mutex
//! exists and belongs to a more privileged copy of us — which is a yes, not an error.

/// `Global\` so the logon copy and a shell launch see the same objects.
pub const MUTEX_NAME: &str = "Global\\RochGPU.SingleInstance";
pub const SHOW_EVENT_NAME: &str = "Global\\RochGPU.Show";

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicIsize, Ordering};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, MUTEX_MODIFY_STATE, OpenEventW, OpenMutexW,
        SYNCHRONIZATION_SYNCHRONIZE, SetEvent, WaitForSingleObject,
    };

    /// The claimed mutex, held (never closed) for the life of the process.
    static HELD: AtomicIsize = AtomicIsize::new(0);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn claim() -> bool {
        let name = wide(super::MUTEX_NAME);
        // SAFETY: `name` is NUL-terminated; the handle is either kept or closed.
        unsafe {
            let h = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
            if h.is_null() {
                return false; // access denied: an elevated copy holds it
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(h);
                return false;
            }
            HELD.store(h as isize, Ordering::SeqCst);
            true
        }
    }

    pub fn already_running() -> bool {
        let name = wide(super::MUTEX_NAME);
        // SAFETY: `name` is NUL-terminated; an opened handle is closed at once.
        unsafe {
            let h = OpenMutexW(MUTEX_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr());
            if !h.is_null() {
                CloseHandle(h);
                return true;
            }
            GetLastError() == ERROR_ACCESS_DENIED
        }
    }

    pub fn ask_existing_to_show() {
        let name = wide(super::SHOW_EVENT_NAME);
        // SAFETY: as above. Best effort: if the running copy is elevated and we are not, the event
        // cannot be opened for modify, and this copy simply steps aside.
        unsafe {
            let h = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
            if !h.is_null() {
                SetEvent(h);
                CloseHandle(h);
            }
        }
    }

    pub fn listen_for_show_requests(show: Box<dyn Fn() + Send>) {
        let name = wide(super::SHOW_EVENT_NAME);
        // SAFETY: auto-reset event, never closed while the listener thread can wait on it.
        let h = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
        if h.is_null() {
            return;
        }
        let raw = h as isize;
        let _ = std::thread::Builder::new().name("RochGPU show listener".into()).spawn(move || {
            let h = raw as HANDLE;
            // SAFETY: the handle stays open for the process's life.
            while unsafe { WaitForSingleObject(h, INFINITE) } == WAIT_OBJECT_0 {
                show();
            }
        });
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn claim() -> bool {
        true
    }
    pub fn already_running() -> bool {
        false
    }
    pub fn ask_existing_to_show() {}
    pub fn listen_for_show_requests(_show: Box<dyn Fn() + Send>) {}
}

/// True when this process is the first one. Keeps the mutex for the process's life.
pub fn claim() -> bool {
    imp::claim()
}

/// Is one already running? Asked before elevating, so it must survive being refused.
pub fn already_running() -> bool {
    imp::already_running()
}

/// Ask the running copy to show itself (best effort).
pub fn ask_existing_to_show() {
    imp::ask_existing_to_show()
}

/// Listen for a later launch asking us to come back from the tray. `show` runs on a background
/// thread; marshal to the UI thread from it.
pub fn listen_for_show_requests(show: impl Fn() + Send + 'static) {
    imp::listen_for_show_requests(Box::new(show))
}
