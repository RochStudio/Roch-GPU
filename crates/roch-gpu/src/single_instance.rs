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
#[cfg(windows)]
pub const MUTEX_NAME: &str = "Global\\RochGPU.SingleInstance";
#[cfg(windows)]
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

/// A Unix socket stands in for both the mutex and the event: something listening on it means a copy
/// is running, and writing to it asks that copy to come forward. Unlike a lock file, a socket left
/// behind by a crash answers nobody, so it cannot keep the next launch out.
#[cfg(not(windows))]
mod imp {
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::Duration;

    /// Bound by `claim`, handed to the listener thread later.
    static LISTENER: Mutex<Option<UnixListener>> = Mutex::new(None);

    const SHOW: &[u8; 4] = b"show";

    fn socket_path() -> PathBuf {
        match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) => PathBuf::from(dir).join("roch-gpu.sock"),
            // No per-user directory (a service, or sudo), so the number keeps users apart.
            None => {
                use std::os::unix::fs::MetadataExt;
                let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0);
                std::env::temp_dir().join(format!("roch-gpu-{uid}.sock"))
            }
        }
    }

    pub(super) fn bind(path: &Path) -> Option<UnixListener> {
        // A stale socket from a crashed run refuses the connection and is safe to replace.
        let _ = std::fs::remove_file(path);
        UnixListener::bind(path).ok()
    }

    pub fn claim() -> bool {
        let path = socket_path();
        // Binding comes first because only one of two launches at the same moment can win it. Removing
        // the file is for when that fails and nothing answers: what a crashed run left behind.
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => Some(listener),
            Err(_) if UnixStream::connect(&path).is_ok() => return false,
            Err(_) => bind(&path),
        };
        // Failing to make the socket is no reason to refuse to run: it only loses the second-launch
        // handshake, the same as on a machine without one.
        if let (Some(listener), Ok(mut held)) = (listener, LISTENER.lock()) {
            *held = Some(listener);
        }
        true
    }

    pub fn already_running() -> bool {
        UnixStream::connect(socket_path()).is_ok()
    }

    pub fn ask_existing_to_show() {
        if let Ok(mut stream) = UnixStream::connect(socket_path()) {
            let _ = stream.write_all(SHOW);
        }
    }

    /// Serve the socket until the process ends. `already_running` connects too, and says nothing, so
    /// only the word itself brings the window forward.
    pub(super) fn serve(listener: UnixListener, show: Box<dyn Fn() + Send>) {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            let mut word = [0u8; 4];
            if stream.read_exact(&mut word).is_ok() && &word == SHOW {
                show();
            }
        }
    }

    pub fn listen_for_show_requests(show: Box<dyn Fn() + Send>) {
        let Some(listener) = LISTENER.lock().ok().and_then(|mut held| held.take()) else { return };
        let _ = std::thread::Builder::new().name("RochGPU show listener".into()).spawn(move || serve(listener, show));
    }
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

#[cfg(all(test, not(windows)))]
mod tests {
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    use super::imp;

    #[test]
    fn only_the_word_brings_a_running_copy_forward_and_a_stale_socket_is_replaced() {
        let path = std::env::temp_dir().join(format!("roch-gpu-test-{}.sock", std::process::id()));
        // What a crashed run leaves behind: a socket file nothing is listening on.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists() && UnixStream::connect(&path).is_err());

        let listener = imp::bind(&path).unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || imp::serve(listener, Box::new(move || tx.send(()).unwrap())));

        // Asking whether a copy is running does not count as asking it to show itself.
        drop(UnixStream::connect(&path).unwrap());
        UnixStream::connect(&path).unwrap().write_all(b"nope").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(400)).is_err());

        UnixStream::connect(&path).unwrap().write_all(b"show").unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
        let _ = std::fs::remove_file(&path);
    }
}
