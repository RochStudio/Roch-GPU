//! The few platform calls the window needs directly: a native message box for the moments before
//! (or after) the egui window exists, and something to open links and the log file with.
//!
//! Window placement and opacity are Win32-only. Wayland leaves position to the compositor and gives a
//! client neither of them, so on Linux those are no-ops that report success.

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

/// A dialog from whichever desktop helper is installed, for the same moments: there is no toolkit in
/// this binary to draw one before the window exists. Failing those a desktop notification, which
/// cannot ask a question, so a Yes/No without a helper is a No. The terminal gets it either way.
#[cfg(not(windows))]
pub fn message_box(text: &str, title: &str, buttons: Buttons, icon: Icon) -> bool {
    use std::process::{Command, Stdio};
    eprintln!("{title}: {text}");
    let zenity_kind = match (buttons, icon) {
        (Buttons::YesNo, _) => "--question",
        (Buttons::Ok, Icon::Warning) => "--warning",
        (Buttons::Ok, Icon::Error) => "--error",
    };
    let kdialog_kind = match (buttons, icon) {
        (Buttons::YesNo, _) => "--warningyesno",
        (Buttons::Ok, Icon::Warning) => "--sorry",
        (Buttons::Ok, Icon::Error) => "--error",
    };
    let silent = |c: &mut Command| {
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    };
    let mut zenity = Command::new("zenity");
    zenity.args([zenity_kind, "--no-markup", "--title", title, "--text", text]);
    silent(&mut zenity);
    if let Ok(status) = zenity.status() {
        return status.success();
    }
    let mut kdialog = Command::new("kdialog");
    kdialog.args(["--title", title, kdialog_kind, text]);
    silent(&mut kdialog);
    if let Ok(status) = kdialog.status() {
        return status.success();
    }
    let mut notify = Command::new("notify-send");
    notify.args(["--app-name=Roch GPU", title, text]);
    silent(&mut notify);
    let _ = notify.status();
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
    use std::process::{Command, Stdio};
    let mut child = Command::new("xdg-open")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("xdg-open failed: {e}"))?;
    // Reaped on the side: it returns quickly, but this must not hold up the window while it does.
    std::thread::spawn(move || child.wait());
    Ok(())
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

#[cfg(windows)]
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

/// The desktop's light or dark preference, from the GNOME setting that most desktops keep in step
/// with their own; a desktop with neither falls back to the app's own default (not dark).
#[cfg(not(windows))]
pub fn system_dark() -> bool {
    let scheme = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).to_lowercase());
    match scheme {
        Some(scheme) => scheme.contains("dark"),
        None => std::env::var("GTK_THEME").is_ok_and(|t| t.to_lowercase().contains("dark")),
    }
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

/// The special workspace the windows go to on Hyprland. It is the one Omarchy's Super+S shows.
#[cfg(not(windows))]
const SCRATCHPAD: &str = "special:scratchpad";

/// Run `hyprctl`, on Hyprland only. `None` anywhere else, and if it fails.
#[cfg(not(windows))]
fn hyprctl(args: &[&str]) -> Option<String> {
    use std::process::{Command, Stdio};
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let out = Command::new("hyprctl").args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A dispatch, in the syntax of Lua-configured Hyprland or, failing that, of the versions before it.
#[cfg(not(windows))]
fn dispatch(lua: &str, classic: &[&str]) -> bool {
    let accepted = |args: &[&str]| hyprctl(args).is_some_and(|out| out.trim() == "ok");
    accepted(&["dispatch", lua]) || accepted(&[&["dispatch"], classic].concat())
}

/// The (address, workspace name) of each of this process's windows in `hyprctl clients -j`.
#[cfg(not(windows))]
fn windows_of(clients: &str, pid: u32) -> Vec<(String, String)> {
    let clients: serde_json::Value = serde_json::from_str(clients).unwrap_or_default();
    let is_address = |a: &str| a.strip_prefix("0x").is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()));
    clients
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["pid"].as_u64() == Some(u64::from(pid)))
        .filter_map(|c| {
            let address = c["address"].as_str().filter(|a| is_address(a))?;
            Some((address.to_string(), c["workspace"]["name"].as_str().unwrap_or("").to_string()))
        })
        .collect()
}

#[cfg(not(windows))]
fn my_windows() -> Vec<(String, String)> {
    hyprctl(&["clients", "-j"]).map_or_else(Vec::new, |json| windows_of(&json, std::process::id()))
}

#[cfg(not(windows))]
fn move_window(address: &str, workspace: &str) -> bool {
    dispatch(
        &format!(
            "hl.dsp.window.move({{ window = \"address:{address}\", workspace = \"{workspace}\", follow = false }})"
        ),
        &["movetoworkspacesilent", &format!("{workspace},address:{address}")],
    )
}

/// The special workspaces the monitors have open, from `hyprctl monitors -j`.
#[cfg(not(windows))]
fn open_special_workspaces(monitors: &str) -> Vec<String> {
    let monitors: serde_json::Value = serde_json::from_str(monitors).unwrap_or_default();
    monitors
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|monitor| monitor["specialWorkspace"]["name"].as_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(not(windows))]
fn open_special_workspaces_now() -> Vec<String> {
    hyprctl(&["monitors", "-j"]).map_or_else(Vec::new, |json| open_special_workspaces(&json))
}

/// Whether one of these windows can be seen: on an ordinary workspace, or a special one that is open.
#[cfg(not(windows))]
fn any_in_view(windows: &[(String, String)], open: &[String]) -> bool {
    windows.iter().any(|(_, workspace)| !workspace.starts_with("special:") || open.contains(workspace))
}

/// Whether this desktop has a scratchpad to hide windows on. Hyprland has no minimized state for a
/// window to go to; every other desktop does, and minimizing works as it should there.
#[cfg(not(windows))]
pub fn can_hide_in_scratchpad() -> bool {
    hyprctl(&["clients", "-j"]).is_some()
}

/// Hide this program's windows on the scratchpad. False if they could not all be hidden.
#[cfg(not(windows))]
pub fn hide_in_scratchpad() -> bool {
    let windows = my_windows();
    let mut hidden = !windows.is_empty();
    for (address, workspace) in &windows {
        // Already there, if the scratchpad is how they came to be on screen.
        if workspace != SCRATCHPAD {
            hidden &= move_window(address, SCRATCHPAD);
        }
    }
    // An open scratchpad would just show them: it has to close, as the key that opened it would.
    if hidden && open_special_workspaces_now().iter().any(|open| open == SCRATCHPAD) {
        hidden = dispatch("hl.dsp.workspace.toggle_special(\"scratchpad\")", &["togglespecialworkspace", "scratchpad"]);
    }
    hidden
}

/// Whether windows that were hidden can be seen again.
#[cfg(not(windows))]
pub fn back_in_view() -> bool {
    any_in_view(&my_windows(), &open_special_workspaces_now())
}

/// Have Hyprland keep giving this program's windows frames while they are out of sight. It normally
/// stops, and every child window draws during the main window's frame, so a Telemetry window on screen
/// would stop updating once the main window was on another workspace or behind a fullscreen game. It
/// costs nothing while idle: a frame only comes after a window has drawn something and asked for one.
#[cfg(not(windows))]
pub fn keep_frames_coming() {
    for (address, _) in my_windows() {
        dispatch(
            &format!(
                "hl.dsp.window.set_prop({{ window = \"address:{address}\", prop = \"render_unfocused\", value = \"1\" }})"
            ),
            &["setprop", &format!("address:{address}"), "render_unfocused", "1"],
        );
    }
}

/// Bring back whatever is hidden on a special workspace, to the workspace in view. Safe from any
/// thread, and not left to the UI thread on purpose: a window out of sight gets no frame callbacks, so
/// the UI thread can sit waiting for one until something else brings the window back.
#[cfg(not(windows))]
pub fn bring_back_from_scratchpad() {
    let windows = my_windows();
    let hidden: Vec<&String> =
        windows.iter().filter(|(_, workspace)| workspace.starts_with("special:")).map(|(address, _)| address).collect();
    if hidden.is_empty() {
        return;
    }
    let active = hyprctl(&["activeworkspace", "-j"])
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|workspace| workspace["id"].as_i64());
    let Some(active) = active else { return };
    for address in &hidden {
        move_window(address, &active.to_string());
    }
    if let Some(address) = hidden.first() {
        dispatch(
            &format!("hl.dsp.focus({{ window = \"address:{address}\" }})"),
            &["focuswindow", &format!("address:{address}")],
        );
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    /// The parts of real `hyprctl clients -j` output this reads.
    const CLIENTS: &str = r#"[
        {"address": "0x564b499fd7f0", "pid": 100, "class": "RochGPU", "workspace": {"id": 1, "name": "1"}},
        {"address": "0x564b499fe000", "pid": 100, "class": "RochGPU", "workspace": {"id": -98, "name": "special:scratchpad"}},
        {"address": "0x564b49900000", "pid": 200, "class": "chromium", "workspace": {"id": 1, "name": "1"}},
        {"address": "not an address", "pid": 100, "workspace": {"id": 1, "name": "1"}}
    ]"#;

    #[test]
    fn only_this_programs_windows_are_ever_moved() {
        assert_eq!(
            windows_of(CLIENTS, 100),
            vec![
                ("0x564b499fd7f0".to_string(), "1".to_string()),
                ("0x564b499fe000".to_string(), "special:scratchpad".to_string()),
            ]
        );
        assert_eq!(windows_of(CLIENTS, 300), vec![]);
    }

    #[test]
    fn unreadable_output_means_no_windows() {
        assert_eq!(windows_of("", 100), vec![]);
        assert_eq!(windows_of("{\"not\": \"a list\"}", 100), vec![]);
    }

    #[test]
    fn a_monitor_with_no_special_workspace_open_has_an_empty_name() {
        // The parts of real `hyprctl monitors -j` output this reads.
        let monitors = r#"[
            {"name": "DP-1", "specialWorkspace": {"id": 0, "name": ""}},
            {"name": "DP-2", "specialWorkspace": {"id": -98, "name": "special:scratchpad"}}
        ]"#;
        assert_eq!(open_special_workspaces(monitors), vec!["special:scratchpad".to_string()]);
        assert_eq!(open_special_workspaces(""), Vec::<String>::new());
    }

    #[test]
    fn a_window_is_out_of_sight_only_on_a_special_workspace_that_is_closed() {
        let on = |workspace: &str| vec![("0x1".to_string(), workspace.to_string())];
        let scratchpad = vec!["special:scratchpad".to_string()];
        assert!(!any_in_view(&on("special:scratchpad"), &[]));
        assert!(any_in_view(&on("special:scratchpad"), &scratchpad));
        assert!(!any_in_view(&on("special:other"), &scratchpad));
        assert!(any_in_view(&on("1"), &[]));
        assert!(!any_in_view(&[], &scratchpad), "no windows is not back in view");
    }
}
