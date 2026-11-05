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
pub mod fonts;
pub mod format;
pub mod native;
pub mod preferences;
pub mod settings_window;
pub mod shared;
pub mod startup;
pub mod telemetry_table;
pub mod theme;
#[cfg(windows)]
pub mod tray;
#[cfg(not(windows))]
#[path = "tray_none.rs"]
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
            #[cfg(not(windows))]
            native::bring_back_from_scratchpad();
            let _ = show_tx.send(());
            if let Some(ctx) = wake.lock().ok().and_then(|g| g.clone()) {
                ctx.request_repaint();
            }
        });
    }

    // Linux waits for the driver half, because the window has to be the right size from its first
    // frame: Hyprland marks every window tiled, and winit then ignores a client's resize request, so
    // the fit-to-content the window does later never lands. Reading sysfs is quick enough not to miss.
    #[cfg(not(windows))]
    let mut ready = match boot.wait() {
        Some(Booted::Exit(code)) => return code,
        other => other,
    };
    #[cfg(not(windows))]
    let (width, height) = match &mut ready {
        Some(Booted::Window { shared, vm, .. }) => measured_size(shared, vm),
        _ => (windows::main_window::WIDTH, windows::main_window::MIN_HEIGHT),
    };
    #[cfg(windows)]
    let (width, height) = (windows::main_window::WIDTH, windows::main_window::MIN_HEIGHT);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Roch GPU")
            .with_app_id("RochGPU")
            .with_inner_size([width, height])
            // A fixed size is what tells a tiling compositor this window floats, and the minimum is
            // half of how it says so. The window scrolls if it is ever asked to be smaller.
            .with_min_inner_size(if cfg!(windows) { [420.0, 300.0] } else { [width, height] })
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
            #[cfg(not(windows))]
            let booted = ready.take();
            #[cfg(windows)]
            let booted = boot.wait();
            let Some(Booted::Window { shared, minimized, vm }) = booted else {
                return Err("startup ended before the window opened".into());
            };
            crate::trace!("gui: driver startup joined");
            if let Ok(mut g) = wake.lock() {
                *g = Some(cc.egui_ctx.clone());
            }
            #[cfg(not(windows))]
            close_on_signal(cc.egui_ctx.clone());
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
    exit_shared.service.release_soft_thermal_limit();
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

/// Close properly on a signal. A session ending or Ctrl+C in the terminal that started the window
/// arrives as one, and by default the process just dies, skipping the exit path that hands a software
/// fan curve back to the driver: the fan would stay at its last duty with nothing managing it. A signal
/// handler cannot touch the UI, so it raises a flag and this wakes the window to read it.
#[cfg(not(windows))]
fn close_on_signal(ctx: egui::Context) {
    crate::console::install_ctrl_c_handler();
    let _ = std::thread::Builder::new().name("RochGPU signal watch".into()).spawn(move || {
        while !crate::console::ctrl_c_pressed() {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        // A hidden window's UI thread may be waiting for a frame that will not come until it is shown.
        native::bring_back_from_scratchpad();
        ctx.request_repaint();
    });
}

/// The size the main window needs, from a layout pass with no window: egui is only a library until
/// something paints its output. Two passes, as egui itself takes to settle a layout on first sight.
#[cfg(not(windows))]
fn measured_size(shared: &Shared, vm: &mut MainViewModel) -> (f32, f32) {
    use windows::main_window;
    /// Roomy on a 1080p screen, and without a window there is no monitor yet to ask. Anything taller
    /// scrolls, which is what the window does whenever its content outgrows it.
    const TALLEST: f32 = 960.0;

    let settings = shared.settings().clone();
    let prefs = preferences::Prefs::from_json(&settings.ui_preferences, settings.dark_mode, settings.poll_interval_ms);
    let ctx = egui::Context::default();
    theme::install_fonts(&ctx, &prefs.font_family);
    theme::apply(&ctx, &prefs, prefs.theme.is_dark(native::system_dark()));
    ctx.set_zoom_factor(prefs.ui_scale);
    let width = main_window::width();
    let mut height = main_window::MIN_HEIGHT;
    for _ in 0..2 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 4096.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            height = main_window::show(ui, vm, None, &mut Vec::new());
        });
        // Nothing will ever paint this pass, and egui insists on being told so.
        output.textures_delta.clear();
    }
    (width, height.clamp(main_window::MIN_HEIGHT, TALLEST).round())
}

/// Wake the window for something another thread just delivered (a sample, a job's result): one
/// frame, `delay` from now. `Context::request_repaint` asks egui for two, the second a settling
/// pass for layout that reacts to input, which fresh readings are not; and with immediate
/// viewports, every root frame also repaints each open child window.
pub fn wake(ctx: &egui::Context, delay: std::time::Duration) {
    ctx.request_repaint_after(delay.max(std::time::Duration::from_millis(1)));
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
