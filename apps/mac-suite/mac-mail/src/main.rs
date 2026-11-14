// SPDX-License-Identifier: GPL-3.0-only

//! Three-pane mail client laid out like macOS Mail: mailboxes on the left,
//! the message list in the middle, the selected message on the right.
//!
//! Works with any IMAP/SMTP account. The password lives in the desktop
//! keyring; server settings in `~/.config/mac-mail/account.conf`.

mod account;
mod compose;
mod mail_io;

use account::Account;
use compose::{ComposeAction, ComposeManager, ComposeMessage, EmailAddress, OriginalMessage};
use cosmic::{
    ApplicationExt, Apply, Element, Task, app,
    iced::{Alignment, Event, Length, Subscription, event, window},
    widget::{self, button, column, container, divider, row, scrollable, text},
};
use mail_io::{Opened, Summary};

const APP_ID: &str = "dev.asw.MacMail";
const SIDEBAR_WIDTH: f32 = 220.0;
const LIST_WIDTH: f32 = 340.0;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    let settings = app::Settings::default().size(cosmic::iced::Size::new(1200.0, 760.0));
    app::run::<Mail>(settings, ())
}

/// Sign-in form fields.
#[derive(Default)]
struct Setup {
    email: String,
    password: String,
    imap_host: String,
    smtp_host: String,
}

#[derive(Debug, Clone)]
enum Msg {
    FormEmail(String),
    FormPassword(String),
    FormImap(String),
    FormSmtp(String),
    SignIn,
    SignedIn(Result<Vec<String>, String>),
    SignOut,
    Folders(Result<Vec<String>, String>),
    SelectFolder(String),
    Headers(String, Result<(Vec<Summary>, Vec<EmailAddress>), String>),
    SelectMessage(u32),
    Body(u32, Box<Result<(Opened, Option<OriginalMessage>), String>>),
    SearchText(String),
    RunSearch,
    ClearSearch,
    Delete,
    Archive,
    Moved(u32, Result<(), String>),
    SaveAttachment(usize),
    Saved(Result<String, String>),
    Refresh,
    Compose,
    Reply,
    ReplyAll,
    Forward,
    ComposeEvent(ComposeMessage),
    ComposeSent(window::Id, Result<(), String>),
    ComposeDraftSaved(window::Id, Result<Option<(String, u32)>, String>),
    TaskPanicked(String),
}

struct Mail {
    core: app::Core,
    /// The signed-in account, once its password is known.
    account: Option<Account>,
    password: String,
    setup: Setup,
    folders: Vec<String>,
    folder: String,
    messages: Vec<Summary>,
    selected: Option<u32>,
    body: Option<String>,
    selected_original: Option<OriginalMessage>,
    attachments: Vec<String>,
    /// Text in the search box, and whether the list shows its results.
    search_text: String,
    searching: bool,
    status: Option<String>,
    busy: bool,
    compose: ComposeManager,
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

fn folder_icon(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower == "inbox" {
        "mail-inbox-symbolic"
    } else if lower.contains("sent") {
        "mail-send-symbolic"
    } else if lower.contains("draft") {
        "document-edit-symbolic"
    } else if lower.contains("trash") || lower.contains("deleted") || lower.contains("bin") {
        "user-trash-symbolic"
    } else if lower.contains("junk") || lower.contains("spam") {
        "mail-mark-junk-symbolic"
    } else if lower.contains("archive") || lower.contains("all mail") {
        "mail-archive-symbolic"
    } else {
        "folder-symbolic"
    }
}

impl Mail {
    /// The first folder whose name contains one of `words`.
    fn find_folder(&self, words: &[&str]) -> Option<String> {
        self.folders
            .iter()
            .find(|name| {
                let lower = name.to_lowercase();
                words.iter().any(|word| lower.contains(word))
            })
            .cloned()
    }

    /// Move or delete the open message; the list updates when the server agrees.
    fn move_selected(&mut self, target: Option<String>) -> app::Task<Msg> {
        let (Some(account), Some(uid)) = (self.account.clone(), self.selected) else {
            return Task::none();
        };
        self.busy = true;
        let password = self.password.clone();
        let folder = self.folder.clone();
        run(
            move || mail_io::move_message(&account, &password, &folder, uid, target.as_deref()),
            move |result| Msg::Moved(uid, result),
        )
    }

    fn load_headers(&mut self, folder: &str) -> app::Task<Msg> {
        let Some(account) = self.account.clone() else {
            return Task::none();
        };
        self.busy = true;
        let password = self.password.clone();
        let folder = folder.to_owned();
        let name = folder.clone();
        run(
            move || mail_io::headers_with_contacts(&account, &password, &folder),
            move |result| Msg::Headers(name, result),
        )
    }

    fn current_original_message(&self) -> Option<OriginalMessage> {
        if let Some(orig) = self.selected_original.clone() {
            return Some(orig);
        }
        let summary = self
            .selected
            .and_then(|uid| self.messages.iter().find(|m| m.uid == uid))?;
        Some(OriginalMessage {
            uid: summary.uid,
            message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            from: vec![EmailAddress::new(
                (!summary.from.is_empty()).then(|| summary.from.clone()),
                summary.address.clone(),
            )],
            reply_to: Vec::new(),
            to: Vec::new(),
            cc: Vec::new(),
            subject: summary.subject.clone(),
            date: summary.date.clone(),
            body_text: self.body.clone().unwrap_or_default(),
        })
    }

    fn sidebar(&self) -> Element<'_, Msg> {
        let mut list = column![text::caption("Mailboxes")].spacing(2);
        for name in &self.folders {
            let unread = if *name == self.folder {
                self.messages.iter().filter(|m| m.unread).count()
            } else {
                0
            };
            let mut entry = row![
                widget::icon::from_name(folder_icon(name)).size(16).icon(),
                text::body(name.as_str()).width(Length::Fill),
            ]
            .spacing(8)
            .align_y(Alignment::Center);
            if unread > 0 {
                entry = entry.push(text::caption(unread.to_string()));
            }
            list = list.push(
                button::custom(entry)
                    .class(if *name == self.folder {
                        button::ButtonClass::Suggested
                    } else {
                        button::ButtonClass::Text
                    })
                    .width(Length::Fill)
                    .on_press(Msg::SelectFolder(name.clone())),
            );
        }
        let account = self.account.as_ref().map_or("", |a| a.email.as_str());
        column![
            scrollable(list).height(Length::Fill),
            text::caption(account),
            button::text("Sign Out").on_press(Msg::SignOut),
        ]
        .spacing(8)
        .padding(12)
        .width(Length::Fixed(SIDEBAR_WIDTH))
        .height(Length::Fill)
        .into()
    }

    fn message_list(&self) -> Element<'_, Msg> {
        let mut list = column![].spacing(0);
        for message in &self.messages {
            let from = if message.unread {
                text::heading(message.from.as_str())
            } else {
                text::body(message.from.as_str())
            };
            let item = column![
                row![from.width(Length::Fill), text::caption(message.date.as_str())]
                    .align_y(Alignment::Center),
                text::body(message.subject.as_str()),
            ]
            .spacing(2)
            .padding([8, 12]);
            list = list
                .push(
                    button::custom(item)
                        .class(if self.selected == Some(message.uid) {
                            button::ButtonClass::Suggested
                        } else {
                            button::ButtonClass::Text
                        })
                        .width(Length::Fill)
                        .on_press(Msg::SelectMessage(message.uid)),
                )
                .push(divider::horizontal::default());
        }
        if self.messages.is_empty() && !self.busy {
            list = list.push(text::body("No messages").apply(container).padding(16));
        }

        let header = row![
            text::title3(self.folder.as_str()).width(Length::Fill),
            button::icon(widget::icon::from_name("view-refresh-symbolic")).on_press(Msg::Refresh),
            button::icon(widget::icon::from_name("mail-message-new-symbolic"))
                .on_press(Msg::Compose),
        ]
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

    fn reading_pane(&self) -> Element<'_, Msg> {
        let Some(message) = self
            .selected
            .and_then(|uid| self.messages.iter().find(|m| m.uid == uid))
        else {
            return text::body("No message selected")
                .apply(container)
                .center(Length::Fill)
                .into();
        };
        let body = self.body.as_deref().unwrap_or("Loading…");
        let in_trash = {
            let lower = self.folder.to_lowercase();
            lower.contains("trash") || lower.contains("deleted")
        };
        let mut actions = row![space_fill()].spacing(4).align_y(Alignment::Center);
        actions = actions
            .push(
                button::icon(widget::icon::from_name("mail-reply-sender-symbolic"))
                    .on_press(Msg::Reply),
            )
            .push(
                button::icon(widget::icon::from_name("mail-reply-all-symbolic"))
                    .on_press(Msg::ReplyAll),
            )
            .push(
                button::icon(widget::icon::from_name("mail-forward-symbolic"))
                    .on_press(Msg::Forward),
            );
        if self.find_folder(&["archive", "all mail"]).is_some() {
            actions = actions.push(
                button::icon(widget::icon::from_name("mail-archive-symbolic"))
                    .on_press(Msg::Archive),
            );
        }
        actions = actions.push(
            button::icon(widget::icon::from_name(if in_trash {
                "edit-delete-symbolic"
            } else {
                "user-trash-symbolic"
            }))
            .on_press(Msg::Delete),
        );
        let mut attachments = row![].spacing(8);
        for (index, name) in self.attachments.iter().enumerate() {
            attachments = attachments.push(
                button::standard(name.as_str()).on_press(Msg::SaveAttachment(index)),
            );
        }
        column![
            row![text::title3(message.subject.as_str()).width(Length::Fill), actions]
                .align_y(Alignment::Center),
            row![
                text::body(message.from.as_str()).width(Length::Fill),
                text::caption(message.date.as_str()),
            ],
            attachments,
            divider::horizontal::default(),
            scrollable(text::body(body.to_owned())).height(Length::Fill),
        ]
        .spacing(12)
        .padding(20)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    fn setup_view(&self) -> Element<'_, Msg> {
        let form = column![
            text::title2("Add a mail account"),
            text::caption("Works with any IMAP/SMTP provider. For Gmail, iCloud and Yahoo, use an app password."),
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
            text::body(self.status.clone().unwrap_or_default()),
        ]
        .spacing(12)
        .width(Length::Fixed(420.0));
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
        let mut mail = Self {
            core,
            account: None,
            password: String::new(),
            setup: Setup::default(),
            folders: Vec::new(),
            folder: "INBOX".to_owned(),
            messages: Vec::new(),
            selected: None,
            body: None,
            selected_original: None,
            attachments: Vec::new(),
            search_text: String::new(),
            searching: false,
            status: None,
            busy: false,
            compose: ComposeManager::new(),
        };

        // A saved account with a stored password signs in by itself.
        let saved = account::load().and_then(|account| {
            account::load_password(&account.email).map(|password| (account, password))
        });
        let task = match saved {
            Some((account, password)) => {
                mail.account = Some(account.clone());
                mail.password = password.clone();
                mail.busy = true;
                run(
                    move || mail_io::folders(&account, &password),
                    Msg::Folders,
                )
            }
            None => Task::none(),
        };
        (mail, task)
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Msg> {
        if self.compose.has_window(id) {
            Some(Msg::ComposeEvent(ComposeMessage::WindowClosed(id)))
        } else {
            None
        }
    }

    fn subscription(&self) -> Subscription<Msg> {
        event::listen_with(|event, _, id| {
            let Event::Window(win_event) = event else {
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
        })
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
                let password = self.setup.password.clone();
                self.account = Some(account.clone());
                self.password = password.clone();
                return run(
                    move || mail_io::folders(&account, &password),
                    Msg::SignedIn,
                );
            }
            Msg::SignedIn(result) => match result {
                Ok(folders) => {
                    if let Some(account) = self.account.as_ref() {
                        let stored = account::store_password(&account.email, &self.password)
                            .and_then(|()| account::save(account).map_err(|e| e.to_string()));
                        if let Err(err) = stored {
                            self.status = Some(err);
                        }
                    }
                    self.setup = Setup::default();
                    return self.update(Msg::Folders(Ok(folders)));
                }
                Err(err) => {
                    self.busy = false;
                    self.account = None;
                    self.status = Some(err);
                }
            },
            Msg::SignOut => {
                if let Some(account) = self.account.take() {
                    account::forget(&account.email);
                }
                self.password.clear();
                self.folders.clear();
                self.messages.clear();
                self.selected = None;
                self.body = None;
                self.selected_original = None;
                self.status = None;
            }
            Msg::Folders(result) => match result {
                Ok(folders) => {
                    self.folder = folders
                        .iter()
                        .find(|name| name.eq_ignore_ascii_case("INBOX"))
                        .or(folders.first())
                        .cloned()
                        .unwrap_or_else(|| "INBOX".to_owned());
                    self.folders = folders;
                    let folder = self.folder.clone();
                    return self.load_headers(&folder);
                }
                Err(err) => {
                    self.busy = false;
                    self.account = None;
                    self.status = Some(err);
                }
            },
            Msg::SelectFolder(folder) => {
                self.folder.clone_from(&folder);
                self.messages.clear();
                self.selected = None;
                self.body = None;
                self.selected_original = None;
                return self.load_headers(&folder);
            }
            Msg::Headers(folder, result) => {
                self.busy = false;
                if folder == self.folder {
                    match result {
                        Ok((messages, contacts)) => {
                            self.messages = messages;
                            self.compose.merge_contacts(contacts);
                            self.status = None;
                        }
                        Err(err) => self.status = Some(err),
                    }
                }
            }
            Msg::Refresh => {
                let folder = self.folder.clone();
                return self.load_headers(&folder);
            }
            Msg::SelectMessage(uid) => {
                self.selected = Some(uid);
                self.body = None;
                self.selected_original = None;
                self.attachments.clear();
                if let Some(message) = self.messages.iter_mut().find(|m| m.uid == uid) {
                    message.unread = false;
                }
                let Some(account) = self.account.clone() else {
                    return Task::none();
                };
                let password = self.password.clone();
                let folder = self.folder.clone();
                return run(
                    move || mail_io::body_with_context(&account, &password, &folder, uid),
                    move |result| Msg::Body(uid, Box::new(result)),
                );
            }
            Msg::Body(uid, result) => {
                if self.selected == Some(uid) {
                    match *result {
                        Ok((opened, original)) => {
                            self.body = Some(opened.text);
                            self.attachments = opened.attachments;
                            self.selected_original = original;
                        }
                        Err(err) => self.body = Some(err),
                    }
                }
            }
            Msg::SearchText(value) => self.search_text = value,
            Msg::RunSearch => {
                let query = self.search_text.trim().to_owned();
                if query.is_empty() {
                    return self.update(Msg::ClearSearch);
                }
                let Some(account) = self.account.clone() else {
                    return Task::none();
                };
                self.busy = true;
                self.searching = true;
                let password = self.password.clone();
                let folder = self.folder.clone();
                let name = folder.clone();
                return run(
                    move || mail_io::search(&account, &password, &folder, &query).map(|s| (s, Vec::new())),
                    move |result| Msg::Headers(name, result),
                );
            }
            Msg::ClearSearch => {
                self.search_text.clear();
                self.searching = false;
                let folder = self.folder.clone();
                return self.load_headers(&folder);
            }
            Msg::Delete => {
                // Deleting from the trash is final; otherwise move to the trash.
                let lower = self.folder.to_lowercase();
                let target = if lower.contains("trash") || lower.contains("deleted") {
                    None
                } else {
                    self.find_folder(&["trash", "deleted", "bin"])
                };
                return self.move_selected(target);
            }
            Msg::Archive => {
                let target = self.find_folder(&["archive", "all mail"]);
                if target.is_some() {
                    return self.move_selected(target);
                }
            }
            Msg::Moved(uid, result) => {
                self.busy = false;
                match result {
                    Ok(()) => {
                        self.messages.retain(|m| m.uid != uid);
                        if self.selected == Some(uid) {
                            self.selected = None;
                            self.body = None;
                            self.selected_original = None;
                            self.attachments.clear();
                        }
                    }
                    Err(err) => self.status = Some(err),
                }
            }
            Msg::SaveAttachment(index) => {
                let (Some(account), Some(uid)) = (self.account.clone(), self.selected) else {
                    return Task::none();
                };
                let password = self.password.clone();
                let folder = self.folder.clone();
                return run(
                    move || mail_io::save_attachment(&account, &password, &folder, uid, index),
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
                    .account
                    .as_ref()
                    .map(|a| a.email.clone())
                    .unwrap_or_default();
                let (id, task, title) = self.compose.open_new(&from);
                let title_task = self.set_window_title(title, id);
                return Task::batch([task.map(|a| a.map(Msg::ComposeEvent)), title_task]);
            }
            Msg::Reply => {
                let Some(original) = self.current_original_message() else {
                    return Task::none();
                };
                let from = self
                    .account
                    .as_ref()
                    .map(|a| a.email.clone())
                    .unwrap_or_default();
                let (id, task, title) = self.compose.open_reply(&from, &original);
                let title_task = self.set_window_title(title, id);
                return Task::batch([task.map(|a| a.map(Msg::ComposeEvent)), title_task]);
            }
            Msg::ReplyAll => {
                let Some(original) = self.current_original_message() else {
                    return Task::none();
                };
                let from = self
                    .account
                    .as_ref()
                    .map(|a| a.email.clone())
                    .unwrap_or_default();
                let (id, task, title) = self.compose.open_reply_all(&from, &original);
                let title_task = self.set_window_title(title, id);
                return Task::batch([task.map(|a| a.map(Msg::ComposeEvent)), title_task]);
            }
            Msg::Forward => {
                let Some(original) = self.current_original_message() else {
                    return Task::none();
                };
                let from = self
                    .account
                    .as_ref()
                    .map(|a| a.email.clone())
                    .unwrap_or_default();
                let (id, task, title) = self.compose.open_forward(&from, &original);
                let title_task = self.set_window_title(title, id);
                return Task::batch([task.map(|a| a.map(Msg::ComposeEvent)), title_task]);
            }
            Msg::ComposeEvent(compose_msg) => {
                if let ComposeMessage::CloseRequested(id) = compose_msg
                    && !self.compose.has_window(id)
                {
                    return cosmic::iced::window::close(id);
                }
                match self.compose.update(compose_msg) {
                    ComposeAction::None | ComposeAction::Closed(_) => {}
                    ComposeAction::Task(task) => return task.map(|a| a.map(Msg::ComposeEvent)),
                    ComposeAction::Send {
                        window_id,
                        payload: _,
                        built,
                    } => {
                        let Some(account) = self.account.clone() else {
                            let _ = self
                                .compose
                                .mark_sent(window_id, Err("Not signed in.".to_owned()));
                            return Task::none();
                        };
                        let password = self.password.clone();
                        let sent_folder = compose::find_sent_folder(&self.folders);
                        return run(
                            move || {
                                mail_io::send_built(
                                    &account,
                                    &password,
                                    &built,
                                    sent_folder.as_deref(),
                                )
                            },
                            move |res| Msg::ComposeSent(window_id, res),
                        );
                    }
                    ComposeAction::SaveDraft {
                        window_id,
                        payload: _,
                        built,
                        existing_draft,
                    } => {
                        let Some(account) = self.account.clone() else {
                            self.compose
                                .mark_draft_saved(window_id, Err("Not signed in.".to_owned()));
                            return Task::none();
                        };
                        let Some(drafts_folder) = compose::find_drafts_folder(&self.folders) else {
                            self.compose.mark_draft_saved(
                                window_id,
                                Err("No Drafts folder found on server.".to_owned()),
                            );
                            return Task::none();
                        };
                        let password = self.password.clone();
                        return run(
                            move || {
                                let replace_ref = existing_draft
                                    .as_ref()
                                    .map(|(f, u)| (f.as_str(), *u));
                                mail_io::save_draft(
                                    &account,
                                    &password,
                                    &drafts_folder,
                                    &built.raw_rfc822,
                                    &built.message_id,
                                    replace_ref,
                                )
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
                    self.status = Some("Message sent".to_owned());
                }
                return task.map(|a| a.map(Msg::ComposeEvent));
            }
            Msg::ComposeDraftSaved(window_id, result) => {
                self.compose.mark_draft_saved(window_id, result);
            }
            Msg::TaskPanicked(err) => {
                self.busy = false;
                self.status = Some(err);
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Msg> {
        if self.account.is_none() || self.folders.is_empty() && self.status.is_some() {
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
        if let Some(status) = self.status.as_ref() {
            main = main.push(text::caption(status.as_str()).apply(container).padding([4, 12]));
        }
        main.height(Length::Fill).into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Msg> {
        let focused = self.core().focused_window().is_some_and(|f| f == id);
        self.compose.view_window(id, focused).map(Msg::ComposeEvent)
    }
}
