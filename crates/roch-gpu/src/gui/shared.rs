//! What the WPF build kept in `App`'s statics: the service, the profile store, the settings, the
//! recovery journal and the startup notice. Bundled into one cheaply clonable handle so the view
//! model, the windows and the worker thread all reach the same objects.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use roch_core::services::{AppSettings, ProfileRecovery, ProfileStore, TuningService, startup_task};
use roch_core::{Result, TuningProfile, only_notes};

/// The logon task, behind a trait so tests never run schtasks.
pub trait StartupTasks: Send + Sync {
    fn exists(&self) -> bool;
    fn register(&self, exe_path: &str, profile_name: &str) -> Result<()>;
    fn unregister(&self) -> Result<()>;
}

/// The real Task Scheduler entry (`roch_core::services::startup_task`).
pub struct SchtasksStartup;

impl StartupTasks for SchtasksStartup {
    fn exists(&self) -> bool {
        startup_task::exists()
    }
    fn register(&self, exe_path: &str, profile_name: &str) -> Result<()> {
        startup_task::register(exe_path, profile_name)
    }
    fn unregister(&self) -> Result<()> {
        startup_task::unregister()
    }
}

/// Answers the first [`StartupTasks::exists`] from a query started on a thread the moment it is
/// made: it is a `schtasks.exe` launch (~10 ms), and startup has better things to wait on. Every
/// later query, and every change, goes straight to the wrapped tasks; a change first discards
/// the early answer, which could describe the task as it was before.
pub struct PrefetchedStartup {
    inner: Arc<dyn StartupTasks>,
    first: Mutex<Option<std::thread::JoinHandle<bool>>>,
}

impl PrefetchedStartup {
    pub fn start(inner: Arc<dyn StartupTasks>) -> Self {
        let query = inner.clone();
        let first = std::thread::Builder::new().name("RochGPU startup query".into()).spawn(move || query.exists()).ok();
        Self { inner, first: Mutex::new(first) }
    }

    fn take_first(&self) -> Option<bool> {
        lock(&self.first).take().and_then(|h| h.join().ok())
    }
}

impl StartupTasks for PrefetchedStartup {
    fn exists(&self) -> bool {
        self.take_first().unwrap_or_else(|| self.inner.exists())
    }
    fn register(&self, exe_path: &str, profile_name: &str) -> Result<()> {
        let _ = self.take_first();
        self.inner.register(exe_path, profile_name)
    }
    fn unregister(&self) -> Result<()> {
        let _ = self.take_first();
        self.inner.unregister()
    }
}

/// Shared application state (C# `App.Service`, `App.Store`, `App.Settings`, `App.Recovery`,
/// `App.RecoveryNotice`). Clone freely; every field is shared.
#[derive(Clone)]
pub struct Shared {
    pub service: Arc<TuningService>,
    pub store: Arc<ProfileStore>,
    settings: Arc<Mutex<AppSettings>>,
    recovery: Arc<Mutex<Option<ProfileRecovery>>>,
    /// Set at startup when a crashed trial was recovered or the logon apply did not go through.
    pub recovery_notice: Option<String>,
    pub startup: Arc<dyn StartupTasks>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    pub fn new(
        service: Arc<TuningService>,
        store: Arc<ProfileStore>,
        settings: AppSettings,
        startup: Arc<dyn StartupTasks>,
    ) -> Self {
        Self {
            service,
            store,
            settings: Arc::new(Mutex::new(settings)),
            recovery: Arc::new(Mutex::new(None)),
            recovery_notice: None,
            startup,
        }
    }

    /// The settings, locked. Keep the guard short.
    pub fn settings(&self) -> MutexGuard<'_, AppSettings> {
        lock(&self.settings)
    }

    /// Write the settings file; a failure is logged, not shown (as in the C#).
    pub fn save_settings(&self) {
        let snapshot = self.settings().clone();
        if let Err(e) = self.store.save_settings(&snapshot) {
            self.log(&format!("Settings could not be saved: {e}"));
        }
    }

    /// The recovery journal, locked (C# `App.Recovery`). `None` when it could not be opened.
    pub fn recovery(&self) -> MutexGuard<'_, Option<ProfileRecovery>> {
        lock(&self.recovery)
    }

    pub fn set_recovery(&self, recovery: Option<ProfileRecovery>) {
        *self.recovery() = recovery;
    }

    /// True when a recovery journal exists and has no trial pending (C# `App.Recovery is
    /// { HasPending: false }`).
    pub fn recovery_idle(&self) -> bool {
        matches!(&*self.recovery(), Some(r) if !r.has_pending())
    }

    /// Open the journal for the selected device, applying through the service. The stock profile
    /// is what a trial reverts to when nothing was ever confirmed.
    pub fn open_recovery(&self) -> Result<ProfileRecovery> {
        let d = self.service.device();
        let key = format!("{}|{}|{}", d.vendor, d.bus_id, d.name);
        let stock = TuningProfile::stock(&self.service.capabilities(), Some(&d.name));
        let svc = self.service.clone();
        ProfileRecovery::new(self.store.root_directory(), &key, &stock, Box::new(move |p| svc.apply(&p)))
    }

    /// Apply a profile as a journalled trial: persist it as pending, apply, then confirm on success
    /// or revert on failure (C# `App.TrialProfile`). Returns the apply's messages.
    pub fn trial_profile(&self, profile: &TuningProfile) -> Vec<String> {
        let mut guard = self.recovery();
        let Some(recovery) = guard.as_mut() else {
            return vec!["Profile recovery is unavailable; no settings applied.".into()];
        };
        let mut candidate = profile.clone_profile();
        candidate.clamp_to(&self.service.capabilities());
        // The C# try block: any failure (journal write, confirm) lands in the catch below.
        let attempt = (|| -> Result<Vec<String>> {
            let errors = recovery.begin(&candidate)?;
            if errors.is_empty() || only_notes(&errors) {
                recovery.confirm()?;
                return Ok(errors);
            }
            let rollback = recovery.revert();
            if !rollback.is_empty() && !only_notes(&rollback) {
                return Ok(vec![format!("Recovery failed; retry by reopening Roch GPU: {}", rollback.join("; "))]);
            }
            Ok(vec![if !errors.is_empty() && !only_notes(&errors) {
                format!("Trial failed and was reverted: {}", errors.join("; "))
            } else {
                "Trial reverted; startup settings were not changed.".into()
            }])
        })();
        match attempt {
            Ok(messages) => messages,
            Err(e) => {
                if recovery.has_pending() {
                    let rollback = recovery.revert();
                    if !rollback.is_empty() && !only_notes(&rollback) {
                        return vec![format!("{} Recovery failed: {}", e.message, rollback.join("; "))];
                    }
                }
                vec![e.message]
            }
        }
    }

    /// The log file next to the profiles (C# `App.LogPath`).
    pub fn log_path(&self) -> PathBuf {
        self.store.root_directory().join("roch-gpu.log")
    }

    /// Append one line to the log (C# `App.LogLine`).
    pub fn log(&self, line: &str) {
        log_line(&self.log_path(), line);
    }
}

const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Append a timestamped line, keeping one previous log and capping the live one: a driver that
/// fails every poll would otherwise fill the disk overnight. Never fails loudly.
pub fn log_line(path: &std::path::Path, line: &str) {
    if fs::metadata(path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let old = PathBuf::from(format!("{}.old", path.display()));
        let _ = fs::remove_file(&old);
        let _ = fs::rename(path, &old);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(f, "{now}  {line}");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct Fake {
        registered: AtomicBool,
        queries: AtomicUsize,
    }

    impl StartupTasks for Fake {
        fn exists(&self) -> bool {
            self.queries.fetch_add(1, Ordering::SeqCst);
            self.registered.load(Ordering::SeqCst)
        }
        fn register(&self, _: &str, _: &str) -> Result<()> {
            self.registered.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn unregister(&self) -> Result<()> {
            self.registered.store(false, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn the_first_query_is_answered_early_and_later_ones_ask_again() {
        let fake = Arc::new(Fake::default());
        let tasks = PrefetchedStartup::start(fake.clone());
        assert!(!tasks.exists());
        assert_eq!(fake.queries.load(Ordering::SeqCst), 1);
        fake.registered.store(true, Ordering::SeqCst);
        assert!(tasks.exists());
        assert_eq!(fake.queries.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_change_discards_the_early_answer() {
        let fake = Arc::new(Fake::default());
        let tasks = PrefetchedStartup::start(fake.clone());
        tasks.register("RochGPU.exe", "Slot 1").unwrap();
        assert!(tasks.exists());
        tasks.unregister().unwrap();
        assert!(!tasks.exists());
    }
}
