//! `cargo xtask install`: the app, a launcher entry and its icon, for one user with no root needed.
//! On Windows the program is a single file with nothing to install, so this is Linux only.
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Result;
use crate::build::BINARY;

/// The name it answers to on the command line.
const COMMAND: &str = "rochgpu";
/// Named after the Wayland app id, which is how a compositor finds a running window's entry and icon.
const DESKTOP_FILE: &str = "share/applications/RochGPU.desktop";
const ICON_FILE: &str = "share/icons/hicolor/256x256/apps/RochGPU.png";

fn prefix_from(args: &[String]) -> Result<PathBuf> {
    match args {
        [] => std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".local"))
            .ok_or_else(|| "HOME is not set. Use --prefix PATH.".to_string()),
        [flag, path] if flag == "--prefix" => {
            let path = PathBuf::from(path);
            if path.is_absolute() { Ok(path) } else { Err("--prefix needs an absolute path.".into()) }
        }
        _ => Err("Usage: cargo xtask install|uninstall [--prefix PATH]".into()),
    }
}

/// The launcher entry. Quoted because the spec requires it for anything but plain characters, and a
/// prefix can be any path.
fn desktop_entry(command: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Roch GPU\nGenericName=GPU tuning\n\
         Comment=GPU tuning and monitoring: clocks, voltage, power, fans\nExec=\"{}\"\nIcon=RochGPU\nTerminal=false\n\
         Categories=Settings;HardwareSettings;\nStartupWMClass=RochGPU\n",
        command.display()
    )
}

/// Write next to the target and rename over it. Copying straight onto a running program fails with
/// "text file busy"; a rename leaves the old copy running and the new one in place.
fn put_file(source: &[u8], target: &Path, executable: bool) -> Result<()> {
    let folder = target.parent().ok_or("Invalid install path")?;
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let pending = folder.join(format!(".rochgpu-pending-{}", std::process::id()));
    std::fs::write(&pending, source).map_err(|e| format!("{}: {e}", pending.display()))?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    std::fs::rename(&pending, target).map_err(|e| {
        let _ = std::fs::remove_file(&pending);
        format!("{}: {e}", target.display())
    })
}

pub fn install(args: &[String]) -> Result<()> {
    let prefix = prefix_from(args)?;
    if cfg!(windows) {
        return Err("On Windows there is nothing to install: run dist\\RochGPU.exe from `cargo xtask build`.".into());
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().ok_or("Workspace root missing")?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "--release", "--locked", "-p", "roch-gpu"])
        .current_dir(root)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("cargo build failed ({status}); nothing was installed."));
    }

    let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"));
    let target = if target.is_relative() { root.join(target) } else { target };
    let built = std::fs::read(target.join("release").join(BINARY)).map_err(|e| e.to_string())?;
    let icon = std::fs::read(root.join("crates/roch-gpu/assets/icon.png")).map_err(|e| e.to_string())?;

    let command = prefix.join("bin").join(COMMAND);
    put_file(&built, &command, true)?;
    put_file(desktop_entry(&command).as_bytes(), &prefix.join(DESKTOP_FILE), false)?;
    put_file(&icon, &prefix.join(ICON_FILE), false)?;

    println!("Installed {}", command.display());
    println!("Launcher entry and icon are under {}", prefix.join("share").display());
    println!("\nTo let this user change the card without root, once:\n  sudo {} setup", command.display());
    println!("See where things stand at any time:\n  {} setup --status", command.display());
    Ok(())
}

pub fn uninstall(args: &[String]) -> Result<()> {
    let prefix = prefix_from(args)?;
    let command = prefix.join("bin").join(COMMAND);
    for path in [command.clone(), prefix.join(DESKTOP_FILE), prefix.join(ICON_FILE)] {
        match std::fs::remove_file(&path) {
            Ok(()) => println!("Removed {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    println!("If you ran the setup, undo it before this copy is gone, or remove these by hand:");
    println!("  /etc/udev/rules.d/60-roch-gpu.rules  /etc/modprobe.d/roch-gpu.conf");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_must_be_absolute_and_flags_are_checked() {
        assert!(prefix_from(&["--prefix".into(), "relative".into()]).is_err());
        assert!(prefix_from(&["--prefix".into()]).is_err());
        assert!(prefix_from(&["--other".into(), "/x".into()]).is_err());
        // What counts as absolute depends on the platform: `/opt/roch` is not, on Windows.
        let absolute = std::env::temp_dir().join("roch");
        assert_eq!(prefix_from(&["--prefix".into(), absolute.display().to_string()]).unwrap(), absolute);
    }

    #[test]
    fn the_entry_points_at_the_installed_command_and_matches_the_app_id() {
        let entry = desktop_entry(Path::new("/home/my user/.local/bin/rochgpu"));
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nExec=\"/home/my user/.local/bin/rochgpu\"\n"), "{entry}");
        assert!(entry.contains("\nStartupWMClass=RochGPU\n"));
        assert!(entry.contains("\nIcon=RochGPU\n"));
        assert!(DESKTOP_FILE.ends_with("RochGPU.desktop") && ICON_FILE.ends_with("RochGPU.png"));
    }

    #[test]
    fn a_running_program_can_be_replaced() {
        let dir = std::env::temp_dir().join(format!("roch-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join("bin/rochgpu");
        put_file(b"old", &target, true).unwrap();
        put_file(b"new", &target, true).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o755);
        }
        assert_eq!(std::fs::read_dir(dir.join("bin")).unwrap().count(), 1, "nothing left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
