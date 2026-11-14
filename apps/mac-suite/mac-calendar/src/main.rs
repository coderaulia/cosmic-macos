// SPDX-License-Identifier: GPL-3.0-only

//! Calendar with day, week and month views, laid out like macOS Calendar:
//! a sidebar with calendars and a mini month, a toolbar with the view
//! switcher and navigation, and the selected view filling the rest.
//!
//! Events are not stored yet; this is the shell the event store and CalDAV
//! sync plug into.

use cosmic::{
    Apply, Element, Task, app,
    iced::{Alignment, Length},
    theme,
    widget::{self, button, column, container, divider, grid, row, scrollable, space, text},
};
use jiff::{ToSpan, Zoned, civil::Date};

mod caldav;

const APP_ID: &str = "dev.asw.MacCalendar";
const MONTH_NAMES: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];
const WEEKDAY_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
/// First and last hour shown in the day and week views.
const DAY_HOURS: std::ops::Range<i8> = 0..24;
const HOUR_HEIGHT: f32 = 48.0;
const SIDEBAR_WIDTH: f32 = 220.0;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    let settings = app::Settings::default().size(cosmic::iced::Size::new(1100.0, 720.0));
    app::run::<Calendar>(settings, ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Day,
    Week,
    Month,
}

#[derive(Debug, Clone)]
enum Message {
    SetView(View),
    Previous,
    Next,
    Today,
    Select(Date),
    /// Show the new-event form for the selected day.
    NewEvent,
    FormTitle(String),
    FormStart(String),
    FormEnd(String),
    SaveEvent,
    CancelEvent,
    DeleteEvent(usize),
    /// Sync button: set up the server first time, then sync.
    OpenSync,
    SyncUrl(String),
    SyncUser(String),
    SyncPassword(String),
    SaveSync,
    CancelSync,
    Synced(Result<Vec<Event>, String>),
}

/// Calendar server settings form.
struct SyncForm {
    url: String,
    user: String,
    password: String,
}

/// One event. Times are "HH:MM"; an event without a start time is all-day.
#[derive(Debug, Clone)]
pub struct Event {
    pub date: Date,
    pub start: Option<(i8, i8)>,
    pub end: Option<(i8, i8)>,
    pub title: String,
    /// Identifies the event on the server and here.
    pub uid: String,
    /// The event's URL on the server; empty until it has been uploaded.
    pub href: String,
}

/// A fresh unique id for a new event.
fn new_uid() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{nanos:x}-{:x}@mac-calendar", std::process::id())
}

/// Events removed here that still exist on the server, as their URLs.
fn deleted_path() -> Option<std::path::PathBuf> {
    events_path().map(|p| p.with_file_name("deleted.txt"))
}

fn load_deleted() -> Vec<String> {
    deleted_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn save_deleted(deleted: &[String]) {
    if let Some(path) = deleted_path() {
        let _ = std::fs::write(path, deleted.join("\n"));
    }
}

fn parse_time(value: &str) -> Option<(i8, i8)> {
    let (h, m) = value.trim().split_once(':')?;
    let (h, m): (i8, i8) = (h.parse().ok()?, m.parse().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some((h, m))
}

fn format_time((h, m): (i8, i8)) -> String {
    format!("{h:02}:{m:02}")
}

/// Events live in a tab-separated file:
/// `date<TAB>start<TAB>end<TAB>title`, with empty times for all-day events.
fn events_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share")))?;
    Some(base.join("mac-calendar/events.tsv"))
}

fn load_events() -> Vec<Event> {
    let Some(contents) = events_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(6, '\t');
            let date: Date = fields.next()?.parse().ok()?;
            let start = parse_time(fields.next()?);
            let end = parse_time(fields.next()?);
            let title = fields.next()?.to_owned();
            let uid = fields
                .next()
                .filter(|uid| !uid.is_empty())
                .map_or_else(new_uid, str::to_owned);
            let href = fields.next().unwrap_or_default().to_owned();
            Some(Event { date, start, end, title, uid, href })
        })
        .collect()
}

fn save_events(events: &[Event]) {
    let Some(path) = events_path() else { return };
    let contents: String = events
        .iter()
        .map(|e| {
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                e.date,
                e.start.map(format_time).unwrap_or_default(),
                e.end.map(format_time).unwrap_or_default(),
                e.title.replace(['\t', '\n'], " "),
                e.uid,
                e.href
            )
        })
        .collect();
    let result = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, contents));
    if let Err(err) = result {
        tracing::error!(?err, "Failed to save events");
    }
}

/// The new-event form's fields.
#[derive(Default)]
struct Form {
    title: String,
    start: String,
    end: String,
}

struct Calendar {
    core: app::Core,
    view: View,
    today: Date,
    selected: Date,
    events: Vec<Event>,
    form: Option<Form>,
    deleted: Vec<String>,
    sync_form: Option<SyncForm>,
    syncing: bool,
    status: Option<String>,
}

/// First Sunday on or before `date`.
fn week_start(date: Date) -> Date {
    let back = date.weekday().to_sunday_zero_offset();
    date.checked_sub((back as i64).days()).unwrap_or(date)
}

/// The 42 days (6 weeks) shown by a month grid containing `date`.
fn month_grid_days(date: Date) -> impl Iterator<Item = Date> {
    let first = week_start(date.first_of_month());
    (0..42).filter_map(move |i| first.checked_add((i as i64).days()).ok())
}

fn hour_label(hour: i8) -> String {
    match hour {
        0 => "12 AM".to_owned(),
        12 => "Noon".to_owned(),
        h if h < 12 => format!("{h} AM"),
        h => format!("{} PM", h - 12),
    }
}

impl Calendar {
    fn title(&self) -> String {
        let month = MONTH_NAMES[self.selected.month() as usize - 1];
        match self.view {
            View::Day => format!(
                "{} {}, {}",
                month,
                self.selected.day(),
                self.selected.year()
            ),
            View::Week | View::Month => format!("{} {}", month, self.selected.year()),
        }
    }

    fn step(&self, forward: bool) -> Date {
        let span = match self.view {
            View::Day => 1.day(),
            View::Week => 1.week(),
            View::Month => 1.month(),
        };
        let moved = if forward {
            self.selected.checked_add(span)
        } else {
            self.selected.checked_sub(span)
        };
        moved.unwrap_or(self.selected)
    }

    fn day_button(&self, date: Date, in_month: bool, size: f32) -> Element<'_, Message> {
        let class = if date == self.selected {
            button::ButtonClass::Suggested
        } else if date == self.today {
            button::ButtonClass::Standard
        } else {
            button::ButtonClass::Text
        };
        let label = text::body(date.day().to_string());
        let label: Element<'_, Message> = if in_month {
            label.into()
        } else {
            label.class(theme::Text::Color(cosmic::iced::Color::from_rgba(0.5, 0.5, 0.5, 1.0)))
                .into()
        };
        button::custom(label.apply(container).center(Length::Fill))
            .class(class)
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .on_press(Message::Select(date))
            .into()
    }

    /// Small month in the sidebar, for jumping to a date.
    fn mini_month(&self) -> Element<'_, Message> {
        let mut days = grid().column_spacing(0).row_spacing(0);
        for name in WEEKDAY_NAMES {
            days = days.push(
                text::caption(&name[..1])
                    .apply(container)
                    .center_x(Length::Fixed(28.0)),
            );
        }
        days = days.insert_row();
        for (i, date) in month_grid_days(self.selected).enumerate() {
            if i > 0 && i % 7 == 0 {
                days = days.insert_row();
            }
            let in_month = date.month() == self.selected.month();
            days = days.push(self.day_button(date, in_month, 28.0));
        }

        column![
            text::heading(format!(
                "{} {}",
                MONTH_NAMES[self.selected.month() as usize - 1],
                self.selected.year()
            )),
            days,
        ]
        .spacing(8)
        .into()
    }

    /// Sync with the server on a worker thread.
    fn start_sync(&mut self, server: caldav::Server) -> app::Task<Message> {
        let Some(password) = caldav::load_password(&server.user) else {
            self.status = Some("Enter the server password again.".to_owned());
            self.sync_form = Some(SyncForm {
                url: server.url,
                user: server.user,
                password: String::new(),
            });
            return Task::none();
        };
        self.syncing = true;
        self.status = Some("Syncing…".to_owned());
        let (local, deleted) = (self.events.clone(), self.deleted.clone());
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || {
                caldav::sync(&server, &password, local, deleted)
            })
            .await
            .unwrap_or_else(|err| Err(err.to_string()));
            Message::Synced(result)
        })
    }

    /// The selected day's events, each with a delete button.
    fn day_events(&self) -> Element<'_, Message> {
        let mut list = column![text::caption(format!("Events on {}", self.selected))].spacing(4);
        let mut any = false;
        for (index, event) in self.events.iter().enumerate().filter(|(_, e)| e.date == self.selected) {
            any = true;
            let when = match event.start {
                Some(start) => format_time(start),
                None => "All day".to_owned(),
            };
            list = list.push(
                row![
                    column![text::body(event.title.as_str()), text::caption(when)].width(Length::Fill),
                    button::icon(widget::icon::from_name("edit-delete-symbolic"))
                        .on_press(Message::DeleteEvent(index)),
                ]
                .align_y(Alignment::Center),
            );
        }
        if !any {
            list = list.push(text::caption("No events"));
        }
        column![space::vertical().height(Length::Fixed(16.0)), list].into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        column![
            text::caption("On this computer"),
            text::body("● Personal"),
            self.day_events(),
            space::vertical().height(Length::Fill),
            self.mini_month(),
        ]
        .spacing(8)
        .padding(16)
        .width(Length::Fixed(SIDEBAR_WIDTH))
        .height(Length::Fill)
        .into()
    }

    fn toolbar(&self) -> Element<'_, Message> {
        let view_button = |label: &'static str, view: View| {
            let class = if self.view == view {
                button::ButtonClass::Suggested
            } else {
                button::ButtonClass::Text
            };
            button::text(label).class(class).on_press(Message::SetView(view))
        };
        row![
            text::title3(self.title()),
            space::horizontal().width(Length::Fill),
            row![
                view_button("Day", View::Day),
                view_button("Week", View::Week),
                view_button("Month", View::Month),
            ]
            .spacing(2),
            space::horizontal().width(Length::Fixed(24.0)),
            button::icon(widget::icon::from_name("go-previous-symbolic")).on_press(Message::Previous),
            button::text("Today").on_press(Message::Today),
            button::icon(widget::icon::from_name("go-next-symbolic")).on_press(Message::Next),
            button::icon(widget::icon::from_name("list-add-symbolic")).on_press(Message::NewEvent),
            button::icon(widget::icon::from_name("emblem-synchronizing-symbolic"))
                .on_press_maybe((!self.syncing).then_some(Message::OpenSync)),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .padding([12, 16])
        .into()
    }

    fn month_view(&self) -> Element<'_, Message> {
        let mut header = row![].spacing(0);
        for name in WEEKDAY_NAMES {
            header = header.push(
                text::caption(name)
                    .apply(container)
                    .width(Length::Fill)
                    .align_x(Alignment::End)
                    .padding([4, 8]),
            );
        }

        let mut weeks = column![].spacing(0).height(Length::Fill);
        let days: Vec<Date> = month_grid_days(self.selected).collect();
        for week in days.chunks(7) {
            let mut cells = row![].spacing(0).height(Length::Fill);
            for &date in week {
                // Today's number is bold, like macOS.
                let number = if date == self.today {
                    text::heading(date.day().to_string())
                } else {
                    text::body(date.day().to_string())
                }
                .apply(container)
                .padding([2, 8]);
                let mut cell_content = column![row![space::horizontal().width(Length::Fill), number]]
                    .spacing(2)
                    .height(Length::Fill);
                for event in self.events.iter().filter(|e| e.date == date).take(3) {
                    let label = match event.start {
                        Some(start) => format!("{} {}", format_time(start), event.title),
                        None => event.title.clone(),
                    };
                    cell_content = cell_content.push(
                        text::caption(label)
                            .apply(container)
                            .padding([0, 6])
                            .class(theme::Container::Primary),
                    );
                }
                let cell = button::custom(cell_content)
                .class(if date == self.selected {
                    button::ButtonClass::Standard
                } else {
                    button::ButtonClass::Text
                })
                .width(Length::Fill)
                .height(Length::Fill)
                .on_press(Message::Select(date));
                cells = cells.push(cell);
            }
            weeks = weeks.push(cells);
        }

        column![header, divider::horizontal::default(), weeks]
            .height(Length::Fill)
            .into()
    }

    /// Hour rows for `days` side by side; used by both day and week view.
    fn time_grid(&self, days: Vec<Date>) -> Element<'_, Message> {
        let mut header = row![space::horizontal().width(Length::Fixed(64.0))];
        for &date in &days {
            let label = format!(
                "{} {}",
                WEEKDAY_NAMES[date.weekday().to_sunday_zero_offset() as usize],
                date.day()
            );
            let label = if date == self.today {
                text::heading(label)
            } else {
                text::body(label)
            };
            header = header.push(
                label
                    .apply(container)
                    .center_x(Length::Fill)
                    .padding([6, 0]),
            );
        }

        let mut hours = column![].spacing(0);
        for hour in DAY_HOURS {
            let mut line = row![
                text::caption(hour_label(hour))
                    .apply(container)
                    .width(Length::Fixed(64.0))
                    .align_x(Alignment::End)
                    .padding([0, 8]),
            ]
            .height(Length::Fixed(HOUR_HEIGHT));
            for &date in &days {
                let mut slot = column![divider::horizontal::default()]
                    .spacing(2)
                    .width(Length::Fill)
                    .height(Length::Fill);
                for event in self
                    .events
                    .iter()
                    .filter(|e| e.date == date && e.start.is_some_and(|(h, _)| h == hour))
                {
                    let span = match (event.start, event.end) {
                        (Some(s), Some(e)) => format!("{}–{} {}", format_time(s), format_time(e), event.title),
                        (Some(s), None) => format!("{} {}", format_time(s), event.title),
                        _ => event.title.clone(),
                    };
                    slot = slot.push(
                        text::caption(span)
                            .apply(container)
                            .width(Length::Fill)
                            .padding([2, 6])
                            .class(theme::Container::Primary),
                    );
                }
                line = line.push(slot);
            }
            hours = hours.push(line);
        }

        column![
            header,
            divider::horizontal::default(),
            scrollable(hours).height(Length::Fill),
        ]
        .height(Length::Fill)
        .into()
    }
}

impl cosmic::Application for Calendar {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn init(mut core: app::Core, _flags: ()) -> (Self, app::Task<Message>) {
        core.window.header_title = "Calendar".to_owned();
        let today = Zoned::now().date();
        (
            Self {
                core,
                view: View::Month,
                today,
                selected: today,
                events: load_events(),
                form: None,
                deleted: load_deleted(),
                sync_form: None,
                syncing: false,
                status: None,
            },
            Task::none(),
        )
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::SetView(view) => self.view = view,
            Message::Previous => self.selected = self.step(false),
            Message::Next => self.selected = self.step(true),
            Message::Today => {
                self.today = Zoned::now().date();
                self.selected = self.today;
            }
            Message::Select(date) => self.selected = date,
            Message::NewEvent => {
                self.form = Some(Form {
                    title: String::new(),
                    start: "09:00".to_owned(),
                    end: "10:00".to_owned(),
                });
            }
            Message::FormTitle(value) => {
                if let Some(form) = self.form.as_mut() {
                    form.title = value;
                }
            }
            Message::FormStart(value) => {
                if let Some(form) = self.form.as_mut() {
                    form.start = value;
                }
            }
            Message::FormEnd(value) => {
                if let Some(form) = self.form.as_mut() {
                    form.end = value;
                }
            }
            Message::SaveEvent => {
                if let Some(form) = self.form.take() {
                    let title = form.title.trim();
                    if !title.is_empty() {
                        self.events.push(Event {
                            date: self.selected,
                            start: parse_time(&form.start),
                            end: parse_time(&form.end),
                            title: title.to_owned(),
                            uid: new_uid(),
                            href: String::new(),
                        });
                        self.events.sort_by_key(|e| (e.date, e.start));
                        save_events(&self.events);
                    }
                }
            }
            Message::CancelEvent => self.form = None,
            Message::DeleteEvent(index) => {
                if index < self.events.len() {
                    let removed = self.events.remove(index);
                    save_events(&self.events);
                    // Remember to delete it on the server at the next sync.
                    if !removed.href.is_empty() {
                        self.deleted.push(removed.href);
                        save_deleted(&self.deleted);
                    }
                }
            }
            Message::OpenSync => {
                // Not set up yet: ask for the server; otherwise sync right away.
                match caldav::load_server() {
                    Some(server) if self.sync_form.is_none() => {
                        return self.start_sync(server);
                    }
                    _ => {
                        let server = caldav::load_server().unwrap_or_default();
                        self.sync_form = Some(SyncForm {
                            url: server.url,
                            user: server.user,
                            password: String::new(),
                        });
                    }
                }
            }
            Message::SyncUrl(value) => {
                if let Some(form) = self.sync_form.as_mut() {
                    form.url = value;
                }
            }
            Message::SyncUser(value) => {
                if let Some(form) = self.sync_form.as_mut() {
                    form.user = value;
                }
            }
            Message::SyncPassword(value) => {
                if let Some(form) = self.sync_form.as_mut() {
                    form.password = value;
                }
            }
            Message::CancelSync => self.sync_form = None,
            Message::SaveSync => {
                let Some(form) = self.sync_form.take() else {
                    return Task::none();
                };
                let server = caldav::Server {
                    url: form.url.trim().to_owned(),
                    user: form.user.trim().to_owned(),
                };
                let saved = caldav::store_password(&server.user, &form.password)
                    .and_then(|()| caldav::save_server(&server).map_err(|e| e.to_string()));
                if let Err(err) = saved {
                    self.status = Some(err);
                    return Task::none();
                }
                return self.start_sync(server);
            }
            Message::Synced(result) => {
                self.syncing = false;
                match result {
                    Ok(events) => {
                        self.events = events;
                        self.deleted.clear();
                        save_events(&self.events);
                        save_deleted(&self.deleted);
                        self.status = Some("Synced".to_owned());
                    }
                    Err(err) => self.status = Some(err),
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let content = match self.view {
            View::Month => self.month_view(),
            View::Week => {
                let start = week_start(self.selected);
                let days = (0..7)
                    .filter_map(|i| start.checked_add((i as i64).days()).ok())
                    .collect();
                self.time_grid(days)
            }
            View::Day => self.time_grid(vec![self.selected]),
        };

        let mut main = column![self.toolbar()];
        if let Some(form) = self.form.as_ref() {
            main = main.push(
                row![
                    text::body(format!("New event on {}", self.selected)),
                    widget::text_input("Title", form.title.as_str())
                        .on_input(Message::FormTitle)
                        .on_submit(|_| Message::SaveEvent)
                        .width(Length::Fill),
                    widget::text_input("Start", form.start.as_str())
                        .on_input(Message::FormStart)
                        .width(Length::Fixed(80.0)),
                    widget::text_input("End", form.end.as_str())
                        .on_input(Message::FormEnd)
                        .width(Length::Fixed(80.0)),
                    button::suggested("Add").on_press(Message::SaveEvent),
                    button::text("Cancel").on_press(Message::CancelEvent),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .padding([8, 16]),
            );
        }
        if let Some(form) = self.sync_form.as_ref() {
            main = main.push(
                row![
                    widget::text_input("Calendar URL (CalDAV)", form.url.as_str())
                        .on_input(Message::SyncUrl)
                        .width(Length::Fill),
                    widget::text_input("User", form.user.as_str())
                        .on_input(Message::SyncUser)
                        .width(Length::Fixed(140.0)),
                    widget::text_input("Password", form.password.as_str())
                        .password()
                        .on_input(Message::SyncPassword)
                        .width(Length::Fixed(140.0)),
                    button::suggested("Connect").on_press(Message::SaveSync),
                    button::text("Cancel").on_press(Message::CancelSync),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .padding([8, 16]),
            );
        }
        if let Some(status) = self.status.as_ref() {
            main = main.push(text::caption(status.as_str()).apply(container).padding([2, 16]));
        }
        let main = main
            .push(divider::horizontal::default())
            .push(content)
            .width(Length::Fill)
            .height(Length::Fill);

        row![self.sidebar(), divider::vertical::default(), main]
        .height(Length::Fill)
        .into()
    }
}
