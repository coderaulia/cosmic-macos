// SPDX-License-Identifier: GPL-3.0-only

//! Local persistence (events as an `.ics` file, calendars as a small table)
//! and `.ics` file import.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use jiff::civil::Date;

use crate::{
    caldav,
    event::{CalendarInfo, Event, default_calendars},
};

const EVENTS_FILE: &str = "events.ics";
/// Older versions stored events as tab-separated lines.
const LEGACY_EVENTS_FILE: &str = "events.tsv";
const CALENDARS_FILE: &str = "calendars.tsv";

/// `$XDG_DATA_HOME/mac-calendar` (or `~/.local/share/mac-calendar`).
pub fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("mac-calendar"))
}

fn parse_hhmm(value: &str) -> Option<(i8, i8)> {
    let (h, m) = value.trim().split_once(':')?;
    let (h, m): (i8, i8) = (h.parse().ok()?, m.parse().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some((h, m))
}

/// Events of the old `date<TAB>start<TAB>end<TAB>title<TAB>uid<TAB>href` file.
fn parse_legacy(contents: &str) -> Vec<Event> {
    contents
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(6, '\t');
            let date: Date = fields.next()?.parse().ok()?;
            let start = parse_hhmm(fields.next()?);
            let end = parse_hhmm(fields.next()?);
            let title = fields.next()?;
            let mut event = Event::new(date, title);
            event.start = start;
            event.end = end;
            if let Some(uid) = fields.next().filter(|uid| !uid.is_empty()) {
                event.uid = uid.to_owned();
            }
            event.href = fields.next().unwrap_or_default().to_owned();
            Some(event)
        })
        .collect()
}

pub fn load_events(dir: &Path) -> Vec<Event> {
    if let Ok(ics) = fs::read_to_string(dir.join(EVENTS_FILE)) {
        return caldav::parse_events(&ics, "");
    }
    fs::read_to_string(dir.join(LEGACY_EVENTS_FILE))
        .map(|contents| parse_legacy(&contents))
        .unwrap_or_default()
}

pub fn save_events(dir: &Path, events: &[Event]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    // Write beside the file first so a crash cannot leave half an agenda.
    let tmp = dir.join(format!("{EVENTS_FILE}.tmp"));
    fs::write(&tmp, caldav::events_to_ics(events, true))?;
    fs::rename(tmp, dir.join(EVENTS_FILE))
}

pub fn load_calendars(dir: &Path) -> Vec<CalendarInfo> {
    let calendars: Vec<CalendarInfo> = fs::read_to_string(dir.join(CALENDARS_FILE))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let id = fields.next().filter(|id| !id.is_empty())?.to_owned();
            let name = fields.next()?.to_owned();
            let color_idx = fields.next()?.parse().ok()?;
            let visible = fields.next().is_none_or(|v| v != "0");
            Some(CalendarInfo { id, name, color_idx, visible })
        })
        .collect();
    if calendars.is_empty() { default_calendars() } else { calendars }
}

pub fn save_calendars(dir: &Path, calendars: &[CalendarInfo]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let contents: String = calendars
        .iter()
        .map(|c| {
            format!(
                "{}\t{}\t{}\t{}\n",
                c.id,
                c.name.replace(['\t', '\n'], " "),
                c.color_idx,
                u8::from(c.visible)
            )
        })
        .collect();
    fs::write(dir.join(CALENDARS_FILE), contents)
}

/// A list of lines kept beside the events (events to delete or upload at the
/// next sync).
pub fn load_list(dir: &Path, name: &str) -> Vec<String> {
    fs::read_to_string(dir.join(name))
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

pub fn save_list(dir: &Path, name: &str, lines: &[String]) {
    if fs::create_dir_all(dir).is_ok() {
        let _ = fs::write(dir.join(name), lines.join("\n"));
    }
}

/// Events of an `.ics` file, ready to be added to `calendar`: they belong to
/// it, take its color and have no server link yet.
pub fn import_ics_file(path: &Path, calendar: &CalendarInfo) -> Result<Vec<Event>, String> {
    let bytes = fs::read(path).map_err(|err| format!("Could not read {}: {err}", path.display()))?;
    let ics = String::from_utf8_lossy(&bytes);
    let mut events = caldav::parse_events(&ics, "");
    if events.is_empty() {
        return Err(format!("No events found in {}.", path.display()));
    }
    for event in &mut events {
        event.calendar_id.clone_from(&calendar.id);
        event.color_idx = calendar.color_idx;
        event.href.clear();
    }
    Ok(events)
}

/// Add `imported` to `events`; an event with a uid that is already present
/// replaces it. Returns how many events were added or replaced.
pub fn merge_imported(events: &mut Vec<Event>, imported: Vec<Event>) -> usize {
    let count = imported.len();
    for mut event in imported {
        match events.iter_mut().find(|e| e.uid == event.uid) {
            Some(existing) => {
                // Keep the server link so the change is uploaded as an edit.
                event.href.clone_from(&existing.href);
                *existing = event;
            }
            None => events.push(event),
        }
    }
    events.sort_by_key(|e| (e.date, e.start));
    count
}

/// Export `events` as a standard iCalendar (`.ics`) file at `path`.
pub fn export_ics_file(path: &Path, events: &[Event]) -> Result<usize, String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .map_err(|err| format!("Could not create {}: {err}", parent.display()))?;
    }
    fs::write(path, caldav::events_to_ics(events, false))
        .map_err(|err| format!("Could not write {}: {err}", path.display()))?;
    Ok(events.len())
}

/// Events from servers or files may name calendars we do not have; move
/// them to the first calendar so they stay visible and togglable.
pub fn assign_unknown_calendars(events: &mut [Event], calendars: &[CalendarInfo]) {
    let Some(first) = calendars.first() else { return };
    for event in events {
        match calendars.iter().find(|c| {
            c.id.eq_ignore_ascii_case(&event.calendar_id)
                || c.name.eq_ignore_ascii_case(&event.calendar_id)
        }) {
            Some(calendar) => {
                event.calendar_id.clone_from(&calendar.id);
                event.color_idx = calendar.color_idx;
            }
            None => {
                event.calendar_id.clone_from(&first.id);
                event.color_idx = first.color_idx;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        event::new_uid,
        recurrence::{Frequency, RecurrenceRule},
    };
    use jiff::civil::date;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mac-calendar-test-{name}-{}-{}",
            std::process::id(),
            new_uid()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rich_event() -> Event {
        let mut event = Event::new(date(2026, 3, 14), "Pi day, party; bring pie");
        event.start = Some((15, 0));
        event.end = Some((16, 30));
        event.location = "Cafe, 12 Main St".to_owned();
        event.notes = "Line one\nLine two; with \\ backslash".to_owned();
        event.calendar_id = "family".to_owned();
        event.color_idx = 2;
        event.reminder_minutes = Some(30);
        event.recurrence = RecurrenceRule {
            freq: Frequency::Yearly,
            interval: 1,
            until: Some(date(2030, 3, 14)),
            count: Some(5),
            by_day: Vec::new(),
        };
        event
    }

    #[test]
    fn events_survive_a_save_and_load() {
        let dir = temp_dir("roundtrip");
        let mut all_day = Event::new(date(2026, 12, 25), "Holiday");
        all_day.href = "https://example.org/cal/1.ics".to_owned();
        let events = vec![rich_event(), all_day];
        save_events(&dir, &events).unwrap();
        assert_eq!(load_events(&dir), events);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_tsv_is_still_read() {
        let dir = temp_dir("legacy");
        fs::write(
            dir.join(LEGACY_EVENTS_FILE),
            "2026-02-01\t09:00\t10:00\tOld meeting\tuid-1\thttps://x/1.ics\n2026-02-02\t\t\tAll day\t\t\n",
        )
        .unwrap();
        let events = load_events(&dir);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].title, "Old meeting");
        assert_eq!(events[0].start, Some((9, 0)));
        assert_eq!(events[0].uid, "uid-1");
        assert_eq!(events[0].calendar_id, "personal");
        assert_eq!(events[1].start, None);
        assert!(!events[1].uid.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn calendars_round_trip_and_default() {
        let dir = temp_dir("calendars");
        assert_eq!(load_calendars(&dir), default_calendars());
        let mut calendars = default_calendars();
        calendars[1].visible = false;
        calendars.push(CalendarInfo {
            id: "gym".to_owned(),
            name: "Gym".to_owned(),
            color_idx: 4,
            visible: true,
        });
        save_calendars(&dir, &calendars).unwrap();
        assert_eq!(load_calendars(&dir), calendars);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn import_assigns_calendar_and_merges_by_uid() {
        let dir = temp_dir("import");
        let path = dir.join("invite.ics");
        fs::write(
            &path,
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:abc\r\nDTSTART:20260401T100000Z\r\n\
             SUMMARY:Board meeting\r\nLOCATION:Room 4\r\nCATEGORIES:MEETING\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nDTSTART;VALUE=DATE:20260402\r\nSUMMARY:No uid\r\nEND:VEVENT\r\n\
             END:VCALENDAR\r\n",
        )
        .unwrap();
        let work = default_calendars().remove(1);
        let imported = import_ics_file(&path, &work).unwrap();
        assert_eq!(imported.len(), 2);
        assert!(imported.iter().all(|e| e.calendar_id == "work" && e.color_idx == 1));
        assert!(!imported[1].uid.is_empty());
        assert_eq!(imported[0].location, "Room 4");

        let mut events = vec![Event::new(date(2026, 1, 1), "Existing")];
        assert_eq!(merge_imported(&mut events, imported.clone()), 2);
        assert_eq!(events.len(), 3);
        // Importing the same file again replaces instead of duplicating.
        merge_imported(&mut events, imported);
        assert_eq!(events.len(), 3);

        assert!(import_ics_file(&dir.join("missing.ics"), &work).is_err());
        fs::write(dir.join("empty.ics"), "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap();
        assert!(import_ics_file(&dir.join("empty.ics"), &work).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_calendars_move_to_the_first() {
        let calendars = default_calendars();
        let mut event = Event::new(date(2026, 1, 1), "x");
        event.calendar_id = "MEETING".to_owned();
        let mut work = Event::new(date(2026, 1, 1), "y");
        work.calendar_id = "Work".to_owned();
        let mut events = vec![event, work];
        assign_unknown_calendars(&mut events, &calendars);
        assert_eq!(events[0].calendar_id, "personal");
        assert_eq!(events[1].calendar_id, "work");
        assert_eq!(events[1].color_idx, 1);
    }

    #[test]
    fn export_writes_valid_ics_that_imports_back() {
        let dir = temp_dir("export");
        let path = dir.join("out.ics");
        let events = vec![rich_event()];
        assert_eq!(export_ics_file(&path, &events).unwrap(), 1);
        let work = default_calendars().remove(1);
        let reimported = import_ics_file(&path, &work).unwrap();
        assert_eq!(reimported.len(), 1);
        assert_eq!(reimported[0].title, events[0].title);
        assert_eq!(reimported[0].location, events[0].location);
        assert_eq!(reimported[0].notes, events[0].notes);
        assert_eq!(reimported[0].recurrence, events[0].recurrence);
        assert_eq!(reimported[0].reminder_minutes, events[0].reminder_minutes);
        assert_eq!(reimported[0].calendar_id, "work");
        fs::remove_dir_all(dir).unwrap();
    }
}
