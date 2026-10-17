//! Read-only static graphics-card details, matching Roch Viewer's Graphics section
//! (C# `GraphicsWindow`).

use eframe::egui::{self, Align, Context, CornerRadius, Layout, RichText, ViewportClass, ViewportId, vec2};
use roch_core::{GpuGraphicsInfo, GpuGraphicsRow};

use super::{ChildChrome, Placement, WinCtx, content_frame};
use crate::gui::theme::{self, BODY, pal};

pub struct GraphicsWindow {
    rows: Vec<GpuGraphicsRow>,
    chrome: ChildChrome,
}

impl GraphicsWindow {
    pub fn id() -> ViewportId {
        ViewportId::from_hash_of("roch_graphics")
    }

    /// Snapshot the card's identity once; it does not change while the app runs.
    pub fn new(info: &GpuGraphicsInfo) -> Self {
        Self { rows: info.rows(), chrome: ChildChrome::default() }
    }

    #[cfg(test)]
    pub fn rows(&self) -> &[GpuGraphicsRow] {
        &self.rows
    }

    pub fn focus(&mut self) {
        self.chrome.request_focus();
    }

    /// Draw it. Returns false once the user has closed it.
    pub fn show(&mut self, ctx: &Context, w: &mut WinCtx) -> bool {
        let builder = self
            .chrome
            .builder("Roch GPU — Graphics", vec2(620.0, 600.0), w.icon, Placement::RightOfOwner, w.main_rect)
            .with_min_inner_size(vec2(560.0, 480.0))
            .with_taskbar(false);
        ctx.show_viewport_immediate(Self::id(), builder, |ui, _class: ViewportClass| {
            let closed = self.chrome.frame(ui.ctx(), w.dark);
            content_frame(ui.ctx(), 0).inner_margin(egui::Margin { left: 14, right: 14, top: 12, bottom: 12 }).show(
                ui,
                |ui| {
                    ui.set_min_size(ui.available_size());
                    self.body(ui);
                },
            );
            !closed
        })
    }

    fn body(&self, ui: &mut egui::Ui) {
        let p = pal(ui.ctx());
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("Graphics").font(theme::bold(BODY)).color(p.text));
                ui.add_space(3.0);
                ui.label(RichText::new("Static hardware and driver information").font(theme::regular(BODY)));
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(4.0);
                egui::Frame::new()
                    .fill(p.panel)
                    .corner_radius(CornerRadius::same(5))
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| ui.label(RichText::new("READ ONLY").font(theme::bold(BODY)).color(p.text)));
            });
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("GRAPHICS").font(theme::bold(BODY)).color(p.brand));
        });
        ui.add_space(3.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for row in &self.rows {
                let fill = if row.is_banded { p.highlight } else { egui::Color32::TRANSPARENT };
                egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(8, 3)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_top(|ui| {
                        let (name_rect, _) = ui.allocate_exact_size(vec2(174.0, 15.0), egui::Sense::hover());
                        ui.painter().text(
                            name_rect.left_center(),
                            egui::Align2::LEFT_CENTER,
                            &row.name,
                            theme::regular(BODY),
                            p.text,
                        );
                        ui.add_space(8.0);
                        ui.add(
                            egui::Label::new(RichText::new(&row.value).font(theme::regular(BODY)).color(p.accent))
                                .wrap(),
                        );
                    });
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphics_binds_every_identity_row() {
        let info = GpuGraphicsInfo {
            gpu: "Intel Arc Pro B60".into(),
            board_manufacturer: "ASRock".into(),
            code_name: "Battlemage BMG-G21 WKSTN".into(),
            memory_type: "GDDR6".into(),
            driver_date: "2026-09-17".into(),
            ..Default::default()
        };
        let w = GraphicsWindow::new(&info);
        assert_eq!(w.rows().len(), 17);
        assert!(w.rows().iter().any(|r| r.value == "ASRock"));
    }

    #[test]
    fn graphics_follows_light_and_dark() {
        use crate::gui::theme::Palette;
        assert_ne!(Palette::for_mode(true).bg, Palette::for_mode(false).bg);
    }
}
