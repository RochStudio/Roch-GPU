//! The V/F curve editor (C# `CurveWindow` + `Controls/VfCurveEditor`). **Stub — part 2 ports it.**
//!
//! Contract with the app (see `GUI_NOTES.md`): opened centred over the main window; it edits
//! `Field::TargetVoltage` / `Field::CoreOffset` in the shared view model, and its driver calls
//! (`read_vf_curve`, `set_vf_curve_targets`, `set_voltage_lock`) belong on a worker thread.

use eframe::egui::{Context, RichText, ViewportClass, ViewportId, vec2};

use super::{ChildChrome, Placement, WinCtx, child_builder, content_frame};
use crate::gui::theme::{self, BODY};

#[derive(Default)]
pub struct CurveWindow {
    chrome: ChildChrome,
}

impl CurveWindow {
    pub fn id() -> ViewportId {
        ViewportId::from_hash_of("roch_curve")
    }

    pub fn focus(&mut self) {
        self.chrome.request_focus();
    }

    pub fn show(&mut self, ctx: &Context, w: &mut WinCtx) -> bool {
        let builder =
            child_builder("Voltage / Frequency curve", vec2(960.0, 620.0), w.icon, Placement::CenterOwner, w.main_rect)
                .with_min_inner_size(vec2(700.0, 460.0));
        ctx.show_viewport_immediate(Self::id(), builder, |ui, _class: ViewportClass| {
            let closed = self.chrome.frame(ui.ctx(), w.dark);
            content_frame(ui.ctx(), 12).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.label(RichText::new("Voltage / Frequency curve").font(theme::bold(BODY)));
                ui.label("V/F curve editor: not ported yet (part 2).");
            });
            !closed
        })
    }
}
