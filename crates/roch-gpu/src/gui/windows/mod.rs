//! One file per window. The main window draws into the root viewport; every other window is an
//! egui *immediate* viewport (a native OS window drawn inside the root's frame), so they all share
//! the one `MainViewModel` by `&mut` without locks — the WPF windows shared one DataContext the
//! same way.
//!
//! A child window is a struct holding its own UI state, owned as `Option<T>` by the app (`None` =
//! closed). Each frame the app calls `T::show(&mut self, ctx, &mut WinCtx) -> bool`; returning
//! `false` closes it. See `GUI_NOTES.md` for the extension contract part 2 fills in.

pub mod curve;
pub mod fan;
pub mod graphics;
pub mod main_window;
pub mod monitor;
pub mod xoc;

use std::sync::Arc;

use eframe::egui::{self, Context, IconData, Pos2, Rect, Vec2, ViewportBuilder, ViewportCommand, ViewportId};

use super::shared::Shared;
use super::theme;
use super::view_model::MainViewModel;

/// What a child window gets each frame.
pub struct WinCtx<'a> {
    pub vm: &'a mut MainViewModel,
    /// For windows that call the service directly (Monitor/Fan/Curve, part 2).
    #[allow(dead_code)]
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

/// Builder for a decorated child window with the app icon.
pub fn child_builder(
    title: &str,
    size: Vec2,
    icon: &Arc<IconData>,
    placement: Placement,
    main: Option<Rect>,
) -> ViewportBuilder {
    let mut b = ViewportBuilder::default().with_title(title).with_inner_size(size).with_icon(icon.clone());
    if let Some(m) = main {
        let pos = match placement {
            Placement::CenterOwner => m.center() - size / 2.0,
            Placement::RightOfOwner => Pos2::new(m.right() + 8.0, m.top()),
        };
        b = b.with_position(pos);
    }
    b
}

/// Per-window bookkeeping every child needs: push the title-bar theme to Windows when the window
/// opens and whenever the mode flips, and report a close request.
#[derive(Debug, Default)]
pub struct ChildChrome {
    theme_sent: Option<bool>,
    focus_requested: bool,
    fitted: Option<Vec2>,
}

impl ChildChrome {
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
