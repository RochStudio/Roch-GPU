//! The eframe application: owns the view model, the child windows, the tray and the worker thread,
//! and carries out what the main window asks for (C# `MainWindow.xaml.cs`).
//!
//! Frame flow: `logic()` runs every frame *and* while the window is hidden in the tray — it drains
//! the tray, the "show yourself" requests from a second launch, the service's telemetry, and the
//! worker's finished jobs, and hands newly queued jobs to the worker. `ui()` draws the main window,
//! then each open child window as an immediate viewport, then any modal dialog.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{self, Context, IconData, TextureHandle, ViewportCommand, vec2};
use roch_core::services::ServiceEvent;

use super::native;
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
                ctx.request_repaint();
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
}

impl RochApp {
    pub fn new(cc: &eframe::CreationContext<'_>, shared: Shared, minimized: bool, show_requests: Receiver<()>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let dark = shared.settings().dark_mode;
        theme::install_fonts(&ctx);
        theme::apply(&ctx, dark);

        let vm = MainViewModel::new(shared.clone());
        let events = shared.service.subscribe();
        // Wake the UI for each sample; the sample itself travels through `events`.
        let wake = ctx.clone();
        shared.service.add_telemetry_listener(move |_| wake.request_repaint());
        // The monitor starts closed, so the poll starts in the background (no driver call unless a
        // fan curve needs a temperature).
        shared.service.set_background_mode(true);
        let interval = shared.settings().poll_interval_ms.max(250) as u64;
        if let Err(e) = shared.service.start_polling(interval) {
            shared.log(&format!("Polling could not start: {e}"));
        }

        let tray = Tray::new(&ctx);
        let worker = Worker::start(shared.clone(), ctx.clone());
        let auto_monitor = shared.settings().auto_open_monitor;
        let mut app = Self {
            shared,
            vm,
            dark,
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
                tray.set_tooltip(&format!(
                    "Roch GPU — {:.0}°C, {:.0} MHz, fan {:.0}%",
                    t.temperature_c, t.core_clock_mhz, t.fan_percent
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
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
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
            MainAction::ToggleTheme => {
                self.dark = !self.dark;
                theme::apply(ctx, self.dark);
                self.shared.settings().dark_mode = self.dark;
                self.shared.save_settings();
            }
            MainAction::Minimize => self.minimize_to_tray(ctx),
            MainAction::Close => self.request_close(ctx),
            MainAction::OpenUrl(url) => {
                // A browser that will not open is worth a status line, not a crash dialog.
                if let Err(e) = native::shell_open(url) {
                    self.vm.report(format!("Could not open {url}: {e}"), true);
                }
            }
            MainAction::ConfirmClearSlot(n) => self.dialog = Some(Dialog::ClearSlot(n)),
        }
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

impl eframe::App for RochApp {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
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
        let mut actions = Vec::new();
        let needed = main_window::show(ui, &mut self.vm, self.icon_tex.as_ref(), &mut actions);
        // SizeToContent="Height" with MinHeight="640".
        let want = needed.max(main_window::MIN_HEIGHT).round();
        if (want - self.content_height).abs() > 1.0 {
            self.content_height = want;
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(main_window::WIDTH, want)));
        }
        for a in actions {
            self.handle_action(&ctx, a);
        }
        self.show_children(&ctx);
        self.show_dialog(&ctx);
        // Jobs queued by this frame's clicks start now, not on the next event.
        self.dispatch_jobs();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::Palette::for_mode(self.dark).bg.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Hand fans back to the driver if this app was running a curve, then persist settings.
        self.shared.service.release_fan_control();
        let _ = self.shared.service.stop_polling();
        self.shared.save_settings();
    }
}
