//! The kernel's attribute files: read them, write them, and explain a refused write.

use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

use roch_core::GpuError;

/// The command that runs `rochgpu setup`, spelled so it works from here. sudo searches its own short
/// `secure_path`, which on most distributions leaves out `~/.local/bin` where this usually lives, so a
/// bare `sudo rochgpu` would only answer "command not found".
pub fn setup_command(flags: &str) -> String {
    const ON_SUDO_PATH: [&str; 5] = ["/usr/local/sbin", "/usr/local/bin", "/usr/sbin", "/usr/bin", "/bin"];
    let exe = std::env::current_exe().ok();
    let program = match &exe {
        Some(path) if !path.parent().is_some_and(|dir| ON_SUDO_PATH.iter().any(|d| dir == Path::new(d))) => {
            let path = path.display().to_string();
            if path.contains(' ') { format!("'{path}'") } else { path }
        }
        _ => "rochgpu".to_string(),
    };
    format!("sudo {program} setup{flags}")
}

/// What to do about a refused write. Named in one place because every control can hit it.
pub fn permission_hint() -> String {
    format!("Run `{}` once to allow this user, or run this command with sudo.", setup_command(""))
}

/// Where a write goes. A real run writes the attribute; the tests put a simulated kernel here,
/// because a plain file cannot refuse a value or hold a change back until a commit.
pub trait AttrWriter: Send {
    fn write(&mut self, path: &Path, value: &str) -> io::Result<()>;
}

/// The real thing.
pub struct KernelWriter;

impl AttrWriter for KernelWriter {
    fn write(&mut self, path: &Path, value: &str) -> io::Result<()> {
        // Opened without create or truncate: a missing attribute is an error rather than a new
        // file, and the driver takes each write() as one command.
        let mut file = fs::OpenOptions::new().write(true).open(path)?;
        file.write_all(value.as_bytes())
    }
}

/// The attribute's text with its trailing newline gone. `None` covers everything a read can say: not
/// there on this card, or refused because the card is asleep (the driver answers EPERM rather than
/// waking it).
pub fn read_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

pub fn read_i64(path: &Path) -> Option<i64> {
    read_text(path)?.parse().ok()
}

/// A hexadecimal attribute such as `vendor`: `0x1002`.
pub fn read_hex(path: &Path) -> Option<u32> {
    let text = read_text(path)?;
    u32::from_str_radix(text.trim_start_matches("0x"), 16).ok()
}

/// Whether this process may write the attribute. Opening it does nothing to the card; only a write
/// reaches the driver.
pub fn can_write(path: &Path) -> bool {
    fs::OpenOptions::new().write(true).open(path).is_ok()
}

/// The tuning files directly under a card's device directory, which `rochgpu setup` hands to a user:
/// exactly the ones the backend writes, and no others.
pub const OWNED_FILES: [&str; 4] = [
    "pp_od_clk_voltage",
    "power_dpm_force_performance_level",
    "gpu_od/fan_ctrl/fan_curve",
    "gpu_od/fan_ctrl/fan_zero_rpm_enable",
];

/// The same, under the card's hwmon directory.
pub const OWNED_HWMON_FILES: [&str; 3] = ["power1_cap", "pwm1", "pwm1_enable"];

/// The device directory of every amdgpu card under `root` (`/sys/class/drm`). Follows only symlinks
/// and never reads an attribute, so a sleeping card stays asleep.
pub fn amdgpu_device_dirs(root: &Path) -> Vec<std::path::PathBuf> {
    let is_card =
        |name: &str| name.strip_prefix("card").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    let mut dirs: Vec<_> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| is_card(&e.file_name().to_string_lossy()))
        .map(|e| e.path().join("device"))
        .filter(|d| fs::read_link(d.join("driver")).is_ok_and(|l| l.file_name().is_some_and(|n| n == "amdgpu")))
        .collect();
    dirs.sort();
    dirs
}

/// The tuning files this card actually has. Changing a file's owner is the kernel's business alone and
/// does not wake the card.
pub fn owned_files(device_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = OWNED_FILES.iter().map(|f| device_dir.join(f)).collect();
    for hwmon in fs::read_dir(device_dir.join("hwmon")).into_iter().flatten().flatten() {
        files.extend(OWNED_HWMON_FILES.iter().map(|f| hwmon.path().join(f)));
    }
    files.retain(|f| f.exists());
    files
}

/// A refused write, in terms of what the person can do about it.
pub fn write_error(path: &Path, value: &str, error: &io::Error) -> GpuError {
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if error.kind() == io::ErrorKind::PermissionDenied {
        return GpuError::new(format!("Not allowed to write {file}. {}", permission_hint()));
    }
    const EINVAL: i32 = 22;
    const EOPNOTSUPP: i32 = 95;
    const ENOTSUPP: i32 = 524; // the kernel's own, which never reaches errno.h
    match error.raw_os_error() {
        Some(EINVAL) => GpuError::new(format!(
            "The driver refused '{}' for {file}: the value is outside what this card allows.",
            value.trim()
        )),
        Some(EOPNOTSUPP | ENOTSUPP) => GpuError::new(format!("The driver does not support {file} on this card.")),
        _ => GpuError::new(format!("Could not write {file}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(raw: i32) -> String {
        write_error(Path::new("/sys/x/power1_cap"), "5\n", &io::Error::from_raw_os_error(raw)).message
    }

    #[test]
    fn the_setup_command_can_be_run_from_here() {
        // The test binary is not on sudo's path, so it has to be named in full.
        let exe = std::env::current_exe().unwrap();
        assert_eq!(setup_command(" --overdrive"), format!("sudo {} setup --overdrive", exe.display()));
    }

    #[test]
    fn refused_writes_say_what_to_do() {
        assert!(message(13).contains(&setup_command("")), "EACCES");
        assert!(message(1).contains(&setup_command("")), "EPERM");
        assert!(message(22).contains("refused '5' for power1_cap"), "EINVAL, without the newline");
        assert!(message(95).contains("does not support power1_cap"));
        assert!(message(524).contains("does not support power1_cap"));
        assert!(message(5).contains("Could not write power1_cap"), "anything else keeps the OS's words");
    }
}
