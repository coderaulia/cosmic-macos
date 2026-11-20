// SPDX-License-Identifier: GPL-3.0-only

//! Application list loading, deduplication, and search ranking.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
    pub categories: Vec<String>,
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
        if exec.trim().is_empty() {
            return None;
        }
        Some(Self {
            name_lc: d.name.to_lowercase(),
            id_lc: d.id.to_lowercase(),
            exec_lc: exec_program(&exec).to_lowercase(),
            icon: d.icon.as_cosmic_icon(),
            categories: d.categories,
            id: d.id,
            name: d.name,
            exec,
            terminal: d.terminal,
        })
    }

    /// Desktop-file identifier with `.desktop` suffix for layout persistence.
    pub fn desktop_file_id(&self) -> String {
        if self.id.ends_with(".desktop") {
            self.id.clone()
        } else {
            format!("{}.desktop", self.id)
        }
    }

    fn rank(&self, query_lc: &str) -> Option<u8> {
        rank(&self.name_lc, &self.id_lc, &self.exec_lc, query_lc)
    }

    #[cfg(test)]
    pub(crate) fn for_test(id: &str, name: &str, categories: &[&str]) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            exec: id.to_owned(),
            terminal: false,
            icon: icon::from_name("application-default").handle(),
            categories: categories.iter().map(|s| (*s).to_owned()).collect(),
            name_lc: name.to_lowercase(),
            id_lc: id.to_lowercase(),
            exec_lc: id.to_lowercase(),
        }
    }
}

/// Program name of an Exec line (first token, without directories).
fn exec_program(exec: &str) -> &str {
    let first = exec.split_whitespace().next().unwrap_or("");
    first.rsplit('/').next().unwrap_or(first)
}

/// Returns `true` for internal `COSMIC Preset: …` helper desktop entries.
pub fn is_helper_entry(name: &str) -> bool {
    name.trim()
        .to_ascii_lowercase()
        .starts_with("cosmic preset:")
}

/// Normalized desktop-file id (lowercase, `.desktop` suffix stripped).
pub fn normalize_desktop_id(id: &str) -> String {
    let trimmed = id.trim();
    let base = trimmed.strip_suffix(".desktop").unwrap_or(trimmed);
    base.to_ascii_lowercase()
}

/// Normalized display name for duplicate comparison.
fn normalize_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Splits an `Exec` line into unquoted tokens while stripping freedesktop `%x`
/// field codes and normalizing the executable path to its basename.
pub fn normalize_exec(exec: &str) -> String {
    let tokens = split_exec_tokens(exec);
    let mut out = Vec::with_capacity(tokens.len());
    for tok in tokens {
        if is_field_code(&tok) {
            continue;
        }
        if out.is_empty() {
            let base = tok.rsplit('/').next().unwrap_or(&tok);
            out.push(base.to_lowercase());
        } else {
            out.push(tok.to_lowercase());
        }
    }
    out.join(" ")
}

fn is_field_code(tok: &str) -> bool {
    matches!(
        tok,
        "%f" | "%F" | "%u" | "%U" | "%d" | "%D" | "%n" | "%N" | "%i" | "%c" | "%k" | "%v" | "%m"
    )
}

fn split_exec_tokens(exec: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = exec.chars().peekable();

    while let Some(ch) = chars.next() {
        match quote {
            Some(q) if ch == q => {
                quote = None;
            }
            Some(_) => {
                if ch == '\\' {
                    if let Some(next) = chars.next() {
                        cur.push(next);
                    }
                } else {
                    cur.push(ch);
                }
            }
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
            }
            None if ch.is_whitespace() => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            None => {
                if ch == '\\' {
                    if let Some(next) = chars.next() {
                        cur.push(next);
                    }
                } else {
                    cur.push(ch);
                }
            }
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// Priority rank for a `.desktop` file path (lower = preferred).
/// Prefers `~/.local/share/applications` (or `$XDG_DATA_HOME/applications`)
/// over `/usr/local/share/applications` and `/usr/share/applications`.
pub fn source_priority(
    path: Option<&Path>,
    xdg_data_home: Option<&Path>,
    home: Option<&Path>,
) -> u8 {
    let Some(path) = path else {
        return 4;
    };
    if let Some(data_home) = xdg_data_home
        && path.starts_with(data_home.join("applications"))
    {
        return 0;
    }
    if let Some(home_dir) = home
        && path.starts_with(home_dir.join(".local/share/applications"))
    {
        return 0;
    }
    let path_str = path.to_string_lossy();
    if path_str.contains("/.local/share/applications/") {
        return 0;
    }
    if path.starts_with("/usr/local/share/applications") {
        return 2;
    }
    if path.starts_with("/usr/share/applications") {
        return 3;
    }
    1
}

/// Filters helper entries and deduplicates desktop entries first by desktop-file
/// id, then by normalized `(Name, Exec)`, preferring user entries under
/// `~/.local/share/applications` over `/usr/share/applications`.
pub fn dedupe_entries(data: Vec<DesktopEntryData>) -> Vec<DesktopEntryData> {
    let xdg_data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    dedupe_entries_with_dirs(data, xdg_data_home.as_deref(), home.as_deref())
}

pub fn dedupe_entries_with_dirs(
    data: Vec<DesktopEntryData>,
    xdg_data_home: Option<&Path>,
    home: Option<&Path>,
) -> Vec<DesktopEntryData> {
    let mut candidates: Vec<DesktopEntryData> = data
        .into_iter()
        .filter(|d| {
            d.exec.as_ref().is_some_and(|e| !e.trim().is_empty()) && !is_helper_entry(&d.name)
        })
        .collect();

    // Sort stably so higher-priority directories (e.g. ~/.local/share/applications)
    // are always inspected first.
    candidates.sort_by_key(|d| source_priority(d.path.as_deref(), xdg_data_home, home));

    let mut seen_ids = HashSet::new();
    let mut seen_name_exec = HashSet::new();
    let mut deduped = Vec::with_capacity(candidates.len());

    for entry in candidates {
        let norm_id = normalize_desktop_id(&entry.id);
        let norm_name = normalize_name(&entry.name);
        let norm_exec = normalize_exec(entry.exec.as_deref().unwrap_or_default());

        if seen_ids.contains(&norm_id) {
            continue;
        }
        let name_exec_key = (norm_name, norm_exec);
        if seen_name_exec.contains(&name_exec_key) {
            continue;
        }
        seen_ids.insert(norm_id);
        seen_name_exec.insert(name_exec_key);
        deduped.push(entry);
    }

    deduped.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.to_lowercase().cmp(&b.id.to_lowercase()))
    });
    deduped
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

/// Loads all visible, deduplicated applications, sorted by name. Blocking.
pub fn load() -> Vec<DesktopEntryData> {
    let locales = fde::get_languages_from_env();
    let desktop_env = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let raw: Vec<DesktopEntryData> =
        desktop::load_applications(&locales, false, desktop_env.as_deref()).collect();
    dedupe_entries(raw)
}

pub fn into_entries(data: Vec<DesktopEntryData>) -> Vec<AppEntry> {
    data.into_iter().filter_map(AppEntry::from_data).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(
        id: &str,
        name: &str,
        exec: Option<&str>,
        path: Option<&str>,
    ) -> DesktopEntryData {
        DesktopEntryData {
            id: id.to_owned(),
            name: name.to_owned(),
            exec: exec.map(str::to_owned),
            path: path.map(PathBuf::from),
            ..Default::default()
        }
    }

    #[test]
    fn exec_program_strips_dir_and_args() {
        assert_eq!(exec_program("/usr/bin/firefox %u"), "firefox");
        assert_eq!(exec_program("gimp-2.10 %U"), "gimp-2.10");
        assert_eq!(exec_program(""), "");
    }

    #[test]
    fn rank_orders_prefix_word_substring_id() {
        assert_eq!(
            rank("firefox", "org.mozilla.firefox", "firefox", "fire"),
            Some(0)
        );
        assert_eq!(rank("mozilla firefox", "x", "x", "fire"), Some(1));
        assert_eq!(rank("superfirefox", "x", "x", "fire"), Some(2));
        assert_eq!(rank("web", "org.mozilla.firefox", "x", "fire"), Some(3));
        assert_eq!(rank("web", "x", "browser", "zzz"), None);
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(rank("a", "b", "c", ""), Some(0));
    }

    #[test]
    fn normalize_exec_strips_paths_quotes_and_field_codes() {
        assert_eq!(
            normalize_exec("\"/usr/bin/artcraft-launcher\" --no-sandbox %U"),
            "artcraft-launcher --no-sandbox"
        );
        assert_eq!(
            normalize_exec("artcraft-launcher --no-sandbox %f"),
            "artcraft-launcher --no-sandbox"
        );
        assert_eq!(normalize_exec("  /bin/foo   bar  "), "foo bar");
    }

    #[test]
    fn dedupe_prefers_local_share_by_id_and_by_name_exec() {
        let home = Path::new("/home/alice");
        let entries = vec![
            make_entry(
                "artcraft-system",
                "ArtCraft Launcher",
                Some("/usr/bin/artcraft-launcher %U"),
                Some("/usr/share/applications/artcraft-system.desktop"),
            ),
            make_entry(
                "artcraft-local",
                "ArtCraft Launcher",
                Some("/home/alice/.local/bin/artcraft-launcher %u"),
                Some("/home/alice/.local/share/applications/artcraft-local.desktop"),
            ),
            make_entry(
                "firefox",
                "Firefox System",
                Some("/usr/bin/firefox %u"),
                Some("/usr/share/applications/firefox.desktop"),
            ),
            make_entry(
                "Firefox.desktop",
                "Firefox Local",
                Some("/home/alice/.local/bin/firefox-custom"),
                Some("/home/alice/.local/share/applications/firefox.desktop"),
            ),
        ];

        let out = dedupe_entries_with_dirs(entries, None, Some(home));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "artcraft-local");
        assert_eq!(out[1].name, "Firefox Local");
    }

    #[test]
    fn dedupe_hides_cosmic_preset_helpers_and_empty_exec() {
        let entries = vec![
            make_entry(
                "cosmic-preset-macos-glass",
                "COSMIC Preset: macOS Glass",
                Some("/home/alice/.local/bin/cosmic-preset apply macos-glass"),
                Some("/home/alice/.local/share/applications/cosmic-preset-macos-glass.desktop"),
            ),
            make_entry(
                "cosmic-preset-revert",
                "  cosmic preset: Revert Original",
                Some("/home/alice/.local/bin/cosmic-preset revert"),
                Some("/home/alice/.local/share/applications/cosmic-preset-revert.desktop"),
            ),
            make_entry("no-exec", "Broken App", None, None),
            make_entry("empty-exec", "Empty App", Some("   "), None),
            make_entry("calc", "Calculator", Some("gnome-calculator"), None),
        ];

        let out = dedupe_entries_with_dirs(entries, None, None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "calc");
    }
}
