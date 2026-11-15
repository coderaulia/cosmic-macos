// SPDX-License-Identifier: GPL-3.0-only
//! Reads `mac-calendar`'s local store (`events.ics`, falling back to legacy
//! `events.tsv`) for the Up Next desktop widget.

use std::fs;
use std::path::{Path, PathBuf};

use jiff::{
    civil::{Date, DateTime},
    tz::TimeZone,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgendaEntry {
    pub date: Date,
    /// Start time as `"HH:MM"`; `None` for all-day events.
    pub time: Option<String>,
    pub title: String,
}

/// `$XDG_DATA_HOME/mac-calendar` (or `~/.local/share/mac-calendar`).
pub fn calendar_data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("mac-calendar"))
}

/// Loads all events from `mac-calendar`'s local store.
pub fn load_agenda() -> Vec<AgendaEntry> {
    let Some(dir) = calendar_data_dir() else {
        return Vec::new();
    };
    load_agenda_from(&dir)
}

pub fn load_agenda_from(dir: &Path) -> Vec<AgendaEntry> {
    if let Ok(ics) = fs::read_to_string(dir.join("events.ics")) {
        return parse_ics(&ics);
    }
    fs::read_to_string(dir.join("events.tsv"))
        .map(|tsv| parse_tsv(&tsv))
        .unwrap_or_default()
}

pub fn parse_tsv(contents: &str) -> Vec<AgendaEntry> {
    contents
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(6, '\t');
            let date: Date = fields.next()?.parse().ok()?;
            let start = fields.next()?;
            let _end = fields.next()?;
            let title = fields.next()?.to_owned();
            Some(AgendaEntry {
                date,
                time: (!start.is_empty()).then(|| start.to_owned()),
                title,
            })
        })
        .collect()
}

pub fn parse_ics(ics: &str) -> Vec<AgendaEntry> {
    let mut lines: Vec<String> = Vec::new();
    for line in ics.lines() {
        if let Some(cont) = line.strip_prefix([' ', '\t'])
            && let Some(last) = lines.last_mut()
        {
            last.push_str(cont);
        } else {
            lines.push(line.to_owned());
        }
    }

    let mut entries = Vec::new();
    let mut in_event = false;
    let mut in_alarm = false;
    let mut dtstart: Option<(Date, Option<String>)> = None;
    let mut title = String::new();

    for raw in lines {
        let line = raw.trim_end();
        if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
            in_event = true;
            in_alarm = false;
            dtstart = None;
            title.clear();
        } else if line.eq_ignore_ascii_case("END:VEVENT") {
            if in_event && let Some((date, time)) = dtstart.take() {
                entries.push(AgendaEntry {
                    date,
                    time,
                    title: if title.is_empty() {
                        "(Untitled)".to_owned()
                    } else {
                        std::mem::take(&mut title)
                    },
                });
            }
            in_event = false;
        } else if line.eq_ignore_ascii_case("BEGIN:VALARM") {
            in_alarm = true;
        } else if line.eq_ignore_ascii_case("END:VALARM") {
            in_alarm = false;
        } else if in_event
            && !in_alarm
            && let Some((name_params, value)) = line.split_once(':')
        {
            let (name, params) = name_params
                .split_once(';')
                .unwrap_or((name_params, ""));
            match name.to_ascii_uppercase().as_str() {
                "DTSTART" => dtstart = parse_ics_time(params, value),
                "SUMMARY" => title = unescape_ics_text(value).replace('\n', " "),
                _ => {}
            }
        }
    }
    entries
}

fn parse_ics_time(params: &str, value: &str) -> Option<(Date, Option<String>)> {
    let value = value.trim();
    if value.len() == 8 {
        let dt = DateTime::strptime("%Y%m%d%H%M%S", format!("{value}000000")).ok()?;
        return Some((dt.date(), None));
    }
    let utc = value.ends_with('Z');
    let civil = DateTime::strptime("%Y%m%dT%H%M%S", value.trim_end_matches('Z')).ok()?;
    let zone = if utc {
        Some(TimeZone::UTC)
    } else {
        params
            .split(';')
            .find_map(|p| p.strip_prefix("TZID="))
            .and_then(|tz| TimeZone::get(tz.trim_matches('"')).ok())
    };
    let local = match zone {
        Some(z) => civil
            .to_zoned(z)
            .ok()?
            .with_time_zone(TimeZone::system())
            .datetime(),
        None => civil,
    };
    Some((local.date(), Some(format!("{:02}:{:02}", local.hour(), local.minute()))))
}

fn unescape_ics_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Next `limit` events at or after `now_hm` on `today`.
pub fn upcoming<'a>(
    agenda: &'a [AgendaEntry],
    today: Date,
    now_hm: &str,
    limit: usize,
) -> Vec<&'a AgendaEntry> {
    let mut next: Vec<&AgendaEntry> = agenda
        .iter()
        .filter(|e| match &e.time {
            _ if e.date > today => true,
            Some(t) => e.date == today && t.as_str() >= now_hm,
            None => e.date == today,
        })
        .collect();
    next.sort_by(|a, b| (a.date, a.time.as_deref()).cmp(&(b.date, b.time.as_deref())));
    next.truncate(limit);
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn parses_ics_and_sorts_upcoming() {
        let ics = "\
BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
DTSTART:20260110T080000\r\n\
SUMMARY:Morning standup\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART;VALUE=DATE:20260110\r\n\
SUMMARY:Release day\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART:20260110T153000\r\n\
SUMMARY:Design\\, Review\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART:20260111T090000\r\n\
SUMMARY:Tomorrow sync\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";
        let events = parse_ics(ics);
        assert_eq!(events.len(), 4);
        let next = upcoming(&events, date(2026, 1, 10), "12:00", 3);
        let titles: Vec<_> = next.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["Release day", "Design, Review", "Tomorrow sync"]);
    }

    #[test]
    fn parses_legacy_tsv() {
        let tsv = "2026-01-10\t14:00\t15:00\tDentist\tuid1\t\n2026-01-11\t\t\tHoliday\tuid2\t\n";
        let events = parse_tsv(tsv);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].time.as_deref(), Some("14:00"));
        assert_eq!(events[1].time, None);
    }
}
