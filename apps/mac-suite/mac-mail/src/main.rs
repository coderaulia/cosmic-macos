// SPDX-License-Identifier: GPL-3.0-only

//! Three-pane mail client laid out like macOS Mail: mailboxes on the left,
//! the message list in the middle, the selected message on the right.
//!
//! Works with any IMAP/SMTP account. The password lives in the desktop
//! keyring; server settings in `~/.config/mac-mail/account.conf`.

mod account;
mod mail_io;

use account::Account;
use cosmic::{
    Apply, Element, Task, app,
    iced::{Alignment, Length},
    widget::{
        self, button, column, container, divider, row, scrollable, text, text_editor,
    },
};
use mac_mail::{
    html_mail::{self, DecodedImage, HtmlMailEvent, MailBodyState, MailContent},
    sanitizer,
};
use mail_io::{Opened, Summary};
use std::collections::HashMap;

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

struct Compose {
    to: String,
    subject: String,
    body: text_editor::Content<cosmic::Renderer>,
    /// Files to send along, by path.
    attachments: Vec<String>,
    /// Text in the "add attachment" box.
    attach_input: String,
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
    Headers(String, Result<Vec<Summary>, String>),
    SelectMessage(u32),
    Body(u32, Box<Result<Opened, String>>),
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
    ComposeTo(String),
    ComposeSubject(String),
    ComposeBody(text_editor::Action),
    Send,
    Sent(Result<(), String>),
    CancelCompose,
    Reply,
    Forward,
    AttachInput(String),
    AddAttachment,
    RemoveAttachment(usize),
    HtmlMail(HtmlMailEvent),
    RemoteImagesLoaded(u32, HashMap<String, Result<DecodedImage, String>>),
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
    body: Option<MailContent>,
    body_state: MailBodyState,
    attachments: Vec<String>,
    /// Text in the search box, and whether the list shows its results.
    search_text: String,
    searching: bool,
    status: Option<String>,
    busy: bool,
    compose: Option<Compose>,
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
    Msg::Sent(Err(message))
}

/// Empty space that takes up the remaining width.
fn space_fill<'a>() -> widget::Space {
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
            move || mail_io::headers(&account, &password, &folder),
            move |result| Msg::Headers(name, result),
        )
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
        if let Some(compose) = self.compose.as_ref() {
            return column![
                text::title3("New Message"),
                widget::text_input("To", compose.to.as_str()).on_input(Msg::ComposeTo),
                widget::text_input("Subject", compose.subject.as_str())
                    .on_input(Msg::ComposeSubject),
                text_editor::text_editor(&compose.body)
                    .on_action(Msg::ComposeBody)
                    .height(Length::Fill),
                self.attachment_bar(compose),
                row![
                    button::suggested("Send").on_press(Msg::Send),
                    button::text("Cancel").on_press(Msg::CancelCompose),
                ]
                .spacing(8),
            ]
            .spacing(12)
            .padding(20)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }

        let Some(message) = self
            .selected
            .and_then(|uid| self.messages.iter().find(|m| m.uid == uid))
        else {
            return text::body("No message selected")
                .apply(container)
                .center(Length::Fill)
                .into();
        };
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
        let body_view: Element<'_, Msg> = match self.body.as_ref() {
            Some(content) => html_mail::view(content, &self.body_state, Msg::HtmlMail),
            None => text::body("Loading…").into(),
        };
        column![
            row![text::title3(message.subject.as_str()).width(Length::Fill), actions]
                .align_y(Alignment::Center),
            row![
                text::body(message.from.as_str()).width(Length::Fill),
                text::caption(message.date.as_str()),
            ],
            attachments,
            divider::horizontal::default(),
            scrollable(body_view).height(Length::Fill),
        ]
        .spacing(12)
        .padding(20)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    /// The files attached to the message being written, and a box to add more.
    fn attachment_bar<'a>(&self, compose: &'a Compose) -> Element<'a, Msg> {
        let mut files = row![].spacing(8).align_y(Alignment::Center);
        for (index, path) in compose.attachments.iter().enumerate() {
            let name = std::path::Path::new(path)
                .file_name()
                .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
            files = files.push(
                button::standard(format!("{name} ✕")).on_press(Msg::RemoveAttachment(index)),
            );
        }
        column![
            files,
            row![
                widget::text_input("Attach a file: type its path, like ~/Documents/report.pdf", compose.attach_input.as_str())
                    .on_input(Msg::AttachInput)
                    .on_submit(|_| Msg::AddAttachment)
                    .width(Length::Fill),
                button::standard("Attach").on_press(Msg::AddAttachment),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(8)
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
            body_state: MailBodyState::default(),
            attachments: Vec::new(),
            search_text: String::new(),
            searching: false,
            status: None,
            busy: false,
            compose: None,
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
                self.body_state = MailBodyState::default();
                self.compose = None;
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
                self.body_state = MailBodyState::default();
                self.compose = None;
                return self.load_headers(&folder);
            }
            Msg::Headers(folder, result) => {
                self.busy = false;
                if folder == self.folder {
                    match result {
                        Ok(messages) => {
                            self.messages = messages;
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
                self.compose = None;
                self.selected = Some(uid);
                self.body = None;
                self.body_state = MailBodyState::default();
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
                    move || mail_io::body(&account, &password, &folder, uid),
                    move |result| Msg::Body(uid, Box::new(result)),
                );
            }
            Msg::Body(uid, result) => {
                if self.selected == Some(uid) {
                    self.body_state = MailBodyState::default();
                    match *result {
                        Ok(opened) => {
                            self.body = Some(opened.content);
                            self.attachments = opened.attachments;
                        }
                        Err(err) => self.body = Some(MailContent::from_plain_text(&err)),
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
                    move || mail_io::search(&account, &password, &folder, &query),
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
                            self.body_state = MailBodyState::default();
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
                self.compose = Some(Compose {
                    to: String::new(),
                    subject: String::new(),
                    body: text_editor::Content::new(),
                    attachments: Vec::new(),
                    attach_input: String::new(),
                });
            }
            Msg::Reply | Msg::Forward => {
                let reply = matches!(message, Msg::Reply);
                let Some(original) = self
                    .selected
                    .and_then(|uid| self.messages.iter().find(|m| m.uid == uid))
                else {
                    return Task::none();
                };
                // The original text, quoted line by line.
                let quoted: String = self
                    .body
                    .as_ref()
                    .map(|c| c.plain_text.as_str())
                    .unwrap_or_default()
                    .lines()
                    .map(|line| format!("> {line}\n"))
                    .collect();
                let intro = if reply {
                    format!("\n\nOn {}, {} wrote:\n", original.date, original.from)
                } else {
                    format!(
                        "\n\n---------- Forwarded message ----------\nFrom: {}\nDate: {}\nSubject: {}\n\n",
                        original.from, original.date, original.subject
                    )
                };
                let prefix = if reply { "Re: " } else { "Fwd: " };
                let subject = if original.subject.to_lowercase().starts_with(&prefix.to_lowercase()) {
                    original.subject.clone()
                } else {
                    format!("{prefix}{}", original.subject)
                };
                self.compose = Some(Compose {
                    to: if reply { original.address.clone() } else { String::new() },
                    subject,
                    body: text_editor::Content::with_text(&format!("{intro}{quoted}")),
                    attachments: Vec::new(),
                    attach_input: String::new(),
                });
            }
            Msg::AttachInput(value) => {
                if let Some(compose) = self.compose.as_mut() {
                    compose.attach_input = value;
                }
            }
            Msg::AddAttachment => {
                if let Some(compose) = self.compose.as_mut() {
                    let path = compose.attach_input.trim().trim_matches('\'').trim_matches('"');
                    // A leading ~ means the home folder.
                    let path = match (path.strip_prefix("~/"), std::env::var("HOME")) {
                        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
                        _ => path.to_owned(),
                    };
                    if std::path::Path::new(&path).is_file() {
                        compose.attachments.push(path);
                        compose.attach_input.clear();
                        self.status = None;
                    } else {
                        self.status = Some(format!("No such file: {path}"));
                    }
                }
            }
            Msg::RemoveAttachment(index) => {
                if let Some(compose) = self.compose.as_mut()
                    && index < compose.attachments.len()
                {
                    compose.attachments.remove(index);
                }
            }
            Msg::ComposeTo(value) => {
                if let Some(compose) = self.compose.as_mut() {
                    compose.to = value;
                }
            }
            Msg::ComposeSubject(value) => {
                if let Some(compose) = self.compose.as_mut() {
                    compose.subject = value;
                }
            }
            Msg::ComposeBody(action) => {
                if let Some(compose) = self.compose.as_mut() {
                    compose.body.perform(action);
                }
            }
            Msg::CancelCompose => self.compose = None,
            Msg::Send => {
                let (Some(account), Some(compose)) =
                    (self.account.clone(), self.compose.as_ref())
                else {
                    return Task::none();
                };
                let password = self.password.clone();
                let (to, subject, body, attachments) = (
                    compose.to.clone(),
                    compose.subject.clone(),
                    compose.body.text(),
                    compose.attachments.clone(),
                );
                self.busy = true;
                return run(
                    move || {
                        mail_io::send(&account, &password, &to, &subject, &body, &attachments)
                    },
                    Msg::Sent,
                );
            }
            Msg::Sent(result) => {
                self.busy = false;
                match result {
                    Ok(()) => {
                        self.compose = None;
                        self.status = None;
                    }
                    Err(err) => self.status = Some(err),
                }
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
                    if let (Some(uid), Some(content)) = (self.selected, self.body.as_ref())
                        && !self.body_state.loading_remote_images
                        && !content.remote_image_urls.is_empty()
                    {
                        self.body_state.loading_remote_images = true;
                        let urls = content.remote_image_urls.clone();
                        return run(
                            move || html_mail::fetch_remote_images(&urls),
                            move |results| Msg::RemoteImagesLoaded(uid, results),
                        );
                    }
                }
                HtmlMailEvent::OpenInBrowser => {
                    if let Some(content) = self.body.as_ref() {
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
            Msg::RemoteImagesLoaded(uid, results) => {
                if self.selected == Some(uid) {
                    self.body_state.loading_remote_images = false;
                    self.body_state.remote_images_loaded = true;
                    self.body_state.remote_images = results;
                }
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
}
