//! The window half (C# `GpuTuner.App`): an eframe/egui port of the WPF GUI.
//!
//! Module map (details in `GUI_NOTES.md`):
//! * [`startup`] — flags, backend detection, recovery journal, logon apply (App.OnStartup)
//! * [`shared`] — the service/store/settings/recovery bundle and the log (App statics)
//! * [`view_model`] — MainViewModel, UI-free and unit-tested
//! * [`app`] — the eframe App: windows, tray, worker thread, dialogs (MainWindow.xaml.cs)
//! * [`windows`] — one file per window; Monitor/Fan/Curve are stubs for part 2
//! * [`theme`], [`widgets`], [`format`], [`assets`], [`tray`], [`native`]

pub mod app;
pub mod assets;
pub mod format;
pub mod native;
pub mod shared;
pub mod startup;
pub mod theme;
pub mod tray;
pub mod view_model;
pub mod widgets;
pub mod windows;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::sync::mpsc::channel;

use eframe::egui;
use roch_core::services::ProfileStore;

use crate::single_instance;
use shared::SchtasksStartup;
use startup::{Flags, Started};

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

    // Claimed here rather than in the entry point because the elevated relaunch is a new process:
    // whichever copy actually runs the window is the one that should hold the handle.
    if !single_instance::claim() {
        single_instance::ask_existing_to_show();
        return 0;
    }

    install_panic_log(store.root_directory().join("roch-gpu.log"));
    let started = startup::start(&flags, store, settings, Arc::new(SchtasksStartup), |message| {
        native::message_box(
            &format!("{message}\n\nStart with a simulated GPU instead (UI demo only)?"),
            "Roch GPU — no supported GPU",
            native::Buttons::YesNo,
            native::Icon::Warning,
        )
    });
    let (shared, minimized) = match started {
        Started::Exit(code) => return code,
        Started::Window { shared, minimized } => (shared, minimized),
    };

    // A second launch cannot open a window of its own, so it asks this one to come forward.
    let (show_tx, show_rx) = channel::<()>();
    let wake: Arc<std::sync::Mutex<Option<egui::Context>>> = Arc::default();
    {
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
    let exit_shared = shared.clone();
    let result = eframe::run_native(
        "Roch GPU",
        options,
        Box::new(move |cc| {
            if let Ok(mut g) = wake.lock() {
                *g = Some(cc.egui_ctx.clone());
            }
            Ok(Box::new(app::RochApp::new(cc, shared, minimized, show_rx)))
        }),
    );
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

/// Log panics to the app log, as the WPF unhandled-exception handlers did.
fn install_panic_log(path: std::path::PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        shared::log_line(&path, &format!("FATAL: {info}"));
        previous(info);
    }));
}
