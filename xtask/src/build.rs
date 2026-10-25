use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Result;

trait Runner {
    fn cargo(&mut self, root: &Path, args: &[&str]) -> Result<()>;
    fn smoke(&mut self, exe: &Path, appdata: &Path) -> Result<()>;
}

struct ProcessRunner;
impl Runner for ProcessRunner {
    fn cargo(&mut self, root: &Path, args: &[&str]) -> Result<()> {
        println!("cargo {}", args.join(" "));
        let status = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args(args)
            .current_dir(root)
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("cargo {} failed ({status}); no executable was published.", args.join(" ")))
        }
    }

    fn smoke(&mut self, exe: &Path, appdata: &Path) -> Result<()> {
        for verb in ["info", "sensors"] {
            let output = Command::new(exe)
                .args([verb, "--mock"])
                .env("APPDATA", appdata)
                .output()
                .map_err(|e| format!("Could not run mock {verb}: {e}"))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            if !output.status.success() || (verb == "info" && !stdout.contains("Simulated")) {
                return Err(format!("Mock {verb} failed: {stdout} {}", String::from_utf8_lossy(&output.stderr)));
            }
            println!("Mock {verb}: passed");
        }
        Ok(())
    }
}

fn steps(root: &Path, runner: &mut impl Runner) -> Result<()> {
    for args in [
        &["fmt", "--all", "--check"][..],
        &["test", "--workspace", "--release", "--locked"][..],
        &["clippy", "--workspace", "--all-targets", "--release", "--locked", "--", "-D", "warnings"][..],
        &["build", "--release", "--locked", "-p", "roch-gpu"][..],
    ] {
        runner.cargo(root, args)?;
    }
    Ok(())
}

/// This folder is exclusively created by this process; never clean up a pre-existing path.
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
        let path = std::env::temp_dir().join(format!("roch-native-build-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&path).map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn package(source: &Path, destination: &Path, runner: &mut impl Runner) -> Result<()> {
    let scratch = Scratch::new()?;
    // Validate a private copy before touching any existing published executable.
    let staged = scratch.0.join("RochGPU.exe");
    std::fs::copy(source, &staged).map_err(|e| format!("Could not stage {}: {e}", source.display()))?;
    runner.smoke(&staged, &scratch.0.join("appdata"))?;
    let folder = destination.parent().ok_or("Invalid output path")?;
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let pending = folder.join(format!(".RochGPU-pending-{}.exe", std::process::id()));
    std::fs::copy(&staged, &pending).map_err(|e| e.to_string())?;
    if let Err(error) = std::fs::rename(&pending, destination) {
        let _ = std::fs::remove_file(&pending);
        return Err(format!(
            "Could not replace {}: {error}. Close Roch GPU, including its tray instance, or use --out-dir PATH. The existing file was kept.",
            destination.display()
        ));
    }
    println!(
        "Built {} ({} bytes)",
        destination.display(),
        std::fs::metadata(destination).map_err(|e| e.to_string())?.len()
    );
    Ok(())
}

pub fn run(args: &[String]) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().ok_or("Workspace root missing")?;
    let folder = match args {
        [] => root.join("dist"),
        [flag, path] if flag == "--out-dir" => std::env::current_dir().map_err(|e| e.to_string())?.join(path),
        _ => return Err("Usage: cargo xtask build [--out-dir PATH]".into()),
    };
    let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"));
    let target = if target.is_relative() { root.join(target) } else { target };
    let mut runner = ProcessRunner;
    steps(root, &mut runner)?;
    package(&target.join("release/RochGPU.exe"), &folder.join("RochGPU.exe"), &mut runner)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct FakeRunner {
        commands: Vec<String>,
        fail_at: Option<usize>,
        smoke_fails: bool,
    }
    impl Runner for FakeRunner {
        fn cargo(&mut self, _: &Path, args: &[&str]) -> Result<()> {
            self.commands.push(args[0].into());
            if self.fail_at == Some(self.commands.len()) { Err("injected failure".into()) } else { Ok(()) }
        }
        fn smoke(&mut self, _: &Path, _: &Path) -> Result<()> {
            if self.smoke_fails { Err("injected smoke failure".into()) } else { Ok(()) }
        }
    }
    #[test]
    fn failed_checks_stop_before_building() {
        for fail_at in 1..=3 {
            let mut runner = FakeRunner { fail_at: Some(fail_at), ..Default::default() };
            assert!(steps(Path::new("."), &mut runner).is_err());
            assert_eq!(runner.commands.len(), fail_at);
            assert!(!runner.commands.iter().any(|s| s == "build"));
        }
    }
    #[test]
    fn packaging_preserves_old_binary_when_smoke_fails_and_replaces_it_on_success() {
        let scratch = Scratch::new().unwrap();
        let source = scratch.0.join("source.exe");
        let dest = scratch.0.join("output/RochGPU.exe");
        std::fs::create_dir(dest.parent().unwrap()).unwrap();
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&dest, b"old").unwrap();
        let mut runner = FakeRunner { smoke_fails: true, ..Default::default() };
        assert!(package(&source, &dest, &mut runner).is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"old");
        runner.smoke_fails = false;
        package(&source, &dest, &mut runner).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
    }
}
