//! The gated levers (C# `XocWindow`): NVVDD and MSVDD rail ranges, the two OCP current limits and
//! the clock range. Each has one Enable/Disable button that writes that lever to the card at once
//! (or puts it back); its controls are dead while it is disarmed, because a slider that moves but
//! is never written would claim a setting the card has not got. Shares the main view model, so the
//! values are the ones a saved profile carries.

use eframe::egui::{self, Align, Context, Layout, RichText, Ui, ViewportClass, ViewportId, vec2};
use roch_core::XocLever;

use super::{ChildChrome, Placement, WinCtx, content_frame};
use crate::gui::theme::{self, BODY, pal};
use crate::gui::view_model::{Field, MainViewModel};
use crate::gui::widgets::{self, ButtonKind};

const WIDTH: f32 = 430.0;
const MIN_HEIGHT: f32 = 600.0;

#[derive(Default)]
pub struct XocWindow {
    chrome: ChildChrome,
}

impl XocWindow {
    pub fn id() -> ViewportId {
        ViewportId::from_hash_of("roch_xoc")
    }

    pub fn focus(&mut self) {
        self.chrome.request_focus();
    }

    pub fn show(&mut self, ctx: &Context, w: &mut WinCtx) -> bool {
        let builder = self
            .chrome
            .builder("XOC", vec2(WIDTH, 767.0), w.icon, Placement::CenterOwner, w.main_rect)
            .with_resizable(false)
            .with_minimize_button(false)
            .with_maximize_button(false);
        let vm = &mut *w.vm;
        ctx.show_viewport_immediate(Self::id(), builder, |ui, _class: ViewportClass| {
            let closed = self.chrome.frame(ui.ctx(), w.dark);
            let resp = content_frame(ui.ctx(), 12).show(ui, |ui| {
                ui.set_width(WIDTH - 24.0);
                body(ui, vm);
            });
            let h = (resp.response.rect.height()).max(MIN_HEIGHT);
            self.chrome.fit(ui.ctx(), vec2(WIDTH, h));
            !closed
        })
    }
}

fn body(ui: &mut Ui, vm: &mut MainViewModel) {
    let p = pal(ui.ctx());
    let busy = vm.busy();
    // Header: what is armed, and what is live.
    egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("XOC").font(theme::bold(BODY)).color(p.text));
        ui.add_space(8.0);
        ui.add(egui::Label::new(RichText::new(vm.xoc_status_text()).font(theme::regular(BODY))).wrap());
    });
    ui.add_space(8.0);

    egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if vm.has_voltage_rail() {
            range_lever(
                ui,
                vm,
                "NVVDD Range (mV)",
                (Field::VoltageRailFloor, Field::VoltageRailMax),
                XocLever::NVVDD,
                "Write this rail range to the card now, or put it back",
                busy,
            );
        }
        if vm.has_msvdd_rail() {
            range_lever(
                ui,
                vm,
                "MSVDD Range (mV)",
                (Field::MsvddRailFloor, Field::MsvddRailMax),
                XocLever::MSVDD,
                "Write this rail range to the card now, or put it back",
                busy,
            );
        }
        if vm.has_ocp() {
            ocp_lever(ui, vm, "NVVDD OCP Limit (A)", Field::NvvddOcpAmps, XocLever::NVVDD_OCP, busy);
            ocp_lever(ui, vm, "MSVDD OCP Limit (A)", Field::MsvddOcpAmps, XocLever::MSVDD_OCP, busy);
        }
        if vm.has_clock_lock() {
            range_lever(
                ui,
                vm,
                "Clock Range (MHz)",
                (Field::ClockLockMin, Field::ClockLockMax),
                XocLever::CLOCK_RANGE,
                "Pin the graphics clock inside this window now, or put it back",
                busy,
            );
            // The thumbs keep showing the user's own range; say what is holding the clock lower.
            if let Some(note) = vm.soft_thermal_clock_range_note() {
                ui.add(egui::Label::new(RichText::new(note).font(theme::regular(BODY)).color(p.warn)).wrap());
                ui.add_space(6.0);
            }
        }
    });

    // One warning for the window rather than a paragraph per lever.
    ui.add_space(8.0);
    egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 7)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let mut job = egui::text::LayoutJob::default();
        let fmt = |bold: bool| egui::TextFormat {
            font_id: if bold { theme::bold(BODY) } else { theme::regular(BODY) },
            color: p.warn,
            ..Default::default()
        };
        job.append("These can damage hardware, not just crash it.", 0.0, fmt(true));
        job.append(
            " Nothing here is sanity-checked by the driver, and raising an OCP limit takes away the card's own protection. Arm one at a time and test between each. Nothing here survives a reboot.",
            0.0,
            fmt(false),
        );
        job.wrap.max_width = ui.available_width();
        ui.add(egui::Label::new(job).wrap());
    });
}

/// "Enable" while disarmed, "Disable" while armed: the button names what pressing it will do.
fn gate(ui: &mut Ui, vm: &mut MainViewModel, lever: XocLever, tip: &str, busy: bool) {
    let label = if vm.is_armed(lever) { "Disable" } else { "Enable" };
    ui.add_space(3.0);
    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
        let r = widgets::button(ui, label, ButtonKind::Primary, vec2(64.0, 21.0), !busy, true).on_hover_text(tip);
        if r.clicked() {
            vm.toggle_lever(lever);
        }
    });
}

fn row_label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::bold(BODY)));
}

fn range_lever(
    ui: &mut Ui,
    vm: &mut MainViewModel,
    title: &str,
    (lo, hi): (Field, Field),
    lever: XocLever,
    tip: &str,
    busy: bool,
) {
    ui.add_space(2.0);
    let armed = vm.is_armed(lever);
    ui.horizontal(|ui| {
        row_label(ui, title);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            widgets::value_box(ui, vm, hi, vec2(46.0, 20.0), armed).on_hover_text(vm.range_text(hi));
            widgets::value_box(ui, vm, lo, vec2(46.0, 20.0), armed).on_hover_text(vm.range_text(lo));
        });
    });
    ui.add_space(2.0);
    let w = ui.available_width();
    widgets::range_slider(ui, vm, lo, hi, w, armed);
    gate(ui, vm, lever, tip, busy);
    ui.add_space(6.0);
}

fn ocp_lever(ui: &mut Ui, vm: &mut MainViewModel, title: &str, f: Field, lever: XocLever, busy: bool) {
    ui.add_space(2.0);
    let armed = vm.is_armed(lever);
    ui.horizontal(|ui| {
        row_label(ui, title);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            widgets::value_box(ui, vm, f, vec2(60.0, 20.0), armed).on_hover_text(vm.range_text(f));
        });
    });
    ui.add_space(1.0);
    let w = ui.available_width();
    widgets::nudged_slider(ui, vm, f, w, armed, true);
    gate(ui, vm, lever, "Write this current limit to the card now, or put it back", busy);
    ui.add_space(6.0);
}
