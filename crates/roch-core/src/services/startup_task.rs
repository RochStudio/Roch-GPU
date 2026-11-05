//! Runs the app at logon to apply a confirmed profile.
//!
//! On Windows that is a Task Scheduler entry, created with schtasks.exe so no COM interop is needed,
//! which runs the app elevated with `--apply-profile <name> --minimized`: apply the profile, then sit
//! in the tray. Always resident rather than apply-and-quit, which is what a *software* fan curve
//! needs: nothing steps it if no process is left alive.
//!
//! On Linux it is an XDG autostart entry, which GNOME, KDE and the systemd generator that uwsm sessions
//! use all honour, running `--apply-profile <name> --exit`. It applies and quits, because Wayland
//! offers no way to sit hidden: a resident copy would be a window on screen at every login. The
//! firmware runs its own fan curve on the cards that have one, so only a software curve on an older
//! card is lost, and that one needs the window open.

use crate::Result;

/// The Windows task's name.
pub const TASK_NAME: &str = "Roch GPU Apply Profile";

pub fn is_supported() -> bool {
    cfg!(any(windows, target_os = "linux"))
}

pub use imp::{exists, register, unregister};

#[cfg(windows)]
mod imp {
    use std::process::Command;

    use super::{Result, TASK_NAME};
    use crate::GpuError;

    pub fn exists() -> bool {
        run(&["/Query", "/TN", TASK_NAME]).0 == 0
    }

    /// Create or replace the task. `exe_path` must be the full path to the executable.
    pub fn register(exe_path: &str, profile_name: &str) -> Result<()> {
        // Delete first rather than leaning on /Create /F: Windows keeps the name a task was first
        // created with, so an old spelling would survive every rewrite.
        if exists() {
            run(&["/Delete", "/F", "/TN", TASK_NAME]);
        }
        // /RL HIGHEST = run elevated; /SC ONLOGON; /DELAY gives the driver a few seconds to settle.
        let tr = format!("\"{exe_path}\" --apply-profile \"{profile_name}\" --minimized");
        let (code, output) = run(&[
            "/Create", "/F", "/TN", TASK_NAME, "/SC", "ONLOGON", "/RL", "HIGHEST", "/DELAY", "0000:15", "/TR", &tr,
        ]);
        if code != 0 {
            return Err(GpuError::new(format!("schtasks failed: {output}")));
        }
        Ok(())
    }

    /// Remove the task. Deleting one that was never registered is success — but a delete that
    /// reports success while the task survives is not, so the outcome is verified.
    pub fn unregister() -> Result<()> {
        if !exists() {
            return Ok(());
        }
        let (code, output) = run(&["/Delete", "/F", "/TN", TASK_NAME]);
        if exists() {
            return Err(GpuError::new(format!("schtasks could not remove the task (exit {code}): {output}")));
        }
        Ok(())
    }

    fn run(args: &[&str]) -> (i32, String) {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = Command::new("schtasks");
        cmd.args(args).creation_flags(CREATE_NO_WINDOW);
        match cmd.output() {
            Ok(out) => {
                let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
                (out.status.code().unwrap_or(-1), text)
            }
            Err(e) => (-1, e.to_string()),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::Result;
    use crate::GpuError;
    use crate::services::profile_store::storage_root;

    fn entry_path() -> PathBuf {
        storage_root::config_home().join("autostart").join("roch-gpu-apply-profile.desktop")
    }

    /// One argument of an `Exec=` line, under the Desktop Entry spec's two layers of escaping: the
    /// quoting of the command line, then the backslash escapes of any string value. So a literal `$`
    /// reaches the file as `\\$`, and a literal backslash as four of them. `%` starts a field code.
    pub(super) fn exec_argument(argument: &str) -> String {
        let mut quoted = String::from("\"");
        for c in argument.chars() {
            match c {
                '"' | '`' | '$' => {
                    quoted.push_str("\\\\");
                    quoted.push(c);
                }
                '\\' => quoted.push_str("\\\\\\\\"),
                '%' => quoted.push_str("%%"),
                '\n' | '\r' => quoted.push(' '),
                _ => quoted.push(c),
            }
        }
        quoted.push('"');
        quoted
    }

    pub(super) fn desktop_entry(exe_path: &str, profile_name: &str) -> String {
        // The running copy's path gains this once an install has replaced the file under it. The entry
        // has to name what the next login can start.
        let exe_path = exe_path.strip_suffix(" (deleted)").unwrap_or(exe_path);
        format!(
            "[Desktop Entry]\nType=Application\nName=Roch GPU apply profile\n\
             Comment=Applies the Roch GPU startup profile at logon, then exits\n\
             Exec={} --apply-profile {} --exit\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
            exec_argument(exe_path),
            exec_argument(profile_name)
        )
    }

    pub(super) fn write_entry(path: &Path, exe_path: &str, profile_name: &str) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
            storage_root::restore_owner(dir);
        }
        fs::write(path, desktop_entry(exe_path, profile_name))?;
        storage_root::restore_owner(path);
        Ok(())
    }

    pub fn exists() -> bool {
        entry_path().is_file()
    }

    /// Create or replace the entry. `exe_path` must be the full path to the executable.
    pub fn register(exe_path: &str, profile_name: &str) -> Result<()> {
        write_entry(&entry_path(), exe_path, profile_name)
    }

    /// Remove the entry. Removing one that was never registered is success.
    pub fn unregister() -> Result<()> {
        let path = entry_path();
        if path.exists() {
            fs::remove_file(&path).map_err(|e| GpuError::context("could not remove the startup entry", e))?;
        }
        Ok(())
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::imp::*;

    #[test]
    fn the_entry_applies_and_exits() {
        let entry = desktop_entry("/home/me/.local/bin/rochgpu", "Slot 1");
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(
            entry.contains("\nExec=\"/home/me/.local/bin/rochgpu\" --apply-profile \"Slot 1\" --exit\n"),
            "{entry}"
        );
        assert!(entry.contains("\nType=Application\n"));
    }

    #[test]
    fn a_program_replaced_while_running_is_named_by_its_real_path() {
        let entry = desktop_entry("/home/me/.local/bin/rochgpu (deleted)", "Slot 1");
        assert!(entry.contains("\nExec=\"/home/me/.local/bin/rochgpu\" --apply-profile"), "{entry}");
    }

    #[test]
    fn awkward_names_stay_one_argument() {
        // `%` would be a field code; the shell-ish characters are escaped twice over, once for the
        // command line and once for the string value that holds it.
        assert_eq!(exec_argument("100%"), r#""100%%""#);
        assert_eq!(exec_argument("a$b"), r#""a\\$b""#);
        assert_eq!(exec_argument("a\"b"), r#""a\\"b""#);
        assert_eq!(exec_argument("a`b"), r#""a\\`b""#);
        assert_eq!(exec_argument("a\\b"), r#""a\\\\b""#);
        assert_eq!(exec_argument("two\nlines"), r#""two lines""#, "a newline would end the Exec line");
    }

    #[test]
    fn writing_makes_the_folder_and_replaces_the_old_entry() {
        let dir = std::env::temp_dir().join(format!("roch-startup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("autostart/entry.desktop");
        write_entry(&path, "/x/rochgpu", "One").unwrap();
        write_entry(&path, "/x/rochgpu", "Two").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"Two\"") && !text.contains("\"One\""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
