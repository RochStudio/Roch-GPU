//! Durable profile trial journal. The candidate is persisted before any write reaches the card;
//! if rollback fails the pending state is retained, so an unstable tune is never silently
//! forgotten.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::models::TuningProfile;
use crate::{GpuError, Result, only_notes};

/// The applier: writes a profile to the card and returns its messages (errors and notes). A
/// failure that would have been an exception in the C# build is returned as a message.
pub type ApplyFn = Box<dyn FnMut(TuningProfile) -> Vec<String> + Send>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct State {
    pub device_key: String,
    pub last_confirmed: Option<TuningProfile>,
    pub pending: Option<TuningProfile>,
    pub startup: BTreeMap<String, TuningProfile>,
}

impl State {
    /// System.Text.Json's default shape: compact, enums as numbers.
    fn to_json(&self) -> String {
        let profile = |p: &Option<TuningProfile>| p.as_ref().map(|p| p.to_numeric_enum_value()).unwrap_or(Value::Null);
        let mut obj = Map::new();
        obj.insert("DeviceKey".into(), Value::String(self.device_key.clone()));
        obj.insert("LastConfirmed".into(), profile(&self.last_confirmed));
        obj.insert("Pending".into(), profile(&self.pending));
        let startup: Map<String, Value> =
            self.startup.iter().map(|(k, v)| (k.clone(), v.to_numeric_enum_value())).collect();
        obj.insert("Startup".into(), Value::Object(startup));
        Value::Object(obj).to_string()
    }

    fn from_json(json: &str) -> Result<State> {
        let mut s: State = serde_json::from_str(json)?;
        for p in s.last_confirmed.iter_mut().chain(s.pending.iter_mut()).chain(s.startup.values_mut()) {
            p.migrate_legacy();
        }
        Ok(s)
    }
}

pub struct ProfileRecovery {
    path: PathBuf,
    key: String,
    apply: ApplyFn,
    defaults: TuningProfile,
    state: State,
    can_confirm: bool,
}

fn success(errors: &[String]) -> bool {
    errors.is_empty() || only_notes(errors)
}

impl ProfileRecovery {
    /// Open (or start) the journal for `device_key` in `directory`. An unreadable journal, or one
    /// written for another GPU, fails closed rather than being discarded.
    pub fn new(directory: &Path, device_key: &str, defaults: &TuningProfile, apply: ApplyFn) -> Result<Self> {
        fs::create_dir_all(directory)?;
        let hash = Sha256::digest(device_key.as_bytes());
        let hex: String = hash.iter().map(|b| format!("{b:02X}")).collect();
        let path = directory.join(format!("recovery-{hex}.json"));
        let state = if path.exists() {
            let text = fs::read_to_string(&path)?;
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            if text.trim().is_empty() || text.trim() == "null" {
                return Err(GpuError::new("Empty recovery journal"));
            }
            State::from_json(text)?
        } else {
            State { device_key: device_key.to_string(), ..Default::default() }
        };
        if state.device_key != device_key {
            return Err(GpuError::new("Recovery journal belongs to a different GPU"));
        }
        Ok(Self {
            path,
            key: device_key.to_string(),
            apply,
            defaults: defaults.clone_profile(),
            state,
            can_confirm: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn has_pending(&self) -> bool {
        self.state.pending.is_some()
    }

    pub fn last_confirmed(&self) -> Option<TuningProfile> {
        self.state.last_confirmed.as_ref().map(TuningProfile::clone_profile)
    }

    pub fn pending(&self) -> Option<TuningProfile> {
        self.state.pending.as_ref().map(TuningProfile::clone_profile)
    }

    fn save(&mut self, next: State) -> Result<()> {
        let temp = PathBuf::from(format!("{}.tmp", self.path.display()));
        {
            let mut file = fs::File::create(&temp)?;
            file.write_all(next.to_json().as_bytes())?;
            file.sync_all()?;
        }
        fs::rename(&temp, &self.path)?;
        self.state = next;
        Ok(())
    }

    fn copy(&self) -> State {
        State {
            device_key: self.key.clone(),
            last_confirmed: self.state.last_confirmed.as_ref().map(TuningProfile::clone_profile),
            pending: self.state.pending.as_ref().map(TuningProfile::clone_profile),
            startup: self.state.startup.iter().map(|(k, v)| (k.clone(), v.clone_profile())).collect(),
        }
    }

    /// Persist `candidate` as pending, then apply it. Fails if a trial is already pending.
    pub fn begin(&mut self, candidate: &TuningProfile) -> Result<Vec<String>> {
        if self.has_pending() {
            return Err(GpuError::new("Recover the pending trial before applying another profile."));
        }
        self.can_confirm = false;
        let mut next = self.copy();
        next.pending = Some(candidate.clone_profile());
        self.save(next)?;
        let errors = (self.apply)(candidate.clone_profile());
        self.can_confirm = success(&errors);
        Ok(errors)
    }

    pub fn confirm(&mut self) -> Result<()> {
        if !self.has_pending() || !self.can_confirm {
            return Err(GpuError::new("No successful profile trial is active."));
        }
        let mut next = self.copy();
        next.last_confirmed = next.pending.take();
        self.save(next)?;
        self.can_confirm = false;
        Ok(())
    }

    /// Re-apply the last confirmed profile (or the defaults). The pending entry is cleared only
    /// when that rollback succeeds.
    pub fn revert(&mut self) -> Vec<String> {
        self.can_confirm = false;
        if !self.has_pending() {
            return Vec::new();
        }
        let target = self.state.last_confirmed.as_ref().unwrap_or(&self.defaults).clone_profile();
        let errors = (self.apply)(target);
        if success(&errors) {
            let mut next = self.copy();
            next.pending = None;
            if let Err(e) = self.save(next) {
                return vec![e.message];
            }
        }
        errors
    }

    pub fn approve_startup(&mut self, name: &str) -> Result<()> {
        if self.has_pending() || self.state.last_confirmed.is_none() {
            return Err(GpuError::new("Confirm a profile trial before enabling startup."));
        }
        let mut next = self.copy();
        let confirmed = next.last_confirmed.as_ref().map(TuningProfile::clone_profile);
        if let Some(p) = confirmed {
            next.startup.insert(name.to_string(), p);
        }
        self.save(next)
    }

    pub fn startup_profile(&self, name: &str) -> Option<TuningProfile> {
        self.state.startup.get(name).map(TuningProfile::clone_profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("roch-recovery-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn applier(log: Arc<Mutex<Vec<i32>>>, fail_on: i32) -> ApplyFn {
        Box::new(move |p: TuningProfile| {
            log.lock().unwrap().push(p.core_offset_mhz);
            if p.core_offset_mhz == fail_on { vec!["driver rejected".into()] } else { vec![] }
        })
    }

    #[test]
    fn begin_confirm_revert_and_persistence() {
        let dir = temp_dir("flow");
        let log = Arc::new(Mutex::new(Vec::new()));
        let defaults = TuningProfile::default();
        let mut r = ProfileRecovery::new(&dir, "gpu-a", &defaults, applier(log.clone(), 999)).unwrap();
        let p = TuningProfile { core_offset_mhz: 30, ..Default::default() };
        assert!(r.begin(&p).unwrap().is_empty());
        assert!(r.has_pending());
        assert!(r.begin(&p).is_err(), "second trial refused while one is pending");
        r.confirm().unwrap();
        assert_eq!(r.last_confirmed().unwrap().core_offset_mhz, 30);
        r.approve_startup("Slot 1").unwrap();

        // A failed trial stays pending and survives a restart.
        let bad = TuningProfile { core_offset_mhz: 999, ..Default::default() };
        assert_eq!(r.begin(&bad).unwrap(), vec!["driver rejected".to_string()]);
        assert!(r.confirm().is_err());
        drop(r);
        let mut r = ProfileRecovery::new(&dir, "gpu-a", &defaults, applier(log.clone(), 999)).unwrap();
        assert!(r.has_pending());
        assert_eq!(r.startup_profile("Slot 1").unwrap().core_offset_mhz, 30);
        assert!(r.revert().is_empty());
        assert!(!r.has_pending());
        assert_eq!(*log.lock().unwrap(), vec![30, 999, 30]);

        let text = fs::read_to_string(r.path()).unwrap();
        assert!(text.contains("\"FanMode\":0"), "journal keeps numeric enums: {text}");

        assert!(ProfileRecovery::new(&dir, "gpu-b", &defaults, applier(log.clone(), 0)).is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn foreign_or_empty_journal_fails_closed() {
        let dir = temp_dir("closed");
        let log = Arc::new(Mutex::new(Vec::new()));
        let r = ProfileRecovery::new(&dir, "gpu-a", &TuningProfile::default(), applier(log.clone(), 0)).unwrap();
        let path = r.path().to_path_buf();
        fs::write(&path, r#"{"DeviceKey":"other","Startup":{}}"#).unwrap();
        assert!(ProfileRecovery::new(&dir, "gpu-a", &TuningProfile::default(), applier(log.clone(), 0)).is_err());
        fs::write(&path, "null").unwrap();
        assert!(ProfileRecovery::new(&dir, "gpu-a", &TuningProfile::default(), applier(log, 0)).is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
