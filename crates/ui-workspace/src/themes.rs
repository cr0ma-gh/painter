//! Colour themes: a palette of named UI colours, saved as JSON with
//! `"#rrggbb"` / `"#rrggbbaa"` strings. A theme file needs only the colours
//! it changes; the rest come from the preset it names as `base` (dark by
//! default). The theme editor shows contrast warnings for text that would
//! be hard to read.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An sRGB colour with alpha, written as hex in JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b, 255])
    }

    pub const fn gray(v: u8) -> Self {
        Self([v, v, v, 255])
    }

    pub fn to_hex(self) -> String {
        let [r, g, b, a] = self.0;
        if a == 255 {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        }
    }

    /// `#rgb`, `#rrggbb` or `#rrggbbaa` (the `#` is optional).
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('#');
        let byte = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok();
        match s.len() {
            3 => {
                let mut out = [255u8; 4];
                for (k, c) in s.chars().enumerate() {
                    let v = c.to_digit(16)? as u8;
                    out[k] = v * 17;
                }
                Some(Self(out))
            }
            6 => Some(Self([byte(0)?, byte(2)?, byte(4)?, 255])),
            8 => Some(Self([byte(0)?, byte(2)?, byte(4)?, byte(6)?])),
            _ => None,
        }
    }

    /// WCAG relative luminance.
    pub fn luminance(self) -> f32 {
        let lin = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let [r, g, b, _] = self.0;
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }

    /// WCAG contrast ratio, 1..21.
    pub fn contrast(self, other: Rgba) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
}

impl Serialize for Rgba {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgba::from_hex(&s).ok_or_else(|| serde::de::Error::custom(format!("not a colour: {s:?}")))
    }
}

/// Every colour the UI draws with.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThemeColors {
    /// Gaps between areas and behind the dock.
    pub backdrop: Rgba,
    /// Panel bodies.
    pub panel: Rgba,
    /// Panel title bars and inactive tabs.
    pub title: Rgba,
    /// Lists, wells and slider tracks.
    pub well: Rgba,
    /// Separators and outlines.
    pub line: Rgba,
    pub text: Rgba,
    pub text_dim: Rgba,
    /// Selected rows and pressed buttons.
    pub selected: Rgba,
    /// Slider fills, focus and the current tab.
    pub accent: Rgba,
    /// Around the canvas in the viewport.
    pub canvas_backdrop: Rgba,
    /// Thin outline at the canvas edge.
    pub canvas_border: Rgba,
    /// Selection outlines (marching ants' dark phase).
    pub selection: Rgba,
    /// Transform and frame handles.
    pub handles: Rgba,
    /// 3D wireframes and guides.
    pub wireframe: Rgba,
    pub warning: Rgba,
}

impl ThemeColors {
    /// Every colour with its name, for the theme editor.
    pub fn fields_mut(&mut self) -> [(&'static str, &mut Rgba); 15] {
        [
            ("Backdrop", &mut self.backdrop),
            ("Panel", &mut self.panel),
            ("Title bars", &mut self.title),
            ("Wells", &mut self.well),
            ("Lines", &mut self.line),
            ("Text", &mut self.text),
            ("Dim text", &mut self.text_dim),
            ("Selected", &mut self.selected),
            ("Accent", &mut self.accent),
            ("Canvas backdrop", &mut self.canvas_backdrop),
            ("Canvas border", &mut self.canvas_border),
            ("Selection", &mut self.selection),
            ("Handles", &mut self.handles),
            ("Wireframe", &mut self.wireframe),
            ("Warning", &mut self.warning),
        ]
    }

    /// The current look (see the app's `theme.rs`).
    pub const DARK: ThemeColors = ThemeColors {
        backdrop: Rgba::rgb(29, 30, 33),
        panel: Rgba::rgb(42, 43, 47),
        title: Rgba::rgb(49, 50, 55),
        well: Rgba::rgb(33, 34, 38),
        line: Rgba::rgb(24, 25, 28),
        text: Rgba::rgb(222, 224, 228),
        text_dim: Rgba::rgb(146, 150, 158),
        selected: Rgba::rgb(52, 76, 122),
        accent: Rgba::rgb(84, 142, 255),
        canvas_backdrop: Rgba::rgb(37, 38, 42),
        canvas_border: Rgba::gray(18),
        selection: Rgba::gray(0),
        handles: Rgba::rgb(255, 170, 40),
        wireframe: Rgba::rgb(240, 150, 60),
        warning: Rgba::rgb(240, 120, 100),
    };

    pub const LIGHT: ThemeColors = ThemeColors {
        backdrop: Rgba::rgb(204, 206, 211),
        panel: Rgba::rgb(238, 239, 242),
        title: Rgba::rgb(226, 228, 233),
        well: Rgba::rgb(250, 250, 252),
        line: Rgba::rgb(194, 197, 204),
        text: Rgba::rgb(30, 32, 36),
        text_dim: Rgba::rgb(98, 102, 112),
        selected: Rgba::rgb(198, 215, 246),
        accent: Rgba::rgb(38, 102, 228),
        canvas_backdrop: Rgba::rgb(184, 187, 193),
        canvas_border: Rgba::gray(120),
        selection: Rgba::gray(0),
        handles: Rgba::rgb(220, 120, 0),
        wireframe: Rgba::rgb(210, 110, 20),
        warning: Rgba::rgb(190, 50, 30),
    };

    /// For low vision: pure black and white, a strong yellow accent.
    pub const HIGH_CONTRAST: ThemeColors = ThemeColors {
        backdrop: Rgba::gray(0),
        panel: Rgba::gray(12),
        title: Rgba::gray(28),
        well: Rgba::gray(0),
        line: Rgba::gray(200),
        text: Rgba::gray(255),
        text_dim: Rgba::gray(210),
        selected: Rgba::rgb(0, 70, 160),
        accent: Rgba::rgb(255, 214, 0),
        canvas_backdrop: Rgba::gray(0),
        canvas_border: Rgba::gray(255),
        selection: Rgba::gray(0),
        handles: Rgba::rgb(255, 214, 0),
        wireframe: Rgba::rgb(0, 230, 255),
        warning: Rgba::rgb(255, 90, 90),
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeBase {
    #[default]
    Dark,
    Light,
    HighContrast,
}

impl ThemeBase {
    pub fn colors(self) -> ThemeColors {
        match self {
            ThemeBase::Dark => ThemeColors::DARK,
            ThemeBase::Light => ThemeColors::LIGHT,
            ThemeBase::HighContrast => ThemeColors::HIGH_CONTRAST,
        }
    }

    pub fn is_dark(self) -> bool {
        self != ThemeBase::Light
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Theme {
    pub name: String,
    /// Preset the colours start from; also picks egui's dark or light widgets.
    pub base: ThemeBase,
    pub colors: ThemeColors,
    /// Body text size in points.
    pub font_size: f32,
    /// Corner rounding of widgets and windows.
    pub rounding: u8,
}

impl Default for Theme {
    fn default() -> Self {
        Theme::preset(ThemeBase::Dark)
    }
}

impl Theme {
    pub fn preset(base: ThemeBase) -> Self {
        let name = match base {
            ThemeBase::Dark => "Dark",
            ThemeBase::Light => "Light",
            ThemeBase::HighContrast => "High Contrast",
        };
        Self { name: name.into(), base, colors: base.colors(), font_size: 12.5, rounding: 5 }
    }

    pub fn presets() -> Vec<Theme> {
        [ThemeBase::Dark, ThemeBase::Light, ThemeBase::HighContrast].into_iter().map(Theme::preset).collect()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Reads a theme file. Missing colours and settings come from `base`.
    pub fn from_json(json: &str) -> Result<Theme, String> {
        let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let base: ThemeBase = match value.get("base") {
            Some(b) => serde_json::from_value(b.clone()).map_err(|e| format!("base: {e}"))?,
            None => ThemeBase::Dark,
        };
        let mut theme = Theme::preset(base);
        if let Some(name) = value.get("name").and_then(|n| n.as_str()) {
            theme.name = name.to_string();
        }
        if let Some(size) = value.get("font_size").and_then(|v| v.as_f64()) {
            theme.font_size = (size as f32).clamp(8.0, 24.0);
        }
        if let Some(r) = value.get("rounding").and_then(|v| v.as_u64()) {
            theme.rounding = r.min(16) as u8;
        }
        if let Some(colors) = value.get("colors").and_then(|c| c.as_object()) {
            // Merge over the base: write the base out, overlay, read back.
            let mut merged = serde_json::to_value(theme.colors).map_err(|e| e.to_string())?;
            let map = merged.as_object_mut().expect("colours are an object");
            for (key, v) in colors {
                if !map.contains_key(key) {
                    return Err(format!("unknown colour {key:?}"));
                }
                map.insert(key.clone(), v.clone());
            }
            theme.colors = serde_json::from_value(merged).map_err(|e| e.to_string())?;
        }
        Ok(theme)
    }

    /// Text / background pairs below WCAG AA for normal text (4.5:1), or
    /// 3:1 for dimmed text; shown in the theme editor.
    pub fn contrast_warnings(&self) -> Vec<String> {
        let c = &self.colors;
        let pairs = [
            ("text on panel", c.text, c.panel, 4.5),
            ("text on title bars", c.text, c.title, 4.5),
            ("text on selected rows", c.text, c.selected, 4.5),
            ("text in wells", c.text, c.well, 4.5),
            ("dim text on panel", c.text_dim, c.panel, 3.0),
            ("accent on panel", c.accent, c.panel, 3.0),
        ];
        pairs
            .into_iter()
            .filter_map(|(what, fg, bg, min)| {
                let ratio = fg.contrast(bg);
                (ratio < min).then(|| format!("{what}: contrast {ratio:.1}:1, below {min}:1"))
            })
            .collect()
    }
}

/// How many built-in presets open the list.
const PRESETS: usize = 3;

/// The themes on offer (presets plus the user's) and which one is in use.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeLibrary {
    pub themes: Vec<Theme>,
    pub active: usize,
    /// Bumped on every change, so the app re-applies styles only when needed.
    pub revision: u64,
    /// The system's light or dark mode picks the theme (on by default).
    pub follow_system: bool,
    /// The themes for the system's dark and light modes, by name.
    pub dark_theme: String,
    pub light_theme: String,
    /// The system's mode last seen (`Some(true)`: dark): the theme follows only its changes.
    pub seen_system: Option<bool>,
}

impl Default for ThemeLibrary {
    fn default() -> Self {
        Self {
            themes: Theme::presets(),
            active: 0,
            revision: 0,
            follow_system: true,
            dark_theme: "Dark".into(),
            light_theme: "Light".into(),
            seen_system: None,
        }
    }
}

impl ThemeLibrary {
    pub fn current(&self) -> &Theme {
        &self.themes[self.active.min(self.themes.len() - 1)]
    }

    /// Edits the current theme; built-in presets are copied first, so they
    /// stay available to go back to.
    pub fn edit(&mut self) -> &mut Theme {
        if self.active < PRESETS && self.themes[self.active] == Theme::preset(self.themes[self.active].base) {
            let mut copy = self.themes[self.active].clone();
            let custom = format!("{} (custom)", copy.name);
            let original = std::mem::replace(&mut copy.name, custom);
            // The copy takes the original's place for its mode.
            for slot in [&mut self.dark_theme, &mut self.light_theme] {
                if *slot == original {
                    *slot = copy.name.clone();
                }
            }
            self.themes.push(copy);
            self.active = self.themes.len() - 1;
        }
        self.revision += 1;
        let i = self.active;
        &mut self.themes[i]
    }

    pub fn select(&mut self, index: usize) {
        if index < self.themes.len() && index != self.active {
            self.active = index;
            self.revision += 1;
        }
    }

    /// A theme picked by hand: it becomes the one for its kind of mode (a dark
    /// theme for dark mode, a light one for light mode).
    pub fn choose(&mut self, index: usize) {
        let Some(theme) = self.themes.get(index) else { return };
        if theme.base.is_dark() {
            self.dark_theme = theme.name.clone();
        } else {
            self.light_theme = theme.name.clone();
        }
        self.select(index);
    }

    /// Following the system: its mode's theme, if it isn't the one in use
    /// already. Returns whether the theme changed.
    pub fn follow(&mut self, system_dark: bool) -> bool {
        // Only when the system's mode changes (or is first seen), so a theme
        // picked or edited in between stays.
        if !self.follow_system || self.seen_system == Some(system_dark) {
            return false;
        }
        self.seen_system = Some(system_dark);
        let (name, fallback) = if system_dark { (&self.dark_theme, ThemeBase::Dark) } else { (&self.light_theme, ThemeBase::Light) };
        let wanted = self
            .themes
            .iter()
            .position(|t| t.name == *name)
            .or_else(|| self.themes.iter().position(|t| t.base == fallback))
            .unwrap_or(0);
        let before = self.revision;
        self.select(wanted);
        self.revision != before
    }

    /// The user's themes (presets aren't stored), the active one's name and
    /// how the system's mode picks one.
    pub fn to_json(&self) -> String {
        let custom: Vec<serde_json::Value> =
            self.themes.iter().skip(PRESETS).filter_map(|t| serde_json::to_value(t).ok()).collect();
        serde_json::json!({
            "active": self.current().name,
            "themes": custom,
            "follow_system": self.follow_system,
            "dark_theme": self.dark_theme,
            "light_theme": self.light_theme,
        })
        .to_string()
    }

    /// Restores [`Self::to_json`]; themes that don't read are skipped.
    pub fn from_json(json: &str) -> Self {
        let mut lib = ThemeLibrary::default();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else { return lib };
        for t in value.get("themes").and_then(|t| t.as_array()).into_iter().flatten() {
            if let Ok(theme) = Theme::from_json(&t.to_string()) {
                lib.themes.push(theme);
            }
        }
        if let Some(name) = value.get("active").and_then(|a| a.as_str()) {
            lib.active = lib.themes.iter().position(|t| t.name == name).unwrap_or(0);
        }
        match value.get("follow_system").and_then(|v| v.as_bool()) {
            Some(follow) => {
                lib.follow_system = follow;
                for (key, slot) in [("dark_theme", &mut lib.dark_theme), ("light_theme", &mut lib.light_theme)] {
                    if let Some(name) = value.get(key).and_then(|v| v.as_str()) {
                        *slot = name.to_string();
                    }
                }
            }
            // Settings from before this option: the theme in use stays the one for its mode.
            None => lib.choose(lib.active),
        }
        lib
    }

    /// Adds (or replaces, by name) a theme loaded from JSON and selects it.
    pub fn import(&mut self, json: &str) -> Result<usize, String> {
        let theme = Theme::from_json(json)?;
        let index = match self.themes.iter().position(|t| t.name == theme.name) {
            Some(i) if i >= PRESETS => {
                self.themes[i] = theme;
                i
            }
            _ => {
                self.themes.push(theme);
                self.themes.len() - 1
            }
        };
        self.active = index;
        self.revision += 1;
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        assert_eq!(Rgba::from_hex("#62c4"), None);
        assert_eq!(Rgba::from_hex("#fa0"), Some(Rgba([255, 170, 0, 255])));
        assert_eq!(Rgba::from_hex("6284c4").unwrap().to_hex(), "#6284c4");
        assert_eq!(Rgba::from_hex("#00000080").unwrap().0[3], 128);
        assert_eq!(Rgba([1, 2, 3, 4]).to_hex(), "#01020304");
        assert!((Rgba::gray(0).contrast(Rgba::gray(255)) - 21.0).abs() < 0.01);
    }

    #[test]
    fn the_systems_mode_picks_the_theme() {
        let mut lib = ThemeLibrary::default();
        assert!(lib.follow_system, "on by default");
        assert!(lib.follow(false));
        assert_eq!(lib.current().name, "Light");
        assert!(!lib.follow(false), "already there");
        assert!(lib.follow(true));
        assert_eq!(lib.current().name, "Dark");
        // Picking High Contrast by hand makes it the dark mode's theme.
        lib.choose(2);
        lib.follow(false);
        lib.follow(true);
        assert_eq!(lib.current().name, "High Contrast");
        // Off: the system's mode changes nothing.
        lib.follow_system = false;
        assert!(!lib.follow(false));
        // Saved and read back.
        let back = ThemeLibrary::from_json(&lib.to_json());
        assert_eq!((back.follow_system, back.dark_theme.as_str(), back.light_theme.as_str()), (false, "High Contrast", "Light"));
        // Settings from before: following, with the theme in use kept for its mode.
        let old = ThemeLibrary::from_json(r#"{"active":"High Contrast","themes":[]}"#);
        assert!(old.follow_system);
        assert_eq!(old.dark_theme, "High Contrast");
    }

    #[test]
    fn partial_theme_files_merge_over_their_base() {
        let json = r##"{ "name": "Sepia", "base": "light", "colors": { "panel": "#efe4cf", "accent": "#8a4b14" }, "font_size": 14 }"##;
        let t = Theme::from_json(json).unwrap();
        assert_eq!(t.name, "Sepia");
        assert_eq!(t.base, ThemeBase::Light);
        assert_eq!(t.colors.panel, Rgba::rgb(0xef, 0xe4, 0xcf));
        assert_eq!(t.colors.text, ThemeColors::LIGHT.text, "untouched colours come from the base");
        assert_eq!(t.font_size, 14.0);
        assert!(Theme::from_json(r##"{ "colors": { "nope": "#000" } }"##).is_err());
        assert!(Theme::from_json(r#"{ "colors": { "panel": "red" } }"#).is_err());
        // Full round trip.
        for preset in Theme::presets() {
            assert_eq!(Theme::from_json(&preset.to_json()).unwrap(), preset);
        }
    }

    #[test]
    fn presets_are_readable_and_bad_themes_warn() {
        for preset in Theme::presets() {
            assert!(preset.contrast_warnings().is_empty(), "{}: {:?}", preset.name, preset.contrast_warnings());
        }
        let mut lib = ThemeLibrary::default();
        lib.edit().colors.text = Rgba::gray(70);
        assert_eq!(lib.themes.len(), 4, "the preset was copied before editing");
        assert_eq!(lib.current().name, "Dark (custom)");
        assert!(!lib.current().contrast_warnings().is_empty());
        let i = lib.import(r#"{ "name": "Mine", "base": "high_contrast" }"#).unwrap();
        assert_eq!(lib.active, i);
        assert!(lib.revision >= 2);
        let back = ThemeLibrary::from_json(&lib.to_json());
        assert_eq!(back.themes, lib.themes);
        assert_eq!(back.active, lib.active);
        assert_eq!(ThemeLibrary::from_json("garbage"), ThemeLibrary::default());
    }
}
