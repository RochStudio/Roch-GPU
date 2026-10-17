//! The window half (C# `GpuTuner.App`): an eframe/egui port of the WPF GUI.
//!
//! Module map (details in `GUI_NOTES.md`):
//! * [`startup`] — flags, backend detection, recovery journal, logon apply (App.OnStartup)
//! * [`shared`] — the service/store/settings/recovery bundle and the log (App statics)
//! * [`view_model`] — MainViewModel, UI-free and unit-tested
//! * [`app`] — the eframe App: windows, tray, worker thread, dialogs (MainWindow.xaml.cs)
//! * [`windows`] — one file per window
//! * [`telemetry_table`], [`fan_editor`], [`vf_editor`] — the Monitor/Fan/Curve windows' logic, UI-free
//! * [`theme`], [`widgets`], [`format`], [`assets`], [`tray`], [`native`]

pub mod app;
pub mod assets;
pub mod bench;
pub mod fan_editor;
pub mod format;
pub mod native;
pub mod shared;
pub mod startup;
pub mod telemetry_table;
pub mod theme;
pub mod tray;
pub mod vf_editor;
pub mod view_model;
pub mod widgets;
pub mod windows;

#[cfg(test)]
mod tests;

use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use eframe::egui;
use roch_core::services::{AppSettings, ProfileStore};

use crate::single_instance;
use shared::{PrefetchedStartup, SchtasksStartup, Shared, StartupTasks};
use startup::{Flags, Started};
use view_model::MainViewModel;

/// Run the window. `args` are the window's own flags (`--mock`, `--minimized`,
/// `--apply-profile <name>`, `--exit`); returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let flags = Flags::parse(args);
    let store = match ProfileStore::new(None) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            native::message_box(&e.message, "Roch GPU error", native::Buttons::Ok, native::Icon::Error);
            return 1;
        }
    };
    let settings = store.load_settings();
    crate::trace!("gui: store + settings loaded");

    // Claimed here rather than in the entry point because the elevated relaunch is a new process:
    // whichever copy actually runs the window is the one that should hold the handle.
    let isolated = crate::trace::isolated(args);
    if !isolated && !single_instance::claim() {
        single_instance::ask_existing_to_show();
        return 0;
    }

    crate::trace!("gui: single instance claimed");
    let log_path = store.root_directory().join("roch-gpu.log");
    install_panic_log(log_path.clone());
    if flags.exit_after {
        // `--exit`: apply the logon profile and quit. No window, so nothing to overlap with.
        return match startup::start(&flags, store, settings, Arc::new(SchtasksStartup), offer_mock) {
            Started::Exit(code) => code,
            Started::Window { .. } => 0,
        };
    }
    // The driver half of startup runs beside the window's creation rather than before it.
    let boot = Boot::spawn(flags, store, settings);

    // A second launch cannot open a window of its own, so it asks this one to come forward.
    let (show_tx, show_rx) = channel::<()>();
    let wake: Arc<std::sync::Mutex<Option<egui::Context>>> = Arc::default();
    if !isolated {
        let wake = wake.clone();
        single_instance::listen_for_show_requests(move || {
            let _ = show_tx.send(());
            if let Some(ctx) = wake.lock().ok().and_then(|g| g.clone()) {
                ctx.request_repaint();
            }
        });
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Roch GPU")
            .with_app_id("RochGPU")
            .with_inner_size([windows::main_window::WIDTH, windows::main_window::MIN_HEIGHT])
            .with_min_inner_size([windows::main_window::WIDTH, windows::main_window::MIN_HEIGHT])
            .with_decorations(false)
            .with_resizable(false)
            .with_maximize_button(false)
            .with_icon(assets::window_icon()),
        renderer: eframe::Renderer::Glow,
        centered: true,
        persist_window: false,
        ..Default::default()
    };
    crate::trace!("gui: run_native");
    let result = eframe::run_native(
        "Roch GPU",
        options,
        Box::new(|cc| {
            crate::trace!("gui: eframe created (window + GL context)");
            let Some(Booted::Window { shared, minimized, vm }) = boot.wait() else {
                return Err("startup ended before the window opened".into());
            };
            crate::trace!("gui: driver startup joined");
            if let Ok(mut g) = wake.lock() {
                *g = Some(cc.egui_ctx.clone());
            }
            let app = app::RochApp::new(cc, shared, *vm, minimized, show_rx);
            crate::trace!("gui: app created");
            Ok(Box::new(app))
        }),
    );
    // The window can fail before it ever asked for the driver half (no OpenGL), which may still
    // be mid-apply: let it finish rather than exit under it.
    let _ = boot.wait();
    let exit_shared = match boot.outcome() {
        Some(Ok(shared)) => shared,
        Some(Err(code)) => return code,
        None => {
            // Only reachable if the driver half panicked, which the panic log has recorded.
            shared::log_line(&log_path, "Startup did not finish.");
            return 1;
        }
    };
    // Belt and braces for a window that never got to on_exit.
    exit_shared.service.release_fan_control();
    exit_shared.save_settings();
    match result {
        Ok(()) => 0,
        Err(e) => {
            exit_shared.log(&format!("Window failed: {e}"));
            native::message_box(&format!("{e}"), "Roch GPU error", native::Buttons::Ok, native::Icon::Error);
            1
        }
    }
}

/// The "no supported GPU" prompt: start the simulated GPU instead?
fn offer_mock(message: &str) -> bool {
    native::message_box(
        &format!("{message}\n\nStart with a simulated GPU instead (UI demo only)?"),
        "Roch GPU — no supported GPU",
        native::Buttons::YesNo,
        native::Icon::Warning,
    )
}

/// What the driver half of startup hands the window.
enum Booted {
    Exit(i32),
    Window { shared: Shared, minimized: bool, vm: Box<MainViewModel> },
}

/// The driver half of startup — backend detection and initialisation, the recovery journal, the
/// logon apply and the view model's first reads — on its own thread, so it overlaps eframe
/// creating the window and its OpenGL context (each ~90-100 ms on an RTX 5070 Ti, and until
/// now one after the other). The window is created hidden and stays so until its first frame,
/// which waits for this; the steps themselves run in the same order as before. The logon task's
/// `schtasks /Query` (a process launch) is started first, on a thread of its own.
struct Boot {
    handle: Mutex<Option<JoinHandle<Booted>>>,
    /// What the thread produced, once joined: the shared state, or the exit code.
    outcome: Mutex<Option<Result<Shared, i32>>>,
}

impl Boot {
    fn spawn(flags: Flags, store: Arc<ProfileStore>, settings: AppSettings) -> Self {
        let log_path = store.root_directory().join("roch-gpu.log");
        let tasks: Arc<dyn StartupTasks> = Arc::new(PrefetchedStartup::start(Arc::new(SchtasksStartup)));
        let spawned = std::thread::Builder::new().name("RochGPU startup".into()).spawn(move || {
            match startup::start(&flags, store, settings, tasks, offer_mock) {
                Started::Exit(code) => Booted::Exit(code),
                Started::Window { shared, minimized } => {
                    crate::trace!("gui: backend ready ({})", shared.service.backend_name());
                    let vm = Box::new(MainViewModel::new(shared.clone()));
                    crate::trace!("gui: view model");
                    Booted::Window { shared, minimized, vm }
                }
            }
        });
        let (handle, outcome) = match spawned {
            Ok(h) => (Some(h), None),
            Err(e) => {
                shared::log_line(&log_path, &format!("Could not start: {e}"));
                (None, Some(Err(1)))
            }
        };
        Self { handle: Mutex::new(handle), outcome: Mutex::new(outcome) }
    }

    /// Wait for the thread, the first time only (later calls return `None`). A panic on it is
    /// re-raised here, as it would have been when this ran on the main thread.
    fn wait(&self) -> Option<Booted> {
        let handle = self.handle.lock().ok()?.take()?;
        let booted = handle.join().unwrap_or_else(|e| std::panic::resume_unwind(e));
        if let Ok(mut o) = self.outcome.lock() {
            *o = Some(match &booted {
                Booted::Exit(code) => Err(*code),
                Booted::Window { shared, .. } => Ok(shared.clone()),
            });
        }
        Some(booted)
    }

    fn outcome(&self) -> Option<Result<Shared, i32>> {
        self.outcome.lock().ok()?.clone()
    }
}

/// Log panics to the app log, as the WPF unhandled-exception handlers did.
fn install_panic_log(path: std::path::PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        shared::log_line(&path, &format!("FATAL: {info}"));
        previous(info);
    }));
}
