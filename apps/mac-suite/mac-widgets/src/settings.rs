// SPDX-License-Identifier: GPL-3.0-only
//! Widget settings from `~/.config/mac-suite/widgets.conf` (`key=value` lines).

use std::path::{Path, PathBuf};

const SYSMON_APP_ID: &str = "dev.asw.CosmicExtAppletSysmon";

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
        Self::parse_with_default_system(text, true)
    }

    pub fn parse_with_default_system(text: &str, default_show_system: bool) -> Self {
        let mut s = Self {
            show_system: default_show_system,
            ..Self::default()
        };
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

    /// Loads the settings file using explicit XDG base directories.
    /// When `show_system` is not set in `widgets.conf`, it defaults to `false`
    /// if `dev.asw.CosmicExtAppletSysmon` is present in the panel layout or
    /// installed as a `.desktop` entry.
    pub fn load_with(config_home: Option<&Path>, data_home: Option<&Path>) -> Self {
        let default_show_system = !sysmon_present_in(config_home, data_home);
        let conf = config_home
            .map(|dir| dir.join("mac-suite/widgets.conf"))
            .and_then(|path| std::fs::read_to_string(path).ok());
        match conf {
            Some(text) => Self::parse_with_default_system(&text, default_show_system),
            None => Self {
                show_system: default_show_system,
                ..Self::default()
            },
        }
    }

    /// Loads the settings file; a missing or unreadable file gives defaults.
    pub fn load() -> Self {
        Self::load_with(xdg_config_home().as_deref(), xdg_data_home().as_deref())
    }
}

/// Returns `true` when `dev.asw.CosmicExtAppletSysmon` is present in the
/// COSMIC panel layout (`plugins_wings` / `plugins_center`) or installed as a
/// `.desktop` file under `data_home/applications/`.
pub fn sysmon_present_in(config_home: Option<&Path>, data_home: Option<&Path>) -> bool {
    if let Some(cfg) = config_home {
        for panel in ["com.system76.CosmicPanel.Panel", "com.system76.CosmicPanel.Dock"] {
            for file in ["plugins_wings", "plugins_center"] {
                let path = cfg.join("cosmic").join(panel).join("v1").join(file);
                if let Ok(contents) = std::fs::read_to_string(path)
                    && contents.contains(SYSMON_APP_ID)
                {
                    return true;
                }
            }
        }
    }
    if let Some(data) = data_home {
        let desktop = data.join("applications").join(format!("{SYSMON_APP_ID}.desktop"));
        if desktop.is_file() {
            return true;
        }
    }
    false
}

/// Returns `true` when `dev.asw.CosmicExtAppletSysmon` is active in the user's
/// panel layout or installed `.desktop` directory.
pub fn sysmon_present() -> bool {
    sysmon_present_in(xdg_config_home().as_deref(), xdg_data_home().as_deref())
}

fn xdg_config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

fn xdg_data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

/// `~/.config/mac-suite` (honouring `XDG_CONFIG_HOME`).
pub fn config_dir() -> Option<PathBuf> {
    xdg_config_home().map(|base| base.join("mac-suite"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "mac-widgets-test-{label}-{}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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

    #[test]
    fn show_system_defaults_true_when_sysmon_absent() {
        let root = temp_dir("sysmon-absent");
        let cfg = root.join("config");
        let data = root.join("data");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(&data).unwrap();

        let s = WidgetSettings::load_with(Some(&cfg), Some(&data));
        assert!(s.show_system);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn show_system_defaults_false_when_sysmon_in_wings() {
        let root = temp_dir("sysmon-wings");
        let cfg = root.join("config");
        let data = root.join("data");
        let panel = cfg.join("cosmic/com.system76.CosmicPanel.Panel/v1");
        std::fs::create_dir_all(&panel).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            panel.join("plugins_wings"),
            r#"Some(([], ["dev.asw.CosmicExtAppletSysmon"]))"#,
        )
        .unwrap();

        let s = WidgetSettings::load_with(Some(&cfg), Some(&data));
        assert!(!s.show_system);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn show_system_defaults_false_when_sysmon_in_center() {
        let root = temp_dir("sysmon-center");
        let cfg = root.join("config");
        let data = root.join("data");
        let panel = cfg.join("cosmic/com.system76.CosmicPanel.Panel/v1");
        std::fs::create_dir_all(&panel).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            panel.join("plugins_center"),
            r#"Some(["dev.asw.CosmicExtAppletSysmon"])"#,
        )
        .unwrap();

        let s = WidgetSettings::load_with(Some(&cfg), Some(&data));
        assert!(!s.show_system);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn show_system_defaults_false_when_sysmon_desktop_installed() {
        let root = temp_dir("sysmon-desktop");
        let cfg = root.join("config");
        let data = root.join("data");
        let apps = data.join("applications");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join("dev.asw.CosmicExtAppletSysmon.desktop"),
            "[Desktop Entry]\nName=System Stats\n",
        )
        .unwrap();

        let s = WidgetSettings::load_with(Some(&cfg), Some(&data));
        assert!(!s.show_system);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn explicit_widgets_conf_overrides_sysmon_default() {
        let root = temp_dir("sysmon-override");
        let cfg = root.join("config");
        let data = root.join("data");
        let apps = data.join("applications");
        let suite = cfg.join("mac-suite");
        std::fs::create_dir_all(&suite).unwrap();
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join("dev.asw.CosmicExtAppletSysmon.desktop"),
            "[Desktop Entry]\nName=System Stats\n",
        )
        .unwrap();
        std::fs::write(suite.join("widgets.conf"), "show_system=true\n").unwrap();

        let s = WidgetSettings::load_with(Some(&cfg), Some(&data));
        assert!(s.show_system);
        let _ = std::fs::remove_dir_all(&root);
    }
}
