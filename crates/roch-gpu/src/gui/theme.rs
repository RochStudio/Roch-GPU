//! Light and dark, switched live (C# `Native.Theme` + the brushes in `App.xaml`).
//!
//! The shared Roch Viewer palette. Every window reads the current [`Palette`] from the egui context
//! with [`pal`], so flipping the mode repaints everything on the next frame. Typography is the
//! Viewer's: Consolas 12 px (bold for labels and values), loaded from the Windows font folder
//! with egui's built-in fonts as the fallback for glyphs Consolas lacks.

use eframe::egui::{self, Color32, Context, FontData, FontDefinitions, FontFamily, FontId, Id, Stroke};

/// Body text size in points (the WPF 12 device-independent pixels).
pub const BODY: f32 = 12.0;

/// The named brushes of `App.xaml`, resolved for one mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub panel: Color32,
    pub panel_alt: Color32,
    pub border: Color32,
    pub section_border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub header: Color32,
    pub highlight: Color32,
    pub brand: Color32,
    pub brand_hover: Color32,
    pub selected: Color32,
    pub title_hover: Color32,
    pub close_hover: Color32,
    /// GPU information values and the value boxes: red, per the v1.1.0 release.
    pub accent: Color32,
    pub accent_dim: Color32,
    pub warn: Color32,
    pub danger: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    pub const LIGHT: Palette = Palette {
        dark: false,
        bg: rgb(0xFFFFFF),
        panel: rgb(0xFFFFFF),
        panel_alt: rgb(0xFFFFFF),
        border: rgb(0xD0D0D0),
        section_border: rgb(0xD0D0D0),
        text: rgb(0x000000),
        muted: rgb(0x62666D),
        header: rgb(0xFFFFFF),
        highlight: rgb(0xFFFFFF),
        brand: rgb(0xD0343A),
        brand_hover: rgb(0xE0383E),
        selected: rgb(0xD0343A),
        title_hover: rgb(0xE6E6E6),
        close_hover: rgb(0xC42B1C),
        accent: rgb(0xB91C1C),
        accent_dim: rgb(0xE7B9B9),
        warn: rgb(0xB45309),
        danger: rgb(0xDC2626),
    };

    pub const DARK: Palette = Palette {
        dark: true,
        bg: rgb(0x000000),
        panel: rgb(0x000000),
        panel_alt: rgb(0x000000),
        border: rgb(0x383838),
        section_border: rgb(0x383838),
        text: rgb(0xFFFFFF),
        muted: rgb(0xA0A0A0),
        header: rgb(0x000000),
        highlight: rgb(0x000000),
        brand: rgb(0xD0343A),
        brand_hover: rgb(0xE0383E),
        selected: rgb(0xD0343A),
        title_hover: rgb(0x222222),
        close_hover: rgb(0xC42B1C),
        accent: rgb(0xFF5A5F),
        accent_dim: rgb(0x6E1717),
        warn: rgb(0xFF7A7A),
        danger: rgb(0xE53935),
    };

    pub fn for_mode(dark: bool) -> Palette {
        if dark { Self::DARK } else { Self::LIGHT }
    }
}

fn palette_id() -> Id {
    Id::unique("roch_gpu_palette")
}

/// The palette in force for this context.
pub fn pal(ctx: &Context) -> Palette {
    ctx.data(|d| d.get_temp::<Palette>(palette_id())).unwrap_or(Palette::LIGHT)
}

/// The bold body font.
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

/// The regular body font.
pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Load Consolas (regular and bold) and Segoe UI Symbol from the Windows font folder, ahead of
/// egui's own fonts. Missing files are skipped: the app still renders with the built-in fonts.
pub fn install_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
    let mut load = |key: &str, file: &str| -> bool {
        match std::fs::read(dir.join("Fonts").join(file)) {
            Ok(bytes) => {
                fonts.font_data.insert(key.to_owned(), std::sync::Arc::new(FontData::from_owned(bytes)));
                true
            }
            Err(_) => false,
        }
    };
    let regular = load("consolas", "consola.ttf");
    let bold = load("consolas-bold", "consolab.ttf");
    let symbols = load("segoe-symbol", "seguisym.ttf");

    let defaults: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let chain = |first: Option<&str>| -> Vec<String> {
        let mut v: Vec<String> = first.map(|f| vec![f.to_owned()]).unwrap_or_default();
        if symbols {
            v.push("segoe-symbol".into());
        }
        v.extend(defaults.iter().cloned());
        v
    };
    let regular_chain = chain(regular.then_some("consolas"));
    let bold_chain = chain(if bold { Some("consolas-bold") } else { regular.then_some("consolas") });
    fonts.families.insert(FontFamily::Proportional, regular_chain.clone());
    fonts.families.insert(FontFamily::Monospace, regular_chain);
    fonts.families.insert(FontFamily::Name("bold".into()), bold_chain);
    ctx.set_fonts(fonts);
}

/// Paint everything for the given mode: store the palette and rebuild egui's visuals from it.
pub fn apply(ctx: &Context, dark: bool) {
    let p = Palette::for_mode(dark);
    ctx.data_mut(|d| d.insert_temp(palette_id(), p));
    let mut style = (*ctx.global_style()).clone();
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.override_text_color = Some(p.text);
    v.panel_fill = p.bg;
    v.window_fill = p.bg;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.extreme_bg_color = p.panel_alt;
    v.faint_bg_color = p.panel_alt;
    v.hyperlink_color = p.brand;
    v.selection.bg_fill = p.selected;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.text_cursor.stroke = Stroke::new(1.5, p.text);
    v.popup_shadow = egui::Shadow::NONE;
    v.window_shadow = egui::Shadow::NONE;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.bg_fill = p.panel_alt;
        w.weak_bg_fill = p.panel_alt;
        w.bg_stroke = Stroke::new(1.0, p.border);
        w.fg_stroke = Stroke::new(1.0, p.text);
        w.corner_radius = egui::CornerRadius::same(2);
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.section_border);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.brand_hover);
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.brand_hover);
    v.widgets.open.bg_stroke = Stroke::new(1.0, p.brand_hover);
    style.visuals = v;
    style.text_styles.insert(egui::TextStyle::Body, regular(BODY));
    style.text_styles.insert(egui::TextStyle::Button, regular(BODY));
    style.text_styles.insert(egui::TextStyle::Small, regular(10.0));
    style.text_styles.insert(egui::TextStyle::Monospace, regular(BODY));
    style.text_styles.insert(egui::TextStyle::Heading, bold(14.0));
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.interact_size.y = 20.0;
    style.interaction.tooltip_delay = 0.4;
    // The same style under both egui themes, and the preference pinned, so the OS setting never
    // overrides the app's own choice.
    ctx.set_style_of(egui::Theme::Light, style.clone());
    ctx.set_style_of(egui::Theme::Dark, style);
    ctx.set_theme(if dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
}

/// The title-bar mode Windows draws for a viewport (decorated windows only). Sent to each window
/// when it opens and on every switch.
pub fn system_theme(dark: bool) -> egui::SystemTheme {
    if dark { egui::SystemTheme::Dark } else { egui::SystemTheme::Light }
}
