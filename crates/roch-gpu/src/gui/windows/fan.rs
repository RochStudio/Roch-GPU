//! Fan control in its own window (C# `FanWindow` + `Controls/FanCurveEditor`): the mode, a duty
//! per fan, AMD Zero RPM, and the temperature → duty curve editor.
//!
//! Writes on Apply rather than on every drag: a fan slider dragged from 30 to 90 would otherwise
//! send sixty separate cooler writes on the way past. Apply touches only the fans (someone in here
//! is deciding about fans, not asking for a half-finished clock edit to be applied too), runs on
//! the worker thread, then brings the main window's fan controls and the active profile slot into
//! line. A software curve keeps being stepped by the service's poll after this window closes (in
//! the background poll when nothing is on screen); a hardware curve is run by the driver.
//!
//! While open, polling runs in the foreground, and samples arrive through
//! [`FanWindow::on_telemetry`] for the per-fan duty/RPM captions.

use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, Align, Align2, Color32, Context, CursorIcon, Id, Layout, Rect, RichText, Sense, Shape, Stroke, Ui,
    ViewportClass, ViewportId, pos2, vec2,
};
use roch_core::models::clock_step;
use roch_core::{FanCurve, FanMode, FanPoint, GpuCapabilities, GpuTelemetry, VoltageControlStyle, only_notes};

use super::{ChildChrome, Placement, WinCtx, content_frame};
use crate::gui::fan_editor::{self, FanEditor, FanGeom, parse_number};
use crate::gui::format::one_decimal;
use crate::gui::theme::{self, BODY, Palette, pal};
use crate::gui::view_model::{Field, MainViewModel};
use crate::gui::widgets::{self, ButtonKind};

const WIDTH: f32 = 560.0;
const EDITOR_HEIGHT: f32 = 240.0;
/// The step the arrows take and the grid the sliders snap to: the main window's fan step, so the
/// two cannot drift apart.
const STEP: i32 = clock_step::FAN_PERCENT;
/// egui's double-click window.
const DOUBLE_CLICK_S: f64 = 0.3;

/// What Apply reported, posted back from the worker.
type StatusSlot = Arc<Mutex<Option<(String, bool)>>>;

#[derive(Default)]
pub struct FanWindow {
    chrome: ChildChrome,
    loaded: bool,
    /// 0 Auto, 1 Fixed, 2 Curve — local until Apply, as the WPF radio buttons were.
    mode: i32,
    sliders: Vec<i32>,
    slider_range: (i32, i32),
    link: bool,
    readouts: Vec<String>,
    editor: FanEditor,
    hysteresis: String,
    min_step: String,
    /// One (temperature, duty) box pair per point, in the editor's order.
    point_rows: Vec<(String, String)>,
    status: String,
    status_error: bool,
    posted: StatusSlot,
    last_press: Option<(f64, egui::Pos2)>,
}

impl FanWindow {
    pub fn id() -> ViewportId {
        ViewportId::from_hash_of("roch_fan")
    }

    pub fn focus(&mut self) {
        self.chrome.request_focus();
    }

    /// A foreground telemetry sample, delivered on the UI thread: the live readings caption each
    /// fan's slider, so the duties can be compared against what the fans are actually doing.
    pub fn on_telemetry(&mut self, t: &GpuTelemetry) {
        for (i, r) in self.readouts.iter_mut().enumerate() {
            let duty = t.fan_percents.get(i).copied().unwrap_or(t.fan_percent);
            let rpm = t.fan_rpms.get(i).copied().unwrap_or(t.fan_rpm);
            *r = readout(duty, rpm);
        }
    }

    /// Seed everything from the view model the first time the window is drawn.
    fn load(&mut self, vm: &MainViewModel) {
        let caps = &vm.caps;
        let count = caps.fan_count.max(1) as usize;
        let max = if caps.fan_max_percent <= 0 { 100 } else { caps.fan_max_percent };
        self.slider_range = (caps.fan_min_percent, max.max(caps.fan_min_percent));
        self.sliders = (0..count)
            .map(|i| {
                let v = vm.per_fan_percents().get(i).copied().unwrap_or_else(|| vm.get(Field::FixedFan));
                v.clamp(self.slider_range.0, self.slider_range.1)
            })
            .collect();
        self.readouts = vec!["—".into(); count];
        self.link = true;
        self.editor.set_points(&vm.editor_curve().points);
        self.editor.set_max_points(hardware_point_limit(caps));
        self.hysteresis = one_decimal(vm.editor_curve().hysteresis_c);
        self.min_step = one_decimal(vm.editor_curve().minimum_step_percent);
        self.mode = vm.fan_mode_index().clamp(0, 2);
        self.loaded = true;
    }

    pub fn show(&mut self, ctx: &Context, w: &mut WinCtx) -> bool {
        if !self.loaded {
            self.load(w.vm);
        }
        if let Some((text, err)) = self.posted.lock().unwrap_or_else(|e| e.into_inner()).take() {
            self.status = text;
            self.status_error = err;
        }
        let builder = self
            .chrome
            .builder("Fan Control", vec2(WIDTH, 697.0), w.icon, Placement::CenterOwner, w.main_rect)
            .with_min_inner_size(vec2(460.0, 200.0));
        ctx.show_viewport_immediate(Self::id(), builder, |ui, _class: ViewportClass| {
            let closed = self.chrome.frame(ui.ctx(), w.dark);
            let width = ui.ctx().input(|i| i.viewport().inner_rect.map(|r| r.width())).unwrap_or(WIDTH);
            let resp = content_frame(ui.ctx(), 12).show(ui, |ui| {
                ui.set_width(width - 24.0);
                self.body(ui, w);
            });
            // SizeToContent="Height".
            self.chrome.fit(ui.ctx(), vec2(width, resp.response.rect.height()));
            !closed
        })
    }

    fn body(&mut self, ui: &mut Ui, w: &mut WinCtx) {
        let p = pal(ui.ctx());
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let caps = w.vm.caps.clone();

        // Mode first: it decides which of the panels below is worth looking at.
        panel(ui, 12, 10, |ui| {
            section_title(ui, "FAN CONTROL");
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                for (i, (name, tip)) in [
                    ("Auto", "The driver's own fan behaviour"),
                    ("Fixed", "Hold a duty, whatever the temperature does"),
                    ("Curve", "Follow the temperature-to-speed curve."),
                ]
                .into_iter()
                .enumerate()
                {
                    if widgets::radio(ui, self.mode == i as i32, name, true).on_hover_text(tip).clicked() {
                        self.mode = i as i32;
                    }
                }
            });
            ui.add_space(7.0);
            wrapped(ui, &mode_note(self.mode, &caps, w.vm.has_zero_rpm()), p.text);
            if w.vm.has_zero_rpm() {
                ui.add_space(12.0);
                let mut z = w.vm.zero_rpm();
                if widgets::checkbox(ui, &mut z, "Zero RPM (fans stop at idle)", true)
                    .on_hover_text(
                        "Allow idle fan stop where supported by the driver and selected fan mode. Press Apply to save.",
                    )
                    .changed()
                {
                    w.vm.set_zero_rpm(z);
                }
            }
        });
        ui.add_space(8.0);

        match self.mode {
            1 => panel(ui, 12, 10, |ui| self.fixed_panel(ui)),
            2 => panel(ui, 12, 10, |ui| self.curve_panel(ui, w.vm, &caps)),
            _ => {}
        }
        if self.mode != 0 {
            ui.add_space(8.0);
        }

        // Status and Apply.
        panel(ui, 10, 7, |ui| {
            ui.horizontal(|ui| {
                let busy = w.vm.busy();
                let label = "Apply";
                let bw =
                    (ui.painter().layout_no_wrap(label.into(), theme::bold(BODY), p.text).size().x + 30.0).max(90.0);
                let text_w = (ui.available_width() - bw - 8.0).max(50.0);
                ui.allocate_ui_with_layout(vec2(text_w, 0.0), Layout::left_to_right(Align::Center), |ui| {
                    wrapped(ui, &self.status, if self.status_error { p.danger } else { p.muted });
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::button(ui, label, ButtonKind::Primary, vec2(bw, 25.0), !busy, true)
                        .on_hover_text("Write the fan settings to the card now")
                        .clicked()
                    {
                        self.apply(w.vm);
                    }
                });
            });
        });
    }

    fn fixed_panel(&mut self, ui: &mut Ui) {
        let p = pal(ui.ctx());
        ui.horizontal(|ui| {
            section_title(ui, "FIXED SPEED");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut link = self.link;
                if widgets::checkbox(ui, &mut link, "Move together", true)
                    .on_hover_text("One slider drives them all. Untick to set each fan on its own.")
                    .clicked()
                {
                    self.link = link;
                    if link && let Some(&first) = self.sliders.first() {
                        self.sliders.iter_mut().for_each(|s| *s = first);
                    }
                }
            });
        });
        let count = self.sliders.len();
        ui.add_space(2.0);
        let note = if count > 1 {
            format!("This card reports {count} fans, each addressed separately by the driver.")
        } else {
            "This card reports one fan.".into()
        };
        wrapped(ui, &note, p.text);
        ui.add_space(6.0);

        for i in 0..count {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let name = if count > 1 { format!("Fan {}", i + 1) } else { "Fan speed".into() };
                ui.label(RichText::new(name).font(theme::bold(BODY)).color(p.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(&self.readouts[i]).font(theme::regular(BODY)).color(p.text));
                });
            });
            ui.add_space(1.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let (min, max) = self.slider_range;
                let mut v = self.sliders[i];
                let mut moved = false;
                if widgets::nudge_button(ui, "\u{25C0}", true) {
                    v = (v - STEP).clamp(min, max);
                    moved = true;
                }
                let sw = (ui.available_width() - 18.0 - 4.0).max(20.0);
                moved |= widgets::value_slider(ui, &mut v, self.slider_range, STEP, sw, true);
                if widgets::nudge_button(ui, "\u{25B6}", true) {
                    v = (v + STEP).clamp(min, max);
                    moved = true;
                }
                if moved {
                    // Linked sliders move as one; unlinked, each is its own fan.
                    if self.link {
                        self.sliders.iter_mut().for_each(|s| *s = v);
                    } else {
                        self.sliders[i] = v;
                    }
                }
            });
        }
        ui.add_space(15.0);
    }

    fn curve_panel(&mut self, ui: &mut Ui, vm: &mut MainViewModel, caps: &GpuCapabilities) {
        let p = pal(ui.ctx());
        ui.horizontal(|ui| {
            section_title(ui, "FAN CURVE");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if widgets::text_button(ui, "Default curve", ButtonKind::Normal, vec2(8.0, 3.0), true).clicked() {
                    vm.editor_curve_mut().points = FanCurve::default_points();
                    self.editor.set_points(&vm.editor_curve().points);
                    vm.mark_curve_dirty();
                }
                // A curve the driver runs has no hysteresis or step of ours to tune.
                if !caps.fan_curve_is_hardware {
                    let muted = |ui: &mut Ui, t: &str| {
                        ui.label(RichText::new(t).font(theme::regular(BODY)).color(p.text));
                    };
                    ui.add_space(12.0);
                    muted(ui, "%");
                    ui.add_space(4.0);
                    let step = widgets::text_box(
                        ui,
                        &mut self.min_step,
                        Id::unique("fan_min_step"),
                        vec2(36.0, 22.0),
                        false,
                        true,
                    );
                    ui.add_space(6.0);
                    muted(ui, "°C   Min step");
                    ui.add_space(6.0);
                    let hyst = widgets::text_box(
                        ui,
                        &mut self.hysteresis,
                        Id::unique("fan_hyst"),
                        vec2(36.0, 22.0),
                        false,
                        true,
                    );
                    ui.add_space(6.0);
                    muted(ui, "Hysteresis");
                    if hyst.changed() || step.changed() {
                        // Applied on every keystroke, as the WPF TextChanged handler did.
                        if let Some(h) = parse_number(&self.hysteresis) {
                            vm.editor_curve_mut().hysteresis_c = h.clamp(0.0, 20.0);
                        }
                        if let Some(s) = parse_number(&self.min_step) {
                            vm.editor_curve_mut().minimum_step_percent = s.clamp(0.0, 20.0);
                        }
                        vm.mark_curve_dirty();
                    }
                }
            });
        });
        ui.add_space(4.0);

        let edit = self.curve_editor(ui);
        if edit.curve {
            vm.editor_curve_mut().points = self.editor.points().to_vec();
            vm.mark_curve_dirty();
        }

        ui.add_space(6.0);
        wrapped(
            ui,
            "Drag a point, or select one and type its temperature and duty. Double-click the plot to add a point, right-click one to remove it. One curve drives every fan.",
            p.text,
        );
        ui.add_space(8.0);
        self.point_rows(ui, vm);
    }

    /// Every point, not just the one selected: a curve is read as a set of pairs, and comparing
    /// them means seeing them together.
    fn point_rows(&mut self, ui: &mut Ui, vm: &mut MainViewModel) {
        let p = pal(ui.ctx());
        let n = self.editor.points().len();
        ui.horizontal(|ui| {
            section_title(ui, "POINTS");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let hint = match self.editor.max_points() {
                    Some(m) => format!("{n} of {m} points  ·  Enter to apply"),
                    None => format!("{n} points  ·  Enter to apply"),
                };
                ui.label(RichText::new(hint).font(theme::regular(BODY)).color(p.text));
            });
        });
        ui.add_space(2.0);
        self.point_rows.resize(n, (String::new(), String::new()));
        for i in 0..n {
            let (tid, fid) = (Id::unique(("fan_point", i, 0)), Id::unique(("fan_point", i, 1)));
            // Never overwrite the box under the caret: its value is half-typed.
            let focus = ui.memory(|m| (m.has_focus(tid), m.has_focus(fid)));
            let pt = self.editor.points()[i];
            if !focus.0 {
                self.point_rows[i].0 = format!("{:.0}", pt.temperature_c);
            }
            if !focus.1 {
                self.point_rows[i].1 = format!("{:.0}", pt.fan_percent);
            }
            ui.add_space(1.0);
            let mut commit = false;
            ui.horizontal(|ui| {
                let unit = |ui: &mut Ui, t: &str| {
                    ui.add_space(5.0);
                    ui.label(RichText::new(t).font(theme::regular(BODY)).color(p.text));
                    ui.add_space(14.0);
                };
                let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                ui.painter().text(
                    r.left_center(),
                    Align2::LEFT_CENTER,
                    (i + 1).to_string(),
                    theme::regular(BODY),
                    p.text,
                );
                for (k, id) in [(0, tid), (1, fid)] {
                    let row = &mut self.point_rows[i];
                    let text = if k == 0 { &mut row.0 } else { &mut row.1 };
                    let resp = widgets::text_box(ui, text, id, vec2(52.0, 20.0), true, true);
                    // Typing into a row points at that point, so the plot highlights it too.
                    if resp.gained_focus() {
                        self.editor.select_point(i);
                    }
                    commit |= resp.lost_focus() || widgets::enter_pressed(ui, &resp);
                    unit(ui, if k == 0 { "°C" } else { "%" });
                }
            });
            ui.add_space(1.0);
            if commit {
                self.commit_point(i, vm);
            }
        }
    }

    /// Send one row to the editor, then read the point back into it: a temperature is held
    /// between its neighbours, so the number that lands is not always the number typed.
    fn commit_point(&mut self, i: usize, vm: &mut MainViewModel) {
        let Some((t, f)) = self.point_rows.get(i) else { return };
        if self.editor.try_update_point(i, parse_number(t), parse_number(f)) {
            vm.editor_curve_mut().points = self.editor.points().to_vec();
            vm.mark_curve_dirty();
        }
        if let (Some(pt), Some(row)) = (self.editor.points().get(i), self.point_rows.get_mut(i)) {
            row.0 = format!("{:.0}", pt.temperature_c);
            row.1 = format!("{:.0}", pt.fan_percent);
        }
    }

    /// The plot: grid, curve, points, the dragged point's readout; and its pointer handling.
    fn curve_editor(&mut self, ui: &mut Ui) -> fan_editor::FanEdit {
        let p = pal(ui.ctx());
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), EDITOR_HEIGHT), Sense::click_and_drag());
        let g = FanGeom { w: f64::from(rect.width()), h: f64::from(rect.height()) };
        let local = |pos: egui::Pos2| (f64::from(pos.x - rect.left()), f64::from(pos.y - rect.top()));
        let mut edit = fan_editor::FanEdit::default();

        let (pressed, secondary, down, moving, released, pos, now) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.secondary_pressed(),
                i.pointer.primary_down(),
                i.pointer.is_moving(),
                i.pointer.primary_released(),
                i.pointer.interact_pos(),
                i.time,
            )
        });
        let inside = pos.is_some_and(|at| rect.contains(at)) && resp.hovered();
        if let Some(at) = pos {
            if pressed && inside {
                let double =
                    self.last_press.is_some_and(|(t, p0)| now - t <= DOUBLE_CLICK_S && (at - p0).length() < 6.0);
                self.last_press = if double { None } else { Some((now, at)) };
                let (x, y) = local(at);
                let e = self.editor.press(&g, x, y, double);
                edit.curve |= e.curve;
                edit.selection |= e.selection;
            } else if down && moving && self.editor.dragging().is_some() {
                let (x, y) = local(at);
                let e = self.editor.drag_to(&g, x, y);
                edit.curve |= e.curve;
            }
            if secondary && inside {
                let (x, y) = local(at);
                edit.curve |= self.editor.remove_at(&g, x, y).curve;
            }
        }
        if released || !down {
            self.editor.release();
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        }

        paint_fan_curve(ui, rect, &g, &self.editor, &p);
        edit
    }

    /// Apply: write the mode, duties, curve and Zero RPM on the worker, then sync the main
    /// window's controls and the active slot.
    fn apply(&mut self, vm: &mut MainViewModel) {
        let mode = match self.mode {
            2 => FanMode::Curve,
            1 => FanMode::Fixed,
            _ => FanMode::Auto,
        };
        let duties: Vec<i32> = if self.link || self.sliders.len() < 2 { Vec::new() } else { self.sliders.clone() };
        let percent = self.sliders.first().copied().unwrap_or(50);
        let curve = vm.editor_curve().clone_config();
        let zero = vm.has_zero_rpm().then(|| vm.zero_rpm());
        let hardware = vm.caps.fan_curve_is_hardware;
        let describe = describe(mode, &duties, percent, hardware);
        let posted = Arc::clone(&self.posted);
        vm.queue_custom("Applying fans…", move |sh| {
            let errs = sh.service.set_fans(mode, percent, &duties, &curve, zero);
            Box::new(move |vm: &mut MainViewModel| {
                let ok = errs.is_empty() || only_notes(&errs);
                let mut text = if errs.is_empty() { describe } else { errs.join("  |  ") };
                if ok {
                    // Keep the main window's fan controls telling the same story...
                    vm.sync_fans_from_service(mode, percent, &duties);
                    // ...and the saved profile: without this the fans are the one setting you can
                    // apply, watch work, reboot, and find gone.
                    let saved = vm.save_fans_to_active_slot(mode, percent, &duties, &curve);
                    text.push_str("  ·  ");
                    text.push_str(&saved);
                }
                *posted.lock().unwrap_or_else(|e| e.into_inner()) = Some((text, !ok));
            })
        });
    }
}

/// A driver-run curve holds at most `fan_curve_points` points (Intel's table size, AMD's five).
fn hardware_point_limit(caps: &GpuCapabilities) -> Option<usize> {
    (caps.fan_curve_is_hardware && caps.fan_curve_points >= 2).then_some(caps.fan_curve_points as usize)
}

/// The per-fan caption: "45 %   ·   1200 rpm".
fn readout(duty: f64, rpm: f64) -> String {
    let duty_text = if duty.is_finite() { format!("{duty:.0} %") } else { "—".into() };
    if rpm.is_finite() { format!("{duty_text}   ·   {rpm:.0} rpm") } else { duty_text }
}

/// The line under the mode buttons.
fn mode_note(mode: i32, caps: &GpuCapabilities, has_zero_rpm: bool) -> String {
    let mut s: String = match mode {
        2 if caps.fan_curve_is_hardware => {
            "The driver runs this curve after Apply, even when the window closes.".into()
        }
        2 => "This app runs the curve and must stay open for it to hold.".into(),
        1 => "Held whatever the temperature does. The card's own thermal protection still applies.".into(),
        _ if has_zero_rpm => {
            "The driver controls fan speed. Press Apply to use Auto and the selected Zero RPM setting.".into()
        }
        _ => "The driver controls fan speed. Press Apply to use Auto.".into(),
    };
    if mode == 2 && caps.voltage_style == VoltageControlStyle::Percent {
        s.push_str(&format!(
            " Use 2–{} points within 25–100°C, with fan speeds that stay level or increase.",
            caps.fan_curve_points
        ));
    }
    s
}

/// The status line after a successful write (C# `Describe`).
fn describe(mode: FanMode, duties: &[i32], percent: i32, hardware_curve: bool) -> String {
    match mode {
        FanMode::Auto => "Fans handed back to the driver.".into(),
        FanMode::Curve if hardware_curve => "Curve saved to the driver.".into(),
        FanMode::Curve => "Curve running. It stops if this app closes.".into(),
        FanMode::Fixed if duties.len() > 1 => format!(
            "Set: {}",
            duties.iter().enumerate().map(|(i, d)| format!("fan {} {d}%", i + 1)).collect::<Vec<_>>().join(", ")
        ),
        FanMode::Fixed => format!("All fans at {percent}%."),
    }
}

/// A flat section (`Panel`): no box, just padding.
fn panel(ui: &mut Ui, x: i8, y: i8, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new().inner_margin(egui::Margin::symmetric(x, y)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

fn section_title(ui: &mut Ui, text: &str) {
    let p = pal(ui.ctx());
    ui.label(RichText::new(text).font(theme::bold(BODY)).color(p.text));
}

fn wrapped(ui: &mut Ui, text: &str, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).font(theme::regular(BODY)).color(color)).wrap());
}

/// Paint the fan curve editor (C# `FanCurveEditor.OnRender`).
fn paint_fan_curve(ui: &Ui, rect: Rect, g: &FanGeom, e: &FanEditor, p: &Palette) {
    use fan_editor::{PAD_B, PAD_L, PAD_R, PAD_T, T_MAX, T_MIN};
    if rect.width() < 40.0 || rect.height() < 40.0 {
        return;
    }
    let painter = ui.painter_at(rect);
    let at = |x: f64, y: f64| pos2(rect.left() + x as f32, rect.top() + y as f32);
    let (w, h) = (g.w, g.h);
    let grid = Stroke::new(1.0, p.border);
    let font = theme::regular(BODY);

    // Horizontal grid (fan %).
    for f in (0..=100).step_by(20) {
        let y = g.to_screen(FanPoint::new(0.0, f64::from(f))).1.round() + 0.5;
        painter.line_segment([at(PAD_L, y), at(w - PAD_R, y)], grid);
        painter.text(at(PAD_L - 5.0, y), Align2::RIGHT_CENTER, format!("{f}%"), font.clone(), p.text);
    }
    // Vertical grid (temperature).
    for t in (0..=110).step_by(10) {
        let x = g.to_screen(FanPoint::new(f64::from(t), 0.0)).0.round() + 0.5;
        painter.line_segment([at(x, PAD_T), at(x, h - PAD_B)], grid);
        if t % 20 == 0 {
            painter.text(at(x, h - PAD_B + 4.0), Align2::CENTER_TOP, format!("{t}°"), font.clone(), p.text);
        }
    }

    // The curve, extended flat to the left and right edges as the driver does.
    let pts = e.points();
    if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
        let mut line = vec![g.to_screen(FanPoint::new(T_MIN, first.fan_percent))];
        line.extend(pts.iter().map(|q| g.to_screen(*q)));
        line.push(g.to_screen(FanPoint::new(T_MAX, last.fan_percent)));
        let line: Vec<_> = line.into_iter().map(|(x, y)| at(x, y)).collect();
        painter.add(Shape::line(line, Stroke::new(2.0, p.accent)));
    }

    // Points: the dragged or selected one filled and larger.
    for (i, q) in pts.iter().enumerate() {
        let (x, y) = g.to_screen(*q);
        let live = Some(i) == e.dragging() || Some(i) == e.selected();
        let fill = if live { p.accent } else { p.panel };
        painter.circle(at(x, y), if live { 6.0 } else { 5.0 }, fill, Stroke::new(2.0, p.accent));
        if Some(i) == e.dragging() {
            let label = format!("{:.0}°C → {:.0}%", q.temperature_c, q.fan_percent);
            let galley = painter.layout_no_wrap(label, font.clone(), p.text);
            let tx = (x + 8.0).min(w - f64::from(galley.size().x) - 4.0);
            let ty = PAD_T.max(y - 18.0);
            painter.galley(at(tx, ty), galley, p.text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts() {
        assert_eq!(readout(45.0, 1200.0), "45 %   ·   1200 rpm");
        assert_eq!(readout(f64::NAN, f64::NAN), "—");
        assert_eq!(describe(FanMode::Fixed, &[60, 75], 60, false), "Set: fan 1 60%, fan 2 75%");
        assert_eq!(describe(FanMode::Fixed, &[], 65, false), "All fans at 65%.");
        assert_eq!(describe(FanMode::Curve, &[], 65, true), "Curve saved to the driver.");
        let intel = GpuCapabilities {
            voltage_style: VoltageControlStyle::Percent,
            fan_curve_is_hardware: true,
            fan_curve_points: 10,
            ..Default::default()
        };
        assert_eq!(
            mode_note(2, &intel, false),
            "The driver runs this curve after Apply, even when the window closes. Use 2–10 points within 25–100°C, with fan speeds that stay level or increase."
        );
        assert!(mode_note(0, &GpuCapabilities::default(), true).contains("Zero RPM"));
        assert_eq!(hardware_point_limit(&intel), Some(10));
        assert_eq!(hardware_point_limit(&GpuCapabilities::default()), None);
    }
}
