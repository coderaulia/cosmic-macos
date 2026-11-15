// SPDX-License-Identifier: GPL-3.0-only

//! Application list loading and search ranking.

use cosmic::desktop::{self, DesktopEntryData, IconSourceExt, fde};
use cosmic::widget::icon;

/// One launchable application.
#[derive(Clone)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub terminal: bool,
    pub icon: icon::Handle,
    name_lc: String,
    id_lc: String,
    exec_lc: String,
}

impl std::fmt::Debug for AppEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppEntry").field("id", &self.id).finish()
    }
}

impl AppEntry {
    fn from_data(d: DesktopEntryData) -> Option<Self> {
        let exec = d.exec?;
        Some(Self {
            name_lc: d.name.to_lowercase(),
            id_lc: d.id.to_lowercase(),
            exec_lc: exec_program(&exec).to_lowercase(),
            icon: d.icon.as_cosmic_icon(),
            id: d.id,
            name: d.name,
            exec,
            terminal: d.terminal,
        })
    }

    fn rank(&self, query_lc: &str) -> Option<u8> {
        rank(&self.name_lc, &self.id_lc, &self.exec_lc, query_lc)
    }
}

/// Program name of an Exec line (first token, without directories).
fn exec_program(exec: &str) -> &str {
    let first = exec.split_whitespace().next().unwrap_or("");
    first.rsplit('/').next().unwrap_or(first)
}

/// Lower is better; `None` means no match. All inputs must be lowercase.
fn rank(name: &str, id: &str, exec: &str, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    if name.starts_with(query) {
        Some(0)
    } else if name
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w.starts_with(query))
    {
        Some(1)
    } else if name.contains(query) {
        Some(2)
    } else if id.contains(query) || exec.contains(query) {
        Some(3)
    } else {
        None
    }
}

/// Indices into `apps` matching `query`, best match first (stable by name).
pub fn filter(apps: &[AppEntry], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    let mut hits: Vec<(u8, usize)> = apps
        .iter()
        .enumerate()
        .filter_map(|(i, a)| a.rank(&q).map(|r| (r, i)))
        .collect();
    hits.sort_by_key(|&(r, i)| (r, i));
    hits.into_iter().map(|(_, i)| i).collect()
}

/// Loads all visible applications, sorted by name. Blocking.
pub fn load() -> Vec<DesktopEntryData> {
    let locales = fde::get_languages_from_env();
    let desktop_env = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let mut apps: Vec<DesktopEntryData> =
        desktop::load_applications(&locales, false, desktop_env.as_deref()).collect();
    apps.sort_by_cached_key(|a| a.name.to_lowercase());
    apps
}

pub fn into_entries(data: Vec<DesktopEntryData>) -> Vec<AppEntry> {
    data.into_iter().filter_map(AppEntry::from_data).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_program_strips_dir_and_args() {
        assert_eq!(exec_program("/usr/bin/firefox %u"), "firefox");
        assert_eq!(exec_program("gimp-2.10 %U"), "gimp-2.10");
        assert_eq!(exec_program(""), "");
    }

    #[test]
    fn rank_orders_prefix_word_substring_id() {
        assert_eq!(rank("firefox", "org.mozilla.firefox", "firefox", "fire"), Some(0));
        assert_eq!(rank("mozilla firefox", "x", "x", "fire"), Some(1));
        assert_eq!(rank("superfirefox", "x", "x", "fire"), Some(2));
        assert_eq!(rank("web", "org.mozilla.firefox", "x", "fire"), Some(3));
        assert_eq!(rank("web", "x", "browser", "zzz"), None);
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(rank("a", "b", "c", ""), Some(0));
    }
}
