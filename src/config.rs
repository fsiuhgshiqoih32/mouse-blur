//! Settings, stored as TOML in the user's config directory and reloaded live when edited.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::composer::Tint;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    /// The cursor's own colours.
    Natural,
    /// The trail in one colour (see `color`).
    Tint,
    /// A hue sweep along the trail.
    Rainbow,
}

impl Style {
    pub fn name(self) -> &'static str {
        match self {
            Style::Natural => "natural",
            Style::Tint => "tint",
            Style::Rainbow => "rainbow",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "natural" => Some(Style::Natural),
            "tint" | "tinted" | "color" | "colour" => Some(Style::Tint),
            "rainbow" => Some(Style::Rainbow),
            _ => None,
        }
    }
}

/// Lag compensation: automatic (from the display refresh rate) or a fixed number of milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lag {
    Auto,
    Ms(u32),
}

impl Lag {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("auto") {
            Some(Lag::Auto)
        } else {
            s.trim_end_matches("ms").trim().parse().ok().map(|v: u32| Lag::Ms(v.min(MAX_LAG_MS)))
        }
    }
}

impl fmt::Display for Lag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Lag::Auto => f.write_str("auto"),
            Lag::Ms(v) => write!(f, "{v}"),
        }
    }
}

impl Serialize for Lag {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Lag::Auto => s.serialize_str("auto"),
            Lag::Ms(v) => s.serialize_u32(*v),
        }
    }
}

impl<'de> Deserialize<'de> for Lag {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Num(i64),
            Str(String),
        }
        match Raw::deserialize(d)? {
            Raw::Num(n) if n < 0 => Ok(Lag::Auto),
            Raw::Num(n) => Ok(Lag::Ms((n as u32).min(MAX_LAG_MS))),
            Raw::Str(s) => Lag::parse(&s).ok_or_else(|| serde::de::Error::custom("expected \"auto\" or milliseconds")),
        }
    }
}

pub const MAX_LAG_MS: u32 = 60;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// Percent. 100 is a physically accurate exposure.
    pub intensity: u32,
    /// Shutter time in milliseconds.
    pub blur_ms: u32,
    /// Percent of fade towards the tail.
    pub taper: u32,
    pub lag_ms: Lag,
    /// Pointer speed in px/s below which there is no smear.
    pub min_speed: u32,
    pub style: Style,
    /// Trail colour for `style = "tint"`, as `#RRGGBB`.
    pub color: String,
    /// Global toggle shortcut, e.g. "Ctrl+Alt+B". Empty disables it.
    pub hotkey: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: true,
            intensity: 300,
            blur_ms: 50,
            taper: 50,
            lag_ms: Lag::Auto,
            min_speed: 150,
            style: Style::Natural,
            color: "#38B6FF".into(),
            hotkey: "Ctrl+Alt+B".into(),
        }
    }
}

pub const INTENSITY_PRESETS: [(u32, &str); 5] =
    [(100, "Realistic"), (200, "Subtle"), (300, "Normal"), (450, "Strong"), (600, "Maximum")];
pub const BLUR_PRESETS: [(u32, &str); 5] =
    [(25, "Short"), (50, "Medium"), (80, "Long"), (120, "Very long"), (200, "Extreme")];
pub const TAPER_PRESETS: [(u32, &str); 4] = [(0, "None"), (25, "Light"), (50, "Medium"), (100, "Full")];
pub const LAG_PRESETS: [Lag; 7] =
    [Lag::Auto, Lag::Ms(0), Lag::Ms(5), Lag::Ms(10), Lag::Ms(15), Lag::Ms(20), Lag::Ms(30)];
pub const STYLE_PRESETS: [(Style, &str); 3] =
    [(Style::Natural, "Natural"), (Style::Tint, "Tinted"), (Style::Rainbow, "Rainbow")];

impl Settings {
    /// Clamps every value into its supported range.
    pub fn sanitized(mut self) -> Self {
        self.intensity = self.intensity.clamp(10, 1000);
        self.blur_ms = self.blur_ms.clamp(5, 300);
        self.taper = self.taper.min(100);
        self.min_speed = self.min_speed.min(5000);
        if parse_color(&self.color).is_none() {
            self.color = Settings::default().color;
        }
        self
    }

    pub fn tint(&self, phase: f32) -> Tint {
        match self.style {
            Style::Natural => Tint::Natural,
            Style::Tint => Tint::Solid(parse_color(&self.color).unwrap_or([0x38, 0xB6, 0xFF])),
            Style::Rainbow => Tint::Rainbow { phase },
        }
    }

    /// The config file contents, with comments explaining each key.
    pub fn to_toml_string(&self) -> String {
        let q = |s: &str| toml::Value::String(s.to_string()).to_string();
        format!(
            "# Mouse Blur settings. Changes are picked up automatically while it runs.\n\
             \n\
             # Turn the effect on or off (the hotkey and tray menu do the same).\n\
             enabled = {}\n\
             \n\
             # How visible the smear is, in percent. 100 is a physically accurate exposure.\n\
             intensity = {}\n\
             \n\
             # Shutter time in milliseconds: how far back in time the smear reaches.\n\
             blur_ms = {}\n\
             \n\
             # How much the smear fades towards its tail, in percent.\n\
             taper = {}\n\
             \n\
             # Lag compensation in milliseconds, or \"auto\" (1.5 display frames). The real pointer is\n\
             # drawn a frame or two before any app can draw, so the smear is pushed ahead to meet it.\n\
             lag_ms = {}\n\
             \n\
             # Pointer speed (pixels per second) below which there is no smear.\n\
             min_speed = {}\n\
             \n\
             # \"natural\" (the cursor's own colours), \"tint\" (one colour, see below) or \"rainbow\".\n\
             style = {}\n\
             color = {}\n\
             \n\
             # Global shortcut that toggles the effect, e.g. \"Ctrl+Alt+B\" or \"Super+Shift+M\".\n\
             # Leave empty to disable.\n\
             hotkey = {}\n",
            self.enabled,
            self.intensity,
            self.blur_ms,
            self.taper,
            match self.lag_ms {
                Lag::Auto => q("auto"),
                Lag::Ms(v) => v.to_string(),
            },
            self.min_speed,
            q(self.style.name()),
            q(&self.color),
            q(&self.hotkey),
        )
    }
}

/// Parses `#RRGGBB` or `RRGGBB`.
pub fn parse_color(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.is_ascii() {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("mouse-blur")
}

pub fn default_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn load(path: &Path) -> Result<Settings, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str::<Settings>(&text).map(Settings::sanitized).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn save(path: &Path, s: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write then rename, so a reader never sees a half-written file.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, s.to_toml_string())?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let s = Settings { lag_ms: Lag::Ms(12), style: Style::Rainbow, ..Settings::default() };
        let back: Settings = toml::from_str(&s.to_toml_string()).unwrap();
        assert_eq!(back, s);
        let back: Settings = toml::from_str(&Settings::default().to_toml_string()).unwrap();
        assert_eq!(back, Settings::default());
    }

    #[test]
    fn partial_files_use_defaults() {
        let s: Settings = toml::from_str("intensity = 450\nlag_ms = -1").unwrap();
        assert_eq!(s.intensity, 450);
        assert_eq!(s.lag_ms, Lag::Auto);
        assert_eq!(s.blur_ms, 50);
    }

    #[test]
    fn sanitizes_out_of_range_values() {
        let s = Settings { intensity: 99_999, color: "nope".into(), ..Settings::default() }.sanitized();
        assert_eq!(s.intensity, 1000);
        assert_eq!(s.color, Settings::default().color);
    }

    #[test]
    fn colors_and_lag_parse() {
        assert_eq!(parse_color("#38b6ff"), Some([0x38, 0xB6, 0xFF]));
        assert_eq!(parse_color("38B6FF"), Some([0x38, 0xB6, 0xFF]));
        assert_eq!(parse_color("#38b6f"), None);
        assert_eq!(Lag::parse("auto"), Some(Lag::Auto));
        assert_eq!(Lag::parse("12ms"), Some(Lag::Ms(12)));
    }
}
