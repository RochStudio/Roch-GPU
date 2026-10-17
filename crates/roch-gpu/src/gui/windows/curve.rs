//! The graphical V/F curve editor (C# `CurveWindow` + `Controls/VfCurveEditor`): reads the curve
//! from the backend, lets the user drag points, and writes explicit per-point targets back.
//! Everything is volatile — "Reset to stock" or a reboot undoes it.
//!
//! Driver traffic stays off the UI thread: reads (the curve, the live cap and ceiling after each
//! sample) go through a [`Tap`]; writes (`set_vf_curve_targets`, `set_voltage_lock`) are worker
//! jobs (`vm.queue_custom`), whose UI half updates the shared view model — a flatten here is the
//! same setting as the main window's voltage cap (`Field::TargetVoltage`), so it is told, or its
//! next Apply would write the cap it still believed in and undo the flatten. On Intel
//! (`can_edit_vf_curve`) the curve carries the core offset, so `Field::CoreOffset` follows it.

use std::f32::consts::FRAC_PI_2;

use eframe::egui::{
    self, Align, Align2, Color32, Context, CornerRadius, CursorIcon, Event, EventFilter, Id, Key, Layout, Rect, Sense,
    Shape, Stroke, Ui, ViewportClass, ViewportId, epaint::TextShape, pos2, vec2,
};
use roch_core::models::voltage_plan;
use roch_core::services::TuningService;
use roch_core::{GpuCapabilities, VfCurveSample, VoltageControlStyle};

use super::{ChildChrome, Placement, Tap, WinCtx, content_frame};
use crate::gui::theme::{self, BODY, pal};
use crate::gui::vf_editor::{self, VfEditor, VfGeom, VfKey};
use crate::gui::view_model::{Field, MainViewModel};
use crate::gui::widgets::{self, ButtonKind};

const DOUBLE_CLICK_S: f64 = 0.3;
const HINT: &str = "Drag a point to change its clock. Ctrl+drag flattens everything to the right (undervolt). Shift+drag moves the whole tail. Double-click a point to reset it, right-click anywhere to reset all.";

/// What arrives from the polling thread, a one-off read, or a finished write.
enum CurveMsg {
    /// After each foreground sample: the live operating point, the active cap and the ceiling.
    Live {
        volt_mv: f64,
        clock_mhz: f64,
        cap_mv: i32,
        ceiling_mv: i32,
    },
    /// A curve read (C# `LoadCurve`); `note` replaces the status line it would otherwise set.
    Loaded {
        read: Box<CurveRead>,
        note: Option<(String, bool)>,
    },
    Cap(i32),
    /// Apply's read-back: what the driver really took (it snaps to its own grid).
    Points(Vec<VfCurveSample>),
    Say(String, bool),
}

struct CurveRead {
    points: Result<Vec<VfCurveSample>, String>,
    cap_mv: i32,
    ceiling_mv: i32,
}

/// The active voltage lock, 0 when there is none or the backend cannot say (-1).
fn read_cap_mv(svc: &TuningService) -> i32 {
    svc.read_voltage_lock_mv().max(0)
}

/// The highest voltage the card may reach with the applied boost — not the top of the V/F table,
/// which is fixed in hardware (a boost adds no points to it). Measured ceilings, so the dashed
/// extrapolation past the last point stops where the card does. 0 on Intel and on a failed read.
fn read_ceiling_mv(svc: &TuningService, caps: &GpuCapabilities) -> i32 {
    if caps.voltage_style == VoltageControlStyle::Percent {
        return 0;
    }
    match svc.read_tuning_state() {
        Ok(st) => voltage_plan::ceiling_mv(
            st.voltage_boost_percent,
            st.voltage_offset_mv,
            svc.stock_ceiling_mv(),
            svc.boost_ceiling_mv(),
            caps.stock_max_voltage_mv,
        ),
        Err(_) => 0,
    }
}

fn read_curve(svc: &TuningService) -> CurveRead {
    let caps = svc.capabilities();
    CurveRead {
        points: svc.read_vf_curve().map_err(|e| e.message),
        cap_mv: read_cap_mv(svc),
        ceiling_mv: read_ceiling_mv(svc, &caps),
    }
}

#[derive(Default)]
pub struct CurveWindow {
    chrome: ChildChrome,
    tap: Option<Tap<CurveMsg>>,
    editor: VfEditor,
    flatten: String,
    status: String,
    status_error: bool,
    last_press: Option<(f64, egui::Pos2)>,
}

impl CurveWindow {
    pub fn id() -> ViewportId {
        ViewportId::from_hash_of("roch_curve")
    }

    pub fn focus(&mut self) {
        self.chrome.request_focus();
    }

    fn say(&mut self, msg: impl Into<String>, error: bool) {
        self.status = msg.into();
        self.status_error = error;
    }

    /// First show: configure the editor from the card's capabilities, subscribe, read the curve.
    fn ensure_started(&mut self, ctx: &Context, w: &WinCtx) {
        if self.tap.is_some() {
            return;
        }
        let caps = &w.vm.caps;
        self.editor.step_mhz = caps.vf_curve_step_mhz.max(1);
        self.editor.freq_min_mhz = caps.vf_curve_min_mhz;
        self.editor.freq_max_mhz = caps.vf_curve_max_mhz;
        if caps.can_edit_vf_curve {
            self.editor.full_frequency_range = true;
            self.editor.volt_min_mv = caps.min_voltage_mv;
            self.editor.volt_max_mv = caps.max_voltage_mv;
        }
        self.flatten = "1000".into();
        self.say(HINT, false);
        let tap = Tap::listen(&w.shared.service, ctx, |svc, t| {
            let caps = svc.capabilities();
            CurveMsg::Live {
                volt_mv: t.voltage_mv,
                clock_mhz: t.core_clock_mhz,
                cap_mv: read_cap_mv(svc),
                ceiling_mv: read_ceiling_mv(svc, &caps),
            }
        });
        tap.spawn(|svc| CurveMsg::Loaded { read: Box::new(read_curve(svc)), note: None });
        self.tap = Some(tap);
    }

    fn receive(&mut self) {
        let msgs = self.tap.as_ref().map(Tap::drain).unwrap_or_default();
        for m in msgs {
            match m {
                CurveMsg::Live { volt_mv, clock_mhz, cap_mv, ceiling_mv } => {
                    self.editor.set_live(volt_mv, clock_mhz);
                    self.editor.set_voltage_cap(cap_mv);
                    self.editor.set_voltage_ceiling(ceiling_mv);
                }
                CurveMsg::Loaded { read, note } => {
                    self.loaded(*read);
                    if let Some((text, err)) = note {
                        self.say(text, err);
                    }
                }
                CurveMsg::Cap(mv) => self.editor.set_voltage_cap(mv),
                CurveMsg::Points(pts) => self.editor.set_points(&pts),
                CurveMsg::Say(text, err) => self.say(text, err),
            }
        }
    }

    fn loaded(&mut self, read: CurveRead) {
        match read.points {
            Ok(pts) => {
                self.editor.set_points(&pts);
                self.editor.set_voltage_cap(read.cap_mv);
                self.editor.set_voltage_ceiling(read.ceiling_mv);
                match self.editor.loaded_status() {
                    Some(s) => self.say(s, false),
                    None => self.say("This card/driver did not return a V/F curve — nothing to edit.", true),
                }
            }
            Err(e) => self.say(format!("Could not read the curve: {e}"), true),
        }
    }

    pub fn show(&mut self, ctx: &Context, w: &mut WinCtx) -> bool {
        self.ensure_started(ctx, w);
        self.receive();
        let builder = self
            .chrome
            .builder("Voltage / Frequency curve", vec2(960.0, 620.0), w.icon, Placement::CenterOwner, w.main_rect)
            .with_min_inner_size(vec2(700.0, 460.0));
        ctx.show_viewport_immediate(Self::id(), builder, |ui, _class: ViewportClass| {
            let closed = self.chrome.frame(ui.ctx(), w.dark);
            content_frame(ui.ctx(), 12).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                self.body(ui, w);
            });
            !closed
        })
    }

    fn body(&mut self, ui: &mut Ui, w: &mut WinCtx) {
        let p = pal(ui.ctx());
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let busy = w.vm.busy();
        let caps = w.vm.caps.clone();

        // Toolbar.
        egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_height(28.0);
                let label = |ui: &mut Ui, t: &str, color: Color32| {
                    ui.label(egui::RichText::new(t).font(theme::regular(BODY)).color(color));
                };
                label(ui, "Flatten above", p.text);
                ui.add_space(6.0);
                let edit = egui::TextEdit::singleline(&mut self.flatten)
                    .id(Id::unique("vf_flatten"))
                    .font(theme::regular(BODY))
                    .text_color(p.text)
                    .align(Align2::RIGHT_CENTER)
                    .margin(egui::Margin::symmetric(4, 3))
                    .background_color(p.panel_alt)
                    .desired_width(52.0);
                let resp = ui.add_sized(vec2(60.0, 22.0), edit);
                if resp.gained_focus()
                    && let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), resp.id)
                {
                    let all = egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(self.flatten.chars().count()),
                    );
                    state.cursor.set_char_range(Some(all));
                    state.store(ui.ctx(), resp.id);
                }
                ui.add_space(4.0);
                label(ui, "mV", p.text);
                ui.add_space(8.0);
                if tool_button(ui, "Apply flatten", ButtonKind::Normal, 10.0, !busy)
                    .on_hover_text(
                        "Clamp every point above this voltage to the frequency at that point — the classic undervolt",
                    )
                    .clicked()
                {
                    self.flatten_clicked(w.vm, &caps);
                }
                ui.add_space(6.0);
                if tool_button(ui, "Remove flatten", ButtonKind::Normal, 10.0, !busy)
                    .on_hover_text("Clear the active voltage cap and reveal the underlying curve again")
                    .clicked()
                {
                    self.remove_flatten_clicked(w.vm, &caps);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if tool_button(ui, "Apply to GPU", ButtonKind::Primary, 14.0, !busy).clicked() {
                        self.apply_clicked(w.vm, &caps);
                    }
                    ui.add_space(6.0);
                    if tool_button(ui, "Reset to stock", ButtonKind::Danger, 12.0, !busy).clicked() {
                        self.reset_clicked(w.vm, &caps);
                    }
                    ui.add_space(6.0);
                    if tool_button(ui, "Reload from GPU", ButtonKind::Normal, 12.0, true).clicked()
                        && let Some(tap) = &self.tap
                    {
                        tap.spawn(|svc| CurveMsg::Loaded { read: Box::new(read_curve(svc)), note: None });
                    }
                });
            });
        });
        ui.add_space(10.0);

        // The status line sits at the bottom; the editor takes everything between.
        let inner_w = ui.available_width() - 20.0;
        let color = if self.status_error { p.danger } else { p.muted };
        let galley = ui.painter().layout(self.status.clone(), theme::regular(BODY), color, inner_w);
        let status_h = galley.size().y + 12.0;
        let editor_h = (ui.available_height() - status_h - 10.0).max(80.0);
        egui::Frame::new().inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            let size = vec2(ui.available_width(), editor_h - 20.0);
            let changed = self.curve_editor(ui, size);
            if changed && let Some(s) = self.editor.edit_status() {
                self.say(s, false);
            }
        });
        ui.add_space(10.0);
        egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let (r, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
            ui.painter().galley(r.min, galley, color);
        });
    }

    // ------------------------------------------------------------------ buttons

    /// Flatten = cap the voltage. On NVIDIA the delta table only owns the lower curve points (the
    /// driver derives the top ~23 from them), so a per-point flatten cannot reach a cap up at
    /// 1000 mV; the voltage lock expresses the same intent and applies immediately. Elsewhere it
    /// flattens the points, to be written by Apply to GPU.
    fn flatten_clicked(&mut self, vm: &mut MainViewModel, caps: &GpuCapabilities) {
        let Ok(mv) = self.flatten.trim().parse::<i32>() else {
            self.say("Enter a voltage in mV.", true);
            return;
        };
        // Free text, so clamp it to the window the main slider offers: a big enough number would
        // overflow the microvolt conversion into something arbitrary.
        let lo = if caps.min_voltage_mv > 0 { caps.min_voltage_mv } else { 600 };
        let boost = vm.boost_ceiling_mv();
        let hi = if caps.can_edit_vf_curve {
            caps.max_voltage_mv
        } else if boost > 0 {
            boost
        } else {
            1200
        };
        if mv < lo || mv > hi {
            self.say(format!("Voltage must be between {lo} and {hi} mV on this card."), true);
            return;
        }
        if vm.backend_name.starts_with("NVIDIA") {
            let Some(tx) = self.tap.as_ref().map(Tap::sender) else { return };
            vm.queue_custom("Setting voltage cap…", move |sh| {
                let result = sh.service.set_voltage_lock(mv);
                Box::new(move |vm: &mut MainViewModel| match result {
                    Ok(()) => {
                        // The editor draws everything above the cap flat without touching the
                        // stored points, so Remove flatten can reveal the exact curve again.
                        let _ = tx.send(CurveMsg::Cap(mv));
                        vm.set(Field::TargetVoltage, mv); // and the main window, which saves it
                        let _ = tx.send(CurveMsg::Say(
                            format!("Voltage capped at {mv} mV — applied to the GPU now. Check the live voltage."),
                            false,
                        ));
                    }
                    Err(e) => {
                        let _ = tx.send(CurveMsg::Say(format!("Could not set the voltage cap: {}", e.message), true));
                    }
                })
            });
            return;
        }
        if self.editor.flatten_from(mv) {
            self.say(format!("Flattened everything above {mv} mV. Press Apply to GPU to commit."), false);
        } else {
            self.say(format!("No curve point at or below {mv} mV — nothing to flatten."), true);
        }
    }

    fn remove_flatten_clicked(&mut self, vm: &mut MainViewModel, caps: &GpuCapabilities) {
        let Some(tap) = &self.tap else { return };
        if caps.can_edit_vf_curve {
            let note = "Reloaded the applied curve. Use Reset to stock to remove an applied Intel curve flatten.";
            tap.spawn(move |svc| CurveMsg::Loaded {
                read: Box::new(read_curve(svc)),
                note: Some((note.into(), false)),
            });
            return;
        }
        let tx = tap.sender();
        vm.queue_custom("Removing voltage cap…", move |sh| {
            let svc = &sh.service;
            let result: Result<Option<i32>, String> = (|| {
                let before = read_cap_mv(svc);
                if before <= 0 {
                    return Ok(None);
                }
                svc.set_voltage_lock(0).map_err(|e| e.message)?;
                let after = read_cap_mv(svc);
                if after > 0 {
                    return Err(format!("the driver still reports a {after} mV cap"));
                }
                Ok(Some(before))
            })();
            Box::new(move |vm: &mut MainViewModel| {
                let msg = match result {
                    Ok(None) => CurveMsg::Say("No active voltage flatten to remove.".into(), false),
                    Ok(Some(before)) => {
                        let _ = tx.send(CurveMsg::Cap(0));
                        // At the reachable ceiling the profile serialises this as "no cap", so a
                        // later Apply cannot quietly restore the flatten just removed.
                        let ceiling = vm.reachable_ceiling_mv();
                        vm.set(Field::TargetVoltage, ceiling);
                        CurveMsg::Say(
                            format!("Voltage flatten removed (was {before} mV). The underlying curve is unchanged."),
                            false,
                        )
                    }
                    Err(e) => CurveMsg::Say(format!("Could not remove the voltage flatten: {e}"), true),
                };
                let _ = tx.send(msg);
            })
        });
    }

    fn reset_clicked(&mut self, vm: &mut MainViewModel, caps: &GpuCapabilities) {
        self.editor.reset_to_stock();
        if let Some(s) = self.editor.edit_status() {
            self.say(s, false);
        }
        let Some(tx) = self.tap.as_ref().map(Tap::sender) else { return };
        let intel = caps.can_edit_vf_curve;
        vm.queue_custom("Resetting the curve…", move |sh| {
            let result = sh.service.set_vf_curve_targets(&[]).map_err(|e| e.message);
            let reread = (result.is_ok() && intel).then(|| read_curve(&sh.service));
            Box::new(move |vm: &mut MainViewModel| {
                let done = "Curve cleared on the GPU — back to stock.".to_owned();
                match (result, reread) {
                    (Ok(()), Some(read)) => {
                        vm.set(Field::CoreOffset, 0);
                        let _ = tx.send(CurveMsg::Loaded { read: Box::new(read), note: Some((done, false)) });
                    }
                    (Ok(()), None) => {
                        let _ = tx.send(CurveMsg::Say(done, false));
                    }
                    (Err(e), _) => {
                        let _ = tx.send(CurveMsg::Say(format!("Reset failed: {e}"), true));
                    }
                }
            })
        });
    }

    fn apply_clicked(&mut self, vm: &mut MainViewModel, caps: &GpuCapabilities) {
        let Some(tx) = self.tap.as_ref().map(Tap::sender) else { return };
        // Only the points that differ; the backend returns the rest to stock.
        let targets = self.editor.targets();
        let intel = caps.can_edit_vf_curve;
        vm.queue_custom("Applying the curve…", move |sh| {
            let svc = &sh.service;
            let result: Result<(Option<i32>, Vec<VfCurveSample>), String> = (|| {
                svc.set_vf_curve_targets(&targets).map_err(|e| e.message)?;
                let core = if intel { Some(svc.read_tuning_state().map_err(|e| e.message)?.core_offset_mhz) } else { None };
                // Read back so the user sees what the driver really accepted.
                let after = svc.read_vf_curve().map_err(|e| e.message)?;
                Ok((core, after))
            })();
            let sent = targets.len();
            Box::new(move |vm: &mut MainViewModel| match result {
                Ok((core, after)) => {
                    if let Some(c) = core {
                        vm.set(Field::CoreOffset, c);
                    }
                    let applied = after.iter().filter(|p| p.live_mhz != p.stock_mhz).count();
                    let msg = if sent == 0 {
                        "Curve cleared — back to stock.".to_owned()
                    } else if after.is_empty() {
                        "Applied, but the driver did not return the curve for read-back — cannot confirm what stuck."
                            .to_owned()
                    } else {
                        format!(
                            "Applied. Driver reports {applied} point(s) changed. Graph now shows what the card actually took."
                        )
                    };
                    if !after.is_empty() {
                        let _ = tx.send(CurveMsg::Points(after));
                    }
                    let _ = tx.send(CurveMsg::Say(msg, false));
                }
                Err(e) => {
                    let _ = tx.send(CurveMsg::Say(format!("Apply failed: {e}"), true));
                }
            })
        });
    }

    // ------------------------------------------------------------------ the plot

    /// Pointer and keyboard handling, then painting. Returns true when the curve changed.
    fn curve_editor(&mut self, ui: &mut Ui, size: egui::Vec2) -> bool {
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let id = Id::unique("vf_curve_editor");
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        let g = VfGeom { w: f64::from(rect.width()), h: f64::from(rect.height()) };
        let mut changed = false;

        let (pressed, secondary, down, moving, pos, now, mods) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.secondary_pressed(),
                i.pointer.primary_down(),
                i.pointer.is_moving(),
                i.pointer.interact_pos(),
                i.time,
                i.modifiers,
            )
        });
        let inside = resp.hovered() && pos.is_some_and(|at| rect.contains(at));
        if let Some(at) = pos {
            let (x, y) = (f64::from(at.x - rect.left()), f64::from(at.y - rect.top()));
            if pressed && inside {
                resp.request_focus();
                let double =
                    self.last_press.is_some_and(|(t, p0)| now - t <= DOUBLE_CLICK_S && (at - p0).length() < 6.0);
                self.last_press = if double { None } else { Some((now, at)) };
                changed |= self.editor.press(&g, x, y, double);
            } else if down && moving && self.editor.dragging().is_some() {
                changed |= self.editor.drag_to(&g, y, mods.shift, mods.ctrl);
            }
            if secondary && inside {
                changed |= self.editor.right_click();
            }
        }
        // Button up, or the drag lost to Alt+Tab / a dialog: unfreeze the axes.
        if !down && self.editor.dragging().is_some() {
            self.editor.end_drag();
        }

        // Arrow keys while the plot has focus (kept from egui's focus navigation).
        if resp.has_focus() {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() },
                )
            });
            let keys: Vec<(Key, egui::Modifiers)> = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                        _ => None,
                    })
                    .collect()
            });
            for (key, m) in keys {
                let k = match key {
                    Key::ArrowLeft => VfKey::Left,
                    Key::ArrowRight => VfKey::Right,
                    Key::ArrowUp => VfKey::Up,
                    Key::ArrowDown => VfKey::Down,
                    _ => continue,
                };
                changed |= self.editor.key(k, m.shift, m.ctrl).1;
            }
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        }
        paint(ui, rect, &g, &self.editor);
        changed
    }
}

/// A toolbar button: WPF padding, all one height as the StackPanel stretches them.
fn tool_button(ui: &mut Ui, text: &str, kind: ButtonKind, pad_x: f32, enabled: bool) -> egui::Response {
    let w = ui.painter().layout_no_wrap(text.to_owned(), theme::regular(BODY), Color32::WHITE).size().x;
    widgets::button(ui, text, kind, vec2((w + 2.0 * pad_x + 2.0).round(), 28.0), enabled, kind == ButtonKind::Primary)
}

/// The editor's own high-contrast colours (it drew into a DrawingContext, outside the brushes).
struct VfColors {
    grid: Color32,
    label: Color32,
    stock: Color32,
    curve: Color32,
    point_fill: Color32,
    select: Color32,
    live: Color32,
    ceiling: Color32,
    unreachable: Color32,
    cap: Color32,
    cap_shade: Color32,
}

impl VfColors {
    fn for_mode(dark: bool) -> Self {
        let rgb = Color32::from_rgb;
        let argb = |a, r, g, b| Color32::from_rgba_unmultiplied(r, g, b, a);
        if dark {
            Self {
                grid: rgb(0x38, 0x38, 0x38),
                label: Color32::WHITE,
                stock: rgb(0xA0, 0xA0, 0xA0),
                curve: rgb(0xFF, 0x5A, 0x5F),
                point_fill: Color32::BLACK,
                select: Color32::WHITE,
                live: rgb(0xE0, 0x70, 0x4B),
                ceiling: rgb(0x7F, 0xB0, 0xE8),
                unreachable: argb(0x18, 0x7F, 0xB0, 0xE8),
                cap: Color32::WHITE,
                cap_shade: argb(26, 255, 255, 255),
            }
        } else {
            Self {
                grid: rgb(0xD0, 0xD0, 0xD0),
                label: Color32::BLACK,
                stock: rgb(0x62, 0x66, 0x6D),
                curve: rgb(0xB9, 0x1C, 0x1C),
                point_fill: Color32::WHITE,
                select: Color32::BLACK,
                live: rgb(0xB4, 0x53, 0x09),
                ceiling: rgb(0x25, 0x63, 0xEB),
                unreachable: argb(0x18, 0x25, 0x63, 0xEB),
                cap: Color32::BLACK,
                cap_shade: argb(20, 15, 23, 42),
            }
        }
    }
}

/// Paint the V/F editor (C# `VfCurveEditor.OnRender`).
fn paint(ui: &Ui, rect: Rect, g: &VfGeom, e: &VfEditor) {
    use vf_editor::{PAD_B, PAD_L, PAD_R, PAD_T};
    let (w, h) = (g.w, g.h);
    if w < 60.0 || h < 60.0 {
        return;
    }
    let c = VfColors::for_mode(pal(ui.ctx()).dark);
    let painter = ui.painter_at(rect);
    let at = |x: f64, y: f64| pos2(rect.left() + x as f32, rect.top() + y as f32);
    let font = theme::regular(BODY);
    let pts = e.points();
    if pts.is_empty() {
        painter.text(at(PAD_L, h / 2.0), Align2::LEFT_TOP, "V/F curve unavailable on this card/driver.", font, c.label);
        return;
    }
    let a = e.axes();
    let sc = |v: f64, f: f64| e.to_screen_in(&a, g, v, f);
    let grid = Stroke::new(1.0, c.grid);

    // Horizontal grid — frequency, every 200 MHz as Afterburner.
    let mut f = a.f_min;
    while f <= a.f_max {
        let y = sc(f64::from(a.v_min), f64::from(f)).1.round() + 0.5;
        painter.line_segment([at(PAD_L, y), at(w - PAD_R, y)], grid);
        painter.text(at(PAD_L - 6.0, y), Align2::RIGHT_CENTER, f.to_string(), font.clone(), c.label);
        f += 200;
    }
    // Vertical grid — voltage every 25 mV, labelled as densely as fits.
    let every = vf_editor::voltage_label_every(g.plot_w(), &a, 25);
    let mut v = a.v_min;
    let mut vi = 0usize;
    while v <= a.v_max {
        let x = sc(f64::from(v), f64::from(a.f_min)).0.round() + 0.5;
        painter.line_segment([at(x, PAD_T), at(x, h - PAD_B)], grid);
        if vi.is_multiple_of(every) {
            painter.text(at(x, h - PAD_B + 5.0), Align2::CENTER_TOP, v.to_string(), font.clone(), c.label);
        }
        v += 25;
        vi += 1;
    }
    // Axis titles, centred on their axes.
    painter.text(
        at(PAD_L + g.plot_w() / 2.0, h - PAD_B + 22.0),
        Align2::CENTER_TOP,
        "Voltage (mV)",
        font.clone(),
        c.label,
    );
    let y_title = painter.layout_no_wrap("Core clock (MHz)".into(), font.clone(), c.label);
    let y_mid = PAD_T + g.plot_h() / 2.0;
    let size = y_title.size();
    let origin = at(15.0, y_mid) - size / 2.0;
    painter.add(TextShape::new(origin, y_title, c.label).with_angle_and_anchor(-FRAC_PI_2, Align2::CENTER_CENTER));

    // Curve geometry is clipped to the plot, so idle points below the frame and a live marker
    // past the curve's top cannot spill into the axis gutters.
    let plot = Rect::from_min_max(at(PAD_L, PAD_T), at((w - PAD_R).max(PAD_L), (h - PAD_B).max(PAD_T)));
    let painter = painter.with_clip_rect(plot);
    let pt = |i: usize, mhz: i32| {
        let (x, y) = sc(f64::from(pts[i].voltage_mv), f64::from(mhz));
        at(x, y)
    };

    // Stock curve (dim, dashed reference).
    let stock: Vec<_> = (0..pts.len()).map(|i| pt(i, pts[i].stock_mhz)).collect();
    painter.extend(Shape::dashed_line(&stock, Stroke::new(1.0, c.stock), 2.0, 2.0));
    // The edited curve as the card will run it.
    let live: Vec<_> = (0..pts.len()).map(|i| pt(i, e.display_mhz(i))).collect();
    painter.add(Shape::line(live, Stroke::new(2.0, c.curve)));

    // Past its last point the table has no more curve, so the card holds that clock up to the
    // boost ceiling: dashed, in the ceiling's colour, because it is extrapolation.
    let last = pts.len() - 1;
    let last_mv = pts[last].voltage_mv;
    let ceiling = e.ceiling_mv();
    if ceiling > last_mv && last_mv >= a.v_min {
        let ey = sc(f64::from(last_mv), f64::from(e.display_mhz(last))).1;
        let x0 = sc(f64::from(last_mv), 0.0).0;
        let x1 = sc(f64::from(ceiling.min(a.v_max)), 0.0).0;
        painter.extend(Shape::dashed_line(&[at(x0, ey), at(x1, ey)], Stroke::new(2.0, c.ceiling), 4.0, 4.0));
    }

    // Points: edited ones filled; the dragged/selected one ringed and larger.
    for (i, q) in pts.iter().enumerate() {
        let s = pt(i, e.display_mhz(i));
        let modified = !e.is_capped(i) && q.live_mhz != q.stock_mhz;
        let active = Some(i) == e.dragging() || Some(i) == e.selected();
        let fill = if modified || active { c.curve } else { c.point_fill };
        let stroke = Stroke::new(2.0, if active { c.select } else { c.curve });
        painter.circle(s, if active { 6.0 } else { 4.5 }, fill, stroke);
    }

    // Readout for the point being worked on.
    if let Some((i, text)) = e.readout() {
        let (x, y) = sc(f64::from(pts[i].voltage_mv), f64::from(pts[i].live_mhz));
        let galley = painter.layout_no_wrap(text, font.clone(), c.label);
        let tx = (x + 10.0).min(w - f64::from(galley.size().x) - 4.0);
        painter.galley(at(tx, PAD_T.max(y - 20.0)), galley, c.label);
    }

    // A cap usually explains where the ceiling sits; then both markers land on the same mV and
    // the cap carries the wording.
    let cap = e.cap_mv();
    let ceiling_is_cap = cap > 0 && ceiling > 0 && (ceiling - cap).abs() <= 1;
    if cap > 0 && cap >= a.v_min && cap <= a.v_max {
        let cap_x = sc(f64::from(cap), 0.0).0;
        painter.rect_filled(
            Rect::from_min_max(at(cap_x, PAD_T), at((w - PAD_R).max(cap_x), PAD_T + g.plot_h())),
            CornerRadius::ZERO,
            c.cap_shade,
        );
        painter.line_segment([at(cap_x, PAD_T), at(cap_x, h - PAD_B)], Stroke::new(1.5, c.cap));
        if let Some(at_cap) = e.cap_point().filter(|p| p.voltage_mv > 0) {
            let cap_y = sc(f64::from(cap), f64::from(at_cap.live_mhz)).1;
            painter.extend(Shape::dashed_line(
                &[at(cap_x, cap_y), at(w - PAD_R, cap_y)],
                Stroke::new(2.0, c.cap),
                4.0,
                4.0,
            ));
            // Labelled at the top of its line, clear of the selected point's readout.
            let text = format!(
                "capped {cap} mV → {} MHz{}",
                at_cap.live_mhz,
                if ceiling_is_cap { " — points above are unreachable" } else { "" }
            );
            let galley = painter.layout_no_wrap(text, font.clone(), c.cap);
            let tx = (cap_x + 6.0).min(w - PAD_R - f64::from(galley.size().x) - 2.0);
            painter.galley(at(tx, PAD_T + 4.0), galley, c.cap);
        }
    }

    // Where the card stops: past the table's end it is boost headroom; inside the curve every
    // point above it is drawn but unreachable.
    if ceiling > 0 && !ceiling_is_cap && ceiling >= a.v_min && ceiling <= a.v_max {
        let cx = sc(f64::from(ceiling), 0.0).0;
        painter.extend(Shape::dotted_line(&[at(cx, PAD_T), at(cx, h - PAD_B)], c.ceiling, 3.0, 0.75));
        if ceiling < last_mv {
            let rx = sc(f64::from(last_mv.min(a.v_max)), 0.0).0;
            if rx > cx {
                painter.rect_filled(
                    Rect::from_min_max(at(cx, PAD_T), at(rx, h - PAD_B)),
                    CornerRadius::ZERO,
                    c.unreachable,
                );
            }
        }
        let text = if ceiling > last_mv {
            format!("boost ceiling {ceiling} mV — table ends at {last_mv}")
        } else {
            format!("card tops out at {ceiling} mV — points above are unreachable")
        };
        let galley = painter.layout_no_wrap(text, font.clone(), c.ceiling);
        let tw = f64::from(galley.size().x);
        let tx = if cx + 6.0 + tw < w - PAD_R { cx + 6.0 } else { cx - 6.0 - tw };
        painter.galley(at(tx, PAD_T + 20.0), galley, c.ceiling);
    }

    // The live operating point, only when it is inside the frame.
    let (lv, lf) = e.live();
    if !lv.is_nan() && lv >= f64::from(a.v_min) && lv <= f64::from(a.v_max) {
        let x = sc(lv, 0.0).0;
        painter.extend(Shape::dashed_line(&[at(x, PAD_T), at(x, h - PAD_B)], Stroke::new(1.0, c.live), 4.0, 4.0));
        if !lf.is_nan() && lf >= f64::from(a.f_min) && lf <= f64::from(a.f_max) {
            let (x, y) = sc(lv, lf);
            painter.circle_filled(at(x, y), 4.0, c.live);
        }
    }
}
