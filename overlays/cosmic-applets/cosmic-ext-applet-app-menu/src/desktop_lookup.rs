// SPDX-License-Identifier: GPL-3.0-only

//! `.desktop` file parsing, `Actions=` extraction, and active application resolution.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use cosmic::desktop::fde;

/// A single `[Desktop Action <id>]` entry parsed from a `.desktop` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAction {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub icon: Option<String>,
}

/// Parsed `.desktop` application metadata used for menu bar display and actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAppInfo {
    pub id: String,
    pub name: String,
    pub comment: Option<String>,
    pub version: Option<String>,
    pub startup_wm_class: Option<String>,
    pub exec: Option<String>,
    pub icon: Option<String>,
    pub terminal: bool,
    pub no_display: bool,
    pub hidden: bool,
    pub actions: Vec<DesktopAction>,
}

/// Resolved display identity and optional `.desktop` metadata for the active window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedApp {
    pub display_name: String,
    pub desktop: Option<DesktopAppInfo>,
}

/// Parse a `.desktop` file's UTF-8 contents into a [`DesktopAppInfo`].
///
/// Extracts localized `Name`, `Comment`/`GenericName`, `StartupWMClass`, `Exec`,
/// `Icon`, `Terminal`, `NoDisplay`, `Hidden`, version metadata, and `Actions=`
/// sections (`[Desktop Action <id>]`).
#[must_use]
pub fn parse_desktop_file_str(
    desktop_id: &str,
    content: &str,
    locales: &[String],
) -> Option<DesktopAppInfo> {
    let mut current_section = String::new();
    let mut main_fields: HashMap<String, String> = HashMap::new();
    let mut action_sections: HashMap<String, HashMap<String, String>> = HashMap::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            current_section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        let val = val.trim().to_string();
        if current_section == "Desktop Entry" {
            main_fields.insert(key, val);
        } else if let Some(action_id) = current_section.strip_prefix("Desktop Action ") {
            action_sections
                .entry(action_id.trim().to_string())
                .or_default()
                .insert(key, val);
        }
    }

    if main_fields.is_empty() {
        return None;
    }

    if let Some(entry_type) = main_fields.get("Type")
        && entry_type != "Application"
    {
        return None;
    }

    let hidden = main_fields
        .get("Hidden")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"));
    if hidden {
        return None;
    }

    let no_display = main_fields
        .get("NoDisplay")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"));

    let clean_id = desktop_id
        .strip_suffix(".desktop")
        .unwrap_or(desktop_id)
        .to_string();

    let name = localized_value(&main_fields, "Name", locales)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| clean_id.clone());

    let comment = localized_value(&main_fields, "Comment", locales)
        .or_else(|| localized_value(&main_fields, "GenericName", locales))
        .filter(|s| !s.is_empty());

    let version = main_fields
        .get("X-AppImage-Version")
        .or_else(|| main_fields.get("X-Flatpak-Version"))
        .or_else(|| main_fields.get("X-Version"))
        .cloned()
        .filter(|s| !s.is_empty());

    let startup_wm_class = main_fields
        .get("StartupWMClass")
        .cloned()
        .filter(|s| !s.is_empty());
    let exec = main_fields.get("Exec").cloned().filter(|s| !s.is_empty());
    let icon = main_fields.get("Icon").cloned().filter(|s| !s.is_empty());
    let terminal = main_fields
        .get("Terminal")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"));

    let mut actions = Vec::new();
    if let Some(raw_actions) = main_fields.get("Actions") {
        for action_id in raw_actions
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if let Some(fields) = action_sections.get(action_id) {
                let Some(action_name) =
                    localized_value(fields, "Name", locales).filter(|s| !s.is_empty())
                else {
                    continue;
                };
                let Some(action_exec) = fields.get("Exec").cloned().filter(|s| !s.is_empty())
                else {
                    continue;
                };
                let action_icon = fields.get("Icon").cloned().filter(|s| !s.is_empty());
                actions.push(DesktopAction {
                    id: action_id.to_string(),
                    name: action_name,
                    exec: action_exec,
                    icon: action_icon,
                });
            }
        }
    }

    Some(DesktopAppInfo {
        id: clean_id,
        name,
        comment,
        version,
        startup_wm_class,
        exec,
        icon,
        terminal,
        no_display,
        hidden,
        actions,
    })
}

fn localized_value(
    fields: &HashMap<String, String>,
    key: &str,
    locales: &[String],
) -> Option<String> {
    for loc in locales {
        let full = format!("{key}[{loc}]");
        if let Some(v) = fields.get(&full) {
            return Some(v.clone());
        }
        if let Some(lang) = loc.split(['_', '.', '@']).next() {
            let short = format!("{key}[{lang}]");
            if let Some(v) = fields.get(&short) {
                return Some(v.clone());
            }
        }
    }
    fields.get(key).cloned()
}

/// Load all `.desktop` application entries from standard XDG directories.
#[must_use]
pub fn load_system_desktop_entries(locales: &[String]) -> Vec<DesktopAppInfo> {
    let mut seen_ids = std::collections::HashSet::new();
    let mut entries = Vec::new();

    for path in fde::default_paths() {
        let Ok(dir_entries) = walk_desktop_files(&path) else {
            continue;
        };
        for file_path in dir_entries {
            let Some(desktop_id) = desktop_id_from_path(&path, &file_path) else {
                continue;
            };
            if !seen_ids.insert(desktop_id.to_ascii_lowercase()) {
                continue;
            }
            let Ok(content) = fs::read_to_string(&file_path) else {
                continue;
            };
            if let Some(info) = parse_desktop_file_str(&desktop_id, &content, locales) {
                entries.push(info);
            }
        }
    }

    entries
}

fn walk_desktop_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("desktop") {
                files.push(path);
            }
        }
    }
    Ok(files)
}

fn desktop_id_from_path(root: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .iter()
        .filter_map(|s| s.to_str().map(ToString::to_string))
        .collect();
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join("-");
    Some(joined.strip_suffix(".desktop").unwrap_or(&joined).to_string())
}

fn normalize_name(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn exec_basename(exec: &str) -> Option<&str> {
    let mut tokens = exec.split_whitespace();
    let first = tokens.next()?;
    let bin = if first == "env" {
        tokens.find(|t| !t.contains('='))?
    } else {
        first
    };
    let clean = bin.trim_matches('"').trim_matches('\'');
    clean.rsplit('/').next().filter(|s| !s.is_empty())
}

/// Resolve an active window (`app_id`, `title`) against `.desktop` entries.
///
/// Matching priority:
/// 1. No window focused (empty `app_id` and `title`) -> `fallback_name` (e.g. `"Files"`) + file manager entry if present.
/// 2. Exact desktop ID match (case-insensitive, `.desktop` suffix optional).
/// 3. `StartupWMClass` match (case-insensitive).
/// 4. Reverse-DNS matching (full ID, last segment, or normalized segment against desktop IDs).
/// 5. `Exec=` binary basename match.
/// 6. Fallback to `title` if non-empty, else `app_id`, else `fallback_name`.
#[must_use]
pub fn resolve_active_app(
    app_id: &str,
    title: &str,
    entries: &[DesktopAppInfo],
    fallback_name: &str,
) -> ResolvedApp {
    let raw_id = app_id.trim();
    let raw_title = title.trim();

    if raw_id.is_empty() && raw_title.is_empty() {
        let files_entry = find_desktop_match("com.system76.CosmicFiles", entries)
            .or_else(|| find_desktop_match("dev.asw.MacFiles", entries))
            .or_else(|| find_desktop_match("org.gnome.Nautilus", entries))
            .cloned();
        return ResolvedApp {
            display_name: fallback_name.to_string(),
            desktop: files_entry,
        };
    }

    if let Some(entry) = find_desktop_match(raw_id, entries) {
        return ResolvedApp {
            display_name: entry.name.clone(),
            desktop: Some(entry.clone()),
        };
    }

    let fallback_display = if !raw_title.is_empty() {
        raw_title.to_string()
    } else if !raw_id.is_empty() {
        raw_id
            .strip_suffix(".desktop")
            .unwrap_or(raw_id)
            .rsplit('.')
            .next()
            .unwrap_or(raw_id)
            .to_string()
    } else {
        fallback_name.to_string()
    };

    ResolvedApp {
        display_name: fallback_display,
        desktop: None,
    }
}

/// Find the best matching [`DesktopAppInfo`] for an `app_id`.
#[must_use]
pub fn find_desktop_match<'a>(
    app_id: &str,
    entries: &'a [DesktopAppInfo],
) -> Option<&'a DesktopAppInfo> {
    let clean = app_id.trim().strip_suffix(".desktop").unwrap_or(app_id.trim());
    if clean.is_empty() {
        return None;
    }

    // Prefer visible (`!no_display`) entries before hidden helper entries.
    let ordered = entries
        .iter()
        .filter(|e| !e.no_display)
        .chain(entries.iter().filter(|e| e.no_display));

    // 1. Exact desktop ID match
    for entry in ordered.clone() {
        if entry.id.eq_ignore_ascii_case(clean) {
            return Some(entry);
        }
    }

    // 2. StartupWMClass match
    for entry in ordered.clone() {
        if let Some(wm) = &entry.startup_wm_class
            && wm.eq_ignore_ascii_case(clean)
        {
            return Some(entry);
        }
    }

    // 3. Reverse-DNS matching (e.g. `org.mozilla.firefox` <-> `firefox`,
    //    or `com.system76.CosmicFiles` <-> `cosmic-files`)
    let clean_last = clean.rsplit('.').next().unwrap_or(clean);
    let clean_norm = normalize_name(clean);
    let clean_last_norm = normalize_name(clean_last);

    for entry in ordered.clone() {
        let entry_last = entry.id.rsplit('.').next().unwrap_or(&entry.id);
        if entry_last.eq_ignore_ascii_case(clean)
            || entry.id.eq_ignore_ascii_case(clean_last)
            || entry_last.eq_ignore_ascii_case(clean_last)
        {
            return Some(entry);
        }
    }

    for entry in ordered.clone() {
        let entry_norm = normalize_name(&entry.id);
        let entry_last_norm = normalize_name(entry.id.rsplit('.').next().unwrap_or(&entry.id));
        if (!clean_norm.is_empty() && entry_norm == clean_norm)
            || (!clean_last_norm.is_empty()
                && (entry_norm == clean_last_norm || entry_last_norm == clean_last_norm))
        {
            return Some(entry);
        }
    }

    // 4. Exec= basename match
    for entry in ordered {
        if let Some(exec) = &entry.exec
            && let Some(base) = exec_basename(exec)
            && (base.eq_ignore_ascii_case(clean)
                || base.eq_ignore_ascii_case(clean_last)
                || (!clean_last_norm.is_empty() && normalize_name(base) == clean_last_norm))
        {
            return Some(entry);
        }
    }

    None
}

/// Split out the primary `new-window` action (if present) from remaining `.desktop` actions.
#[must_use]
pub fn split_new_window_action(
    actions: &[DesktopAction],
) -> (Option<&DesktopAction>, Vec<&DesktopAction>) {
    let mut new_window = None;
    let mut others = Vec::new();
    for action in actions {
        let norm = normalize_name(&action.id);
        if new_window.is_none() && norm == "newwindow" {
            new_window = Some(action);
        } else {
            others.push(action);
        }
    }
    (new_window, others)
}

/// Elide a label so that bold 13px text fits within `max_px` (220px for the app pill).
#[must_use]
pub fn elide_to_max_width(label: &str, max_px: f32) -> String {
    // Account for 16px total horizontal pill padding (8px left + 8px right).
    let text_budget = (max_px - 16.0).max(24.0);
    let total_width: f32 = label.chars().map(approx_bold_13_char_width).sum();
    if total_width <= text_budget {
        return label.to_string();
    }

    let ellipsis_width = 11.0;
    let mut acc = 0.0;
    let mut out = String::new();
    for ch in label.chars() {
        let w = approx_bold_13_char_width(ch);
        if acc + w + ellipsis_width > text_budget {
            break;
        }
        acc += w;
        out.push(ch);
    }
    let trimmed = out.trim_end();
    format!("{trimmed}…")
}

fn approx_bold_13_char_width(ch: char) -> f32 {
    match ch {
        'i' | 'l' | 'I' | '!' | '|' | '.' | ',' | ':' | ';' | '\'' => 4.2,
        'f' | 'j' | 'r' | 't' | '-' | ' ' => 5.5,
        'm' | 'w' | 'M' | 'W' | '@' | '%' => 10.8,
        c if c.is_ascii_uppercase() => 8.8,
        _ => 7.4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIREFOX_DESKTOP: &str = r#"
[Desktop Entry]
Type=Application
Name=Firefox
Comment=Browse the World Wide Web
Exec=/usr/lib/firefox/firefox %u
Icon=firefox
StartupWMClass=firefox
Actions=new-window;new-private-window;

[Desktop Action new-window]
Name=Open a New Window
Exec=/usr/lib/firefox/firefox --new-window %u

[Desktop Action new-private-window]
Name=Open a New Private Window
Exec=/usr/lib/firefox/firefox --private-window %u
"#;

    const CODE_DESKTOP: &str = r#"
[Desktop Entry]
Type=Application
Name=Visual Studio Code
Comment=Code Editing. Redefined.
Exec=/usr/share/code/code %F
Icon=vscode
StartupWMClass=Code
Actions=new-empty-window;

[Desktop Action new-empty-window]
Name=New Empty Window
Exec=/usr/share/code/code --new-window %F
"#;

    const FILES_DESKTOP: &str = r#"
[Desktop Entry]
Type=Application
Name=Files
Comment=Browse files and folders
Exec=cosmic-files %U
Icon=system-file-manager
"#;

    #[test]
    fn parses_desktop_entry_and_actions() {
        let info = parse_desktop_file_str("org.mozilla.firefox", FIREFOX_DESKTOP, &[])
            .expect("should parse Firefox desktop entry");
        assert_eq!(info.id, "org.mozilla.firefox");
        assert_eq!(info.name, "Firefox");
        assert_eq!(info.startup_wm_class.as_deref(), Some("firefox"));
        assert_eq!(info.actions.len(), 2);
        assert_eq!(info.actions[0].id, "new-window");
        assert_eq!(info.actions[0].name, "Open a New Window");
        assert_eq!(info.actions[1].id, "new-private-window");

        let (new_win, rest) = split_new_window_action(&info.actions);
        assert_eq!(new_win.map(|a| a.id.as_str()), Some("new-window"));
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].id, "new-private-window");
    }

    #[test]
    fn resolves_startup_wm_class_and_reverse_dns_and_fallback() {
        let entries = vec![
            parse_desktop_file_str("org.mozilla.firefox", FIREFOX_DESKTOP, &[]).unwrap(),
            parse_desktop_file_str("code", CODE_DESKTOP, &[]).unwrap(),
            parse_desktop_file_str("com.system76.CosmicFiles", FILES_DESKTOP, &[]).unwrap(),
        ];

        // StartupWMClass match (case-insensitive)
        let code = resolve_active_app("code", "main.rs - Visual Studio Code", &entries, "Files");
        assert_eq!(code.display_name, "Visual Studio Code");

        // Reverse-DNS match (short app_id -> reverse-DNS desktop file)
        let ff_short = resolve_active_app("firefox", "Mozilla Firefox", &entries, "Files");
        assert_eq!(ff_short.display_name, "Firefox");

        // Reverse-DNS match (reverse-DNS app_id -> exact or normalized match)
        let files = resolve_active_app("cosmic-files", "Home", &entries, "Files");
        assert_eq!(files.display_name, "Files");

        // Fallback when no window is focused -> "Files"
        let empty = resolve_active_app("", "", &entries, "Files");
        assert_eq!(empty.display_name, "Files");
        assert_eq!(
            empty.desktop.as_ref().map(|d| d.id.as_str()),
            Some("com.system76.CosmicFiles")
        );

        // Fallback to window title for unknown app
        let unknown = resolve_active_app("org.example.Mystery", "Mystery App", &entries, "Files");
        assert_eq!(unknown.display_name, "Mystery App");
        assert!(unknown.desktop.is_none());
    }

    #[test]
    fn elides_long_app_names_for_220px_pill() {
        assert_eq!(elide_to_max_width("Files", 220.0), "Files");
        let long = "Very Long Enterprise Application Name That Exceeds Width";
        let elided = elide_to_max_width(long, 220.0);
        assert!(elided.ends_with('…'));
        assert!(elided.len() < long.len());
    }
}
