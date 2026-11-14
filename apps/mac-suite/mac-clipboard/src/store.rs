// SPDX-License-Identifier: GPL-3.0-only

//! Content-addressed clipboard history store with atomic index writes,
//! `.meta` sidecar recovery, ring-buffer eviction, and blob garbage collection.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::ClipboardConfig;
use crate::filter::detect_secret;
use crate::model::{
    BlobRefs, ClipboardItem, FilesMeta, GNOME_COPIED_FILES_MIME, HTML_MIME, ImageProcessError,
    ItemKind, URI_LIST_MIME, canonical_files_hash, canonical_rich_text_hash, canonical_text_hash,
    detect_single_url, html_to_plain_text, make_files_preview, make_text_preview,
    normalize_line_endings, process_image_bytes, sha256_hex,
};

const INDEX_VERSION: u32 = 1;
const PINS_VERSION: u32 = 1;

static ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("image error: {0}")]
    Image(#[from] ImageProcessError),
    #[error("item not found: {0}")]
    NotFound(String),
    #[error("ambiguous item id prefix: {0}")]
    AmbiguousId(String),
    #[error("pin limit ({0}) reached")]
    PinLimitReached(usize),
}

#[derive(Debug, Serialize, Deserialize)]
struct IndexFile {
    version: u32,
    items: Vec<ClipboardItem>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PinsFile {
    version: u32,
    pinned_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BlobMeta {
    id: String,
    kind: ItemKind,
    hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plain_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plain_blob_hash: Option<String>,
    mime_types: Vec<String>,
    size_bytes: u64,
    preview: String,
    created: i64,
    last_used: i64,
    use_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_dims: Option<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files_meta: Option<FilesMeta>,
    #[serde(default)]
    masked_secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
}

impl BlobMeta {
    fn from_item(item: &ClipboardItem) -> Self {
        Self {
            id: item.id.clone(),
            kind: item.kind,
            hash: item.hash.clone(),
            plain_hash: item.plain_hash.clone(),
            plain_blob_hash: item.blob_refs.plain_text.clone(),
            mime_types: item.mime_types.clone(),
            size_bytes: item.size_bytes,
            preview: item.preview.clone(),
            created: item.created,
            last_used: item.last_used,
            use_count: item.use_count,
            source_app: item.source_app.clone(),
            image_dims: item.image_dims,
            files_meta: item.files_meta.clone(),
            masked_secret: item.masked_secret,
            expires_at: item.expires_at,
        }
    }

    fn into_item(self, primary_blob_hash: String, pinned: bool) -> ClipboardItem {
        ClipboardItem {
            id: self.id,
            kind: self.kind,
            hash: self.hash,
            plain_hash: self.plain_hash,
            mime_types: self.mime_types,
            blob_refs: BlobRefs {
                primary: primary_blob_hash,
                plain_text: self.plain_blob_hash,
                oversized: false,
            },
            size_bytes: self.size_bytes,
            preview: self.preview,
            created: self.created,
            last_used: self.last_used,
            use_count: self.use_count,
            pinned,
            source_app: self.source_app,
            image_dims: self.image_dims,
            files_meta: self.files_meta,
            masked_secret: self.masked_secret,
            expires_at: self.expires_at,
        }
    }
}

pub struct ClipboardStore {
    root: PathBuf,
    items: Vec<ClipboardItem>,
    pinned_ids: Vec<String>,
}

impl ClipboardStore {
    /// Returns `~/.local/share/mac-clipboard` (respecting `XDG_DATA_HOME`).
    #[must_use]
    pub fn default_data_dir() -> PathBuf {
        let base = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))
            })
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        base.join("mac-clipboard")
    }

    /// Opens or initializes the store at `root` (`0700` permissions).
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref().to_path_buf();
        ensure_dir_0700(&root)?;
        ensure_dir_0700(&root.join("blobs"))?;
        ensure_dir_0700(&root.join("thumbs"))?;

        // Clean up leftover `.tmp` files from an interrupted atomic write.
        let index_tmp = root.join("index.json.tmp");
        if index_tmp.exists() {
            let _ = fs::remove_file(&index_tmp);
        }
        let pins_tmp = root.join("pins.json.tmp");
        if pins_tmp.exists() {
            let _ = fs::remove_file(&pins_tmp);
        }

        let mut pinned_ids = load_pins(&root.join("pins.json"));
        let index_path = root.join("index.json");

        let mut items = if index_path.exists() {
            match fs::read(&index_path)
                .map_err(StoreError::from)
                .and_then(|bytes| serde_json::from_slice::<IndexFile>(&bytes).map_err(StoreError::from))
            {
                Ok(parsed) => parsed.items,
                Err(_) => {
                    let broken_name = format!("index.json.broken-{}", unix_secs_now());
                    let broken_path = root.join(broken_name);
                    let _ = fs::rename(&index_path, &broken_path);
                    let recovered = recover_from_sidecars(&root, &pinned_ids)?;
                    let mut store = Self {
                        root: root.clone(),
                        items: recovered,
                        pinned_ids: pinned_ids.clone(),
                    };
                    store.sync_pins_from_items();
                    store.save_index()?;
                    store.save_pins()?;
                    return Ok(store);
                }
            }
        } else {
            let recovered = recover_from_sidecars(&root, &pinned_ids)?;
            if !recovered.is_empty() {
                let mut store = Self {
                    root: root.clone(),
                    items: recovered,
                    pinned_ids: pinned_ids.clone(),
                };
                store.sync_pins_from_items();
                store.save_index()?;
                return Ok(store);
            }
            Vec::new()
        };

        // Reconcile pinned flags with `pins.json`.
        let pin_set: HashSet<&str> = pinned_ids.iter().map(String::as_str).collect();
        if !pin_set.is_empty() {
            for item in &mut items {
                if pin_set.contains(item.id.as_str()) {
                    item.pinned = true;
                }
            }
        }
        for item in &items {
            if item.pinned && !pinned_ids.contains(&item.id) {
                pinned_ids.push(item.id.clone());
            }
        }

        Ok(Self {
            root,
            items,
            pinned_ids,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn items(&self) -> &[ClipboardItem] {
        &self.items
    }

    #[must_use]
    pub fn pinned_ids(&self) -> &[String] {
        &self.pinned_ids
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn blob_path(&self, hash: &str) -> PathBuf {
        let prefix = hash.get(..2).unwrap_or("00");
        self.root.join("blobs").join(prefix).join(hash)
    }

    #[must_use]
    pub fn meta_path(&self, hash: &str) -> PathBuf {
        let prefix = hash.get(..2).unwrap_or("00");
        self.root
            .join("blobs")
            .join(prefix)
            .join(format!("{hash}.meta"))
    }

    #[must_use]
    pub fn thumbnail_path(&self, item: &ClipboardItem) -> Option<PathBuf> {
        let hash = item.thumbnail_hash()?;
        let path = self.root.join("thumbs").join(format!("{hash}.png"));
        path.exists().then_some(path)
    }

    #[must_use]
    pub fn thumbnail_path_for_hash(&self, hash: &str) -> PathBuf {
        self.root.join("thumbs").join(format!("{hash}.png"))
    }

    /// Reads raw bytes for a blob hash.
    pub fn read_blob(&self, hash: &str) -> Result<Vec<u8>, StoreError> {
        Ok(fs::read(self.blob_path(hash))?)
    }

    /// Reads the plain text representation for an item (works for Text, Link, RichText).
    pub fn read_plain_text(&self, item: &ClipboardItem) -> Result<Option<String>, StoreError> {
        if item.blob_refs.oversized {
            return Ok(Some(item.preview.clone()));
        }
        match item.kind {
            ItemKind::Text | ItemKind::Link => {
                let bytes = self.read_blob(&item.blob_refs.primary)?;
                Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
            }
            ItemKind::RichText => {
                if let Some(plain_hash) = item.blob_refs.plain_text.as_deref() {
                    let bytes = self.read_blob(plain_hash)?;
                    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
                } else {
                    let html_bytes = self.read_blob(&item.blob_refs.primary)?;
                    let html = String::from_utf8_lossy(&html_bytes);
                    Ok(Some(html_to_plain_text(&html)))
                }
            }
            ItemKind::Files => Ok(item
                .files_meta
                .as_ref()
                .map(|f| f.paths.join("\n"))),
            ItemKind::Image => Ok(None),
        }
    }

    /// Reads HTML bytes for a `RichText` item.
    pub fn read_html(&self, item: &ClipboardItem) -> Result<Option<String>, StoreError> {
        if item.kind != ItemKind::RichText || item.blob_refs.oversized {
            return Ok(None);
        }
        let bytes = self.read_blob(&item.blob_refs.primary)?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// Resolves an item index by exact id or unique id prefix.
    pub fn resolve_index(&self, id_or_prefix: &str) -> Result<usize, StoreError> {
        if let Some(idx) = self.items.iter().position(|i| i.id == id_or_prefix) {
            return Ok(idx);
        }
        let matches: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.id.starts_with(id_or_prefix))
            .map(|(idx, _)| idx)
            .collect();
        match matches.as_slice() {
            [] => Err(StoreError::NotFound(id_or_prefix.to_string())),
            [only] => Ok(*only),
            _ => Err(StoreError::AmbiguousId(id_or_prefix.to_string())),
        }
    }

    pub fn get(&self, id_or_prefix: &str) -> Result<&ClipboardItem, StoreError> {
        let idx = self.resolve_index(id_or_prefix)?;
        Ok(&self.items[idx])
    }

    /// Inserts plain text (or upgrades to `Link` when it is a single URL).
    pub fn insert_text(
        &mut self,
        raw_text: &str,
        mime_types: Vec<String>,
        source_app: Option<String>,
        now_ms: i64,
        cfg: &ClipboardConfig,
    ) -> Result<ClipboardItem, StoreError> {
        let normalized = normalize_line_endings(raw_text);
        let canonical = canonical_text_hash(&normalized);

        if let Some(idx) = self.items.iter().position(|item| item.hash == canonical) {
            return self.bump_existing(idx, source_app, now_ms, cfg);
        }

        let kind = if detect_single_url(&normalized).is_some() {
            ItemKind::Link
        } else {
            ItemKind::Text
        };

        let secret = if cfg.secret_heuristic {
            detect_secret(&normalized)
        } else {
            None
        };

        let preview = match &secret {
            Some(s) => s.masked_preview.clone(),
            None => make_text_preview(&normalized),
        };
        let expires_at = secret.as_ref().map(|s| now_ms.saturating_add(s.ttl_ms));
        let masked_secret = secret.is_some();

        let bytes = normalized.as_bytes();
        let blob_hash = self.write_blob(bytes)?;
        let item = ClipboardItem {
            id: generate_item_id(&canonical),
            kind,
            hash: canonical.clone(),
            plain_hash: Some(canonical),
            mime_types,
            blob_refs: BlobRefs {
                primary: blob_hash.clone(),
                plain_text: None,
                oversized: false,
            },
            size_bytes: bytes.len() as u64,
            preview,
            created: now_ms,
            last_used: now_ms,
            use_count: 1,
            pinned: false,
            source_app,
            image_dims: None,
            files_meta: None,
            masked_secret,
            expires_at,
        };

        self.write_meta(&blob_hash, &item)?;
        self.items.insert(0, item.clone());
        self.enforce_limits(now_ms, cfg)?;
        self.save_index()?;
        Ok(item)
    }

    /// Inserts rich HTML text alongside plain text. If an existing `Text` item has
    /// the same canonical plain text, that item is upgraded in place to `RichText`.
    pub fn insert_rich_text(
        &mut self,
        raw_html: &str,
        raw_plain: Option<&str>,
        mime_types: Vec<String>,
        source_app: Option<String>,
        now_ms: i64,
        cfg: &ClipboardConfig,
    ) -> Result<ClipboardItem, StoreError> {
        let html = normalize_line_endings(raw_html);
        let plain = match raw_plain {
            Some(p) if !p.is_empty() => normalize_line_endings(p),
            _ => html_to_plain_text(&html),
        };
        let plain_hash = canonical_text_hash(&plain);
        let rich_hash = canonical_rich_text_hash(&html, &plain);

        if let Some(idx) = self.items.iter().position(|item| item.hash == rich_hash) {
            return self.bump_existing(idx, source_app, now_ms, cfg);
        }

        let html_blob = self.write_blob(html.as_bytes())?;
        let plain_blob = self.write_blob(plain.as_bytes())?;
        let size_bytes = (html.len() + plain.len()) as u64;
        let preview = make_text_preview(&plain);

        // Upgrade matching plain-text item if one exists.
        if let Some(idx) = self.items.iter().position(|item| {
            matches!(item.kind, ItemKind::Text | ItemKind::Link) && item.hash == plain_hash
        }) {
            let mut upgraded = self.items.remove(idx);
            upgraded.kind = ItemKind::RichText;
            upgraded.hash = rich_hash;
            upgraded.plain_hash = Some(plain_hash);
            upgraded.mime_types = if mime_types.is_empty() {
                vec![HTML_MIME.to_string(), "text/plain;charset=utf-8".to_string()]
            } else {
                mime_types
            };
            upgraded.blob_refs = BlobRefs {
                primary: html_blob.clone(),
                plain_text: Some(plain_blob),
                oversized: false,
            };
            upgraded.size_bytes = size_bytes;
            upgraded.preview = preview;
            upgraded.last_used = now_ms;
            upgraded.use_count = upgraded.use_count.saturating_add(1);
            if source_app.is_some() {
                upgraded.source_app = source_app;
            }
            self.write_meta(&html_blob, &upgraded)?;
            self.items.insert(0, upgraded.clone());
            self.enforce_limits(now_ms, cfg)?;
            self.save_index()?;
            return Ok(upgraded);
        }

        let item = ClipboardItem {
            id: generate_item_id(&rich_hash),
            kind: ItemKind::RichText,
            hash: rich_hash,
            plain_hash: Some(plain_hash),
            mime_types: if mime_types.is_empty() {
                vec![HTML_MIME.to_string(), "text/plain;charset=utf-8".to_string()]
            } else {
                mime_types
            },
            blob_refs: BlobRefs {
                primary: html_blob.clone(),
                plain_text: Some(plain_blob),
                oversized: false,
            },
            size_bytes,
            preview,
            created: now_ms,
            last_used: now_ms,
            use_count: 1,
            pinned: false,
            source_app,
            image_dims: None,
            files_meta: None,
            masked_secret: false,
            expires_at: None,
        };

        self.write_meta(&html_blob, &item)?;
        self.items.insert(0, item.clone());
        self.enforce_limits(now_ms, cfg)?;
        self.save_index()?;
        Ok(item)
    }

    /// Inserts an image, normalizing to PNG and generating a 256 px thumbnail.
    pub fn insert_image(
        &mut self,
        raw: &[u8],
        mime: &str,
        source_app: Option<String>,
        now_ms: i64,
        cfg: &ClipboardConfig,
    ) -> Result<ClipboardItem, StoreError> {
        let processed = process_image_bytes(raw, mime, cfg.max_image_pixels)?;

        if let Some(idx) = self
            .items
            .iter()
            .position(|item| item.kind == ItemKind::Image && item.hash == processed.canonical_hash)
        {
            return self.bump_existing(idx, source_app, now_ms, cfg);
        }

        let blob_hash = self.write_blob(&processed.png_bytes)?;
        let thumb_path = self.thumbnail_path_for_hash(&blob_hash);
        write_atomic_0600(&thumb_path, &processed.thumb_png_bytes)?;

        let preview = format!("Image {}×{}", processed.width, processed.height);
        let item = ClipboardItem {
            id: generate_item_id(&processed.canonical_hash),
            kind: ItemKind::Image,
            hash: processed.canonical_hash,
            plain_hash: None,
            mime_types: vec!["image/png".to_string()],
            blob_refs: BlobRefs {
                primary: blob_hash.clone(),
                plain_text: None,
                oversized: false,
            },
            size_bytes: processed.png_bytes.len() as u64,
            preview,
            created: now_ms,
            last_used: now_ms,
            use_count: 1,
            pinned: false,
            source_app,
            image_dims: Some((processed.width, processed.height)),
            files_meta: None,
            masked_secret: false,
            expires_at: None,
        };

        self.write_meta(&blob_hash, &item)?;
        self.items.insert(0, item.clone());
        self.enforce_limits(now_ms, cfg)?;
        self.save_index()?;
        Ok(item)
    }

    /// Inserts a file list item, deduplicating regardless of path ordering.
    pub fn insert_files(
        &mut self,
        files: FilesMeta,
        source_app: Option<String>,
        now_ms: i64,
        cfg: &ClipboardConfig,
    ) -> Result<ClipboardItem, StoreError> {
        let canonical = canonical_files_hash(&files.paths);

        if let Some(idx) = self
            .items
            .iter()
            .position(|item| item.kind == ItemKind::Files && item.hash == canonical)
        {
            let mut existing = self.items.remove(idx);
            existing.last_used = now_ms;
            existing.use_count = existing.use_count.saturating_add(1);
            if source_app.is_some() {
                existing.source_app = source_app;
            }
            existing.files_meta = Some(files.clone());
            existing.preview = make_files_preview(&files);
            let primary = existing.blob_refs.primary.clone();
            self.write_meta(&primary, &existing)?;
            self.items.insert(0, existing.clone());
            self.enforce_limits(now_ms, cfg)?;
            self.save_index()?;
            return Ok(existing);
        }

        let payload = serde_json::to_vec(&files)?;
        let blob_hash = self.write_blob(&payload)?;
        let preview = make_files_preview(&files);
        let item = ClipboardItem {
            id: generate_item_id(&canonical),
            kind: ItemKind::Files,
            hash: canonical,
            plain_hash: None,
            mime_types: vec![
                URI_LIST_MIME.to_string(),
                GNOME_COPIED_FILES_MIME.to_string(),
            ],
            blob_refs: BlobRefs {
                primary: blob_hash.clone(),
                plain_text: None,
                oversized: false,
            },
            size_bytes: payload.len() as u64,
            preview,
            created: now_ms,
            last_used: now_ms,
            use_count: 1,
            pinned: false,
            source_app,
            image_dims: None,
            files_meta: Some(files),
            masked_secret: false,
            expires_at: None,
        };

        self.write_meta(&blob_hash, &item)?;
        self.items.insert(0, item.clone());
        self.enforce_limits(now_ms, cfg)?;
        self.save_index()?;
        Ok(item)
    }

    /// Records an oversized placeholder item (when enabled in settings) without storing blob bytes.
    pub fn insert_oversized_placeholder(
        &mut self,
        kind: ItemKind,
        byte_len: usize,
        source_app: Option<String>,
        now_ms: i64,
        cfg: &ClipboardConfig,
    ) -> Result<ClipboardItem, StoreError> {
        let preview = format!("Too large to store ({} bytes)", byte_len);
        let hash = sha256_hex(format!("oversized:{kind:?}:{byte_len}:{now_ms}").as_bytes());
        let item = ClipboardItem {
            id: generate_item_id(&hash),
            kind,
            hash,
            plain_hash: None,
            mime_types: Vec::new(),
            blob_refs: BlobRefs {
                primary: String::new(),
                plain_text: None,
                oversized: true,
            },
            size_bytes: byte_len as u64,
            preview,
            created: now_ms,
            last_used: now_ms,
            use_count: 1,
            pinned: false,
            source_app,
            image_dims: None,
            files_meta: None,
            masked_secret: false,
            expires_at: None,
        };
        self.items.insert(0, item.clone());
        self.enforce_limits(now_ms, cfg)?;
        self.save_index()?;
        Ok(item)
    }

    pub fn pin(
        &mut self,
        id_or_prefix: &str,
        pinned: bool,
        max_pinned: usize,
    ) -> Result<ClipboardItem, StoreError> {
        let idx = self.resolve_index(id_or_prefix)?;
        if pinned && !self.items[idx].pinned {
            let current_pinned = self.items.iter().filter(|i| i.pinned).count();
            if current_pinned >= max_pinned {
                return Err(StoreError::PinLimitReached(max_pinned));
            }
        }
        self.items[idx].pinned = pinned;
        let item = self.items[idx].clone();
        if pinned {
            if !self.pinned_ids.contains(&item.id) {
                self.pinned_ids.insert(0, item.id.clone());
            }
        } else {
            self.pinned_ids.retain(|id| id != &item.id);
        }
        self.save_pins()?;
        self.save_index()?;
        Ok(item)
    }

    pub fn mark_used(
        &mut self,
        id_or_prefix: &str,
        now_ms: i64,
    ) -> Result<ClipboardItem, StoreError> {
        let idx = self.resolve_index(id_or_prefix)?;
        let mut item = self.items.remove(idx);
        item.last_used = now_ms;
        item.use_count = item.use_count.saturating_add(1);
        if !item.blob_refs.oversized && !item.blob_refs.primary.is_empty() {
            let primary = item.blob_refs.primary.clone();
            self.write_meta(&primary, &item)?;
        }
        self.items.insert(0, item.clone());
        self.save_index()?;
        Ok(item)
    }

    pub fn remove(&mut self, id_or_prefix: &str) -> Result<ClipboardItem, StoreError> {
        let idx = self.resolve_index(id_or_prefix)?;
        let removed = self.items.remove(idx);
        self.pinned_ids.retain(|id| id != &removed.id);
        self.gc_unreferenced_blobs()?;
        self.save_pins()?;
        self.save_index()?;
        Ok(removed)
    }

    pub fn clear(&mut self, keep_pinned: bool) -> Result<usize, StoreError> {
        let before = self.items.len();
        if keep_pinned {
            self.items.retain(|item| item.pinned);
        } else {
            self.items.clear();
            self.pinned_ids.clear();
        }
        let removed = before.saturating_sub(self.items.len());
        self.gc_unreferenced_blobs()?;
        self.save_pins()?;
        self.save_index()?;
        Ok(removed)
    }

    /// Expires old items and evicts oldest unpinned items to stay within
    /// `cfg.max_items` and `cfg.max_bytes`, deleting unreferenced blobs.
    pub fn enforce_limits(
        &mut self,
        now_ms: i64,
        cfg: &ClipboardConfig,
    ) -> Result<(), StoreError> {
        self.items.retain(|item| {
            if let Some(exp) = item.expires_at
                && now_ms >= exp
            {
                return false;
            }
            if !item.pinned
                && let Some(ttl_ms) = cfg.expiry.as_millis()
                && now_ms.saturating_sub(item.last_used) >= ttl_ms
            {
                return false;
            }
            true
        });

        // Evict by unpinned item count.
        loop {
            let unpinned_count = self.items.iter().filter(|i| !i.pinned).count();
            if unpinned_count <= cfg.max_items {
                break;
            }
            let Some(victim_idx) = self.oldest_unpinned_index() else {
                break;
            };
            self.items.remove(victim_idx);
        }

        // Evict by total referenced blob bytes.
        let mut blob_sizes = self.measure_referenced_blob_sizes();
        loop {
            let total_bytes: u64 = blob_sizes.values().copied().sum();
            if total_bytes <= cfg.max_bytes {
                break;
            }
            let Some(victim_idx) = self.oldest_unpinned_index() else {
                break;
            };
            self.items.remove(victim_idx);
            blob_sizes = self.recompute_referenced_sizes(&blob_sizes);
        }

        self.sync_pins_from_items();
        self.gc_unreferenced_blobs()?;
        Ok(())
    }

    fn bump_existing(
        &mut self,
        idx: usize,
        source_app: Option<String>,
        now_ms: i64,
        cfg: &ClipboardConfig,
    ) -> Result<ClipboardItem, StoreError> {
        let mut item = self.items.remove(idx);
        item.last_used = now_ms;
        item.use_count = item.use_count.saturating_add(1);
        if source_app.is_some() {
            item.source_app = source_app;
        }
        if !item.blob_refs.oversized && !item.blob_refs.primary.is_empty() {
            let primary = item.blob_refs.primary.clone();
            self.write_meta(&primary, &item)?;
        }
        self.items.insert(0, item.clone());
        self.enforce_limits(now_ms, cfg)?;
        self.save_index()?;
        Ok(item)
    }

    fn oldest_unpinned_index(&self) -> Option<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| !item.pinned)
            .min_by_key(|(idx, item)| (item.last_used, std::cmp::Reverse(*idx)))
            .map(|(idx, _)| idx)
    }

    fn measure_referenced_blob_sizes(&self) -> HashMap<String, u64> {
        let mut sizes = HashMap::new();
        for item in &self.items {
            for hash in item.referenced_blobs() {
                if !sizes.contains_key(hash) {
                    let len = fs::metadata(self.blob_path(hash))
                        .map(|m| m.len())
                        .unwrap_or(0);
                    sizes.insert(hash.to_string(), len);
                }
            }
        }
        sizes
    }

    fn recompute_referenced_sizes(
        &self,
        known: &HashMap<String, u64>,
    ) -> HashMap<String, u64> {
        let mut next = HashMap::new();
        for item in &self.items {
            for hash in item.referenced_blobs() {
                if let Some(&sz) = known.get(hash) {
                    next.insert(hash.to_string(), sz);
                }
            }
        }
        next
    }

    fn gc_unreferenced_blobs(&self) -> Result<(), StoreError> {
        let mut live_blobs: HashSet<&str> = HashSet::new();
        let mut live_thumbs: HashSet<&str> = HashSet::new();
        for item in &self.items {
            for hash in item.referenced_blobs() {
                live_blobs.insert(hash);
            }
            if let Some(t) = item.thumbnail_hash() {
                live_thumbs.insert(t);
            }
        }

        let blobs_dir = self.root.join("blobs");
        if blobs_dir.exists() {
            for prefix_entry in fs::read_dir(&blobs_dir)? {
                let prefix_entry = prefix_entry?;
                let prefix_path = prefix_entry.path();
                if !prefix_path.is_dir() {
                    continue;
                }
                for entry in fs::read_dir(&prefix_path)? {
                    let entry = entry?;
                    let path = entry.path();
                    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    let hash = name.strip_suffix(".meta").unwrap_or(name);
                    if !live_blobs.contains(hash) {
                        let _ = fs::remove_file(&path);
                    }
                }
                // Remove empty shard directory cleanly.
                if fs::read_dir(&prefix_path)
                    .map(|mut d| d.next().is_none())
                    .unwrap_or(false)
                {
                    let _ = fs::remove_dir(&prefix_path);
                }
            }
        }

        let thumbs_dir = self.root.join("thumbs");
        if thumbs_dir.exists() {
            for entry in fs::read_dir(&thumbs_dir)? {
                let entry = entry?;
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let hash = name.strip_suffix(".png").unwrap_or(name);
                if !live_thumbs.contains(hash) {
                    let _ = fs::remove_file(&path);
                }
            }
        }

        Ok(())
    }

    fn write_blob(&self, bytes: &[u8]) -> Result<String, StoreError> {
        let hash = sha256_hex(bytes);
        let path = self.blob_path(&hash);
        if let Some(parent) = path.parent() {
            ensure_dir_0700(parent)?;
        }
        if !path.exists() {
            write_atomic_0600(&path, bytes)?;
        }
        Ok(hash)
    }

    fn write_meta(&self, primary_blob_hash: &str, item: &ClipboardItem) -> Result<(), StoreError> {
        let path = self.meta_path(primary_blob_hash);
        if let Some(parent) = path.parent() {
            ensure_dir_0700(parent)?;
        }
        let meta = BlobMeta::from_item(item);
        let bytes = serde_json::to_vec_pretty(&meta)?;
        write_atomic_0600(&path, &bytes)
    }

    fn sync_pins_from_items(&mut self) {
        let live_ids: HashSet<&str> = self.items.iter().map(|i| i.id.as_str()).collect();
        self.pinned_ids.retain(|id| live_ids.contains(id.as_str()));
        for item in &self.items {
            if item.pinned && !self.pinned_ids.contains(&item.id) {
                self.pinned_ids.push(item.id.clone());
            }
        }
    }

    pub fn save_index(&self) -> Result<(), StoreError> {
        let payload = IndexFile {
            version: INDEX_VERSION,
            items: self.items.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&payload)?;
        write_atomic_0600(&self.root.join("index.json"), &bytes)
    }

    pub fn save_pins(&self) -> Result<(), StoreError> {
        let payload = PinsFile {
            version: PINS_VERSION,
            pinned_ids: self.pinned_ids.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&payload)?;
        write_atomic_0600(&self.root.join("pins.json"), &bytes)
    }
}

fn generate_item_id(canonical_hash: &str) -> String {
    let seq = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let prefix = canonical_hash.get(..12).unwrap_or(canonical_hash);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    format!("{prefix}-{:x}-{seq:x}", nanos & 0xffff_ffff)
}

fn ensure_dir_0700(path: &Path) -> Result<(), StoreError> {
    if !path.exists() {
        fs::create_dir_all(path)?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn write_atomic_0600(target: &Path, data: &[u8]) -> Result<(), StoreError> {
    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let tmp_path = target.with_file_name(format!("{file_name}.tmp"));

    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        file.write_all(data)?;
        file.sync_all()?;
    }

    fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp_path, target)?;

    if let Some(parent) = target.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn load_pins(path: &Path) -> Vec<String> {
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice::<PinsFile>(&bytes)
        .map(|p| p.pinned_ids)
        .unwrap_or_default()
}

fn recover_from_sidecars(root: &Path, pinned_ids: &[String]) -> Result<Vec<ClipboardItem>, StoreError> {
    let blobs_dir = root.join("blobs");
    if !blobs_dir.exists() {
        return Ok(Vec::new());
    }
    let pin_set: HashSet<&str> = pinned_ids.iter().map(String::as_str).collect();
    let mut items = Vec::new();

    for prefix_entry in fs::read_dir(&blobs_dir)? {
        let prefix_entry = prefix_entry?;
        let prefix_path = prefix_entry.path();
        if !prefix_path.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&prefix_path)? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(blob_hash) = name.strip_suffix(".meta") else {
                continue;
            };
            let blob_file = prefix_path.join(blob_hash);
            if !blob_file.exists() {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            let Ok(meta) = serde_json::from_slice::<BlobMeta>(&bytes) else {
                continue;
            };
            let pinned = pin_set.contains(meta.id.as_str());
            items.push(meta.into_item(blob_hash.to_string(), pinned));
        }
    }

    items.sort_by_key(|a| std::cmp::Reverse(a.last_used));
    Ok(items)
}

fn unix_secs_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[must_use]
pub fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
