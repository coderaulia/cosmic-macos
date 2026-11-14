// SPDX-License-Identifier: GPL-3.0-only

//! Local disk cache for folders, message headers, raw RFC 822 message bodies,
//! pagination state, and harvested contact addresses.
//!
//! Layout under `$XDG_CACHE_HOME/mac-mail/<account-key>/` (or `$HOME/.cache/...`):
//! - `folders.json`: folder list, friendly display names, semantic kinds, unread counts
//! - `contacts.json`: harvested email addresses & display names for autocomplete
//! - `folders/<folder-key>/headers.json`: cached summaries & pagination metadata
//! - `folders/<folder-key>/messages/<uid>.eml`: raw RFC 822 message bytes

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Semantic role of an IMAP mailbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FolderKind {
    Inbox,
    Drafts,
    Sent,
    Archive,
    Junk,
    Trash,
    Other,
}

impl FolderKind {
    fn sort_rank(self) -> u8 {
        match self {
            FolderKind::Inbox => 0,
            FolderKind::Drafts => 1,
            FolderKind::Sent => 2,
            FolderKind::Archive => 3,
            FolderKind::Junk => 4,
            FolderKind::Trash => 5,
            FolderKind::Other => 6,
        }
    }
}

/// Folder metadata cached on disk and presented in the sidebar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderInfo {
    /// Raw server folder name used in IMAP commands (e.g. `"INBOX.Drafts"`).
    pub raw_name: String,
    /// Human-friendly display name (e.g. `"Drafts"`).
    pub display_name: String,
    /// Semantic folder kind from RFC 6154 attributes or name heuristics.
    pub kind: FolderKind,
    /// Hierarchy delimiter reported by `LIST` (e.g. `"."` or `"/"`).
    pub delimiter: Option<String>,
    /// Unread (`\Unseen`) count reported by the server or updated locally.
    pub unread: u32,
    /// Total message count (`MESSAGES`/`EXISTS`) if known.
    pub total: Option<u32>,
    /// Server `UIDVALIDITY` if known.
    pub uid_validity: Option<u32>,
}

/// Summary of a message header for the message list and threading/reply helpers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageSummary {
    pub uid: u32,
    /// Display name or email of the sender.
    pub from: String,
    /// Raw email address of the sender (first `From` mailbox).
    #[serde(default)]
    pub from_address: String,
    /// Where a reply goes: first `Reply-To` address, else sender's address.
    pub address: String,
    /// Formatted `To` recipients (`Name <addr>` or `addr`).
    #[serde(default)]
    pub to: Vec<String>,
    /// Formatted `Cc` recipients.
    #[serde(default)]
    pub cc: Vec<String>,
    pub subject: String,
    /// Formatted `YYYY-MM-DD HH:MM` timestamp string.
    pub date: String,
    /// Unix timestamp in seconds if parsed from the `Date` header.
    #[serde(default)]
    pub timestamp: Option<i64>,
    pub unread: bool,
    /// Whether the message contains non-inline file attachments.
    #[serde(default)]
    pub has_attachments: bool,
    /// Short single-line / two-line body preview when available.
    #[serde(default)]
    pub preview: String,
    /// `Message-ID` header value (without angle brackets or preserved as parsed).
    #[serde(default)]
    pub message_id: Option<String>,
    /// `In-Reply-To` header value (first message ID) for conversation threading.
    #[serde(default)]
    pub in_reply_to: Option<String>,
    /// `References` header values for reply threading.
    #[serde(default)]
    pub references: Vec<String>,
}

/// Cached folder header list plus pagination state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedFolderHeaders {
    pub uid_validity: Option<u32>,
    pub total_exists: u32,
    pub lowest_seq_loaded: u32,
    pub has_more: bool,
    pub summaries: Vec<MessageSummary>,
}

/// Harvested contact entry used for compose address autocomplete.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownContact {
    pub name: Option<String>,
    pub email: String,
    pub last_seen_ts: i64,
    pub frequency: u32,
}

/// Classify a raw IMAP folder name using optional attribute debug strings (RFC 6154)
/// and name heuristics, producing a friendly display name.
pub fn classify_folder(
    raw_name: &str,
    attribute_names: &[String],
    delimiter: Option<&str>,
) -> FolderInfo {
    let kind = detect_folder_kind(raw_name, attribute_names, delimiter);
    let display_name = friendly_folder_name(raw_name, kind, delimiter);
    FolderInfo {
        raw_name: raw_name.to_owned(),
        display_name,
        kind,
        delimiter: delimiter.map(str::to_owned),
        unread: 0,
        total: None,
        uid_validity: None,
    }
}

fn detect_folder_kind(
    raw_name: &str,
    attribute_names: &[String],
    delimiter: Option<&str>,
) -> FolderKind {
    if raw_name.eq_ignore_ascii_case("INBOX") {
        return FolderKind::Inbox;
    }

    for attr in attribute_names {
        let lower = attr.to_ascii_lowercase();
        if lower == "drafts" || lower == "\\drafts" {
            return FolderKind::Drafts;
        }
        if lower == "sent" || lower == "\\sent" {
            return FolderKind::Sent;
        }
        if lower == "trash" || lower == "\\trash" {
            return FolderKind::Trash;
        }
        if lower == "junk" || lower == "\\junk" {
            return FolderKind::Junk;
        }
        if lower == "archive" || lower == "\\archive" || lower == "all" || lower == "\\all" {
            return FolderKind::Archive;
        }
    }

    let leaf = folder_leaf(raw_name, delimiter).to_ascii_lowercase();
    if leaf == "inbox" {
        FolderKind::Inbox
    } else if leaf == "draft" || leaf == "drafts" {
        FolderKind::Drafts
    } else if matches!(
        leaf.as_str(),
        "sent" | "sent mail" | "sent items" | "sent messages"
    ) {
        FolderKind::Sent
    } else if matches!(leaf.as_str(), "archive" | "archives" | "all mail") {
        FolderKind::Archive
    } else if matches!(leaf.as_str(), "junk" | "junk e-mail" | "spam" | "bulk mail") {
        FolderKind::Junk
    } else if matches!(
        leaf.as_str(),
        "trash" | "deleted" | "deleted items" | "deleted messages" | "bin"
    ) {
        FolderKind::Trash
    } else {
        FolderKind::Other
    }
}

fn folder_leaf<'a>(raw_name: &'a str, delimiter: Option<&str>) -> &'a str {
    if let Some(delim) = delimiter.filter(|d| !d.is_empty())
        && let Some((_, tail)) = raw_name.rsplit_once(delim)
    {
        return tail;
    }
    raw_name
        .rsplit(['.', '/'])
        .next()
        .unwrap_or(raw_name)
}

/// Convert raw IMAP mailbox names like `INBOX.Drafts` or `[Gmail]/Sent Mail`
/// into clean user-facing labels.
pub fn friendly_folder_name(raw_name: &str, kind: FolderKind, delimiter: Option<&str>) -> String {
    match kind {
        FolderKind::Inbox => return "Inbox".to_owned(),
        FolderKind::Drafts => return "Drafts".to_owned(),
        FolderKind::Sent => return "Sent".to_owned(),
        FolderKind::Archive => {
            let leaf = folder_leaf(raw_name, delimiter);
            if leaf.eq_ignore_ascii_case("all mail") {
                return "All Mail".to_owned();
            }
            return "Archive".to_owned();
        }
        FolderKind::Junk => {
            let leaf = folder_leaf(raw_name, delimiter);
            if leaf.eq_ignore_ascii_case("spam") {
                return "Spam".to_owned();
            }
            return "Junk".to_owned();
        }
        FolderKind::Trash => return "Trash".to_owned(),
        FolderKind::Other => {}
    }

    let stripped = strip_common_prefixes(raw_name, delimiter);
    if let Some(delim) = delimiter.filter(|d| !d.is_empty()) {
        stripped
            .split(delim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" / ")
    } else {
        stripped
            .split(['.', '/'])
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

fn strip_common_prefixes<'a>(raw_name: &'a str, delimiter: Option<&str>) -> &'a str {
    let mut s = raw_name;
    for prefix in ["[Gmail]/", "[Gmail].", "[Google Mail]/", "[Google Mail]."] {
        if let Some(rest) = s.get(..prefix.len()).filter(|p| p.eq_ignore_ascii_case(prefix)) {
            let _ = rest;
            s = &s[prefix.len()..];
            break;
        }
    }
    if let Some(delim) = delimiter.filter(|d| !d.is_empty()) {
        let inbox_prefix_len = "INBOX".len() + delim.len();
        if s.len() > inbox_prefix_len
            && s[..5].eq_ignore_ascii_case("INBOX")
            && &s[5..inbox_prefix_len] == delim
        {
            s = &s[inbox_prefix_len..];
        }
    } else if s.len() > 6 && s[..5].eq_ignore_ascii_case("INBOX") && matches!(s.as_bytes()[5], b'.' | b'/') {
        s = &s[6..];
    }
    if s.is_empty() { raw_name } else { s }
}

/// Sort folders into standard mail order: Inbox, Drafts, Sent, Archive, Junk, Trash, then custom.
pub fn sort_folders(folders: &mut [FolderInfo]) {
    folders.sort_by(|a, b| {
        a.kind
            .sort_rank()
            .cmp(&b.kind.sort_rank())
            .then_with(|| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()))
            .then_with(|| a.raw_name.cmp(&b.raw_name))
    });
}

/// Find the first folder matching a semantic `FolderKind`.
pub fn find_special_folder(folders: &[FolderInfo], kind: FolderKind) -> Option<&FolderInfo> {
    folders.iter().find(|f| f.kind == kind)
}

/// Given a set of candidate UIDs from `UID SEARCH`, select up to `page_size` UIDs
/// strictly less than `before_uid` (newest-first) and report whether even older UIDs remain.
pub fn select_older_uid_page(
    mut candidate_uids: Vec<u32>,
    before_uid: u32,
    page_size: usize,
) -> (Vec<u32>, bool) {
    if before_uid <= 1 || page_size == 0 {
        return (Vec::new(), false);
    }
    candidate_uids.retain(|&uid| uid < before_uid);
    candidate_uids.sort_unstable_by(|a, b| b.cmp(a));
    candidate_uids.dedup();
    let has_more = candidate_uids.len() > page_size;
    candidate_uids.truncate(page_size);
    (candidate_uids, has_more)
}

/// Parse `(Option<display_name>, email)` pairs from header strings or comma-separated lists.
pub fn extract_email_addresses(input: &str) -> Vec<(Option<String>, String)> {
    let mut out = Vec::new();
    for segment in split_outside_quotes(input, ',') {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let (Some(lt), Some(gt)) = (trimmed.rfind('<'), trimmed.rfind('>'))
            && lt < gt
        {
            let addr = trimmed[lt + 1..gt].trim();
            let raw_name = trimmed[..lt].trim().trim_matches('"').trim();
            if is_plausible_email(addr) {
                let name = (!raw_name.is_empty()).then(|| raw_name.to_owned());
                out.push((name, addr.to_ascii_lowercase()));
            }
        } else {
            let addr = trimmed.trim_matches(|c| matches!(c, '<' | '>' | '"' | ' '));
            if is_plausible_email(addr) {
                out.push((None, addr.to_ascii_lowercase()));
            }
        }
    }
    out
}

fn split_outside_quotes(input: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_quotes = false;
    let mut escape = false;
    let mut start = 0;
    for (idx, ch) in input.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escape = true,
            '"' => in_quotes = !in_quotes,
            c if c == sep && !in_quotes => {
                parts.push(&input[start..idx]);
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&input[start..]);
    parts
}

fn is_plausible_email(addr: &str) -> bool {
    let Some((local, domain)) = addr.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && domain.contains('.')
        && !addr.chars().any(char::is_whitespace)
}

/// Disk-backed cache scoped to one mail account.
#[derive(Clone, Debug)]
pub struct MailCache {
    root: PathBuf,
}

impl MailCache {
    /// Open or initialize the cache directory under `$XDG_CACHE_HOME/mac-mail/<account>`.
    pub fn new(account_email: &str) -> Self {
        let base = default_cache_base().unwrap_or_else(|| std::env::temp_dir().join("mac-mail"));
        Self::with_root(base, account_email)
    }

    /// Open or initialize a cache rooted under a custom directory (primarily for tests).
    pub fn with_root(base_dir: impl AsRef<Path>, account_email: &str) -> Self {
        let account_key = sanitize_key(&account_email.to_ascii_lowercase());
        Self {
            root: base_dir.as_ref().join(account_key),
        }
    }

    pub fn root_dir(&self) -> &Path {
        &self.root
    }

    fn folders_path(&self) -> PathBuf {
        self.root.join("folders.json")
    }

    fn contacts_path(&self) -> PathBuf {
        self.root.join("contacts.json")
    }

    fn folder_dir(&self, folder: &str) -> PathBuf {
        self.root.join("folders").join(sanitize_key(folder))
    }

    fn headers_path(&self, folder: &str) -> PathBuf {
        self.folder_dir(folder).join("headers.json")
    }

    fn message_path(&self, folder: &str, uid: u32) -> PathBuf {
        self.folder_dir(folder)
            .join("messages")
            .join(format!("{uid}.eml"))
    }

    /// Remove all cached files for this account (e.g. on sign-out).
    pub fn clear_account(&self) -> Result<(), String> {
        if self.root.exists() {
            fs::remove_dir_all(&self.root).map_err(|err| err.to_string())?;
        }
        Ok(())
    }

    /// Persist folder list and unread metadata.
    pub fn save_folders(&self, folders: &[FolderInfo]) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(folders).map_err(|err| err.to_string())?;
        write_atomic(&self.folders_path(), &json)
    }

    /// Load cached folder list (returns empty vec if nothing cached yet or unreadable).
    pub fn load_folders(&self) -> Vec<FolderInfo> {
        fs::read(self.folders_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Update unread/total/uid_validity metadata for a single folder in `folders.json`.
    pub fn update_folder_metadata(
        &self,
        raw_folder: &str,
        unread: u32,
        total: Option<u32>,
        uid_validity: Option<u32>,
    ) -> Result<(), String> {
        let mut folders = self.load_folders();
        if let Some(entry) = folders.iter_mut().find(|f| f.raw_name == raw_folder) {
            entry.unread = unread;
            if total.is_some() {
                entry.total = total;
            }
            if uid_validity.is_some() {
                entry.uid_validity = uid_validity;
            }
            self.save_folders(&folders)?;
        }
        Ok(())
    }

    /// Load cached headers and pagination state for a folder.
    pub fn load_folder_headers(&self, folder: &str) -> Option<CachedFolderHeaders> {
        let bytes = fs::read(self.headers_path(folder)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Save headers and pagination state for a folder.
    pub fn save_folder_headers(
        &self,
        folder: &str,
        headers: &CachedFolderHeaders,
    ) -> Result<(), String> {
        let json = serde_json::to_vec(headers).map_err(|err| err.to_string())?;
        write_atomic(&self.headers_path(folder), &json)
    }

    /// Invalidate cached headers and raw message bodies for a folder (e.g. when `UIDVALIDITY` changes).
    pub fn invalidate_folder(&self, folder: &str) -> Result<(), String> {
        let dir = self.folder_dir(folder);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|err| err.to_string())?;
        }
        Ok(())
    }

    /// Merge newly fetched summaries into the folder header cache, deduplicating by UID
    /// and preserving previously cached previews if the new summary lacks one.
    #[allow(clippy::too_many_arguments)]
    pub fn merge_folder_headers(
        &self,
        folder: &str,
        uid_validity: Option<u32>,
        total_exists: u32,
        lowest_seq_loaded: u32,
        has_more: bool,
        incoming: Vec<MessageSummary>,
        replace_top: bool,
    ) -> Result<CachedFolderHeaders, String> {
        let existing = self.load_folder_headers(folder);
        if let (Some(prev), Some(new_uv)) = (&existing, uid_validity)
            && let Some(old_uv) = prev.uid_validity
            && old_uv != new_uv
        {
            self.invalidate_folder(folder)?;
        }

        let existing = self.load_folder_headers(folder).unwrap_or_default();
        let mut by_uid: HashMap<u32, MessageSummary> = if replace_top {
            // Keep older summaries below the lowest UID in the refreshed top page so
            // previously loaded older pages are preserved unless UIDs vanished.
            let min_new_uid = incoming.iter().map(|s| s.uid).min();
            existing
                .summaries
                .into_iter()
                .filter(|s| min_new_uid.is_some_and(|min_uid| s.uid < min_uid))
                .map(|s| (s.uid, s))
                .collect()
        } else {
            existing
                .summaries
                .into_iter()
                .map(|s| (s.uid, s))
                .collect()
        };

        let retained_older = replace_top && !by_uid.is_empty();

        for mut summary in incoming {
            if summary.preview.is_empty()
                && let Some(prev) = by_uid.get(&summary.uid)
                && !prev.preview.is_empty()
            {
                summary.preview = prev.preview.clone();
            }
            by_uid.insert(summary.uid, summary);
        }

        let mut summaries: Vec<MessageSummary> = by_uid.into_values().collect();
        summaries.sort_by_key(|s| std::cmp::Reverse(s.uid));

        let next_has_more = if retained_older {
            existing.has_more
        } else {
            has_more
        };
        let next_lowest_seq = if retained_older && existing.lowest_seq_loaded > 0 {
            existing.lowest_seq_loaded
        } else if next_has_more {
            lowest_seq_loaded.max(2)
        } else {
            1
        };

        let next = CachedFolderHeaders {
            uid_validity: uid_validity.or(existing.uid_validity),
            total_exists,
            lowest_seq_loaded: next_lowest_seq,
            has_more: next_has_more,
            summaries,
        };
        self.save_folder_headers(folder, &next)?;
        Ok(next)
    }

    /// Update the read/unread flag of a cached message and adjust the folder's cached unread count.
    pub fn set_message_unread(&self, folder: &str, uid: u32, unread: bool) -> Result<(), String> {
        let mut changed = false;
        if let Some(mut headers) = self.load_folder_headers(folder) {
            if let Some(msg) = headers.summaries.iter_mut().find(|m| m.uid == uid)
                && msg.unread != unread
            {
                msg.unread = unread;
                changed = true;
            }
            if changed {
                self.save_folder_headers(folder, &headers)?;
            }
        }
        if changed {
            let mut folders = self.load_folders();
            if let Some(f) = folders.iter_mut().find(|f| f.raw_name == folder) {
                if unread {
                    f.unread = f.unread.saturating_add(1);
                } else {
                    f.unread = f.unread.saturating_sub(1);
                }
                self.save_folders(&folders)?;
            }
        }
        Ok(())
    }

    /// Update the preview text of a cached message (for instance after opening its body).
    pub fn update_message_preview(
        &self,
        folder: &str,
        uid: u32,
        preview: &str,
    ) -> Result<(), String> {
        if preview.is_empty() {
            return Ok(());
        }
        if let Some(mut headers) = self.load_folder_headers(folder)
            && let Some(msg) = headers.summaries.iter_mut().find(|m| m.uid == uid)
            && msg.preview != preview
        {
            msg.preview = preview.to_owned();
            self.save_folder_headers(folder, &headers)?;
        }
        Ok(())
    }

    /// Remove a message from a folder's cached header list and delete its raw body cache.
    pub fn remove_message(&self, folder: &str, uid: u32) -> Result<(), String> {
        let mut was_unread = false;
        if let Some(mut headers) = self.load_folder_headers(folder) {
            let before = headers.summaries.len();
            headers.summaries.retain(|m| {
                if m.uid == uid {
                    was_unread = m.unread;
                    false
                } else {
                    true
                }
            });
            if headers.summaries.len() != before {
                headers.total_exists = headers.total_exists.saturating_sub(1);
                self.save_folder_headers(folder, &headers)?;
                let mut folders = self.load_folders();
                if let Some(f) = folders.iter_mut().find(|f| f.raw_name == folder) {
                    if was_unread {
                        f.unread = f.unread.saturating_sub(1);
                    }
                    if let Some(total) = f.total.as_mut() {
                        *total = total.saturating_sub(1);
                    }
                    self.save_folders(&folders)?;
                }
            }
        }
        self.delete_raw_message(folder, uid);
        Ok(())
    }

    /// Search cached headers offline by sender, subject, or preview text.
    pub fn search_cached_headers(&self, folder: &str, query: &str) -> Vec<MessageSummary> {
        let q = query.trim().to_ascii_lowercase();
        let Some(headers) = self.load_folder_headers(folder) else {
            return Vec::new();
        };
        if q.is_empty() {
            return headers.summaries;
        }
        headers
            .summaries
            .into_iter()
            .filter(|m| {
                m.subject.to_ascii_lowercase().contains(&q)
                    || m.from.to_ascii_lowercase().contains(&q)
                    || m.from_address.to_ascii_lowercase().contains(&q)
                    || m.preview.to_ascii_lowercase().contains(&q)
            })
            .collect()
    }

    /// Save raw RFC 822 message bytes on disk.
    pub fn save_raw_message(&self, folder: &str, uid: u32, raw: &[u8]) -> Result<(), String> {
        write_atomic(&self.message_path(folder, uid), raw)
    }

    /// Load cached raw RFC 822 message bytes if present.
    pub fn load_raw_message(&self, folder: &str, uid: u32) -> Option<Vec<u8>> {
        fs::read(self.message_path(folder, uid)).ok()
    }

    /// Whether the raw RFC 822 message bytes are already cached on disk.
    pub fn has_raw_message(&self, folder: &str, uid: u32) -> bool {
        self.message_path(folder, uid).is_file()
    }

    /// Delete cached raw message bytes if present.
    pub fn delete_raw_message(&self, folder: &str, uid: u32) {
        let _ = fs::remove_file(self.message_path(folder, uid));
    }

    /// Load all harvested contacts, ordered by frequency desc then name/email.
    pub fn load_contacts(&self) -> Vec<KnownContact> {
        let mut contacts: Vec<KnownContact> = fs::read(self.contacts_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        contacts.sort_by(|a, b| {
            b.frequency
                .cmp(&a.frequency)
                .then_with(|| b.last_seen_ts.cmp(&a.last_seen_ts))
                .then_with(|| a.email.cmp(&b.email))
        });
        contacts
    }

    /// Record `(Option<name>, email)` entries into the persistent contacts cache.
    pub fn harvest_contacts(
        &self,
        entries: &[(Option<String>, String)],
        timestamp: Option<i64>,
    ) -> Result<(), String> {
        if entries.is_empty() {
            return Ok(());
        }
        let ts = timestamp.unwrap_or_else(current_unix_secs);
        let existing = self.load_contacts();
        let mut by_email: HashMap<String, KnownContact> = existing
            .into_iter()
            .map(|c| (c.email.to_ascii_lowercase(), c))
            .collect();

        let mut seen_in_batch = HashSet::new();
        for (name, email) in entries {
            let norm = email.trim().to_ascii_lowercase();
            if !is_plausible_email(&norm) || !seen_in_batch.insert(norm.clone()) {
                continue;
            }
            let clean_name = name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case(&norm))
                .map(str::to_owned);
            match by_email.get_mut(&norm) {
                Some(entry) => {
                    entry.frequency = entry.frequency.saturating_add(1);
                    entry.last_seen_ts = entry.last_seen_ts.max(ts);
                    if entry.name.is_none() && clean_name.is_some() {
                        entry.name = clean_name;
                    }
                }
                None => {
                    by_email.insert(
                        norm.clone(),
                        KnownContact {
                            name: clean_name,
                            email: norm,
                            last_seen_ts: ts,
                            frequency: 1,
                        },
                    );
                }
            }
        }

        let mut list: Vec<KnownContact> = by_email.into_values().collect();
        list.sort_by(|a, b| {
            b.frequency
                .cmp(&a.frequency)
                .then_with(|| b.last_seen_ts.cmp(&a.last_seen_ts))
                .then_with(|| a.email.cmp(&b.email))
        });
        // Cap stored contacts so the file stays fast.
        list.truncate(2000);
        let json = serde_json::to_vec(&list).map_err(|err| err.to_string())?;
        write_atomic(&self.contacts_path(), &json)
    }

    /// Return up to `limit` contact suggestions matching `query` (prefix/substring on name or email).
    pub fn suggest_contacts(&self, query: &str, limit: usize) -> Vec<KnownContact> {
        let q = query.trim().to_ascii_lowercase();
        let contacts = self.load_contacts();
        if q.is_empty() {
            return contacts.into_iter().take(limit).collect();
        }
        let mut matches: Vec<(u8, KnownContact)> = contacts
            .into_iter()
            .filter_map(|c| {
                let email_lower = c.email.to_ascii_lowercase();
                let name_lower = c.name.as_deref().unwrap_or("").to_ascii_lowercase();
                let rank = if email_lower.starts_with(&q) || name_lower.starts_with(&q) {
                    0
                } else if email_lower.contains(&q) || name_lower.contains(&q) {
                    1
                } else {
                    return None;
                };
                Some((rank, c))
            })
            .collect();
        matches.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.frequency.cmp(&a.1.frequency))
                .then_with(|| a.1.email.cmp(&b.1.email))
        });
        matches.into_iter().take(limit).map(|(_, c)| c).collect()
    }
}

fn default_cache_base() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(base.join("mac-mail"))
}

fn current_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Convert an arbitrary account or folder name into a collision-resistant, filesystem-safe key.
pub fn sanitize_key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 16);
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '@' | '-' | '_') {
            out.push(ch);
        } else if ch == '.' && !out.ends_with('.') && !out.is_empty() {
            out.push('.');
        } else {
            out.push('_');
        }
    }
    if out.is_empty() || out == "." || out == ".." {
        out = "_root".to_owned();
    }
    // Append a short FNV-1a hash so folders differing only in punctuation don't collide.
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in raw.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{out}-{hash:08x}")
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err("invalid cache path".to_owned());
    };
    fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.tmp.{}.{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cache"),
        std::process::id(),
        seq
    );
    let tmp_path = parent.join(tmp_name);
    fs::write(&tmp_path, bytes).map_err(|err| err.to_string())?;
    fs::rename(&tmp_path, path).map_err(|err| {
        let _ = fs::remove_file(&tmp_path);
        err.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache(name: &str) -> (PathBuf, MailCache) {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "mac-mail-cache-test-{name}-{}-{seq}",
            std::process::id()
        ));
        let cache = MailCache::with_root(&dir, "Alice@Example.com");
        (dir, cache)
    }

    #[test]
    fn folder_name_mapping_and_sorting() {
        let mut folders = vec![
            classify_folder("INBOX.Projects.2026", &[], Some(".")),
            classify_folder("INBOX.Trash", &[], Some(".")),
            classify_folder("INBOX.Drafts", &[], Some(".")),
            classify_folder("[Gmail]/Sent Mail", &["Sent".to_owned()], Some("/")),
            classify_folder("INBOX", &[], Some(".")),
            classify_folder("Custom/Receipts", &["Archive".to_owned()], Some("/")),
            classify_folder("Junk", &[], None),
        ];
        sort_folders(&mut folders);

        let names: Vec<(&str, FolderKind)> = folders
            .iter()
            .map(|f| (f.display_name.as_str(), f.kind))
            .collect();
        assert_eq!(
            names,
            vec![
                ("Inbox", FolderKind::Inbox),
                ("Drafts", FolderKind::Drafts),
                ("Sent", FolderKind::Sent),
                ("Archive", FolderKind::Archive),
                ("Junk", FolderKind::Junk),
                ("Trash", FolderKind::Trash),
                ("Projects / 2026", FolderKind::Other),
            ]
        );
        assert_eq!(
            find_special_folder(&folders, FolderKind::Drafts)
                .map(|f| f.raw_name.as_str()),
            Some("INBOX.Drafts")
        );
        assert_eq!(
            find_special_folder(&folders, FolderKind::Sent)
                .map(|f| f.raw_name.as_str()),
            Some("[Gmail]/Sent Mail")
        );
    }

    #[test]
    fn cache_persists_folders_headers_bodies_and_unread_metadata() {
        let (dir, cache) = temp_cache("persistence");

        let mut inbox = classify_folder("INBOX", &[], Some("."));
        inbox.unread = 2;
        inbox.total = Some(10);
        inbox.uid_validity = Some(42);
        cache.save_folders(&[inbox]).unwrap();

        let loaded_folders = cache.load_folders();
        assert_eq!(loaded_folders.len(), 1);
        assert_eq!(loaded_folders[0].unread, 2);
        assert_eq!(loaded_folders[0].uid_validity, Some(42));

        let summaries = vec![
            MessageSummary {
                uid: 102,
                from: "Bob".to_owned(),
                from_address: "bob@example.com".to_owned(),
                address: "bob@example.com".to_owned(),
                to: vec!["alice@example.com".to_owned()],
                cc: vec![],
                subject: "Second".to_owned(),
                date: "2026-11-14 10:00".to_owned(),
                timestamp: Some(1_700_000_200),
                unread: true,
                has_attachments: true,
                preview: "Hello there".to_owned(),
                message_id: Some("<102@example.com>".to_owned()),
                in_reply_to: None,
                references: vec![],
            },
            MessageSummary {
                uid: 101,
                from: "Carol".to_owned(),
                from_address: "carol@example.com".to_owned(),
                address: "carol@example.com".to_owned(),
                to: vec![],
                cc: vec![],
                subject: "First".to_owned(),
                date: "2026-11-14 09:00".to_owned(),
                timestamp: Some(1_700_000_100),
                unread: true,
                has_attachments: false,
                preview: String::new(),
                message_id: None,
                in_reply_to: None,
                references: vec![],
            },
        ];
        let merged = cache
            .merge_folder_headers("INBOX", Some(42), 10, 9, true, summaries, true)
            .unwrap();
        assert_eq!(merged.summaries.len(), 2);
        assert!(merged.has_more);

        // Cache a raw message and check offline load
        let raw = b"From: Bob <bob@example.com>\r\nSubject: Second\r\n\r\nBody!";
        cache.save_raw_message("INBOX", 102, raw).unwrap();
        assert!(cache.has_raw_message("INBOX", 102));
        assert_eq!(cache.load_raw_message("INBOX", 102).unwrap(), raw);

        // Mark read updates both message summary and folder unread count
        cache.set_message_unread("INBOX", 102, false).unwrap();
        let headers_after = cache.load_folder_headers("INBOX").unwrap();
        assert!(!headers_after.summaries[0].unread);
        assert_eq!(cache.load_folders()[0].unread, 1);

        // Removing an unread message decrements unread and deletes body
        cache.remove_message("INBOX", 101).unwrap();
        assert_eq!(cache.load_folders()[0].unread, 0);
        assert_eq!(cache.load_folder_headers("INBOX").unwrap().summaries.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn uidvalidity_change_invalidates_stale_folder_cache() {
        let (dir, cache) = temp_cache("uidvalidity");
        cache.save_raw_message("INBOX", 55, b"old raw").unwrap();
        cache
            .merge_folder_headers(
                "INBOX",
                Some(1),
                1,
                1,
                false,
                vec![MessageSummary {
                    uid: 55,
                    from: "Old".to_owned(),
                    from_address: "old@example.com".to_owned(),
                    address: "old@example.com".to_owned(),
                    to: vec![],
                    cc: vec![],
                    subject: "Old".to_owned(),
                    date: String::new(),
                    timestamp: None,
                    unread: false,
                    has_attachments: false,
                    preview: String::new(),
                    message_id: None,
                    in_reply_to: None,
                    references: vec![],
                }],
                true,
            )
            .unwrap();

        // Now UIDVALIDITY changes to 2
        let updated = cache
            .merge_folder_headers("INBOX", Some(2), 0, 1, false, vec![], true)
            .unwrap();
        assert!(updated.summaries.is_empty());
        assert!(!cache.has_raw_message("INBOX", 55));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pagination_selects_older_uids_in_descending_batches() {
        let all_uids = vec![5, 12, 18, 25, 30, 40, 45];
        let (page1, more1) = select_older_uid_page(all_uids.clone(), 40, 3);
        assert_eq!(page1, vec![30, 25, 18]);
        assert!(more1);

        let (page2, more2) = select_older_uid_page(all_uids.clone(), 18, 3);
        assert_eq!(page2, vec![12, 5]);
        assert!(!more2);

        let (empty, more3) = select_older_uid_page(all_uids, 1, 3);
        assert!(empty.is_empty());
        assert!(!more3);
    }

    #[test]
    fn address_extraction_and_contact_autocomplete() {
        let parsed = extract_email_addresses(
            "\"Doe, Jane\" <Jane.Doe@Example.com>, bob@work.org, invalid, \"\" <carol@test.io>",
        );
        assert_eq!(
            parsed,
            vec![
                (Some("Doe, Jane".to_owned()), "jane.doe@example.com".to_owned()),
                (None, "bob@work.org".to_owned()),
                (None, "carol@test.io".to_owned()),
            ]
        );

        let (dir, cache) = temp_cache("contacts");
        cache.harvest_contacts(&parsed, Some(1000)).unwrap();
        cache
            .harvest_contacts(&[(None, "bob@work.org".to_owned())], Some(2000))
            .unwrap();

        let suggestions = cache.suggest_contacts("bob", 5);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].email, "bob@work.org");
        assert_eq!(suggestions[0].frequency, 2);

        let jane = cache.suggest_contacts("jane", 5);
        assert_eq!(jane.len(), 1);
        assert_eq!(jane[0].name.as_deref(), Some("Doe, Jane"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn offline_header_search_matches_sender_subject_and_preview() {
        let (dir, cache) = temp_cache("search");
        cache
            .merge_folder_headers(
                "INBOX",
                Some(10),
                2,
                1,
                false,
                vec![
                    MessageSummary {
                        uid: 10,
                        from: "Alice".to_owned(),
                        from_address: "alice@acme.org".to_owned(),
                        address: "alice@acme.org".to_owned(),
                        to: vec![],
                        cc: vec![],
                        subject: "Quarterly Budget".to_owned(),
                        date: String::new(),
                        timestamp: None,
                        unread: false,
                        has_attachments: false,
                        preview: "See attached spreadsheet".to_owned(),
                        message_id: None,
                        in_reply_to: None,
                        references: vec![],
                    },
                    MessageSummary {
                        uid: 9,
                        from: "Bob".to_owned(),
                        from_address: "bob@other.org".to_owned(),
                        address: "bob@other.org".to_owned(),
                        to: vec![],
                        cc: vec![],
                        subject: "Lunch".to_owned(),
                        date: String::new(),
                        timestamp: None,
                        unread: true,
                        has_attachments: false,
                        preview: "Tacos at noon?".to_owned(),
                        message_id: None,
                        in_reply_to: None,
                        references: vec![],
                    },
                ],
                true,
            )
            .unwrap();

        assert_eq!(cache.search_cached_headers("INBOX", "spreadsheet").len(), 1);
        assert_eq!(cache.search_cached_headers("INBOX", "bob@other").len(), 1);
        assert_eq!(cache.search_cached_headers("INBOX", "budget")[0].uid, 10);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_top_preserves_exhausted_has_more_after_pagination() {
        let (dir, cache) = temp_cache("refresh-has-more");
        let make_msg = |uid: u32| MessageSummary {
            uid,
            from: "A".to_owned(),
            from_address: "a@example.com".to_owned(),
            address: "a@example.com".to_owned(),
            to: vec![],
            cc: vec![],
            subject: format!("Msg {uid}"),
            date: String::new(),
            timestamp: None,
            unread: false,
            has_attachments: false,
            preview: String::new(),
            message_id: None,
            in_reply_to: None,
            references: vec![],
        };

        // Initial top page (seqs 3..=4 -> UIDs 30, 40) with more older messages on server
        let p1 = cache
            .merge_folder_headers("INBOX", Some(1), 4, 3, true, vec![make_msg(40), make_msg(30)], true)
            .unwrap();
        assert!(p1.has_more);
        assert_eq!(p1.lowest_seq_loaded, 3);

        // Paginate to the end (UIDs 10, 20; has_more = false)
        let p2 = cache
            .merge_folder_headers("INBOX", Some(1), 4, 1, false, vec![make_msg(20), make_msg(10)], false)
            .unwrap();
        assert!(!p2.has_more);
        assert_eq!(p2.lowest_seq_loaded, 1);
        assert_eq!(p2.summaries.len(), 4);

        // Refresh top page (now UID 50 arrived; sync_headers passes has_more = true for top slice)
        let refreshed = cache
            .merge_folder_headers("INBOX", Some(1), 5, 4, true, vec![make_msg(50), make_msg(40)], true)
            .unwrap();
        assert!(!refreshed.has_more);
        assert_eq!(refreshed.lowest_seq_loaded, 1);
        assert_eq!(refreshed.summaries.len(), 5);

        let _ = fs::remove_dir_all(&dir);
    }
}
