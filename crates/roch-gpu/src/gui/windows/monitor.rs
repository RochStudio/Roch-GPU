//! The hardware monitor (C# `MonitorWindow` + `ViewModels/TelemetryTable`): three summary cards
//! and the sensor table with current / minimum / maximum / average columns.
//!
//! It owns its telemetry subscription (a [`Tap`]) rather than being fed by the main window, so
//! opening and closing it costs nothing but the subscription. The extra clock counters
//! (`measure_extra_clocks`) are read on the polling thread right after each sample, never on the
//! UI thread. While it is open the app keeps polling in the foreground (`update_poll_detail`).
//!
//! * opens to the right of the main window (`Placement::RightOfOwner`);
//! * clicking Telemetry again focuses it, or closes it when it already has focus.

use std::collections::BTreeMap;
use std::time::Instant;

use chrono::{DateTime, Utc};
use eframe::egui::{
    self, Align, Align2, Color32, Context, CornerRadius, CursorIcon, Layout, Rect, RichText, Sense, Stroke, StrokeKind,
    Ui, ViewportClass, ViewportId, pos2, vec2,
};
use roch_core::GpuTelemetry;
use roch_core::util::dotnet_custom;

use super::{ChildChrome, Placement, Tap, WinCtx, content_frame};
use crate::gui::telemetry_table::{TelemetryRow, TelemetryTable};
use crate::gui::theme::{self, BODY, Palette, pal};
use crate::gui::widgets::{self, ButtonKind};

/// A sample and the extra clock counters read straight after it.
type Sample = (GpuTelemetry, BTreeMap<String, f64>);

/// Sensor-name column; the four figures share the rest.
const NAME_COL: f32 = 230.0;
/// The scrollbar's always-reserved gutter, matched by the fixed heading.
const GUTTER: f32 = 17.0;

pub struct MonitorWindow {
    chrome: ChildChrome,
    tap: Option<Tap<Sample>>,
    /// Built from the first sample: that is when the card has said which sensors it reports.
    table: Option<TelemetryTable>,
    /// When the running figures were last started from nothing.
    stats_since: Instant,
    last_stamp: Option<DateTime<Utc>>,
    summary: [String; 3],
    elapsed: String,
}

impl Default for MonitorWindow {
    fn default() -> Self {
        Self {
            chrome: ChildChrome::default(),
            tap: None,
            table: None,
            stats_since: Instant::now(),
            last_stamp: None,
            summary: ["—".into(), "—".into(), "—".into()],
            elapsed: "Waiting for sensors…".into(),
        }
    }
}

/// `value.ToString(format) + " " + unit`, or an em dash for a reading the card does not give.
/// The C# formats are the custom "0.0" and "0", which round exact ties away from zero.
fn summary(value: f64, unit: &str, decimals: usize) -> String {
    if value.is_finite() { format!("{} {unit}", dotnet_custom(value, decimals, decimals)) } else { "—".into() }
}

/// The three summary cards for one sample, for the comparison with the C# build
/// (`crate::audit_dump`).
#[cfg(test)]
pub(crate) fn audit_summaries(t: &GpuTelemetry) -> [String; 3] {
    [summary(t.temperature_c, "°C", 1), summary(t.core_clock_mhz, "MHz", 0), summary(t.power_watts, "W", 1)]
}

/// `TimeSpan.ToString(@"hh\:mm\:ss")`.
fn hms(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}

impl MonitorWindow {
    pub fn id() -> ViewportId {
        ViewportId::from_hash_of("roch_monitor")
    }

    pub fn focus(&mut self) {
        self.chrome.request_focus();
    }

    /// Subscribe on first show, and draw the last sample at once instead of waiting a poll.
    fn ensure_tap(&mut self, ctx: &Context, w: &WinCtx) {
        if self.tap.is_some() {
            return;
        }
        let svc = &w.shared.service;
        let tap = Tap::listen(svc, ctx, |svc, t| (t.clone(), svc.measure_extra_clocks()));
        if let Some(latest) = svc.latest() {
            tap.spawn(move |svc| (latest, svc.measure_extra_clocks()));
        }
        self.tap = Some(tap);
    }

    fn render(&mut self, w: &WinCtx, t: &GpuTelemetry, extra: &BTreeMap<String, f64>) {
        // The one-off read of the last sample can land after a fresher one.
        if self.last_stamp.is_some_and(|s| t.timestamp < s) {
            return;
        }
        self.last_stamp = Some(t.timestamp);
        if self.table.is_none() {
            crate::trace!("telemetry: first sample arrived");
        }
        let table =
            self.table.get_or_insert_with(|| TelemetryTable::new(t, &w.vm.caps, extra.keys().map(String::as_str)));
        table.add(t, Some(extra));
        self.summary =
            [summary(t.temperature_c, "°C", 1), summary(t.core_clock_mhz, "MHz", 0), summary(t.power_watts, "W", 1)];
        self.elapsed = format!("{} sensors  ·  {}", table.sensor_count(), hms(self.stats_since.elapsed().as_secs()));
    }

    pub fn show(&mut self, ctx: &Context, w: &mut WinCtx) -> bool {
        self.ensure_tap(ctx, w);
        let samples = self.tap.as_ref().map(Tap::drain).unwrap_or_default();
        let rows_before = self.table.as_ref().map_or(0, |t| t.rows().len());
        for (t, extra) in &samples {
            self.render(w, t, extra);
        }
        if self.table.as_ref().map_or(0, |t| t.rows().len()) != rows_before {
            // New rows: the scroll area sizes its bar from the previous frame's content, so let
            // it settle now rather than at the next sample (samples wake one frame each).
            ctx.request_repaint();
        }
        let builder = self
            .chrome
            .builder("Roch GPU — Telemetry", vec2(800.0, 900.0), w.icon, Placement::RightOfOwner, w.main_rect)
            .with_min_inner_size(vec2(740.0, 480.0));
        ctx.show_viewport_immediate(Self::id(), builder, |ui, _class: ViewportClass| {
            let closed = self.chrome.frame(ui.ctx(), w.dark);
            content_frame(ui.ctx(), 0).inner_margin(egui::Margin { left: 20, right: 20, top: 16, bottom: 14 }).show(
                ui,
                |ui| {
                    ui.set_min_size(ui.available_size());
                    self.body(ui, w);
                },
            );
            !closed
        })
    }

    fn body(&mut self, ui: &mut Ui, w: &mut WinCtx) {
        let p = pal(ui.ctx());
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

        // Title, device and the badge.
        let top = ui.max_rect().top();
        ui.label(RichText::new("Telemetry").font(theme::bold(BODY)).color(p.text));
        ui.add_space(4.0);
        ui.label(RichText::new(w.vm.telemetry_device_name()).font(theme::regular(BODY)).color(p.text));
        let badge = ui.painter().layout_no_wrap("LIVE · READ ONLY".into(), theme::bold(BODY), p.text);
        let r = Rect::from_min_size(
            pos2(ui.max_rect().right() - badge.size().x - 20.0, top),
            vec2(badge.size().x + 20.0, badge.size().y + 12.0),
        );
        ui.painter().rect_filled(r, CornerRadius::same(5), p.panel);
        ui.painter().galley(r.min + vec2(10.0, 6.0), badge, p.text);
        ui.add_space(16.0);

        // Summary cards.
        let gap = 12.0;
        let card_w = ((ui.available_width() - 2.0 * gap) / 3.0).floor();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (label, value) in ["GPU TEMPERATURE", "CORE CLOCK", "BOARD POWER"].iter().zip(&self.summary) {
                egui::Frame::new()
                    .fill(p.panel)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.set_width(card_w - 34.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(*label).font(theme::bold(BODY)).color(p.text));
                            ui.add_space(5.0);
                            ui.label(RichText::new(value).font(theme::regular(BODY)).color(p.text));
                        });
                    });
            }
        });
        ui.add_space(18.0);

        // Fixed column heading; its right gutter matches the scrollbar in the body.
        let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), 15.0 + 10.0), Sense::hover());
        let cols = columns(Rect::from_min_max(head.min, pos2(head.right() - GUTTER, head.top() + 15.0)));
        let painter = ui.painter();
        let bold = theme::bold(BODY);
        painter.text(
            pos2(cols[0].left() + 8.0, cols[0].center().y),
            Align2::LEFT_CENTER,
            "Sensor",
            bold.clone(),
            p.text,
        );
        for (c, t) in cols[1..].iter().zip(["Current", "Minimum", "Maximum", "Average"]) {
            painter.text(pos2(c.right() - 8.0, c.center().y), Align2::RIGHT_CENTER, t, bold.clone(), p.text);
        }
        painter.hline(head.x_range(), head.bottom() - 0.5, Stroke::new(1.0, p.border));

        // The footer is laid out first from the bottom so the table can take what is left.
        let footer_h = 13.0 + 28.0;
        let body_h = (ui.available_height() - footer_h).max(40.0);
        let rows: &[TelemetryRow] = self.table.as_ref().map(TelemetryTable::rows).unwrap_or(&[]);
        ui.allocate_ui(vec2(ui.available_width(), body_h), |ui| {
            ui.set_min_height(body_h);
            ui.scope(|ui| {
                style_scrollbar(ui, &p);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .show(ui, |ui| {
                        for row in rows {
                            draw_row(ui, row, &p);
                        }
                    });
            });
        });

        // Footer: elapsed, the auto-open switch, Reset statistics.
        let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().hline(line.x_range(), line.center().y, Stroke::new(1.0, p.border));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.set_height(28.0);
            ui.label(RichText::new(&self.elapsed).font(theme::regular(BODY)).color(p.text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::text_button(ui, "Reset statistics", ButtonKind::Normal, vec2(12.0, 6.0), true)
                    .on_hover_text("Clear minimum, maximum and average values. GPU settings are not changed.")
                    .clicked()
                {
                    if let Some(t) = &mut self.table {
                        t.reset_stats();
                    }
                    self.stats_since = Instant::now();
                }
                ui.add_space(16.0);
                let mut on = w.shared.settings().auto_open_monitor;
                if switch(ui, &mut on, "Open at launch")
                    .on_hover_text("Open telemetry automatically when Roch GPU starts.")
                    .clicked()
                {
                    w.shared.settings().auto_open_monitor = on;
                    w.shared.save_settings();
                }
            });
        });
    }
}

/// The five column rectangles of a row: name, then four equal figures.
fn columns(r: Rect) -> [Rect; 5] {
    let rest = ((r.width() - NAME_COL).max(0.0)) / 4.0;
    let mut out = [r; 5];
    out[0] = Rect::from_min_max(r.min, pos2(r.left() + NAME_COL, r.bottom()));
    for (i, c) in out.iter_mut().enumerate().skip(1) {
        let x0 = r.left() + NAME_COL + rest * (i - 1) as f32;
        *c = Rect::from_min_max(pos2(x0, r.top()), pos2(x0 + rest, r.bottom()));
    }
    out
}

fn draw_row(ui: &mut Ui, row: &TelemetryRow, p: &Palette) {
    let width = ui.available_width();
    if row.is_header {
        // A group title: brand bar on the left, bold, spanning every column.
        ui.add_space(14.0);
        let (r, _) = ui.allocate_exact_size(vec2(width, 15.0 + 4.0), Sense::hover());
        ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(2.0, r.height())), CornerRadius::ZERO, p.brand);
        ui.painter().text(
            pos2(r.left() + 2.0 + 8.0, r.center().y),
            Align2::LEFT_CENTER,
            &row.name,
            theme::bold(BODY),
            p.text,
        );
        ui.add_space(5.0);
        return;
    }
    let (r, resp) = ui.allocate_exact_size(vec2(width, 15.0 + 12.0), Sense::hover());
    let fill = if resp.hovered() {
        p.panel
    } else if row.is_banded {
        p.highlight
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(r, CornerRadius::same(3), fill);
    let cols = columns(r);
    let painter = ui.painter();
    // The name is clipped with an ellipsis and spelled out in full on hover.
    let mut job = egui::text::LayoutJob::simple_singleline(row.name.clone(), theme::regular(BODY), p.text);
    job.wrap.max_width = cols[0].width() - 16.0;
    job.wrap.max_rows = 1;
    job.wrap.overflow_character = Some('…');
    let galley = painter.layout_job(job);
    painter.galley(pos2(cols[0].left() + 8.0, r.center().y - galley.size().y / 2.0), galley, p.text);
    let figures = [(&row.current, true), (&row.minimum, false), (&row.maximum, false), (&row.average, false)];
    for (c, (text, current)) in cols[1..].iter().zip(figures) {
        let (font, color) = if current { (theme::bold(BODY), p.accent) } else { (theme::regular(BODY), p.text) };
        painter.text(pos2(c.right() - 8.0, c.center().y), Align2::RIGHT_CENTER, text, font, color);
    }
    if resp.hover_pos().is_some_and(|at| cols[0].contains(at)) {
        resp.on_hover_text(&row.name);
    }
}

/// The monitor's scrollbar: a 9 px grip in a 17 px gutter, muted at 30% (65% hovered, 80%
/// dragged), no track.
fn style_scrollbar(ui: &mut Ui, p: &Palette) {
    let s = &mut ui.spacing_mut().scroll;
    *s = egui::style::ScrollStyle::solid();
    s.bar_width = 9.0;
    s.bar_inner_margin = 5.0;
    s.bar_outer_margin = 3.0;
    s.foreground_color = true;
    // A solid bar paints its grip at full opacity, so the WPF opacities go into the colours.
    let v = &mut ui.visuals_mut().widgets;
    v.inactive.fg_stroke.color = p.muted.gamma_multiply(0.3);
    v.hovered.fg_stroke.color = p.muted.gamma_multiply(0.65);
    v.active.fg_stroke.color = p.muted.gamma_multiply(0.8);
    for w in [&mut v.inactive, &mut v.hovered, &mut v.active] {
        w.corner_radius = CornerRadius::same(4);
    }
}

/// Roch Viewer's switch (32×16 track, 12 px knob). "On" is the neutral title-hover tone, not the
/// brand red: a setting that costs an extra poll is not something to advertise.
fn switch(ui: &mut Ui, on: &mut bool, text: &str) -> egui::Response {
    let p = pal(ui.ctx());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), theme::regular(BODY), p.text);
    let size = vec2(32.0 + 7.0 + galley.size().x, 18.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let track = Rect::from_min_size(pos2(rect.left(), rect.center().y - 8.0), vec2(32.0, 16.0));
    let painter = ui.painter();
    painter.rect(
        track,
        CornerRadius::same(8),
        if *on { p.title_hover } else { p.border },
        Stroke::new(1.0, p.muted),
        StrokeKind::Inside,
    );
    let knob_x = if *on { track.right() - 1.0 - 1.0 - 6.0 } else { track.left() + 1.0 + 1.0 + 6.0 };
    painter.circle_filled(pos2(knob_x, track.center().y), 6.0, if *on { p.text } else { p.muted });
    painter.galley(pos2(track.right() + 7.0, rect.center().y - galley.size().y / 2.0), galley, p.text);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_and_elapsed_formats() {
        assert_eq!(summary(28.04, "°C", 1), "28.0 °C");
        assert_eq!(summary(f64::NAN, "W", 1), "—");
        assert_eq!(summary(765.0, "MHz", 0), "765 MHz");
        // .NET's "0.0" / "0" round an exact tie away from zero; Rust's {:.1} / {:.0} would not.
        assert_eq!(summary(45.25, "°C", 1), "45.3 °C");
        assert_eq!(summary(2422.5, "MHz", 0), "2423 MHz");
        assert_eq!(summary(110.25, "W", 1), "110.3 W");
        assert_eq!(hms(6), "00:00:06");
        assert_eq!(hms(3 * 3600 + 61), "03:01:01");
    }

    #[test]
    fn columns_split_the_rest_evenly() {
        let c = columns(Rect::from_min_size(pos2(0.0, 0.0), vec2(630.0, 20.0)));
        assert_eq!(c[0].width(), NAME_COL);
        assert!(c[1..].iter().all(|r| (r.width() - 100.0).abs() < 1e-3));
        assert_eq!(c[4].right(), 630.0);
    }
}
