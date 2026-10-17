//! Everything the WPF `App.OnStartup` did before the main window existed: the flags, backend
//! detection (with the "start simulated instead?" offer), the recovery journal — recovered BEFORE
//! any logon profile so a crashed trial can never be replayed at boot — and the logon apply.
//!
//! Flags: `--mock` (simulated GPU), `--minimized` (straight to the tray), `--apply-profile <name>`
//! (apply that confirmed startup profile, then stay resident for fan curves), `--exit` (apply and
//! quit, no window).

use std::sync::Arc;

use roch_core::services::{AppSettings, ProfileStore, TuningService};
use roch_core::{GpuBackend, only_notes};

use super::shared::{Shared, StartupTasks};
use crate::backends;

/// The window's own command-line flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flags {
    pub mock: bool,
    pub exit_after: bool,
    pub minimized: bool,
    pub apply_profile: Option<String>,
}

impl Flags {
    pub fn parse(args: &[String]) -> Flags {
        let has = |f: &str| args.iter().any(|a| a == f);
        let apply_profile = args.iter().position(|a| a == "--apply-profile").and_then(|i| args.get(i + 1)).cloned();
        Flags { mock: has("--mock"), exit_after: has("--exit"), minimized: has("--minimized"), apply_profile }
    }
}

/// How startup ended.
pub enum Started {
    /// No window: quit with this code.
    Exit(i32),
    /// Open the window (in the tray when `minimized`).
    Window { shared: Shared, minimized: bool },
}

/// Run the startup sequence. `offer_mock` asks the user whether to fall back to the simulated GPU
/// when no supported card initialised (a modal dialog in the real app).
pub fn start(
    flags: &Flags,
    store: Arc<ProfileStore>,
    settings: AppSettings,
    startup: Arc<dyn StartupTasks>,
    offer_mock: impl FnOnce(&str) -> bool,
) -> Started {
    let log_path = store.root_directory().join("roch-gpu.log");
    let log = |s: &str| super::shared::log_line(&log_path, s);
    let mock = flags.mock || settings.use_mock_backend;
    let mut minimized = flags.minimized || settings.start_minimized;
    let mut apply_profile = flags.apply_profile.clone();
    let history = settings.history_seconds.max(1) as usize;

    // NVIDIA, AMD or Intel, whichever initialises. One binary, any card.
    let mut backend: Box<dyn GpuBackend> = backends::mock();
    let mut detect_error = None;
    if !mock {
        match backends::create_and_initialize() {
            Ok((b, attempts)) => {
                for a in attempts {
                    log(&format!("Backend probe — {}: {}", a.name, a.error));
                }
                backend = b;
            }
            Err(e) => detect_error = Some(e),
        }
    }

    let make_service = |b: Box<dyn GpuBackend>| {
        let svc = Arc::new(TuningService::with_history(b, history));
        let path = log_path.clone();
        svc.add_log_listener(move |line| super::shared::log_line(&path, line));
        svc
    };
    let mut service = make_service(backend);
    let init = match detect_error {
        Some(e) => Err(e),
        None => service.initialize(0),
    };
    if let Err(e) = init {
        log(&format!("Init failed: {}", e.message));
        if mock || !offer_mock(&e.message) {
            return Started::Exit(1);
        }
        drop(service);
        service = make_service(backends::mock());
        if let Err(e) = service.initialize(0) {
            log(&format!("Init failed: {}", e.message));
            return Started::Exit(1);
        }
    }

    let mut shared = Shared::new(service.clone(), store, settings, startup);

    // Recover BEFORE processing a logon profile so a crashed trial cannot be replayed at boot.
    let mut notice = None;
    let mut recovered_trial = false;
    match shared.open_recovery() {
        Ok(mut recovery) => {
            if recovery.has_pending() {
                recovered_trial = true;
                let errors = recovery.revert();
                let text = if errors.is_empty() || only_notes(&errors) {
                    "Recovered interrupted profile trial. Startup apply was skipped.".to_string()
                } else {
                    format!("Profile recovery failed: {}", errors.join("; "))
                };
                log(&text);
                notice = Some(text);
            }
            shared.set_recovery(Some(recovery));
        }
        Err(e) => {
            let text = format!("Profile recovery unavailable: {}", e.message);
            log(&text);
            notice = Some(text);
            recovered_trial = true;
        }
    }
    if recovered_trial {
        apply_profile = None;
        minimized = false;
    }

    if let Some(name) = apply_profile {
        let confirmed = shared.recovery().as_ref().and_then(|r| r.startup_profile(&name));
        match confirmed {
            None => {
                let text = format!("Confirm '{name}' using the Startup checkbox before it can run at logon.");
                log(&text);
                notice = Some(text);
                minimized = false;
            }
            Some(p) => {
                let mut guard = shared.recovery();
                if let Some(recovery) = guard.as_mut() {
                    match recovery.begin(&p) {
                        Ok(errs) if errs.is_empty() || only_notes(&errs) => match recovery.confirm() {
                            Ok(()) => log(&format!("Confirmed startup profile '{name}' applied.")),
                            Err(e) => {
                                let text = format!("Startup recovery interrupted: {}", e.message);
                                log(&text);
                                notice = Some(text);
                                minimized = false;
                            }
                        },
                        Ok(errs) => {
                            let rollback = recovery.revert();
                            let tail = if recovery.has_pending() {
                                format!(" Recovery failed: {}", rollback.join("; "))
                            } else {
                                " — restored fallback profile.".to_string()
                            };
                            let text = format!("Startup apply failed: {}{tail}", errs.join("; "));
                            log(&text);
                            notice = Some(text);
                            minimized = false;
                        }
                        Err(e) => {
                            let text = format!("Startup recovery interrupted: {}", e.message);
                            log(&text);
                            notice = Some(text);
                            minimized = false;
                        }
                    }
                }
            }
        }
    }

    if flags.exit_after {
        // A fan curve cannot survive without a resident process; the startup docs say so.
        return Started::Exit(0);
    }
    shared.recovery_notice = notice;
    Started::Window { shared, minimized }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn flags_parse_like_the_wpf_app() {
        let f = Flags::parse(&v(&["--apply-profile", "Slot 1", "--minimized"]));
        assert_eq!(f.apply_profile.as_deref(), Some("Slot 1"));
        assert!(f.minimized && !f.mock && !f.exit_after);
        let f = Flags::parse(&v(&["--mock", "--exit", "--apply-profile"]));
        assert!(f.mock && f.exit_after);
        assert_eq!(f.apply_profile, None);
    }
}
