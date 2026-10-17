//! Registers a Windows Task Scheduler entry that runs the app elevated at logon with
//! `--apply-profile <name> --minimized`: apply the profile, then sit in the tray. Uses
//! schtasks.exe, so no COM interop is needed.
//!
//! Always resident rather than apply-and-quit, which is what a *software* fan curve needs:
//! nothing steps it if no process is left alive.

use std::process::Command;

use crate::{GpuError, Result};

pub const TASK_NAME: &str = "Roch GPU Apply Profile";

pub fn is_windows() -> bool {
    cfg!(windows)
}

pub fn exists() -> bool {
    if !is_windows() {
        return false;
    }
    run(&["/Query", "/TN", TASK_NAME]).0 == 0
}

/// Create or replace the task. `exe_path` must be the full path to the executable.
pub fn register(exe_path: &str, profile_name: &str) -> Result<()> {
    if !is_windows() {
        return Err(GpuError::new("Startup tasks are only supported on Windows."));
    }
    // Delete first rather than leaning on /Create /F: Windows keeps the name a task was first
    // created with, so an old spelling would survive every rewrite.
    if exists() {
        run(&["/Delete", "/F", "/TN", TASK_NAME]);
    }
    // /RL HIGHEST = run elevated; /SC ONLOGON; /DELAY gives the driver a few seconds to settle.
    let tr = format!("\"{exe_path}\" --apply-profile \"{profile_name}\" --minimized");
    let (code, output) =
        run(&["/Create", "/F", "/TN", TASK_NAME, "/SC", "ONLOGON", "/RL", "HIGHEST", "/DELAY", "0000:15", "/TR", &tr]);
    if code != 0 {
        return Err(GpuError::new(format!("schtasks failed: {output}")));
    }
    Ok(())
}

/// Remove the task. Deleting one that was never registered is success — but a delete that
/// reports success while the task survives is not, so the outcome is verified.
pub fn unregister() -> Result<()> {
    if !is_windows() || !exists() {
        return Ok(());
    }
    let (code, output) = run(&["/Delete", "/F", "/TN", TASK_NAME]);
    if exists() {
        return Err(GpuError::new(format!("schtasks could not remove the task (exit {code}): {output}")));
    }
    Ok(())
}

fn run(args: &[&str]) -> (i32, String) {
    let mut cmd = Command::new("schtasks");
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.output() {
        Ok(out) => {
            let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            (out.status.code().unwrap_or(-1), text)
        }
        Err(e) => (-1, e.to_string()),
    }
}
