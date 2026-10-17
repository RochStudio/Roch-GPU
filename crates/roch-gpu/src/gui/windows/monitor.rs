//! The hardware monitor (C# `MonitorWindow` + `TelemetryTable`). **Stub — part 2 ports it.**
//!
//! Contract with the app (see `GUI_NOTES.md`):
//! * the app keeps `Option<MonitorWindow>`; while it is `Some`, polling runs in the foreground
//!   (`TuningService::set_background_mode(false)`), and every foreground sample is handed to
//!   [`MonitorWindow::on_telemetry`] on the UI thread before the frame is drawn;
//! * the window opens to the right of the main window (`Placement::RightOfOwner`);
//! * clicking Telemetry again focuses it, or closes it when it already has focus.

use eframe::egui::{self, Context, RichText, ViewportClass, ViewportId, vec2};
use roch_core::GpuTelemetry;

use super::{ChildChrome, Placement, WinCtx, child_builder, content_frame};
use crate::gui::theme::{self, BODY};

#[derive(Default)]
pub struct MonitorWindow {
    chrome: ChildChrome,
    /// Latest sample (part 2 replaces this with the telemetry table and graphs).
    latest: Option<GpuTelemetry>,
}

impl MonitorWindow {
    pub fn id() -> ViewportId {
        ViewportId::from_hash_of("roch_monitor")
    }

    pub fn focus(&mut self) {
        self.chrome.request_focus();
    }

    /// A foreground telemetry sample, delivered on the UI thread.
    pub fn on_telemetry(&mut self, t: &GpuTelemetry) {
        self.latest = Some(t.clone());
    }

    pub fn show(&mut self, ctx: &Context, w: &mut WinCtx) -> bool {
        let builder =
            child_builder("Roch GPU — Telemetry", vec2(800.0, 900.0), w.icon, Placement::RightOfOwner, w.main_rect)
                .with_min_inner_size(vec2(740.0, 480.0));
        let device = w.vm.telemetry_device_name();
        ctx.show_viewport_immediate(Self::id(), builder, |ui, _class: ViewportClass| {
            let closed = self.chrome.frame(ui.ctx(), w.dark);
            content_frame(ui.ctx(), 12).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.label(RichText::new(&device).font(theme::bold(BODY)));
                ui.add_space(6.0);
                let line = match &self.latest {
                    Some(t) => format!(
                        "{:.0} °C · {:.0} MHz core · {:.0} MHz memory · fan {:.0}%",
                        t.temperature_c, t.core_clock_mhz, t.memory_clock_mhz, t.fan_percent
                    ),
                    None => "Waiting for the first sample…".into(),
                };
                ui.label(line);
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Telemetry table and graphs: not ported yet (part 2).").italics());
            });
            !closed
        })
    }
}
