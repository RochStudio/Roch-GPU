//! No notification-area icon on Linux. Wayland lets no application hide its own window, so an icon
//! could not do the one thing it is for: stand in for a window that has gone away. [`Tray::new`]
//! declining is the case the app already handles for a shell that refuses the icon, and it keeps the
//! binary free of GTK, which the command line on a server has no use for.

use std::sync::mpsc::Receiver;

use eframe::egui::Context;

/// What the tray asked for. Never sent here; the app's handling of it is shared with Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code, reason = "constructed only by the Windows tray")]
pub enum TrayCommand {
    Show,
    Reset,
    Exit,
}

pub struct Tray {
    pub commands: Receiver<TrayCommand>,
}

impl Tray {
    pub fn new(_ctx: &Context) -> Option<Tray> {
        None
    }

    pub fn set_tooltip(&mut self, _text: &str) {}
}
