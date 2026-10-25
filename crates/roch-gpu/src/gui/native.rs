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

/// Match windows on the calling UI thread. GetWindowText sends WM_GETTEXT to windows
/// in our process, so enumerating driver/helper threads here can block the entire UI.
#[cfg(windows)]
fn own_window(title: &str) -> windows_sys::Win32::Foundation::HWND {
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumThreadWindows, GetWindowTextW};
    struct Search<'a> {
        title: &'a str,
        found: windows_sys::Win32::Foundation::HWND,
    }
    unsafe extern "system" fn visit(window: windows_sys::Win32::Foundation::HWND, data: isize) -> i32 {
        // SAFETY: EnumThreadWindows is synchronous and data points to our live Search.
        let search = unsafe { &mut *(data as *mut Search<'_>) };
        let mut text = [0u16; 256];
        let count = unsafe { GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32) };
        if count > 0 && String::from_utf16_lossy(&text[..count as usize]) == search.title {
            search.found = window;
            return 0;
        }
        1
    }
    let mut search = Search { title, found: std::ptr::null_mut() };
    // SAFETY: all enumerated windows belong to this UI thread, including immediate viewports.
    unsafe {
        EnumThreadWindows(GetCurrentThreadId(), Some(visit), &mut search as *mut Search<'_> as isize);
    }
    search.found
}

pub fn system_dark() -> bool {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            winreg::enums::KEY_READ,
        )
        .ok()
        .and_then(|k| k.get_value::<u32, _>("AppsUseLightTheme").ok())
        .is_some_and(|v| v == 0)
}

#[cfg(windows)]
pub fn window_position(title: &str) -> Option<(i32, i32)> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsIconic};
    let window = own_window(title);
    let mut rect = windows_sys::Win32::Foundation::RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: queries on a window verified to belong to our process.
    unsafe {
        if window.is_null() || IsIconic(window) != 0 || GetWindowRect(window, &mut rect) == 0 {
            return None;
        }
    }
    Some((rect.left, rect.top))
}

#[cfg(not(windows))]
pub fn window_position(_: &str) -> Option<(i32, i32)> {
    None
}

/// Keep resized or restored viewports inside their monitor's usable desktop.
#[cfg(windows)]
fn keep_on_screen(title: &str) -> bool {
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, IsIconic, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
    };
    let window = own_window(title);
    // SAFETY: all queries and the optional move target only a window owned by this process.
    unsafe {
        if window.is_null() || IsIconic(window) != 0 {
            return false;
        }
        let mut rect = std::mem::zeroed();
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetWindowRect(window, &mut rect) == 0
            || GetMonitorInfoW(MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST), &mut info) == 0
        {
            return false;
        }
        let work = info.rcWork;
        let x = rect.left.clamp(work.left, (work.right - (rect.right - rect.left)).max(work.left));
        let y = rect.top.clamp(work.top, (work.bottom - (rect.bottom - rect.top)).max(work.top));
        if x != rect.left || y != rect.top {
            SetWindowPos(window, std::ptr::null_mut(), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
    true
}

#[cfg(not(windows))]
fn keep_on_screen(_: &str) -> bool {
    true
}

#[cfg(windows)]
pub fn position_points(at: (i32, i32), zoom: f32) -> Option<eframe::egui::Pos2> {
    use windows_sys::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint};
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    let point = windows_sys::Win32::Foundation::POINT { x: at.0.saturating_add(40), y: at.1.saturating_add(10) };
    // SAFETY: pure monitor/DPI queries; an unplugged monitor is not restored.
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONULL);
        if monitor.is_null() {
            return None;
        }
        let (mut x, mut y) = (96, 96);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) < 0 {
            x = 96;
        }
        let scale = (x.max(1) as f32 / 96.0) * zoom.max(0.1);
        Some(eframe::egui::pos2(at.0 as f32 / scale, at.1 as f32 / scale))
    }
}

#[cfg(not(windows))]
pub fn position_points(_: (i32, i32), _: f32) -> Option<eframe::egui::Pos2> {
    None
}

/// Apply transparency to our own viewport; no driver or other process is involved.
#[cfg(windows)]
fn apply_opacity(title: &str, opacity: f32) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetLayeredWindowAttributes, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes,
        SetWindowLongPtrW, WS_EX_LAYERED,
    };
    let window = own_window(title);
    if window.is_null() {
        return false;
    }
    let alpha = (opacity.clamp(0.3, 1.0) * 255.0).round() as u8;
    // SAFETY: style and alpha changes on a window owned by this process.
    unsafe {
        let style = GetWindowLongPtrW(window, GWL_EXSTYLE);
        let layered = style & WS_EX_LAYERED as isize != 0;
        if alpha == 255 {
            if layered {
                SetWindowLongPtrW(window, GWL_EXSTYLE, style & !(WS_EX_LAYERED as isize));
            }
            return true;
        }
        if layered {
            let (mut key, mut previous, mut flags) = (0, 0, 0);
            if GetLayeredWindowAttributes(window, &mut key, &mut previous, &mut flags) != 0
                && previous == alpha
                && flags == LWA_ALPHA
            {
                return true;
            }
        } else {
            SetWindowLongPtrW(window, GWL_EXSTYLE, style | WS_EX_LAYERED as isize);
        }
        SetLayeredWindowAttributes(window, 0, alpha, LWA_ALPHA);
    }
    true
}

#[cfg(not(windows))]
fn apply_opacity(_: &str, _: f32) -> bool {
    true
}

/// Apply native effects only when their inputs change. Position is deliberately excluded:
/// dragging a window must not fight a SetWindowPos call on every frame.
#[derive(Debug, Default)]
pub struct WindowMaintenance {
    bounds: Option<(eframe::egui::Vec2, f32, Option<eframe::egui::Vec2>)>,
    alpha: Option<u8>,
}

impl WindowMaintenance {
    pub fn update(&mut self, ctx: &eframe::egui::Context, title: &str, opacity: f32) {
        let pixels_per_point = ctx.pixels_per_point();
        let bounds =
            ctx.input(|i| i.viewport().outer_rect.map(|r| (r.size(), pixels_per_point, i.viewport().monitor_size)));
        if bounds.is_some() && bounds != self.bounds && keep_on_screen(title) {
            self.bounds = bounds;
        }
        let alpha = (opacity.clamp(0.3, 1.0) * 255.0).round() as u8;
        if self.alpha != Some(alpha) && apply_opacity(title, opacity) {
            self.alpha = Some(alpha);
        }
    }
}
