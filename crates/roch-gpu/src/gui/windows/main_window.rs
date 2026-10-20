//! The main window (C# `MainWindow.xaml`): a custom 30 px title strip, the GPU header, the window
//! buttons, the tuning sliders, the profile bar, Apply/Reset and the footer. It draws into the
//! root viewport and reports what the user asked for as [`MainAction`]s, which the app carries out
//! (opening windows, the tray, the theme), while edits go straight to the view model.

use eframe::egui::{
    self, Align, Align2, Color32, Layout, Rect, RichText, Sense, TextureHandle, Ui, UiBuilder, pos2, vec2,
};

use crate::gui::theme::{self, BODY, pal};
use crate::gui::view_model::{FOOTER_LINKS, Field, MainViewModel, title_text};
use crate::gui::widgets::{self, ButtonKind};

/// Fixed window width (WPF `Width="660"`) and the floor of the content-sized height.
pub const WIDTH: f32 = 660.0;
pub const MIN_HEIGHT: f32 = 640.0;
const TITLE_H: f32 = 30.0;
/// The value box and the unit column beside it, so `%` and `MHz` sit next to the number.
const VALUE_BOX_W: f32 = 60.0;
const UNIT_COL_W: f32 = 26.0;
const VALUE_COL_W: f32 = VALUE_BOX_W + 5.0 + UNIT_COL_W;

/// What the main window asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MainAction {
    OpenCurve,
    OpenXoc,
    OpenFan,
    ToggleMonitor,
    OpenGraphics,
    OpenLog,
    ToggleTheme,
    Minimize,
    Close,
    OpenUrl(&'static str),
    /// Pick another card of the same backend (the header's GPU menu).
    SelectGpu(usize),
    /// Right-click on an occupied slot: confirm, then clear.
    ConfirmClearSlot(i32),
}

/// Draw the whole window; returns the height the content needs (for SizeToContent).
pub fn show(ui: &mut Ui, vm: &mut MainViewModel, icon: Option<&TextureHandle>, actions: &mut Vec<MainAction>) -> f32 {
    let p = pal(ui.ctx());
    let full = ui.max_rect();
    ui.painter().rect_filled(full, 0.0, p.bg);

    // Title strip.
    let title_rect = Rect::from_min_size(full.min, vec2(full.width(), TITLE_H));
    title_bar(ui, title_rect, icon, actions);

    // DockPanel Margin="8".
    let body =
        Rect::from_min_max(pos2(full.left() + 8.0, title_rect.bottom() + 8.0), pos2(full.right() - 8.0, f32::MAX));
    let mut child = ui.new_child(UiBuilder::new().max_rect(body).layout(Layout::top_down(Align::Min)));
    child.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let top = child.cursor().top();
    header(&mut child, vm, actions);
    sliders(&mut child, vm, actions);
    profile_bar(&mut child, vm, actions);
    let controls_bottom = child.cursor().top();

    // The elastic gap: the Apply panel and the footer sit at the bottom of a window that is at
    // least MIN_HEIGHT tall, as in the WPF grid's star row.
    let bottom_h = measure_bottom(&mut child, vm);
    let natural = (controls_bottom - top) + bottom_h;
    let available = full.height() - TITLE_H - 16.0;
    if available > natural {
        child.add_space(available - natural);
    }
    apply_panel(&mut child, vm);
    footer(&mut child, vm, actions);
    TITLE_H + 16.0 + natural
}

fn title_bar(ui: &mut Ui, rect: Rect, icon: Option<&TextureHandle>, actions: &mut Vec<MainAction>) {
    let p = pal(ui.ctx());
    let drag = ui.interact(rect, ui.scope_id().with("title_drag"), Sense::click_and_drag());
    if drag.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    let mut x = rect.left() + 8.0;
    if let Some(tex) = icon {
        let r = Rect::from_center_size(pos2(x + 12.0, rect.center().y), vec2(24.0, 24.0));
        ui.painter().image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        x += 24.0;
    }
    ui.painter().text(pos2(x + 6.0, rect.center().y), Align2::LEFT_CENTER, title_text(), theme::bold(BODY), p.text);

    let mut buttons = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::right_to_left(Align::Center)));
    buttons.spacing_mut().item_spacing.x = 0.0;
    if widgets::title_button(&mut buttons, "\u{00D7}", 44.0, true, 20.0).on_hover_text("Close").clicked() {
        actions.push(MainAction::Close);
    }
    if widgets::title_button(&mut buttons, "\u{2212}", 44.0, false, 18.0).on_hover_text("Minimise").clicked() {
        actions.push(MainAction::Minimize);
    }
    let dark = p.dark;
    let glyph = if dark { "\u{2600}" } else { "" };
    let theme_btn = widgets::title_button(&mut buttons, glyph, 36.0, false, 18.0);
    if !dark {
        let behind = if theme_btn.hovered() { p.title_hover } else { p.bg };
        widgets::paint_moon(&buttons, theme_btn.rect.center(), Color32::BLACK, behind);
    }
    let tip = if dark { "Switch to light mode" } else { "Switch to dark mode" };
    if theme_btn.on_hover_text(tip).clicked() {
        actions.push(MainAction::ToggleTheme);
    }
}

fn header(ui: &mut Ui, vm: &MainViewModel, actions: &mut Vec<MainAction>) {
    let p = pal(ui.ctx());
    // Panel Padding="6,0,6,7".
    egui::Frame::new().inner_margin(egui::Margin { left: 6, right: 6, top: 0, bottom: 7 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.horizontal_top(|ui| {
            let right_w = 80.0 + 80.0 + 64.0 + 3.0 * 6.0;
            // A picker is taller than the plain label; give the header row room for it.
            let gpu_row_h = if vm.gpu_count() > 1 { 38.0 } else { 30.0 };
            ui.allocate_ui_with_layout(
                vec2(ui.available_width() - right_w - 8.0, gpu_row_h),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        // "GPU:" in the normal text colour, the card's name in the accent.
                        ui.label(RichText::new("GPU:").font(theme::bold(BODY)).color(p.text));
                        if vm.gpu_count() > 1 {
                            let current = vm.gpu_index();
                            let mut picked = None;
                            ui.add_enabled_ui(!vm.busy(), |ui| {
                                egui::ComboBox::from_id_salt("gpu_selector")
                                    .width(ui.available_width())
                                    .selected_text(
                                        RichText::new(vm.device_display_name()).font(theme::bold(BODY)).color(p.accent),
                                    )
                                    .show_ui(ui, |ui| {
                                        for i in 0..vm.gpu_count() {
                                            if ui.selectable_label(i == current, vm.gpu_choice_label(i)).clicked() {
                                                picked = Some(i);
                                            }
                                        }
                                    })
                                    .response
                                    .on_hover_text("Choose the card Roch GPU tunes and monitors");
                            });
                            if let Some(i) = picked {
                                actions.push(MainAction::SelectGpu(i));
                            }
                        } else {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(vm.device_display_name()).font(theme::bold(BODY)).color(p.accent),
                                )
                                .wrap(),
                            );
                        }
                    });
                    widgets::info_line(ui, &vm.driver_line());
                },
            );
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let b = |ui: &mut Ui, t: &str, w: f32| {
                    widgets::button(ui, t, ButtonKind::Header, vec2(w, 24.0), true, false)
                };
                if b(ui, "Log", 64.0).clicked() {
                    actions.push(MainAction::OpenLog);
                }
                if b(ui, "Telemetry", 80.0).on_hover_text("Open the hardware monitor in its own window").clicked() {
                    actions.push(MainAction::ToggleMonitor);
                }
                if b(ui, "Graphics", 80.0).on_hover_text("Open static graphics-card and driver information").clicked() {
                    actions.push(MainAction::OpenGraphics);
                }
            });
        });
        widgets::info_line(ui, &vm.bios_line());
        widgets::info_line(ui, &vm.pcie_bus_line());
        widgets::info_line(ui, &vm.resizable_bar_line());
        widgets::info_line(ui, &vm.memory_type_line());
        ui.add_space(2.0);
        let color = if vm.limit_is_active() { p.warn } else { p.text };
        widgets::label(ui, vm.limit_reason_text(), color, false);
    });
}

/// One Afterburner-style row: label (180) | ◀ slider ▶ | value box (88 column).
fn slider_row(ui: &mut Ui, vm: &mut MainViewModel, label: &str, f: Field, enabled: bool, margin_y: f32) {
    slider_row_with_tip(ui, vm, label, f, enabled, margin_y, None);
}

/// [`slider_row`] with a tooltip on the label too. A label too wide for its column breaks before
/// its parenthesis onto a second line.
fn slider_row_with_tip(
    ui: &mut Ui,
    vm: &mut MainViewModel,
    label: &str,
    f: Field,
    enabled: bool,
    margin_y: f32,
    label_tip: Option<&str>,
) {
    ui.add_space(margin_y);
    let w = ui.available_width();
    ui.allocate_ui_with_layout(vec2(w, 30.0), Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let (r, resp) = ui.allocate_exact_size(vec2(180.0, 30.0), Sense::hover());
        let color = if enabled { pal(ui.ctx()).text } else { pal(ui.ctx()).muted };
        let wide = ui.painter().layout_no_wrap(label.to_string(), theme::bold(BODY), color).size().x > r.width() - 4.0;
        let text = if wide { label.replacen(" (", "\n(", 1) } else { label.to_string() };
        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, text, theme::bold(BODY), color);
        if let Some(tip) = label_tip {
            resp.on_hover_text(tip);
        }
        ui.add_space(8.0);
        let slider_w = w - 180.0 - VALUE_COL_W - 8.0 - 18.0;
        widgets::nudged_slider(ui, vm, f, slider_w, enabled, false);
        ui.add_space(18.0);
        ui.allocate_ui_with_layout(vec2(VALUE_COL_W, 30.0), Layout::left_to_right(Align::Center), |ui| {
            let tip = vm.range_text(f);
            widgets::value_box(ui, vm, f, vec2(VALUE_BOX_W, 30.0), enabled).on_hover_text(tip);
            let unit = vm.field_unit(f);
            if !unit.is_empty() {
                ui.add_space(5.0);
                let color = if enabled { pal(ui.ctx()).text } else { pal(ui.ctx()).muted };
                ui.allocate_ui_with_layout(vec2(UNIT_COL_W, 30.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.label(RichText::new(unit).font(theme::regular(BODY)).color(color));
                });
            }
        });
    });
    ui.add_space(margin_y);
}

fn sliders(ui: &mut Ui, vm: &mut MainViewModel, actions: &mut Vec<MainAction>) {
    let busy = vm.busy();
    ui.add_space(4.0);
    egui::Frame::new().inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        // The three hardware editors share one row; a missing one keeps its column.
        let col = (ui.available_width() - 12.0) / 3.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let size = vec2(col, 26.0);
            if vm.has_curve_editor() {
                if widgets::button(ui, "V/F Curve", ButtonKind::Normal, size, true, true)
                    .on_hover_text("Open the voltage/frequency curve (Afterburner Ctrl+F equivalent)")
                    .clicked()
                {
                    actions.push(MainAction::OpenCurve);
                }
            } else {
                ui.add_space(col);
            }
            if vm.has_xoc() {
                if widgets::button(ui, "XOC", ButtonKind::Normal, size, true, true)
                    .on_hover_text("XOC: rail ranges, OCP current limits, and the clock range")
                    .clicked()
                {
                    actions.push(MainAction::OpenXoc);
                }
            } else {
                ui.add_space(col);
            }
            if widgets::button(ui, "Fan", ButtonKind::Normal, size, vm.caps.can_set_fan_speed, true)
                .on_hover_text("Fan control: a duty per fan, or a curve")
                .clicked()
            {
                actions.push(MainAction::OpenFan);
            }
        });
        ui.add_space(7.0 + 8.0);
        widgets::layout_panel(ui, egui::Margin::symmetric(14, 10), |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            if vm.has_voltage_boost() {
                let label = vm.voltage_boost_label();
                slider_row(ui, vm, label, Field::VoltageBoost, !busy, 12.0);
            }
            if vm.is_voltage_offset() {
                slider_row(ui, vm, "Voltage Offset (mV)", Field::VoltageOffset, !busy, 12.0);
            }
            let can_core = vm.caps.can_set_core_offset && !busy;
            slider_row(ui, vm, vm.core_label(), Field::CoreOffset, can_core, 12.0);
            if vm.has_xbar() {
                slider_row(ui, vm, "XBAR Clock Offset", Field::XbarOffset, !busy, 5.0);
            }
            if vm.has_sys() {
                slider_row(ui, vm, "SYS Clock Offset", Field::SysOffset, !busy, 5.0);
            }
            if vm.has_video() {
                slider_row(ui, vm, "Video Clock Offset", Field::VideoOffset, !busy, 5.0);
            }
            let can_mem = vm.caps.can_set_memory_offset && !busy;
            slider_row(ui, vm, vm.memory_label(), Field::MemoryOffset, can_mem, 12.0);
            if vm.has_intel_clock_range() {
                intel_clock_range(ui, vm, busy);
            }
            let can_power = vm.caps.can_set_power_limit && !busy;
            slider_row(ui, vm, vm.power_label(), Field::PowerLimit, can_power, 12.0);
            if vm.has_temp_limit() {
                let label = vm.temp_label();
                slider_row(ui, vm, &label, Field::TempLimit, !busy, 12.0);
            } else if vm.has_soft_temp_limit() {
                // Where the hardware limit would be, for a driver that has none (RTX 50).
                let tip = vm.soft_temp_range_text();
                slider_row_with_tip(ui, vm, vm.soft_temp_label(), Field::SoftTempLimit, !busy, 12.0, Some(&tip));
            }
            if vm.has_memory_timing() {
                memory_timing(ui, vm);
            }
        });
    });
}

fn intel_clock_range(ui: &mut Ui, vm: &mut MainViewModel, busy: bool) {
    ui.add_space(8.0);
    let mut on = vm.intel_clock_range_enabled();
    if widgets::checkbox(ui, &mut on, "Core Clock Range", !busy).changed() {
        vm.set_intel_clock_range_enabled(on);
    }
    ui.add_space(6.0);
    let enabled = vm.intel_clock_range_enabled() && !busy;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label("Minimum");
        let tip = vm.clock_lock_range_text();
        widgets::value_box(ui, vm, Field::ClockLockMin, vec2(80.0, 22.0), enabled).on_hover_text(&tip);
        ui.add_space(8.0);
        ui.label("Maximum");
        widgets::value_box(ui, vm, Field::ClockLockMax, vec2(80.0, 22.0), enabled).on_hover_text(&tip);
        ui.add_space(5.0);
        let color = if enabled { pal(ui.ctx()).text } else { pal(ui.ctx()).muted };
        ui.label(RichText::new("MHz").font(theme::regular(BODY)).color(color));
    });
    ui.add_space(5.0);
    ui.add(egui::Label::new("Apply to set limits. Uncheck and Apply to restore driver defaults.").wrap());
    ui.add_space(8.0);
}

fn memory_timing(ui: &mut Ui, vm: &mut MainViewModel) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Memory Timing").font(theme::bold(BODY)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let options = vm.memory_timing_options().to_vec();
            let mut idx = vm.memory_timing_index();
            let current = options.get(idx.max(0) as usize).cloned().unwrap_or_default();
            egui::ComboBox::from_id_salt("memory_timing").width(230.0).selected_text(current).show_ui(ui, |ui| {
                for (i, o) in options.iter().enumerate() {
                    ui.selectable_value(&mut idx, i as i32, o);
                }
            });
            if idx != vm.memory_timing_index() {
                vm.set_memory_timing_index(idx);
            }
        })
        .response
        .on_hover_text("Faster memory timings at the same clock. Can cause instability — test it.");
    });
}

fn profile_bar(ui: &mut Ui, vm: &mut MainViewModel, actions: &mut Vec<MainAction>) {
    let p = pal(ui.ctx());
    let busy = vm.busy();
    ui.add_space(12.0);
    // Margin="10,12,10,0" Padding="14,14".
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        let w = ui.available_width() - 10.0;
        ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::top_down(Align::Min), |ui| {
            widgets::layout_panel(ui, egui::Margin::same(14), |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let (r, _) = ui.allocate_exact_size(vec2(56.0, 34.0), Sense::hover());
                    ui.painter().text(r.left_center(), Align2::LEFT_CENTER, "PROFILE", theme::bold(BODY), p.text);
                    for slot in vm.slots().to_vec() {
                        let kind = ButtonKind::Slot { occupied: slot.occupied, active: slot.is_active };
                        let resp = widgets::button(ui, &slot.label(), kind, vec2(48.0, 34.0), !busy, true)
                            .on_hover_text(slot.tip());
                        if resp.clicked() {
                            vm.on_slot_clicked(slot.number);
                        }
                        if resp.secondary_clicked() && slot.occupied && !busy {
                            actions.push(MainAction::ConfirmClearSlot(slot.number));
                        }
                        ui.add_space(4.0);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let armed = vm.save_armed();
                        if widgets::button(
                            ui,
                            "Save",
                            ButtonKind::Toggle { checked: armed },
                            vec2(46.0, 34.0),
                            !busy,
                            true,
                        )
                        .on_hover_text("Press Save, then a number, to store the current settings in that slot")
                        .clicked()
                        {
                            vm.set_save_armed(!armed);
                        }
                    });
                });
                if vm.save_armed() {
                    ui.add_space(5.0);
                    widgets::label(ui, "Pick a slot to save into (press Save again to cancel)", p.warn, false);
                }
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let mut on = vm.apply_on_startup();
                    if widgets::checkbox(ui, &mut on, "Startup", !busy)
                        .on_hover_text("Apply this profile automatically at Windows logon")
                        .changed()
                    {
                        vm.set_apply_on_startup(on);
                    }
                    let current = vm.startup_profile().unwrap_or("").to_string();
                    let mut pick: Option<String> = None;
                    ui.add_enabled_ui(!busy, |ui| {
                        egui::ComboBox::from_id_salt("startup_profile")
                            .width(ui.available_width())
                            .selected_text(current.clone())
                            .show_ui(ui, |ui| {
                                for name in vm.profiles() {
                                    if ui.selectable_label(*name == current, name).clicked() {
                                        pick = Some(name.clone());
                                    }
                                }
                            });
                    });
                    if let Some(name) = pick {
                        vm.set_startup_profile(Some(name));
                    }
                });
            });
        });
    });
}

fn measure_bottom(ui: &mut Ui, vm: &MainViewModel) -> f32 {
    // Apply panel: margin 12 + padding 6+6 + button 42 (+ the pending line), then the footer block.
    let pending = if vm.pending_changes() { 6.0 + 15.0 } else { 0.0 };
    let error = if vm.status_is_error { 32.0 } else { 0.0 };
    let summary = summary_height(ui, vm);
    12.0 + 12.0 + 42.0 + pending + 6.0 + error + summary + 24.0
}

fn summary_height(ui: &Ui, vm: &MainViewModel) -> f32 {
    let w = ui.available_width();
    let galley = ui.painter().layout(vm.applied_summary().to_string(), theme::regular(BODY), Color32::WHITE, w);
    galley.size().y.min(48.0)
}

fn apply_panel(ui: &mut Ui, vm: &mut MainViewModel) {
    let p = pal(ui.ctx());
    let busy = vm.busy();
    ui.add_space(12.0);
    egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let w = (ui.available_width() - 8.0) / 2.0;
            if widgets::button(ui, "Apply", ButtonKind::Primary, vec2(w, 42.0), !busy, true).clicked() {
                vm.apply();
            }
            if widgets::button(ui, "Reset to Defaults", ButtonKind::Danger, vec2(w, 42.0), !busy, false)
                .on_hover_text(
                    "Write the driver's own defaults to the card and load them into the sliders (also clears the voltage curve)",
                )
                .clicked()
            {
                vm.reset();
            }
        });
        if vm.pending_changes() {
            ui.add_space(6.0);
            widgets::label(ui, "Unapplied changes", p.warn, false);
        }
    });
}

fn footer(ui: &mut Ui, vm: &MainViewModel, actions: &mut Vec<MainAction>) {
    let p = pal(ui.ctx());
    ui.add_space(6.0);
    if vm.status_is_error {
        ui.add(
            egui::Label::new(RichText::new(&vm.status).font(theme::regular(BODY)).color(p.danger))
                .wrap()
                .selectable(false),
        )
        .on_hover_text(&vm.status);
    }
    let summary = vm.applied_summary();
    let mut job =
        egui::text::LayoutJob::simple(summary.to_string(), theme::regular(BODY), p.text, ui.available_width());
    job.wrap.max_rows = 3;
    job.wrap.overflow_character = Some('…');
    ui.add(egui::Label::new(job).selectable(false)).on_hover_text(summary);

    // The shared 24 px footer: social links left, status right.
    let w = ui.available_width();
    ui.allocate_ui_with_layout(vec2(w, 24.0), Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (i, (name, url)) in FOOTER_LINKS.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new(" | ").font(theme::regular(BODY)).color(p.text));
            }
            let galley = ui.painter().layout_no_wrap(name.to_string(), theme::bold(BODY), p.brand);
            let (r, resp) = ui.allocate_exact_size(galley.size(), Sense::click());
            let color = if resp.hovered() { p.brand_hover } else { p.brand };
            ui.painter().galley_with_override_text_color(r.min, galley, color);
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            let resp = if *name == "X" { resp.on_hover_text(*url) } else { resp };
            if resp.clicked() {
                actions.push(MainAction::OpenUrl(url));
            }
        }
        ui.add_space(12.0);
        let (status, color) = match vm.busy_label() {
            Some(label) => (label.to_string(), p.text),
            None => (vm.status.clone(), if vm.status_is_error { p.danger } else { p.text }),
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
        let mut job = egui::text::LayoutJob::simple_singleline(status.clone(), theme::regular(BODY), color);
        job.wrap.max_width = rect.width();
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        job.wrap.overflow_character = Some('…');
        let galley = ui.painter().layout_job(job);
        let pos = pos2(rect.right() - galley.size().x, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, color);
        if !status.is_empty() {
            resp.on_hover_text(status);
        }
    });
}
