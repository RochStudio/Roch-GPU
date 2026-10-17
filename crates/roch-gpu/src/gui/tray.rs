//! The notification-area icon (C# `MainWindow.SetupTray`): Show, Reset GPU to defaults, Exit, and
//! double-click to restore. Events arrive on whatever thread the shell uses; they are forwarded
//! to a channel and the UI thread is woken to handle them in `App::logic`, which still runs while
//! the window is hidden in the tray.

use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::Context;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// What the tray asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Show,
    Reset,
    Exit,
}

pub struct Tray {
    icon: TrayIcon,
    pub commands: Receiver<TrayCommand>,
    tooltip: String,
}

/// The 16 px frame of the embedded .ico: hand-tuned for the notification area, so it is worth
/// picking rather than downscaling the 256 px PNG. Falls back to the PNG.
fn tray_icon_image() -> Option<Icon> {
    let ico = super::assets::ICO;
    let rgba = super::assets::ico_frame_rgba(ico, 16).or_else(|| super::assets::png_rgba(super::assets::ICON_PNG))?;
    Icon::from_rgba(rgba.0, rgba.1, rgba.2).ok()
}

impl Tray {
    /// Create the icon. `None` if the shell refuses (the app then minimises normally).
    pub fn new(ctx: &Context) -> Option<Tray> {
        let (tx, rx) = channel::<TrayCommand>();
        let menu = Menu::new();
        let show = MenuItem::new("Show", true, None);
        let reset = MenuItem::new("Reset GPU to defaults", true, None);
        let exit = MenuItem::new("Exit", true, None);
        menu.append_items(&[&show, &reset, &PredefinedMenuItem::separator(), &exit]).ok()?;
        let ids = (show.id().clone(), reset.id().clone(), exit.id().clone());

        let mut builder = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("Roch GPU");
        if let Some(icon) = tray_icon_image() {
            builder = builder.with_icon(icon);
        }
        let icon = builder.build().ok()?;

        let wake = ctx.clone();
        let send = tx.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let cmd = if e.id == ids.0 {
                TrayCommand::Show
            } else if e.id == ids.1 {
                TrayCommand::Reset
            } else if e.id == ids.2 {
                TrayCommand::Exit
            } else {
                return;
            };
            let _ = send.send(cmd);
            wake.request_repaint();
        }));
        let wake = ctx.clone();
        let send: Sender<TrayCommand> = tx;
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = e {
                let _ = send.send(TrayCommand::Show);
                wake.request_repaint();
            }
        }));
        Some(Tray { icon, commands: rx, tooltip: "Roch GPU".into() })
    }

    /// Change the hover text (only when it differs, as the shell call is not free).
    pub fn set_tooltip(&mut self, text: &str) {
        if self.tooltip != text {
            self.tooltip = text.to_owned();
            let _ = self.icon.set_tooltip(Some(text));
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
        TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
        let _ = self.icon.set_visible(false);
    }
}
