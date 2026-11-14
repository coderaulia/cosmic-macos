// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zbus::zvariant::Type;

/// Item row returned by `dev.asw.MacClipboard.List` or read from `index.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
pub struct ClipboardItemSummary {
    pub id: String,
    pub kind: String,
    pub preview: String,
    pub thumb_path: String,
    pub size_bytes: u64,
    pub created_unix: i64,
    pub pinned: bool,
    pub source_app: String,
}

#[zbus::proxy(
    interface = "dev.asw.MacClipboard",
    default_service = "dev.asw.MacClipboard",
    default_path = "/dev/asw/MacClipboard"
)]
pub trait MacClipboard {
    fn list(&self, filter: &str, query: &str, limit: u32) -> zbus::Result<Vec<ClipboardItemSummary>>;

    fn activate(&self, id: &str, plain: bool, paste: bool) -> zbus::Result<bool>;

    fn pin(&self, id: &str, pinned: bool) -> zbus::Result<()>;

    fn remove(&self, id: &str) -> zbus::Result<()>;

    fn clear(&self, keep_pinned: bool) -> zbus::Result<u32>;

    fn set_paused(&self, paused: bool) -> zbus::Result<()>;

    fn toggle(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn paused(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn count(&self) -> zbus::Result<u32>;

    #[zbus(signal)]
    fn changed(&self, revision: u64) -> zbus::Result<()>;
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub items: Vec<ClipboardItemSummary>,
    pub paused: bool,
    pub daemon_available: bool,
}

pub fn resolve_cli_binary(home: Option<&Path>) -> PathBuf {
    if let Some(home) = home {
        let local = home.join(".local/bin/mac-clipboard");
        if local.is_file() {
            return local;
        }
    }
    PathBuf::from("mac-clipboard")
}

pub fn store_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))
        })
        .map(|d| d.join("mac-clipboard"))
}

#[derive(Debug, Deserialize)]
struct FallbackIndex {
    #[serde(default)]
    items: Vec<FallbackItem>,
}

#[derive(Debug, Deserialize)]
struct FallbackItem {
    id: String,
    #[serde(default)]
    kind: serde_json::Value,
    #[serde(default)]
    preview: String,
    #[serde(default)]
    hash: String,
    #[serde(default)]
    size_bytes: u64,
    #[serde(default)]
    created: i64,
    #[serde(default)]
    last_used: i64,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    source_app: Option<String>,
}

/// Fallback loader reading `~/.local/share/mac-clipboard/index.json` when the
/// D-Bus service is unreachable.
pub fn load_store_fallback(dir: &Path, query: &str, limit: usize) -> Vec<ClipboardItemSummary> {
    let index_path = dir.join("index.json");
    let Ok(raw) = std::fs::read_to_string(index_path) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<FallbackIndex>(&raw) else {
        return Vec::new();
    };
    let q = query.trim().to_lowercase();
    let thumbs_dir = dir.join("thumbs");
    parsed
        .items
        .into_iter()
        .map(|item| {
            let kind = match &item.kind {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Object(map) => {
                    map.keys().next().cloned().unwrap_or_else(|| "Text".into())
                }
                _ => "Text".into(),
            };
            let thumb_path = if kind.eq_ignore_ascii_case("image") && !item.hash.is_empty() {
                let p = thumbs_dir.join(format!("{}.png", item.hash));
                if p.is_file() {
                    p.to_string_lossy().into_owned()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            ClipboardItemSummary {
                id: item.id,
                kind,
                preview: item.preview,
                thumb_path,
                size_bytes: item.size_bytes,
                created_unix: item.last_used.max(item.created),
                pinned: item.pinned,
                source_app: item.source_app.unwrap_or_default(),
            }
        })
        .filter(|item| {
            q.is_empty()
                || item.preview.to_lowercase().contains(&q)
                || item.source_app.to_lowercase().contains(&q)
                || item.kind.to_lowercase().contains(&q)
        })
        .take(limit)
        .collect()
}

pub async fn fetch_snapshot(query: &str, limit: u32) -> Snapshot {
    if let Ok(conn) = zbus::Connection::session().await
        && let Ok(proxy) = MacClipboardProxy::new(&conn).await
        && let Ok(items) = proxy.list("all", query, limit).await
    {
        let paused = proxy.paused().await.unwrap_or(false);
        return Snapshot {
            items,
            paused,
            daemon_available: true,
        };
    }

    let items = store_dir()
        .map(|dir| load_store_fallback(&dir, query, limit as usize))
        .unwrap_or_default();
    Snapshot {
        items,
        paused: false,
        daemon_available: false,
    }
}

pub async fn activate_item(id: String, plain: bool, paste: bool) {
    if let Ok(conn) = zbus::Connection::session().await
        && let Ok(proxy) = MacClipboardProxy::new(&conn).await
        && proxy.activate(&id, plain, paste).await.is_ok()
    {
        return;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let bin = resolve_cli_binary(home.as_deref());
    let mut cmd = std::process::Command::new(bin);
    cmd.arg("copy").arg(&id);
    if plain {
        cmd.arg("--plain");
    }
    let _ = cmd.status();
}

pub async fn pin_item(id: String, pinned: bool) {
    if let Ok(conn) = zbus::Connection::session().await
        && let Ok(proxy) = MacClipboardProxy::new(&conn).await
        && proxy.pin(&id, pinned).await.is_ok()
    {
        return;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let bin = resolve_cli_binary(home.as_deref());
    let _ = std::process::Command::new(bin).args(["pin", &id]).status();
}

pub async fn remove_item(id: String) {
    if let Ok(conn) = zbus::Connection::session().await
        && let Ok(proxy) = MacClipboardProxy::new(&conn).await
        && proxy.remove(&id).await.is_ok()
    {
        return;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let bin = resolve_cli_binary(home.as_deref());
    let _ = std::process::Command::new(bin).args(["rm", &id]).status();
}

pub async fn clear_history(keep_pinned: bool) {
    if let Ok(conn) = zbus::Connection::session().await
        && let Ok(proxy) = MacClipboardProxy::new(&conn).await
        && proxy.clear(keep_pinned).await.is_ok()
    {
        return;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let bin = resolve_cli_binary(home.as_deref());
    let _ = std::process::Command::new(bin).arg("clear").status();
}

pub async fn set_paused(paused: bool) {
    if let Ok(conn) = zbus::Connection::session().await
        && let Ok(proxy) = MacClipboardProxy::new(&conn).await
        && proxy.set_paused(paused).await.is_ok()
    {
        return;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let bin = resolve_cli_binary(home.as_deref());
    let _ = std::process::Command::new(bin).arg("pause").status();
}

pub async fn toggle_quick_paste() {
    if let Ok(conn) = zbus::Connection::session().await
        && let Ok(proxy) = MacClipboardProxy::new(&conn).await
        && proxy.toggle().await.is_ok()
    {
        return;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let bin = resolve_cli_binary(home.as_deref());
    let _ = std::process::Command::new(bin).arg("toggle").spawn();
}

pub fn spawn_settings() {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let bin = resolve_cli_binary(home.as_deref());
    let _ = std::process::Command::new(bin).arg("settings").spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn fallback_store_loads_and_filters_items() {
        let dir = std::env::temp_dir().join(format!("clip_fallback_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("thumbs")).unwrap();
        fs::write(dir.join("thumbs/abc.png"), b"png").unwrap();
        let json = r#"{
            "items": [
                {
                    "id": "1",
                    "kind": "Text",
                    "preview": "ssh deploy@host",
                    "hash": "h1",
                    "size_bytes": 15,
                    "created": 100,
                    "last_used": 200,
                    "pinned": true,
                    "source_app": "cosmic-term"
                },
                {
                    "id": "2",
                    "kind": "Image",
                    "preview": "Screenshot 1920x1080",
                    "hash": "abc",
                    "size_bytes": 4096,
                    "created": 150,
                    "last_used": 150,
                    "pinned": false,
                    "source_app": "cosmic-screenshot"
                }
            ]
        }"#;
        fs::write(dir.join("index.json"), json).unwrap();

        let all = load_store_fallback(&dir, "", 10);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "1");
        assert!(all[0].pinned);
        assert!(all[1].thumb_path.ends_with("abc.png"));

        let filtered = load_store_fallback(&dir, "deploy", 10);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "1");

        let _ = fs::remove_dir_all(&dir);
    }
}
