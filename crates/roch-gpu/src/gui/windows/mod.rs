//! One file per window. The main window draws into the root viewport; every other window is an
//! egui *immediate* viewport (a native OS window drawn inside the root's frame), so they all share
//! the one `MainViewModel` by `&mut` without locks — the WPF windows shared one DataContext the
//! same way.
//!
//! A child window is a struct holding its own UI state, owned as `Option<T>` by the app (`None` =
//! closed). Each frame the app calls `T::show(&mut self, ctx, &mut WinCtx) -> bool`; returning
//! `false` closes it. See `GUI_NOTES.md` for the contract and the [`Tap`] subscription helper.

pub mod curve;
pub mod fan;
pub mod graphics;
pub mod main_window;
pub mod monitor;
pub mod xoc;

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Weak};
use std::time::Duration;

use eframe::egui::{self, Context, IconData, Pos2, Rect, Vec2, ViewportBuilder, ViewportCommand, ViewportId};
use roch_core::GpuTelemetry;
use roch_core::services::{ListenerId, TuningService};

use super::shared::Shared;
use super::theme;
use super::view_model::MainViewModel;

/// What a child window gets each frame.
pub struct WinCtx<'a> {
    pub vm: &'a mut MainViewModel,
    /// For windows that call the service directly (Monitor, Fan, Curve).
    pub shared: &'a Shared,
    pub icon: &'a Arc<IconData>,
    pub dark: bool,
    /// The main window's outer rectangle in screen points, for placing windows beside it.
    pub main_rect: Option<Rect>,
}

/// Where a child window opens (WPF `WindowStartupLocation` / explicit Left/Top).
#[derive(Debug, Clone, Copy)]
pub enum Placement {
    /// Centred over the main window (`CenterOwner`).
    CenterOwner,
    /// To the right of the main window, tops aligned (`Left = Left + Width + 8`).
    RightOfOwner,
}

/// Builder for a decorated child window with the app icon, at `pos` when given.
fn child_builder(title: &str, size: Vec2, icon: &Arc<IconData>, pos: Option<Pos2>) -> ViewportBuilder {
    let b = ViewportBuilder::default().with_title(title).with_inner_size(size).with_icon(icon.clone());
    match pos {
        Some(p) => b.with_position(p),
        None => b,
    }
}

/// Where a window of `size` opens relative to the main window.
fn place(placement: Placement, size: Vec2, main: Rect) -> Pos2 {
    match placement {
        Placement::CenterOwner => main.center() - size / 2.0,
        Placement::RightOfOwner => Pos2::new(main.right() + 8.0, main.top()),
    }
}

/// Per-window bookkeeping every child needs: push the title-bar theme to Windows when the window
/// opens and whenever the mode flips, and report a close request.
#[derive(Debug, Default)]
pub struct ChildChrome {
    theme_sent: Option<bool>,
    /// Where the window opened. Worked out once: recomputing it from the main window every frame
    /// dragged the child along whenever the main window moved or changed height.
    placed: Option<Pos2>,
    focus_requested: bool,
    fitted: Option<Vec2>,
}

impl ChildChrome {
    /// The viewport builder: placed beside or over the main window when it opens, then left
    /// wherever the user puts it.
    pub fn builder(
        &mut self,
        title: &str,
        size: Vec2,
        icon: &Arc<IconData>,
        placement: Placement,
        main: Option<Rect>,
    ) -> ViewportBuilder {
        if self.placed.is_none() {
            self.placed = main.map(|m| place(placement, size, m));
        }
        child_builder(title, size, icon, self.placed)
    }

    /// Call at the top of the viewport's ui callback. Returns true when the user closed it.
    pub fn frame(&mut self, ctx: &Context, dark: bool) -> bool {
        if self.theme_sent != Some(dark) {
            ctx.send_viewport_cmd(ViewportCommand::SetTheme(theme::system_theme(dark)));
            self.theme_sent = Some(dark);
        }
        if std::mem::take(&mut self.focus_requested) {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
        ctx.input(|i| i.viewport().close_requested())
    }

    /// Bring the window forward on its next frame (clicking its button again).
    pub fn request_focus(&mut self) {
        self.focus_requested = true;
    }

    /// WPF `SizeToContent`: resize the window to `size` when the content's size changed.
    pub fn fit(&mut self, ctx: &Context, size: Vec2) {
        let size = Vec2::new(size.x.round(), size.y.round());
        if self.fitted.is_none_or(|s| (s - size).length() > 1.0) {
            self.fitted = Some(size);
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        }
    }

    /// Is this viewport the focused window?
    pub fn is_focused(ctx: &Context, id: ViewportId) -> bool {
        ctx.input(|i| i.raw.viewports.get(&id).and_then(|v| v.focused)).unwrap_or(false)
    }
}

/// The themed background frame for a child window's content.
pub fn content_frame(ctx: &Context, margin: i8) -> egui::Frame {
    let p = theme::pal(ctx);
    egui::Frame::new().fill(p.bg).inner_margin(egui::Margin::same(margin))
}

/// A window's own telemetry subscription (the WPF windows' `TelemetryUpdated += ...`), with the
/// extra driver reads it needs per sample done on the polling thread, never the UI thread: the
/// listener runs right after each foreground sample, outside the driver lock, so a slow read (or
/// a long Apply holding the lock) cannot freeze the window. Results arrive through [`Tap::drain`];
/// one-off reads ([`Tap::spawn`]) and worker-job results ([`Tap::sender`]) use the same channel.
/// Dropping the tap (closing the window) unsubscribes.
pub struct Tap<T> {
    rx: Receiver<T>,
    tx: Sender<T>,
    svc: Weak<TuningService>,
    ctx: Context,
    listener: Option<ListenerId>,
}

impl<T: Send + 'static> Tap<T> {
    /// A tap with no per-sample listener (results come from [`Self::spawn`] and jobs only).
    pub fn new(svc: &Arc<TuningService>, ctx: &Context) -> Self {
        let (tx, rx) = channel();
        Self { rx, tx, svc: Arc::downgrade(svc), ctx: ctx.clone(), listener: None }
    }

    /// Run `f` on the polling thread after every foreground sample.
    pub fn listen(
        svc: &Arc<TuningService>,
        ctx: &Context,
        f: impl Fn(&TuningService, &GpuTelemetry) -> T + Send + Sync + 'static,
    ) -> Self {
        let mut tap = Self::new(svc, ctx);
        let (tx, weak, wake) = (tap.tx.clone(), tap.svc.clone(), ctx.clone());
        tap.listener = Some(svc.add_telemetry_listener(move |t| {
            if let Some(svc) = weak.upgrade() {
                let _ = tx.send(f(&svc, t));
                crate::gui::wake(&wake, Duration::ZERO);
            }
        }));
        tap
    }

    /// Run a one-off read on its own thread.
    pub fn spawn(&self, f: impl FnOnce(&TuningService) -> T + Send + 'static) {
        let (tx, weak, wake) = (self.tx.clone(), self.svc.clone(), self.ctx.clone());
        let _ = std::thread::Builder::new().name("RochGPU read".into()).spawn(move || {
            if let Some(svc) = weak.upgrade() {
                let _ = tx.send(f(&svc));
                crate::gui::wake(&wake, Duration::ZERO);
            }
        });
    }

    /// Where a worker job posts its result for this window.
    pub fn sender(&self) -> Sender<T> {
        self.tx.clone()
    }

    /// Everything that arrived since the last frame, oldest first.
    pub fn drain(&self) -> Vec<T> {
        self.rx.try_iter().collect()
    }
}

impl<T> Drop for Tap<T> {
    fn drop(&mut self) {
        if let (Some(id), Some(svc)) = (self.listener.take(), self.svc.upgrade()) {
            svc.remove_listener(id);
        }
    }
}
