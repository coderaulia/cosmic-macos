// SPDX-License-Identifier: GPL-3.0-only

//! Three-pane mail client laid out like macOS Mail: mailboxes on the left,
//! the message list in the middle, the selected message on the right.
//!
//! Supports multiple IMAP/SMTP accounts (`~/.config/mac-mail/accounts.json`,
//! migrating legacy `account.conf`), All Inboxes unified view, conversation
//! threading, OAuth2 PKCE browser sign-in with `XOAUTH2`, `mailto:` CLI URIs,
//! trusted-sender remote image whitelisting, and auto-draft saving.

mod account;
#[allow(dead_code)]
mod backend;
#[allow(dead_code)]
mod cache;
mod compose;
#[allow(dead_code)]
mod imap_pool;
#[allow(dead_code)]
mod oauth;
#[allow(dead_code)]
mod threading;
mod ui_helpers;

use std::{collections::HashMap, time::Duration};

use account::{Account, AccountCredential, TrustedSenders};
use backend::{AttachmentInfo, DEFAULT_PAGE_SIZE, FolderPage, MailBackend, OpenedMessage};
use cache::{FolderInfo, FolderKind, MessageSummary};
use compose::{
    ComposeAction, ComposeManager, ComposeMessage, EmailAddress, MailtoUri, OriginalMessage,
};
use cosmic::{
    ApplicationExt, Apply, Element, Task, app,
    iced::{
        Alignment, Background, Border, Color, Event, Length, Subscription, event, keyboard, time,
        window,
    },
    theme,
    widget::{self, button, column, container, divider, row, scrollable, text},
};
use imap_pool::{IdleWatcher, NewMailNotification};
use mac_mail::{
    html_mail::{self, DecodedImage, HtmlMailEvent, MailBodyState, MailContent},
    sanitizer,
};
use oauth::{OAuthProvider, OAuthSignInResult};
use threading::{MailItem, MailThread, MessageKey};

const APP_ID: &str = "dev.asw.MacMail";
const SIDEBAR_WIDTH: f32 = 236.0;
const LIST_WIDTH: f32 = 348.0;
const AUTO_DRAFT_INTERVAL_SECS: u64 = 15;
const AUTO_DRAFT_MIN_IDLE_SECS: u64 = 3;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    let settings = app::Settings::default().size(cosmic::iced::Size::new(1240.0, 780.0));
    app::run::<Mail>(settings, ())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SidebarView {
    AllInboxes,
    AccountFolder {
        account_email: String,
        folder: String,
    },
}

struct AccountRuntime {
    account: Account,
    credential: AccountCredential,
    backend: MailBackend,
    folders: Vec<FolderInfo>,
    inbox_summaries: Vec<MessageSummary>,
}

impl AccountRuntime {
    fn inbox_folder_name(&self) -> String {
        self.folders
            .iter()
            .find(|f| f.kind == FolderKind::Inbox || f.raw_name.eq_ignore_ascii_case("INBOX"))
            .map_or_else(|| "INBOX".to_owned(), |f| f.raw_name.clone())
    }

    fn inbox_unread(&self) -> u32 {
        self.folders
            .iter()
            .find(|f| f.kind == FolderKind::Inbox || f.raw_name.eq_ignore_ascii_case("INBOX"))
            .map_or(0, |f| f.unread)
    }

    fn default_folder(&self) -> String {
        self.folders
            .iter()
            .find(|f| f.kind == FolderKind::Inbox || f.raw_name.eq_ignore_ascii_case("INBOX"))
            .or(self.folders.first())
            .map_or_else(|| "INBOX".to_owned(), |f| f.raw_name.clone())
    }
}

/// Sign-in form fields.
#[derive(Default)]
struct Setup {
    email: String,
    password: String,
    imap_host: String,
    smtp_host: String,
    oauth_client_id: String,
}

#[derive(Debug, Clone)]
enum Msg {
    FormEmail(String),
    FormPassword(String),
    FormImap(String),
    FormSmtp(String),
    FormOAuthClientId(String),
    SignIn,
    StartOAuth(OAuthProvider),
    OAuthCompleted(Result<OAuthSignInResult, String>),
    AccountSignedIn {
        account: Account,
        credential: AccountCredential,
        result: Result<Vec<FolderInfo>, String>,
    },
    OpenAddAccount,
    CancelAddAccount,
    SignOutAccount(String),
    SelectAllInboxes,
    SwitchAccount(String),
    Folders(String, Result<Vec<FolderInfo>, String>),
    SelectFolder {
        account_email: String,
        folder: String,
    },
    Headers {
        account_email: String,
        folder: String,
        result: Result<FolderPage, String>,
    },
    OlderHeaders {
        account_email: String,
        folder: String,
        result: Result<FolderPage, String>,
    },
    LoadOlder,
    ToggleThreading,
    ToggleThreadPanel,
    SelectMessage(MessageKey),
    SelectNext,
    SelectPrev,
    Body(MessageKey, Box<Result<OpenedMessage, String>>),
    SearchText(String),
    RunSearch,
    SearchResults(SidebarView, Result<Vec<MailItem>, String>),
    ClearSearch,
    Delete,
    Archive,
    MarkUnread,
    MarkedUnread(MessageKey, Result<(), String>),
    ToggleRecipients,
    Moved(MessageKey, Result<(), String>),
    SaveAttachment(usize),
    Saved(Result<String, String>),
    Refresh,
    NewMail(NewMailNotification),
    Compose,
    Reply,
    ReplyAll,
    Forward,
    EditDraft,
    AutoSaveDrafts,
    ComposeEvent(ComposeMessage),
    ComposeSent(window::Id, Result<(), String>),
    ComposeDraftSaved(window::Id, Result<Option<(String, u32)>, String>),
    HtmlMail(HtmlMailEvent),
    RemoteImagesLoaded(MessageKey, HashMap<String, Result<DecodedImage, String>>),
    TaskPanicked(String),
}

struct Mail {
    core: app::Core,
    accounts: Vec<AccountRuntime>,
    active_account: Option<String>,
    view: SidebarView,
    adding_account: bool,
    setup: Setup,
    threaded_mode: bool,
    thread_expanded: bool,
    flat_items: Vec<MailItem>,
    threads: Vec<MailThread>,
    has_more: bool,
    loading_older: bool,
    selected: Option<MessageKey>,
    opened: Option<OpenedMessage>,
    body_content: Option<MailContent>,
    body_state: MailBodyState,
    attachments: Vec<AttachmentInfo>,
    show_recipients: bool,
    search_text: String,
    searching: bool,
    status: Option<String>,
    busy: bool,
    compose: ComposeManager,
    trusted_senders: TrustedSenders,
    pending_mailto: Option<MailtoUri>,
}

/// Run blocking network work on a worker thread and turn its result into a message.
fn run<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) -> Msg + Send + 'static,
) -> app::Task<Msg> {
    cosmic::task::future(async move {
        match tokio::task::spawn_blocking(work).await {
            Ok(value) => done(value),
            Err(err) => done_error(err.to_string()),
        }
    })
}

/// A worker that panicked is reported like any other failure.
fn done_error(message: String) -> Msg {
    Msg::TaskPanicked(message)
}

/// Empty space that takes up the remaining width.
fn space_fill() -> widget::Space {
    widget::space::horizontal().width(Length::Fill)
}

fn folder_icon_for(info: &FolderInfo) -> &'static str {
    match info.kind {
        FolderKind::Inbox => "mail-inbox-symbolic",
        FolderKind::Drafts => "document-edit-symbolic",
        FolderKind::Sent => "mail-send-symbolic",
        FolderKind::Archive => "mail-archive-symbolic",
        FolderKind::Junk => "mail-mark-junk-symbolic",
        FolderKind::Trash => "user-trash-symbolic",
        FolderKind::Other => ui_helpers::folder_icon(&info.raw_name),
    }
}

/// Subtle tinted selection style that preserves readable foreground text
/// instead of turning all text blue.
fn list_row_button_class(selected: bool) -> button::ButtonClass {
    button::ButtonClass::Custom {
        active: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let fg: Color = cosmic.background(theme.transparent).on.into();
            let mut style = button::Style::new();
            style.border_radius = cosmic.corner_radii.radius_s.into();
            style.text_color = Some(fg);
            style.icon_color = Some(fg);
            if selected {
                let mut tint = Color::from(cosmic.accent.base);
                tint.a = 0.14;
                style.background = Some(Background::Color(tint));
            }
            style
        }),
        disabled: Box::new(|theme| {
            let cosmic = theme.cosmic();
            let mut fg: Color = cosmic.background(theme.transparent).on.into();
            fg.a *= 0.5;
            let mut style = button::Style::new();
            style.text_color = Some(fg);
            style.icon_color = Some(fg);
            style
        }),
        hovered: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let fg: Color = cosmic.background(theme.transparent).on.into();
            let mut style = button::Style::new();
            style.border_radius = cosmic.corner_radii.radius_s.into();
            style.text_color = Some(fg);
            style.icon_color = Some(fg);
            let bg = if selected {
                let mut tint = Color::from(cosmic.accent.base);
                tint.a = 0.20;
                tint
            } else {
                let mut tint = fg;
                tint.a = 0.07;
                tint
            };
            style.background = Some(Background::Color(bg));
            style
        }),
        pressed: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let fg: Color = cosmic.background(theme.transparent).on.into();
            let mut tint = Color::from(cosmic.accent.base);
            tint.a = 0.24;
            let mut style = button::Style::new();
            style.border_radius = cosmic.corner_radii.radius_s.into();
            style.background = Some(Background::Color(tint));
            style.text_color = Some(fg);
            style.icon_color = Some(fg);
            style
        }),
    }
}

fn muted_text_class() -> theme::Text {
    theme::Text::Custom(|theme| {
        let mut color: Color = theme.cosmic().background(theme.transparent).on.into();
        color.a = 0.65;
        cosmic::iced::widget::text::Style {
            color: Some(color),
            ..Default::default()
        }
    })
}

fn count_badge<'a>(count_text: String, highlighted: bool) -> Element<'a, Msg> {
    text::caption(count_text)
        .apply(container)
        .padding([1, 7])
        .class(theme::Container::custom(move |theme| {
            let cosmic = theme.cosmic();
            let mut bg: Color = cosmic.background(theme.transparent).on.into();
            bg.a = if highlighted { 0.18 } else { 0.10 };
            widget::container::Style {
                background: Some(Background::Color(bg)),
                border: Border {
                    radius: 10.0.into(),
                    ..Default::default()
                },
                text_color: Some(cosmic.background(theme.transparent).on.into()),
                ..Default::default()
            }
        }))
        .into()
}

fn extract_email_addr(formatted: &str) -> String {
    if let (Some(start), Some(end)) = (formatted.rfind('<'), formatted.rfind('>'))
        && start < end
    {
        return formatted[start + 1..end].trim().to_owned();
    }
    formatted.trim().to_owned()
}

fn idle_subscription_worker(
    params: &(Account, AccountCredential),
) -> cosmic::iced::futures::stream::BoxStream<'static, Msg> {
    let account = params.0.clone();
    let credential = params.1.clone();
    Box::pin(cosmic::iced::stream::channel::<Msg>(
        16,
        move |tx: cosmic::iced::futures::channel::mpsc::Sender<Msg>| async move {
            let _watcher = IdleWatcher::start(account, credential, move |notif| {
                let mut sender = tx.clone();
                let _ = sender.try_send(Msg::NewMail(notif));
            });
            std::future::pending::<()>().await;
        },
    ))
}

impl Mail {
    fn account_runtime(&self, email: &str) -> Option<&AccountRuntime> {
        self.accounts
            .iter()
            .find(|a| a.account.email.eq_ignore_ascii_case(email))
    }

    fn account_runtime_mut(&mut self, email: &str) -> Option<&mut AccountRuntime> {
        self.accounts
            .iter_mut()
            .find(|a| a.account.email.eq_ignore_ascii_case(email))
    }

    fn active_runtime(&self) -> Option<&AccountRuntime> {
        self.active_account
            .as_deref()
            .and_then(|e| self.account_runtime(e))
            .or_else(|| self.accounts.first())
    }

    fn backend_for(&self, email: &str) -> Option<MailBackend> {
        self.account_runtime(email).map(|rt| rt.backend.clone())
    }

    fn clear_reading_pane(&mut self) {
        self.selected = None;
        self.opened = None;
        self.body_content = None;
        self.body_state = MailBodyState::default();
        self.attachments.clear();
        self.show_recipients = false;
    }

    fn rebuild_threads(&mut self) {
        if !self.threaded_mode {
            threading::sort_flat_items(&mut self.flat_items);
        }
        self.threads = threading::build_threads(self.flat_items.clone());
    }

    fn rebuild_all_inboxes_from_cache(&mut self) {
        let mut items = Vec::new();
        for rt in &self.accounts {
            let inbox = rt.inbox_folder_name();
            let summaries = if !rt.inbox_summaries.is_empty() {
                rt.inbox_summaries.clone()
            } else {
                rt.backend
                    .cached_headers(&inbox)
                    .map(|p| p.summaries)
                    .unwrap_or_default()
            };
            for summary in summaries {
                items.push(MailItem {
                    account_email: rt.account.email.clone(),
                    folder: inbox.clone(),
                    summary,
                });
            }
        }
        threading::sort_flat_items(&mut items);
        self.flat_items = items;
        self.has_more = false;
        self.rebuild_threads();
    }

    fn current_folder_display(&self) -> String {
        match &self.view {
            SidebarView::AllInboxes => "All Inboxes".to_owned(),
            SidebarView::AccountFolder {
                account_email,
                folder,
            } => self
                .account_runtime(account_email)
                .and_then(|rt| rt.folders.iter().find(|f| f.raw_name == *folder))
                .map(|f| f.display_name.clone())
                .unwrap_or_else(|| ui_helpers::friendly_folder_name(folder)),
        }
    }

    fn selected_folder_is_trash(&self) -> bool {
        let Some(key) = self.selected.as_ref() else {
            return false;
        };
        self.account_runtime(&key.account_email)
            .and_then(|rt| rt.folders.iter().find(|f| f.raw_name == key.folder))
            .is_some_and(|f| f.kind == FolderKind::Trash)
            || {
                let lower = key.folder.to_lowercase();
                lower.contains("trash") || lower.contains("deleted") || lower.contains("bin")
            }
    }

    fn selected_folder_is_drafts(&self) -> bool {
        let Some(key) = self.selected.as_ref() else {
            return false;
        };
        self.account_runtime(&key.account_email)
            .and_then(|rt| rt.folders.iter().find(|f| f.raw_name == key.folder))
            .is_some_and(|f| f.kind == FolderKind::Drafts)
            || key.folder.to_ascii_lowercase().contains("draft")
    }

    fn selected_has_archive_folder(&self) -> bool {
        let Some(key) = self.selected.as_ref() else {
            return false;
        };
        self.account_runtime(&key.account_email)
            .is_some_and(|rt| rt.folders.iter().any(|f| f.kind == FolderKind::Archive))
    }

    fn sync_compose_contacts(&mut self) {
        for rt in &self.accounts {
            let pairs = rt
                .backend
                .known_contacts()
                .into_iter()
                .map(|c| (c.name, c.email));
            self.compose.merge_contact_pairs(pairs);
        }
    }

    fn selected_item(&self) -> Option<&MailItem> {
        let key = self.selected.as_ref()?;
        self.flat_items.iter().find(|item| item.key() == *key)
    }

    fn current_original_message(&self) -> Option<OriginalMessage> {
        let key = self.selected.as_ref()?;
        if let Some(opened) = self.opened.as_ref()
            && opened.uid == key.uid
        {
            if let Some(parsed) = mail_parser::MessageParser::default().parse(&opened.raw) {
                return Some(compose::original_from_parsed(
                    opened.uid,
                    &parsed,
                    &opened.subject,
                    &opened.date,
                ));
            }
            let parse_addrs = |items: &[String]| -> Vec<EmailAddress> {
                items
                    .iter()
                    .filter_map(|raw| {
                        let addr = extract_email_addr(raw);
                        if addr.is_empty() {
                            return None;
                        }
                        let name = raw
                            .find('<')
                            .map(|i| raw[..i].trim().trim_matches('"').to_owned())
                            .filter(|s| !s.is_empty());
                        Some(EmailAddress::new(name, addr))
                    })
                    .collect()
            };
            return Some(OriginalMessage {
                uid: opened.uid,
                message_id: opened.message_id.clone(),
                in_reply_to: opened.in_reply_to.clone(),
                references: opened.references.clone(),
                from: vec![EmailAddress::new(
                    (!opened.from.is_empty()).then(|| opened.from.clone()),
                    if opened.from_address.is_empty() {
                        extract_email_addr(&opened.from)
                    } else {
                        opened.from_address.clone()
                    },
                )],
                reply_to: opened
                    .reply_to
                    .as_ref()
                    .map(std::slice::from_ref)
                    .map(parse_addrs)
                    .unwrap_or_default(),
                to: parse_addrs(&opened.to),
                cc: parse_addrs(&opened.cc),
                subject: opened.subject.clone(),
                date: opened.date.clone(),
                body_text: opened.text.clone(),
            });
        }
        let summary = &self.selected_item()?.summary;
        Some(OriginalMessage {
            uid: summary.uid,
            message_id: summary.message_id.clone(),
            in_reply_to: summary.in_reply_to.clone(),
            references: summary.references.clone(),
            from: vec![EmailAddress::new(
                (!summary.from.is_empty()).then(|| summary.from.clone()),
                summary.from_address.clone(),
            )],
            reply_to: Vec::new(),
            to: Vec::new(),
            cc: Vec::new(),
            subject: summary.subject.clone(),
            date: summary.date.clone(),
            body_text: summary.preview.clone(),
        })
    }

    fn adjust_folder_unread(&mut self, account_email: &str, folder: &str, delta: isize) {
        if let Some(rt) = self.account_runtime_mut(account_email)
            && let Some(info) = rt.folders.iter_mut().find(|f| f.raw_name == folder)
        {
            if delta < 0 {
                info.unread = info.unread.saturating_sub((-delta) as u32);
            } else {
                info.unread = info.unread.saturating_add(delta as u32);
            }
        }
    }

    fn select_message_by_key(&mut self, key: MessageKey) -> app::Task<Msg> {
        self.selected = Some(key.clone());
        self.opened = None;
        self.body_content = None;
        self.body_state = MailBodyState::default();
        self.attachments.clear();
        self.show_recipients = false;

        let mut marked_read = false;
        for item in &mut self.flat_items {
            if item.key() == key && item.summary.unread {
                item.summary.unread = false;
                marked_read = true;
            }
        }
        if let Some(rt) = self.account_runtime_mut(&key.account_email) {
            let inbox = rt.inbox_folder_name();
            if key.folder == inbox
                && let Some(sum) = rt.inbox_summaries.iter_mut().find(|m| m.uid == key.uid)
            {
                sum.unread = false;
            }
        }
        if marked_read {
            self.adjust_folder_unread(&key.account_email, &key.folder, -1);
            self.rebuild_threads();
        }
        let Some(backend) = self.backend_for(&key.account_email) else {
            return Task::none();
        };
        let folder = key.folder.clone();
        let uid = key.uid;
        run(
            move || backend.open_message(&folder, uid),
            move |result| Msg::Body(key, Box::new(result)),
        )
    }

    fn load_folder_headers(&mut self, account_email: &str, folder: &str) -> app::Task<Msg> {
        let Some(backend) = self.backend_for(account_email) else {
            return Task::none();
        };
        if let Some(cached) = backend.cached_headers(folder)
            && !cached.summaries.is_empty()
        {
            self.flat_items = cached
                .summaries
                .into_iter()
                .map(|summary| MailItem {
                    account_email: account_email.to_owned(),
                    folder: folder.to_owned(),
                    summary,
                })
                .collect();
            self.has_more = cached.has_more;
            self.rebuild_threads();
        }
        self.busy = true;
        let acct = account_email.to_owned();
        let fld = folder.to_owned();
        let fld_arg = fld.clone();
        run(
            move || backend.sync_headers(&fld_arg, DEFAULT_PAGE_SIZE),
            move |result| Msg::Headers {
                account_email: acct,
                folder: fld,
                result,
            },
        )
    }

    fn sync_all_inboxes(&mut self) -> app::Task<Msg> {
        self.rebuild_all_inboxes_from_cache();
        if self.accounts.is_empty() {
            return Task::none();
        }
        self.busy = true;
        let tasks: Vec<app::Task<Msg>> = self
            .accounts
            .iter()
            .map(|rt| {
                let backend = rt.backend.clone();
                let acct = rt.account.email.clone();
                let inbox = rt.inbox_folder_name();
                let inbox_arg = inbox.clone();
                run(
                    move || backend.sync_headers(&inbox_arg, DEFAULT_PAGE_SIZE),
                    move |result| Msg::Headers {
                        account_email: acct,
                        folder: inbox,
                        result,
                    },
                )
            })
            .collect();
        Task::batch(tasks)
    }

    fn visible_row_keys(&self) -> Vec<MessageKey> {
        if self.threaded_mode {
            self.threads.iter().map(|t| t.latest().key()).collect()
        } else {
            self.flat_items.iter().map(MailItem::key).collect()
        }
    }

    fn open_compose_with_title(
        &mut self,
        open: (window::Id, app::Task<ComposeMessage>, String),
    ) -> app::Task<Msg> {
        let (id, task, title) = open;
        let title_task = self.set_window_title(title, id);
        Task::batch([task.map(|a| a.map(Msg::ComposeEvent)), title_task])
    }

    fn trigger_remote_image_load(&mut self, key: MessageKey) -> app::Task<Msg> {
        if let Some(content) = self.body_content.as_ref()
            && !self.body_state.loading_remote_images
            && !content.remote_image_urls.is_empty()
        {
            self.body_state.loading_remote_images = true;
            let urls = content.remote_image_urls.clone();
            return run(
                move || html_mail::fetch_remote_images(&urls),
                move |results| Msg::RemoteImagesLoaded(key, results),
            );
        }
        Task::none()
    }

    fn sidebar(&self) -> Element<'_, Msg> {
        let mut list = column![].spacing(3);

        // Unified "All Inboxes" view.
        let total_inbox_unread: u32 = self.accounts.iter().map(AccountRuntime::inbox_unread).sum();
        let all_selected = matches!(self.view, SidebarView::AllInboxes);
        let mut all_row = row![
            widget::icon::from_name("mail-inbox-symbolic").size(16).icon(),
            text::heading("All Inboxes").width(Length::Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if total_inbox_unread > 0 {
            all_row = all_row.push(count_badge(total_inbox_unread.to_string(), all_selected));
        }
        list = list.push(
            button::custom(all_row)
                .class(list_row_button_class(all_selected))
                .width(Length::Fill)
                .padding([6, 8])
                .on_press(Msg::SelectAllInboxes),
        );

        list = list.push(divider::horizontal::default());

        // Account switcher when multiple accounts are configured (or single account header).
        list = list.push(
            text::caption("Accounts")
                .class(muted_text_class())
                .apply(container)
                .padding([4, 4, 2, 4]),
        );

        let active_email = self.active_runtime().map(|rt| rt.account.email.as_str());
        for rt in &self.accounts {
            let is_active = active_email.is_some_and(|e| e.eq_ignore_ascii_case(&rt.account.email));
            let unread = rt.inbox_unread();
            let mut acct_row = row![
                widget::icon::from_name(if is_active {
                    "emblem-default-symbolic"
                } else {
                    "avatar-default-symbolic"
                })
                .size(14)
                .icon(),
                text::caption(rt.account.email.as_str()).width(Length::Fill),
            ]
            .spacing(6)
            .align_y(Alignment::Center);
            if unread > 0 {
                acct_row = acct_row.push(count_badge(unread.to_string(), is_active));
            }
            list = list.push(
                button::custom(acct_row)
                    .class(list_row_button_class(is_active))
                    .width(Length::Fill)
                    .padding([5, 8])
                    .on_press(Msg::SwitchAccount(rt.account.email.clone())),
            );
        }

        if let Some(rt) = self.active_runtime() {
            list = list.push(divider::horizontal::default());
            list = list.push(
                text::caption("Mailboxes")
                    .class(muted_text_class())
                    .apply(container)
                    .padding([4, 4, 2, 4]),
            );
            for info in &rt.folders {
                let unread = info.unread;
                let is_selected = matches!(
                    &self.view,
                    SidebarView::AccountFolder { account_email, folder }
                    if account_email.eq_ignore_ascii_case(&rt.account.email) && folder == &info.raw_name
                );
                let label = if info.display_name.is_empty() {
                    ui_helpers::friendly_folder_name(&info.raw_name)
                } else {
                    info.display_name.clone()
                };
                let mut entry = row![
                    widget::icon::from_name(folder_icon_for(info))
                        .size(16)
                        .icon(),
                    text::body(label).width(Length::Fill),
                ]
                .spacing(8)
                .align_y(Alignment::Center);

                if unread > 0 {
                    entry = entry.push(count_badge(unread.to_string(), is_selected));
                }

                list = list.push(
                    button::custom(entry)
                        .class(list_row_button_class(is_selected))
                        .width(Length::Fill)
                        .padding([6, 8])
                        .on_press(Msg::SelectFolder {
                            account_email: rt.account.email.clone(),
                            folder: info.raw_name.clone(),
                        }),
                );
            }
        }

        let mut footer = column![
            button::standard("Add Account…")
                .width(Length::Fill)
                .on_press(Msg::OpenAddAccount),
        ]
        .spacing(6);
        if let Some(rt) = self.active_runtime() {
            footer = footer.push(
                button::text(format!("Sign Out ({})", rt.account.email))
                    .on_press(Msg::SignOutAccount(rt.account.email.clone())),
            );
        }

        column![scrollable(list).height(Length::Fill), footer]
            .spacing(8)
            .padding(12)
            .width(Length::Fixed(SIDEBAR_WIDTH))
            .height(Length::Fill)
            .into()
    }

    fn message_row<'a>(
        &self,
        item: &'a MailItem,
        thread_count: usize,
        has_unread: bool,
        has_attachments: bool,
        is_selected: bool,
    ) -> Element<'a, Msg> {
        let message = &item.summary;
        let rel_date = ui_helpers::format_relative_date(message.timestamp, &message.date);

        let unread_dot: Element<'_, Msg> = if has_unread {
            widget::space::horizontal()
                .width(Length::Fixed(8.0))
                .height(Length::Fixed(8.0))
                .apply(container)
                .class(theme::Container::custom(|theme| widget::container::Style {
                    background: Some(Background::Color(theme.cosmic().accent.base.into())),
                    border: Border {
                        radius: 4.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }))
                .into()
        } else {
            widget::space::horizontal()
                .width(Length::Fixed(8.0))
                .height(Length::Fixed(8.0))
                .into()
        };

        let from = if has_unread {
            text::heading(message.from.as_str())
        } else {
            text::body(message.from.as_str())
        };

        let mut top_row = row![from.width(Length::Fill)]
            .spacing(6)
            .align_y(Alignment::Center);
        if thread_count > 1 {
            top_row = top_row.push(count_badge(thread_count.to_string(), is_selected));
        }
        if has_attachments {
            top_row = top_row.push(
                widget::icon::from_name("mail-attachment-symbolic")
                    .size(13)
                    .icon(),
            );
        }
        top_row = top_row.push(text::caption(rel_date).class(muted_text_class()));

        let subject = if has_unread {
            text::caption_heading(message.subject.as_str())
        } else {
            text::caption(message.subject.as_str())
        };

        let mut content = column![top_row, subject].spacing(2).width(Length::Fill);

        if matches!(self.view, SidebarView::AllInboxes) && self.accounts.len() > 1 {
            content = content.push(
                text::caption(item.account_email.as_str()).class(muted_text_class()),
            );
        }

        if !message.preview.is_empty() {
            let snippet = ui_helpers::extract_preview_snippet(
                Some(message.preview.as_str()),
                None,
                96,
            );
            content = content.push(
                text::caption(snippet)
                    .class(muted_text_class())
                    .wrapping(cosmic::iced::widget::text::Wrapping::WordOrGlyph),
            );
        }

        let row_body = row![
            unread_dot.apply(container).padding([6, 0, 0, 0]),
            content,
        ]
        .spacing(8)
        .align_y(Alignment::Start)
        .padding([8, 10]);

        button::custom(row_body)
            .class(list_row_button_class(is_selected))
            .width(Length::Fill)
            .padding(0)
            .on_press(Msg::SelectMessage(item.key()))
            .into()
    }

    fn message_list(&self) -> Element<'_, Msg> {
        let mut list = column![].spacing(2);
        if self.threaded_mode {
            for thread in &self.threads {
                let latest = thread.latest();
                let is_selected = self
                    .selected
                    .as_ref()
                    .is_some_and(|key| thread.contains_key(key));
                list = list
                    .push(self.message_row(
                        latest,
                        thread.messages.len(),
                        thread.has_unread,
                        thread.has_attachments,
                        is_selected,
                    ))
                    .push(divider::horizontal::default());
            }
        } else {
            for item in &self.flat_items {
                let is_selected = self
                    .selected
                    .as_ref()
                    .is_some_and(|key| *key == item.key());
                list = list
                    .push(self.message_row(
                        item,
                        1,
                        item.summary.unread,
                        item.summary.has_attachments,
                        is_selected,
                    ))
                    .push(divider::horizontal::default());
            }
        }

        if matches!(self.view, SidebarView::AccountFolder { .. })
            && self.has_more
            && !self.searching
            && !self.flat_items.is_empty()
        {
            let label = if self.loading_older {
                "Loading older messages…"
            } else {
                "Load older messages"
            };
            list = list.push(
                button::standard(label)
                    .on_press_maybe((!self.loading_older).then_some(Msg::LoadOlder))
                    .width(Length::Fill)
                    .apply(container)
                    .padding([8, 12]),
            );
        }
        if self.flat_items.is_empty() && !self.busy {
            list = list.push(text::body("No messages").apply(container).padding(16));
        }

        let folder_title = self.current_folder_display();
        let mode_label = if self.threaded_mode { "Threads" } else { "Flat" };
        let header = row![
            text::title3(folder_title).width(Length::Fill),
            button::text(mode_label)
                .tooltip("Toggle Threaded / Flat list")
                .on_press(Msg::ToggleThreading),
            button::icon(widget::icon::from_name("view-refresh-symbolic"))
                .tooltip("Refresh")
                .on_press(Msg::Refresh),
            button::icon(widget::icon::from_name("mail-message-new-symbolic"))
                .tooltip("New Message")
                .on_press(Msg::Compose),
        ]
        .spacing(4)
        .align_y(Alignment::Center)
        .padding([8, 12]);

        let search = widget::search_input("Search", self.search_text.as_str())
            .on_input(Msg::SearchText)
            .on_submit(|_| Msg::RunSearch)
            .on_clear(Msg::ClearSearch)
            .apply(container)
            .padding([0, 12, 8, 12]);

        column![header, search, scrollable(list).height(Length::Fill)]
            .width(Length::Fixed(LIST_WIDTH))
            .height(Length::Fill)
            .into()
    }

    fn thread_switcher_card<'a>(
        &self,
        thread: &'a MailThread,
        selected_key: &MessageKey,
    ) -> Element<'a, Msg> {
        let count = thread.messages.len();
        let toggle_label = if self.thread_expanded {
            "Hide ▴"
        } else {
            "Show all ▾"
        };
        let mut card = column![
            row![
                text::caption_heading(format!("Conversation · {count} messages"))
                    .width(Length::Fill),
                button::text(toggle_label)
                    .padding([2, 6])
                    .on_press(Msg::ToggleThreadPanel),
            ]
            .align_y(Alignment::Center),
        ]
        .spacing(4);

        if self.thread_expanded {
            let mut items_col = column![].spacing(2);
            for (idx, item) in thread.messages.iter().enumerate() {
                let is_current = item.key() == *selected_key;
                let rel_date = ui_helpers::format_relative_date(
                    item.summary.timestamp,
                    &item.summary.date,
                );
                let snippet = if item.summary.preview.is_empty() {
                    item.summary.subject.clone()
                } else {
                    ui_helpers::extract_preview_snippet(
                        Some(item.summary.preview.as_str()),
                        None,
                        64,
                    )
                };
                let mut entry = row![
                    text::caption(format!("{}.", idx + 1)).class(muted_text_class()),
                    if item.summary.unread {
                        text::caption_heading(item.summary.from.as_str())
                    } else {
                        text::caption(item.summary.from.as_str())
                    },
                    text::caption(snippet)
                        .class(muted_text_class())
                        .width(Length::Fill),
                    text::caption(rel_date).class(muted_text_class()),
                ]
                .spacing(8)
                .align_y(Alignment::Center);

                if item.summary.unread {
                    entry = entry.push(count_badge("Unread".to_owned(), is_current));
                }

                items_col = items_col.push(
                    button::custom(entry)
                        .class(list_row_button_class(is_current))
                        .width(Length::Fill)
                        .padding([4, 8])
                        .on_press(Msg::SelectMessage(item.key())),
                );
            }
            card = card.push(items_col);
        }

        card.apply(container)
            .padding([8, 12])
            .width(Length::Fill)
            .class(theme::Container::Card)
            .into()
    }

    fn reading_pane(&self) -> Element<'_, Msg> {
        let Some(selected_item) = self.selected_item() else {
            return text::body("No message selected")
                .class(muted_text_class())
                .apply(container)
                .center(Length::Fill)
                .into();
        };
        let selected_key = selected_item.key();
        let message = &selected_item.summary;
        let in_trash = self.selected_folder_is_trash();

        let mut actions = row![space_fill()].spacing(4).align_y(Alignment::Center);
        if self.selected_folder_is_drafts() {
            actions = actions.push(button::standard("Edit Draft").on_press(Msg::EditDraft));
        }
        actions = actions
            .push(
                button::icon(widget::icon::from_name("mail-reply-sender-symbolic"))
                    .tooltip("Reply (R)")
                    .on_press(Msg::Reply),
            )
            .push(
                button::icon(widget::icon::from_name("mail-reply-all-symbolic"))
                    .tooltip("Reply All")
                    .on_press(Msg::ReplyAll),
            )
            .push(
                button::icon(widget::icon::from_name("mail-forward-symbolic"))
                    .tooltip("Forward")
                    .on_press(Msg::Forward),
            );
        if self.selected_has_archive_folder() {
            actions = actions.push(
                button::icon(widget::icon::from_name("mail-archive-symbolic"))
                    .tooltip("Archive")
                    .on_press(Msg::Archive),
            );
        }
        actions = actions
            .push(
                button::icon(widget::icon::from_name(if in_trash {
                    "edit-delete-symbolic"
                } else {
                    "user-trash-symbolic"
                }))
                .tooltip(if in_trash {
                    "Delete Permanently (Del)"
                } else {
                    "Move to Trash (Del)"
                })
                .on_press(Msg::Delete),
            )
            .push(
                button::icon(widget::icon::from_name("mail-unread-symbolic"))
                    .tooltip("Mark Unread")
                    .on_press(Msg::MarkUnread),
            );

        // Sender initials avatar circle.
        let initials = ui_helpers::sender_initials(&message.from, &message.from_address);
        let [r, g, b] = ui_helpers::avatar_color_rgb(if message.from_address.is_empty() {
            &message.from
        } else {
            &message.from_address
        });
        let avatar = text::heading(initials)
            .class(theme::Text::Color(Color::WHITE))
            .apply(container)
            .width(Length::Fixed(40.0))
            .height(Length::Fixed(40.0))
            .center(Length::Fixed(40.0))
            .class(theme::Container::custom(move |_| widget::container::Style {
                background: Some(Background::Color(Color::from_rgb(r, g, b))),
                border: Border {
                    radius: 20.0.into(),
                    ..Default::default()
                },
                text_color: Some(Color::WHITE),
                ..Default::default()
            }));

        let mut sender_line = row![text::heading(message.from.as_str())]
            .spacing(6)
            .align_y(Alignment::Center);
        if !message.from_address.is_empty() && message.from_address != message.from {
            sender_line = sender_line.push(
                text::caption(format!("<{}>", message.from_address)).class(muted_text_class()),
            );
        }

        let header_date = ui_helpers::format_header_date(message.timestamp, &message.date);
        let toggle_label = if self.show_recipients {
            "Hide details ▴"
        } else {
            "Details ▾"
        };

        let to_list = self
            .opened
            .as_ref()
            .filter(|o| !o.to.is_empty())
            .map_or(message.to.as_slice(), |o| o.to.as_slice());
        let cc_list = self
            .opened
            .as_ref()
            .filter(|o| !o.cc.is_empty())
            .map_or(message.cc.as_slice(), |o| o.cc.as_slice());

        let mut sender_col = column![sender_line].spacing(2).width(Length::Fill);
        if !self.show_recipients && !to_list.is_empty() {
            let summary_to = if to_list.len() == 1 {
                format!("To: {}", to_list[0])
            } else {
                format!("To: {} (+{} more)", to_list[0], to_list.len() - 1)
            };
            sender_col = sender_col.push(text::caption(summary_to).class(muted_text_class()));
        }

        let sender_header = row![
            avatar,
            sender_col,
            column![
                text::caption(header_date).class(muted_text_class()),
                button::text(toggle_label)
                    .padding([2, 6])
                    .on_press(Msg::ToggleRecipients),
            ]
            .align_x(Alignment::End)
            .spacing(2),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        let mut header_block = column![
            row![text::title3(message.subject.as_str()).width(Length::Fill), actions]
                .align_y(Alignment::Center),
            sender_header,
        ]
        .spacing(10);

        if let Some(thread) = self
            .threads
            .iter()
            .find(|t| t.contains_key(&selected_key) && t.messages.len() > 1)
        {
            header_block = header_block.push(self.thread_switcher_card(thread, &selected_key));
        }

        if self.show_recipients {
            let mut details = column![].spacing(4);
            let from_full = if message.from_address.is_empty() {
                message.from.clone()
            } else if message.from == message.from_address {
                message.from_address.clone()
            } else {
                format!("{} <{}>", message.from, message.from_address)
            };
            details = details.push(text::caption(format!("From: {from_full}")));
            if !to_list.is_empty() {
                details = details.push(text::caption(format!("To: {}", to_list.join(", "))));
            }
            if !cc_list.is_empty() {
                details = details.push(text::caption(format!("Cc: {}", cc_list.join(", "))));
            }
            if !message.date.is_empty() {
                details = details.push(text::caption(format!("Date: {}", message.date)));
            }
            header_block = header_block.push(
                details
                    .apply(container)
                    .padding([8, 12])
                    .width(Length::Fill)
                    .class(theme::Container::custom(|theme| {
                        let cosmic = theme.cosmic();
                        let mut bg: Color = cosmic.background(theme.transparent).on.into();
                        bg.a = 0.05;
                        widget::container::Style {
                            background: Some(Background::Color(bg)),
                            border: Border {
                                radius: cosmic.corner_radii.radius_s.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }
                    })),
            );
        }

        let mut attachments = row![].spacing(8);
        for (index, att) in self.attachments.iter().enumerate() {
            attachments = attachments.push(
                button::standard(att.filename.as_str()).on_press(Msg::SaveAttachment(index)),
            );
        }

        let body_view: Element<'_, Msg> = match self.body_content.as_ref() {
            Some(content) => html_mail::view(content, &self.body_state, Msg::HtmlMail),
            None => text::body("Loading…").into(),
        };

        let mut content = column![header_block].spacing(12);
        if !self.attachments.is_empty() {
            content = content.push(attachments);
        }
        content = content
            .push(divider::horizontal::default())
            .push(scrollable(body_view).height(Length::Fill));

        content
            .padding(20)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn setup_view(&self) -> Element<'_, Msg> {
        let mut form = column![
            text::title2("Add a mail account"),
            text::caption(
                "Sign in with an app password for any IMAP/SMTP provider, or use OAuth2 browser sign-in for Google or Microsoft."
            ),
            widget::text_input("Email address", self.setup.email.as_str()).on_input(Msg::FormEmail),
            widget::text_input("Password", self.setup.password.as_str())
                .password()
                .on_input(Msg::FormPassword),
            widget::text_input("Incoming server (IMAP)", self.setup.imap_host.as_str())
                .on_input(Msg::FormImap),
            widget::text_input("Outgoing server (SMTP)", self.setup.smtp_host.as_str())
                .on_input(Msg::FormSmtp),
            button::suggested(if self.busy { "Signing in…" } else { "Sign In" })
                .on_press_maybe((!self.busy).then_some(Msg::SignIn)),
            divider::horizontal::default(),
            text::caption("Browser Sign-In (OAuth2 PKCE)"),
            widget::text_input(
                "OAuth2 Client ID (or set MAC_MAIL_GMAIL_CLIENT_ID / MAC_MAIL_OUTLOOK_CLIENT_ID)",
                self.setup.oauth_client_id.as_str(),
            )
            .on_input(Msg::FormOAuthClientId),
            row![
                button::standard("Sign in with Google")
                    .on_press_maybe((!self.busy).then_some(Msg::StartOAuth(OAuthProvider::Gmail))),
                button::standard("Sign in with Microsoft")
                    .on_press_maybe((!self.busy).then_some(Msg::StartOAuth(OAuthProvider::Outlook))),
            ]
            .spacing(8),
            text::body(self.status.clone().unwrap_or_default()),
        ]
        .spacing(10)
        .width(Length::Fixed(460.0));

        if !self.accounts.is_empty() {
            form = form.push(button::text("Cancel").on_press(Msg::CancelAddAccount));
        }

        container(form).center(Length::Fill).into()
    }
}

impl cosmic::Application for Mail {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Msg;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn init(mut core: app::Core, _flags: ()) -> (Self, app::Task<Msg>) {
        core.window.header_title = "Mail".to_owned();
        let initial_mailto = compose::extract_mailto_from_args(std::env::args().skip(1));
        let trusted_senders = TrustedSenders::load_default();
        let cfg = account::load_all();

        let mut mail = Self {
            core,
            accounts: Vec::new(),
            active_account: cfg.active_account.clone(),
            view: SidebarView::AllInboxes,
            adding_account: false,
            setup: Setup::default(),
            threaded_mode: true,
            thread_expanded: true,
            flat_items: Vec::new(),
            threads: Vec::new(),
            has_more: false,
            loading_older: false,
            selected: None,
            opened: None,
            body_content: None,
            body_state: MailBodyState::default(),
            attachments: Vec::new(),
            show_recipients: false,
            search_text: String::new(),
            searching: false,
            status: None,
            busy: false,
            compose: ComposeManager::new(),
            trusted_senders,
            pending_mailto: initial_mailto,
        };

        let mut startup_tasks = Vec::new();
        for account in cfg.accounts {
            let Some(credential) = account::load_credential(&account) else {
                continue;
            };
            let backend = MailBackend::new(account.clone(), credential.clone());
            let cached_folders = backend.cached_folders();
            let inbox_name = cached_folders
                .iter()
                .find(|f| f.kind == FolderKind::Inbox || f.raw_name.eq_ignore_ascii_case("INBOX"))
                .map_or_else(|| "INBOX".to_owned(), |f| f.raw_name.clone());
            let inbox_summaries = backend
                .cached_headers(&inbox_name)
                .map(|p| p.summaries)
                .unwrap_or_default();
            let email = account.email.clone();
            mail.accounts.push(AccountRuntime {
                account,
                credential,
                backend: backend.clone(),
                folders: cached_folders,
                inbox_summaries,
            });
            startup_tasks.push(run(move || backend.sync_folders(), move |res| {
                Msg::Folders(email, res)
            }));
        }

        if !mail.accounts.is_empty() {
            if mail.accounts.len() == 1 {
                let rt = &mail.accounts[0];
                let default_folder = rt.default_folder();
                mail.active_account = Some(rt.account.email.clone());
                mail.view = SidebarView::AccountFolder {
                    account_email: rt.account.email.clone(),
                    folder: default_folder.clone(),
                };
                if let Some(cached_page) = rt.backend.cached_headers(&default_folder) {
                    let acct = rt.account.email.clone();
                    mail.flat_items = cached_page
                        .summaries
                        .into_iter()
                        .map(|summary| MailItem {
                            account_email: acct.clone(),
                            folder: default_folder.clone(),
                            summary,
                        })
                        .collect();
                    mail.has_more = cached_page.has_more;
                    mail.rebuild_threads();
                }
            } else {
                mail.view = SidebarView::AllInboxes;
                mail.rebuild_all_inboxes_from_cache();
            }
            mail.sync_compose_contacts();
            mail.busy = true;

            if let Some(mailto) = mail.pending_mailto.take() {
                let from = mail
                    .active_runtime()
                    .map(|rt| rt.account.email.clone())
                    .unwrap_or_default();
                let open = mail.compose.open_mailto(&from, &mailto);
                startup_tasks.push(mail.open_compose_with_title(open));
            }
        }

        (mail, Task::batch(startup_tasks))
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Msg> {
        if self.compose.has_window(id) {
            Some(Msg::ComposeEvent(ComposeMessage::CloseRequested(id)))
        } else {
            None
        }
    }

    fn subscription(&self) -> Subscription<Msg> {
        let mut subs = Vec::new();

        for rt in &self.accounts {
            subs.push(Subscription::run_with(
                (rt.account.clone(), rt.credential.clone()),
                idle_subscription_worker,
            ));
        }

        if !self.accounts.is_empty() && self.compose.has_dirty_windows() {
            subs.push(
                time::every(Duration::from_secs(AUTO_DRAFT_INTERVAL_SECS))
                    .map(|_| Msg::AutoSaveDrafts),
            );
        }

        subs.push(event::listen_with(|ev, _status, id| {
            let Event::Window(win_event) = ev else {
                return None;
            };
            match win_event {
                window::Event::CloseRequested => {
                    Some(Msg::ComposeEvent(ComposeMessage::CloseRequested(id)))
                }
                window::Event::Closed => {
                    Some(Msg::ComposeEvent(ComposeMessage::WindowClosed(id)))
                }
                window::Event::FileHovered(_) => {
                    Some(Msg::ComposeEvent(ComposeMessage::FilesHovered(id)))
                }
                window::Event::FilesHoveredLeft => {
                    Some(Msg::ComposeEvent(ComposeMessage::FilesHoveredLeft(id)))
                }
                window::Event::FileDropped(paths) => {
                    Some(Msg::ComposeEvent(ComposeMessage::FilesDropped(id, paths)))
                }
                _ => None,
            }
        }));

        let main_id = self.core().main_window_id();
        if !self.accounts.is_empty()
            && !self.adding_account
            && main_id.is_some()
            && self.core().focused_window() == main_id
        {
            subs.push(event::listen_with(|ev, status, _id| {
                if status != event::Status::Ignored {
                    return None;
                }
                let Event::Keyboard(keyboard::Event::KeyPressed {
                    key, modifiers, ..
                }) = ev
                else {
                    return None;
                };
                if modifiers.control() || modifiers.alt() || modifiers.logo() {
                    return None;
                }
                match key {
                    keyboard::Key::Named(keyboard::key::Named::ArrowDown) => {
                        Some(Msg::SelectNext)
                    }
                    keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some(Msg::SelectPrev),
                    keyboard::Key::Named(keyboard::key::Named::Delete) => Some(Msg::Delete),
                    keyboard::Key::Character(c) if c.eq_ignore_ascii_case("r") => Some(Msg::Reply),
                    _ => None,
                }
            }));
        }

        Subscription::batch(subs)
    }

    fn update(&mut self, message: Msg) -> app::Task<Msg> {
        match message {
            Msg::FormEmail(value) => {
                let guess = account::guess(&value);
                self.setup.imap_host = guess.imap_host;
                self.setup.smtp_host = guess.smtp_host;
                self.setup.email = value;
            }
            Msg::FormPassword(value) => self.setup.password = value,
            Msg::FormImap(value) => self.setup.imap_host = value,
            Msg::FormSmtp(value) => self.setup.smtp_host = value,
            Msg::FormOAuthClientId(value) => self.setup.oauth_client_id = value,
            Msg::SignIn => {
                let mut account = account::guess(&self.setup.email);
                account.imap_host = self.setup.imap_host.clone();
                account.smtp_host = self.setup.smtp_host.clone();
                if account.email.is_empty() || account.imap_host.is_empty() {
                    self.status = Some("Enter your address and server.".to_owned());
                    return Task::none();
                }
                self.busy = true;
                self.status = None;
                let credential = AccountCredential::Password(self.setup.password.clone());
                let backend = MailBackend::new(account.clone(), credential.clone());
                return run(
                    move || backend.sync_folders(),
                    move |result| Msg::AccountSignedIn {
                        account,
                        credential,
                        result,
                    },
                );
            }
            Msg::StartOAuth(provider) => {
                self.busy = true;
                self.status = Some(format!(
                    "Waiting for {} sign-in in your browser…",
                    provider.display_name()
                ));
                let client_id = self.setup.oauth_client_id.clone();
                let hint = self.setup.email.clone();
                return run(
                    move || {
                        oauth::run_pkce_browser_flow(
                            provider,
                            (!client_id.trim().is_empty()).then_some(client_id.as_str()),
                            (!hint.trim().is_empty()).then_some(hint.as_str()),
                        )
                    },
                    Msg::OAuthCompleted,
                );
            }
            Msg::OAuthCompleted(result) => match result {
                Ok(oauth_res) => {
                    let email = oauth_res
                        .email
                        .filter(|e| !e.trim().is_empty())
                        .unwrap_or_else(|| self.setup.email.trim().to_owned());
                    if email.is_empty() {
                        self.busy = false;
                        self.status = Some(
                            "Enter your email address before signing in with OAuth2.".to_owned(),
                        );
                        return Task::none();
                    }
                    let account =
                        account::oauth_account(oauth_res.provider, email, oauth_res.client_id);
                    let credential = AccountCredential::OAuth2(oauth_res.tokens);
                    let backend = MailBackend::new(account.clone(), credential.clone());
                    self.status = Some("Connecting mailbox…".to_owned());
                    return run(
                        move || backend.sync_folders(),
                        move |result| Msg::AccountSignedIn {
                            account,
                            credential,
                            result,
                        },
                    );
                }
                Err(err) => {
                    self.busy = false;
                    self.status = Some(err);
                }
            },
            Msg::AccountSignedIn {
                account,
                credential,
                result,
            } => match result {
                Ok(folders) => {
                    if let Err(err) = account::store_credential(&account, &credential)
                        .and_then(|()| account::upsert_account(account.clone()).map_err(|e| e.to_string()))
                    {
                        self.status = Some(err);
                    } else {
                        self.status = None;
                    }

                    let email = account.email.clone();
                    let backend = MailBackend::new(account.clone(), credential.clone());
                    if let Some(existing) = self.account_runtime_mut(&email) {
                        existing.account = account;
                        existing.credential = credential;
                        existing.backend = backend;
                        existing.folders = folders.clone();
                    } else {
                        self.accounts.push(AccountRuntime {
                            account,
                            credential,
                            backend,
                            folders: folders.clone(),
                            inbox_summaries: Vec::new(),
                        });
                    }
                    self.active_account = Some(email.clone());
                    self.adding_account = false;
                    self.setup = Setup::default();
                    self.clear_reading_pane();

                    let default_folder = self
                        .account_runtime(&email)
                        .map_or_else(|| "INBOX".to_owned(), AccountRuntime::default_folder);
                    self.view = SidebarView::AccountFolder {
                        account_email: email.clone(),
                        folder: default_folder.clone(),
                    };

                    let mut tasks = vec![self.load_folder_headers(&email, &default_folder)];
                    if let Some(mailto) = self.pending_mailto.take() {
                        let open = self.compose.open_mailto(&email, &mailto);
                        tasks.push(self.open_compose_with_title(open));
                    }
                    return Task::batch(tasks);
                }
                Err(err) => {
                    self.busy = false;
                    self.status = Some(err);
                }
            },
            Msg::OpenAddAccount => {
                self.adding_account = true;
                self.setup = Setup::default();
                self.status = None;
            }
            Msg::CancelAddAccount => {
                self.adding_account = false;
                self.status = None;
            }
            Msg::SignOutAccount(email) => {
                if let Some(pos) = self
                    .accounts
                    .iter()
                    .position(|rt| rt.account.email.eq_ignore_ascii_case(&email))
                {
                    let rt = self.accounts.remove(pos);
                    rt.backend.disconnect();
                    account::forget(&rt.account.email);
                }
                self.clear_reading_pane();
                self.flat_items.clear();
                self.threads.clear();
                self.has_more = false;
                self.loading_older = false;
                self.status = None;

                if let Some(next) = self.accounts.first().map(|rt| rt.account.email.clone()) {
                    self.active_account = Some(next.clone());
                    let _ = account::set_active_account(&next);
                    if self.accounts.len() > 1 {
                        self.view = SidebarView::AllInboxes;
                        return self.sync_all_inboxes();
                    }
                    let folder = self.accounts[0].default_folder();
                    self.view = SidebarView::AccountFolder {
                        account_email: next.clone(),
                        folder: folder.clone(),
                    };
                    return self.load_folder_headers(&next, &folder);
                } else {
                    self.active_account = None;
                    self.adding_account = false;
                }
            }
            Msg::SelectAllInboxes => {
                self.view = SidebarView::AllInboxes;
                self.searching = false;
                self.clear_reading_pane();
                return self.sync_all_inboxes();
            }
            Msg::SwitchAccount(email) => {
                let _ = account::set_active_account(&email);
                self.active_account = Some(email.clone());
                self.searching = false;
                self.clear_reading_pane();
                let folder = self
                    .account_runtime(&email)
                    .map_or_else(|| "INBOX".to_owned(), AccountRuntime::default_folder);
                self.view = SidebarView::AccountFolder {
                    account_email: email.clone(),
                    folder: folder.clone(),
                };
                return self.load_folder_headers(&email, &folder);
            }
            Msg::Folders(email, result) => match result {
                Ok(folders) => {
                    let info = if let Some(rt) = self.account_runtime_mut(&email) {
                        rt.folders = folders;
                        Some((
                            rt.inbox_folder_name(),
                            rt.default_folder(),
                            rt.backend.clone(),
                        ))
                    } else {
                        None
                    };
                    if let Some((inbox, default_folder, backend)) = info {
                        if matches!(self.view, SidebarView::AllInboxes) {
                            let acct = email.clone();
                            let inbox_arg = inbox.clone();
                            return run(
                                move || backend.sync_headers(&inbox_arg, DEFAULT_PAGE_SIZE),
                                move |res| Msg::Headers {
                                    account_email: acct,
                                    folder: inbox,
                                    result: res,
                                },
                            );
                        } else if let SidebarView::AccountFolder {
                            account_email,
                            folder,
                        } = &self.view
                            && account_email.eq_ignore_ascii_case(&email)
                        {
                            let acct = account_email.clone();
                            let mut fld = folder.clone();
                            let has_folder = self
                                .account_runtime(&email)
                                .is_some_and(|rt| rt.folders.iter().any(|f| f.raw_name == fld));
                            if !has_folder {
                                fld = default_folder;
                                self.view = SidebarView::AccountFolder {
                                    account_email: acct.clone(),
                                    folder: fld.clone(),
                                };
                            }
                            return self.load_folder_headers(&acct, &fld);
                        }
                    }
                }
                Err(err) => {
                    self.busy = false;
                    self.status = Some(err);
                }
            },
            Msg::SelectFolder {
                account_email,
                folder,
            } => {
                self.active_account = Some(account_email.clone());
                let _ = account::set_active_account(&account_email);
                self.view = SidebarView::AccountFolder {
                    account_email: account_email.clone(),
                    folder: folder.clone(),
                };
                self.flat_items.clear();
                self.threads.clear();
                self.has_more = false;
                self.loading_older = false;
                self.clear_reading_pane();
                self.searching = false;
                return self.load_folder_headers(&account_email, &folder);
            }
            Msg::Headers {
                account_email,
                folder,
                result,
            } => {
                self.busy = false;
                match result {
                    Ok(page) => {
                        let updated_folders = self
                            .backend_for(&account_email)
                            .map(|b| b.cached_folders())
                            .unwrap_or_default();
                        if let Some(rt) = self.account_runtime_mut(&account_email) {
                            if !updated_folders.is_empty() {
                                rt.folders = updated_folders;
                            }
                            if folder == rt.inbox_folder_name() {
                                rt.inbox_summaries = page.summaries.clone();
                            }
                        }

                        match &self.view {
                            SidebarView::AllInboxes if !self.searching => {
                                self.rebuild_all_inboxes_from_cache();
                                self.status = None;
                            }
                            SidebarView::AccountFolder {
                                account_email: view_acct,
                                folder: view_folder,
                            } if view_acct.eq_ignore_ascii_case(&account_email)
                                && *view_folder == folder
                                && !self.searching =>
                            {
                                self.has_more = page.has_more;
                                self.flat_items = page
                                    .summaries
                                    .into_iter()
                                    .map(|summary| MailItem {
                                        account_email: account_email.clone(),
                                        folder: folder.clone(),
                                        summary,
                                    })
                                    .collect();
                                self.rebuild_threads();
                                self.status = None;
                            }
                            _ => {}
                        }
                        self.sync_compose_contacts();
                    }
                    Err(err) => {
                        if self.flat_items.is_empty() {
                            self.status = Some(err);
                        } else {
                            self.status = Some(format!("Offline or sync error: {err}"));
                        }
                    }
                }
            }
            Msg::LoadOlder => {
                let SidebarView::AccountFolder {
                    account_email,
                    folder,
                } = &self.view
                else {
                    return Task::none();
                };
                let oldest = self
                    .flat_items
                    .iter()
                    .filter(|i| i.account_email.eq_ignore_ascii_case(account_email))
                    .map(|i| i.summary.uid)
                    .min();
                let (Some(backend), Some(oldest)) = (self.backend_for(account_email), oldest) else {
                    return Task::none();
                };
                self.loading_older = true;
                let acct = account_email.clone();
                let fld = folder.clone();
                let fld_arg = fld.clone();
                return run(
                    move || backend.load_older_headers(&fld_arg, oldest, DEFAULT_PAGE_SIZE),
                    move |result| Msg::OlderHeaders {
                        account_email: acct,
                        folder: fld,
                        result,
                    },
                );
            }
            Msg::OlderHeaders {
                account_email,
                folder,
                result,
            } => {
                self.loading_older = false;
                if let SidebarView::AccountFolder {
                    account_email: view_acct,
                    folder: view_folder,
                } = &self.view
                    && view_acct.eq_ignore_ascii_case(&account_email)
                    && *view_folder == folder
                {
                    match result {
                        Ok(page) => {
                            self.flat_items = page
                                .summaries
                                .into_iter()
                                .map(|summary| MailItem {
                                    account_email: account_email.clone(),
                                    folder: folder.clone(),
                                    summary,
                                })
                                .collect();
                            self.has_more = page.has_more;
                            self.rebuild_threads();
                            self.status = None;
                        }
                        Err(err) => self.status = Some(err),
                    }
                }
            }
            Msg::ToggleThreading => {
                self.threaded_mode = !self.threaded_mode;
                self.rebuild_threads();
            }
            Msg::ToggleThreadPanel => {
                self.thread_expanded = !self.thread_expanded;
            }
            Msg::Refresh => match self.view.clone() {
                SidebarView::AllInboxes => return self.sync_all_inboxes(),
                SidebarView::AccountFolder {
                    account_email,
                    folder,
                } => return self.load_folder_headers(&account_email, &folder),
            },
            Msg::NewMail(notif) => {
                let Some(rt) = self.account_runtime(&notif.account_email) else {
                    return Task::none();
                };
                let backend = rt.backend.clone();
                let inbox = rt.inbox_folder_name();
                let acct = rt.account.email.clone();
                let b1 = backend.clone();
                let inbox_arg = inbox.clone();
                let acct1 = acct.clone();
                let header_task = run(
                    move || b1.sync_headers(&inbox_arg, DEFAULT_PAGE_SIZE),
                    move |res| Msg::Headers {
                        account_email: acct1,
                        folder: inbox,
                        result: res,
                    },
                );
                let folders_task = run(move || backend.sync_folders(), move |res| {
                    Msg::Folders(acct, res)
                });
                return Task::batch([header_task, folders_task]);
            }
            Msg::SelectMessage(key) => {
                return self.select_message_by_key(key);
            }
            Msg::SelectNext => {
                let keys = self.visible_row_keys();
                if keys.is_empty() {
                    return Task::none();
                }
                let current_pos = self.selected.as_ref().and_then(|sel| {
                    if self.threaded_mode {
                        self.threads.iter().position(|t| t.contains_key(sel))
                    } else {
                        keys.iter().position(|k| k == sel)
                    }
                });
                let next_idx = match current_pos {
                    Some(idx) if idx + 1 < keys.len() => idx + 1,
                    Some(idx) => idx,
                    None => 0,
                };
                let next_key = keys[next_idx].clone();
                if self.selected.as_ref() != Some(&next_key) {
                    return self.select_message_by_key(next_key);
                }
            }
            Msg::SelectPrev => {
                let keys = self.visible_row_keys();
                if keys.is_empty() {
                    return Task::none();
                }
                let current_pos = self.selected.as_ref().and_then(|sel| {
                    if self.threaded_mode {
                        self.threads.iter().position(|t| t.contains_key(sel))
                    } else {
                        keys.iter().position(|k| k == sel)
                    }
                });
                let prev_idx = match current_pos {
                    Some(idx) => idx.saturating_sub(1),
                    None => 0,
                };
                let prev_key = keys[prev_idx].clone();
                if self.selected.as_ref() != Some(&prev_key) {
                    return self.select_message_by_key(prev_key);
                }
            }
            Msg::Body(key, result) => {
                if self.selected.as_ref() == Some(&key) {
                    self.body_state = MailBodyState::default();
                    match *result {
                        Ok(opened) => {
                            let content = if !opened.raw.is_empty() {
                                MailContent::from_raw_mime(&opened.raw)
                            } else if let Some(html) = opened.html.as_deref() {
                                MailContent::from_html(html, HashMap::new())
                            } else {
                                MailContent::from_plain_text(&opened.text)
                            };
                            let sender_addr = if opened.from_address.is_empty() {
                                extract_email_addr(&opened.from)
                            } else {
                                opened.from_address.clone()
                            };
                            self.body_state.sender_address = sender_addr.clone();
                            self.attachments = opened.attachments.clone();
                            let auto_load = !content.remote_image_urls.is_empty()
                                && self.trusted_senders.is_trusted(&sender_addr);
                            self.body_content = Some(content);
                            self.opened = Some(opened);
                            if auto_load {
                                return self.trigger_remote_image_load(key);
                            }
                        }
                        Err(err) => {
                            self.opened = None;
                            self.body_content = Some(MailContent::from_plain_text(&err));
                        }
                    }
                }
            }
            Msg::SearchText(value) => self.search_text = value,
            Msg::RunSearch => {
                let query = self.search_text.trim().to_owned();
                if query.is_empty() {
                    return self.update(Msg::ClearSearch);
                }
                self.busy = true;
                self.searching = true;
                let view_snapshot = self.view.clone();
                match &self.view {
                    SidebarView::AllInboxes => {
                        let targets: Vec<(String, String, MailBackend)> = self
                            .accounts
                            .iter()
                            .map(|rt| {
                                (
                                    rt.account.email.clone(),
                                    rt.inbox_folder_name(),
                                    rt.backend.clone(),
                                )
                            })
                            .collect();
                        if targets.is_empty() {
                            self.busy = false;
                            return Task::none();
                        }
                        return run(
                            move || {
                                let mut all = Vec::new();
                                let mut last_err = None;
                                for (acct, folder, backend) in targets {
                                    match backend.search(&folder, &query) {
                                        Ok(summaries) => {
                                            for summary in summaries {
                                                all.push(MailItem {
                                                    account_email: acct.clone(),
                                                    folder: folder.clone(),
                                                    summary,
                                                });
                                            }
                                        }
                                        Err(err) => last_err = Some(err),
                                    }
                                }
                                if all.is_empty()
                                    && let Some(err) = last_err
                                {
                                    Err(err)
                                } else {
                                    Ok(all)
                                }
                            },
                            move |result| Msg::SearchResults(view_snapshot, result),
                        );
                    }
                    SidebarView::AccountFolder {
                        account_email,
                        folder,
                    } => {
                        let Some(backend) = self.backend_for(account_email) else {
                            self.busy = false;
                            return Task::none();
                        };
                        let acct = account_email.clone();
                        let fld = folder.clone();
                        return run(
                            move || {
                                backend.search(&fld, &query).map(|summaries| {
                                    summaries
                                        .into_iter()
                                        .map(|summary| MailItem {
                                            account_email: acct.clone(),
                                            folder: fld.clone(),
                                            summary,
                                        })
                                        .collect()
                                })
                            },
                            move |result| Msg::SearchResults(view_snapshot, result),
                        );
                    }
                }
            }
            Msg::SearchResults(view_snapshot, result) => {
                self.busy = false;
                if view_snapshot == self.view {
                    match result {
                        Ok(items) => {
                            self.flat_items = items;
                            self.has_more = false;
                            self.rebuild_threads();
                            self.status = None;
                        }
                        Err(err) => self.status = Some(err),
                    }
                }
            }
            Msg::ClearSearch => {
                self.search_text.clear();
                self.searching = false;
                match self.view.clone() {
                    SidebarView::AllInboxes => return self.sync_all_inboxes(),
                    SidebarView::AccountFolder {
                        account_email,
                        folder,
                    } => return self.load_folder_headers(&account_email, &folder),
                }
            }
            Msg::Delete => {
                let Some(key) = self.selected.clone() else {
                    return Task::none();
                };
                let Some(backend) = self.backend_for(&key.account_email) else {
                    return Task::none();
                };
                self.busy = true;
                let folder = key.folder.clone();
                let uid = key.uid;
                return run(
                    move || backend.trash_or_delete_message(&folder, uid),
                    move |result| Msg::Moved(key, result),
                );
            }
            Msg::Archive => {
                let Some(key) = self.selected.clone() else {
                    return Task::none();
                };
                let Some(backend) = self.backend_for(&key.account_email) else {
                    return Task::none();
                };
                self.busy = true;
                let folder = key.folder.clone();
                let uid = key.uid;
                return run(
                    move || backend.archive_message(&folder, uid),
                    move |result| Msg::Moved(key, result),
                );
            }
            Msg::MarkUnread => {
                let Some(key) = self.selected.clone() else {
                    return Task::none();
                };
                let Some(backend) = self.backend_for(&key.account_email) else {
                    return Task::none();
                };
                let mut changed = false;
                for item in &mut self.flat_items {
                    if item.key() == key && !item.summary.unread {
                        item.summary.unread = true;
                        changed = true;
                    }
                }
                if changed {
                    self.adjust_folder_unread(&key.account_email, &key.folder, 1);
                    self.rebuild_threads();
                }
                let folder = key.folder.clone();
                let uid = key.uid;
                return run(
                    move || backend.set_unread(&folder, uid, true),
                    move |result| Msg::MarkedUnread(key, result),
                );
            }
            Msg::MarkedUnread(_key, result) => {
                if let Err(err) = result {
                    self.status = Some(err);
                }
            }
            Msg::ToggleRecipients => {
                self.show_recipients = !self.show_recipients;
            }
            Msg::Moved(key, result) => {
                self.busy = false;
                match result {
                    Ok(()) => {
                        let was_unread = self
                            .flat_items
                            .iter()
                            .find(|i| i.key() == key)
                            .is_some_and(|i| i.summary.unread);
                        if was_unread {
                            self.adjust_folder_unread(&key.account_email, &key.folder, -1);
                        }
                        self.flat_items.retain(|i| i.key() != key);
                        if let Some(rt) = self.account_runtime_mut(&key.account_email)
                            && key.folder == rt.inbox_folder_name()
                        {
                            rt.inbox_summaries.retain(|m| m.uid != key.uid);
                        }
                        self.rebuild_threads();
                        if self.selected.as_ref() == Some(&key) {
                            self.clear_reading_pane();
                        }
                    }
                    Err(err) => self.status = Some(err),
                }
            }
            Msg::SaveAttachment(index) => {
                let Some(key) = self.selected.clone() else {
                    return Task::none();
                };
                let Some(backend) = self.backend_for(&key.account_email) else {
                    return Task::none();
                };
                let folder = key.folder.clone();
                let uid = key.uid;
                return run(
                    move || {
                        backend
                            .save_attachment(&folder, uid, index, None)
                            .map(|p| p.display().to_string())
                    },
                    Msg::Saved,
                );
            }
            Msg::Saved(result) => {
                self.status = Some(match result {
                    Ok(path) => format!("Saved to {path}"),
                    Err(err) => err,
                });
            }
            Msg::Compose => {
                let from = self
                    .active_runtime()
                    .map(|rt| rt.account.email.clone())
                    .unwrap_or_default();
                let open = self.compose.open_new(&from);
                return self.open_compose_with_title(open);
            }
            Msg::Reply => {
                let (Some(key), Some(original)) =
                    (self.selected.as_ref(), self.current_original_message())
                else {
                    return Task::none();
                };
                let from = key.account_email.clone();
                let open = self.compose.open_reply(&from, &original);
                return self.open_compose_with_title(open);
            }
            Msg::ReplyAll => {
                let (Some(key), Some(original)) =
                    (self.selected.as_ref(), self.current_original_message())
                else {
                    return Task::none();
                };
                let from = key.account_email.clone();
                let open = self.compose.open_reply_all(&from, &original);
                return self.open_compose_with_title(open);
            }
            Msg::Forward => {
                let (Some(key), Some(original)) =
                    (self.selected.as_ref(), self.current_original_message())
                else {
                    return Task::none();
                };
                let from = key.account_email.clone();
                let open = self.compose.open_forward(&from, &original);
                return self.open_compose_with_title(open);
            }
            Msg::EditDraft => {
                let (Some(key), Some(original)) =
                    (self.selected.as_ref(), self.current_original_message())
                else {
                    return Task::none();
                };
                let from = key.account_email.clone();
                let folder = key.folder.clone();
                let uid = original.uid;
                let open = self.compose.open_draft(&from, folder, uid, &original);
                return self.open_compose_with_title(open);
            }
            Msg::AutoSaveDrafts => {
                let candidates = self
                    .compose
                    .auto_save_candidates(Duration::from_secs(AUTO_DRAFT_MIN_IDLE_SECS));
                let mut tasks = Vec::new();
                for (window_id, payload, built, existing_draft) in candidates {
                    let backend = self
                        .backend_for(&payload.from)
                        .or_else(|| self.active_runtime().map(|rt| rt.backend.clone()));
                    let Some(backend) = backend else {
                        let t = self
                            .compose
                            .mark_draft_saved(window_id, Err("Not signed in.".to_owned()));
                        tasks.push(t.map(|a| a.map(Msg::ComposeEvent)));
                        continue;
                    };
                    let drafts_folder = backend
                        .find_special_folder(FolderKind::Drafts)
                        .map(|f| f.raw_name)
                        .unwrap_or_else(|| "Drafts".to_owned());
                    let old_uid = existing_draft.map(|(_, uid)| uid);
                    tasks.push(run(
                        move || {
                            backend
                                .save_draft(&built.raw_rfc822, old_uid)
                                .map(|opt_uid| opt_uid.map(|uid| (drafts_folder, uid)))
                        },
                        move |res| Msg::ComposeDraftSaved(window_id, res),
                    ));
                }
                return Task::batch(tasks);
            }
            Msg::ComposeEvent(compose_msg) => {
                let draft_uid = match &compose_msg {
                    ComposeMessage::Send(id) => self.compose.existing_draft_uid(*id),
                    _ => None,
                };
                match self.compose.update(compose_msg) {
                    ComposeAction::None | ComposeAction::Closed(_) => {}
                    ComposeAction::Task(task) => return task.map(|a| a.map(Msg::ComposeEvent)),
                    ComposeAction::Send {
                        window_id,
                        payload,
                        built,
                    } => {
                        let backend = self
                            .backend_for(&payload.from)
                            .or_else(|| self.active_runtime().map(|rt| rt.backend.clone()));
                        let Some(backend) = backend else {
                            let task = self
                                .compose
                                .mark_sent(window_id, Err("Not signed in.".to_owned()));
                            return task.map(|a| a.map(Msg::ComposeEvent));
                        };
                        let recipients: Vec<String> = payload
                            .to
                            .iter()
                            .chain(payload.cc.iter())
                            .chain(payload.bcc.iter())
                            .map(|a| a.address.clone())
                            .collect();
                        let from = payload.from;
                        return run(
                            move || {
                                backend.send_raw_message(
                                    &built.raw_rfc822,
                                    &from,
                                    &recipients,
                                    draft_uid,
                                )
                            },
                            move |res| Msg::ComposeSent(window_id, res),
                        );
                    }
                    ComposeAction::SaveDraft {
                        window_id,
                        payload,
                        built,
                        existing_draft,
                    } => {
                        let backend = self
                            .backend_for(&payload.from)
                            .or_else(|| self.active_runtime().map(|rt| rt.backend.clone()));
                        let Some(backend) = backend else {
                            let task = self
                                .compose
                                .mark_draft_saved(window_id, Err("Not signed in.".to_owned()));
                            return task.map(|a| a.map(Msg::ComposeEvent));
                        };
                        let drafts_folder = backend
                            .find_special_folder(FolderKind::Drafts)
                            .map(|f| f.raw_name)
                            .unwrap_or_else(|| "Drafts".to_owned());
                        let old_uid = existing_draft.map(|(_, uid)| uid);
                        return run(
                            move || {
                                backend
                                    .save_draft(&built.raw_rfc822, old_uid)
                                    .map(|opt_uid| opt_uid.map(|uid| (drafts_folder, uid)))
                            },
                            move |res| Msg::ComposeDraftSaved(window_id, res),
                        );
                    }
                }
            }
            Msg::ComposeSent(window_id, result) => {
                let is_ok = result.is_ok();
                let task = self.compose.mark_sent(window_id, result);
                if is_ok {
                    self.sync_compose_contacts();
                    self.status = Some("Message sent".to_owned());
                }
                return task.map(|a| a.map(Msg::ComposeEvent));
            }
            Msg::ComposeDraftSaved(window_id, result) => {
                let task = self.compose.mark_draft_saved(window_id, result);
                return task.map(|a| a.map(Msg::ComposeEvent));
            }
            Msg::HtmlMail(event) => match event {
                HtmlMailEvent::OpenLink(url) => {
                    if let Err(err) = sanitizer::open_external_url(&url) {
                        self.status = Some(err);
                    }
                }
                HtmlMailEvent::HoverLink(hovered) => {
                    self.body_state.hovered_link = hovered;
                }
                HtmlMailEvent::LoadRemoteImages => {
                    if let Some(key) = self.selected.clone() {
                        return self.trigger_remote_image_load(key);
                    }
                }
                HtmlMailEvent::AlwaysLoadFromSender => {
                    let sender = self.body_state.sender_address.clone();
                    if let Err(err) = self.trusted_senders.trust(&sender) {
                        self.status = Some(err);
                    }
                    if let Some(key) = self.selected.clone() {
                        return self.trigger_remote_image_load(key);
                    }
                }
                HtmlMailEvent::OpenInBrowser => {
                    if let Some(content) = self.body_content.as_ref() {
                        let doc = sanitizer::prepare_browser_html(
                            &content.sanitized_html,
                            &content.inline_images,
                            self.body_state.remote_images_loaded,
                        );
                        if let Err(err) = sanitizer::open_in_browser(&doc) {
                            self.status = Some(err);
                        }
                    }
                }
            },
            Msg::RemoteImagesLoaded(key, results) => {
                if self.selected.as_ref() == Some(&key) {
                    self.body_state.loading_remote_images = false;
                    self.body_state.remote_images_loaded = true;
                    self.body_state.remote_images = results;
                }
            }
            Msg::TaskPanicked(err) => {
                self.busy = false;
                self.status = Some(err);
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Msg> {
        if self.accounts.is_empty() || self.adding_account {
            return self.setup_view();
        }
        let mut main = column![row![
            self.sidebar(),
            divider::vertical::default(),
            self.message_list(),
            divider::vertical::default(),
            self.reading_pane(),
        ]
        .height(Length::Fill)];
        if let Some(footer) = self
            .body_state
            .hovered_link
            .as_deref()
            .or(self.status.as_deref())
        {
            main = main.push(text::caption(footer).apply(container).padding([4, 12]));
        }
        main.height(Length::Fill).into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Msg> {
        let focused = self.core().focused_window().is_some_and(|f| f == id);
        self.compose.view_window(id, focused).map(Msg::ComposeEvent)
    }
}
