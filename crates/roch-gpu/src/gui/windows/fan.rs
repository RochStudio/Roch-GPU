//! Fan control (C# `FanWindow` + `Controls/FanCurveEditor`). **Stub — part 2 ports it.**
//!
//! Contract with the app (see `GUI_NOTES.md`):
//! * while open, polling runs in the foreground (the window shows live duty/RPM), and samples
//!   arrive through [`FanWindow::on_telemetry`];
//! * fan state lives in the view model: `fan_mode_index`, `per_fan_percents`, `editor_curve(_mut)`,
//!   `mark_curve_dirty`, `sync_fans_from_service`, `save_fans_to_active_slot`, `zero_rpm`;
//! * writes go through `TuningService::set_fans` — run them as a worker job, not on the UI thread.

use eframe::egui::{Context, RichText, ViewportClass, ViewportId, vec2};
use roch_core::GpuTelemetry;

use super::{ChildChrome, Placement, WinCtx, child_builder, content_frame};
use crate::gui::theme::{self, BODY};

#[derive(Default)]
pub struct FanWindow {
    chrome: ChildChrome,
}

impl FanWindow {
    pub fn id() -> ViewportId {
        ViewportId::from_hash_of("roch_fan")
    }

    pub fn focus(&mut self) {
        self.chrome.request_focus();
    }

    /// A foreground telemetry sample, delivered on the UI thread.
    pub fn on_telemetry(&mut self, _t: &GpuTelemetry) {}

    pub fn show(&mut self, ctx: &Context, w: &mut WinCtx) -> bool {
        let builder = child_builder("Fan Control", vec2(560.0, 697.0), w.icon, Placement::CenterOwner, w.main_rect)
            .with_min_inner_size(vec2(460.0, 300.0));
        ctx.show_viewport_immediate(Self::id(), builder, |ui, _class: ViewportClass| {
            let closed = self.chrome.frame(ui.ctx(), w.dark);
            content_frame(ui.ctx(), 12).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.label(RichText::new("Fan Control").font(theme::bold(BODY)));
                ui.label("Fan window: not ported yet (part 2).");
            });
            !closed
        })
    }
}
