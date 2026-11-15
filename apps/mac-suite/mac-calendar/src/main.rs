// SPDX-License-Identifier: GPL-3.0-only

//! Calendar with day, week and month views, laid out like macOS Calendar:
//! a sidebar with calendars and a mini month, a toolbar with search, the
//! view switcher and navigation, the selected view filling the rest, and an
//! inspector for viewing and editing an event.

use std::{collections::HashSet, path::PathBuf, time::Duration};

use cosmic::{
    Apply, Element, Task, app,
    iced::{
        Alignment, Background, Border, Color, Event as UiEvent, Length, Subscription,
        event as iced_event, keyboard, window,
    },
    theme,
    widget::{self, button, column, container, divider, grid, row, scrollable, space, text},
};
use jiff::{ToSpan, Zoned, civil::Date};

mod caldav;
mod event;
mod recurrence;
mod store;

use event::{
    CalendarInfo, Event, PALETTE, calendar_id_for, due_reminders, minutes, palette_color,
    visible_occurrences,
};
use recurrence::{Frequency, Occurrence, RecurrenceRule};

const APP_ID: &str = "dev.asw.MacCalendar";
const MONTH_NAMES: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];
const WEEKDAY_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
/// First and last hour shown in the day and week views.
const DAY_HOURS: std::ops::Range<i8> = 0..24;
const HOUR_HEIGHT: f32 = 48.0;
const SIDEBAR_WIDTH: f32 = 230.0;
const INSPECTOR_WIDTH: f32 = 330.0;
const GUTTER_WIDTH: f32 = 64.0;
/// Events shown per day in the month grid before "+N more".
const MONTH_EVENTS: usize = 3;
const DELETED_FILE: &str = "deleted.txt";
const EDITED_FILE: &str = "edited.txt";
/// Reminder choices of the inspector, in minutes before the start.
const REMINDER_CHOICES: [Option<u32>; 8] =
    [None, Some(0), Some(5), Some(15), Some(30), Some(60), Some(120), Some(1440)];

/// Decode percent-escapes in a `file://` URI path into UTF-8 bytes.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok())
        {
            out.push(hex);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Turn a CLI argument or pasted path (including `file:///...` URIs) into a `PathBuf`.
fn parse_ics_path(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("file://") {
        let path = rest.strip_prefix("localhost").unwrap_or(rest);
        PathBuf::from(percent_decode(path))
    } else {
        PathBuf::from(trimmed)
    }
}

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    // Any `.ics` files named on the command line are imported at startup.
    let files: Vec<PathBuf> = std::env::args_os()
        .skip(1)
        .filter_map(|arg| {
            let s = arg.to_string_lossy();
            (!s.starts_with('-')).then(|| parse_ics_path(&s))
        })
        .collect();
    let settings = app::Settings::default().size(cosmic::iced::Size::new(1180.0, 740.0));
    app::run::<Calendar>(settings, files)
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
    Search(String),
    /// Reminder check and clock update.
    Tick,
    // Calendars.
    ToggleCalendar(usize, bool),
    ActiveCalendar(usize),
    NewCalendarName(String),
    AddCalendar,
    ToggleImport,
    ImportPath(String),
    ImportFile,
    ToggleExport,
    ExportPath(String),
    ExportFile,
    FilesDropped(Vec<PathBuf>),
    // Inspector.
    NewEvent,
    NewEventAt(Date, i8),
    OpenEvent(usize, Date),
    InspTitle(String),
    InspDate(String),
    InspAllDay(bool),
    InspStart(String),
    InspEnd(String),
    InspLocation(String),
    InspNotes(String),
    InspCalendar(usize),
    InspRepeat(usize),
    InspUntil(String),
    InspCount(String),
    InspReminder(usize),
    SaveEvent,
    CloseInspector,
    DeleteEvent,
    // Server sync.
    OpenSync,
    SyncUrl(String),
    SyncUser(String),
    SyncPassword(String),
    SyncGoogleId(String),
    SignInGoogle,
    GoogleSignedIn(Result<(String, String), String>),
    SaveSync,
    CancelSync,
    Synced(Result<Vec<Event>, String>),
    AppMenuActivate(String),
}

/// Calendar server settings form.
struct SyncForm {
    url: String,
    user: String,
    password: String,
    /// Google OAuth client ID; empty uses the password instead.
    google_id: String,
}

/// The event inspector's fields; edits an existing event or makes a new one.
struct Inspector {
    /// Index into the event list, `None` for a new event.
    editing: Option<usize>,
    title: String,
    date: String,
    all_day: bool,
    start: String,
    end: String,
    location: String,
    notes: String,
    /// Index into the calendar list.
    calendar: usize,
    /// Index into `Frequency::ALL`.
    repeat: usize,
    until: String,
    count: String,
    /// Index into the reminder choices (including a custom one, last).
    reminder: usize,
    custom_reminder: Option<u32>,
    error: Option<String>,
}

fn parse_time(value: &str) -> Option<(i8, i8)> {
    let (h, m) = value.trim().split_once(':')?;
    let (h, m): (i8, i8) = (h.parse().ok()?, m.parse().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some((h, m))
}

fn format_time((h, m): (i8, i8)) -> String {
    format!("{h:02}:{m:02}")
}

fn reminder_label(minutes: Option<u32>) -> String {
    match minutes {
        None => "None".to_owned(),
        Some(0) => "At time of event".to_owned(),
        Some(m) if m % 1440 == 0 => format!("{} day(s) before", m / 1440),
        Some(m) if m % 60 == 0 => format!("{} hour(s) before", m / 60),
        Some(m) => format!("{m} minutes before"),
    }
}

impl Inspector {
    fn new_event(date: Date, calendar: usize) -> Self {
        Self::new_event_at(date, calendar, 9)
    }

    fn new_event_at(date: Date, calendar: usize, hour: i8) -> Self {
        let start_hour = hour.clamp(0, 23);
        let end_hour = (start_hour + 1).min(23);
        let end_min = if start_hour == 23 { 59 } else { 0 };
        Self {
            editing: None,
            title: String::new(),
            date: date.to_string(),
            all_day: false,
            start: format_time((start_hour, 0)),
            end: format_time((end_hour, end_min)),
            location: String::new(),
            notes: String::new(),
            calendar,
            repeat: 0,
            until: String::new(),
            count: String::new(),
            reminder: 0,
            custom_reminder: None,
            error: None,
        }
    }

    fn for_event(index: usize, event: &Event, calendars: &[CalendarInfo]) -> Self {
        let custom_reminder = event
            .reminder_minutes
            .filter(|m| !REMINDER_CHOICES.contains(&Some(*m)));
        let reminder = match event.reminder_minutes {
            None => 0,
            Some(m) => REMINDER_CHOICES
                .iter()
                .position(|c| *c == Some(m))
                .unwrap_or(REMINDER_CHOICES.len()),
        };
        Self {
            editing: Some(index),
            title: event.title.clone(),
            date: event.date.to_string(),
            all_day: event.start.is_none(),
            start: event.start.map_or_else(|| "09:00".to_owned(), format_time),
            end: event.end.map_or_else(|| "10:00".to_owned(), format_time),
            location: event.location.clone(),
            notes: event.notes.clone(),
            calendar: calendars
                .iter()
                .position(|c| c.id == event.calendar_id)
                .unwrap_or(0),
            repeat: Frequency::ALL
                .iter()
                .position(|f| *f == event.recurrence.freq)
                .unwrap_or(0),
            until: event.recurrence.until.map(|d| d.to_string()).unwrap_or_default(),
            count: event.recurrence.count.map(|c| c.to_string()).unwrap_or_default(),
            reminder,
            custom_reminder,
            error: None,
        }
    }

    fn reminder_options(&self) -> Vec<Option<u32>> {
        let mut options = REMINDER_CHOICES.to_vec();
        options.extend(self.custom_reminder.map(Some));
        options
    }

    /// The edited event's fields applied to a copy of `base`, or what is wrong.
    fn build(&self, mut base: Event, calendars: &[CalendarInfo]) -> Result<Event, String> {
        let title = self.title.trim();
        if title.is_empty() {
            return Err("Enter a title.".to_owned());
        }
        base.title = title.to_owned();
        base.date = self
            .date
            .trim()
            .parse()
            .map_err(|_| "The date must look like 2026-03-14.".to_owned())?;
        if self.all_day {
            (base.start, base.end) = (None, None);
        } else {
            let start = parse_time(&self.start).ok_or("The start must look like 09:30.")?;
            base.start = Some(start);
            base.end = match self.end.trim() {
                "" => None,
                end => {
                    let end = parse_time(end).ok_or("The end must look like 10:30.")?;
                    if minutes(end) <= minutes(start) {
                        return Err("The event must end after it starts.".to_owned());
                    }
                    Some(end)
                }
            };
        }
        base.location = self.location.trim().to_owned();
        base.notes = self.notes.trim().to_owned();
        if let Some(calendar) = calendars.get(self.calendar) {
            base.calendar_id.clone_from(&calendar.id);
            base.color_idx = calendar.color_idx;
        }
        let freq = Frequency::ALL[self.repeat.min(Frequency::ALL.len() - 1)];
        let until = match self.until.trim() {
            "" => None,
            until => Some(
                until
                    .parse::<Date>()
                    .map_err(|_| "The end date must look like 2026-12-31.".to_owned())?,
            ),
        };
        let count = match self.count.trim() {
            "" => None,
            count => Some(
                count
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or("Repeat count must be a positive number.")?,
            ),
        };
        base.recurrence = if freq == Frequency::None {
            RecurrenceRule::default()
        } else {
            // Keep the interval and weekdays of a rule that stays the same kind.
            let mut rule = if base.recurrence.freq == freq {
                base.recurrence.clone()
            } else {
                RecurrenceRule::of(freq)
            };
            rule.until = until;
            rule.count = count;
            rule
        };
        base.reminder_minutes = self
            .reminder_options()
            .get(self.reminder)
            .copied()
            .flatten()
            .filter(|_| !self.all_day);
        Ok(base)
    }
}

/// First Sunday on or before `date`.
pub fn week_start(date: Date) -> Date {
    let back = date.weekday().to_sunday_zero_offset();
    date.checked_sub(i64::from(back).days()).unwrap_or(date)
}

/// The 42 days (6 weeks) shown by a month grid containing `date`.
fn month_grid_days(date: Date) -> impl Iterator<Item = Date> {
    let first = week_start(date.first_of_month());
    (0..42).filter_map(move |i| first.checked_add(i64::from(i).days()).ok())
}

fn hour_label(hour: i8) -> String {
    match hour {
        0 => "12 AM".to_owned(),
        12 => "Noon".to_owned(),
        h if h < 12 => format!("{h} AM"),
        h => format!("{} PM", h - 12),
    }
}

/// Minutes since midnight an event covers `[from, to)` on its day; an event
/// without an end lasts an hour.
fn span_minutes(event: &Event) -> Option<(i32, i32)> {
    let start = minutes(event.start?);
    let end = event
        .end
        .map(minutes)
        .filter(|end| *end > start)
        .unwrap_or(start + 60);
    Some((start, end.min(24 * 60)))
}

/// A filled rounded rectangle, used for color dots, stripes and chips.
fn colored(color: Color, radius: f32) -> theme::Container<'static> {
    theme::Container::custom(move |_| widget::container::Style {
        background: Some(Background::Color(color)),
        border: Border { radius: radius.into(), ..Default::default() },
        ..Default::default()
    })
}

fn dot(color: Color, size: f32) -> Element<'static, Message> {
    space::horizontal()
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .apply(container)
        .class(colored(color, size / 2.0))
        .into()
}

struct Calendar {
    core: app::Core,
    view: View,
    today: Date,
    now: (i8, i8),
    selected: Date,
    events: Vec<Event>,
    calendars: Vec<CalendarInfo>,
    /// Calendar that new and imported events go to.
    active_calendar: usize,
    search: String,
    inspector: Option<Inspector>,
    new_calendar: String,
    /// Path field of the "Import .ics…" button; `None` while it is closed.
    import_path: Option<String>,
    /// Path field of the "Export .ics…" button; `None` while it is closed.
    export_path: Option<String>,
    deleted: Vec<String>,
    /// Uids of synced events edited since the last sync.
    edited: Vec<String>,
    /// Reminders already shown, so each shows once.
    notified: HashSet<(String, Date)>,
    sync_form: Option<SyncForm>,
    syncing: bool,
    status: Option<String>,
    app_menu: mac_appmenu::AppMenuHandle,
}

impl Calendar {
    fn title(&self) -> String {
        let month = MONTH_NAMES[self.selected.month() as usize - 1];
        match self.view {
            View::Day => format!("{} {}, {}", month, self.selected.day(), self.selected.year()),
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

    fn save_events(&self) {
        if let Some(dir) = store::data_dir()
            && let Err(err) = store::save_events(&dir, &self.events)
        {
            tracing::error!(?err, "Failed to save events");
        }
    }

    fn save_calendars(&self) {
        if let Some(dir) = store::data_dir()
            && let Err(err) = store::save_calendars(&dir, &self.calendars)
        {
            tracing::error!(?err, "Failed to save calendars");
        }
    }

    fn save_sync_lists(&self) {
        if let Some(dir) = store::data_dir() {
            store::save_list(&dir, DELETED_FILE, &self.deleted);
            store::save_list(&dir, EDITED_FILE, &self.edited);
        }
    }

    fn event_color(&self, event: &Event) -> Color {
        let index = self
            .calendars
            .iter()
            .find(|c| c.id == event.calendar_id)
            .map_or(event.color_idx, |c| c.color_idx);
        palette_color(index)
    }

    fn occurrences(&self, from: Date, to: Date) -> Vec<Occurrence> {
        visible_occurrences(&self.events, &self.calendars, &self.search, from, to)
    }

    /// Import events from `.ics` files into the active calendar.
    fn import_files(&mut self, files: &[PathBuf]) {
        let Some(calendar) = self.calendars.get(self.active_calendar).cloned() else {
            return;
        };
        let mut messages = Vec::new();
        let mut added = 0;
        let mut replaced_uids = Vec::new();
        for path in files {
            match store::import_ics_file(path, &calendar) {
                Ok(events) => {
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    for event in &events {
                        if let Some(existing) = self.events.iter().find(|e| e.uid == event.uid)
                            && !existing.href.is_empty()
                        {
                            replaced_uids.push(existing.uid.clone());
                        }
                    }
                    if let Some(first) = events.first()
                        && files.len() == 1
                    {
                        self.selected = first.date;
                    }
                    let count = store::merge_imported(&mut self.events, events);
                    added += count;
                    messages.push(format!("Imported {count} event(s) from {name} into {}.", calendar.name));
                }
                Err(err) => messages.push(err),
            }
        }
        if added > 0 {
            self.inspector = None;
            self.save_events();
            let mut changed_edited = false;
            for uid in replaced_uids {
                if !self.edited.contains(&uid) {
                    self.edited.push(uid);
                    changed_edited = true;
                }
            }
            if changed_edited {
                self.save_sync_lists();
            }
        }
        self.status = Some(messages.join(" "));
    }

    /// Show a desktop notification for every reminder that has come due.
    fn check_reminders(&mut self) {
        let zoned = Zoned::now();
        self.today = zoned.date();
        self.now = (zoned.hour(), zoned.minute());
        for occurrence in due_reminders(&self.events, zoned.datetime()) {
            let event = &self.events[occurrence.index];
            if self.notified.insert((event.uid.clone(), occurrence.date)) {
                notify(event, occurrence.date);
            }
        }
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
            label.class(theme::Text::Color(Color::from_rgba(0.5, 0.5, 0.5, 1.0))).into()
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
            days = days.push(text::caption(&name[..1]).apply(container).center_x(Length::Fixed(28.0)));
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
        if !server.google_client_id.is_empty() {
            self.syncing = true;
            self.status = Some("Syncing…".to_owned());
            let (local, deleted, edited) =
                (self.events.clone(), self.deleted.clone(), self.edited.clone());
            return cosmic::task::future(async move {
                let result = tokio::task::spawn_blocking(move || {
                    let token = caldav::google_access_token(&server)?;
                    caldav::sync(&server, &token, local, deleted, &edited)
                })
                .await
                .unwrap_or_else(|err| Err(err.to_string()));
                Message::Synced(result)
            });
        }
        let Some(password) = caldav::load_password(&server.user) else {
            self.status = Some("Enter the server password again.".to_owned());
            self.sync_form = Some(SyncForm {
                url: server.url,
                user: server.user,
                password: String::new(),
                google_id: server.google_client_id,
            });
            return Task::none();
        };
        self.syncing = true;
        self.status = Some("Syncing…".to_owned());
        let (local, deleted, edited) =
            (self.events.clone(), self.deleted.clone(), self.edited.clone());
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || {
                caldav::sync(&server, &password, local, deleted, &edited)
            })
            .await
            .unwrap_or_else(|err| Err(err.to_string()));
            Message::Synced(result)
        })
    }

    /// The selected day's events; each opens the inspector.
    fn day_events(&self) -> Element<'_, Message> {
        let mut list = column![text::caption(format!("Events on {}", self.selected))].spacing(4);
        let found = self.occurrences(self.selected, self.selected);
        for occurrence in &found {
            let event = &self.events[occurrence.index];
            let when = match event.start {
                Some(start) => format_time(start),
                None => "All day".to_owned(),
            };
            list = list.push(
                button::custom(
                    row![
                        dot(self.event_color(event), 8.0),
                        column![text::body(event.title.as_str()), text::caption(when)]
                            .width(Length::Fill),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .class(button::ButtonClass::Text)
                .width(Length::Fill)
                .on_press(Message::OpenEvent(occurrence.index, occurrence.date)),
            );
        }
        if found.is_empty() {
            list = list.push(text::caption("No events"));
        }
        list.into()
    }

    /// The calendar list with visibility checkboxes and color dots.
    fn calendar_list(&self) -> Element<'_, Message> {
        let mut list = column![text::caption("Calendars")].spacing(4);
        for (index, calendar) in self.calendars.iter().enumerate() {
            let name = if index == self.active_calendar {
                text::heading(calendar.name.as_str())
            } else {
                text::body(calendar.name.as_str())
            };
            list = list.push(
                row![
                    widget::checkbox(calendar.visible)
                        .on_toggle(move |on| Message::ToggleCalendar(index, on)),
                    dot(palette_color(calendar.color_idx), 10.0),
                    button::custom(name)
                        .class(button::ButtonClass::Text)
                        .width(Length::Fill)
                        .padding([2, 4])
                        .on_press(Message::ActiveCalendar(index)),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        }
        list = list.push(
            row![
                widget::text_input("New calendar", self.new_calendar.as_str())
                    .on_input(Message::NewCalendarName)
                    .on_submit(|_| Message::AddCalendar)
                    .width(Length::Fill),
                button::icon(widget::icon::from_name("list-add-symbolic"))
                    .on_press_maybe((!self.new_calendar.trim().is_empty()).then_some(Message::AddCalendar)),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        );
        list = list.push(
            row![
                button::standard("Import .ics…")
                    .width(Length::Fill)
                    .on_press(Message::ToggleImport),
                button::standard("Export .ics…")
                    .width(Length::Fill)
                    .on_press(Message::ToggleExport),
            ]
            .spacing(6),
        );
        if let Some(path) = self.import_path.as_ref() {
            list = list.push(
                widget::text_input("Path to a .ics file", path.as_str())
                    .on_input(Message::ImportPath)
                    .on_submit(|_| Message::ImportFile),
            );
            list = list.push(
                text::caption(format!(
                    "Imports into {} (you can also drop a file on the window)",
                    self.calendars.get(self.active_calendar).map_or("", |c| c.name.as_str())
                )),
            );
            list = list.push(
                button::suggested("Import")
                    .on_press_maybe((!path.trim().is_empty()).then_some(Message::ImportFile)),
            );
        }
        if let Some(path) = self.export_path.as_ref() {
            list = list.push(
                widget::text_input("Export .ics path", path.as_str())
                    .on_input(Message::ExportPath)
                    .on_submit(|_| Message::ExportFile),
            );
            list = list.push(
                text::caption(format!(
                    "Exports {} events",
                    self.calendars.get(self.active_calendar).map_or("", |c| c.name.as_str())
                )),
            );
            list = list.push(
                button::suggested("Export")
                    .on_press_maybe((!path.trim().is_empty()).then_some(Message::ExportFile)),
            );
        }
        list.into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let top = column![self.calendar_list(), space::vertical().height(Length::Fixed(8.0)), self.day_events()]
            .spacing(8);
        column![
            scrollable(top).height(Length::Fill),
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
            widget::search_input("Search", self.search.as_str())
                .on_input(Message::Search)
                .on_clear(Message::Search(String::new()))
                .width(Length::Fixed(200.0)),
            row![
                view_button("Day", View::Day),
                view_button("Week", View::Week),
                view_button("Month", View::Month),
            ]
            .spacing(2),
            space::horizontal().width(Length::Fixed(8.0)),
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

    /// A day number; today's is a red disc like macOS.
    fn day_number(&self, date: Date) -> Element<'static, Message> {
        let number = text::body(date.day().to_string());
        if date == self.today {
            number
                .class(theme::Text::Color(Color::WHITE))
                .apply(container)
                .padding([1, 7])
                .class(colored(palette_color(3), 11.0))
                .into()
        } else {
            number.apply(container).padding([1, 7]).into()
        }
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

        let days: Vec<Date> = month_grid_days(self.selected).collect();
        let (first, last) = (days[0], days[days.len() - 1]);
        let found = self.occurrences(first, last);
        let mut weeks = column![].spacing(0).height(Length::Fill);
        for week in days.chunks(7) {
            let mut cells = row![].spacing(0).height(Length::Fill);
            for &date in week {
                let mut cell_content = column![
                    row![space::horizontal().width(Length::Fill), self.day_number(date)]
                ]
                .spacing(2)
                .height(Length::Fill);
                let on_day: Vec<&Occurrence> = found.iter().filter(|o| o.date == date).collect();
                for occurrence in on_day.iter().take(MONTH_EVENTS) {
                    cell_content = cell_content.push(self.month_chip(occurrence));
                }
                if on_day.len() > MONTH_EVENTS {
                    cell_content = cell_content.push(
                        text::caption(format!("{} more…", on_day.len() - MONTH_EVENTS))
                            .apply(container)
                            .padding([0, 6]),
                    );
                }
                let selected = date == self.selected;
                let in_month = date.month() == self.selected.month();
                let cell = cell_content
                    .apply(container)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding(2)
                    .class(theme::Container::custom(move |theme| {
                        let cosmic = theme.cosmic();
                        let mut line: Color = cosmic.background(theme.transparent).on.into();
                        line.a = 0.12;
                        let background = if selected {
                            let mut accent: Color = cosmic.accent.base.into();
                            accent.a = 0.14;
                            Some(Background::Color(accent))
                        } else if !in_month {
                            let mut dim: Color = cosmic.background(theme.transparent).on.into();
                            dim.a = 0.04;
                            Some(Background::Color(dim))
                        } else {
                            None
                        };
                        widget::container::Style {
                            background,
                            border: Border { color: line, width: 0.5, ..Default::default() },
                            ..Default::default()
                        }
                    }));
                cells = cells.push(
                    widget::mouse_area(cell)
                        .on_press(Message::Select(date))
                        .on_double_click(Message::NewEvent),
                );
            }
            weeks = weeks.push(cells);
        }

        column![header, divider::horizontal::default(), weeks]
            .height(Length::Fill)
            .into()
    }

    /// One event in a month cell: color dot and title.
    fn month_chip(&self, occurrence: &Occurrence) -> Element<'_, Message> {
        let event = &self.events[occurrence.index];
        let label = match event.start {
            Some(start) => format!("{} {}", format_time(start), event.title),
            None => event.title.clone(),
        };
        button::custom(
            row![dot(self.event_color(event), 7.0), text::caption(label)]
                .spacing(4)
                .align_y(Alignment::Center),
        )
        .class(button::ButtonClass::Text)
        .padding([1, 4])
        .width(Length::Fill)
        .on_press(Message::OpenEvent(occurrence.index, occurrence.date))
        .into()
    }

    /// An event inside one hour slot of the day and week views. A long event
    /// shows its details in its first hour and a colored continuation after.
    fn slot_chip(&self, occurrence: &Occurrence, first: bool) -> Element<'_, Message> {
        let event = &self.events[occurrence.index];
        let color = self.event_color(event);
        let mut tint = color;
        tint.a = 0.22;
        let mut lines = column![].spacing(0);
        if first {
            let when = match (event.start, event.end) {
                (Some(s), Some(e)) => format!("{}–{}", format_time(s), format_time(e)),
                (Some(s), None) => format_time(s),
                _ => String::new(),
            };
            lines = lines.push(text::caption(format!("{} {}", when, event.title)));
            if !event.location.is_empty() {
                lines = lines.push(text::caption(event.location.as_str()).class(theme::Text::Color(
                    Color::from_rgba(0.5, 0.5, 0.5, 1.0),
                )));
            }
        } else {
            lines = lines.push(text::caption(event.title.as_str()));
        }
        let body = row![
            space::horizontal()
                .width(Length::Fixed(3.0))
                .height(Length::Fill)
                .apply(container)
                .class(colored(color, 1.5)),
            lines.padding([2, 6]),
        ];
        let tinted = body
            .apply(container)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(colored(tint, 4.0));
        button::custom(tinted)
            .class(button::ButtonClass::Text)
            .padding(0)
            .width(Length::Fill)
            .height(Length::Fill)
            .on_press(Message::OpenEvent(occurrence.index, occurrence.date))
            .into()
    }

    /// The red "now" row of today's column: a time badge and a line.
    fn now_indicator(&self) -> Element<'static, Message> {
        let red = palette_color(3);
        row![
            text::caption(format_time(self.now))
                .class(theme::Text::Color(Color::WHITE))
                .apply(container)
                .padding([0, 5])
                .class(colored(red, 6.0)),
            space::horizontal()
                .width(Length::Fill)
                .height(Length::Fixed(2.0))
                .apply(container)
                .class(colored(red, 1.0)),
        ]
        .align_y(Alignment::Center)
        .into()
    }

    /// Hour rows for `days` side by side; used by both day and week view.
    fn time_grid(&self, days: Vec<Date>) -> Element<'_, Message> {
        let found: Vec<Vec<Occurrence>> =
            days.iter().map(|&date| self.occurrences(date, date)).collect();

        let mut header = row![space::horizontal().width(Length::Fixed(GUTTER_WIDTH))];
        let mut all_day = row![
            text::caption("all-day")
                .apply(container)
                .width(Length::Fixed(GUTTER_WIDTH))
                .align_x(Alignment::End)
                .padding([0, 8]),
        ];
        let mut any_all_day = false;
        for (&date, on_day) in days.iter().zip(&found) {
            let label = format!(
                "{} {}",
                WEEKDAY_NAMES[date.weekday().to_sunday_zero_offset() as usize],
                date.day()
            );
            let label: Element<'_, Message> = if date == self.today {
                text::heading(label).class(theme::Text::Color(palette_color(3))).into()
            } else {
                text::body(label).into()
            };
            header = header.push(
                widget::mouse_area(label.apply(container).center_x(Length::Fill).padding([6, 0]))
                    .on_press(Message::Select(date)),
            );
            let mut chips = column![].spacing(2).width(Length::Fill);
            for occurrence in on_day.iter().filter(|o| self.events[o.index].start.is_none()) {
                any_all_day = true;
                chips = chips.push(self.month_chip(occurrence));
            }
            all_day = all_day.push(chips);
        }

        let mut hours = column![].spacing(0);
        for hour in DAY_HOURS {
            let (slot_from, slot_to) = (i32::from(hour) * 60, (i32::from(hour) + 1) * 60);
            let mut line = row![
                text::caption(hour_label(hour))
                    .apply(container)
                    .width(Length::Fixed(GUTTER_WIDTH))
                    .align_x(Alignment::End)
                    .padding([0, 8]),
            ]
            .height(Length::Fixed(HOUR_HEIGHT));
            for (&date, on_day) in days.iter().zip(&found) {
                let mut slot = column![divider::horizontal::default()]
                    .spacing(2)
                    .width(Length::Fill)
                    .height(Length::Fill);
                if date == self.today && self.now.0 == hour {
                    slot = slot.push(self.now_indicator());
                }
                let mut chips = row![].spacing(2).width(Length::Fill).height(Length::Fill);
                let mut any = false;
                for occurrence in on_day {
                    let Some((from, to)) = span_minutes(&self.events[occurrence.index]) else {
                        continue;
                    };
                    if from < slot_to && to > slot_from {
                        any = true;
                        chips = chips.push(self.slot_chip(occurrence, from >= slot_from));
                    }
                }
                if any {
                    slot = slot.push(chips);
                }
                line = line.push(
                    widget::mouse_area(slot)
                        .on_press(Message::Select(date))
                        .on_double_click(Message::NewEventAt(date, hour)),
                );
            }
            hours = hours.push(line);
        }

        let mut grid = column![header, divider::horizontal::default()];
        if any_all_day {
            grid = grid.push(all_day).push(divider::horizontal::default());
        }
        grid.push(scrollable(hours).height(Length::Fill))
            .height(Length::Fill)
            .into()
    }

    /// Details and editing form of one event, shown beside the calendar.
    fn inspector_view<'a>(&'a self, form: &'a Inspector) -> Element<'a, Message> {
        let heading = if form.editing.is_some() { "Event" } else { "New Event" };
        let mut fields = column![
            text::title4(heading),
            widget::text_input("Title", form.title.as_str())
                .on_input(Message::InspTitle)
                .on_submit(|_| Message::SaveEvent),
            widget::text_input("Location", form.location.as_str()).on_input(Message::InspLocation),
            widget::text_input("Date (YYYY-MM-DD)", form.date.as_str()).on_input(Message::InspDate),
            widget::toggler(form.all_day)
                .label("All-day".to_owned())
                .on_toggle(Message::InspAllDay),
        ]
        .spacing(10);
        if !form.all_day {
            fields = fields.push(
                row![
                    widget::text_input("Start", form.start.as_str())
                        .on_input(Message::InspStart)
                        .width(Length::Fill),
                    text::body("to"),
                    widget::text_input("End", form.end.as_str())
                        .on_input(Message::InspEnd)
                        .width(Length::Fill),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
        }
        let names: Vec<String> = self.calendars.iter().map(|c| c.name.clone()).collect();
        let repeats: Vec<String> = Frequency::ALL.iter().map(|f| f.label().to_owned()).collect();
        let reminders: Vec<String> = form
            .reminder_options()
            .into_iter()
            .map(reminder_label)
            .collect();
        fields = fields
            .push(
                row![
                    text::body("Calendar").width(Length::Fixed(80.0)),
                    widget::dropdown(names, Some(form.calendar), Message::InspCalendar),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .push(
                row![
                    text::body("Repeat").width(Length::Fixed(80.0)),
                    widget::dropdown(repeats, Some(form.repeat), Message::InspRepeat),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
        if form.repeat != 0 {
            fields = fields
                .push(
                    widget::text_input("Repeat until (YYYY-MM-DD, optional)", form.until.as_str())
                        .on_input(Message::InspUntil),
                )
                .push(
                    widget::text_input("Repeat count (optional)", form.count.as_str())
                        .on_input(Message::InspCount),
                );
            if form.editing.is_some() {
                fields = fields.push(text::caption("Changes apply to every occurrence."));
            }
        }
        if !form.all_day {
            fields = fields.push(
                row![
                    text::body("Alert").width(Length::Fixed(80.0)),
                    widget::dropdown(reminders, Some(form.reminder), Message::InspReminder),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
        }
        fields = fields.push(
            widget::text_input("Notes", form.notes.as_str()).on_input(Message::InspNotes),
        );
        if let Some(error) = form.error.as_ref() {
            fields = fields.push(
                text::caption(error.as_str()).class(theme::Text::Color(palette_color(3))),
            );
        }
        let mut buttons = row![].spacing(8);
        if form.editing.is_some() {
            buttons = buttons.push(button::destructive("Delete").on_press(Message::DeleteEvent));
        }
        buttons = buttons
            .push(space::horizontal().width(Length::Fill))
            .push(button::text("Cancel").on_press(Message::CloseInspector))
            .push(button::suggested(if form.editing.is_some() { "Save" } else { "Add" }).on_press(Message::SaveEvent));
        fields = fields.push(buttons);

        scrollable(fields.padding(16))
            .width(Length::Fixed(INSPECTOR_WIDTH))
            .height(Length::Fill)
            .into()
    }

    fn open_event(&mut self, index: usize, date: Date) {
        if let Some(event) = self.events.get(index) {
            self.selected = date;
            self.inspector = Some(Inspector::for_event(index, event, &self.calendars));
        }
    }

    fn save_inspector(&mut self) {
        let Some(form) = self.inspector.as_mut() else {
            return;
        };
        let base = match form.editing.and_then(|i| self.events.get(i)) {
            Some(existing) => existing.clone(),
            None => Event::new(self.selected, ""),
        };
        let built = form.build(base, &self.calendars);
        let event = match built {
            Ok(event) => event,
            Err(err) => {
                form.error = Some(err);
                return;
            }
        };
        let editing = form.editing;
        self.inspector = None;
        self.selected = event.date;
        match editing {
            Some(index) if index < self.events.len() => {
                if !event.href.is_empty() && !self.edited.contains(&event.uid) {
                    self.edited.push(event.uid.clone());
                    self.save_sync_lists();
                }
                self.events[index] = event;
            }
            _ => self.events.push(event),
        }
        self.events.sort_by_key(|e| (e.date, e.start));
        self.save_events();
    }

    fn delete_inspected(&mut self) {
        let Some(index) = self.inspector.take().and_then(|form| form.editing) else {
            return;
        };
        if index < self.events.len() {
            let removed = self.events.remove(index);
            self.save_events();
            // Remember to delete it on the server at the next sync.
            if !removed.href.is_empty() {
                self.deleted.push(removed.href);
            }
            self.edited.retain(|uid| *uid != removed.uid);
            self.save_sync_lists();
        }
    }

    fn inspector_mut(&mut self) -> Option<&mut Inspector> {
        self.inspector.as_mut()
    }
}

/// Desktop notification through `notify-send`; failures are not fatal.
fn notify(event: &Event, date: Date) {
    let when = event.start.map(format_time).unwrap_or_default();
    let mut body = format!("{date} {when}");
    if !event.location.is_empty() {
        body.push_str(&format!("\n{}", event.location));
    }
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=Calendar", "--icon=appointment-soon", event.title.as_str(), &body])
        .spawn();
}

impl cosmic::Application for Calendar {
    type Executor = cosmic::executor::Default;
    type Flags = Vec<PathBuf>;
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn init(mut core: app::Core, files: Vec<PathBuf>) -> (Self, app::Task<Message>) {
        core.window.header_title = "Calendar".to_owned();
        let zoned = Zoned::now();
        let dir = store::data_dir();
        let calendars = dir.as_deref().map_or_else(event::default_calendars, store::load_calendars);
        let mut events = dir.as_deref().map(store::load_events).unwrap_or_default();
        store::assign_unknown_calendars(&mut events, &calendars);
        events.sort_by_key(|e| (e.date, e.start));
        let mut calendar = Self {
            core,
            view: View::Month,
            today: zoned.date(),
            now: (zoned.hour(), zoned.minute()),
            selected: zoned.date(),
            events,
            calendars,
            active_calendar: 0,
            search: String::new(),
            inspector: None,
            new_calendar: String::new(),
            import_path: None,
            export_path: None,
            deleted: dir.as_deref().map(|d| store::load_list(d, DELETED_FILE)).unwrap_or_default(),
            edited: dir.as_deref().map(|d| store::load_list(d, EDITED_FILE)).unwrap_or_default(),
            notified: HashSet::new(),
            sync_form: None,
            syncing: false,
            status: None,
            app_menu: mac_appmenu::AppMenuHandle::new(APP_ID, Vec::new()),
        };
        if !files.is_empty() {
            calendar.import_files(&files);
        }
        calendar.check_reminders();
        calendar.sync_app_menu();
        (calendar, Task::none())
    }

    fn subscription(&self) -> Subscription<Message> {
        let keys = iced_event::listen_with(|ev, status, _id| match ev {
            UiEvent::Window(window::Event::FileDropped(paths)) => {
                Some(Message::FilesDropped(paths))
            }
            UiEvent::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                if status == iced_event::Status::Ignored
                    && !modifiers.control()
                    && !modifiers.alt()
                    && !modifiers.logo() =>
            {
                match key {
                    keyboard::Key::Named(keyboard::key::Named::ArrowLeft) => Some(Message::Previous),
                    keyboard::Key::Named(keyboard::key::Named::ArrowRight) => Some(Message::Next),
                    keyboard::Key::Named(keyboard::key::Named::Escape) => {
                        Some(Message::CloseInspector)
                    }
                    keyboard::Key::Character(c) if c.eq_ignore_ascii_case("t") => {
                        Some(Message::Today)
                    }
                    keyboard::Key::Character(c) if c.eq_ignore_ascii_case("n") => {
                        Some(Message::NewEvent)
                    }
                    _ => None,
                }
            }
            _ => None,
        });
        let clock = cosmic::iced::time::every(Duration::from_secs(30)).map(|_| Message::Tick);
        let app_menu =
            Subscription::run_with(self.app_menu.clone(), mac_appmenu::activate_stream)
                .map(Message::AppMenuActivate);
        Subscription::batch([keys, clock, app_menu])
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        let task = self.update_inner(message);
        self.sync_app_menu();
        task
    }

    fn view(&self) -> Element<'_, Message> {
        let content = match self.view {
            View::Month => self.month_view(),
            View::Week => {
                let start = week_start(self.selected);
                let days = (0..7)
                    .filter_map(|i| start.checked_add(i64::from(i).days()).ok())
                    .collect();
                self.time_grid(days)
            }
            View::Day => self.time_grid(vec![self.selected]),
        };

        let mut main = column![self.toolbar()];
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
                    widget::text_input("Google client ID", form.google_id.as_str())
                        .on_input(Message::SyncGoogleId)
                        .width(Length::Fixed(160.0)),
                    button::standard("Sign in with Google").on_press(Message::SignInGoogle),
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

        let mut layout = row![self.sidebar(), divider::vertical::default(), main];
        if let Some(form) = self.inspector.as_ref() {
            layout = layout.push(divider::vertical::default()).push(self.inspector_view(form));
        }
        layout.height(Length::Fill).into()
    }
}

impl Calendar {
    fn update_inner(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::SetView(view) => self.view = view,
            Message::Previous => self.selected = self.step(false),
            Message::Next => self.selected = self.step(true),
            Message::Today => {
                self.today = Zoned::now().date();
                self.selected = self.today;
            }
            Message::Select(date) => self.selected = date,
            Message::Search(query) => self.search = query,
            Message::Tick => self.check_reminders(),
            Message::ToggleCalendar(index, visible) => {
                if let Some(calendar) = self.calendars.get_mut(index) {
                    calendar.visible = visible;
                    self.save_calendars();
                }
            }
            Message::ActiveCalendar(index) => {
                if index < self.calendars.len() {
                    self.active_calendar = index;
                }
            }
            Message::NewCalendarName(name) => self.new_calendar = name,
            Message::AddCalendar => {
                let name = self.new_calendar.trim().to_owned();
                if !name.is_empty() {
                    let color_idx = (self.calendars.len() % PALETTE.len()) as u8;
                    self.calendars.push(CalendarInfo {
                        id: calendar_id_for(&name, &self.calendars),
                        name,
                        color_idx,
                        visible: true,
                    });
                    self.active_calendar = self.calendars.len() - 1;
                    self.new_calendar.clear();
                    self.save_calendars();
                }
            }
            Message::ToggleImport => {
                self.export_path = None;
                self.import_path = match self.import_path {
                    Some(_) => None,
                    None => Some(String::new()),
                };
            }
            Message::ImportPath(path) => self.import_path = Some(path),
            Message::ImportFile => {
                if let Some(path) = self.import_path.take() {
                    self.import_files(&[parse_ics_path(&path)]);
                }
            }
            Message::ToggleExport => {
                self.import_path = None;
                self.export_path = match self.export_path {
                    Some(_) => None,
                    None => {
                        let name = self
                            .calendars
                            .get(self.active_calendar)
                            .map_or("calendar", |c| c.id.as_str());
                        let base = std::env::var_os("HOME")
                            .map(PathBuf::from)
                            .unwrap_or_else(|| PathBuf::from("."));
                        Some(base.join(format!("{name}.ics")).display().to_string())
                    }
                };
            }
            Message::ExportPath(path) => self.export_path = Some(path),
            Message::ExportFile => {
                if let Some(path) = self.export_path.take() {
                    let path = path.trim();
                    if let Some(calendar) = self.calendars.get(self.active_calendar) {
                        let subset: Vec<Event> = self
                            .events
                            .iter()
                            .filter(|e| e.calendar_id == calendar.id)
                            .cloned()
                            .collect();
                        match store::export_ics_file(&PathBuf::from(path), &subset) {
                            Ok(count) => {
                                self.status = Some(format!(
                                    "Exported {count} event(s) from {} to {path}.",
                                    calendar.name
                                ));
                            }
                            Err(err) => self.status = Some(err),
                        }
                    }
                }
            }
            Message::FilesDropped(paths) => {
                let files: Vec<PathBuf> = paths
                    .into_iter()
                    .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ics")))
                    .collect();
                self.import_files(&files);
            }
            Message::NewEvent => {
                self.inspector = Some(Inspector::new_event(self.selected, self.active_calendar));
            }
            Message::NewEventAt(date, hour) => {
                self.selected = date;
                self.inspector =
                    Some(Inspector::new_event_at(date, self.active_calendar, hour));
            }
            Message::OpenEvent(index, date) => self.open_event(index, date),
            Message::InspTitle(value) => {
                if let Some(form) = self.inspector_mut() {
                    form.title = value;
                }
            }
            Message::InspDate(value) => {
                if let Some(form) = self.inspector_mut() {
                    form.date = value;
                }
            }
            Message::InspAllDay(value) => {
                if let Some(form) = self.inspector_mut() {
                    form.all_day = value;
                }
            }
            Message::InspStart(value) => {
                if let Some(form) = self.inspector_mut() {
                    form.start = value;
                }
            }
            Message::InspEnd(value) => {
                if let Some(form) = self.inspector_mut() {
                    form.end = value;
                }
            }
            Message::InspLocation(value) => {
                if let Some(form) = self.inspector_mut() {
                    form.location = value;
                }
            }
            Message::InspNotes(value) => {
                if let Some(form) = self.inspector_mut() {
                    form.notes = value;
                }
            }
            Message::InspCalendar(index) => {
                if let Some(form) = self.inspector_mut() {
                    form.calendar = index;
                }
            }
            Message::InspRepeat(index) => {
                if let Some(form) = self.inspector_mut() {
                    form.repeat = index;
                }
            }
            Message::InspUntil(value) => {
                if let Some(form) = self.inspector_mut() {
                    form.until = value;
                }
            }
            Message::InspCount(value) => {
                if let Some(form) = self.inspector_mut() {
                    form.count = value;
                }
            }
            Message::InspReminder(index) => {
                if let Some(form) = self.inspector_mut() {
                    form.reminder = index;
                }
            }
            Message::SaveEvent => self.save_inspector(),
            Message::CloseInspector => self.inspector = None,
            Message::DeleteEvent => self.delete_inspected(),
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
                            google_id: server.google_client_id,
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
            Message::SyncGoogleId(value) => {
                if let Some(form) = self.sync_form.as_mut() {
                    form.google_id = value;
                }
            }
            Message::SignInGoogle => {
                let id = self
                    .sync_form
                    .as_ref()
                    .map(|form| form.google_id.trim().to_owned())
                    .unwrap_or_default();
                self.status = Some("Finish signing in with Google in your browser…".to_owned());
                return cosmic::task::future(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        caldav::google_sign_in(&id).map(|email| (email, id))
                    })
                    .await
                    .unwrap_or_else(|err| Err(err.to_string()));
                    Message::GoogleSignedIn(result)
                });
            }
            Message::GoogleSignedIn(result) => match result {
                Ok((email, client_id)) => {
                    self.sync_form = None;
                    let server = caldav::Server {
                        url: caldav::google_calendar_url(&email),
                        user: email,
                        google_client_id: client_id,
                    };
                    if let Err(err) = caldav::save_server(&server) {
                        self.status = Some(err.to_string());
                        return Task::none();
                    }
                    return self.start_sync(server);
                }
                Err(err) => self.status = Some(err),
            },
            Message::CancelSync => self.sync_form = None,
            Message::SaveSync => {
                let Some(form) = self.sync_form.take() else {
                    return Task::none();
                };
                let server = caldav::Server {
                    url: form.url.trim().to_owned(),
                    user: form.user.trim().to_owned(),
                    google_client_id: String::new(),
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
                    Ok(mut events) => {
                        store::assign_unknown_calendars(&mut events, &self.calendars);
                        self.events = events;
                        self.inspector = None;
                        self.deleted.clear();
                        self.edited.clear();
                        self.save_events();
                        self.save_sync_lists();
                        self.status = Some("Synced".to_owned());
                    }
                    Err(err) => self.status = Some(err),
                }
            }
            Message::AppMenuActivate(item_id) => {
                return self.handle_app_menu_activate(&item_id);
            }
        }
        Task::none()
    }
}

impl Calendar {
    fn build_app_menu_layout(&self) -> mac_appmenu::MenuLayout {
        use mac_appmenu::{MenuItemSpec, MenuSpec};

        let can_delete = self
            .inspector
            .as_ref()
            .is_some_and(|insp| insp.editing.is_some());
        let has_win = self.core.main_window_id().is_some();

        vec![
            MenuSpec::new(
                "file",
                "File",
                vec![
                    MenuItemSpec::action("file.new_event", "New Event").shortcut("N"),
                    MenuItemSpec::separator(),
                    MenuItemSpec::action("file.import_ics", "Import .ics…"),
                    MenuItemSpec::action("file.export_ics", "Export .ics…"),
                    MenuItemSpec::separator(),
                    MenuItemSpec::action("file.sync_caldav", "Sync CalDAV").enabled(!self.syncing),
                ],
            ),
            MenuSpec::new(
                "edit",
                "Edit",
                vec![
                    MenuItemSpec::action("edit.delete_event", "Delete Event")
                        .shortcut("Delete")
                        .enabled(can_delete),
                ],
            ),
            MenuSpec::new(
                "view",
                "View",
                vec![
                    MenuItemSpec::checkbox("view.day", "Day", matches!(self.view, View::Day)),
                    MenuItemSpec::checkbox("view.week", "Week", matches!(self.view, View::Week)),
                    MenuItemSpec::checkbox(
                        "view.month",
                        "Month",
                        matches!(self.view, View::Month),
                    ),
                    MenuItemSpec::separator(),
                    MenuItemSpec::action("view.today", "Today").shortcut("T"),
                ],
            ),
            MenuSpec::new(
                "window",
                "Window",
                vec![
                    MenuItemSpec::action("window.minimize", "Minimize").enabled(has_win),
                    MenuItemSpec::action("window.zoom", "Zoom").enabled(has_win),
                    MenuItemSpec::separator(),
                    MenuItemSpec::action("window.close", "Close Window").enabled(has_win),
                ],
            ),
        ]
    }

    fn sync_app_menu(&self) {
        self.app_menu.set_layout(self.build_app_menu_layout());
    }

    fn handle_app_menu_activate(&mut self, item_id: &str) -> app::Task<Message> {
        match item_id {
            "file.new_event" => self.update_inner(Message::NewEvent),
            "file.import_ics" => self.update_inner(Message::ToggleImport),
            "file.export_ics" => self.update_inner(Message::ToggleExport),
            "file.sync_caldav" => self.update_inner(Message::OpenSync),
            "edit.delete_event" => self.update_inner(Message::DeleteEvent),
            "view.day" => self.update_inner(Message::SetView(View::Day)),
            "view.week" => self.update_inner(Message::SetView(View::Week)),
            "view.month" => self.update_inner(Message::SetView(View::Month)),
            "view.today" => self.update_inner(Message::Today),
            "window.minimize" => self
                .core
                .main_window_id()
                .map(|id| cosmic::iced::window::minimize(id, true))
                .unwrap_or_else(Task::none),
            "window.zoom" => self
                .core
                .main_window_id()
                .map(cosmic::iced::window::toggle_maximize)
                .unwrap_or_else(Task::none),
            "window.close" => self
                .core
                .main_window_id()
                .map(cosmic::iced::window::close)
                .unwrap_or_else(Task::none),
            _ => Task::none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_file_uri_paths() {
        assert_eq!(parse_ics_path("/tmp/my invite.ics"), PathBuf::from("/tmp/my invite.ics"));
        assert_eq!(
            parse_ics_path("file:///home/user/My%20Events%2B2026.ics"),
            PathBuf::from("/home/user/My Events+2026.ics")
        );
        assert_eq!(
            parse_ics_path("file://localhost/tmp/team%20sync.ics"),
            PathBuf::from("/tmp/team sync.ics")
        );
    }
}
