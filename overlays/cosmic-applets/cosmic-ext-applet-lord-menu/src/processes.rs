// SPDX-License-Identifier: GPL-3.0-only

//! Process lookup for Force Quit and recent items parsing.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use rustix::process::{Pid, Signal, getuid, kill_process};

/// Snapshot of a user-owned process read from `/proc/<pid>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcEntry {
    pub pid: u32,
    pub comm: String,
    pub cmdline: String,
    pub cgroup: String,
}

/// Recent item entry shown in the "Recent Items" submenu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentItem {
    pub label: String,
    pub target: RecentTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecentTarget {
    /// Desktop application ID or Exec command.
    AppExec {
        exec: String,
        terminal: bool,
    },
    /// File/document URI or path opened via `xdg-open`.
    Uri(String),
}

/// Scan `/proc` for processes owned by the current user.
#[must_use]
pub fn list_user_processes() -> Vec<ProcEntry> {
    let uid = getuid().as_raw();
    let self_pid = std::process::id();
    let mut out = Vec::new();

    let Ok(entries) = fs::read_dir("/proc") else {
        return out;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid_str) = name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        let path = entry.path();
        if let Ok(status) = fs::read_to_string(path.join("status")) {
            let owned = status
                .lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|u| u.parse::<u32>().ok())
                .is_some_and(|proc_uid| proc_uid == uid);
            if !owned {
                continue;
            }
        } else {
            continue;
        }

        let comm = fs::read_to_string(path.join("comm"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let cmdline = fs::read(path.join("cmdline"))
            .map(|bytes| {
                bytes
                    .split(|&b| b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let cgroup = fs::read_to_string(path.join("cgroup"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        out.push(ProcEntry {
            pid,
            comm,
            cmdline,
            cgroup,
        });
    }

    out
}

fn normalize_token(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Match user process PIDs belonging to a Wayland `app_id` and optional desktop `Exec` binary name.
#[must_use]
pub fn match_pid_candidates(
    app_id: &str,
    exec_name: Option<&str>,
    procs: &[ProcEntry],
) -> Vec<u32> {
    let mut keys: Vec<String> = Vec::new();
    let app_trimmed = app_id.trim();
    if !app_trimmed.is_empty() {
        keys.push(app_trimmed.to_ascii_lowercase());
        if let Some(last) = app_trimmed.rsplit('.').next()
            && !last.is_empty()
        {
            keys.push(last.to_ascii_lowercase());
        }
        let norm = normalize_token(app_trimmed);
        if !norm.is_empty() {
            keys.push(norm);
        }
        if let Some(last) = app_trimmed.rsplit('.').next() {
            let norm_last = normalize_token(last);
            if !norm_last.is_empty() {
                keys.push(norm_last);
            }
        }
    }
    if let Some(exec) = exec_name {
        let base = exec
            .split_whitespace()
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim();
        if !base.is_empty() {
            keys.push(base.to_ascii_lowercase());
            keys.push(normalize_token(base));
        }
    }

    let mut matched = Vec::new();
    let mut seen = HashSet::new();

    for proc in procs {
        let cmd_bin = proc
            .cmdline
            .split_whitespace()
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("");
        let cgroup_lower = proc.cgroup.to_ascii_lowercase();
        let comm_lower = proc.comm.to_ascii_lowercase();
        let cmd_bin_lower = cmd_bin.to_ascii_lowercase();
        let cgroup_norm = normalize_token(&proc.cgroup);

        let is_match = keys.iter().any(|key| {
            if key.len() < 2 {
                return false;
            }
            // Match systemd cgroup scope (e.g. app-cosmic-com.system76.CosmicFiles-1234.scope)
            if !cgroup_lower.is_empty()
                && (cgroup_lower.contains(&format!("-{key}-"))
                    || cgroup_lower.contains(&format!("-{key}.scope"))
                    || (key.len() >= 4 && cgroup_norm.contains(key)))
            {
                return true;
            }
            // Match process comm or executable basename
            comm_lower == *key
                || cmd_bin_lower == *key
                || normalize_token(&proc.comm) == *key
                || normalize_token(cmd_bin) == *key
        });

        if is_match && seen.insert(proc.pid) {
            matched.push(proc.pid);
        }
    }

    matched
}

/// Send `SIGKILL` to each PID. Returns the number of processes signaled successfully.
pub fn force_quit_pids(pids: &[u32]) -> usize {
    let mut killed = 0;
    for &raw_pid in pids {
        let Ok(raw_i32) = i32::try_from(raw_pid) else {
            continue;
        };
        let Some(pid) = Pid::from_raw(raw_i32) else {
            continue;
        };
        if kill_process(pid, Signal::KILL).is_ok() {
            killed += 1;
        }
    }
    killed
}

/// Parse `~/.local/share/recently-used.xbel` into up to `max_items` recent file entries.
#[must_use]
pub fn parse_recently_used_xbel(xml: &str, max_items: usize) -> Vec<RecentItem> {
    let mut entries: Vec<(String, String, String)> = Vec::new();
    let mut search_from = 0;

    while let Some(rel_start) = xml[search_from..].find("<bookmark ") {
        let tag_start = search_from + rel_start;
        let Some(rel_end) = xml[tag_start..].find('>') else {
            break;
        };
        let tag = &xml[tag_start..tag_start + rel_end];
        search_from = tag_start + rel_end + 1;

        let Some(href) = extract_attr(tag, "href") else {
            continue;
        };
        let timestamp = extract_attr(tag, "visited")
            .or_else(|| extract_attr(tag, "modified"))
            .or_else(|| extract_attr(tag, "added"))
            .unwrap_or_default();

        let label = display_label_from_uri(&href);
        if !label.is_empty() {
            entries.push((timestamp, label, href));
        }
    }

    // Sort newest timestamp first (ISO-8601 UTC strings compare lexicographically).
    entries.sort_by(|a, b| b.0.cmp(&a.0));

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (_, label, href) in entries {
        if seen.insert(href.clone()) {
            out.push(RecentItem {
                label,
                target: RecentTarget::Uri(href),
            });
            if out.len() >= max_items {
                break;
            }
        }
    }
    out
}

fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(xml_unescape(&rest[..end]))
}

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn display_label_from_uri(uri: &str) -> String {
    let trimmed = uri.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    percent_decode(last)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Load up to `max_items` recent items from `$XDG_DATA_HOME/recently-used.xbel`.
#[must_use]
pub fn load_recent_xbel_items(max_items: usize) -> Vec<RecentItem> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    let Some(path) = base.map(|b| b.join("recently-used.xbel")) else {
        return Vec::new();
    };
    let Ok(xml) = fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_recently_used_xbel(&xml, max_items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_pids_by_cgroup_comm_and_exec() {
        let procs = vec![
            ProcEntry {
                pid: 101,
                comm: "cosmic-files".to_string(),
                cmdline: "/usr/bin/cosmic-files".to_string(),
                cgroup: "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-cosmic-com.system76.CosmicFiles-101.scope".to_string(),
            },
            ProcEntry {
                pid: 202,
                comm: "Isolated Web Co".to_string(),
                cmdline: "/usr/lib/firefox/firefox -contentproc".to_string(),
                cgroup: "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-org.mozilla.firefox-200.scope".to_string(),
            },
            ProcEntry {
                pid: 303,
                comm: "bash".to_string(),
                cmdline: "/bin/bash".to_string(),
                cgroup: "0::/user.slice/user-1000.slice/session-1.scope".to_string(),
            },
        ];

        assert_eq!(
            match_pid_candidates("com.system76.CosmicFiles", Some("cosmic-files %U"), &procs),
            vec![101]
        );
        assert_eq!(
            match_pid_candidates("firefox", Some("firefox %u"), &procs),
            vec![202]
        );
    }

    #[test]
    fn parses_recently_used_xbel_sorted_newest_first() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0">
  <bookmark href="file:///home/asw/Documents/Old%20Note.md" added="2026-11-10T10:00:00Z" visited="2026-11-10T10:00:00Z"/>
  <bookmark href="file:///home/asw/Projects/report%20%26%20plan.pdf" added="2026-11-14T18:00:00Z" visited="2026-11-14T19:00:00Z"/>
</xbel>"#;
        let items = parse_recently_used_xbel(xml, 10);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "report & plan.pdf");
        assert_eq!(items[1].label, "Old Note.md");
    }
}
