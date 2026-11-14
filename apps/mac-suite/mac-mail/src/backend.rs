// SPDX-License-Identifier: GPL-3.0-only

//! High-level mail backend combining persistent IMAP sessions, local disk cache,
//! pagination, SMTP sending with Sent append, draft saving, message actions,
//! and INBOX IDLE notifications.

use std::{
    fs,
    path::{Path, PathBuf},
};

use imap::types::Flag;
use lettre::{
    Message, SmtpTransport, Transport,
    address::Envelope,
    transport::smtp::authentication::{Credentials, Mechanism},
};
use mail_parser::{AddressList, MessageParser};

use crate::{
    account::{self, Account, AccountCredential},
    cache::{
        CachedFolderHeaders, FolderInfo, FolderKind, KnownContact, MailCache, MessageSummary,
        classify_folder, extract_email_addresses, find_special_folder, select_older_uid_page,
        sort_folders,
    },
    imap_pool::{IdleWatcher, ImapPool, NewMailNotification, parse_appended_uid},
};

/// Default number of messages fetched per page.
pub const DEFAULT_PAGE_SIZE: usize = 50;

/// Result of loading or paginating a folder's headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderPage {
    pub folder: String,
    pub summaries: Vec<MessageSummary>,
    pub total_exists: u32,
    pub has_more: bool,
    pub from_cache: bool,
}

/// Metadata for a file attachment inside an opened message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentInfo {
    pub index: usize,
    pub filename: String,
    pub mime_type: String,
    pub size: usize,
}

/// Inline `cid:` part extracted from a MIME message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlinePart {
    pub content_id: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// Full parsed content of an opened message, including raw RFC 822 bytes for
/// downstream HTML rendering and reply/forward threading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedMessage {
    pub uid: u32,
    pub subject: String,
    pub from: String,
    pub from_address: String,
    pub reply_to: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub date: String,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub text: String,
    pub html: Option<String>,
    pub raw: Vec<u8>,
    pub attachments: Vec<AttachmentInfo>,
    pub inline_parts: Vec<InlinePart>,
    pub from_cache: bool,
}

/// Unified mail backend owning a persistent IMAP pool and an on-disk cache.
#[derive(Clone)]
pub struct MailBackend {
    pool: ImapPool,
    cache: MailCache,
}

impl MailBackend {
    /// Create a backend for `account` using the default `$XDG_CACHE_HOME/mac-mail` cache.
    pub fn new(account: Account, credential: AccountCredential) -> Self {
        let cache = MailCache::new(&account.email);
        let pool = ImapPool::new(account, credential);
        Self { pool, cache }
    }

    /// Create a backend with a custom cache root directory (primarily for tests).
    pub fn with_cache_root(
        account: Account,
        credential: AccountCredential,
        cache_root: impl AsRef<Path>,
    ) -> Self {
        let cache = MailCache::with_root(cache_root, &account.email);
        let pool = ImapPool::new(account, credential);
        Self { pool, cache }
    }

    pub fn account(&self) -> Account {
        self.pool.account()
    }

    pub fn pool(&self) -> &ImapPool {
        &self.pool
    }

    pub fn cache(&self) -> &MailCache {
        &self.cache
    }

    pub fn is_connected(&self) -> bool {
        self.pool.is_connected()
    }

    pub fn disconnect(&self) {
        self.pool.disconnect();
    }

    /// Start the background `INBOX` `IDLE` watcher with desktop notifications.
    pub fn start_idle_watcher<F>(&self, on_new_mail: F) -> IdleWatcher
    where
        F: Fn(NewMailNotification) + Send + Sync + 'static,
    {
        IdleWatcher::start(self.pool.account(), self.pool.credential(), on_new_mail)
    }

    // -------------------------------------------------------------------------
    // Folders & Unread Metadata
    // -------------------------------------------------------------------------

    /// Immediately return cached folders (sorted, with friendly names & unread counts).
    pub fn cached_folders(&self) -> Vec<FolderInfo> {
        let mut folders = self.cache.load_folders();
        sort_folders(&mut folders);
        folders
    }

    /// Fetch the live folder list and per-folder unread counts from IMAP, updating the cache.
    pub fn sync_folders(&self) -> Result<Vec<FolderInfo>, String> {
        let previous = self.cache.load_folders();
        let mut folders = self.pool.with_session(None, false, |session, _, _| {
            let names = session.list(None, Some("*"))?;
            let mut result = Vec::new();
            for name in names.iter() {
                let attr_names: Vec<String> = name
                    .attributes()
                    .iter()
                    .map(|attr| format!("{attr:?}"))
                    .collect();
                if attr_names
                    .iter()
                    .any(|attr| attr.eq_ignore_ascii_case("NoSelect"))
                {
                    continue;
                }
                let mut info = classify_folder(name.name(), &attr_names, name.delimiter());
                match session.status(name.name(), "(MESSAGES UNSEEN UIDVALIDITY)") {
                    Ok(status) => {
                        info.unread = status.unseen.unwrap_or(0);
                        info.total = Some(status.exists);
                        info.uid_validity = status.uid_validity;
                    }
                    Err(_) => {
                        if let Some(prev) = previous.iter().find(|f| f.raw_name == info.raw_name) {
                            info.unread = prev.unread;
                            info.total = prev.total;
                            info.uid_validity = prev.uid_validity;
                        }
                    }
                }
                result.push(info);
            }
            Ok(result)
        })?;

        sort_folders(&mut folders);
        self.cache.save_folders(&folders)?;
        Ok(folders)
    }

    /// Find a special-use folder (Drafts, Sent, Archive, Trash, Junk) from cache or fallback.
    pub fn find_special_folder(&self, kind: FolderKind) -> Option<FolderInfo> {
        let folders = self.cache.load_folders();
        find_special_folder(&folders, kind).cloned()
    }

    // -------------------------------------------------------------------------
    // Headers & Pagination
    // -------------------------------------------------------------------------

    /// Immediately return cached headers for `folder` if present on disk.
    pub fn cached_headers(&self, folder: &str) -> Option<FolderPage> {
        let cached = self.cache.load_folder_headers(folder)?;
        Some(FolderPage {
            folder: folder.to_owned(),
            summaries: cached.summaries,
            total_exists: cached.total_exists,
            has_more: cached.has_more,
            from_cache: true,
        })
    }

    /// Fetch the newest `page_size` message headers for `folder` and cache them.
    pub fn sync_headers(&self, folder: &str, page_size: usize) -> Result<FolderPage, String> {
        let page_size = page_size.max(1) as u32;
        let (mailbox, summaries, unseen_count) =
            self.pool.with_session(Some(folder), true, |session, mailbox, _| {
                let mailbox = mailbox.cloned().unwrap_or_default();
                if mailbox.exists == 0 {
                    return Ok((mailbox, Vec::new(), 0));
                }
                let first = mailbox.exists.saturating_sub(page_size - 1).max(1);
                let fetched = session.fetch(
                    format!("{first}:{}", mailbox.exists),
                    "(UID FLAGS BODYSTRUCTURE BODY.PEEK[HEADER] BODY.PEEK[TEXT]<0.2048>)",
                )?;
                let summaries = summarize_fetches(&fetched);
                let unseen_count = if first == 1 {
                    summaries.iter().filter(|m| m.unread).count() as u32
                } else {
                    session
                        .uid_search("UNSEEN")
                        .map(|uids| uids.len() as u32)
                        .unwrap_or_else(|_| summaries.iter().filter(|m| m.unread).count() as u32)
                };
                Ok((mailbox, summaries, unseen_count))
            })?;

        self.harvest_from_summaries(&summaries);

        let first = mailbox.exists.saturating_sub(page_size - 1).max(1);
        let has_more = first > 1;
        let mut merged = self.cache.merge_folder_headers(
            folder,
            mailbox.uid_validity,
            mailbox.exists,
            first,
            has_more,
            summaries,
            true,
        )?;
        // Attach cached previews when raw body exists on disk.
        self.populate_cached_previews(folder, &mut merged);

        let _ = self.cache.update_folder_metadata(
            folder,
            unseen_count,
            Some(mailbox.exists),
            mailbox.uid_validity,
        );

        Ok(FolderPage {
            folder: folder.to_owned(),
            summaries: merged.summaries,
            total_exists: merged.total_exists,
            has_more: merged.has_more,
            from_cache: false,
        })
    }

    /// Load the next batch of older messages strictly older than `before_uid`.
    pub fn load_older_headers(
        &self,
        folder: &str,
        before_uid: u32,
        page_size: usize,
    ) -> Result<FolderPage, String> {
        if before_uid <= 1 || page_size == 0 {
            let existing = self.cache.load_folder_headers(folder).unwrap_or_default();
            return Ok(FolderPage {
                folder: folder.to_owned(),
                summaries: existing.summaries,
                total_exists: existing.total_exists,
                has_more: false,
                from_cache: true,
            });
        }

        let (mailbox, older_summaries, has_more) =
            self.pool.with_session(Some(folder), false, |session, mailbox, _| {
                let mailbox = mailbox.cloned().unwrap_or_default();
                let uid_set = session.uid_search(format!("UID 1:{}", before_uid - 1))?;
                let candidate_uids: Vec<u32> = uid_set.into_iter().collect();
                let (page_uids, has_more) =
                    select_older_uid_page(candidate_uids, before_uid, page_size);
                if page_uids.is_empty() {
                    return Ok((mailbox, Vec::new(), false));
                }
                let set = page_uids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                let fetched = session.uid_fetch(
                    set,
                    "(UID FLAGS BODYSTRUCTURE BODY.PEEK[HEADER] BODY.PEEK[TEXT]<0.2048>)",
                )?;
                Ok((mailbox, summarize_fetches(&fetched), has_more))
            })?;

        self.harvest_from_summaries(&older_summaries);

        let existing_lowest = self
            .cache
            .load_folder_headers(folder)
            .map_or(0, |h| h.lowest_seq_loaded);
        let next_lowest = if has_more {
            existing_lowest
                .saturating_sub(older_summaries.len() as u32)
                .max(2)
        } else {
            1
        };

        let mut merged = self.cache.merge_folder_headers(
            folder,
            mailbox.uid_validity,
            mailbox.exists,
            next_lowest,
            has_more,
            older_summaries,
            false,
        )?;
        self.populate_cached_previews(folder, &mut merged);

        Ok(FolderPage {
            folder: folder.to_owned(),
            summaries: merged.summaries,
            total_exists: merged.total_exists,
            has_more: merged.has_more,
            from_cache: false,
        })
    }

    fn populate_cached_previews(&self, folder: &str, headers: &mut CachedFolderHeaders) {
        let mut updated = false;
        for summary in &mut headers.summaries {
            if summary.preview.is_empty()
                && let Some(raw) = self.cache.load_raw_message(folder, summary.uid)
                && let Ok(opened) = parse_raw_message(summary.uid, &raw, true)
            {
                let preview = extract_preview(&opened.text, opened.html.as_deref());
                if !preview.is_empty() {
                    summary.preview = preview;
                    updated = true;
                }
            }
        }
        if updated {
            let _ = self.cache.save_folder_headers(folder, headers);
        }
    }

    // -------------------------------------------------------------------------
    // Opening Messages & Attachments
    // -------------------------------------------------------------------------

    /// Open a message, serving from the local disk cache immediately if available,
    /// otherwise fetching via the persistent IMAP session and caching to disk.
    /// Also marks the message as read (`\Seen`).
    pub fn open_message(&self, folder: &str, uid: u32) -> Result<OpenedMessage, String> {
        if let Some(raw) = self.cache.load_raw_message(folder, uid) {
            let opened = parse_raw_message(uid, &raw, true)?;
            // Best-effort flag sync if currently marked unread; ignore offline errors.
            let was_unread = self
                .cache
                .load_folder_headers(folder)
                .and_then(|h| h.summaries.into_iter().find(|s| s.uid == uid))
                .is_some_and(|s| s.unread);
            if was_unread {
                let _ = self.cache.set_message_unread(folder, uid, false);
                let _ = self.pool.with_session(Some(folder), false, |session, _, _| {
                    session.uid_store(uid.to_string(), "+FLAGS (\\Seen)")?;
                    Ok(())
                });
            }
            let preview = extract_preview(&opened.text, opened.html.as_deref());
            let _ = self.cache.update_message_preview(folder, uid, &preview);
            self.harvest_from_opened(&opened);
            return Ok(opened);
        }

        let raw = self
            .pool
            .with_session(Some(folder), false, |session, _, _| {
                let fetched = session.uid_fetch(uid.to_string(), "BODY.PEEK[]")?;
                let bytes = fetched
                    .iter()
                    .next()
                    .and_then(|fetch| fetch.body())
                    .map(<[u8]>::to_vec);
                if bytes.is_some() {
                    let _ = session.uid_store(uid.to_string(), "+FLAGS (\\Seen)");
                }
                Ok(bytes)
            })?
            .ok_or_else(|| "The message could not be read".to_owned())?;

        let _ = self.cache.save_raw_message(folder, uid, &raw);
        let _ = self.cache.set_message_unread(folder, uid, false);
        let opened = parse_raw_message(uid, &raw, false)?;
        let preview = extract_preview(&opened.text, opened.html.as_deref());
        let _ = self.cache.update_message_preview(folder, uid, &preview);
        self.harvest_from_opened(&opened);
        Ok(opened)
    }

    /// Save attachment `index` of a message to `target_dir` (defaults to `~/Downloads`)
    /// without overwriting an existing file. Works offline if the message is cached.
    pub fn save_attachment(
        &self,
        folder: &str,
        uid: u32,
        index: usize,
        target_dir: Option<&Path>,
    ) -> Result<PathBuf, String> {
        let raw = match self.cache.load_raw_message(folder, uid) {
            Some(bytes) => bytes,
            None => {
                let fetched = self
                    .pool
                    .with_session(Some(folder), false, |session, _, _| {
                        let fetched = session.uid_fetch(uid.to_string(), "BODY.PEEK[]")?;
                        Ok(fetched
                            .iter()
                            .next()
                            .and_then(|fetch| fetch.body())
                            .map(<[u8]>::to_vec))
                    })?
                    .ok_or_else(|| "The message could not be read".to_owned())?;
                let _ = self.cache.save_raw_message(folder, uid, &fetched);
                fetched
            }
        };

        let parsed = MessageParser::new()
            .parse(&raw)
            .ok_or("The message could not be read")?;
        let part = file_attachments(&parsed)
            .nth(index)
            .ok_or("Attachment not found")?;
        let name = part
            .attachment_name()
            .unwrap_or("attachment")
            .replace(['/', '\\'], "_");
        let dir = match target_dir {
            Some(dir) => dir.to_path_buf(),
            None => std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or("No home folder")?
                .join("Downloads"),
        };
        fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
        let mut path = dir.join(&name);
        let mut n = 1;
        while path.exists() {
            n += 1;
            path = dir.join(format!("{n}-{name}"));
        }
        fs::write(&path, part.decoded()).map_err(|err| err.to_string())?;
        Ok(path)
    }

    // -------------------------------------------------------------------------
    // Search
    // -------------------------------------------------------------------------

    /// Search `folder` for `query`. Uses IMAP `UID SEARCH` when online and falls
    /// back to searching cached headers when offline.
    pub fn search(&self, folder: &str, query: &str) -> Result<Vec<MessageSummary>, String> {
        let clean: String = query.chars().filter(|c| !matches!(c, '"' | '\\')).collect();
        let live = self.pool.with_session(Some(folder), false, |session, _, _| {
            let uids = session.uid_search(format!("TEXT \"{clean}\""))?;
            if uids.is_empty() {
                return Ok(Vec::new());
            }
            let mut uids: Vec<u32> = uids.into_iter().collect();
            uids.sort_unstable_by(|a, b| b.cmp(a));
            uids.truncate(DEFAULT_PAGE_SIZE);
            let set = uids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let fetched = session.uid_fetch(
                set,
                "(UID FLAGS BODYSTRUCTURE BODY.PEEK[HEADER] BODY.PEEK[TEXT]<0.2048>)",
            )?;
            let mut summaries = summarize_fetches(&fetched);
            summaries.sort_by_key(|s| std::cmp::Reverse(s.uid));
            Ok(summaries)
        });

        match live {
            Ok(results) => Ok(results),
            Err(err) => {
                let offline = self.cache.search_cached_headers(folder, query);
                if offline.is_empty() {
                    Err(err)
                } else {
                    Ok(offline)
                }
            }
        }
    }

    // -------------------------------------------------------------------------
    // Phase 2 & 3 Actions (Read/Unread, Move, Archive, Trash, Drafts, Send)
    // -------------------------------------------------------------------------

    /// Mark a message as unread (`unread = true`) or read (`unread = false`).
    pub fn set_unread(&self, folder: &str, uid: u32, unread: bool) -> Result<(), String> {
        let flag_op = if unread {
            "-FLAGS (\\Seen)"
        } else {
            "+FLAGS (\\Seen)"
        };
        self.pool.with_session(Some(folder), false, |session, _, _| {
            session.uid_store(uid.to_string(), flag_op)?;
            Ok(())
        })?;
        self.cache.set_message_unread(folder, uid, unread)
    }

    /// Move a message to `target_folder`, or permanently expunge it if `target_folder` is `None`.
    pub fn move_message(
        &self,
        folder: &str,
        uid: u32,
        target_folder: Option<&str>,
    ) -> Result<(), String> {
        self.pool
            .with_session(Some(folder), false, |session, _, caps| {
                let uid_str = uid.to_string();
                match target_folder {
                    Some(target) if caps.has_move => {
                        session.uid_mv(&uid_str, target)?;
                    }
                    Some(target) => {
                        let quoted = quote_imap_mailbox(target);
                        session.uid_copy(&uid_str, quoted)?;
                        session.uid_store(&uid_str, "+FLAGS (\\Deleted)")?;
                        session.expunge()?;
                    }
                    None => {
                        session.uid_store(&uid_str, "+FLAGS (\\Deleted)")?;
                        session.expunge()?;
                    }
                }
                Ok(())
            })?;
        self.cache.remove_message(folder, uid)
    }

    /// Move a message to the account's Archive / All Mail folder.
    pub fn archive_message(&self, folder: &str, uid: u32) -> Result<(), String> {
        let target = self
            .find_special_folder(FolderKind::Archive)
            .map(|f| f.raw_name)
            .ok_or_else(|| "No Archive folder found".to_owned())?;
        self.move_message(folder, uid, Some(&target))
    }

    /// Move a message to Trash, or permanently delete it if already inside Trash.
    pub fn trash_or_delete_message(&self, folder: &str, uid: u32) -> Result<(), String> {
        let current_is_trash = self
            .cache
            .load_folders()
            .into_iter()
            .find(|f| f.raw_name == folder)
            .is_some_and(|f| f.kind == FolderKind::Trash)
            || folder_looks_like_trash(folder);
        let target = if current_is_trash {
            None
        } else {
            self.find_special_folder(FolderKind::Trash)
                .map(|f| f.raw_name)
        };
        self.move_message(folder, uid, target.as_deref())
    }

    /// Save a draft RFC 822 message to the Drafts folder (`\Draft \Seen`), replacing
    /// `existing_draft_uid` if provided. Returns the new UID if the server supports `UIDPLUS`.
    pub fn save_draft(
        &self,
        raw_rfc822: &[u8],
        existing_draft_uid: Option<u32>,
    ) -> Result<Option<u32>, String> {
        let drafts_folder = self
            .find_special_folder(FolderKind::Drafts)
            .map(|f| f.raw_name)
            .unwrap_or_else(|| "Drafts".to_owned());

        let new_uid = self.pool.with_session(None, false, |session, _, _| {
            let appended = session
                .append(&drafts_folder, raw_rfc822)
                .flags([Flag::Draft, Flag::Seen])
                .finish()?;
            if let Some(old_uid) = existing_draft_uid {
                session.select(&drafts_folder)?;
                session.uid_store(old_uid.to_string(), "+FLAGS (\\Deleted)")?;
                session.expunge()?;
            }
            Ok(parse_appended_uid(&appended))
        })?;
        // Selection changed if we expunged an old draft; ensure pool state reflects that.
        if existing_draft_uid.is_some() {
            self.pool.invalidate_selected();
        }
        Ok(new_uid)
    }

    /// Delete a draft by UID from the Drafts folder (for instance right after sending).
    pub fn delete_draft(&self, draft_uid: u32) -> Result<(), String> {
        let drafts_folder = self
            .find_special_folder(FolderKind::Drafts)
            .map(|f| f.raw_name)
            .unwrap_or_else(|| "Drafts".to_owned());
        self.move_message(&drafts_folder, draft_uid, None)
    }

    /// Append a sent message (`\Seen`) to the Sent folder when the SMTP server does not do so.
    pub fn append_to_sent(&self, raw_rfc822: &[u8]) -> Result<Option<u32>, String> {
        let sent_folder = self
            .find_special_folder(FolderKind::Sent)
            .map(|f| f.raw_name)
            .unwrap_or_else(|| "Sent".to_owned());
        let uid = self.pool.with_session(None, false, |session, _, _| {
            let appended = session
                .append(&sent_folder, raw_rfc822)
                .flag(Flag::Seen)
                .finish()?;
            Ok(parse_appended_uid(&appended))
        })?;
        Ok(uid)
    }

    /// Send a built `lettre::Message` via SMTP, optionally append to Sent when needed,
    /// harvest recipient addresses, and optionally remove an old draft.
    pub fn send_message(
        &self,
        message: &Message,
        delete_draft_uid: Option<u32>,
    ) -> Result<(), String> {
        let account = self.pool.account();
        let mut credential = self.pool.credential();
        account::ensure_fresh_credential(&account, &mut credential)?;
        if credential.is_oauth2() {
            self.pool.set_credentials(account.clone(), credential.clone());
        }
        let transport = build_smtp_transport(&account, &credential)?;
        transport
            .send(message)
            .map_err(|err| format!("Could not send: {err}"))?;

        let raw = message.formatted();
        self.post_send_housekeeping(&account, &raw, delete_draft_uid);
        Ok(())
    }

    /// Send raw RFC 822 bytes via SMTP using an explicit envelope (`from` + `recipients`),
    /// append to Sent if the SMTP server does not do so automatically, harvest recipients,
    /// and optionally delete `delete_draft_uid`.
    pub fn send_raw_message(
        &self,
        raw_rfc822: &[u8],
        from: &str,
        recipients: &[String],
        delete_draft_uid: Option<u32>,
    ) -> Result<(), String> {
        let account = self.pool.account();
        let mut credential = self.pool.credential();
        account::ensure_fresh_credential(&account, &mut credential)?;
        if credential.is_oauth2() {
            self.pool.set_credentials(account.clone(), credential.clone());
        }
        let from_addr = from
            .parse()
            .map_err(|_| "Your address is not valid".to_owned())?;
        let mut to_addrs = Vec::with_capacity(recipients.len());
        for r in recipients {
            let addr = r
                .trim()
                .parse()
                .map_err(|_| format!("Recipient address is not valid: {r}"))?;
            to_addrs.push(addr);
        }
        let envelope = Envelope::new(Some(from_addr), to_addrs)
            .map_err(|err| format!("Invalid envelope: {err}"))?;

        let transport = build_smtp_transport(&account, &credential)?;
        transport
            .send_raw(&envelope, raw_rfc822)
            .map_err(|err| format!("Could not send: {err}"))?;

        self.post_send_housekeeping(&account, raw_rfc822, delete_draft_uid);
        Ok(())
    }

    fn post_send_housekeeping(
        &self,
        account: &Account,
        raw_rfc822: &[u8],
        delete_draft_uid: Option<u32>,
    ) {
        if let Some(parsed) = MessageParser::new().parse(raw_rfc822) {
            let mut entries = Vec::new();
            collect_address_list(parsed.to(), &mut entries);
            collect_address_list(parsed.cc(), &mut entries);
            collect_address_list(parsed.bcc(), &mut entries);
            let _ = self.cache.harvest_contacts(&entries, None);
        }
        if should_append_to_sent(&account.smtp_host) {
            let _ = self.append_to_sent(raw_rfc822);
        }
        if let Some(draft_uid) = delete_draft_uid {
            let _ = self.delete_draft(draft_uid);
        }
    }

    // -------------------------------------------------------------------------
    // Contacts / Address Harvesting
    // -------------------------------------------------------------------------

    /// All harvested contacts ordered by frequency/recency.
    pub fn known_contacts(&self) -> Vec<KnownContact> {
        self.cache.load_contacts()
    }

    /// Suggest up to `limit` contacts matching `query` for compose autocomplete.
    pub fn suggest_contacts(&self, query: &str, limit: usize) -> Vec<KnownContact> {
        self.cache.suggest_contacts(query, limit)
    }

    fn harvest_from_summaries(&self, summaries: &[MessageSummary]) {
        let mut entries = Vec::new();
        for s in summaries {
            if !s.from_address.is_empty() {
                entries.push((
                    (!s.from.eq_ignore_ascii_case(&s.from_address)).then(|| s.from.clone()),
                    s.from_address.clone(),
                ));
            }
            for raw in s.to.iter().chain(s.cc.iter()) {
                entries.extend(extract_email_addresses(raw));
            }
        }
        let _ = self.cache.harvest_contacts(&entries, None);
    }

    fn harvest_from_opened(&self, opened: &OpenedMessage) {
        let mut entries = Vec::new();
        if !opened.from_address.is_empty() {
            entries.push((
                (!opened.from.eq_ignore_ascii_case(&opened.from_address))
                    .then(|| opened.from.clone()),
                opened.from_address.clone(),
            ));
        }
        for raw in opened.to.iter().chain(opened.cc.iter()) {
            entries.extend(extract_email_addresses(raw));
        }
        let _ = self.cache.harvest_contacts(&entries, None);
    }
}

/// Whether `append_to_sent` should copy sent messages into the Sent IMAP folder.
/// Gmail/Googlemail automatically saves every message sent via SMTP into Sent Mail;
/// standard SMTP relays generally do not.
pub fn should_append_to_sent(smtp_host: &str) -> bool {
    let lower = smtp_host.trim().to_ascii_lowercase();
    !matches!(
        lower.as_str(),
        "smtp.gmail.com" | "smtp.googlemail.com"
    )
}

/// Quote a mailbox name as an IMAP quoted-string (required by `Session::uid_copy`
/// which passes `mailbox_name` unquoted).
pub fn quote_imap_mailbox(mailbox: &str) -> String {
    let escaped = mailbox.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn folder_looks_like_trash(folder: &str) -> bool {
    let lower = folder.to_ascii_lowercase();
    lower.contains("trash") || lower.contains("deleted") || lower.ends_with("bin")
}

fn build_smtp_transport(
    account: &Account,
    credential: &AccountCredential,
) -> Result<SmtpTransport, String> {
    let builder = if account.smtp_port == 465 {
        SmtpTransport::relay(&account.smtp_host)
    } else {
        SmtpTransport::starttls_relay(&account.smtp_host)
    }
    .map_err(|err| err.to_string())?;
    let mut configured = builder.port(account.smtp_port).credentials(Credentials::new(
        account.email.clone(),
        credential.secret_for_imap().to_owned(),
    ));
    if credential.is_oauth2() {
        configured = configured.authentication(vec![Mechanism::Xoauth2]);
    }
    Ok(configured.build())
}

fn summarize_fetches(fetched: &imap::types::Fetches) -> Vec<MessageSummary> {
    let parser = MessageParser::new();
    let mut summaries: Vec<MessageSummary> = fetched
        .iter()
        .filter_map(|fetch| {
            let uid = fetch.uid?;
            let raw_header = fetch.header()?;
            let mut combined;
            let message = if let Some(text_bytes) = fetch.text() {
                combined = Vec::with_capacity(raw_header.len() + 2 + text_bytes.len());
                combined.extend_from_slice(raw_header);
                if !raw_header.ends_with(b"\r\n\r\n") && !raw_header.ends_with(b"\n\n") {
                    combined.extend_from_slice(b"\r\n");
                }
                combined.extend_from_slice(text_bytes);
                parser.parse(&combined)?
            } else {
                parser.parse(raw_header)?
            };
            let from_mb = message.from().and_then(|list| list.first());
            let from = from_mb
                .map(|mailbox| {
                    mailbox
                        .name()
                        .filter(|name| !name.is_empty())
                        .or(mailbox.address())
                        .unwrap_or_default()
                        .to_owned()
                })
                .unwrap_or_default();
            let from_address = from_mb
                .and_then(|mailbox| mailbox.address())
                .unwrap_or_default()
                .to_owned();
            let address = message
                .reply_to()
                .or_else(|| message.from())
                .and_then(|list| list.first())
                .and_then(|mailbox| mailbox.address())
                .unwrap_or_default()
                .to_owned();
            let to = format_address_list(message.to());
            let cc = format_address_list(message.cc());
            let (date, timestamp) = match message.date() {
                Some(d) => (
                    format!(
                        "{:04}-{:02}-{:02} {:02}:{:02}",
                        d.year, d.month, d.day, d.hour, d.minute
                    ),
                    Some(d.to_timestamp()),
                ),
                None => (String::new(), None),
            };
            let bs_dbg = fetch.bodystructure().map(|bs| format!("{bs:?}"));
            let ct_hdr = message
                .content_type()
                .map(|ct| format!("{}/{}", ct.ctype(), ct.subtype().unwrap_or("")));
            let has_attachments =
                detect_has_attachments(bs_dbg.as_deref(), ct_hdr.as_deref());
            let preview = if fetch.text().is_some() {
                let text = message.body_text(0).map(|t| t.into_owned()).unwrap_or_default();
                let html = message.body_html(0).map(|h| h.into_owned());
                extract_preview(&text, html.as_deref())
            } else {
                String::new()
            };
            let message_id = message.message_id().map(str::to_owned);
            let in_reply_to = message
                .in_reply_to()
                .and_then(|list| list.first())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let references = message
                .references()
                .map(|list| list.iter().map(str::to_owned).collect())
                .unwrap_or_default();

            Some(MessageSummary {
                uid,
                from,
                from_address,
                address,
                to,
                cc,
                subject: message.subject().unwrap_or("(no subject)").to_owned(),
                date,
                timestamp,
                unread: !fetch.flags().iter().any(|flag| matches!(flag, Flag::Seen)),
                has_attachments,
                preview,
                message_id,
                in_reply_to,
                references,
            })
        })
        .collect();
    summaries.sort_by_key(|s| std::cmp::Reverse(s.uid));
    summaries
}

/// Filter `message.attachments()` to exclude inline `cid:` parts unless explicitly marked
/// `Content-Disposition: attachment`, keeping indices consistent with `save_attachment`.
fn file_attachments<'a>(
    message: &'a mail_parser::Message<'a>,
) -> impl Iterator<Item = mail_parser::MessagePart<'a>> + 'a {
    message.attachments().filter(|part| {
        let is_explicit_attachment = part
            .content_disposition()
            .is_some_and(|cd| cd.ctype().eq_ignore_ascii_case("attachment"));
        if is_explicit_attachment {
            return true;
        }
        part.content_id().is_none()
    })
}

/// Detect whether a message carries file attachments from IMAP `BODYSTRUCTURE` debug text
/// or top-level `Content-Type` header.
pub fn detect_has_attachments(
    bodystructure_dbg: Option<&str>,
    top_content_type: Option<&str>,
) -> bool {
    if let Some(bs) = bodystructure_dbg {
        let lower = bs.to_ascii_lowercase();
        if lower.contains("disposition: some(contentdisposition { ty: \"attachment\"") {
            return true;
        }
    }
    if let Some(ct) = top_content_type {
        let lower = ct.to_ascii_lowercase();
        if lower.starts_with("multipart/mixed") {
            return true;
        }
    }
    false
}

/// Parse raw RFC 822 bytes into a full `OpenedMessage`.
pub fn parse_raw_message(
    uid: u32,
    raw: &[u8],
    from_cache: bool,
) -> Result<OpenedMessage, String> {
    let message = MessageParser::new()
        .parse(raw)
        .ok_or_else(|| "The message could not be read".to_owned())?;

    let from_mb = message.from().and_then(|list| list.first());
    let from = from_mb
        .map(|mailbox| {
            mailbox
                .name()
                .filter(|name| !name.is_empty())
                .or(mailbox.address())
                .unwrap_or_default()
                .to_owned()
        })
        .unwrap_or_default();
    let from_address = from_mb
        .and_then(|mailbox| mailbox.address())
        .unwrap_or_default()
        .to_owned();
    let reply_to = message
        .reply_to()
        .and_then(|list| list.first())
        .and_then(|mailbox| mailbox.address())
        .map(str::to_owned);
    let to = format_address_list(message.to());
    let cc = format_address_list(message.cc());
    let date = message
        .date()
        .map(|d| {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                d.year, d.month, d.day, d.hour, d.minute
            )
        })
        .unwrap_or_default();
    let message_id = message.message_id().map(str::to_owned);
    let in_reply_to = message
        .in_reply_to()
        .and_then(|list| list.first())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let references = message
        .references()
        .map(|list| list.iter().map(str::to_owned).collect())
        .unwrap_or_default();

    let html = message.body_html(0).map(|h| h.into_owned());
    let text = message
        .body_text(0)
        .map(|t| t.into_owned())
        .or_else(|| html.as_deref().map(strip_html_to_text))
        .unwrap_or_else(|| "(this message has no text)".to_owned());

    let attachments = file_attachments(&message)
        .enumerate()
        .map(|(index, part)| {
            let mime_type = part
                .content_type()
                .map(|ct| match ct.subtype() {
                    Some(sub) => format!("{}/{sub}", ct.ctype()),
                    None => ct.ctype().to_owned(),
                })
                .unwrap_or_else(|| "application/octet-stream".to_owned());
            let decoded = part.decoded();
            AttachmentInfo {
                index,
                filename: part.attachment_name().unwrap_or("attachment").to_owned(),
                mime_type,
                size: decoded.len(),
            }
        })
        .collect();

    let mut inline_parts = Vec::new();
    for part in message.parts() {
        if let Some(cid) = part.content_id() {
            let mime_type = part
                .content_type()
                .map(|ct| match ct.subtype() {
                    Some(sub) => format!("{}/{sub}", ct.ctype()),
                    None => ct.ctype().to_owned(),
                })
                .unwrap_or_else(|| "application/octet-stream".to_owned());
            inline_parts.push(InlinePart {
                content_id: cid.trim_matches(['<', '>']).to_owned(),
                mime_type,
                data: part.decoded().into_owned(),
            });
        }
    }

    Ok(OpenedMessage {
        uid,
        subject: message.subject().unwrap_or("(no subject)").to_owned(),
        from,
        from_address,
        reply_to,
        to,
        cc,
        date,
        message_id,
        in_reply_to,
        references,
        text,
        html,
        raw: raw.to_vec(),
        attachments,
        inline_parts,
        from_cache,
    })
}

fn format_address_list(list: Option<AddressList<'_>>) -> Vec<String> {
    let Some(list) = list else {
        return Vec::new();
    };
    list.mailboxes()
        .filter_map(|mb| {
            let addr = mb.address()?;
            match mb.name().filter(|n| !n.is_empty()) {
                Some(name) => Some(format!("{name} <{addr}>")),
                None => Some(addr.to_owned()),
            }
        })
        .collect()
}

fn collect_address_list(
    list: Option<AddressList<'_>>,
    out: &mut Vec<(Option<String>, String)>,
) {
    let Some(list) = list else {
        return;
    };
    for mb in list.mailboxes() {
        if let Some(addr) = mb.address() {
            let name = mb
                .name()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_owned);
            out.push((name, addr.to_ascii_lowercase()));
        }
    }
}

/// Produce a concise two-line preview string (<= 140 chars) from plain text or HTML.
pub fn extract_preview(text: &str, html: Option<&str>) -> String {
    let source = if !text.trim().is_empty() && text != "(this message has no text)" {
        text.to_owned()
    } else if let Some(html_body) = html {
        strip_html_to_text(html_body)
    } else {
        return String::new();
    };

    let collapsed: String = source.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let preview: String = chars.by_ref().take(140).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn strip_html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut skip_block = false;
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !in_tag && bytes[i] == b'<' {
            if lower[i..].starts_with("<style") || lower[i..].starts_with("<script") {
                skip_block = true;
            } else if lower[i..].starts_with("</style") || lower[i..].starts_with("</script") {
                skip_block = false;
            }
            in_tag = true;
            i += 1;
            continue;
        }
        if in_tag {
            if bytes[i] == b'>' {
                in_tag = false;
                if !skip_block {
                    out.push(' ');
                }
            }
            i += 1;
            continue;
        }
        if !skip_block
            && let Some(ch) = html[i..].chars().next()
        {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sent_append_policy_skips_gmail_and_appends_others() {
        assert!(!should_append_to_sent("smtp.gmail.com"));
        assert!(!should_append_to_sent("SMTP.GOOGLEMAIL.COM"));
        assert!(should_append_to_sent("smtp.fastmail.com"));
        assert!(should_append_to_sent("mail.example.org"));
    }

    #[test]
    fn attachment_detection_checks_bodystructure_and_content_type() {
        assert!(detect_has_attachments(
            Some("Multipart { common: ..., disposition: Some(ContentDisposition { ty: \"attachment\" }) }"),
            None
        ));
        assert!(detect_has_attachments(None, Some("multipart/mixed")));
        assert!(!detect_has_attachments(
            Some("Text { disposition: Some(ContentDisposition { ty: \"inline\" }) }"),
            Some("multipart/alternative")
        ));
    }

    #[test]
    fn parse_raw_message_extracts_headers_inline_cid_and_attachments() {
        let raw = concat!(
            "From: Alice <alice@example.com>\r\n",
            "Reply-To: replies@example.com\r\n",
            "To: Bob <bob@example.com>, carol@example.com\r\n",
            "Cc: Dave <dave@example.com>\r\n",
            "Subject: Project Update\r\n",
            "Date: Sat, 14 Nov 2026 14:48:00 +0000\r\n",
            "Message-ID: <msg-1@example.com>\r\n",
            "References: <root@example.com>\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"mix\"\r\n",
            "\r\n",
            "--mix\r\n",
            "Content-Type: multipart/related; boundary=\"rel\"\r\n",
            "\r\n",
            "--rel\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "\r\n",
            "<p>Hello <b>Bob</b> <img src=\"cid:logo1\"></p>\r\n",
            "--rel\r\n",
            "Content-Type: image/png\r\n",
            "Content-ID: <logo1>\r\n",
            "Content-Disposition: inline\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "AQID\r\n",
            "--rel--\r\n",
            "--mix\r\n",
            "Content-Type: application/pdf; name=\"report.pdf\"\r\n",
            "Content-Disposition: attachment; filename=\"report.pdf\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "JVBERi0xLjQ=\r\n",
            "--mix--\r\n",
        );

        let opened = parse_raw_message(77, raw.as_bytes(), false).unwrap();
        assert_eq!(opened.uid, 77);
        assert_eq!(opened.from, "Alice");
        assert_eq!(opened.from_address, "alice@example.com");
        assert_eq!(opened.reply_to.as_deref(), Some("replies@example.com"));
        assert_eq!(
            opened.to,
            vec!["Bob <bob@example.com>", "carol@example.com"]
        );
        assert_eq!(opened.cc, vec!["Dave <dave@example.com>"]);
        assert_eq!(opened.message_id.as_deref(), Some("msg-1@example.com"));
        assert_eq!(opened.references, vec!["root@example.com"]);
        assert!(opened.html.as_deref().unwrap().contains("<b>Bob</b>"));
        assert_eq!(opened.attachments.len(), 1);
        assert_eq!(opened.attachments[0].filename, "report.pdf");
        assert_eq!(opened.attachments[0].mime_type, "application/pdf");
        assert_eq!(opened.inline_parts.len(), 1);
        assert_eq!(opened.inline_parts[0].content_id, "logo1");
        assert_eq!(opened.inline_parts[0].data, vec![1, 2, 3]);

        let preview = extract_preview(&opened.text, opened.html.as_deref());
        assert_eq!(preview, "Hello Bob");
    }

    #[test]
    fn backend_serves_cached_messages_and_attachments_offline() {
        let dir = std::env::temp_dir().join(format!(
            "mac-mail-backend-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);

        let account = Account {
            email: "alice@example.com".to_owned(),
            imap_host: "127.0.0.1".to_owned(),
            imap_port: 1, // unreachable offline port
            smtp_host: "127.0.0.1".to_owned(),
            smtp_port: 1,
            ..Default::default()
        };
        let backend = MailBackend::with_cache_root(
            account,
            AccountCredential::Password("pw".to_owned()),
            &dir,
        );

        let raw = concat!(
            "From: Bob <bob@example.com>\r\n",
            "Subject: Offline Test\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n",
            "\r\n",
            "--b\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "Offline body text\r\n",
            "--b\r\n",
            "Content-Type: text/plain; name=\"notes.txt\"\r\n",
            "Content-Disposition: attachment; filename=\"notes.txt\"\r\n",
            "\r\n",
            "hello file\r\n",
            "--b--\r\n",
        );
        backend
            .cache()
            .save_raw_message("INBOX", 500, raw.as_bytes())
            .unwrap();

        let opened = backend.open_message("INBOX", 500).unwrap();
        assert!(opened.from_cache);
        assert!(opened.text.contains("Offline body text"));

        let saved_path = backend
            .save_attachment("INBOX", 500, 0, Some(&dir.join("dl")))
            .unwrap();
        assert_eq!(fs::read_to_string(&saved_path).unwrap(), "hello file");

        // Contacts were harvested offline from opening the cached message
        let contacts = backend.suggest_contacts("bob", 5);
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].email, "bob@example.com");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn preview_strips_style_script_and_collapses_whitespace() {
        let preview = extract_preview(
            "",
            Some("<style>.x{color:red}</style><script>alert(1)</script><p>Hello   <b>world</b>!\nSecond line</p>"),
        );
        assert_eq!(preview, "Hello world ! Second line");
    }

    #[test]
    fn quote_imap_mailbox_escapes_spaces_quotes_and_backslashes() {
        assert_eq!(
            quote_imap_mailbox("[Gmail]/Sent Mail"),
            "\"[Gmail]/Sent Mail\""
        );
        assert_eq!(
            quote_imap_mailbox("Folder \"A\"\\B"),
            "\"Folder \\\"A\\\"\\\\B\""
        );
    }
}
