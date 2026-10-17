//! The custom-drawn controls of `App.xaml` / `MainWindow.xaml`: flat buttons with a brand-coloured
//! hover border, the Afterburner-style slider with arrow nudges, the value box
//! (`ValueBoxBehavior`: click selects all, Enter commits, a refused value comes back clamped), the
//! two-thumb `RangeSlider`, and the "Label: value" information line.

use std::time::Duration;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, CursorIcon, Id, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2,
    vec2,
};

use super::theme::{self, BODY, pal};
use super::view_model::{Field, MainViewModel};

/// The look of a [`button`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// Default button: panel fill, border, brand border on hover.
    Normal,
    /// Apply: filled brand red, white bold text.
    Primary,
    /// Reset to Defaults: danger-coloured border.
    Danger,
    /// The small header buttons (Graphics, Telemetry, Log): square corners.
    Header,
    /// A numbered profile slot.
    Slot { occupied: bool, active: bool },
    /// The Save toggle: red while armed.
    Toggle { checked: bool },
}

/// A flat button of the given size. `bold` picks the bold face.
pub fn button(ui: &mut Ui, text: &str, kind: ButtonKind, size: Vec2, enabled: bool, bold: bool) -> Response {
    let p = pal(ui.ctx());
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let hovered = enabled && resp.hovered();
    let pressed = enabled && resp.is_pointer_button_down_on();
    let (mut fill, mut stroke, mut fg, radius) = match kind {
        ButtonKind::Normal => (p.panel_alt, p.border, p.text, 4),
        ButtonKind::Primary => (p.selected, p.selected, Color32::WHITE, 4),
        ButtonKind::Danger => (p.panel_alt, p.danger, p.text, 4),
        ButtonKind::Header => (p.panel_alt, p.section_border, p.text, 0),
        ButtonKind::Slot { occupied, active } => {
            if active {
                (p.selected, p.selected, Color32::WHITE, 4)
            } else if occupied {
                (p.panel_alt, p.border, p.text, 4)
            } else {
                (p.panel_alt, p.border, p.muted, 4)
            }
        }
        ButtonKind::Toggle { checked } => {
            if checked {
                (p.selected, p.selected, Color32::WHITE, 4)
            } else {
                (p.panel_alt, p.border, p.text, 4)
            }
        }
    };
    if hovered && !matches!(kind, ButtonKind::Primary) {
        stroke = p.brand_hover;
    }
    if hovered && matches!(kind, ButtonKind::Primary) {
        fill = p.brand_hover;
        stroke = p.brand_hover;
    }
    if pressed && matches!(kind, ButtonKind::Header) {
        fill = p.header;
    }
    if !enabled {
        fg = p.muted;
        if !matches!(kind, ButtonKind::Primary) {
            fill = p.bg;
            stroke = p.border;
        } else {
            fill = fill.gamma_multiply(0.55);
        }
    }
    let painter = ui.painter();
    painter.rect(rect, CornerRadius::same(radius), fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
    let font = if bold { theme::bold(BODY) } else { theme::regular(BODY) };
    painter.text(rect.center(), Align2::CENTER_CENTER, text, font, fg);
    if enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp }
}

/// A caption button in a custom title bar (44×30, symbol glyph).
pub fn title_button(ui: &mut Ui, glyph: &str, width: f32, close: bool, font_size: f32) -> Response {
    let p = pal(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    let hovered = resp.hovered();
    let (fill, fg) = match (hovered, close) {
        (true, true) => (p.close_hover, Color32::WHITE),
        (true, false) => (p.title_hover, p.text),
        _ => (Color32::TRANSPARENT, p.text),
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, theme::regular(font_size), fg);
    resp
}

/// The filled crescent of `ThemeGlyph` (light mode's "switch to dark" button) in an 18×18 box.
///
/// The WPF path `M14.125,1.324 A8,8 0 1 0 14.125,16.676 A8,8 0 0 1 14.125,1.324 Z` is two r=8
/// arcs through the tips (14.125, 1.324) and (14.125, 16.676). A circle of radius 8 through both
/// tips has its centre 2.253 either side of x=14.125: the large arc belongs to the circle centred
/// at x=11.872 (the moon's body) and the small inner arc to the one centred at x=16.378 (the cut).
/// That leaves a crescent about 4.5 px thick at its widest. Painted as the body, then the cut in
/// `behind`, clipped to the glyph box.
pub fn paint_moon(ui: &Ui, center: egui::Pos2, color: Color32, behind: Color32) {
    const TIP_X: f32 = 14.125;
    const HALF_CHORD: f32 = 7.676_073;
    const R: f32 = 8.0;
    let offset = (R * R - HALF_CHORD * HALF_CHORD).sqrt();
    let origin = center - vec2(9.0, 9.0);
    let painter = ui.painter_at(Rect::from_min_size(origin, vec2(18.0, 18.0)));
    painter.circle_filled(origin + vec2(TIP_X - offset, 9.0), R, color);
    painter.circle_filled(origin + vec2(TIP_X + offset, 9.0), R, behind);
}

/// Text in the muted style, optionally coloured.
pub fn label(ui: &mut Ui, text: impl Into<String>, color: Color32, bold: bool) -> Response {
    let font = if bold { theme::bold(BODY) } else { theme::regular(BODY) };
    ui.add(egui::Label::new(egui::RichText::new(text.into()).font(font).color(color)).selectable(false))
}

/// "Driver: 581.42" with the value in the accent colour (C# `InformationLinePartConverter`). An
/// empty line takes no space, as the WPF TextBlock collapsed to nothing.
pub fn info_line(ui: &mut Ui, text: &str) {
    if text.is_empty() {
        return;
    }
    let p = pal(ui.ctx());
    let (label_part, value_part) = super::format::info_line_parts(text);
    let mut job = egui::text::LayoutJob::default();
    let fmt = |c: Color32| egui::TextFormat { font_id: theme::regular(BODY), color: c, ..Default::default() };
    job.append(label_part, 0.0, fmt(p.text));
    job.append(value_part, 0.0, fmt(p.accent));
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    job.wrap.max_width = ui.available_width();
    ui.add(egui::Label::new(job).selectable(false));
}

/// Hold-to-repeat arrow button (WPF RepeatButton: 400 ms delay, then every 60 ms). Returns how
/// many steps to take this frame (0 or 1).
pub fn nudge_button(ui: &mut Ui, glyph: &str, enabled: bool) -> bool {
    let p = pal(ui.ctx());
    let sense = if enabled { Sense::click_and_drag() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 17.0), sense);
    let id = resp.id;
    let down = enabled && resp.is_pointer_button_down_on();
    let now = ui.input(|i| i.time);
    let mut fire = false;
    if down {
        let started: Option<(f64, f64)> = ui.data(|d| d.get_temp(id));
        match started {
            None => {
                fire = true;
                ui.data_mut(|d| d.insert_temp(id, (now, now + 0.4)));
            }
            Some((t0, next)) if now >= next => {
                fire = true;
                ui.data_mut(|d| d.insert_temp(id, (t0, now + 0.06)));
            }
            _ => {}
        }
        ui.ctx().request_repaint_after(Duration::from_millis(30));
    } else {
        ui.data_mut(|d| d.remove::<(f64, f64)>(id));
    }
    let (fill, fg, stroke) = if down {
        (p.selected, Color32::WHITE, p.border)
    } else if enabled && resp.hovered() {
        (p.panel_alt, p.text, p.brand_hover)
    } else {
        (p.panel_alt, if enabled { p.text } else { p.muted }, p.border)
    };
    ui.painter().rect(rect, CornerRadius::same(3), fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, theme::regular(9.0), fg);
    fire
}

/// Map a value to an x position on a track whose thumb is `thumb_w` wide.
fn x_for(rect: Rect, thumb_w: f32, min: i32, max: i32, v: i32) -> f32 {
    let range = (max - min) as f32;
    let t = if range <= 0.0 { 0.0 } else { ((v - min) as f32 / range).clamp(0.0, 1.0) };
    rect.left() + thumb_w / 2.0 + t * (rect.width() - thumb_w).max(1.0)
}

/// Map an x position back to a value on the step grid counted from `min` (WPF snap-to-tick).
fn value_at(rect: Rect, thumb_w: f32, min: i32, max: i32, step: i32, x: f32) -> i32 {
    let t = ((x - rect.left() - thumb_w / 2.0) / (rect.width() - thumb_w).max(1.0)).clamp(0.0, 1.0);
    let raw = min as f64 + t as f64 * (max - min) as f64;
    let step = step.max(1) as f64;
    let v = min as f64 + ((raw - min as f64) / step).round() * step;
    (v.round() as i64).clamp(min as i64, max.max(min) as i64) as i32
}

/// The main window's slider (`LayoutSlider`): a 4 px track, accent to the left of the 12×24
/// thumb, border colour to the right. Click or drag moves to the point.
pub fn layout_slider(ui: &mut Ui, vm: &mut MainViewModel, f: Field, width: f32, enabled: bool) {
    slider_impl(ui, vm, f, width, enabled, 24.0, 12.0, 4.0)
}

/// The compact XOC slider (`Tight`): 17 px high.
pub fn tight_slider(ui: &mut Ui, vm: &mut MainViewModel, f: Field, width: f32, enabled: bool) {
    slider_impl(ui, vm, f, width, enabled, 17.0, 10.0, 4.0)
}

#[allow(clippy::too_many_arguments)]
fn slider_impl(ui: &mut Ui, vm: &mut MainViewModel, f: Field, width: f32, enabled: bool, h: f32, tw: f32, th: f32) {
    let sense = if enabled { Sense::click_and_drag() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(width.max(20.0), h), sense);
    let (min, max) = vm.slider_range(f);
    if enabled
        && (resp.dragged() || resp.clicked() || resp.drag_started())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let v = value_at(rect, tw, min, max, vm.step(f), pos.x);
        vm.set(f, v);
    }
    let x = x_for(rect, tw, min, max, vm.get(f));
    paint_slider(ui, rect, x, enabled, h, tw, th);
}

/// A `Tight` slider over a plain value (the fan window's per-fan duties), snapping to `step`
/// counted from `min` (WPF snap-to-tick). Returns true when the value moved.
pub fn value_slider(ui: &mut Ui, value: &mut i32, range: (i32, i32), step: i32, width: f32, enabled: bool) -> bool {
    let (min, max) = range;
    let (h, tw, th) = (17.0, 10.0, 4.0);
    let sense = if enabled { Sense::click_and_drag() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(width.max(20.0), h), sense);
    let mut moved = false;
    if enabled
        && (resp.dragged() || resp.clicked() || resp.drag_started())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let v = value_at(rect, tw, min, max, step, pos.x);
        moved = v != *value;
        *value = v;
    }
    let x = x_for(rect, tw, min, max, *value);
    paint_slider(ui, rect, x, enabled, h, tw, th);
    moved
}

#[allow(clippy::too_many_arguments)]
fn paint_slider(ui: &Ui, rect: Rect, x: f32, enabled: bool, h: f32, tw: f32, th: f32) {
    let p = pal(ui.ctx());
    let mid = rect.center().y;
    let painter = ui.painter();
    let left_col = if enabled { p.accent } else { p.muted };
    painter.rect_filled(Rect::from_min_max(pos2(x, mid - th / 2.0), pos2(rect.right(), mid + th / 2.0)), 0.0, p.border);
    painter.rect_filled(Rect::from_min_max(pos2(rect.left(), mid - th / 2.0), pos2(x, mid + th / 2.0)), 0.0, left_col);
    let thumb = Rect::from_center_size(pos2(x, mid), vec2(tw, h));
    let tc = if enabled { p.text } else { p.muted };
    painter.rect(thumb, CornerRadius::same(1), tc, Stroke::new(1.0, tc), StrokeKind::Inside);
}

/// Nudge, slider, nudge — the middle column of a main-window row.
pub fn nudged_slider(ui: &mut Ui, vm: &mut MainViewModel, f: Field, width: f32, enabled: bool, tight: bool) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if nudge_button(ui, "\u{25C0}", enabled) {
            vm.nudge(f, -1);
        }
        let w = (width - 2.0 * 18.0 - 8.0).max(20.0);
        if tight {
            tight_slider(ui, vm, f, w, enabled);
        } else {
            layout_slider(ui, vm, f, w, enabled);
        }
        if nudge_button(ui, "\u{25B6}", enabled) {
            vm.nudge(f, 1);
        }
    });
}

/// Two thumbs on one track (`RangeSlider`): a rail's floor and ceiling, or a clock window. The
/// thumbs cannot cross; a click moves whichever is nearer. Dimmed to 35% when disabled.
pub fn range_slider(ui: &mut Ui, vm: &mut MainViewModel, lo: Field, hi: Field, width: f32, enabled: bool) {
    const THUMB_W: f32 = 9.0;
    const THUMB_H: f32 = 15.0;
    const TRACK_H: f32 = 3.0;
    let sense = if enabled { Sense::click_and_drag() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 17.0), sense);
    let (min, max) = vm.slider_range(hi);
    let step = vm.step(hi);
    let drag_id = resp.id.with("upper");
    if enabled
        && let Some(pos) = resp.interact_pointer_pos()
        && (resp.drag_started() || resp.clicked() || resp.is_pointer_button_down_on())
    {
        {
            let grabbed: Option<bool> = ui.data(|d| d.get_temp(drag_id));
            let upper = grabbed.unwrap_or_else(|| {
                let xl = x_for(rect, THUMB_W, min, max, vm.get(lo));
                let xh = x_for(rect, THUMB_W, min, max, vm.get(hi));
                (pos.x - xh).abs() <= (pos.x - xl).abs()
            });
            ui.data_mut(|d| d.insert_temp(drag_id, upper));
            let v = value_at(rect, THUMB_W, min, max, step, pos.x);
            if upper {
                vm.set(hi, v.max(vm.get(lo)));
            } else {
                vm.set(lo, v.min(vm.get(hi)));
            }
        }
    }
    if !resp.is_pointer_button_down_on() {
        ui.data_mut(|d| d.remove::<bool>(drag_id));
    }
    let alpha = if enabled { 1.0 } else { 0.35 };
    let track = Color32::from_rgb(0x3A, 0x3A, 0x42).gamma_multiply(alpha);
    let span = Color32::from_rgb(0xD3, 0x2F, 0x2F).gamma_multiply(alpha);
    let thumb = Color32::from_rgb(0xF3, 0xF3, 0xF5).gamma_multiply(alpha);
    let mid = rect.center().y;
    let xl = x_for(rect, THUMB_W, min, max, vm.get(lo));
    let xh = x_for(rect, THUMB_W, min, max, vm.get(hi));
    let painter = ui.painter();
    painter.rect_filled(
        Rect::from_min_max(pos2(rect.left(), mid - TRACK_H / 2.0), pos2(rect.right(), mid + TRACK_H / 2.0)),
        CornerRadius::same(1),
        track,
    );
    if xh > xl {
        painter.rect_filled(
            Rect::from_min_max(pos2(xl, mid - TRACK_H / 2.0), pos2(xh, mid + TRACK_H / 2.0)),
            CornerRadius::same(1),
            span,
        );
    }
    for x in [xl, xh] {
        painter.rect_filled(Rect::from_center_size(pos2(x, mid), vec2(THUMB_W, THUMB_H)), CornerRadius::same(2), thumb);
    }
}

/// A typed value box bound to one field (`ValueBox` + `ValueBoxBehavior`). Shows the view
/// model's text while unfocused; on focus the whole number is selected; Enter or leaving the box
/// commits, and the box then shows what the view model accepted.
pub fn value_box(ui: &mut Ui, vm: &mut MainViewModel, f: Field, size: Vec2, enabled: bool) -> Response {
    let p = pal(ui.ctx());
    let id = ui.scope_id().with(("value_box", f as u8));
    let buffer_id = id.with("buffer");
    let focused = ui.memory(|m| m.has_focus(id));
    let mut text: String =
        if focused { ui.data(|d| d.get_temp(buffer_id)).unwrap_or_else(|| vm.input_text(f)) } else { vm.input_text(f) };
    let color = if enabled { p.accent } else { p.muted };
    let edit = egui::TextEdit::singleline(&mut text)
        .id(id)
        .font(theme::bold(BODY))
        .text_color(color)
        .align(egui::Align2::RIGHT_CENTER)
        .margin(egui::Margin::symmetric(4, 1))
        .background_color(p.panel_alt)
        .desired_width(size.x - 8.0);
    let resp = ui.add_enabled_ui(enabled, |ui| ui.add_sized(size, edit)).inner;
    // A single-line TextEdit gives up focus on Enter; the WPF box kept it and re-selected the
    // accepted value, so take focus straight back.
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter)) && (resp.has_focus() || resp.lost_focus());
    if resp.gained_focus() || resp.clicked() {
        text = vm.input_text(f);
        select_all(ui, id, &text);
    }
    if enter {
        vm.commit_input(f, &text);
        text = vm.input_text(f);
        ui.memory_mut(|m| m.request_focus(id));
        select_all(ui, id, &text);
        ui.data_mut(|d| d.insert_temp(buffer_id, text));
        return resp;
    }
    if resp.lost_focus() {
        vm.commit_input(f, &text);
        ui.data_mut(|d| d.remove::<String>(buffer_id));
        return resp;
    }
    if resp.has_focus() {
        ui.data_mut(|d| d.insert_temp(buffer_id, text));
    }
    resp
}

fn select_all(ui: &Ui, id: Id, text: &str) {
    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
        let range =
            egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(text.chars().count()));
        state.cursor.set_char_range(Some(range));
        state.store(ui.ctx(), id);
    }
}

/// A themed check box with the label to its right.
pub fn checkbox(ui: &mut Ui, checked: &mut bool, text: &str, enabled: bool) -> Response {
    let p = pal(ui.ctx());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), theme::regular(BODY), p.text);
    let size = vec2(14.0 + 6.0 + galley.size().x, 18.0f32.max(galley.size().y));
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, mut resp) = ui.allocate_exact_size(size, sense);
    if resp.clicked() {
        *checked = !*checked;
        resp.mark_changed();
    }
    let boxr = Rect::from_min_size(pos2(rect.left(), rect.center().y - 7.0), vec2(14.0, 14.0));
    let (fill, stroke) = if *checked {
        (p.selected, p.selected)
    } else if enabled && resp.hovered() {
        (p.bg, p.brand_hover)
    } else {
        (p.bg, if enabled { p.border } else { p.muted })
    };
    let painter = ui.painter();
    painter.rect(boxr, CornerRadius::same(2), fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
    if *checked {
        let c = boxr.center();
        painter.line_segment([c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.0)], Stroke::new(2.0, Color32::WHITE));
        painter.line_segment([c + vec2(-1.0, 3.0), c + vec2(4.0, -3.5)], Stroke::new(2.0, Color32::WHITE));
    }
    let fg = if enabled { p.text } else { p.muted };
    painter.galley_with_override_text_color(
        pos2(boxr.right() + 6.0, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
    resp
}

/// A bordered section (`LayoutPanel`): 1 px section border, 2 px corners.
pub fn layout_panel<R>(ui: &mut Ui, margin: egui::Margin, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = pal(ui.ctx());
    egui::Frame::new()
        .stroke(Stroke::new(1.0, p.section_border))
        .corner_radius(CornerRadius::same(2))
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A themed free-text box (WPF `TextBox`; `value` = the `ValueBox` look: bold, accent colour,
/// right-aligned, everything selected on focus). Enter keeps the focus, as it did in WPF; check
/// [`enter_pressed`] to commit on it.
pub fn text_box(ui: &mut Ui, text: &mut String, id: Id, size: Vec2, value: bool, enabled: bool) -> Response {
    let p = pal(ui.ctx());
    let color = if !enabled {
        p.muted
    } else if value {
        p.accent
    } else {
        p.text
    };
    let edit = egui::TextEdit::singleline(text)
        .id(id)
        .font(if value { theme::bold(BODY) } else { theme::regular(BODY) })
        .text_color(color)
        .align(if value { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER })
        .margin(egui::Margin::symmetric(4, 1))
        .background_color(p.panel_alt)
        .return_key(None::<egui::KeyboardShortcut>)
        .desired_width(size.x - 8.0);
    let resp = ui.add_enabled_ui(enabled, |ui| ui.add_sized(size, edit)).inner;
    if value && resp.gained_focus() {
        select_all(ui, id, text);
    }
    resp
}

/// Enter was pressed in this (focused) box this frame.
pub fn enter_pressed(ui: &Ui, resp: &Response) -> bool {
    resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// A WPF radio button: a 14 px circle, filled with a centre dot when checked. Returns the click.
pub fn radio(ui: &mut Ui, checked: bool, text: &str, enabled: bool) -> Response {
    let p = pal(ui.ctx());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), theme::regular(BODY), p.text);
    let size = vec2(14.0 + 6.0 + galley.size().x, 18.0f32.max(galley.size().y));
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let c = pos2(rect.left() + 7.0, rect.center().y);
    let ring = if !enabled {
        p.muted
    } else if resp.hovered() {
        p.brand_hover
    } else {
        p.text
    };
    let painter = ui.painter();
    painter.circle(c, 6.5, p.bg, Stroke::new(1.0, ring));
    if checked {
        painter.circle_filled(c, 3.5, if enabled { p.text } else { p.muted });
    }
    let fg = if enabled { p.text } else { p.muted };
    painter.galley_with_override_text_color(
        pos2(rect.left() + 20.0, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
    if enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp }
}

/// A button sized to its text with WPF's default `Padding` (12,6 unless given).
pub fn text_button(ui: &mut Ui, text: &str, kind: ButtonKind, pad: Vec2, enabled: bool) -> Response {
    let w = ui.painter().layout_no_wrap(text.to_owned(), theme::regular(BODY), Color32::WHITE).size().x;
    button(ui, text, kind, vec2((w + 2.0 * pad.x + 2.0).round(), (15.0 + 2.0 * pad.y + 2.0).round()), enabled, false)
}
