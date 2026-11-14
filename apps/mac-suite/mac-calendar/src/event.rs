// SPDX-License-Identifier: GPL-3.0-only

//! The event and calendar data model, plus the pure helpers on it: search
//! and reminders.

use std::sync::atomic::{AtomicU32, Ordering};

use jiff::{
    ToSpan,
    civil::{Date, DateTime},
};

use crate::recurrence::{Occurrence, RecurrenceRule, expand};

/// One event. Times are `(hour, minute)`; an event without a start time is
/// all-day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub date: Date,
    pub start: Option<(i8, i8)>,
    pub end: Option<(i8, i8)>,
    pub title: String,
    /// Identifies the event on the server and here.
    pub uid: String,
    /// The event's URL on the server; empty until it has been uploaded.
    pub href: String,
    pub location: String,
    pub notes: String,
    /// Id of the calendar this event belongs to (`CATEGORIES` in iCalendar).
    pub calendar_id: String,
    /// Palette index used when the calendar is unknown.
    pub color_idx: u8,
    /// Minutes before the start at which to remind; `None` for no reminder.
    pub reminder_minutes: Option<u32>,
    pub recurrence: RecurrenceRule,
}

impl Event {
    pub fn new(date: Date, title: &str) -> Self {
        Self {
            date,
            start: None,
            end: None,
            title: title.to_owned(),
            uid: new_uid(),
            href: String::new(),
            location: String::new(),
            notes: String::new(),
            calendar_id: DEFAULT_CALENDAR.to_owned(),
            color_idx: 0,
            reminder_minutes: None,
            recurrence: RecurrenceRule::default(),
        }
    }

    /// Whether `query` (case-insensitive) occurs in the title, location or
    /// notes. An empty query matches everything.
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || [&self.title, &self.location, &self.notes]
                .iter()
                .any(|field| field.to_lowercase().contains(&query))
    }
}

/// A fresh unique id for a new event.
pub fn new_uid() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{nanos:x}-{:x}-{:x}@mac-calendar",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

pub const DEFAULT_CALENDAR: &str = "personal";

/// Calendar colors, like the macOS calendar color menu (r, g, b).
pub const PALETTE: [(u8, u8, u8); 8] = [
    (0x1B, 0x7F, 0xF5), // blue
    (0xFF, 0x9F, 0x0A), // orange
    (0x30, 0xB0, 0x50), // green
    (0xFF, 0x45, 0x3A), // red
    (0xAF, 0x52, 0xDE), // purple
    (0xA2, 0x84, 0x5E), // brown
    (0xFF, 0x2D, 0x78), // pink
    (0x64, 0xD2, 0xFF), // teal
];

pub fn palette_color(index: u8) -> cosmic::iced::Color {
    let (r, g, b) = PALETTE[usize::from(index) % PALETTE.len()];
    cosmic::iced::Color::from_rgb8(r, g, b)
}

/// A named calendar in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarInfo {
    pub id: String,
    pub name: String,
    pub color_idx: u8,
    pub visible: bool,
}

pub fn default_calendars() -> Vec<CalendarInfo> {
    [("personal", "Personal", 0), ("work", "Work", 1), ("family", "Family", 2)]
        .into_iter()
        .map(|(id, name, color_idx)| CalendarInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            color_idx,
            visible: true,
        })
        .collect()
}

/// An id for a calendar called `name` that is not in `existing` yet.
pub fn calendar_id_for(name: &str, existing: &[CalendarInfo]) -> String {
    let slug: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-');
    let base = if slug.is_empty() { "calendar" } else { slug };
    let mut id = base.to_owned();
    let mut n = 2;
    while existing.iter().any(|c| c.id == id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// Minutes since midnight of an `(hour, minute)` time.
pub fn minutes((h, m): (i8, i8)) -> i32 {
    i32::from(h) * 60 + i32::from(m)
}

/// Occurrences of `events` in `from..=to` that pass the calendar visibility
/// and the search `query`.
pub fn visible_occurrences(
    events: &[Event],
    calendars: &[CalendarInfo],
    query: &str,
    from: Date,
    to: Date,
) -> Vec<Occurrence> {
    expand(events, from, to)
        .into_iter()
        .filter(|o| {
            let event = &events[o.index];
            event.matches(query)
                && calendars
                    .iter()
                    .find(|c| c.id == event.calendar_id)
                    .is_none_or(|c| c.visible)
        })
        .collect()
}

/// Occurrences whose reminder time has been reached at `now` but whose
/// event has not started yet.
pub fn due_reminders(events: &[Event], now: DateTime) -> Vec<Occurrence> {
    let today = now.date();
    let tomorrow = today.tomorrow().unwrap_or(today);
    expand(events, today, tomorrow)
        .into_iter()
        .filter(|o| {
            let event = &events[o.index];
            let (Some((h, m)), Some(before)) = (event.start, event.reminder_minutes) else {
                return false;
            };
            let begins = o.date.at(h, m, 0, 0);
            let Ok(remind_at) = begins.checked_sub(i64::from(before).minutes()) else {
                return false;
            };
            remind_at <= now && now < begins
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn search_matches_title_location_and_notes() {
        let mut event = Event::new(date(2026, 1, 1), "Dentist");
        event.location = "12 Main Street".to_owned();
        event.notes = "Bring insurance card".to_owned();
        assert!(event.matches(""));
        assert!(event.matches("dent"));
        assert!(event.matches("MAIN st"));
        assert!(event.matches("insurance"));
        assert!(!event.matches("gym"));
    }

    #[test]
    fn visible_occurrences_filters_by_calendar_and_query() {
        let mut work = Event::new(date(2026, 1, 5), "Planning");
        work.calendar_id = "work".to_owned();
        let mut home = Event::new(date(2026, 1, 5), "Planting");
        home.location = "Garden".to_owned();
        let events = vec![work, home];
        let mut calendars = default_calendars();
        let day = date(2026, 1, 5);
        assert_eq!(visible_occurrences(&events, &calendars, "", day, day).len(), 2);
        assert_eq!(visible_occurrences(&events, &calendars, "plann", day, day).len(), 1);
        assert_eq!(visible_occurrences(&events, &calendars, "garden", day, day)[0].index, 1);
        calendars[1].visible = false;
        let left = visible_occurrences(&events, &calendars, "", day, day);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].index, 1);
    }

    #[test]
    fn calendar_ids_are_unique() {
        let calendars = default_calendars();
        assert_eq!(calendar_id_for("Work", &calendars), "work-2");
        assert_eq!(calendar_id_for("Side Project!", &calendars), "side-project");
        assert_eq!(calendar_id_for("???", &calendars), "calendar");
    }

    #[test]
    fn reminders_fire_inside_their_window_only() {
        let mut event = Event::new(date(2026, 1, 5), "Call");
        event.start = Some((10, 0));
        event.reminder_minutes = Some(15);
        let events = vec![event];
        let at = |h, m| date(2026, 1, 5).at(h, m, 0, 0);
        assert!(due_reminders(&events, at(9, 44)).is_empty());
        assert_eq!(due_reminders(&events, at(9, 45)).len(), 1);
        assert_eq!(due_reminders(&events, at(9, 59)).len(), 1);
        assert!(due_reminders(&events, at(10, 0)).is_empty());
    }

    #[test]
    fn reminder_crosses_midnight() {
        let mut event = Event::new(date(2026, 1, 6), "Flight");
        event.start = Some((0, 10));
        event.reminder_minutes = Some(30);
        let events = vec![event];
        assert_eq!(due_reminders(&events, date(2026, 1, 5).at(23, 50, 0, 0)).len(), 1);
    }
}
