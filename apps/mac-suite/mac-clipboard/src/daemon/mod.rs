// SPDX-License-Identifier: GPL-3.0-only

//! Background clipboard daemon: Wayland watcher, owner, auto-paste, and D-Bus service.

pub mod owner;
pub mod watcher;
pub mod wayland;

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, interface};

use crate::config::ClipboardConfig;
use crate::daemon::owner::offer_item;
use crate::daemon::watcher::{DaemonShared, spawn_watcher};
use crate::daemon::wayland::WaylandHandle;
use crate::dbus::{BUS_NAME, DbusListItem, OBJECT_PATH, close_overlay_if_open, is_overlay_open};
use crate::filter::TransientClearGuard;
use crate::search::{FilterPill, search_items};
use crate::store::{ClipboardStore, now_unix_ms};

struct MacClipboardService {
    shared: Arc<Mutex<DaemonShared>>,
    wayland: WaylandHandle,
    notify_tx: tokio::sync::mpsc::UnboundedSender<u64>,
    conn: Connection,
}

impl MacClipboardService {
    fn bump_revision(&self) {
        if let Ok(mut state) = self.shared.lock() {
            state.revision = state.revision.saturating_add(1);
            let rev = state.revision;
            drop(state);
            let _ = self.notify_tx.send(rev);
        }
    }
}

#[interface(name = "dev.asw.MacClipboard")]
impl MacClipboardService {
    async fn list(
        &self,
        filter: &str,
        query: &str,
        limit: u32,
    ) -> zbus::fdo::Result<Vec<DbusListItem>> {
        let Ok(mut state) = self.shared.lock() else {
            return Err(zbus::fdo::Error::Failed("state lock poisoned".into()));
        };
        let now_ms = now_unix_ms();
        let cfg = state.config.clone();
        let before = state.store.len();
        let _ = state.store.enforce_limits(now_ms, &cfg);
        if state.store.len() != before {
            let _ = state.store.save_index();
        }

        let pill = FilterPill::from_filter_str(filter);
        let hits = search_items(state.store.items(), pill, query, limit as usize);
        let out = hits
            .into_iter()
            .map(|hit| {
                let item = hit.item;
                let thumb = state
                    .store
                    .thumbnail_path(item)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                DbusListItem {
                    id: item.id.clone(),
                    kind: item.kind.as_str().to_string(),
                    preview: item.preview.clone(),
                    thumbnail_path: thumb,
                    size_bytes: item.size_bytes,
                    created: item.created,
                    pinned: item.pinned,
                    source_app: item.source_app.clone().unwrap_or_default(),
                }
            })
            .collect();
        Ok(out)
    }

    async fn get(&self, id: &str) -> zbus::fdo::Result<HashMap<String, OwnedValue>> {
        let Ok(state) = self.shared.lock() else {
            return Err(zbus::fdo::Error::Failed("state lock poisoned".into()));
        };
        let item = state
            .store
            .get(id)
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
            .clone();

        let mut map = HashMap::new();
        insert_val(&mut map, "id", item.id.as_str());
        insert_val(&mut map, "kind", item.kind.as_str());
        insert_val(&mut map, "preview", item.preview.as_str());
        insert_val(&mut map, "size", item.size_bytes);
        insert_val(&mut map, "created", item.created);
        insert_val(&mut map, "last_used", item.last_used);
        insert_val(&mut map, "use_count", item.use_count);
        insert_val(&mut map, "pinned", item.pinned);
        insert_val(
            &mut map,
            "source_app",
            item.source_app.as_deref().unwrap_or(""),
        );
        insert_val(&mut map, "mime_types", item.mime_types.clone());
        insert_val(&mut map, "masked_secret", item.masked_secret);

        let thumb = state
            .store
            .thumbnail_path(&item)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        insert_val(&mut map, "thumbnail_path", thumb);

        if let Some((w, h)) = item.image_dims {
            insert_val(&mut map, "width", w);
            insert_val(&mut map, "height", h);
        }

        if let Ok(Some(text)) = state.store.read_plain_text(&item) {
            insert_val(&mut map, "text", text);
        }
        if let Ok(Some(html)) = state.store.read_html(&item) {
            insert_val(&mut map, "html", html);
        }
        if let Some(files) = &item.files_meta {
            insert_val(&mut map, "paths", files.paths.clone());
            insert_val(&mut map, "cut", files.cut);
        }
        if !item.blob_refs.oversized && !item.blob_refs.primary.is_empty() {
            let blob = state.store.blob_path(&item.blob_refs.primary);
            insert_val(&mut map, "blob_path", blob.to_string_lossy().into_owned());
        }

        Ok(map)
    }

    async fn activate(&self, id: &str, plain: bool, paste: bool) -> zbus::fdo::Result<bool> {
        let (terminal_apps, auto_paste_enabled) = {
            let Ok(mut state) = self.shared.lock() else {
                return Err(zbus::fdo::Error::Failed("state lock poisoned".into()));
            };
            let now_ms = now_unix_ms();
            let item = state
                .store
                .mark_used(id, now_ms)
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            offer_item(&state.store, &item, plain)
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            (
                state.config.terminal_apps.clone(),
                state.config.auto_paste,
            )
        };

        self.bump_revision();

        if paste && auto_paste_enabled {
            let wayland = self.wayland.clone();
            let pasted = tokio::task::spawn_blocking(move || wayland.trigger_paste(&terminal_apps))
                .await
                .unwrap_or(false);
            Ok(pasted)
        } else {
            Ok(false)
        }
    }

    async fn pin(&self, id: &str, pinned: bool) -> zbus::fdo::Result<()> {
        {
            let Ok(mut state) = self.shared.lock() else {
                return Err(zbus::fdo::Error::Failed("state lock poisoned".into()));
            };
            let max_pinned = state.config.max_pinned;
            state
                .store
                .pin(id, pinned, max_pinned)
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        }
        self.bump_revision();
        Ok(())
    }

    async fn remove(&self, id: &str) -> zbus::fdo::Result<()> {
        {
            let Ok(mut state) = self.shared.lock() else {
                return Err(zbus::fdo::Error::Failed("state lock poisoned".into()));
            };
            state
                .store
                .remove(id)
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        }
        self.bump_revision();
        Ok(())
    }

    async fn clear(&self, keep_pinned: bool) -> zbus::fdo::Result<u32> {
        let removed = {
            let Ok(mut state) = self.shared.lock() else {
                return Err(zbus::fdo::Error::Failed("state lock poisoned".into()));
            };
            state
                .store
                .clear(keep_pinned)
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
        };
        self.bump_revision();
        Ok(removed as u32)
    }

    async fn set_paused(
        &self,
        paused: bool,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        {
            let Ok(mut state) = self.shared.lock() else {
                return Err(zbus::fdo::Error::Failed("state lock poisoned".into()));
            };
            state.config.paused = paused;
            state.config.paused_until_unix = None;
            let _ = state.config.save();
        }
        let _ = self.paused_changed(&emitter).await;
        self.bump_revision();
        Ok(())
    }

    async fn toggle(&self) -> zbus::fdo::Result<()> {
        if close_overlay_if_open(&self.conn).await {
            return Ok(());
        }
        spawn_overlay_show().map_err(zbus::fdo::Error::Failed)
    }

    async fn show(&self) -> zbus::fdo::Result<()> {
        if is_overlay_open(&self.conn).await {
            return Ok(());
        }
        spawn_overlay_show().map_err(zbus::fdo::Error::Failed)
    }

    async fn hide(&self) -> zbus::fdo::Result<()> {
        let _ = close_overlay_if_open(&self.conn).await;
        Ok(())
    }

    #[zbus(signal)]
    async fn changed(emitter: &SignalEmitter<'_>, revision: u64) -> zbus::Result<()>;

    #[zbus(property)]
    async fn paused(&self) -> bool {
        let now_ms = now_unix_ms();
        self.shared
            .lock()
            .map(|s| s.config.is_paused_at(now_ms))
            .unwrap_or(false)
    }

    #[zbus(property)]
    async fn count(&self) -> u32 {
        self.shared
            .lock()
            .map(|s| s.store.len() as u32)
            .unwrap_or(0)
    }
}

fn insert_val<'a, V>(map: &mut HashMap<String, OwnedValue>, key: &str, val: V)
where
    V: Into<Value<'a>>,
{
    if let Ok(owned) = OwnedValue::try_from(val.into()) {
        map.insert(key.to_string(), owned);
    }
}

fn spawn_overlay_show() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Command::new(exe)
        .arg("show")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Runs the clipboard daemon until `SIGTERM`/`SIGINT` or bus loss.
pub async fn run_daemon() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = ClipboardConfig::load();
    let mut store = ClipboardStore::open(ClipboardStore::default_data_dir())?;
    let now_ms = now_unix_ms();
    if config.paused
        && let Some(until) = config.paused_until_unix
        && now_ms >= until
    {
        config.paused = false;
        config.paused_until_unix = None;
    }
    store.enforce_limits(now_ms, &config)?;
    store.save_index()?;

    let shared = Arc::new(Mutex::new(DaemonShared {
        store,
        config,
        guard: TransientClearGuard::new(),
        revision: 1,
    }));

    let wayland = WaylandHandle::start();
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();

    let conn = zbus::Connection::session().await?;
    let service = MacClipboardService {
        shared: Arc::clone(&shared),
        wayland: wayland.clone(),
        notify_tx: notify_tx.clone(),
        conn: conn.clone(),
    };
    conn.object_server().at(OBJECT_PATH, service).await?;
    conn.request_name(BUS_NAME).await?;

    let cancel_handle = spawn_watcher(Arc::clone(&shared), wayland, notify_tx);

    // Periodically expire timed items/pauses and flush Changed signals.
    let conn_clone = conn.clone();
    let shared_clone = Arc::clone(&shared);
    let signal_task = tokio::spawn(async move {
        let iface_ref = conn_clone
            .object_server()
            .interface::<_, MacClipboardService>(OBJECT_PATH)
            .await
            .ok();
        let mut ticker = tokio::time::interval(Duration::from_secs(15));
        loop {
            tokio::select! {
                Some(rev) = notify_rx.recv() => {
                    if let Some(ref iface) = iface_ref {
                        let emitter = iface.signal_emitter();
                        let _ = MacClipboardService::changed(emitter, rev).await;
                        let svc = iface.get().await;
                        let _ = svc.count_changed(emitter).await;
                    }
                }
                _ = ticker.tick() => {
                    let now_ms = now_unix_ms();
                    let changed = if let Ok(mut state) = shared_clone.lock() {
                        let cfg = state.config.clone();
                        let before = state.store.len();
                        let _ = state.store.enforce_limits(now_ms, &cfg);
                        if state.store.len() != before {
                            let _ = state.store.save_index();
                            state.revision = state.revision.saturating_add(1);
                            Some(state.revision)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let (Some(rev), Some(iface)) = (changed, iface_ref.as_ref()) {
                        let emitter = iface.signal_emitter();
                        let _ = MacClipboardService::changed(emitter, rev).await;
                        let svc = iface.get().await;
                        let _ = svc.count_changed(emitter).await;
                    }
                }
            }
        }
    });

    let _ = tokio::signal::ctrl_c().await;
    if let Some(cancel) = cancel_handle {
        cancel.cancel();
    }
    signal_task.abort();
    Ok(())
}
