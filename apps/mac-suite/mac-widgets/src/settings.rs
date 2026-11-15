// SPDX-License-Identifier: GPL-3.0-only
//! Widget settings from `~/.config/mac-suite/widgets.conf` (`key=value` lines).

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Units {
    #[default]
    Celsius,
    Fahrenheit,
}

impl Units {
    /// Converts a Celsius value to this unit.
    pub fn from_celsius(self, c: f64) -> f64 {
        match self {
            Units::Celsius => c,
            Units::Fahrenheit => c * 9.0 / 5.0 + 32.0,
        }
    }

    /// Temperature rounded and formatted, e.g. `21°`.
    pub fn format(self, c: f64) -> String {
        format!("{:.0}°", self.from_celsius(c))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WidgetSettings {
    pub show_weather: bool,
    pub show_up_next: bool,
    pub show_system: bool,
    pub units: Units,
}

impl Default for WidgetSettings {
    fn default() -> Self {
        Self {
            show_weather: true,
            show_up_next: true,
            show_system: true,
            units: Units::Celsius,
        }
    }
}

impl WidgetSettings {
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim().to_ascii_lowercase());
            let flag = value != "false" && value != "0" && value != "no";
            match key {
                "show_weather" => s.show_weather = flag,
                "show_up_next" => s.show_up_next = flag,
                "show_system" => s.show_system = flag,
                "weather_units" => {
                    s.units = if value == "fahrenheit" {
                        Units::Fahrenheit
                    } else {
                        Units::Celsius
                    }
                }
                _ => {}
            }
        }
        s
    }

    /// Loads the settings file; a missing or unreadable file gives defaults.
    pub fn load() -> Self {
        config_dir()
            .and_then(|dir| std::fs::read_to_string(dir.join("widgets.conf")).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }
}

/// `~/.config/mac-suite` (honouring `XDG_CONFIG_HOME`).
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("mac-suite"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty() {
        assert_eq!(WidgetSettings::parse(""), WidgetSettings::default());
    }

    #[test]
    fn parses_toggles_and_units() {
        let s = WidgetSettings::parse("show_weather=false\nshow_system = 0\nweather_units=fahrenheit\n");
        assert!(!s.show_weather && s.show_up_next && !s.show_system);
        assert_eq!(s.units, Units::Fahrenheit);
    }

    #[test]
    fn converts_units() {
        assert_eq!(Units::Fahrenheit.format(100.0), "212°");
        assert_eq!(Units::Celsius.format(20.4), "20°");
    }
}
