// SPDX-License-Identifier: GPL-3.0-only

//! Three-pane mail client laid out like macOS Mail: mailboxes on the left,
//! the message list in the middle, the selected message on the right.
//!
//! Works with any IMAP/SMTP account. The password lives in the desktop
//! keyring; server settings in `~/.config/mac-mail/account.conf`.

mod account;
#[allow(dead_code)]
mod backend;
#[allow(dead_code)]
mod cache;
#[allow(dead_code)]
mod imap_pool;
mod mail_io;
mod ui_helpers;

use std::collections::{HashMap, HashSet};

use account::Account;
use cosmic::{
    Apply, Element, Task, app,
    iced::{
        Alignment, Background, Border, Color, Event, Length, Subscription, event, keyboard,
    },
    theme,
    widget::{
        self, button, column, container, divider, row, scrollable, text, text_editor,
    },
};
use mail_io::{FolderInfo, Opened, Summary};

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
    SignedIn(Result<Vec<FolderInfo>, String>),
    SignOut,
    Folders(Result<Vec<FolderInfo>, String>),
    SelectFolder(String),
    Headers(String, Result<Vec<Summary>, String>),
    SelectMessage(u32),
    SelectNext,
    SelectPrev,
    Body(u32, Result<Opened, String>),
    SearchText(String),
    RunSearch,
    ClearSearch,
    Delete,
    Archive,
    MarkUnread,
    MarkedUnread(u32, Result<(), String>),
    ToggleRecipients,
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
    ReplyAll,
    Forward,
    AttachInput(String),
    AddAttachment,
    RemoveAttachment(usize),
}

struct Mail {
    core: app::Core,
    /// The signed-in account, once its password is known.
    account: Option<Account>,
    password: String,
    setup: Setup,
    folders: Vec<String>,
    folder_unreads: HashMap<String, usize>,
    folder: String,
    messages: Vec<Summary>,
    selected: Option<u32>,
    body: Option<String>,
    attachments: Vec<String>,
    show_recipients: bool,
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
fn space_fill() -> widget::Space {
    widget::space::horizontal().width(Length::Fill)
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

fn extract_email_addr(formatted: &str) -> String {
    if let (Some(start), Some(end)) = (formatted.rfind('<'), formatted.rfind('>'))
        && start < end
    {
        return formatted[start + 1..end].trim().to_owned();
    }
    formatted.trim().to_owned()
}

impl Mail {
    /// The first folder whose raw or friendly name matches one of `words`.
    fn find_folder(&self, words: &[&str]) -> Option<String> {
        self.folders
            .iter()
            .find(|name| {
                let lower = name.to_lowercase();
                let friendly = ui_helpers::friendly_folder_name(name).to_lowercase();
                words
                    .iter()
                    .any(|word| lower.contains(word) || friendly.contains(word))
            })
            .cloned()
    }

    fn select_message_by_uid(&mut self, uid: u32) -> app::Task<Msg> {
        self.compose = None;
        self.selected = Some(uid);
        self.body = None;
        self.attachments.clear();
        self.show_recipients = false;
        if let Some(message) = self.messages.iter_mut().find(|m| m.uid == uid)
            && message.unread
        {
            message.unread = false;
            if let Some(count) = self.folder_unreads.get_mut(&self.folder) {
                *count = count.saturating_sub(1);
            }
        }
        let Some(account) = self.account.clone() else {
            return Task::none();
        };
        let password = self.password.clone();
        let folder = self.folder.clone();
        run(
            move || mail_io::body(&account, &password, &folder, uid),
            move |result| Msg::Body(uid, result),
        )
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
            let unread = self.folder_unreads.get(name).copied().unwrap_or(0);
            let friendly = ui_helpers::friendly_folder_name(name);
            let is_selected = *name == self.folder;
            let mut entry = row![
                widget::icon::from_name(ui_helpers::folder_icon(name))
                    .size(16)
                    .icon(),
                text::body(friendly).width(Length::Fill),
            ]
            .spacing(8)
            .align_y(Alignment::Center);

            if unread > 0 {
                let badge = text::caption(unread.to_string())
                    .apply(container)
                    .padding([1, 7])
                    .class(theme::Container::custom(move |theme| {
                        let cosmic = theme.cosmic();
                        let mut bg: Color = cosmic.background(theme.transparent).on.into();
                        bg.a = if is_selected { 0.18 } else { 0.10 };
                        widget::container::Style {
                            background: Some(Background::Color(bg)),
                            border: Border {
                                radius: 10.0.into(),
                                ..Default::default()
                            },
                            text_color: Some(cosmic.background(theme.transparent).on.into()),
                            ..Default::default()
                        }
                    }));
                entry = entry.push(badge);
            }

            list = list.push(
                button::custom(entry)
                    .class(list_row_button_class(is_selected))
                    .width(Length::Fill)
                    .padding([6, 8])
                    .on_press(Msg::SelectFolder(name.clone())),
            );
        }
        let account = self.account.as_ref().map_or("", |a| a.email.as_str());
        column![
            scrollable(list).height(Length::Fill),
            text::caption(account).class(muted_text_class()),
            button::text("Sign Out").on_press(Msg::SignOut),
        ]
        .spacing(8)
        .padding(12)
        .width(Length::Fixed(SIDEBAR_WIDTH))
        .height(Length::Fill)
        .into()
    }

    fn message_list(&self) -> Element<'_, Msg> {
        let mut list = column![].spacing(2);
        for message in &self.messages {
            let is_selected = self.selected == Some(message.uid);
            let rel_date = ui_helpers::format_relative_date(message.timestamp, &message.date);

            let unread_dot: Element<'_, Msg> = if message.unread {
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

            let from = if message.unread {
                text::heading(message.from.as_str())
            } else {
                text::body(message.from.as_str())
            };

            let mut top_row = row![from.width(Length::Fill)].spacing(6).align_y(Alignment::Center);
            if message.has_attachments {
                top_row = top_row.push(
                    widget::icon::from_name("mail-attachment-symbolic")
                        .size(13)
                        .icon(),
                );
            }
            top_row = top_row.push(text::caption(rel_date).class(muted_text_class()));

            let subject = if message.unread {
                text::caption_heading(message.subject.as_str())
            } else {
                text::caption(message.subject.as_str())
            };

            let mut content = column![top_row, subject].spacing(2).width(Length::Fill);

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

            list = list
                .push(
                    button::custom(row_body)
                        .class(list_row_button_class(is_selected))
                        .width(Length::Fill)
                        .padding(0)
                        .on_press(Msg::SelectMessage(message.uid)),
                )
                .push(divider::horizontal::default());
        }
        if self.messages.is_empty() && !self.busy {
            list = list.push(text::body("No messages").apply(container).padding(16));
        }

        let folder_title = ui_helpers::friendly_folder_name(&self.folder);
        let header = row![
            text::title3(folder_title).width(Length::Fill),
            button::icon(widget::icon::from_name("view-refresh-symbolic"))
                .tooltip("Refresh")
                .on_press(Msg::Refresh),
            button::icon(widget::icon::from_name("mail-message-new-symbolic"))
                .tooltip("New Message")
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
                .class(muted_text_class())
                .apply(container)
                .center(Length::Fill)
                .into();
        };
        let body = self.body.as_deref().unwrap_or("Loading…");
        let in_trash = {
            let lower = self.folder.to_lowercase();
            lower.contains("trash") || lower.contains("deleted") || lower.contains("bin")
        };

        let mut actions = row![space_fill()].spacing(4).align_y(Alignment::Center);
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
        if self.find_folder(&["archive", "all mail"]).is_some() {
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
        let initials = ui_helpers::sender_initials(&message.from, &message.from_email);
        let [r, g, b] = ui_helpers::avatar_color_rgb(if message.from_email.is_empty() {
            &message.from
        } else {
            &message.from_email
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

        let mut sender_line = row![text::heading(message.from.as_str())].spacing(6).align_y(Alignment::Center);
        if !message.from_email.is_empty() && message.from_email != message.from {
            sender_line = sender_line.push(
                text::caption(format!("<{}>", message.from_email)).class(muted_text_class()),
            );
        }

        let header_date = ui_helpers::format_header_date(message.timestamp, &message.date);
        let toggle_label = if self.show_recipients {
            "Hide details ▴"
        } else {
            "Details ▾"
        };

        let mut sender_col = column![sender_line].spacing(2).width(Length::Fill);
        if !self.show_recipients && !message.to.is_empty() {
            let summary_to = if message.to.len() == 1 {
                format!("To: {}", message.to[0])
            } else {
                format!("To: {} (+{} more)", message.to[0], message.to.len() - 1)
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

        if self.show_recipients {
            let mut details = column![].spacing(4);
            let from_full = if message.from_email.is_empty() {
                message.from.clone()
            } else if message.from == message.from_email {
                message.from_email.clone()
            } else {
                format!("{} <{}>", message.from, message.from_email)
            };
            details = details.push(text::caption(format!("From: {from_full}")));
            if !message.to.is_empty() {
                details = details.push(text::caption(format!("To: {}", message.to.join(", "))));
            }
            if !message.cc.is_empty() {
                details = details.push(text::caption(format!("Cc: {}", message.cc.join(", "))));
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
        for (index, name) in self.attachments.iter().enumerate() {
            attachments = attachments.push(
                button::standard(name.as_str()).on_press(Msg::SaveAttachment(index)),
            );
        }

        let mut content = column![header_block].spacing(12);
        if !self.attachments.is_empty() {
            content = content.push(attachments);
        }
        content = content
            .push(divider::horizontal::default())
            .push(scrollable(text::body(body.to_owned())).height(Length::Fill));

        content
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
            folder_unreads: HashMap::new(),
            folder: "INBOX".to_owned(),
            messages: Vec::new(),
            selected: None,
            body: None,
            attachments: Vec::new(),
            show_recipients: false,
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

    fn subscription(&self) -> Subscription<Msg> {
        if self.account.is_none() || self.compose.is_some() {
            return Subscription::none();
        }
        event::listen_with(|ev, status, _id| {
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
                keyboard::Key::Named(keyboard::key::Named::ArrowDown) => Some(Msg::SelectNext),
                keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some(Msg::SelectPrev),
                keyboard::Key::Named(keyboard::key::Named::Delete) => Some(Msg::Delete),
                keyboard::Key::Character(c) if c.eq_ignore_ascii_case("r") => Some(Msg::Reply),
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
                self.folder_unreads.clear();
                self.messages.clear();
                self.selected = None;
                self.body = None;
                self.show_recipients = false;
                self.compose = None;
                self.status = None;
            }
            Msg::Folders(result) => match result {
                Ok(folder_infos) => {
                    self.folder_unreads.clear();
                    let mut names = Vec::with_capacity(folder_infos.len());
                    for info in folder_infos {
                        self.folder_unreads.insert(info.name.clone(), info.unread);
                        names.push(info.name);
                    }
                    ui_helpers::sort_folders(&mut names);
                    self.folder = names
                        .iter()
                        .find(|name| name.eq_ignore_ascii_case("INBOX"))
                        .or(names.first())
                        .cloned()
                        .unwrap_or_else(|| "INBOX".to_owned());
                    self.folders = names;
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
                self.show_recipients = false;
                self.compose = None;
                return self.load_headers(&folder);
            }
            Msg::Headers(folder, result) => {
                self.busy = false;
                if folder == self.folder {
                    match result {
                        Ok(messages) => {
                            if !self.searching {
                                let count = messages.iter().filter(|m| m.unread).count();
                                self.folder_unreads
                                    .entry(folder)
                                    .and_modify(|existing| {
                                        if messages.len() < 60 {
                                            *existing = count;
                                        }
                                    })
                                    .or_insert(count);
                            }
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
                return self.select_message_by_uid(uid);
            }
            Msg::SelectNext => {
                if self.messages.is_empty() {
                    return Task::none();
                }
                let next_idx = match self
                    .selected
                    .and_then(|uid| self.messages.iter().position(|m| m.uid == uid))
                {
                    Some(idx) if idx + 1 < self.messages.len() => idx + 1,
                    Some(idx) => idx,
                    None => 0,
                };
                let uid = self.messages[next_idx].uid;
                if self.selected != Some(uid) {
                    return self.select_message_by_uid(uid);
                }
            }
            Msg::SelectPrev => {
                if self.messages.is_empty() {
                    return Task::none();
                }
                let prev_idx = match self
                    .selected
                    .and_then(|uid| self.messages.iter().position(|m| m.uid == uid))
                {
                    Some(idx) => idx.saturating_sub(1),
                    None => 0,
                };
                let uid = self.messages[prev_idx].uid;
                if self.selected != Some(uid) {
                    return self.select_message_by_uid(uid);
                }
            }
            Msg::Body(uid, result) => {
                if self.selected == Some(uid) {
                    match result {
                        Ok(opened) => {
                            self.body = Some(opened.text);
                            self.attachments = opened.attachments;
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
            Msg::MarkUnread => {
                let (Some(account), Some(uid)) = (self.account.clone(), self.selected) else {
                    return Task::none();
                };
                if let Some(msg) = self.messages.iter_mut().find(|m| m.uid == uid)
                    && !msg.unread
                {
                    msg.unread = true;
                    *self.folder_unreads.entry(self.folder.clone()).or_insert(0) += 1;
                }
                let password = self.password.clone();
                let folder = self.folder.clone();
                return run(
                    move || mail_io::set_unread(&account, &password, &folder, uid, true),
                    move |result| Msg::MarkedUnread(uid, result),
                );
            }
            Msg::MarkedUnread(_uid, result) => {
                if let Err(err) = result {
                    self.status = Some(err);
                }
            }
            Msg::ToggleRecipients => {
                self.show_recipients = !self.show_recipients;
            }
            Msg::Moved(uid, result) => {
                self.busy = false;
                match result {
                    Ok(()) => {
                        if let Some(msg) = self.messages.iter().find(|m| m.uid == uid)
                            && msg.unread
                            && let Some(count) = self.folder_unreads.get_mut(&self.folder)
                        {
                            *count = count.saturating_sub(1);
                        }
                        self.messages.retain(|m| m.uid != uid);
                        if self.selected == Some(uid) {
                            self.selected = None;
                            self.body = None;
                            self.attachments.clear();
                            self.show_recipients = false;
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
            Msg::Reply | Msg::ReplyAll | Msg::Forward => {
                let is_reply_all = matches!(message, Msg::ReplyAll);
                let reply = matches!(message, Msg::Reply | Msg::ReplyAll);
                let Some(original) = self
                    .selected
                    .and_then(|uid| self.messages.iter().find(|m| m.uid == uid))
                else {
                    return Task::none();
                };
                // The original text, quoted line by line.
                let quoted: String = self
                    .body
                    .as_deref()
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
                let to = if is_reply_all {
                    let my_email = self
                        .account
                        .as_ref()
                        .map(|a| a.email.trim().to_lowercase())
                        .unwrap_or_default();
                    let mut seen = HashSet::new();
                    let mut recipients = Vec::new();
                    for raw in std::iter::once(original.address.as_str())
                        .chain(original.to.iter().map(String::as_str))
                        .chain(original.cc.iter().map(String::as_str))
                    {
                        let addr = extract_email_addr(raw);
                        let lower = addr.to_lowercase();
                        if !addr.is_empty() && lower != my_email && seen.insert(lower) {
                            recipients.push(addr);
                        }
                    }
                    recipients.join(", ")
                } else if reply {
                    original.address.clone()
                } else {
                    String::new()
                };
                self.compose = Some(Compose {
                    to,
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
