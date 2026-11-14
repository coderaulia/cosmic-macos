// SPDX-License-Identifier: GPL-3.0-only

//! Dedicated multi-window compose module (Phase 2: Replying and writing).
//!
//! Provides separate compose windows for New Message, Reply, Reply All, Forward,
//! and Draft editing, with:
//! - From, To, Cc, toggleable Bcc, and Subject fields
//! - Address autocomplete populated from known/mailed contacts
//! - Removable attachment chips (filename + human-readable byte size), native
//!   file picker dialog, and drag-and-drop support
//! - Plain-text editor with simple formatting toolbar (`**bold**`, `*italic*`,
//!   links, bulleted/numbered lists) converted to clean HTML on send
//! - Collapsible quote of the original message below the new text
//! - RFC 5322 `multipart/alternative` (+ `multipart/mixed` for attachments)
//!   and `In-Reply-To` / `References` threading headers
//! - Hooks for saving drafts to Drafts and copying sent mail to Sent

pub mod contacts;
pub mod format;
pub mod mailto;
pub mod mime;
pub mod recipients;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use cosmic::{
    Apply, Element, Task, app,
    iced::{Alignment, Border, Color, Length, Size, widget::text_editor::Motion, window},
    theme,
    widget::{self, button, column, container, divider, header_bar, row, scrollable, text, text_editor},
};

#[allow(unused_imports)]
pub use contacts::AddressBook;
#[allow(unused_imports)]
pub use format::{QuotedOriginal, build_email_html, build_email_plain, plain_to_html_fragment};
#[allow(unused_imports)]
pub use mailto::{MailtoUri, extract_mailto_from_args, parse_mailto_uri};
#[allow(unused_imports)]
pub use mime::{
    AttachmentData, BuiltMessage, OutgoingMessage, build_mime_message, format_byte_size,
    guess_mime_type,
};
#[allow(unused_imports)]
pub use recipients::{
    EmailAddress, OriginalMessage, ReplyRecipients, ThreadingHeaders, format_address_list,
    parse_address_list, reply_all_recipients, reply_recipients, reply_subject, threading_headers,
};

/// Which kind of message a compose window is creating.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposeKind {
    New,
    Reply,
    ReplyAll,
    Forward,
    Draft,
}

/// Which recipient input currently has active autocomplete suggestions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipientField {
    To,
    Cc,
    Bcc,
}

/// Formatting actions triggered from the compose toolbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatAction {
    Bold,
    Italic,
    Link,
    BulletList,
    NumberedList,
}

/// An attachment displayed as a chip in the compose window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentItem {
    pub path: PathBuf,
    pub filename: String,
    pub size_bytes: u64,
}

impl AttachmentItem {
    pub fn from_path(path: PathBuf) -> Result<Self, String> {
        let meta = std::fs::metadata(&path)
            .map_err(|err| format!("Cannot access {}: {err}", path.display()))?;
        if !meta.is_file() {
            return Err(format!("Not a file: {}", path.display()));
        }
        let filename = path
            .file_name()
            .map_or_else(|| "attachment".to_owned(), |n| n.to_string_lossy().into_owned());
        Ok(Self {
            path,
            filename,
            size_bytes: meta.len(),
        })
    }

    pub fn chip_label(&self) -> String {
        format!("{} ({})", self.filename, format_byte_size(self.size_bytes))
    }
}

/// Messages handled by [`ComposeManager`].
#[derive(Clone, Debug)]
pub enum ComposeMessage {
    WindowOpened(window::Id),
    CloseRequested(window::Id),
    WindowClosed(window::Id),
    DragWindow(window::Id),
    MinimizeWindow(window::Id),
    MaximizeWindow(window::Id),
    FromChanged(window::Id, String),
    ToChanged(window::Id, String),
    CcChanged(window::Id, String),
    BccChanged(window::Id, String),
    ToggleBcc(window::Id),
    AcceptSuggestion(window::Id, RecipientField, EmailAddress),
    SubjectChanged(window::Id, String),
    BodyAction(window::Id, text_editor::Action),
    ApplyFormat(window::Id, FormatAction),
    ToggleQuote(window::Id),
    PickAttachments(window::Id),
    AttachmentsPicked(window::Id, Result<Vec<PathBuf>, String>),
    FilesHovered(window::Id),
    FilesHoveredLeft(window::Id),
    FilesDropped(window::Id, Vec<PathBuf>),
    RemoveAttachment(window::Id, usize),
    SaveDraft(window::Id),
    Send(window::Id),
}

/// Side-effect returned from [`ComposeManager::update`] for the host `Mail` app
/// (or backend hook) to execute.
#[allow(dead_code)]
#[derive(Debug)]
pub enum ComposeAction {
    None,
    Task(app::Task<ComposeMessage>),
    Send {
        window_id: window::Id,
        payload: OutgoingMessage,
        built: BuiltMessage,
    },
    SaveDraft {
        window_id: window::Id,
        payload: OutgoingMessage,
        built: BuiltMessage,
        existing_draft: Option<(String, u32)>,
    },
    Closed(window::Id),
}

/// State for a single open compose window.
#[allow(dead_code)]
pub struct ComposeWindowState {
    pub id: window::Id,
    pub kind: ComposeKind,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub show_bcc: bool,
    pub subject: String,
    pub body: text_editor::Content<cosmic::Renderer>,
    pub editor_id: widget::Id,
    pub quote: Option<QuotedOriginal>,
    pub quote_expanded: bool,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub attachments: Vec<AttachmentItem>,
    pub active_recipient_field: Option<RecipientField>,
    pub suggestions: Vec<EmailAddress>,
    pub drag_hovering: bool,
    pub sending: bool,
    pub saving_draft: bool,
    pub status: Option<String>,
    pub error: Option<String>,
    pub dirty: bool,
    pub last_edited_at: Option<Instant>,
    pub close_after_save: bool,
    pub existing_draft: Option<(String, u32)>,
}

impl ComposeWindowState {
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.last_edited_at = Some(Instant::now());
    }

    pub fn has_meaningful_content(&self) -> bool {
        !self.to.trim().is_empty()
            || !self.cc.trim().is_empty()
            || !self.bcc.trim().is_empty()
            || !self.subject.trim().is_empty()
            || !self.body.text().trim().is_empty()
            || !self.attachments.is_empty()
    }

    pub fn window_title(&self) -> String {
        let trimmed = self.subject.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
        match self.kind {
            ComposeKind::New => "New Message".to_owned(),
            ComposeKind::Reply => "Reply".to_owned(),
            ComposeKind::ReplyAll => "Reply All".to_owned(),
            ComposeKind::Forward => "Forward".to_owned(),
            ComposeKind::Draft => "Draft".to_owned(),
        }
    }

    fn build_outgoing_payload(&self, allow_empty_recipients: bool) -> Result<OutgoingMessage, String> {
        let to = parse_address_list(&self.to)?;
        let cc = parse_address_list(&self.cc)?;
        let bcc = parse_address_list(&self.bcc)?;
        let new_text = self.body.text();
        let plain_body = build_email_plain(&new_text, self.quote.as_ref());
        let html_body = build_email_html(&new_text, self.quote.as_ref());

        let mut attachments = Vec::with_capacity(self.attachments.len());
        for item in &self.attachments {
            attachments.push(AttachmentData::from_path(&item.path)?);
        }

        Ok(OutgoingMessage {
            from: self.from.clone(),
            to,
            cc,
            bcc,
            subject: self.subject.clone(),
            plain_body,
            html_body,
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
            attachments,
            allow_empty_recipients,
        })
    }
}

/// Payload tuple returned by [`ComposeManager::auto_save_candidates`].
pub type AutoSaveCandidate = (
    window::Id,
    OutgoingMessage,
    BuiltMessage,
    Option<(String, u32)>,
);

/// Manages all open compose windows and the shared autocomplete address book.
pub struct ComposeManager {
    windows: HashMap<window::Id, ComposeWindowState>,
    address_book: AddressBook,
}

impl Default for ComposeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ComposeManager {
    pub fn new() -> Self {
        Self {
            windows: HashMap::new(),
            address_book: AddressBook::load_default(),
        }
    }

    /// Returns `true` if `id` belongs to an open compose window.
    pub fn has_window(&self, id: window::Id) -> bool {
        self.windows.contains_key(&id)
    }

    /// Return the server draft UID if compose window `id` is backed by a saved draft.
    pub fn existing_draft_uid(&self, id: window::Id) -> Option<u32> {
        self.windows
            .get(&id)
            .and_then(|w| w.existing_draft.as_ref().map(|(_, uid)| *uid))
    }

    #[allow(dead_code)]
    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    /// Merge harvested contacts (e.g., from `@backend-imap` or loaded message headers)
    /// into the autocomplete address book.
    #[allow(dead_code)]
    pub fn merge_contacts(&mut self, addrs: impl IntoIterator<Item = EmailAddress>) {
        self.address_book.merge(addrs);
    }

    /// Convenience helper for callers with `(name, email)` tuples (e.g., `KnownContact`).
    #[allow(dead_code)]
    pub fn merge_contact_pairs(
        &mut self,
        pairs: impl IntoIterator<Item = (Option<String>, String)>,
    ) {
        self.address_book.merge(
            pairs
                .into_iter()
                .map(|(name, email)| EmailAddress::new(name, email)),
        );
    }

    /// Open a blank New Message compose window.
    pub fn open_new(&mut self, from_email: &str) -> (window::Id, app::Task<ComposeMessage>, String) {
        self.spawn_window(ComposeWindowInit {
            kind: ComposeKind::New,
            from: from_email.to_owned(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: String::new(),
            body_text: String::new(),
            quote: None,
            in_reply_to: None,
            references: None,
            existing_draft: None,
        })
    }

    /// Open a pre-filled Compose window from a parsed `mailto:` URI.
    pub fn open_mailto(
        &mut self,
        from_email: &str,
        mailto: &MailtoUri,
    ) -> (window::Id, app::Task<ComposeMessage>, String) {
        self.spawn_window(ComposeWindowInit {
            kind: ComposeKind::New,
            from: from_email.to_owned(),
            to: mailto.to.clone(),
            cc: mailto.cc.clone(),
            bcc: mailto.bcc.clone(),
            subject: mailto.subject.clone(),
            body_text: mailto.body.clone(),
            quote: None,
            in_reply_to: None,
            references: None,
            existing_draft: None,
        })
    }

    /// Whether any open compose window has unsaved edits eligible for auto-draft saving.
    pub fn has_dirty_windows(&self) -> bool {
        self.windows
            .values()
            .any(|w| w.dirty && !w.saving_draft && !w.sending && w.has_meaningful_content())
    }

    /// Candidate draft ready for background auto-save.
    pub fn auto_save_candidates(&mut self, min_idle: Duration) -> Vec<AutoSaveCandidate> {
        let mut out = Vec::new();
        for (&id, w) in &mut self.windows {
            if !w.dirty || w.saving_draft || w.sending || !w.has_meaningful_content() {
                continue;
            }
            if w.last_edited_at.is_some_and(|t| t.elapsed() < min_idle) {
                continue;
            }
            if let Ok(payload) = w.build_outgoing_payload(true)
                && let Ok(built) = build_mime_message(&payload)
            {
                w.saving_draft = true;
                w.error = None;
                w.status = Some("Auto-saving draft…".to_owned());
                out.push((id, payload, built, w.existing_draft.clone()));
            }
        }
        out
    }

    /// Open a Reply compose window above a collapsed quote of `original`.
    pub fn open_reply(
        &mut self,
        from_email: &str,
        original: &OriginalMessage,
    ) -> (window::Id, app::Task<ComposeMessage>, String) {
        let recips = reply_recipients(original, from_email);
        let threading = threading_headers(original);
        self.record_original_contacts(original);
        self.spawn_window(ComposeWindowInit {
            kind: ComposeKind::Reply,
            from: from_email.to_owned(),
            to: format_address_list(&recips.to),
            cc: String::new(),
            bcc: String::new(),
            subject: reply_subject(&original.subject, false),
            body_text: String::new(),
            quote: Some(QuotedOriginal {
                is_forward: false,
                from: original.primary_sender_display(),
                date: original.date.clone(),
                subject: original.subject.clone(),
                body: original.body_text.clone(),
            }),
            in_reply_to: threading.in_reply_to,
            references: threading.references,
            existing_draft: None,
        })
    }

    /// Open a Reply All compose window, stripping `from_email` and preserving Cc.
    pub fn open_reply_all(
        &mut self,
        from_email: &str,
        original: &OriginalMessage,
    ) -> (window::Id, app::Task<ComposeMessage>, String) {
        let recips = reply_all_recipients(original, from_email);
        let threading = threading_headers(original);
        self.record_original_contacts(original);
        self.spawn_window(ComposeWindowInit {
            kind: ComposeKind::ReplyAll,
            from: from_email.to_owned(),
            to: format_address_list(&recips.to),
            cc: format_address_list(&recips.cc),
            bcc: String::new(),
            subject: reply_subject(&original.subject, false),
            body_text: String::new(),
            quote: Some(QuotedOriginal {
                is_forward: false,
                from: original.primary_sender_display(),
                date: original.date.clone(),
                subject: original.subject.clone(),
                body: original.body_text.clone(),
            }),
            in_reply_to: threading.in_reply_to,
            references: threading.references,
            existing_draft: None,
        })
    }

    /// Open a Forward compose window with the forwarded message collapsible below.
    pub fn open_forward(
        &mut self,
        from_email: &str,
        original: &OriginalMessage,
    ) -> (window::Id, app::Task<ComposeMessage>, String) {
        self.record_original_contacts(original);
        self.spawn_window(ComposeWindowInit {
            kind: ComposeKind::Forward,
            from: from_email.to_owned(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: reply_subject(&original.subject, true),
            body_text: String::new(),
            quote: Some(QuotedOriginal {
                is_forward: true,
                from: original.primary_sender_display(),
                date: original.date.clone(),
                subject: original.subject.clone(),
                body: original.body_text.clone(),
            }),
            in_reply_to: None,
            references: None,
            existing_draft: None,
        })
    }

    /// Open an existing Draft message in a compose window.
    #[allow(dead_code)]
    pub fn open_draft(
        &mut self,
        from_email: &str,
        folder: String,
        uid: u32,
        draft: &OriginalMessage,
    ) -> (window::Id, app::Task<ComposeMessage>, String) {
        self.spawn_window(ComposeWindowInit {
            kind: ComposeKind::Draft,
            from: from_email.to_owned(),
            to: format_address_list(&draft.to),
            cc: format_address_list(&draft.cc),
            bcc: String::new(),
            subject: draft.subject.clone(),
            body_text: draft.body_text.clone(),
            quote: None,
            in_reply_to: draft.in_reply_to.clone(),
            references: (!draft.references.is_empty()).then(|| draft.references.join(" ")),
            existing_draft: Some((folder, uid)),
        })
    }

    fn record_original_contacts(&mut self, original: &OriginalMessage) {
        let all = original
            .from
            .iter()
            .chain(original.reply_to.iter())
            .chain(original.to.iter())
            .chain(original.cc.iter())
            .cloned();
        self.address_book.merge(all);
    }

    fn spawn_window(
        &mut self,
        init: ComposeWindowInit,
    ) -> (window::Id, app::Task<ComposeMessage>, String) {
        let (id, open_task) = window::open(window::Settings {
            size: Size::new(760.0, 640.0),
            min_size: Some(Size::new(520.0, 420.0)),
            exit_on_close_request: false,
            decorations: false,
            ..Default::default()
        });
        let show_bcc = !init.bcc.trim().is_empty();
        let state = ComposeWindowState {
            id,
            kind: init.kind,
            from: init.from,
            to: init.to,
            cc: init.cc,
            bcc: init.bcc,
            show_bcc,
            subject: init.subject,
            body: text_editor::Content::with_text(&init.body_text),
            editor_id: widget::Id::unique(),
            quote: init.quote,
            quote_expanded: false,
            in_reply_to: init.in_reply_to,
            references: init.references,
            attachments: Vec::new(),
            active_recipient_field: None,
            suggestions: Vec::new(),
            drag_hovering: false,
            sending: false,
            saving_draft: false,
            status: None,
            error: None,
            dirty: false,
            last_edited_at: None,
            close_after_save: false,
            existing_draft: init.existing_draft,
        };
        let title = state.window_title();
        self.windows.insert(id, state);
        let task = open_task.map(|id| cosmic::Action::App(ComposeMessage::WindowOpened(id)));
        (id, task, title)
    }

    /// Notify the compose window of the send result; closes the window on success.
    pub fn mark_sent(&mut self, id: window::Id, result: Result<(), String>) -> app::Task<ComposeMessage> {
        match result {
            Ok(()) => {
                self.windows.remove(&id);
                window::close(id)
            }
            Err(err) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.sending = false;
                    w.error = Some(err);
                }
                Task::none()
            }
        }
    }

    /// Notify the compose window of the draft-save result.
    pub fn mark_draft_saved(
        &mut self,
        id: window::Id,
        result: Result<Option<(String, u32)>, String>,
    ) -> app::Task<ComposeMessage> {
        let mut should_close = false;
        if let Some(w) = self.windows.get_mut(&id) {
            w.saving_draft = false;
            match result {
                Ok(new_draft_ref) => {
                    if let Some(draft_ref) = new_draft_ref {
                        w.existing_draft = Some(draft_ref);
                    }
                    w.dirty = false;
                    w.error = None;
                    w.status = Some("Saved to Drafts".to_owned());
                    if w.close_after_save {
                        should_close = true;
                    }
                }
                Err(err) => {
                    w.close_after_save = false;
                    w.error = Some(err);
                }
            }
        }
        if should_close {
            self.windows.remove(&id);
            window::close(id)
        } else {
            Task::none()
        }
    }

    /// Handle a [`ComposeMessage`] and return any host [`ComposeAction`] needed.
    pub fn update(&mut self, msg: ComposeMessage) -> ComposeAction {
        match msg {
            ComposeMessage::WindowOpened(_id) => ComposeAction::None,
            ComposeMessage::CloseRequested(id) => {
                if let Some(w) = self.windows.get_mut(&id)
                    && w.dirty
                    && !w.saving_draft
                    && !w.sending
                    && w.error.is_none()
                    && w.has_meaningful_content()
                    && let Ok(payload) = w.build_outgoing_payload(true)
                    && let Ok(built) = build_mime_message(&payload)
                {
                    w.saving_draft = true;
                    w.close_after_save = true;
                    w.status = Some("Saving draft…".to_owned());
                    let existing_draft = w.existing_draft.clone();
                    return ComposeAction::SaveDraft {
                        window_id: id,
                        payload,
                        built,
                        existing_draft,
                    };
                }
                ComposeAction::Task(window::close(id))
            }
            ComposeMessage::WindowClosed(id) => {
                self.windows.remove(&id);
                ComposeAction::Closed(id)
            }
            ComposeMessage::DragWindow(id) => ComposeAction::Task(window::drag(id)),
            ComposeMessage::MinimizeWindow(id) => ComposeAction::Task(window::minimize(id, true)),
            ComposeMessage::MaximizeWindow(id) => ComposeAction::Task(window::toggle_maximize(id)),
            ComposeMessage::FromChanged(id, val) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.from = val;
                    w.mark_dirty();
                }
                ComposeAction::None
            }
            ComposeMessage::ToChanged(id, val) => {
                let suggestions = self.address_book.suggest(&val, 5);
                if let Some(w) = self.windows.get_mut(&id) {
                    w.to = val;
                    w.active_recipient_field = (!suggestions.is_empty()).then_some(RecipientField::To);
                    w.suggestions = suggestions;
                    w.mark_dirty();
                    w.error = None;
                }
                ComposeAction::None
            }
            ComposeMessage::CcChanged(id, val) => {
                let suggestions = self.address_book.suggest(&val, 5);
                if let Some(w) = self.windows.get_mut(&id) {
                    w.cc = val;
                    w.active_recipient_field = (!suggestions.is_empty()).then_some(RecipientField::Cc);
                    w.suggestions = suggestions;
                    w.mark_dirty();
                    w.error = None;
                }
                ComposeAction::None
            }
            ComposeMessage::BccChanged(id, val) => {
                let suggestions = self.address_book.suggest(&val, 5);
                if let Some(w) = self.windows.get_mut(&id) {
                    w.bcc = val;
                    w.active_recipient_field = (!suggestions.is_empty()).then_some(RecipientField::Bcc);
                    w.suggestions = suggestions;
                    w.mark_dirty();
                    w.error = None;
                }
                ComposeAction::None
            }
            ComposeMessage::ToggleBcc(id) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.show_bcc = !w.show_bcc;
                    if !w.show_bcc && w.active_recipient_field == Some(RecipientField::Bcc) {
                        w.active_recipient_field = None;
                        w.suggestions.clear();
                    }
                }
                ComposeAction::None
            }
            ComposeMessage::AcceptSuggestion(id, field, addr) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    match field {
                        RecipientField::To => {
                            w.to = AddressBook::apply_suggestion(&w.to, &addr);
                        }
                        RecipientField::Cc => {
                            w.cc = AddressBook::apply_suggestion(&w.cc, &addr);
                        }
                        RecipientField::Bcc => {
                            w.bcc = AddressBook::apply_suggestion(&w.bcc, &addr);
                        }
                    }
                    w.active_recipient_field = None;
                    w.suggestions.clear();
                    w.mark_dirty();
                }
                ComposeAction::None
            }
            ComposeMessage::SubjectChanged(id, val) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.subject = val;
                    w.mark_dirty();
                }
                ComposeAction::None
            }
            ComposeMessage::BodyAction(id, action) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    if action.is_edit() {
                        w.mark_dirty();
                        w.status = None;
                    }
                    w.body.perform(action);
                }
                ComposeAction::None
            }
            ComposeMessage::ApplyFormat(id, fmt) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    apply_format_to_editor(&mut w.body, fmt);
                    w.mark_dirty();
                }
                ComposeAction::None
            }
            ComposeMessage::ToggleQuote(id) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.quote_expanded = !w.quote_expanded;
                }
                ComposeAction::None
            }
            ComposeMessage::PickAttachments(id) => {
                let task = cosmic::task::future(async move {
                    let dialog = cosmic::dialog::file_chooser::open::Dialog::new()
                        .title("Attach Files");
                    let res = match dialog.open_files().await {
                        Ok(response) => {
                            let paths = response
                                .urls()
                                .iter()
                                .filter_map(|url| url.to_file_path().ok())
                                .collect::<Vec<_>>();
                            Ok(paths)
                        }
                        Err(cosmic::dialog::file_chooser::Error::Cancelled) => Ok(Vec::new()),
                        Err(err) => Err(format!("Could not open file picker: {err}")),
                    };
                    ComposeMessage::AttachmentsPicked(id, res)
                });
                ComposeAction::Task(task)
            }
            ComposeMessage::AttachmentsPicked(id, result) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    match result {
                        Ok(paths) => attach_paths(w, paths),
                        Err(err) => w.error = Some(err),
                    }
                }
                ComposeAction::None
            }
            ComposeMessage::FilesHovered(id) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.drag_hovering = true;
                }
                ComposeAction::None
            }
            ComposeMessage::FilesHoveredLeft(id) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.drag_hovering = false;
                }
                ComposeAction::None
            }
            ComposeMessage::FilesDropped(id, paths) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.drag_hovering = false;
                    attach_paths(w, paths);
                }
                ComposeAction::None
            }
            ComposeMessage::RemoveAttachment(id, index) => {
                if let Some(w) = self.windows.get_mut(&id)
                    && index < w.attachments.len()
                {
                    w.attachments.remove(index);
                    w.mark_dirty();
                }
                ComposeAction::None
            }
            ComposeMessage::SaveDraft(id) => {
                let Some(w) = self.windows.get_mut(&id) else {
                    return ComposeAction::None;
                };
                if w.saving_draft || w.sending {
                    return ComposeAction::None;
                }
                match w
                    .build_outgoing_payload(true)
                    .and_then(|payload| build_mime_message(&payload).map(|built| (payload, built)))
                {
                    Ok((payload, built)) => {
                        w.saving_draft = true;
                        w.error = None;
                        w.status = Some("Saving draft…".to_owned());
                        let existing_draft = w.existing_draft.clone();
                        ComposeAction::SaveDraft {
                            window_id: id,
                            payload,
                            built,
                            existing_draft,
                        }
                    }
                    Err(err) => {
                        w.error = Some(err);
                        ComposeAction::None
                    }
                }
            }
            ComposeMessage::Send(id) => {
                let Some(w) = self.windows.get_mut(&id) else {
                    return ComposeAction::None;
                };
                if w.sending {
                    return ComposeAction::None;
                }
                match w
                    .build_outgoing_payload(false)
                    .and_then(|payload| build_mime_message(&payload).map(|built| (payload, built)))
                {
                    Ok((payload, built)) => {
                        let mailed: Vec<EmailAddress> = payload
                            .to
                            .iter()
                            .chain(payload.cc.iter())
                            .chain(payload.bcc.iter())
                            .cloned()
                            .collect();
                        self.address_book.merge(mailed);
                        if let Some(w) = self.windows.get_mut(&id) {
                            w.sending = true;
                            w.error = None;
                            w.status = Some("Sending…".to_owned());
                        }
                        ComposeAction::Send {
                            window_id: id,
                            payload,
                            built,
                        }
                    }
                    Err(err) => {
                        w.error = Some(err);
                        ComposeAction::None
                    }
                }
            }
        }
    }

    /// Render the compose window for `id`.
    pub fn view_window(&self, id: window::Id, focused: bool) -> Element<'_, ComposeMessage> {
        let Some(w) = self.windows.get(&id) else {
            return text::body("Compose window closed")
                .apply(container)
                .center(Length::Fill)
                .into();
        };

        let title = w.window_title();
        let top_bar = header_bar()
            .title(title)
            .focused(focused)
            .on_drag(ComposeMessage::DragWindow(id))
            .on_minimize(ComposeMessage::MinimizeWindow(id))
            .on_maximize(ComposeMessage::MaximizeWindow(id))
            .on_close(ComposeMessage::CloseRequested(id));

        let send_label = if w.sending { "Sending…" } else { "Send" };
        let draft_label = if w.saving_draft { "Saving…" } else { "Save Draft" };

        let action_bar = row![
            button::suggested(send_label)
                .on_press_maybe((!w.sending).then_some(ComposeMessage::Send(id))),
            button::standard(draft_label)
                .on_press_maybe((!w.saving_draft && !w.sending).then_some(ComposeMessage::SaveDraft(id))),
            button::standard("Attach…")
                .leading_icon(widget::icon::from_name("mail-attachment-symbolic"))
                .on_press(ComposeMessage::PickAttachments(id)),
            widget::space::horizontal().width(Length::Fill),
            format_button("B", FormatAction::Bold, id),
            format_button("I", FormatAction::Italic, id),
            format_button("Link", FormatAction::Link, id),
            format_button("• List", FormatAction::BulletList, id),
            format_button("1. List", FormatAction::NumberedList, id),
        ]
        .spacing(6)
        .align_y(Alignment::Center);

        let from_row = row![
            field_label("From"),
            widget::text_input("your@address", &w.from)
                .on_input(move |v| ComposeMessage::FromChanged(id, v))
                .width(Length::Fill),
            button::text(if w.show_bcc { "Hide Bcc" } else { "Bcc" })
                .on_press(ComposeMessage::ToggleBcc(id)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let mut headers = column![from_row].spacing(6);

        headers = headers.push(recipient_row(
            "To",
            "recipient@example.com, …",
            &w.to,
            move |v| ComposeMessage::ToChanged(id, v),
        ));
        if w.active_recipient_field == Some(RecipientField::To) && !w.suggestions.is_empty() {
            headers = headers.push(suggestion_bar(id, RecipientField::To, &w.suggestions));
        }

        headers = headers.push(recipient_row(
            "Cc",
            "cc@example.com, …",
            &w.cc,
            move |v| ComposeMessage::CcChanged(id, v),
        ));
        if w.active_recipient_field == Some(RecipientField::Cc) && !w.suggestions.is_empty() {
            headers = headers.push(suggestion_bar(id, RecipientField::Cc, &w.suggestions));
        }

        if w.show_bcc {
            headers = headers.push(recipient_row(
                "Bcc",
                "bcc@example.com, …",
                &w.bcc,
                move |v| ComposeMessage::BccChanged(id, v),
            ));
            if w.active_recipient_field == Some(RecipientField::Bcc) && !w.suggestions.is_empty() {
                headers = headers.push(suggestion_bar(id, RecipientField::Bcc, &w.suggestions));
            }
        }

        headers = headers.push(
            row![
                field_label("Subject"),
                widget::text_input("Subject", &w.subject)
                    .on_input(move |v| ComposeMessage::SubjectChanged(id, v))
                    .width(Length::Fill),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );

        let mut body_col = column![
            action_bar,
            divider::horizontal::default(),
            headers,
            divider::horizontal::default(),
        ]
        .spacing(10);

        if w.drag_hovering || !w.attachments.is_empty() {
            body_col = body_col.push(attachments_area(id, w));
        }

        let editor = text_editor::text_editor(&w.body)
            .id(w.editor_id.clone())
            .placeholder("Write your message… (**bold**, *italic*, [links](https://…), lists)")
            .on_action(move |action| ComposeMessage::BodyAction(id, action))
            .padding(10)
            .height(Length::Fill);

        body_col = body_col.push(editor);

        if let Some(quote) = w.quote.as_ref() {
            body_col = body_col.push(collapsible_quote_view(id, quote, w.quote_expanded));
        }

        if let Some(err) = w.error.as_ref() {
            body_col = body_col.push(
                container(text::caption(err.as_str()))
                    .padding([6, 10])
                    .class(theme::Container::Custom(Box::new(|_t| {
                        cosmic::iced::widget::container::Style {
                            background: Some(Color::from_rgba(0.85, 0.2, 0.2, 0.14).into()),
                            border: Border {
                                color: Color::from_rgba(0.85, 0.2, 0.2, 0.45),
                                width: 1.0,
                                radius: 6.0.into(),
                            },
                            ..Default::default()
                        }
                    }))),
            );
        } else if let Some(status) = w.status.as_ref() {
            body_col = body_col.push(text::caption(status.as_str()));
        }

        let content = container(body_col)
            .padding(16)
            .width(Length::Fill)
            .height(Length::Fill);

        container(column![top_bar, content])
            .class(theme::Container::WindowBackground)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

struct ComposeWindowInit {
    kind: ComposeKind,
    from: String,
    to: String,
    cc: String,
    bcc: String,
    subject: String,
    body_text: String,
    quote: Option<QuotedOriginal>,
    in_reply_to: Option<String>,
    references: Option<String>,
    existing_draft: Option<(String, u32)>,
}

fn field_label<'a>(label: &'a str) -> Element<'a, ComposeMessage> {
    text::caption(label).width(Length::Fixed(58.0)).into()
}

fn format_button<'a>(
    label: &'a str,
    fmt: FormatAction,
    id: window::Id,
) -> Element<'a, ComposeMessage> {
    button::standard(label)
        .on_press(ComposeMessage::ApplyFormat(id, fmt))
        .into()
}

fn recipient_row<'a>(
    label: &'a str,
    placeholder: &'a str,
    value: &'a str,
    on_input: impl Fn(String) -> ComposeMessage + 'a,
) -> Element<'a, ComposeMessage> {
    row![
        field_label(label),
        widget::text_input(placeholder, value)
            .on_input(on_input)
            .width(Length::Fill),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn suggestion_bar<'a>(
    id: window::Id,
    field: RecipientField,
    suggestions: &'a [EmailAddress],
) -> Element<'a, ComposeMessage> {
    let mut chips = row![].spacing(6);
    for addr in suggestions {
        let label = addr.format_display();
        chips = chips.push(
            button::standard(label)
                .on_press(ComposeMessage::AcceptSuggestion(id, field, addr.clone())),
        );
    }
    row![
        widget::space::horizontal().width(Length::Fixed(66.0)),
        chips.wrap(),
    ]
    .into()
}

fn attachments_area<'a>(
    id: window::Id,
    w: &'a ComposeWindowState,
) -> Element<'a, ComposeMessage> {
    let mut chips = row![].spacing(6).align_y(Alignment::Center);
    for (idx, att) in w.attachments.iter().enumerate() {
        let chip = row![
            widget::icon::from_name("mail-attachment-symbolic").size(14).icon(),
            text::caption(att.chip_label()),
            button::text("✕")
                .padding([0, 4])
                .on_press(ComposeMessage::RemoveAttachment(id, idx)),
        ]
        .spacing(4)
        .align_y(Alignment::Center);

        chips = chips.push(
            container(chip)
                .padding([4, 8])
                .class(theme::Container::Card),
        );
    }

    let mut box_col = column![].spacing(6);
    if !w.attachments.is_empty() {
        box_col = box_col.push(chips.wrap());
    }
    if w.drag_hovering {
        box_col = box_col.push(
            container(text::caption("Drop files here to attach"))
                .padding([8, 12])
                .width(Length::Fill)
                .class(theme::Container::Custom(Box::new(|t| {
                    let accent: Color = t.cosmic().accent_color().into();
                    cosmic::iced::widget::container::Style {
                        background: Some(
                            Color::from_rgba(accent.r, accent.g, accent.b, 0.12).into(),
                        ),
                        border: Border {
                            color: accent,
                            width: 1.5,
                            radius: 6.0.into(),
                        },
                        ..Default::default()
                    }
                }))),
        );
    }
    box_col.into()
}

fn collapsible_quote_view<'a>(
    id: window::Id,
    quote: &'a QuotedOriginal,
    expanded: bool,
) -> Element<'a, ComposeMessage> {
    let toggle_label = if expanded {
        format!("▼ Hide quoted message ({})", quote.attribution_line())
    } else {
        format!("▶ Show quoted message ({})", quote.attribution_line())
    };

    let mut col = column![
        button::text(toggle_label).on_press(ComposeMessage::ToggleQuote(id)),
    ]
    .spacing(6);

    if expanded {
        let quote_text = if quote.is_forward {
            format!(
                "From: {}\nDate: {}\nSubject: {}\n\n{}",
                quote.from, quote.date, quote.subject, quote.body
            )
        } else {
            quote.body.clone()
        };
        let quote_box = container(
            scrollable(text::caption(quote_text).width(Length::Fill)).height(Length::Fixed(150.0)),
        )
        .padding([8, 12])
        .width(Length::Fill)
        .class(theme::Container::Secondary);
        col = col.push(quote_box);
    }

    col.into()
}

fn attach_paths(w: &mut ComposeWindowState, paths: Vec<PathBuf>) {
    let mut errors = Vec::new();
    for path in paths {
        if w.attachments.iter().any(|existing| existing.path == path) {
            continue;
        }
        match AttachmentItem::from_path(path) {
            Ok(item) => {
                w.attachments.push(item);
                w.mark_dirty();
            }
            Err(err) => errors.push(err),
        }
    }
    if errors.is_empty() {
        w.error = None;
    } else {
        w.error = Some(errors.join("; "));
    }
}

fn apply_format_to_editor(
    content: &mut text_editor::Content<cosmic::Renderer>,
    fmt: FormatAction,
) {
    let selection = content.selection();
    let snippet = match fmt {
        FormatAction::Bold => match selection {
            Some(sel) if !sel.is_empty() => format!("**{sel}**"),
            _ => "**bold**".to_owned(),
        },
        FormatAction::Italic => match selection {
            Some(sel) if !sel.is_empty() => format!("*{sel}*"),
            _ => "*italic*".to_owned(),
        },
        FormatAction::Link => match selection {
            Some(sel) if !sel.is_empty() => {
                if sel.starts_with("http://") || sel.starts_with("https://") {
                    format!("[link]({sel})")
                } else {
                    format!("[{sel}](https://)")
                }
            }
            _ => "[link text](https://)".to_owned(),
        },
        FormatAction::BulletList => {
            if let Some(sel) = selection.filter(|s| !s.is_empty()) {
                sel.lines()
                    .map(|l| format!("- {l}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                content.perform(text_editor::Action::Move(Motion::Home));
                "- ".to_owned()
            }
        }
        FormatAction::NumberedList => {
            if let Some(sel) = selection.filter(|s| !s.is_empty()) {
                sel.lines()
                    .enumerate()
                    .map(|(idx, l)| format!("{}. {l}", idx + 1))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                content.perform(text_editor::Action::Move(Motion::Home));
                "1. ".to_owned()
            }
        }
    };

    content.perform(text_editor::Action::Edit(text_editor::Edit::Paste(
        Arc::new(snippet),
    )));
}

/// Find the Drafts folder name from a list of server mailbox names.
#[allow(dead_code)]
pub fn find_drafts_folder(folders: &[String]) -> Option<String> {
    folders
        .iter()
        .find(|name| name.to_ascii_lowercase().contains("draft"))
        .cloned()
}

/// Find the Sent folder name from a list of server mailbox names.
#[allow(dead_code)]
pub fn find_sent_folder(folders: &[String]) -> Option<String> {
    folders
        .iter()
        .find(|name| {
            let lower = name.to_ascii_lowercase();
            lower.contains("sent")
        })
        .cloned()
}

/// Determine whether an IMAP server automatically copies SMTP-submitted mail
/// into Sent (so the client should skip appending a duplicate copy).
#[allow(dead_code)]
pub fn server_auto_saves_sent(imap_host: &str) -> bool {
    let lower = imap_host.trim().to_ascii_lowercase();
    lower.contains("gmail.com")
        || lower.contains("googlemail.com")
        || lower.contains("office365.com")
        || lower.contains("outlook.com")
}

/// Convert a `mail_parser::Message` into an [`OriginalMessage`] suitable for
/// Reply, Reply All, Forward, or Draft editing.
pub fn original_from_parsed(
    uid: u32,
    message: &mail_parser::Message<'_>,
    fallback_subject: &str,
    fallback_date: &str,
) -> OriginalMessage {
    let extract_addrs = |addr_opt: Option<mail_parser::AddressList<'_>>| -> Vec<EmailAddress> {
        let Some(addr) = addr_opt else {
            return Vec::new();
        };
        addr.mailboxes()
            .filter_map(|m| {
                let email = m.address()?.trim();
                if email.is_empty() {
                    return None;
                }
                let name = m.name().map(str::to_owned);
                Some(EmailAddress::new(name, email))
            })
            .collect()
    };

    let message_id = message
        .message_id()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("<{}>", s.trim_start_matches('<').trim_end_matches('>')));

    let in_reply_to = message
        .in_reply_to()
        .and_then(|tl| tl.first())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("<{}>", s.trim_start_matches('<').trim_end_matches('>')));

    let references = message
        .references()
        .map(|list| {
            list.iter()
                .map(|s| format!("<{}>", s.trim().trim_start_matches('<').trim_end_matches('>')))
                .collect()
        })
        .unwrap_or_default();

    let body_text = message
        .body_text(0)
        .map(|t| t.into_owned())
        .unwrap_or_default();

    let date = message
        .date()
        .map(|d| {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                d.year, d.month, d.day, d.hour, d.minute
            )
        })
        .unwrap_or_else(|| fallback_date.to_owned());

    OriginalMessage {
        uid,
        message_id,
        in_reply_to,
        references,
        from: extract_addrs(message.from()),
        reply_to: extract_addrs(message.reply_to()),
        to: extract_addrs(message.to()),
        cc: extract_addrs(message.cc()),
        subject: message
            .subject()
            .unwrap_or(fallback_subject)
            .to_owned(),
        date,
        body_text,
    }
}
