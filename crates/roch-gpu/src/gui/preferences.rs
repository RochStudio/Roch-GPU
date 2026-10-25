//! Appearance preferences shared with Roch Viewer, adapted for the GPU windows.
//!
//! Defaults retain the GPU window's Consolas font and layout at 100% scale.
//! Preferences live in `UiPreferences` in settings.json. The legacy DarkMode
//! and PollIntervalMs fields are also written for compatibility with older builds.

use std::collections::BTreeMap;

use eframe::egui::Color32;
use serde_json::{Map, Value, json};

/// Light, Dark, or whatever Windows is set to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    Dark,
    System,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::Light, ThemeMode::Dark, ThemeMode::System];

    pub fn name(self) -> &'static str {
        match self {
            ThemeMode::Light => "Light",
            ThemeMode::Dark => "Dark",
            ThemeMode::System => "Follow Windows",
        }
    }

    fn key(self) -> &'static str {
        match self {
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
            ThemeMode::System => "system",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key.to_ascii_lowercase().as_str() {
            "light" => Some(ThemeMode::Light),
            "dark" => Some(ThemeMode::Dark),
            "system" => Some(ThemeMode::System),
            _ => None,
        }
    }

    /// Whether the window is dark, given what Windows itself is set to.
    pub fn is_dark(self, system_dark: bool) -> bool {
        match self {
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
            ThemeMode::System => system_dark,
        }
    }
}

/// The colours one mode is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    /// Window and panel background.
    pub background: Color32,
    /// Names, headings in the chrome, ordinary text.
    pub text: Color32,
    /// Every reading.
    pub value: Color32,
    /// The selected tab, section headings, links: the brand colour.
    pub accent: Color32,
    /// The accent under the pointer.
    pub accent_hover: Color32,
    /// Panel and button outlines.
    pub border: Color32,
    /// Readings that are not in effect, and secondary text.
    pub dimmed: Color32,
    /// Every other table row, when row shading is on.
    pub shading: Color32,
    /// An unselected tab or button under the pointer.
    pub hover: Color32,
}

/// The editable colour roles, in the order the Settings window lists them.
pub const COLOR_ROLES: &[(&str, &str)] = &[
    ("background", "Background"),
    ("text", "Text"),
    ("value", "Values"),
    ("accent", "Accent"),
    ("accent_hover", "Accent (hover)"),
    ("border", "Borders"),
    ("dimmed", "Dimmed text"),
    ("shading", "Row shading"),
    ("hover", "Hover"),
];

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Colors {
    /// The Python build's light theme.
    pub const DEFAULT_LIGHT: Colors = Colors {
        background: rgb(0xFFFFFF),
        text: rgb(0x000000),
        value: rgb(0xB91C1C),
        accent: rgb(0xD0343A),
        accent_hover: rgb(0xE0383E),
        border: rgb(0xD0D0D0),
        dimmed: rgb(0x62666D),
        shading: rgb(0xF0F0F0),
        hover: rgb(0xE6E6E6),
    };

    /// The Python build's dark theme.
    pub const DEFAULT_DARK: Colors = Colors {
        background: rgb(0x000000),
        text: rgb(0xFFFFFF),
        value: rgb(0xFF5A5F),
        accent: rgb(0xD0343A),
        accent_hover: rgb(0xE0383E),
        border: rgb(0x383838),
        dimmed: rgb(0xA0A0A0),
        shading: rgb(0x161616),
        hover: rgb(0x222222),
    };

    pub fn get(&self, role: &str) -> Color32 {
        match role {
            "background" => self.background,
            "text" => self.text,
            "value" => self.value,
            "accent" => self.accent,
            "accent_hover" => self.accent_hover,
            "border" => self.border,
            "dimmed" => self.dimmed,
            "shading" => self.shading,
            _ => self.hover,
        }
    }

    pub fn set(&mut self, role: &str, color: Color32) {
        let slot = match role {
            "background" => &mut self.background,
            "text" => &mut self.text,
            "value" => &mut self.value,
            "accent" => &mut self.accent,
            "accent_hover" => &mut self.accent_hover,
            "border" => &mut self.border,
            "dimmed" => &mut self.dimmed,
            "shading" => &mut self.shading,
            _ => &mut self.hover,
        };
        *slot = color;
    }

    fn to_json(self) -> Value {
        let mut map = Map::new();
        for (role, _) in COLOR_ROLES {
            map.insert(role.to_string(), Value::from(hex(self.get(role))));
        }
        Value::Object(map)
    }

    fn from_json(value: Option<&Value>, fallback: Colors) -> Colors {
        let mut colors = fallback;
        if let Some(Value::Object(map)) = value {
            for (role, _) in COLOR_ROLES {
                if let Some(color) = map.get(*role).and_then(Value::as_str).and_then(parse_hex) {
                    colors.set(role, color);
                }
            }
        }
        colors
    }
}

/// `#RRGGBB`.
pub fn hex(color: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
}

/// `#RRGGBB` or `RRGGBB`, any case.
pub fn parse_hex(text: &str) -> Option<Color32> {
    let digits = text.trim().trim_start_matches('#');
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok().map(rgb)
}

/// A named pair of colour sets, one per mode.
pub struct Preset {
    pub name: &'static str,
    pub light: Colors,
    pub dark: Colors,
}

/// Starting points for the colours; every role can be changed after.
pub fn presets() -> Vec<Preset> {
    let tint = |base: Colors, accent: u32, hover: u32, value: u32| Colors {
        accent: rgb(accent),
        accent_hover: rgb(hover),
        value: rgb(value),
        ..base
    };
    vec![
        Preset { name: "Roch (default)", light: Colors::DEFAULT_LIGHT, dark: Colors::DEFAULT_DARK },
        Preset {
            name: "High contrast",
            light: Colors {
                background: rgb(0xFFFFFF),
                text: rgb(0x000000),
                value: rgb(0x8B0000),
                accent: rgb(0x0000C0),
                accent_hover: rgb(0x0000FF),
                border: rgb(0x000000),
                dimmed: rgb(0x404040),
                shading: rgb(0xE8E8E8),
                hover: rgb(0xD0D0D0),
            },
            dark: Colors {
                background: rgb(0x000000),
                text: rgb(0xFFFFFF),
                value: rgb(0xFFFF00),
                accent: rgb(0x00FFFF),
                accent_hover: rgb(0x80FFFF),
                border: rgb(0xFFFFFF),
                dimmed: rgb(0xC0C0C0),
                shading: rgb(0x202020),
                hover: rgb(0x303030),
            },
        },
        Preset {
            name: "Graphite",
            light: Colors {
                background: rgb(0xF3F3F3),
                text: rgb(0x1E1E1E),
                value: rgb(0x9E2A2B),
                accent: rgb(0x4A4A4A),
                accent_hover: rgb(0x5E5E5E),
                border: rgb(0xC8C8C8),
                dimmed: rgb(0x6E6E6E),
                shading: rgb(0xE9E9E9),
                hover: rgb(0xDDDDDD),
            },
            dark: Colors {
                background: rgb(0x1E1E1E),
                text: rgb(0xE6E6E6),
                value: rgb(0xFF7B72),
                accent: rgb(0x5A5A5A),
                accent_hover: rgb(0x6E6E6E),
                border: rgb(0x3C3C3C),
                dimmed: rgb(0x9A9A9A),
                shading: rgb(0x262626),
                hover: rgb(0x2E2E2E),
            },
        },
        Preset {
            name: "Ocean",
            light: tint(Colors::DEFAULT_LIGHT, 0x1F6FEB, 0x388BFD, 0x0B5CAD),
            dark: Colors {
                background: rgb(0x0D1117),
                shading: rgb(0x161B22),
                hover: rgb(0x21262D),
                border: rgb(0x30363D),
                ..tint(Colors::DEFAULT_DARK, 0x1F6FEB, 0x388BFD, 0x58A6FF)
            },
        },
        Preset {
            name: "Forest",
            light: tint(Colors::DEFAULT_LIGHT, 0x2E7D32, 0x388E3C, 0x1B5E20),
            dark: Colors {
                background: rgb(0x0B140D),
                shading: rgb(0x122016),
                hover: rgb(0x1B2C1F),
                border: rgb(0x2A3D2E),
                ..tint(Colors::DEFAULT_DARK, 0x2E7D32, 0x43A047, 0x7EE787)
            },
        },
        Preset {
            name: "Amber",
            light: tint(Colors::DEFAULT_LIGHT, 0xB45309, 0xD97706, 0x92400E),
            dark: Colors {
                background: rgb(0x120D05),
                shading: rgb(0x1C140A),
                hover: rgb(0x2A1E0E),
                border: rgb(0x3A2A14),
                ..tint(Colors::DEFAULT_DARK, 0xB45309, 0xD97706, 0xFFB000)
            },
        },
        Preset {
            name: "Purple",
            light: tint(Colors::DEFAULT_LIGHT, 0x7C3AED, 0x8B5CF6, 0x6D28D9),
            dark: Colors {
                background: rgb(0x0F0B16),
                shading: rgb(0x17111F),
                hover: rgb(0x221A2E),
                border: rgb(0x32284A),
                ..tint(Colors::DEFAULT_DARK, 0x7C3AED, 0x8B5CF6, 0xC4B5FD)
            },
        },
    ]
}

/// Saved presentation choices. GPU tuning and startup profiles remain in AppSettings.
#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub theme: ThemeMode,
    pub light: Colors,
    pub dark: Colors,
    pub contrast: f32,
    pub row_shading: bool,
    pub font_family: String,
    pub text_delta: f32,
    pub row_delta: f32,
    pub bold_headings: bool,
    pub ui_scale: f32,
    pub corner_radius: f32,
    pub remember_positions: bool,
    pub positions: BTreeMap<String, (i32, i32)>,
    pub always_on_top: bool,
    pub opacity: f32,
    pub refresh_ms: u32,
    pub show_footer_links: bool,
    pub show_gpu_details: bool,
    pub show_applied_summary: bool,
    pub panels: Vec<Panel>,
    pub cards: Vec<Card>,
    pub card_columns: usize,
    pub hidden_sensors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Panel {
    pub key: String,
    pub visible: bool,
}

pub const PANELS: &[(&str, &str)] = &[("tuning", "Tuning controls"), ("profiles", "Profile slots")];

#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub sensor: String,
    pub label: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Light,
            light: Colors::DEFAULT_LIGHT,
            dark: Colors::DEFAULT_DARK,
            contrast: 0.0,
            row_shading: false,
            font_family: "Consolas".into(),
            text_delta: 0.0,
            row_delta: 0.0,
            bold_headings: true,
            ui_scale: 1.0,
            corner_radius: 8.0,
            remember_positions: true,
            positions: BTreeMap::new(),
            always_on_top: false,
            opacity: 1.0,
            refresh_ms: 1000,
            show_footer_links: true,
            show_gpu_details: true,
            show_applied_summary: true,
            panels: PANELS.iter().map(|(key, _)| Panel { key: (*key).into(), visible: true }).collect(),
            cards: [("temp", "GPU TEMPERATURE"), ("core", "CORE CLOCK"), ("watts", "BOARD POWER")]
                .into_iter()
                .map(|(sensor, label)| Card { sensor: sensor.into(), label: label.into() })
                .collect(),
            card_columns: 3,
            hidden_sensors: Vec::new(),
        }
    }
}

fn number(map: &Map<String, Value>, key: &str, default: f32, min: f32, max: f32) -> f32 {
    map.get(key)
        .and_then(Value::as_f64)
        .map(|v| v as f32)
        .filter(|v| v.is_finite())
        .map_or(default, |v| v.clamp(min, max))
}
fn flag(map: &Map<String, Value>, key: &str, default: bool) -> bool {
    map.get(key).and_then(Value::as_bool).unwrap_or(default)
}

impl Prefs {
    pub fn from_json(value: &Value, legacy_dark: bool, legacy_interval: i32) -> Self {
        let mut p = Self {
            theme: if legacy_dark { ThemeMode::Dark } else { ThemeMode::Light },
            refresh_ms: legacy_interval.clamp(250, 5000) as u32,
            ..Self::default()
        };
        let Some(map) = value.as_object() else { return p };
        p.theme = map.get("theme").and_then(Value::as_str).and_then(ThemeMode::from_key).unwrap_or(p.theme);
        p.light = Colors::from_json(map.get("light"), p.light);
        p.dark = Colors::from_json(map.get("dark"), p.dark);
        p.contrast = number(map, "contrast", 0.0, -1.0, 1.0);
        p.row_shading = flag(map, "row_shading", false);
        p.font_family = map
            .get("font_family")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Consolas")
            .chars()
            .take(128)
            .collect();
        p.text_delta = number(map, "text_delta", 0.0, -3.0, 8.0);
        p.row_delta = number(map, "row_delta", 0.0, -3.0, 10.0);
        p.bold_headings = flag(map, "bold_headings", true);
        p.ui_scale = number(map, "ui_scale", 1.0, 0.75, 2.0);
        p.corner_radius = number(map, "corner_radius", 8.0, 0.0, 14.0);
        p.remember_positions = flag(map, "remember_positions", true);
        p.always_on_top = flag(map, "always_on_top", false);
        p.opacity = number(map, "opacity", 1.0, 0.3, 1.0);
        p.refresh_ms =
            map.get("refresh_ms").and_then(Value::as_u64).map_or(p.refresh_ms, |v| v.clamp(250, 5000) as u32);
        p.show_footer_links = flag(map, "show_footer_links", true);
        p.show_gpu_details = flag(map, "show_gpu_details", true);
        p.show_applied_summary = flag(map, "show_applied_summary", true);
        p.card_columns = map.get("card_columns").and_then(Value::as_u64).unwrap_or(3).clamp(1, 6) as usize;
        if let Some(saved) = map.get("positions").and_then(Value::as_object) {
            for (name, position) in saved.iter().take(16) {
                if let Some(xy) = position.as_array()
                    && let (Some(x), Some(y)) = (xy.first().and_then(Value::as_i64), xy.get(1).and_then(Value::as_i64))
                    && let (Ok(x), Ok(y)) = (i32::try_from(x), i32::try_from(y))
                {
                    p.positions.insert(name.clone(), (x, y));
                }
            }
        }
        if let Some(saved) = map.get("panels").and_then(Value::as_array) {
            p.panels.clear();
            for item in saved {
                if let Some(key) = item.get("key").and_then(Value::as_str)
                    && PANELS.iter().any(|(k, _)| *k == key)
                    && !p.panels.iter().any(|panel| panel.key == key)
                {
                    p.panels.push(Panel {
                        key: key.into(),
                        visible: item.get("visible").and_then(Value::as_bool).unwrap_or(true),
                    });
                }
            }
            for (key, _) in PANELS {
                if !p.panels.iter().any(|panel| panel.key == *key) {
                    p.panels.push(Panel { key: (*key).into(), visible: true });
                }
            }
        }
        if let Some(saved) = map.get("cards").and_then(Value::as_array) {
            p.cards = saved
                .iter()
                .take(24)
                .filter_map(|item| {
                    let sensor = item.get("sensor")?.as_str()?.chars().take(128).collect();
                    let label = item.get("label").and_then(Value::as_str).unwrap_or("").chars().take(96).collect();
                    Some(Card { sensor, label })
                })
                .collect();
        }
        if let Some(saved) = map.get("hidden_sensors").and_then(Value::as_array) {
            p.hidden_sensors = saved.iter().take(512).filter_map(Value::as_str).map(str::to_string).collect();
            p.hidden_sensors.sort();
            p.hidden_sensors.dedup();
        }
        p
    }

    pub fn to_json(&self) -> Value {
        json!({"theme": self.theme.key(), "light": self.light.to_json(), "dark": self.dark.to_json(),
            "contrast": self.contrast, "row_shading": self.row_shading, "font_family": self.font_family,
            "text_delta": self.text_delta, "row_delta": self.row_delta, "bold_headings": self.bold_headings,
            "ui_scale": self.ui_scale, "corner_radius": self.corner_radius, "remember_positions": self.remember_positions,
            "positions": self.positions, "always_on_top": self.always_on_top, "opacity": self.opacity,
            "refresh_ms": self.refresh_ms, "show_footer_links": self.show_footer_links,
            "show_gpu_details": self.show_gpu_details, "show_applied_summary": self.show_applied_summary,
            "panels": self.panels.iter().map(|p| json!({"key":p.key,"visible":p.visible})).collect::<Vec<_>>(),
            "cards": self.cards.iter().map(|c| json!({"sensor":c.sensor,"label":c.label})).collect::<Vec<_>>(),
            "card_columns": self.card_columns, "hidden_sensors": self.hidden_sensors})
    }
    pub fn colors(&self, dark: bool) -> &Colors {
        if dark { &self.dark } else { &self.light }
    }
    pub fn colors_mut(&mut self, dark: bool) -> &mut Colors {
        if dark { &mut self.dark } else { &mut self.light }
    }
}

pub fn apply_contrast(fg: Color32, bg: Color32, amount: f32) -> Color32 {
    let luminance = |c: Color32| 0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32;
    let target = if amount < 0.0 {
        bg
    } else if luminance(bg) > 127.5 {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
    let t = amount.abs().min(1.0) * if amount < 0.0 { 0.65 } else { 1.0 };
    let blend = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(blend(fg.r(), target.r()), blend(fg.g(), target.g()), blend(fg.b(), target.b()))
}

pub fn current(ctx: &eframe::egui::Context) -> std::sync::Arc<Prefs> {
    ctx.data(|d| d.get_temp::<std::sync::Arc<Prefs>>(eframe::egui::Id::unique("roch_gpu_ui_preferences")))
        .unwrap_or_default()
}
pub fn publish(ctx: &eframe::egui::Context, prefs: &Prefs) {
    ctx.data_mut(|d| {
        d.insert_temp(eframe::egui::Id::unique("roch_gpu_ui_preferences"), std::sync::Arc::new(prefs.clone()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renderers_share_an_immutable_preference_snapshot() {
        let ctx = eframe::egui::Context::default();
        let mut prefs = Prefs::default();
        publish(&ctx, &prefs);
        let first = current(&ctx);
        assert!(std::sync::Arc::ptr_eq(&first, &current(&ctx)));
        prefs.cards[0].label = "New label".into();
        publish(&ctx, &prefs);
        assert_ne!(first.cards[0].label, current(&ctx).cards[0].label);
        assert!(!std::sync::Arc::ptr_eq(&first, &current(&ctx)));
    }
    #[test]
    fn old_settings_keep_the_theme_and_poll_interval() {
        let p = Prefs::from_json(&Value::Null, true, 2000);
        assert_eq!(p.theme, ThemeMode::Dark);
        assert_eq!(p.refresh_ms, 2000);
        assert_eq!(p.ui_scale, 1.0);
    }
    #[test]
    fn customized_layout_and_colors_survive_restart() {
        let mut p = Prefs { theme: ThemeMode::System, opacity: 0.7, ..Prefs::default() };
        p.light.value = Color32::from_rgb(3, 29, 84);
        p.panels.swap(0, 1);
        p.panels[1].visible = false;
        p.cards[0].label = "My temperature".into();
        p.positions.insert("Roch GPU".into(), (-1800, 125));
        p.hidden_sensors = vec!["fanrpm0".into()];
        assert_eq!(Prefs::from_json(&p.to_json(), false, 1000), p);
    }
    #[test]
    fn malformed_and_extreme_preferences_are_sanitized_individually() {
        let p = Prefs::from_json(
            &json!({"ui_scale":999,"opacity":-5,"refresh_ms":u64::MAX,
            "text_delta":"bad", "light":{"value":"invalid"},"positions":{"a":[i64::MAX, 0]},
            "panels":[{"key":"profiles","visible":false},{"key":"profiles"},{"key":"foreign"}]}),
            false,
            1000,
        );
        assert_eq!(p.ui_scale, 2.0);
        assert_eq!(p.opacity, 0.3);
        assert_eq!(p.refresh_ms, 5000);
        assert_eq!(p.text_delta, 0.0);
        assert_eq!(p.light.value, Colors::DEFAULT_LIGHT.value);
        assert!(p.positions.is_empty());
        assert_eq!(p.panels.len(), 2);
        assert!(!p.panels[0].visible);
    }
    #[test]
    fn every_viewer_preset_and_theme_is_available() {
        assert_eq!(presets().len(), 7);
        assert!(ThemeMode::System.is_dark(true));
        assert!(!ThemeMode::System.is_dark(false));
        assert_eq!(parse_hex("#abcdef"), Some(Color32::from_rgb(171, 205, 239)));
    }
}
