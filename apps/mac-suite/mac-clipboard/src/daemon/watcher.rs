// SPDX-License-Identifier: GPL-3.0-only

//! Clipboard watcher thread using `wl_clipboard_rs::watch::Watcher`
//! with 2 s MIME read timeouts, size caps, sensitive filtering, and keep-alive.

use std::io::Read;
use std::os::fd::AsFd;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use wl_clipboard_rs::paste::{self, Seat};
use wl_clipboard_rs::watch::{CancelHandle, ClipboardEvent, ClipboardType, Offer, Watcher};

use crate::config::ClipboardConfig;
use crate::daemon::owner::offer_item;
use crate::daemon::wayland::WaylandHandle;
use crate::filter::{OfferFilterDecision, TransientClearGuard, filter_offer};
use crate::model::{
    ClipboardItem, FilesMeta, ItemKind, MimePlan, parse_file_uri_list, parse_gnome_copied_files,
    select_mime_plan,
};
use crate::store::{ClipboardStore, StoreError, now_unix_ms};

pub const READ_TIMEOUT: Duration = Duration::from_secs(2);

pub struct DaemonShared {
    pub store: ClipboardStore,
    pub config: ClipboardConfig,
    pub guard: TransientClearGuard,
    pub revision: u64,
}

#[derive(Debug)]
pub enum ReadError {
    Timeout,
    TooLarge(usize),
    Failed(String),
}

pub trait OfferReader {
    fn read_mime(&mut self, mime: &str, max_bytes: usize) -> Result<Vec<u8>, ReadError>;
}

struct WlOfferReader<'a, 'b> {
    offer: &'a mut Offer<'b>,
}

impl OfferReader for WlOfferReader<'_, '_> {
    fn read_mime(&mut self, mime: &str, max_bytes: usize) -> Result<Vec<u8>, ReadError> {
        let mut reader = self
            .offer
            .receive(mime)
            .map_err(|e| ReadError::Failed(e.to_string()))?;

        if let Ok(flags) = fcntl_getfl(reader.as_fd()) {
            let _ = fcntl_setfl(reader.as_fd(), flags | OFlags::NONBLOCK);
        }

        let deadline = Instant::now() + READ_TIMEOUT;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];

        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(ReadError::Timeout);
            }
            let rem = deadline.saturating_duration_since(now);
            let ts = Timespec {
                tv_sec: rem.as_secs() as i64,
                tv_nsec: i64::from(rem.subsec_nanos()),
            };

            let mut fds = [PollFd::new(&reader, PollFlags::IN | PollFlags::HUP)];
            match poll(&mut fds, Some(&ts)) {
                Ok(0) => return Err(ReadError::Timeout),
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(ReadError::Failed(e.to_string())),
            }

            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => return Ok(buf),
                    Ok(n) => {
                        if buf.len().saturating_add(n) > max_bytes {
                            return Err(ReadError::TooLarge(buf.len().saturating_add(n)));
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(ReadError::Failed(e.to_string())),
                }
            }
        }
    }
}

/// Spawns the Wayland clipboard watcher thread and returns its `CancelHandle`.
pub fn spawn_watcher(
    shared: Arc<Mutex<DaemonShared>>,
    wayland: WaylandHandle,
    notify_tx: tokio::sync::mpsc::UnboundedSender<u64>,
) -> Option<CancelHandle> {
    let watch_primary = shared.lock().map(|s| s.config.watch_primary).unwrap_or(false);
    let mode = if watch_primary {
        ClipboardType::Both
    } else {
        ClipboardType::Regular
    };

    let watcher = match Watcher::new(mode, Seat::Unspecified) {
        Ok(w) => w,
        Err(paste::Error::PrimarySelectionUnsupported) if watch_primary => {
            tracing::warn!("primary selection unsupported by compositor, watching regular clipboard only");
            match Watcher::new(ClipboardType::Regular, Seat::Unspecified) {
                Ok(w) => w,
                Err(err) => {
                    tracing::error!("failed to start clipboard watcher: {err}");
                    return None;
                }
            }
        }
        Err(err) => {
            tracing::error!("failed to start clipboard watcher: {err}");
            return None;
        }
    };

    let cancel = watcher.cancel_handle();

    let _ = std::thread::Builder::new()
        .name("clipboard-watcher".to_string())
        .spawn(move || run_watcher_loop(watcher, shared, wayland, notify_tx));

    Some(cancel)
}

fn run_watcher_loop(
    mut watcher: Watcher,
    shared: Arc<Mutex<DaemonShared>>,
    wayland: WaylandHandle,
    notify_tx: tokio::sync::mpsc::UnboundedSender<u64>,
) {
    loop {
        match watcher.next_event() {
            Ok(Some(ClipboardEvent::Changed {
                clipboard,
                mime_types,
                mut offer,
            })) => {
                let _ = clipboard;
                let focused_app = wayland.focused_app();
                let now_ms = now_unix_ms();

                let (decision, cfg) = {
                    let Ok(mut state) = shared.lock() else {
                        continue;
                    };
                    // Expire timed pause if elapsed.
                    if state.config.paused
                        && let Some(until) = state.config.paused_until_unix
                        && now_ms >= until
                    {
                        state.config.paused = false;
                        state.config.paused_until_unix = None;
                    }
                    let paused = state.config.is_paused_at(now_ms);
                    let decision = filter_offer(
                        &mime_types,
                        focused_app.as_deref(),
                        &state.config.ignored_apps,
                        paused,
                    );
                    match decision {
                        OfferFilterDecision::Allow => {}
                        OfferFilterDecision::SkipSensitiveMime
                        | OfferFilterDecision::SkipIgnoredApp => {
                            state.guard.record_sensitive_or_ignored(Instant::now());
                        }
                        OfferFilterDecision::SkipSelf | OfferFilterDecision::SkipPaused => {}
                    }
                    (decision, state.config.clone())
                };

                if decision != OfferFilterDecision::Allow {
                    continue;
                }

                let mut reader = WlOfferReader { offer: &mut offer };
                let Some(plan) = select_mime_plan(&mime_types) else {
                    continue;
                };

                let mut state = match shared.lock() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                match ingest_mime_plan(
                    &mut reader,
                    &plan,
                    &mime_types,
                    focused_app,
                    now_ms,
                    &cfg,
                    &mut state.store,
                ) {
                    Ok(Some(_item)) => {
                        state.guard.record_allowed(Instant::now());
                        state.revision = state.revision.saturating_add(1);
                        let rev = state.revision;
                        drop(state);
                        let _ = notify_tx.send(rev);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        tracing::debug!("failed to ingest clipboard offer: {err}");
                    }
                }
            }
            Ok(Some(ClipboardEvent::Cleared { clipboard })) => {
                if clipboard != paste::ClipboardType::Regular {
                    continue;
                }
                let Ok(state) = shared.lock() else {
                    continue;
                };
                if state.config.keep_alive
                    && state.guard.allow_keep_alive(Instant::now())
                    && let Some(latest) = state.store.items().first()
                    && !latest.masked_secret
                    && !latest.blob_refs.oversized
                    && let Err(err) = offer_item(&state.store, latest, false)
                {
                    tracing::debug!("keep-alive re-offer skipped: {err}");
                }
            }
            Ok(None) => {
                tracing::info!("clipboard watcher stopped");
                break;
            }
            Err(err) => {
                tracing::error!("clipboard watcher error: {err}");
                break;
            }
        }
    }
}

pub fn ingest_mime_plan(
    reader: &mut dyn OfferReader,
    plan: &MimePlan,
    offered_mimes: &[String],
    source_app: Option<String>,
    now_ms: i64,
    cfg: &ClipboardConfig,
    store: &mut ClipboardStore,
) -> Result<Option<ClipboardItem>, StoreError> {
    match plan {
        MimePlan::GnomeFiles { mime } => {
            let bytes = match reader.read_mime(mime, cfg.max_text_bytes) {
                Ok(b) => b,
                Err(ReadError::TooLarge(sz)) => {
                    return record_oversized(store, ItemKind::Files, sz, source_app, now_ms, cfg);
                }
                Err(_) => return Ok(None),
            };
            let Some(files) = parse_gnome_copied_files(&bytes) else {
                return Ok(None);
            };
            Ok(Some(store.insert_files(files, source_app, now_ms, cfg)?))
        }
        MimePlan::UriList { uri_mime, fallback } => {
            if let Ok(bytes) = reader.read_mime(uri_mime, cfg.max_text_bytes)
                && let Some(paths) = parse_file_uri_list(&bytes)
            {
                let files = FilesMeta { paths, cut: false };
                return Ok(Some(store.insert_files(files, source_app, now_ms, cfg)?));
            }
            if let Some(fb) = fallback {
                return ingest_mime_plan(
                    reader,
                    fb,
                    offered_mimes,
                    source_app,
                    now_ms,
                    cfg,
                    store,
                );
            }
            Ok(None)
        }
        MimePlan::Image { mime } => {
            let bytes = match reader.read_mime(mime, cfg.max_image_bytes) {
                Ok(b) => b,
                Err(ReadError::TooLarge(sz)) => {
                    return record_oversized(store, ItemKind::Image, sz, source_app, now_ms, cfg);
                }
                Err(_) => return Ok(None),
            };
            match store.insert_image(&bytes, mime, source_app.clone(), now_ms, cfg) {
                Ok(item) => Ok(Some(item)),
                Err(StoreError::Image(crate::model::ImageProcessError::TooManyPixels(_, _, _))) => {
                    record_oversized(store, ItemKind::Image, bytes.len(), source_app, now_ms, cfg)
                }
                Err(e) => Err(e),
            }
        }
        MimePlan::RichText {
            html_mime,
            plain_mime,
        } => {
            let html_bytes = match reader.read_mime(html_mime, cfg.max_html_bytes) {
                Ok(b) => b,
                Err(ReadError::TooLarge(sz)) => {
                    return record_oversized(
                        store,
                        ItemKind::RichText,
                        sz,
                        source_app,
                        now_ms,
                        cfg,
                    );
                }
                Err(_) => return Ok(None),
            };
            let html = String::from_utf8_lossy(&html_bytes);
            let plain = plain_mime
                .as_deref()
                .and_then(|m| reader.read_mime(m, cfg.max_text_bytes).ok())
                .map(|b| String::from_utf8_lossy(&b).into_owned());
            Ok(Some(store.insert_rich_text(
                &html,
                plain.as_deref(),
                offered_mimes.to_vec(),
                source_app,
                now_ms,
                cfg,
            )?))
        }
        MimePlan::PlainText { mime } => {
            let bytes = match reader.read_mime(mime, cfg.max_text_bytes) {
                Ok(b) => b,
                Err(ReadError::TooLarge(sz)) => {
                    return record_oversized(store, ItemKind::Text, sz, source_app, now_ms, cfg);
                }
                Err(_) => return Ok(None),
            };
            let text = String::from_utf8_lossy(&bytes);
            if text.is_empty() {
                return Ok(None);
            }
            Ok(Some(store.insert_text(
                &text,
                offered_mimes.to_vec(),
                source_app,
                now_ms,
                cfg,
            )?))
        }
    }
}

fn record_oversized(
    store: &mut ClipboardStore,
    kind: ItemKind,
    byte_len: usize,
    source_app: Option<String>,
    now_ms: i64,
    cfg: &ClipboardConfig,
) -> Result<Option<ClipboardItem>, StoreError> {
    if !cfg.record_oversized_placeholder {
        return Ok(None);
    }
    Ok(Some(store.insert_oversized_placeholder(
        kind,
        byte_len,
        source_app,
        now_ms,
        cfg,
    )?))
}
