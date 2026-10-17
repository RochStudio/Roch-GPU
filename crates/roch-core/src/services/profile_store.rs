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
    use std::path::PathBuf;

    pub fn app_data() -> PathBuf {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")).unwrap_or_else(|| PathBuf::from("."))
        })
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
        let mut s = AppSettings::default();
        s.dark_mode = true;
        s.nvvdd_default_max_by_gpu.insert("gpu".into(), 1035);
        store.save_settings(&s).unwrap();
        assert_eq!(store.load_settings(), s);
        let text = fs::read_to_string(store.settings_path()).unwrap();
        assert!(text.contains("\"NvvddDefaultMaxByGpu\""));
        fs::write(store.settings_path(), "{ not json").unwrap();
        assert_eq!(store.load_settings(), AppSettings::default());
        let _ = fs::remove_dir_all(dir);
    }
}
