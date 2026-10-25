//! Viewer-style live appearance settings, with GPU-specific panels and telemetry layouts.
use std::collections::BTreeMap;
use std::sync::Arc;

use super::fonts::{self, Family};
use super::preferences::{self, COLOR_ROLES, Card, Colors, PANELS, Prefs, ThemeMode};
use super::shared::Shared;
use super::theme::{self, Palette};
use super::windows::{ChildChrome, Placement};
use eframe::egui::{self, Context, IconData, RichText, Ui, ViewportId, vec2};

const NAV_WIDTH: f32 = 130.0;
const TEXT_SIZE: f32 = 13.0;

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Appearance,
    Colours,
    Text,
    Layout,
    Telemetry,
    Window,
    Behaviour,
}
impl Page {
    const ALL: [(Self, &'static str); 7] = [
        (Self::Appearance, "Appearance"),
        (Self::Colours, "Colours"),
        (Self::Text, "Text"),
        (Self::Layout, "Layout"),
        (Self::Telemetry, "Telemetry"),
        (Self::Window, "Window"),
        (Self::Behaviour, "Behaviour"),
    ];
}

pub struct SettingsWindow {
    chrome: ChildChrome,
    page: Page,
    fonts: Option<Vec<Family>>,
    font_filter: String,
    sensor_filter: String,
    editing_dark: Option<bool>,
    hex: BTreeMap<String, String>,
}
impl Default for SettingsWindow {
    fn default() -> Self {
        Self {
            chrome: ChildChrome::default(),
            page: Page::Appearance,
            fonts: None,
            font_filter: String::new(),
            sensor_filter: String::new(),
            editing_dark: None,
            hex: BTreeMap::new(),
        }
    }
}
impl SettingsWindow {
    pub fn id() -> ViewportId {
        ViewportId::from_hash_of("roch_settings")
    }
    pub fn focus(&mut self) {
        self.chrome.request_focus();
    }
    pub fn show(
        &mut self,
        ctx: &Context,
        prefs: &mut Prefs,
        shared: &Shared,
        icon: &Arc<IconData>,
        main: Option<egui::Rect>,
        catalog: &[(String, String)],
    ) -> bool {
        let dark = super::theme::pal(ctx).dark;
        let builder = self
            .chrome
            .builder("Roch GPU — Settings", vec2(640.0, 600.0), icon, Placement::CenterOwner, main)
            .with_min_inner_size(vec2(520.0, 420.0));
        ctx.show_viewport_immediate(Self::id(), builder, |ui, _| {
            let mut closed = self.chrome.frame(ui.ctx(), dark) || ui.input(|i| i.key_pressed(egui::Key::Escape));
            let palette = theme::pal(ui.ctx());
            settings_style(ui, &palette);
            egui::Panel::bottom("gpu-settings-footer")
                .frame(egui::Frame::new().fill(palette.bg).inner_margin(egui::Margin::symmetric(12, 8)))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .button("Reset all")
                            .on_hover_text("Reset UI settings; keep GPU tuning and profiles")
                            .clicked()
                        {
                            *prefs = Prefs::default();
                            self.hex.clear();
                        }
                        if ui
                            .button("Settings file")
                            .on_hover_text("Open the folder that holds settings.json")
                            .clicked()
                            && let Some(folder) = shared.store.settings_path().parent()
                            && let Err(error) = super::native::shell_open(&folder.display().to_string())
                        {
                            shared.log(&error);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Close").clicked() {
                                closed = true;
                            }
                        });
                    });
                });
            egui::Panel::left("gpu-settings-nav")
                .exact_size(NAV_WIDTH)
                .resizable(false)
                .frame(egui::Frame::new().fill(palette.bg).inner_margin(egui::Margin::symmetric(8, 12)))
                .show(ui, |ui| {
                    for (page, name) in Page::ALL {
                        let selected = self.page == page;
                        let button = egui::Button::new(RichText::new(name).color(if selected {
                            egui::Color32::WHITE
                        } else {
                            palette.text
                        }))
                        .fill(if selected { palette.brand } else { palette.bg })
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(14)
                        .min_size(vec2(NAV_WIDTH - 16.0, 26.0));
                        if ui.add(button).clicked() {
                            self.page = page;
                        }
                    }
                });
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(palette.bg).inner_margin(egui::Margin {
                    left: 14,
                    right: 14,
                    top: 12,
                    bottom: 4,
                }))
                .show(ui, |ui| {
                    egui::ScrollArea::both().id_salt(self.page as u8).auto_shrink([false, false]).show(ui, |ui| {
                        ui.heading(
                            RichText::new(Page::ALL.iter().find(|(page, _)| *page == self.page).unwrap().1)
                                .color(palette.brand),
                        );
                        ui.add_space(6.0);
                        match self.page {
                            Page::Appearance => self.appearance(ui, prefs),
                            Page::Colours => self.colours(ui, prefs),
                            Page::Text => self.text(ui, prefs),
                            Page::Layout => self.layout(ui, prefs),
                            Page::Telemetry => self.telemetry(ui, prefs, catalog),
                            Page::Window => Self::window(ui, prefs),
                            Page::Behaviour => Self::behaviour(ui, prefs, shared),
                        }
                    });
                });
            !closed
        })
    }
    fn appearance(&mut self, ui: &mut Ui, p: &mut Prefs) {
        section(ui, "Theme");
        ui.horizontal(|ui| {
            for mode in ThemeMode::ALL {
                ui.radio_value(&mut p.theme, mode, mode.name());
            }
        });
        section(ui, "Colour scheme");
        ui.horizontal_wrapped(|ui| {
            for preset in preferences::presets() {
                if ui.selectable_label(preset.light == p.light && preset.dark == p.dark, preset.name).clicked() {
                    p.light = preset.light;
                    p.dark = preset.dark;
                    self.hex.clear();
                }
            }
        });
        hint(ui, "A starting point: every colour can be changed on the Colours page.");
        section(ui, "Contrast");
        let mut percent = (p.contrast * 100.0).round();
        if ui.add(egui::Slider::new(&mut percent, -100.0..=100.0).step_by(5.0).fixed_decimals(0).suffix("%")).changed()
        {
            p.contrast = percent / 100.0;
        }
        hint(ui, "Above 0 pushes text and readings further from the background; below 0 softens them.");
        section(ui, "Rows");
        ui.checkbox(&mut p.row_shading, "Shade every other row");
        ui.horizontal(|ui| {
            ui.label("Corner roundness");
            ui.add(egui::Slider::new(&mut p.corner_radius, 0.0..=14.0).step_by(1.0).fixed_decimals(0).suffix(" px"));
        });
    }
    fn colours(&mut self, ui: &mut Ui, prefs: &mut Prefs) {
        let palette = theme::pal(ui.ctx());
        let editing_dark = self.editing_dark.unwrap_or(palette.dark);
        ui.horizontal(|ui| {
            ui.label("Editing");
            let mut dark = editing_dark;
            ui.selectable_value(&mut dark, false, "Light mode");
            ui.selectable_value(&mut dark, true, "Dark mode");
            if dark != editing_dark {
                self.editing_dark = Some(dark);
                self.hex.clear();
            }
        });
        hint(ui, "Each mode keeps its own colours. Click a swatch to pick, or type a #RRGGBB value.");
        ui.add_space(6.0);
        let defaults = if editing_dark { Colors::DEFAULT_DARK } else { Colors::DEFAULT_LIGHT };
        egui::Grid::new("colour-roles").num_columns(4).spacing(vec2(12.0, 8.0)).show(ui, |ui| {
            for (role, label) in COLOR_ROLES {
                let key = format!("{}-{role}", if editing_dark { "dark" } else { "light" });
                let colors = prefs.colors_mut(editing_dark);
                let mut color = colors.get(role);
                ui.label(*label);
                if egui::color_picker::color_edit_button_srgba(ui, &mut color, egui::color_picker::Alpha::Opaque)
                    .changed()
                {
                    colors.set(role, color);
                    self.hex.remove(&key);
                }
                let text = self.hex.entry(key.clone()).or_insert_with(|| preferences::hex(color));
                let response = ui.add_sized(
                    [96.0, 22.0],
                    egui::TextEdit::singleline(text).font(egui::TextStyle::Monospace).char_limit(7),
                );
                if response.changed()
                    && let Some(parsed) = preferences::parse_hex(text)
                {
                    prefs.colors_mut(editing_dark).set(role, parsed);
                }
                if response.lost_focus() {
                    *text = preferences::hex(prefs.colors(editing_dark).get(role));
                }
                let default = defaults.get(role);
                if ui.add_enabled(color != default, egui::Button::new("Default")).clicked() {
                    prefs.colors_mut(editing_dark).set(role, default);
                    self.hex.remove(&key);
                }
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        if ui.button(format!("Reset all {} colours", if editing_dark { "dark" } else { "light" })).clicked() {
            *prefs.colors_mut(editing_dark) = defaults;
            self.hex.clear();
        }
        if !prefs.row_shading {
            hint(ui, "Row shading is off: turn it on under Appearance to see the shading colour.");
        }
    }

    fn text(&mut self, ui: &mut Ui, prefs: &mut Prefs) {
        let palette = theme::pal(ui.ctx());
        section(ui, "Font");
        let families = self.fonts.get_or_insert_with(fonts::installed_families);
        ui.horizontal(|ui| {
            ui.label("Search");
            ui.add(egui::TextEdit::singleline(&mut self.font_filter).desired_width(200.0).hint_text("font name"));
            if ui
                .add_enabled(
                    !prefs.font_family.eq_ignore_ascii_case("Consolas"),
                    egui::Button::new("Consolas (default)"),
                )
                .clicked()
            {
                prefs.font_family = "Consolas".into();
            }
        });
        let filter = self.font_filter.to_lowercase();
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, palette.border))
            .corner_radius(theme::radius(6))
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("font-list").max_height(190.0).auto_shrink([false, true]).show(
                    ui,
                    |ui| {
                        for family in
                            families.iter().filter(|f| filter.is_empty() || f.name.to_lowercase().contains(&filter))
                        {
                            let selected = family.name.eq_ignore_ascii_case(&prefs.font_family);
                            let label = if family.monospace {
                                format!("{}  · fixed width", family.name)
                            } else {
                                family.name.clone()
                            };
                            if ui.selectable_label(selected, label).clicked() {
                                prefs.font_family = family.name.clone();
                            }
                        }
                    },
                );
            });
        hint(
            ui,
            "Fixed-width fonts keep the columns most even. Consolas is laid out to match the original window exactly.",
        );

        section(ui, "Size");
        ui.horizontal(|ui| {
            ui.label("Text size");
            let base = 12.0;
            let mut size = base + prefs.text_delta;
            if ui.add(egui::Slider::new(&mut size, 9.0..=20.0).step_by(0.5).fixed_decimals(1).suffix(" px")).changed() {
                prefs.text_delta = size - base;
            }
        });
        hint(ui, "The windows grow with the text, and rows grow so it stays clear of the next one.");
        ui.checkbox(&mut prefs.bold_headings, "Bold headings, tabs and buttons");
    }

    fn layout(&mut self, ui: &mut Ui, p: &mut Prefs) {
        section(ui, "Scale");
        let mut scale = p.ui_scale * 100.0;
        if ui.add(egui::Slider::new(&mut scale, 75.0..=200.0).step_by(5.0).fixed_decimals(0).suffix("%")).changed() {
            p.ui_scale = scale / 100.0;
        }
        hint(ui, "Everything at once: text, buttons, spacing and the window itself.");
        section(ui, "Row spacing");
        ui.add(egui::Slider::new(&mut p.row_delta, -3.0..=10.0).step_by(1.0).fixed_decimals(0).suffix(" px"));
        hint(ui, "Extra height for every table row; below 0 packs them tighter.");
        section(ui, "Footer");
        ui.checkbox(&mut p.show_footer_links, "Show the YouTube | X | Discord links");
        section(ui, "Main window");
        ui.checkbox(&mut p.show_gpu_details, "Show GPU information details");
        ui.checkbox(&mut p.show_applied_summary, "Show applied settings summary");
        section(ui, "Panels");
        hint(ui, "Top to bottom. Use the arrows to change the order.");
        let mut movement = None;
        let count = p.panels.len();
        for (i, panel) in p.panels.iter_mut().enumerate() {
            ui.push_id(i, |ui| {
                ui.horizontal(|ui| {
                    let label =
                        PANELS.iter().find(|(key, _)| *key == panel.key).map_or(panel.key.as_str(), |(_, name)| *name);
                    ui.checkbox(&mut panel.visible, label);
                    if ui.add_enabled(i > 0, egui::Button::new("↑")).clicked() {
                        movement = Some((i, i - 1));
                    }
                    if ui.add_enabled(i + 1 < count, egui::Button::new("↓")).clicked() {
                        movement = Some((i, i + 1));
                    }
                });
            });
        }
        if let Some((from, to)) = movement {
            p.panels.swap(from, to);
        }
    }
    fn telemetry(&mut self, ui: &mut Ui, p: &mut Prefs, catalog: &[(String, String)]) {
        section(ui, "Layout");
        hint(ui, "Choose cards, labels and order. Open Telemetry to discover all sensors.");
        ui.horizontal(|ui| {
            ui.label("Columns");
            if ui.add_enabled(p.card_columns > 1, egui::Button::new("−")).clicked() {
                p.card_columns -= 1;
            }
            ui.label(p.card_columns.to_string());
            if ui.add_enabled(p.card_columns < 6, egui::Button::new("+")).clicked() {
                p.card_columns += 1;
            }
            if ui.button("Reset cards").clicked() {
                let defaults = Prefs::default();
                p.cards = defaults.cards;
                p.card_columns = defaults.card_columns;
            }
        });
        section(ui, "Cards");
        let mut movement = None;
        let mut remove = None;
        let count = p.cards.len();
        for (i, card) in p.cards.iter_mut().enumerate() {
            ui.push_id(("card", i), |ui| {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(format!("Card {}", i + 1));
                        if ui.add_enabled(i > 0, egui::Button::new("↑")).clicked() {
                            movement = Some((i, i - 1));
                        }
                        if ui.add_enabled(i + 1 < count, egui::Button::new("↓")).clicked() {
                            movement = Some((i, i + 1));
                        }
                        if ui.button("Remove").clicked() {
                            remove = Some(i);
                        }
                    });
                    let selected = catalog
                        .iter()
                        .find(|(key, _)| *key == card.sensor)
                        .map_or(card.sensor.as_str(), |(_, name)| name.as_str());
                    egui::ComboBox::from_id_salt("sensor").selected_text(selected).width(280.0).show_ui(ui, |ui| {
                        for (key, name) in catalog {
                            ui.selectable_value(&mut card.sensor, key.clone(), name);
                        }
                    });
                    ui.add(egui::TextEdit::singleline(&mut card.label).hint_text("Card label"));
                });
            });
        }
        if let Some(i) = remove {
            p.cards.remove(i);
        } else if let Some((from, to)) = movement {
            p.cards.swap(from, to);
        }
        if ui.add_enabled(p.cards.len() < 24, egui::Button::new("Add card")).clicked() {
            p.cards.push(Card { sensor: "temp".into(), label: "GPU temperature".into() });
        }
        section(ui, "Visible sensor rows");
        ui.add(egui::TextEdit::singleline(&mut self.sensor_filter).hint_text("Filter sensors"));
        let filter = self.sensor_filter.to_lowercase();
        for (key, name) in catalog.iter().filter(|(_, name)| name.to_lowercase().contains(&filter)) {
            let mut visible = !p.hidden_sensors.contains(key);
            if ui.checkbox(&mut visible, name).changed() {
                if visible {
                    p.hidden_sensors.retain(|k| k != key);
                } else {
                    p.hidden_sensors.push(key.clone());
                }
            }
        }
    }
    fn window(ui: &mut Ui, p: &mut Prefs) {
        section(ui, "Position");
        ui.checkbox(&mut p.remember_positions, "Remember where each window was");
        ui.horizontal(|ui| {
            hint(ui, &format!("{} saved", p.positions.len()));
            if ui.add_enabled(!p.positions.is_empty(), egui::Button::new("Forget saved positions")).clicked() {
                p.positions.clear();
            }
        });
        section(ui, "On screen");
        ui.checkbox(&mut p.always_on_top, "Keep the windows above other programs");
        let mut opacity = p.opacity * 100.0;
        ui.horizontal(|ui| {
            ui.label("Opacity");
            if ui.add(egui::Slider::new(&mut opacity, 30.0..=100.0).fixed_decimals(0).suffix("%")).changed() {
                p.opacity = opacity / 100.0;
            }
        });
    }
    fn behaviour(ui: &mut Ui, p: &mut Prefs, shared: &Shared) {
        section(ui, "Live readings");
        ui.horizontal(|ui| {
            ui.label("Update every");
            let mut seconds = p.refresh_ms as f32 / 1000.0;
            if ui
                .add(egui::Slider::new(&mut seconds, 0.25..=5.0).step_by(0.25).fixed_decimals(2).suffix(" s"))
                .changed()
            {
                p.refresh_ms = (seconds * 1000.0).round() as u32;
            }
        });
        hint(ui, "How often live readings update. Faster costs a little more processor time.");
        section(ui, "Telemetry");
        let mut auto_open = shared.settings().auto_open_monitor;
        if ui.checkbox(&mut auto_open, "Open Telemetry when Roch GPU starts").changed() {
            shared.settings().auto_open_monitor = auto_open;
            shared.save_settings();
        }
    }
}

fn settings_style(ui: &mut Ui, palette: &Palette) {
    let style = ui.style_mut();
    style.spacing.item_spacing = vec2(8.0, 6.0);
    style.spacing.button_padding = vec2(8.0, 3.0);
    style.spacing.slider_width = 220.0;
    style.spacing.interact_size.y = 22.0;
    for text_style in [egui::TextStyle::Body, egui::TextStyle::Button, egui::TextStyle::Monospace] {
        style
            .text_styles
            .insert(text_style, egui::FontId::new(TEXT_SIZE, egui::FontFamily::Name("settings-mono".into())));
    }
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(TEXT_SIZE + 3.0, egui::FontFamily::Name("settings-bold".into())),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(TEXT_SIZE - 2.0, egui::FontFamily::Name("settings-mono".into())),
    );
    let visuals = &mut style.visuals;
    visuals.widgets.inactive.bg_fill = palette.title_hover;
    visuals.widgets.inactive.weak_bg_fill = palette.title_hover;
    visuals.widgets.hovered.bg_fill = palette.border;
    visuals.widgets.hovered.weak_bg_fill = palette.border;
    visuals.widgets.active.bg_fill = palette.brand;
    visuals.selection.bg_fill = palette.brand;
    visuals.slider_trailing_fill = true;
    visuals.extreme_bg_color = palette.title_hover;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, palette.border);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, palette.brand);
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = egui::CornerRadius::same(6);
    }
}

fn section(ui: &mut Ui, title: &str) {
    let palette = theme::pal(ui.ctx());
    ui.add_space(8.0);
    ui.label(RichText::new(title).color(palette.text));
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, palette.border);
    ui.add_space(2.0);
}

fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).small().color(theme::pal(ui.ctx()).muted));
}
