//! Persists profiles + app settings as JSON under %AppData%\Roch GPU (or a custom folder), in the
//! same layout and JSON shape the C# build used.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::models::TuningProfile;

pub struct ProfileStore {
    root: PathBuf,
}

impl ProfileStore {
    /// Open the store at `root`, or the default folder when `None`.
    pub fn new(root: Option<PathBuf>) -> Result<Self> {
        let root = root.unwrap_or_else(storage_root::resolve);
        let store = Self { root };
        fs::create_dir_all(store.profiles_directory())?;
        storage_root::restore_owner(store.root_directory());
        storage_root::restore_owner(&store.profiles_directory());
        Ok(store)
    }

    pub fn root_directory(&self) -> &Path {
        &self.root
    }

    pub fn profiles_directory(&self) -> PathBuf {
        self.root.join("profiles")
    }

    pub fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// Profile names, ordered case-insensitively.
    pub fn list_profile_names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.profiles_directory())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let is_json = path.extension().is_some_and(|x| x.eq_ignore_ascii_case("json"));
                if !is_json || !path.is_file() {
                    return None;
                }
                path.file_stem().map(|s| s.to_string_lossy().into_owned())
            })
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        names
    }

    pub fn load(&self, name: &str) -> Result<Option<TuningProfile>> {
        let path = self.path_for(name);
        if !path.exists() {
            return Ok(None);
        }
        let json = fs::read_to_string(&path)?;
        let mut p = TuningProfile::from_json(strip_bom(&json))?;
        p.name = name.to_string();
        Ok(Some(p))
    }

    /// Write through a temp file and rename, so a crash mid-write never leaves half a profile.
    pub fn save(&self, profile: &mut TuningProfile) -> Result<()> {
        profile.modified_utc = chrono::Utc::now();
        let json = to_indented_json(profile)?;
        let target = self.path_for(&profile.name);
        let tmp = PathBuf::from(format!("{}.tmp", target.display()));
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &target)?;
        storage_root::restore_owner(&target);
        Ok(())
    }

    pub fn delete(&self, name: &str) -> Result<bool> {
        let path = self.path_for(name);
        if !path.exists() {
            return Ok(false);
        }
        fs::remove_file(path)?;
        Ok(true)
    }

    /// Settings that fail to parse fall back to defaults rather than blocking the app.
    pub fn load_settings(&self) -> AppSettings {
        fs::read_to_string(self.settings_path())
            .ok()
            .and_then(|json| serde_json::from_str(strip_bom(&json)).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, s: &AppSettings) -> Result<()> {
        fs::write(self.settings_path(), to_indented_json(s)?)?;
        storage_root::restore_owner(&self.settings_path());
        Ok(())
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.profiles_directory().join(format!("{}.json", sanitize_file_name(name)))
    }
}

/// System.Text.Json's `WriteIndented` uses two spaces, as serde_json's pretty printer does.
fn to_indented_json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)?)
}

fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// Replace the characters Windows forbids in file names (`Path.GetInvalidFileNameChars`).
pub fn sanitize_file_name(name: &str) -> String {
    name.chars()
        .map(
            |c| {
                if matches!(c, '"' | '<' | '>' | '|' | ':' | '*' | '?' | '\\' | '/') || (c as u32) < 32 {
                    '_'
                } else {
                    c
                }
            },
        )
        .collect()
}

/// Where profiles and settings live. The folder was named for the tool's old name; renaming it
/// outright would orphan every saved profile and the recorded rail defaults, so the old folder is
/// moved across once.
pub mod storage_root {
    use std::path::{Path, PathBuf};

    pub fn app_data() -> PathBuf {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(config_home)
    }

    /// The per-user config folder. Under sudo that is still the person who asked: root has no profiles
    /// of its own, and `sudo rochgpu apply-profile` should find theirs.
    pub fn config_home() -> PathBuf {
        if let Some(invoker) = invoker() {
            return invoker.home.join(".config");
        }
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Who ran `sudo` or `pkexec`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Invoker {
        pub name: String,
        pub uid: u32,
        pub gid: u32,
        pub home: PathBuf,
    }

    /// This line of /etc/passwd, when it is the user with this number.
    pub fn parse_passwd_line(line: &str, uid: u32) -> Option<Invoker> {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let (found, gid) = (fields.nth(1)?.parse::<u32>().ok()?, fields.next()?.parse::<u32>().ok()?);
        let home = fields.nth(1)?;
        (found == uid).then(|| Invoker { name: name.to_string(), uid, gid, home: PathBuf::from(home) })
    }

    /// The person behind this process, when it is root on someone's behalf. `None` otherwise, which is
    /// every run on Windows.
    #[cfg(not(windows))]
    pub fn invoker() -> Option<Invoker> {
        use std::os::unix::fs::MetadataExt;
        // /proc/self belongs to the effective user, and asking it needs no unsafe code.
        if std::fs::metadata("/proc/self").ok()?.uid() != 0 {
            return None;
        }
        let uid = std::env::var("SUDO_UID").or_else(|_| std::env::var("PKEXEC_UID")).ok()?.trim().parse().ok()?;
        let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
        passwd.lines().find_map(|line| parse_passwd_line(line, uid))
    }

    #[cfg(windows)]
    pub fn invoker() -> Option<Invoker> {
        None
    }

    /// Give a file made under sudo back to the person it belongs to. A settings file left owned by
    /// root would quietly stop saving on every later run without sudo.
    pub fn restore_owner(path: &Path) {
        #[cfg(not(windows))]
        if let Some(invoker) = invoker() {
            let _ = std::os::unix::fs::chown(path, Some(invoker.uid), Some(invoker.gid));
        }
        #[cfg(windows)]
        let _ = path;
    }

    pub fn resolve() -> PathBuf {
        let app_data = app_data();
        let current = app_data.join("Roch GPU");
        let legacy = app_data.join("RochGpuOC");
        if !current.exists() && legacy.exists() {
            let _ = std::fs::rename(&legacy, &current);
        }
        current
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct AppSettings {
    pub startup_profile: Option<String>,
    pub apply_on_startup: bool,
    pub start_minimized: bool,
    /// Open the hardware monitor as soon as the app starts. Off by default: the monitor is the
    /// only thing that polls the driver.
    pub auto_open_monitor: bool,
    pub poll_interval_ms: i32,
    pub history_seconds: i32,
    pub use_mock_backend: bool,
    /// Fresh settings open in light mode; an explicitly saved theme is retained.
    pub dark_mode: bool,
    /// GUI appearance and layout preferences. Kept separate from tuning profiles;
    /// older builds can ignore this object and still read the existing settings.
    pub ui_preferences: serde_json::Value,
    /// The GPU the window last tuned, as an index into the backend's device list. Clamped to the
    /// enumerated list at startup, so a removed card or a driver that reorders falls back to 0.
    pub gpu_index: usize,
    /// Last applied profile per GPU (by name), restored into the sliders on launch.
    pub last_profile_by_gpu: BTreeMap<String, String>,
    /// Highest core voltage seen under load, per GPU.
    pub observed_max_voltage_by_gpu: BTreeMap<String, i32>,
    /// Highest core voltage seen per GPU with the boost wound fully open.
    pub observed_max_boosted_voltage_by_gpu: BTreeMap<String, i32>,
    /// Voltage-rail ceilings as first seen on each GPU, in mV, and what Reset restores them to.
    /// Recorded once, on first sight of a GPU, and never overwritten: a rail offset survives a
    /// reboot, so every later process would otherwise call the last applied value "default".
    pub nvvdd_default_max_by_gpu: BTreeMap<String, i32>,
    pub msvdd_default_max_by_gpu: BTreeMap<String, i32>,
    /// Stock OCP limits per GPU, in milliamps, recorded the first time the card is seen.
    pub nvvdd_ocp_default_by_gpu: BTreeMap<String, i32>,
    pub msvdd_ocp_default_by_gpu: BTreeMap<String, i32>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            startup_profile: None,
            apply_on_startup: false,
            start_minimized: false,
            auto_open_monitor: false,
            poll_interval_ms: 1000,
            history_seconds: 120,
            use_mock_backend: false,
            dark_mode: false,
            ui_preferences: serde_json::Value::Null,
            gpu_index: 0,
            last_profile_by_gpu: BTreeMap::new(),
            observed_max_voltage_by_gpu: BTreeMap::new(),
            observed_max_boosted_voltage_by_gpu: BTreeMap::new(),
            nvvdd_default_max_by_gpu: BTreeMap::new(),
            msvdd_default_max_by_gpu: BTreeMap::new(),
            nvvdd_ocp_default_by_gpu: BTreeMap::new(),
            msvdd_ocp_default_by_gpu: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("roch-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn save_load_list_delete() {
        let dir = temp_dir("roundtrip");
        let store = ProfileStore::new(Some(dir.clone())).unwrap();
        let mut p = TuningProfile { name: "b: slot?".into(), core_offset_mhz: 90, ..Default::default() };
        store.save(&mut p).unwrap();
        let mut a = TuningProfile { name: "A".into(), ..Default::default() };
        store.save(&mut a).unwrap();
        assert_eq!(store.list_profile_names(), vec!["A".to_string(), "b_ slot_".to_string()]);
        let back = store.load("b: slot?").unwrap().unwrap();
        assert_eq!(back.core_offset_mhz, 90);
        assert_eq!(back.name, "b: slot?");
        assert!(store.delete("A").unwrap());
        assert!(!store.delete("A").unwrap());
        assert!(store.load("missing").unwrap().is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_round_trip_and_bad_file() {
        let dir = temp_dir("settings");
        let store = ProfileStore::new(Some(dir.clone())).unwrap();
        assert_eq!(store.load_settings(), AppSettings::default());
        let mut s = AppSettings {
            dark_mode: true,
            startup_profile: Some("Slot 2".into()),
            ui_preferences: serde_json::json!({"theme":"system", "cards":[{"sensor":"temp", "label":"My GPU"}]}),
            ..Default::default()
        };
        s.nvvdd_default_max_by_gpu.insert("gpu".into(), 1035);
        store.save_settings(&s).unwrap();
        assert_eq!(store.load_settings(), s);
        let text = fs::read_to_string(store.settings_path()).unwrap();
        assert!(text.contains("\"NvvddDefaultMaxByGpu\""));
        fs::write(store.settings_path(), "{ not json").unwrap();
        assert_eq!(store.load_settings(), AppSettings::default());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_settings_load_without_ui_preferences() {
        let settings: AppSettings =
            serde_json::from_str(r#"{"DarkMode":true,"StartupProfile":"Slot 3","PollIntervalMs":2000}"#).unwrap();
        assert!(settings.dark_mode);
        assert_eq!(settings.startup_profile.as_deref(), Some("Slot 3"));
        assert_eq!(settings.poll_interval_ms, 2000);
        assert!(settings.ui_preferences.is_null());
    }
}
