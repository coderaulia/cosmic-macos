// SPDX-License-Identifier: GPL-3.0-only

//! Configuration for `mac-clipboard` backed by `cosmic-config`.

use cosmic::cosmic_config::{self, Config, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};
use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "dev.asw.MacClipboard";
pub const CONFIG_VERSION: u64 = 1;

pub const DEFAULT_IGNORED_APPS: &[&str] = &[
    "org.keepassxc.KeePassXC",
    "com.bitwarden.desktop",
    "1password",
    "org.gnome.seahorse.Application",
];

pub const DEFAULT_TERMINAL_APPS: &[&str] = &[
    "com.system76.CosmicTerm",
    "org.gnome.Ptyxis",
    "kitty",
    "Alacritty",
    "foot",
    "org.wezfurlong.wezterm",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExpiryPolicy {
    #[default]
    Never,
    OneDay,
    SevenDays,
    ThirtyDays,
}

impl ExpiryPolicy {
    #[must_use]
    pub const fn as_millis(self) -> Option<i64> {
        match self {
            Self::Never => None,
            Self::OneDay => Some(24 * 60 * 60 * 1000),
            Self::SevenDays => Some(7 * 24 * 60 * 60 * 1000),
            Self::ThirtyDays => Some(30 * 24 * 60 * 60 * 1000),
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Never => "Never",
            Self::OneDay => "1 day",
            Self::SevenDays => "7 days",
            Self::ThirtyDays => "30 days",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, CosmicConfigEntry)]
#[version = 1]
pub struct ClipboardConfig {
    pub max_items: usize,
    pub max_pinned: usize,
    pub max_bytes: u64,
    pub max_text_bytes: usize,
    pub max_html_bytes: usize,
    pub max_image_bytes: usize,
    pub max_image_pixels: u64,
    pub record_oversized_placeholder: bool,
    pub expiry: ExpiryPolicy,
    pub watch_primary: bool,
    pub keep_alive: bool,
    pub auto_paste: bool,
    pub ignored_apps: Vec<String>,
    pub terminal_apps: Vec<String>,
    pub hotkey_hint: String,
    pub paused: bool,
    pub paused_until_unix: Option<i64>,
    pub secret_heuristic: bool,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            max_items: 500,
            max_pinned: 100,
            max_bytes: 256 * 1024 * 1024,
            max_text_bytes: 1024 * 1024,
            max_html_bytes: 2 * 1024 * 1024,
            max_image_bytes: 32 * 1024 * 1024,
            max_image_pixels: 40_000_000,
            record_oversized_placeholder: false,
            expiry: ExpiryPolicy::Never,
            watch_primary: false,
            keep_alive: true,
            auto_paste: true,
            ignored_apps: DEFAULT_IGNORED_APPS.iter().map(|s| (*s).to_string()).collect(),
            terminal_apps: DEFAULT_TERMINAL_APPS.iter().map(|s| (*s).to_string()).collect(),
            hotkey_hint: "⌘V".to_string(),
            paused: false,
            paused_until_unix: None,
            secret_heuristic: false,
        }
    }
}

impl ClipboardConfig {
    /// Load configuration from `cosmic-config`, falling back to defaults.
    #[must_use]
    pub fn load() -> Self {
        let Ok(config) = Config::new(APP_ID, CONFIG_VERSION) else {
            return Self::default();
        };
        match Self::get_entry(&config) {
            Ok(entry) => entry,
            Err((_, entry)) => entry,
        }
    }

    /// Persist configuration to `cosmic-config`.
    pub fn save(&self) -> Result<(), cosmic_config::Error> {
        let config = Config::new(APP_ID, CONFIG_VERSION)?;
        self.write_entry(&config)
    }

    /// Returns whether recording is currently paused at `now_unix_ms`.
    #[must_use]
    pub fn is_paused_at(&self, now_unix_ms: i64) -> bool {
        if !self.paused {
            return false;
        }
        match self.paused_until_unix {
            Some(until_ms) => now_unix_ms < until_ms,
            None => true,
        }
    }
}
