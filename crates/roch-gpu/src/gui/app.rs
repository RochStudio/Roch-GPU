//! The eframe application: owns the view model, the child windows, the tray and the worker thread,
//! and carries out what the main window asks for (C# `MainWindow.xaml.cs`).
//!
//! Frame flow: `logic()` runs every frame *and* while the window is hidden in the tray — it drains
//! the tray, the "show yourself" requests from a second launch, the service's telemetry, and the
//! worker's finished jobs, and hands newly queued jobs to the worker. `ui()` draws the main window,
//! then each open child window as an immediate viewport, then any modal dialog.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, Context, IconData, TextureHandle, ViewportCommand, vec2};
use roch_core::services::ServiceEvent;

use super::bench::{self, Bench};
use super::native;
use super::preferences::{self, Prefs, ThemeMode};
use super::settings_window::SettingsWindow;
use super::shared::Shared;
use super::theme;
use super::tray::{Tray, TrayCommand};
use super::view_model::{Job, MainViewModel, Outcome};
use super::windows::curve::CurveWindow;
use super::windows::fan::FanWindow;
use super::windows::graphics::GraphicsWindow;
use super::windows::main_window::{self, MainAction};
use super::windows::monitor::MonitorWindow;
use super::windows::xoc::XocWindow;
use super::windows::{ChildChrome, WinCtx};

/// How long a sample waits for the open windows' own per-sample reads before it is drawn.
const SAMPLE_COALESCE: Duration = Duration::from_millis(30);

/// Runs view-model jobs one at a time, in order, off the UI thread.
struct Worker {
    jobs: Sender<Job>,
    done: Receiver<Outcome>,
}

impl Worker {
    fn start(shared: Shared, ctx: Context) -> Worker {
        let (jobs, rx) = channel::<Job>();
        let (tx, done) = channel::<Outcome>();
        let _ = std::thread::Builder::new().name("RochGPU worker".into()).spawn(move || {
            for job in rx {
                let outcome = job.run(&shared);
                if tx.send(outcome).is_err() {
                    break;
                }
                super::wake(&ctx, Duration::ZERO);
            }
        });
        Worker { jobs, done }
    }
}

/// A question the app is waiting on (the WPF MessageBoxes, as egui modals).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialog {
    ClearSlot(i32),
    /// Closing while this app is enforcing a fan curve or fixed duty.
    CloseWithFanControl,
}

/// The child windows that were open when the app went to the tray, reopened on restore.
#[derive(Debug, Default, Clone, Copy)]
struct HiddenChildren {
    monitor: bool,
    fan: bool,
}

pub struct RochApp {
    shared: Shared,
    vm: MainViewModel,
    dark: bool,
    prefs: Prefs,
    applied_font: String,
    poll_interval: u32,
    pending_preferences_save: Option<Instant>,
    last_system_check: Instant,
    last_position_sample: Instant,
    sampled_positions: BTreeMap<String, (i32, i32)>,
    root_position_restored: bool,
    window_maintenance: native::WindowMaintenance,
    settings_window: Option<SettingsWindow>,
    content_width: f32,
    icon: Arc<IconData>,
    icon_tex: Option<TextureHandle>,
    events: Receiver<ServiceEvent>,
    show_requests: Receiver<()>,
    worker: Worker,
    tray: Option<Tray>,
    hidden: bool,
    hidden_children: HiddenChildren,
    really_close: bool,
    dialog: Option<Dialog>,
    content_height: f32,
    monitor: Option<MonitorWindow>,
    fan: Option<FanWindow>,
    curve: Option<CurveWindow>,
    xoc: Option<XocWindow>,
    graphics: Option<GraphicsWindow>,
    /// The tray icon is made after the first frame (it is a few ms of shell calls the window
    /// need not wait for), unless the app starts in the tray.
    tray_due: bool,
    first_frame_drawn: bool,
    /// Root frames drawn (only advanced while tracing).
    frames: u64,
    bench: Option<Bench>,
}

impl RochApp {
    /// `vm` was built on the startup thread (`gui::Boot`), from the same `shared`.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        shared: Shared,
        vm: MainViewModel,
        minimized: bool,
        show_requests: Receiver<()>,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        let settings = shared.settings().clone();
        let prefs = Prefs::from_json(&settings.ui_preferences, settings.dark_mode, settings.poll_interval_ms);
        let dark = prefs.theme.is_dark(native::system_dark());
        theme::install_fonts(&ctx, &prefs.font_family);
        theme::apply(&ctx, &prefs, dark);
        ctx.set_zoom_factor(prefs.ui_scale);
        crate::trace!("app: fonts + theme");

        let events = shared.service.subscribe();
        // Wake the UI for each sample; the sample itself travels through `events`. The short
        // delay lets a window's own per-sample read (a `Tap`, which wakes on its own as soon as
        // it is done) land in the same frame instead of a second one.
        let wake = ctx.clone();
        shared.service.add_telemetry_listener(move |_| super::wake(&wake, SAMPLE_COALESCE));
        // And for each log line: the software temperature limit logs every ceiling it moves, also
        // while no sample is being taken, and the limiter line shows it.
        let wake = ctx.clone();
        shared.service.add_log_listener(move |_| super::wake(&wake, SAMPLE_COALESCE));
        // The monitor starts closed, so the poll starts in the background (no driver call unless a
        // fan curve needs a temperature).
        shared.service.set_background_mode(true);
        let interval = u64::from(prefs.refresh_ms);
        if let Err(e) = shared.service.start_polling(interval) {
            shared.log(&format!("Polling could not start: {e}"));
        }

        crate::trace!("app: polling started");
        let tray = if minimized { Tray::new(&ctx) } else { None };
        let worker = Worker::start(shared.clone(), ctx.clone());
        let auto_monitor = shared.settings().auto_open_monitor;
        // The benchmark only ever drives the simulated GPU.
        let bench = crate::trace::bench().filter(|_| shared.service.backend_name() == "Simulated GPU").map(Bench::new);
        let mut app = Self {
            shared,
            vm,
            dark,
            applied_font: prefs.font_family.clone(),
            poll_interval: prefs.refresh_ms,
            prefs,
            pending_preferences_save: None,
            last_system_check: Instant::now(),
            last_position_sample: Instant::now(),
            sampled_positions: BTreeMap::new(),
            root_position_restored: false,
            window_maintenance: native::WindowMaintenance::default(),
            settings_window: None,
            content_width: main_window::WIDTH,
            icon: super::assets::window_icon(),
            icon_tex: super::assets::icon_texture(&ctx),
            events,
            show_requests,
            worker,
            tray,
            hidden: false,
            hidden_children: HiddenChildren::default(),
            really_close: false,
            dialog: None,
            content_height: main_window::MIN_HEIGHT,
            monitor: None,
            fan: None,
            curve: None,
            xoc: None,
            graphics: None,
            tray_due: !minimized,
            first_frame_drawn: false,
            frames: 0,
            bench,
        };
        if minimized {
            app.minimize_to_tray(&ctx);
        }
        if auto_monitor {
            app.set_monitor_open(true);
        }
        app
    }

    // ------------------------------------------------------------------ tray / visibility

    /// Hide to the tray (there is no button for it: minimising is the gesture). The child windows
    /// that show live readings are closed and reopened on restore, so the poll can drop to its
    /// background rate while nothing is on screen; the service keeps stepping a fan curve.
    fn minimize_to_tray(&mut self, ctx: &Context) {
        if self.tray.is_none() {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
            return;
        }
        self.hidden_children = HiddenChildren { monitor: self.monitor.is_some(), fan: self.fan.is_some() };
        self.monitor = None;
        self.fan = None;
        self.curve = None;
        self.xoc = None;
        self.graphics = None;
        self.settings_window = None;
        self.update_poll_detail();
        self.hidden = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
    }

    fn restore_from_tray(&mut self, ctx: &Context) {
        if self.hidden {
            self.hidden = false;
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            let h = std::mem::take(&mut self.hidden_children);
            if h.monitor {
                self.set_monitor_open(true);
            }
            if h.fan {
                self.fan = Some(FanWindow::default());
                self.update_poll_detail();
            }
        }
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    /// Any window showing live readings keeps the poll in the foreground; with none open the poll
    /// makes no driver call at all unless a fan curve needs a temperature.
    fn update_poll_detail(&mut self) {
        let paused = self.monitor.is_none() && self.fan.is_none();
        self.shared.service.set_background_mode(paused);
        if paused {
            // Nothing will refresh these now; don't leave the last sample pretending to be current.
            self.vm.set_telemetry(None);
            if let Some(t) = &mut self.tray {
                t.set_tooltip("Roch GPU");
            }
        }
    }

    pub fn set_monitor_open(&mut self, open: bool) {
        if open {
            match &mut self.monitor {
                Some(m) => m.focus(),
                None => self.monitor = Some(MonitorWindow::default()),
            }
        } else {
            self.monitor = None;
        }
        self.update_poll_detail();
    }

    // ------------------------------------------------------------------ closing

    /// The close button, Alt+F4 or the tray's Exit. Asks first when closing would hand a fan
    /// curve this app is running back to the driver.
    fn request_close(&mut self, ctx: &Context) {
        let fan_active = self.vm.is_curve_fan() || self.vm.is_fixed_fan();
        if !self.really_close
            && self.tray.is_some()
            && !self.shared.service.capabilities().fan_curve_is_hardware
            && fan_active
        {
            if self.hidden {
                self.restore_from_tray(ctx);
            }
            self.dialog = Some(Dialog::CloseWithFanControl);
            return;
        }
        self.really_close = true;
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    // ------------------------------------------------------------------ per-frame plumbing

    fn pump(&mut self, ctx: &Context) {
        // Tray menu and double-click.
        let commands: Vec<TrayCommand> =
            self.tray.as_ref().map(|t| t.commands.try_iter().collect()).unwrap_or_default();
        for c in commands {
            match c {
                TrayCommand::Show => self.restore_from_tray(ctx),
                TrayCommand::Reset => self.vm.reset(),
                TrayCommand::Exit => {
                    self.really_close = true;
                    self.request_close(ctx);
                }
            }
        }
        // A second launch asking this one to come forward.
        if self.show_requests.try_iter().count() > 0 {
            self.restore_from_tray(ctx);
        }
        // Service events: the latest foreground sample feeds the limiter line and the tray text.
        let mut latest = None;
        for e in self.events.try_iter() {
            if let ServiceEvent::Telemetry(t) = e {
                if let Some(f) = &mut self.fan {
                    f.on_telemetry(&t);
                }
                latest = Some(t);
            }
        }
        // A sample can already be queued when polling pauses; drop it rather than show it stale.
        if let Some(t) = latest
            && !self.shared.service.background_mode()
        {
            if let Some(tray) = &mut self.tray {
                // C# custom "0" formats: exact ties round away from zero.
                let n = |v: f64| roch_core::util::dotnet_custom(v, 0, 0);
                tray.set_tooltip(&format!(
                    "Roch GPU — {}°C, {} MHz, fan {}%",
                    n(t.temperature_c),
                    n(t.core_clock_mhz),
                    n(t.fan_percent)
                ));
            }
            self.vm.set_telemetry(Some((*t).clone()));
        }
        // Finished jobs, then newly queued ones.
        for outcome in self.worker.done.try_iter().collect::<Vec<_>>() {
            self.vm.finish(outcome);
        }
        self.dispatch_jobs();
        if self.vm.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn dispatch_jobs(&mut self) {
        for job in self.vm.take_jobs() {
            if let Err(e) = self.worker.jobs.send(job) {
                // The worker died: run it here rather than lose a write the user asked for.
                let outcome = e.0.run(&self.shared);
                self.vm.finish(outcome);
            }
        }
    }

    fn handle_action(&mut self, ctx: &Context, action: MainAction) {
        match action {
            MainAction::OpenCurve => match &mut self.curve {
                Some(w) => w.focus(),
                None => self.curve = Some(CurveWindow::default()),
            },
            MainAction::OpenXoc => match &mut self.xoc {
                Some(w) => w.focus(),
                None => self.xoc = Some(XocWindow::default()),
            },
            MainAction::OpenFan => {
                match &mut self.fan {
                    Some(w) => w.focus(),
                    None => self.fan = Some(FanWindow::default()),
                }
                self.update_poll_detail(); // it wants live readings
            }
            MainAction::ToggleMonitor => {
                // Opens it, or brings it forward; clicking again while it has focus closes it.
                if self.monitor.is_none() {
                    self.set_monitor_open(true);
                } else if ChildChrome::is_focused(ctx, MonitorWindow::id()) {
                    self.set_monitor_open(false);
                } else if let Some(m) = &mut self.monitor {
                    m.focus();
                }
            }
            MainAction::OpenGraphics => match &mut self.graphics {
                Some(w) => w.focus(),
                None => self.graphics = Some(GraphicsWindow::new(&self.shared.service.read_graphics_info())),
            },
            MainAction::OpenLog => {
                let path = self.shared.log_path();
                if !path.exists() {
                    let _ = std::fs::write(&path, "");
                }
                if let Err(e) = native::shell_open(&path.to_string_lossy()) {
                    self.vm.report(format!("Could not open the log: {e}"), true);
                }
            }
            MainAction::OpenSettings => match &mut self.settings_window {
                Some(window) => window.focus(),
                None => self.settings_window = Some(SettingsWindow::default()),
            },
            MainAction::Minimize => self.minimize_to_tray(ctx),
            MainAction::Close => self.request_close(ctx),
            MainAction::OpenUrl(url) => {
                // A browser that will not open is worth a status line, not a crash dialog.
                if let Err(e) = native::shell_open(url) {
                    self.vm.report(format!("Could not open {url}: {e}"), true);
                }
            }
            MainAction::SelectGpu(index) => self.select_gpu(ctx, index),
            MainAction::ConfirmClearSlot(n) => self.dialog = Some(Dialog::ClearSlot(n)),
        }
    }

    /// Move the whole window to another card of the same backend.
    fn select_gpu(&mut self, ctx: &Context, index: usize) {
        if index == self.vm.gpu_index() {
            return;
        }
        if self.vm.busy() {
            self.vm.report("Wait for the current apply to finish before switching GPU.", true);
            return;
        }
        if self.vm.pending_changes() {
            self.vm.report("Apply or Reset your changes before switching GPU.", true);
            return;
        }
        if let Err(e) = self.vm.switch_gpu(index) {
            self.vm.report(format!("Could not switch GPU: {e}"), true);
            return;
        }
        // These were built around the old card's sensors, capabilities or curve.
        self.fan = None;
        self.curve = None;
        self.xoc = None;
        self.graphics = None;
        if let Some(monitor) = &mut self.monitor {
            monitor.reset_for_new_gpu();
        }
        self.update_poll_detail();
        self.vm.report(format!("Switched to {}", self.vm.device_display_name()), false);
        ctx.request_repaint();
    }

    fn show_children(&mut self, ctx: &Context) {
        let main_rect = ctx.input(|i| i.viewport().outer_rect);
        let mut w = WinCtx { vm: &mut self.vm, shared: &self.shared, icon: &self.icon, dark: self.dark, main_rect };
        let mut poll_changed = false;
        if let Some(win) = &mut self.xoc
            && !win.show(ctx, &mut w)
        {
            self.xoc = None;
        }
        if let Some(win) = &mut self.graphics
            && !win.show(ctx, &mut w)
        {
            self.graphics = None;
        }
        if let Some(win) = &mut self.curve
            && !win.show(ctx, &mut w)
        {
            self.curve = None;
        }
        if let Some(win) = &mut self.fan
            && !win.show(ctx, &mut w)
        {
            self.fan = None;
            poll_changed = true;
        }
        if let Some(win) = &mut self.monitor
            && !win.show(ctx, &mut w)
        {
            self.monitor = None;
            poll_changed = true;
        }
        if poll_changed {
            self.update_poll_detail();
        }
        if self.settings_window.is_none() {
            return;
        }
        let before = self.prefs.clone();
        let catalog = self
            .monitor
            .as_ref()
            .map(MonitorWindow::sensor_catalog)
            .filter(|catalog| !catalog.is_empty())
            .unwrap_or_else(|| {
                vec![
                    ("temp".into(), "Temperatures / GPU".into()),
                    ("core".into(), "Clocks / GPU core".into()),
                    ("watts".into(), "Power / Board draw".into()),
                ]
            });
        if let Some(window) = &mut self.settings_window
            && !window.show(ctx, &mut self.prefs, &self.shared, &self.icon, main_rect, &catalog)
        {
            self.settings_window = None;
        }
        if self.prefs != before {
            self.apply_preferences(ctx);
        }
    }

    fn apply_preferences(&mut self, ctx: &Context) {
        self.dark = self.prefs.theme.is_dark(self.prefs.theme == ThemeMode::System && native::system_dark());
        if self.applied_font != self.prefs.font_family {
            theme::install_fonts(ctx, &self.prefs.font_family);
            self.applied_font = self.prefs.font_family.clone();
        }
        theme::apply(ctx, &self.prefs, self.dark);
        if ctx.zoom_factor() != self.prefs.ui_scale {
            ctx.set_zoom_factor(self.prefs.ui_scale);
        }
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(if self.prefs.always_on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        }));
        self.pending_preferences_save = Some(Instant::now());
        ctx.request_repaint();
    }

    fn save_preferences(&self) {
        {
            let mut settings = self.shared.settings();
            settings.ui_preferences = self.prefs.to_json();
            settings.dark_mode = self.dark;
            settings.poll_interval_ms = self.prefs.refresh_ms as i32;
        }
        self.shared.save_settings();
    }

    fn preferences_tick(&mut self, ctx: &Context) {
        self.window_maintenance.update(ctx, "Roch GPU", self.prefs.opacity);
        if !self.root_position_restored {
            self.root_position_restored = true;
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(if self.prefs.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }));
            if self.prefs.remember_positions
                && let Some(position) = self.prefs.positions.get("Roch GPU")
                && let Some(at) = native::position_points(*position, ctx.zoom_factor())
            {
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(at));
            }
        }
        if self.prefs.theme == ThemeMode::System {
            if self.last_system_check.elapsed() >= Duration::from_secs(2) {
                self.last_system_check = Instant::now();
                if native::system_dark() != self.dark {
                    self.apply_preferences(ctx);
                }
            }
            ctx.request_repaint_after(Duration::from_secs(2));
        }
        if self.prefs.remember_positions && self.last_position_sample.elapsed() >= Duration::from_secs(1) {
            self.last_position_sample = Instant::now();
            let mut changed = false;
            for title in [
                "Roch GPU",
                "Roch GPU — Settings",
                "Roch GPU — Telemetry",
                "Roch GPU — Graphics",
                "XOC",
                "Fan Control",
                "Voltage / Frequency curve",
            ] {
                if let Some(position) = native::window_position(title)
                    && self.sampled_positions.insert(title.into(), position) != Some(position)
                    && self.prefs.positions.get(title) != Some(&position)
                {
                    self.prefs.positions.insert(title.into(), position);
                    changed = true;
                }
            }
            if changed {
                preferences::publish(ctx, &self.prefs);
                self.pending_preferences_save.get_or_insert_with(Instant::now);
            }
        }
        if let Some(since) = self.pending_preferences_save {
            if since.elapsed() >= Duration::from_millis(350) {
                self.pending_preferences_save = None;
                if self.poll_interval != self.prefs.refresh_ms {
                    let interval = self.prefs.refresh_ms;
                    self.poll_interval = interval;
                    self.vm.queue_custom("Updating refresh interval…", move |shared| {
                        let result = shared.service.start_polling(u64::from(interval));
                        Box::new(move |vm| {
                            if let Err(error) = result {
                                vm.report(format!("Polling could not restart: {error}"), true);
                            }
                        })
                    });
                }
                self.save_preferences();
            } else {
                ctx.request_repaint_after(Duration::from_millis(350));
            }
        }
    }

    fn show_dialog(&mut self, ctx: &Context) {
        let Some(dialog) = self.dialog else { return };
        let p = theme::pal(ctx);
        let mut answer: Option<u8> = None; // 0 = yes, 1 = no, 2 = cancel
        let text = match dialog {
            Dialog::ClearSlot(n) => format!("Clear slot {n}?"),
            Dialog::CloseWithFanControl => "Fan control is active. Minimize to tray to keep it running?\n\nYes = minimize to tray\nNo = exit (fans return to automatic)".to_string(),
        };
        let modal = egui::Modal::new(egui::Id::unique("roch_dialog"))
            .frame(egui::Frame::popup(&ctx.global_style()).fill(p.bg).inner_margin(egui::Margin::same(16)))
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.label(egui::RichText::new("Roch GPU").font(theme::bold(theme::BODY)));
                ui.add_space(8.0);
                ui.label(text);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    use super::widgets::{ButtonKind, button};
                    let size = vec2(80.0, 26.0);
                    if button(ui, "Yes", ButtonKind::Primary, size, true, true).clicked() {
                        answer = Some(0);
                    }
                    if button(ui, "No", ButtonKind::Normal, size, true, false).clicked() {
                        answer = Some(1);
                    }
                    if dialog == Dialog::CloseWithFanControl
                        && button(ui, "Cancel", ButtonKind::Normal, size, true, false).clicked()
                    {
                        answer = Some(2);
                    }
                });
            });
        if modal.should_close() && answer.is_none() {
            answer = Some(if dialog == Dialog::CloseWithFanControl { 2 } else { 1 });
        }
        let Some(a) = answer else { return };
        self.dialog = None;
        match (dialog, a) {
            (Dialog::ClearSlot(n), 0) => self.vm.clear_slot(n),
            (Dialog::CloseWithFanControl, 0) => self.minimize_to_tray(ctx),
            (Dialog::CloseWithFanControl, 1) => {
                self.really_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            _ => {}
        }
    }
}

impl RochApp {
    /// One trace line per root frame: the main window's own time, and the whole frame including
    /// every open child viewport (each of which is laid out, painted and swapped inside it).
    fn trace_frame(&mut self, ctx: &Context, total: Duration, main: Duration) {
        self.frames += 1;
        let open: Vec<&str> = [
            (self.monitor.is_some(), "telemetry"),
            (self.fan.is_some(), "fan"),
            (self.curve.is_some(), "curve"),
            (self.xoc.is_some(), "xoc"),
            (self.graphics.is_some(), "graphics"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        crate::trace!(
            "frame #{}: {:.2} ms (main {:.2} ms) open [{}]",
            self.frames,
            total.as_secs_f64() * 1000.0,
            main.as_secs_f64() * 1000.0,
            open.join(" ")
        );
        if self.frames == 1 {
            crate::trace!("first frame drawn");
            ctx.request_repaint(); // so the next frame can stamp "presented"
        }
    }

    fn bench_step(&mut self, ctx: &Context, step: bench::Step) {
        use bench::{Step, Win};
        match step {
            Step::Open(Win::Monitor) => self.set_monitor_open(true),
            Step::Close(Win::Monitor) => self.set_monitor_open(false),
            Step::Open(Win::Curve) => self.handle_action(ctx, MainAction::OpenCurve),
            Step::Open(Win::Xoc) => self.handle_action(ctx, MainAction::OpenXoc),
            Step::Open(Win::Fan) => self.handle_action(ctx, MainAction::OpenFan),
            Step::Open(Win::Graphics) => self.handle_action(ctx, MainAction::OpenGraphics),
            Step::Close(Win::Curve) => self.curve = None,
            Step::Close(Win::Xoc) => self.xoc = None,
            Step::Close(Win::Fan) => {
                self.fan = None;
                self.update_poll_detail();
            }
            Step::Close(Win::Graphics) => self.graphics = None,
            Step::Exit => {
                self.really_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }
}

impl eframe::App for RochApp {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Comma)) {
            self.handle_action(ctx, MainAction::OpenSettings);
        }
        self.preferences_tick(ctx);
        if self.frames == 1 && crate::trace::enabled() {
            crate::trace!("first frame presented");
        }
        if let Some(mut b) = self.bench.take() {
            let (step, wait) = b.tick(self.frames);
            ctx.request_repaint_after(wait);
            if step != Some(bench::Step::Exit) {
                self.bench = Some(b);
            }
            if let Some(step) = step {
                self.bench_step(ctx, step);
            }
        }
        if self.tray_due && self.first_frame_drawn {
            self.tray_due = false;
            self.tray = Tray::new(ctx);
            crate::trace!("app: tray");
        }
        self.pump(ctx);

        // Alt+F4, or the taskbar's Close: run our own close path instead.
        if ctx.input(|i| i.viewport().close_requested()) && !self.really_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.request_close(ctx);
        }
        // Minimised from the taskbar (Win+D, clicking the button): go to the tray as the WPF
        // StateChanged handler did.
        if !self.hidden && ctx.input(|i| i.viewport().minimized) == Some(true) && self.tray.is_some() {
            self.minimize_to_tray(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let started = crate::trace::enabled().then(std::time::Instant::now);
        let mut actions = Vec::new();
        let needed = main_window::show(ui, &mut self.vm, self.icon_tex.as_ref(), &mut actions);
        let main_done = started.map(|t| t.elapsed());
        // SizeToContent="Height" with MinHeight="640".
        let max_height =
            ctx.input(|i| i.viewport().monitor_size.map(|size| (size.y - 80.0).max(300.0))).unwrap_or(f32::MAX);
        let want = needed.max(main_window::MIN_HEIGHT).min(max_height).round();
        let max_width =
            ctx.input(|i| i.viewport().monitor_size.map(|size| (size.x - 40.0).max(420.0))).unwrap_or(f32::MAX);
        let width = main_window::width().min(max_width);
        if (want - self.content_height).abs() > 1.0 || (width - self.content_width).abs() > 1.0 {
            self.content_height = want;
            self.content_width = width;
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(width, want)));
        }
        for a in actions {
            self.handle_action(&ctx, a);
        }
        self.show_children(&ctx);
        self.show_dialog(&ctx);
        // Jobs queued by this frame's clicks start now, not on the next event.
        self.dispatch_jobs();
        if !self.first_frame_drawn {
            self.first_frame_drawn = true;
            if self.tray_due {
                ctx.request_repaint(); // the next frame makes the tray icon
            }
        }
        if let (Some(t), Some(main)) = (started, main_done) {
            self.trace_frame(&ctx, t.elapsed(), main);
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::Palette::from_preferences(&self.prefs, self.dark).bg.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Hand fans back to the driver if this app was running a curve, and the clock if the
        // software temperature limit was holding it down; then persist settings.
        self.shared.service.release_fan_control();
        self.shared.service.release_soft_thermal_limit();
        let _ = self.shared.service.stop_polling();
        self.save_preferences();
    }
}
