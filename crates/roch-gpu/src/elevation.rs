//! Elevation for the window half.
//!
//! The window writes clocks, so it wants elevation — but asking for it in the manifest would break
//! the other half: a manifest-elevated process is launched fresh and does not inherit the caller's
//! console, so `RochGPU.exe info` would print into a console nobody can see. Elevating here, and
//! only for the window, keeps both halves usable.

/// True when this process runs with an elevated (administrator) token.
#[cfg(windows)]
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: the token handle is closed on every path; the buffer is a TOKEN_ELEVATION of the
    // size passed.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    true
}

/// The argument string for the relaunch: arguments containing a space are quoted, the rest pass
/// through (the C# joined them the same way).
pub fn join_args(args: &[String]) -> String {
    args.iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() }).collect::<Vec<_>>().join(" ")
}

/// Start an elevated copy with the same arguments. True when it was started and this one should
/// quit; false when the prompt was declined, in which case running on unelevated beats exiting
/// silently: the monitor still works, and every write fails with a message that says why.
#[cfg(windows)]
pub fn relaunch_elevated(args: &[String]) -> bool {
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let Ok(exe) = std::env::current_exe() else { return false };
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let file = wide(&exe.to_string_lossy());
    let params = wide(&join_args(args));
    let verb = wide("runas");
    // SAFETY: every pointer refers to a NUL-terminated buffer alive for the call; the struct is
    // zero-initialised as the API expects for unused members.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        // This process exits right after the call returns.
        info.fMask = SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_SHOWNORMAL;
        ShellExecuteExW(&mut info) != 0
    }
}

#[cfg(not(windows))]
pub fn relaunch_elevated(_args: &[String]) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaunch_quotes_only_spaced_arguments() {
        let args = vec!["--apply-profile".to_string(), "Slot 1".into(), "--minimized".into()];
        assert_eq!(join_args(&args), "--apply-profile \"Slot 1\" --minimized");
        assert_eq!(join_args(&[]), "");
    }
}
