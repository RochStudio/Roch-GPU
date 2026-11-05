//! The fonts installed on this machine, for the Settings window's font list.
//!
//! Windows records every installed font in the registry -- the machine's
//! under HKLM and the user's own under HKCU -- as "Name Style (TrueType)"
//! mapped to a file. On Linux fontconfig knows them, through `fc-list`. A
//! family is offered when it has a regular face; its bold face is used for
//! headings when it has one.

#[cfg(any(windows, test))]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One family's faces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Family {
    pub name: String,
    pub regular: PathBuf,
    pub bold: Option<PathBuf>,
    pub monospace: bool,
}

/// Families known to be fixed-pitch, listed first: the tables are laid out
/// for one, and a proportional face still works but looks less even.
const MONOSPACE: &[&str] = &[
    "Consolas",
    "Cascadia Mono",
    "Cascadia Code",
    "Courier New",
    "Lucida Console",
    "Lucida Sans Typewriter",
    "JetBrains Mono",
    "Fira Code",
    "Fira Mono",
    "Source Code Pro",
    "Hack",
    "DejaVu Sans Mono",
    "Roboto Mono",
    "Ubuntu Mono",
    "Inconsolata",
    "IBM Plex Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "SimSun-ExtB",
    "MS Gothic",
];

/// What the Settings window calls the font used when none has been chosen.
pub const DEFAULT_FONT_LABEL: &str = if cfg!(windows) { "Consolas (default)" } else { "System monospace (default)" };

#[cfg(any(windows, test))]
const STYLE_WORDS: &[&str] = &[
    "Regular",
    "Bold",
    "Italic",
    "Oblique",
    "Light",
    "Semilight",
    "SemiLight",
    "Semibold",
    "SemiBold",
    "Medium",
    "Black",
    "Thin",
    "ExtraLight",
    "Extra Light",
    "ExtraBold",
    "Extra Bold",
    "Heavy",
    "Condensed",
    "Narrow",
];

/// "Consolas Bold (TrueType)" -> ("Consolas", "Bold").
#[cfg(any(windows, test))]
fn split_name(entry: &str) -> (String, String) {
    let base = entry.split(" (").next().unwrap_or(entry).trim();
    let mut family: Vec<&str> = base.split(' ').collect();
    let mut style: Vec<&str> = Vec::new();
    while let Some(last) = family.last() {
        if family.len() > 1 && STYLE_WORDS.iter().any(|w| w.eq_ignore_ascii_case(last)) {
            style.insert(0, family.pop().unwrap());
        } else {
            break;
        }
    }
    (family.join(" "), style.join(" "))
}

#[cfg(any(windows, test))]
fn font_path(value: &str, fonts_dir: &Path) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() { path } else { fonts_dir.join(path) }
}

fn is_font_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("ttf") | Some("otf") | Some("ttc")
    )
}

/// Group registry entries into families. Pure, so it can be tested.
#[cfg(any(windows, test))]
pub fn families_from_entries(entries: &[(String, String)], fonts_dir: &Path) -> Vec<Family> {
    let mut regular: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut bold: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (name, value) in entries {
        // Collections ("Cambria & Cambria Math") name several families in
        // one file; the first is the one the file opens as.
        let name = name.split(" & ").next().unwrap_or(name);
        let path = font_path(value, fonts_dir);
        if !is_font_file(&path) {
            continue;
        }
        let (family, style) = split_name(name);
        if family.is_empty() {
            continue;
        }
        match style.as_str() {
            "" | "Regular" => {
                regular.entry(family).or_insert(path);
            }
            "Bold" => {
                bold.entry(family).or_insert(path);
            }
            _ => {}
        }
    }
    let mut families: Vec<Family> = regular
        .into_iter()
        .map(|(name, path)| Family {
            monospace: MONOSPACE.iter().any(|m| m.eq_ignore_ascii_case(&name)),
            bold: bold.get(&name).cloned(),
            regular: path,
            name,
        })
        .collect();
    families
        .sort_by(|a, b| b.monospace.cmp(&a.monospace).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    families
}

#[cfg(windows)]
fn read_values(root: winreg::RegKey, fonts_dir: &Path) -> Vec<(String, String)> {
    let Ok(key) =
        root.open_subkey_with_flags(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts", winreg::enums::KEY_READ)
    else {
        return Vec::new();
    };
    key.enum_values()
        .filter_map(Result::ok)
        .filter_map(|(name, _)| {
            let value: String = key.get_value(&name).ok()?;
            Some((name, font_path(&value, fonts_dir).to_string_lossy().into_owned()))
        })
        .collect()
}

#[cfg(windows)]
pub fn installed_families() -> Vec<Family> {
    let system_dir = PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| r"C:\Windows".into())).join("Fonts");
    let user_dir = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join(r"Microsoft\Windows\Fonts");
    let mut entries = read_values(winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE), &system_dir);
    entries.extend(read_values(winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER), &user_dir));
    families_from_entries(&entries, &system_dir)
}

/// Group `fc-list --format '%{family[0]}\t%{style[0]}\t%{spacing}\t%{file}\n'` into families. Pure,
/// so it can be tested. A file can answer to several names; the first is the one to show.
#[cfg(any(not(windows), test))]
pub fn families_from_fc_list(listing: &str) -> Vec<Family> {
    use std::collections::BTreeMap as Map;
    let mut found: Map<String, (Option<PathBuf>, Option<PathBuf>, bool)> = Map::new();
    for line in listing.lines() {
        let mut fields = line.split('\t');
        let (Some(family), Some(style), Some(spacing), Some(file)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let path = PathBuf::from(file);
        if family.is_empty() || !is_font_file(&path) {
            continue;
        }
        let entry = found.entry(family.to_string()).or_default();
        // fontconfig's spacing: 90 is dual width, 100 mono, 110 charcell. Proportional prints nothing.
        entry.2 |= spacing.parse::<i32>().is_ok_and(|s| s >= 90);
        if style.eq_ignore_ascii_case("Regular") || style.eq_ignore_ascii_case("Book") {
            entry.0.get_or_insert(path);
        } else if style.eq_ignore_ascii_case("Bold") {
            entry.1.get_or_insert(path);
        }
    }
    let mut families: Vec<Family> = found
        .into_iter()
        .filter_map(|(name, (regular, bold, fixed))| {
            let monospace = fixed || MONOSPACE.iter().any(|m| m.eq_ignore_ascii_case(&name));
            Some(Family { regular: regular?, bold, monospace, name })
        })
        .collect();
    families
        .sort_by(|a, b| b.monospace.cmp(&a.monospace).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    families
}

#[cfg(not(windows))]
pub fn installed_families() -> Vec<Family> {
    let listing = std::process::Command::new("fc-list")
        .args(["--format", "%{family[0]}\t%{style[0]}\t%{spacing}\t%{file}\n"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    families_from_fc_list(&listing)
}

/// The system's own faces, by fontconfig's reckoning: its monospace, regular and bold, and whichever
/// font best covers the few symbols a monospace font tends to lack (◀ ▶ ☀ →). `None` for anything
/// fontconfig cannot answer, in which case egui's built-in fonts are what is left.
#[cfg(not(windows))]
pub fn system_faces() -> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
    let matched = |pattern: &str| -> Option<PathBuf> {
        let out = std::process::Command::new("fc-match").args(["--format", "%{file}", pattern]).output().ok()?;
        let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        (out.status.success() && is_font_file(&path) && path.is_file()).then_some(path)
    };
    (matched("monospace:regular"), matched("monospace:bold"), matched(":charset=25c0 25b6 2600 2192"))
}

pub fn find_family(name: &str) -> Option<Family> {
    installed_families().into_iter().find(|f| f.name.eq_ignore_ascii_case(name) && f.regular.is_file())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_split_into_family_and_style() {
        assert_eq!(split_name("Consolas Bold (TrueType)"), ("Consolas".into(), "Bold".into()));
        assert_eq!(split_name("Cascadia Code SemiBold (TrueType)"), ("Cascadia Code".into(), "SemiBold".into()));
        assert_eq!(split_name("Arial (TrueType)"), ("Arial".into(), String::new()));
        assert_eq!(split_name("Black (TrueType)"), ("Black".into(), String::new()));
    }

    #[test]
    fn families_pair_regular_and_bold_and_put_monospace_first() {
        let dir = Path::new("C:\\Windows\\Fonts");
        let entries = vec![
            ("Arial (TrueType)".to_string(), "arial.ttf".to_string()),
            ("Consolas (TrueType)".to_string(), "consola.ttf".to_string()),
            ("Consolas Bold (TrueType)".to_string(), "consolab.ttf".to_string()),
            ("Consolas Italic (TrueType)".to_string(), "consolai.ttf".to_string()),
            ("Some Bitmap Font".to_string(), "font.fon".to_string()),
            ("My Font (TrueType)".to_string(), "C:\\Users\\x\\my.otf".to_string()),
        ];
        let families = families_from_entries(&entries, dir);
        let names: Vec<&str> = families.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Consolas", "Arial", "My Font"]);
        assert_eq!(families[0].bold.as_deref(), Some(dir.join("consolab.ttf").as_path()));
        #[cfg(windows)]
        assert_eq!(families[2].regular, PathBuf::from("C:\\Users\\x\\my.otf"));
    }

    #[test]
    fn fc_list_lines_become_families() {
        let listing = "Arial\tRegular\t\t/f/arial.ttf\n\
                       JetBrains Mono\tBold\t100\t/f/jb-bold.ttf\n\
                       JetBrains Mono\tRegular\t100\t/f/jb.ttf\n\
                       JetBrains Mono\tItalic\t100\t/f/jb-italic.ttf\n\
                       Hack\tRegular\t\t/f/hack.otf\n\
                       Only Italic\tItalic\t\t/f/oi.ttf\n\
                       Bitmap\tRegular\t\t/f/bitmap.pcf.gz\n\
                       \tRegular\t\t/f/nameless.ttf\n\
                       not a font line\n";
        let families = families_from_fc_list(listing);
        let names: Vec<&str> = families.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Hack", "JetBrains Mono", "Arial"], "fixed-pitch first, then by name");
        assert!(families[0].monospace, "by name, for a font that does not declare it");
        assert!(families[1].monospace, "by its spacing");
        assert_eq!(families[1].regular, PathBuf::from("/f/jb.ttf"));
        assert_eq!(families[1].bold.as_deref(), Some(Path::new("/f/jb-bold.ttf")));
        assert!(!families[2].monospace && families[2].bold.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn this_machine_has_consolas() {
        // Every supported Windows ships it.
        assert!(installed_families().iter().any(|f| f.name == "Consolas" && f.monospace));
    }
}
