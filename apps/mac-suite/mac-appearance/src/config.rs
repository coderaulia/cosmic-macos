// SPDX-License-Identifier: GPL-3.0-only

//! `~/.config/mac-suite/appearance.conf` and `location.conf` (key=value files).

use std::{
    fs,
    path::{Path, PathBuf},
};

/// Minutes after midnight for the defaults (07:00 and 19:00).
pub const DEFAULT_LIGHT_AT: u32 = 7 * 60;
pub const DEFAULT_DARK_AT: u32 = 19 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    Light,
    Dark,
    #[default]
    Auto,
    Schedule,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Light, Mode::Dark, Mode::Auto, Mode::Schedule];

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Light => "light",
            Mode::Dark => "dark",
            Mode::Auto => "auto",
            Mode::Schedule => "schedule",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Light => "Light",
            Mode::Dark => "Dark",
            Mode::Auto => "Automatic (sunrise to sunset)",
            Mode::Schedule => "Custom schedule",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WallpaperMode {
    #[default]
    None,
    Pair,
    Dynamic,
}

impl WallpaperMode {
    pub const ALL: [WallpaperMode; 3] =
        [WallpaperMode::None, WallpaperMode::Pair, WallpaperMode::Dynamic];

    pub fn as_str(self) -> &'static str {
        match self {
            WallpaperMode::None => "none",
            WallpaperMode::Pair => "pair",
            WallpaperMode::Dynamic => "dynamic",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WallpaperMode::None => "Leave wallpaper alone",
            WallpaperMode::Pair => "Light and dark picture",
            WallpaperMode::Dynamic => "Dynamic (dawn, day, dusk, night)",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub mode: Mode,
    /// Minutes after midnight, used by `Mode::Schedule`.
    pub light_at: u32,
    pub dark_at: u32,
    pub wallpaper: WallpaperMode,
    pub wallpaper_light: String,
    pub wallpaper_dark: String,
    pub dynamic_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Auto,
            light_at: DEFAULT_LIGHT_AT,
            dark_at: DEFAULT_DARK_AT,
            wallpaper: WallpaperMode::None,
            wallpaper_light: String::new(),
            wallpaper_dark: String::new(),
            dynamic_dir: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Location {
    pub city: String,
    pub lat: f64,
    pub lon: f64,
}

/// Parse `HH:MM` into minutes after midnight.
pub fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m) = (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

pub fn format_hhmm(minutes: u32) -> String {
    format!("{:02}:{:02}", minutes / 60 % 24, minutes % 60)
}

/// Split `key=value` lines, skipping blanks and `#` comments.
fn pairs(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (k, v) = line.split_once('=')?;
        Some((k.trim(), v.trim()))
    })
}

impl Settings {
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for (k, v) in pairs(text) {
            match k {
                "mode" => s.mode = Mode::parse(v).unwrap_or(s.mode),
                "light_at" => s.light_at = parse_hhmm(v).unwrap_or(s.light_at),
                "dark_at" => s.dark_at = parse_hhmm(v).unwrap_or(s.dark_at),
                "wallpaper" => s.wallpaper = WallpaperMode::parse(v).unwrap_or(s.wallpaper),
                "wallpaper_light" => s.wallpaper_light = v.to_owned(),
                "wallpaper_dark" => s.wallpaper_dark = v.to_owned(),
                "dynamic_dir" => s.dynamic_dir = v.to_owned(),
                _ => {}
            }
        }
        s
    }

    pub fn serialize(&self) -> String {
        format!(
            "mode={}\nlight_at={}\ndark_at={}\nwallpaper={}\nwallpaper_light={}\nwallpaper_dark={}\ndynamic_dir={}\n",
            self.mode.as_str(),
            format_hhmm(self.light_at),
            format_hhmm(self.dark_at),
            self.wallpaper.as_str(),
            one_line(&self.wallpaper_light),
            one_line(&self.wallpaper_dark),
            one_line(&self.dynamic_dir),
        )
    }
}

impl Location {
    /// Returns `None` unless both coordinates are present and valid.
    pub fn parse(text: &str) -> Option<Self> {
        let mut loc = Self::default();
        let (mut lat, mut lon) = (None, None);
        for (k, v) in pairs(text) {
            match k {
                "city" => loc.city = v.to_owned(),
                "lat" => lat = v.parse::<f64>().ok(),
                "lon" => lon = v.parse::<f64>().ok(),
                _ => {}
            }
        }
        loc.lat = lat.filter(|v| (-90.0..=90.0).contains(v))?;
        loc.lon = lon.filter(|v| (-180.0..=180.0).contains(v))?;
        Some(loc)
    }

    pub fn serialize(&self) -> String {
        format!("city={}\nlat={}\nlon={}\n", one_line(&self.city), self.lat, self.lon)
    }
}

/// Values must not contain line breaks.
fn one_line(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

/// `$XDG_CONFIG_HOME` or `~/.config`.
pub fn config_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")).unwrap_or_default()
}

pub fn settings_path() -> PathBuf {
    config_home().join("mac-suite/appearance.conf")
}

pub fn location_path() -> PathBuf {
    config_home().join("mac-suite/location.conf")
}

pub fn load_settings() -> Settings {
    fs::read_to_string(settings_path()).map(|t| Settings::parse(&t)).unwrap_or_default()
}

pub fn load_location() -> Option<Location> {
    fs::read_to_string(location_path()).ok().and_then(|t| Location::parse(&t))
}

pub fn write_file(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // Write beside the target and rename, so watchers never see a partial file.
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

pub fn save_settings(s: &Settings) -> std::io::Result<()> {
    write_file(&settings_path(), &s.serialize())
}

pub fn save_location(l: &Location) -> std::io::Result<()> {
    write_file(&location_path(), &l.serialize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let s = Settings {
            mode: Mode::Schedule,
            light_at: 6 * 60 + 30,
            dark_at: 21 * 60,
            wallpaper: WallpaperMode::Dynamic,
            wallpaper_light: "/a/l.jpg".into(),
            wallpaper_dark: "/a/d.jpg".into(),
            dynamic_dir: "/a/dyn".into(),
        };
        assert_eq!(Settings::parse(&s.serialize()), s);
    }

    #[test]
    fn settings_tolerate_garbage() {
        let s = Settings::parse("# c\nmode=bogus\nlight_at=25:00\nnonsense\nwallpaper = pair \n");
        assert_eq!(s.mode, Mode::Auto);
        assert_eq!(s.light_at, DEFAULT_LIGHT_AT);
        assert_eq!(s.wallpaper, WallpaperMode::Pair);
    }

    #[test]
    fn location_parse() {
        let l = Location::parse("city=Jakarta\nlat=-6.2\nlon=106.8\n").unwrap();
        assert_eq!(l.city, "Jakarta");
        assert_eq!(Location::parse(&l.serialize()), Some(l));
        assert!(Location::parse("lat=95\nlon=0").is_none());
        assert!(Location::parse("city=x").is_none());
    }

    #[test]
    fn hhmm() {
        assert_eq!(parse_hhmm("07:05"), Some(425));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(format_hhmm(425), "07:05");
    }
}
