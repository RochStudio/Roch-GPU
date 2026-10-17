//! The few Win32 calls the window needs directly: a native message box for the moments before
//! (or after) the egui window exists, and ShellExecute for links and the log file.

/// Buttons of a [`message_box`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Buttons {
    Ok,
    YesNo,
}

/// Icon of a [`message_box`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Warning,
    Error,
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A modal native message box with no owner. Returns true for Yes/OK.
#[cfg(windows)]
pub fn message_box(text: &str, title: &str, buttons: Buttons, icon: Icon) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDOK, IDYES, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_YESNO, MessageBoxW,
    };
    let (t, c) = (wide(text), wide(title));
    let flags = match buttons {
        Buttons::Ok => MB_OK,
        Buttons::YesNo => MB_YESNO,
    } | match icon {
        Icon::Warning => MB_ICONWARNING,
        Icon::Error => MB_ICONERROR,
    } | MB_SETFOREGROUND;
    // SAFETY: both strings are NUL-terminated and outlive the call; a null owner is allowed.
    let r = unsafe { MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), flags) };
    r == IDYES || r == IDOK
}

#[cfg(not(windows))]
pub fn message_box(text: &str, title: &str, _buttons: Buttons, _icon: Icon) -> bool {
    eprintln!("{title}: {text}");
    false
}

/// Open a URL or a file with its associated program (C# `UseShellExecute = true`).
#[cfg(windows)]
pub fn shell_open(target: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let (verb, file) = (wide("open"), wide(target));
    // SAFETY: NUL-terminated buffers alive for the call; null parameters/directory are allowed.
    let h = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute reports success as a value greater than 32.
    if h as isize > 32 { Ok(()) } else { Err(format!("ShellExecute failed ({})", h as isize)) }
}

#[cfg(not(windows))]
pub fn shell_open(target: &str) -> Result<(), String> {
    Err(format!("cannot open {target} on this platform"))
}
