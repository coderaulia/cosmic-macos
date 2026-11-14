// SPDX-License-Identifier: GPL-3.0-only

//! A small CalDAV client: fetch the events of one calendar, upload new ones
//! and delete removed ones. Works with servers that accept HTTP Basic
//! authentication (Nextcloud, Radicale, Baikal, Fastmail, iCloud with an app
//! password). The calendar's own URL is entered by the user.

use std::{collections::HashMap, fs, path::PathBuf, process::Command};

use base64::Engine;
use jiff::{
    Zoned,
    civil::{Date, DateTime},
    tz::TimeZone,
};

use crate::{
    event::{Event, new_uid},
    recurrence::RecurrenceRule,
};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Default)]
pub struct Server {
    pub url: String,
    pub user: String,
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("mac-calendar/caldav.conf"))
}

pub fn load_server() -> Option<Server> {
    let contents = fs::read_to_string(config_path()?).ok()?;
    let mut server = Server::default();
    for line in contents.lines() {
        match line.split_once('=')? {
            ("url", value) => server.url = value.to_owned(),
            ("user", value) => server.user = value.to_owned(),
            _ => {}
        }
    }
    (!server.url.is_empty()).then_some(server)
}

pub fn save_server(server: &Server) -> std::io::Result<()> {
    let path = config_path().ok_or_else(|| std::io::Error::other("no config directory"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, format!("url={}\nuser={}\n", server.url, server.user))
}

/// The password lives in the desktop keyring, not on disk.
pub fn store_password(user: &str, password: &str) -> Result<()> {
    use std::{io::Write, process::Stdio};
    let mut child = Command::new("secret-tool")
        .args(["store", "--label", "Calendar", "service", "mac-calendar", "account", user])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|err| format!("secret-tool is needed to store the password: {err}"))?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(password.as_bytes())
        .map_err(|err| err.to_string())?;
    child
        .wait()
        .map_err(|err| err.to_string())?
        .success()
        .then_some(())
        .ok_or_else(|| "could not store the password in the keyring".to_owned())
}

pub fn load_password(user: &str) -> Option<String> {
    let output = Command::new("secret-tool")
        .args(["lookup", "service", "mac-calendar", "account", user])
        .output()
        .ok()?;
    let password = String::from_utf8(output.stdout).ok()?;
    (output.status.success() && !password.is_empty()).then_some(password)
}

fn request(
    method: &str,
    url: &str,
    server: &Server,
    password: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Result<(u16, String)> {
    // The password goes only to the configured server, and only encrypted
    // unless that server is on this computer or the home network.
    if !origin(url).eq_ignore_ascii_case(origin(&server.url)) {
        return Err("Refusing to send the password to a different server.".to_owned());
    }
    if origin(&server.url).to_ascii_lowercase().starts_with("http://")
        && !is_private_host(&origin(&server.url).to_ascii_lowercase())
    {
        return Err("Use an https:// address: the password must not be sent unencrypted.".to_owned());
    }
    let credentials =
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{password}", server.user));
    let mut builder = ureq::http::Request::builder()
        .method(method)
        .uri(url)
        .header("Authorization", format!("Basic {credentials}"));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(body).map_err(|err| err.to_string())?;
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build();
    let agent: ureq::Agent = config.into();
    let mut response = agent
        .run(request)
        .map_err(|err| format!("Could not reach the server: {err}"))?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().unwrap_or_default();
    Ok((status, text))
}

/// `scheme://host[:port]` of a URL.
fn origin(url: &str) -> &str {
    let after_scheme = url.find("://").map_or(0, |index| index + 3);
    let end = url[after_scheme..]
        .find('/')
        .map_or(url.len(), |index| after_scheme + index);
    &url[..end]
}

/// Whether plain `http://` is acceptable: only for this computer or a home network.
fn is_private_host(origin: &str) -> bool {
    let host = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    let host = host.split(':').next().unwrap_or(host);
    host == "localhost"
        || host.ends_with(".local")
        || host.starts_with("127.")
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host
            .strip_prefix("172.")
            .and_then(|rest| rest.split('.').next()?.parse::<u8>().ok())
            .is_some_and(|second| (16..=31).contains(&second))
}

/// Turn an `href` from the server into a full URL on the same server.
/// A link to any other server is refused, so the password is never sent there.
fn absolute(base: &str, href: &str) -> Option<String> {
    if href.starts_with("http://") || href.starts_with("https://") {
        return origin(href)
            .eq_ignore_ascii_case(origin(base))
            .then(|| href.to_owned());
    }
    let separator = if href.starts_with('/') { "" } else { "/" };
    Some(format!("{}{separator}{href}", origin(base)))
}

/// The text inside the first `<…name>…</…>` element, whatever its prefix.
fn element<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let mut search_from = 0;
    while let Some(found) = xml[search_from..].find(name) {
        let at = search_from + found;
        let before = &xml[..at];
        let opens = before.rfind('<');
        let closes = before.rfind('>');
        // Must be right after "<" or "<prefix:" and followed by ">" or attributes.
        if let Some(open) = opens
            && closes.is_none_or(|close| close < open)
            && !before[open + 1..].starts_with('/')
            && before[open + 1..].chars().all(|c| c.is_alphanumeric() || c == ':')
        {
            let after = &xml[at + name.len()..];
            if after.starts_with('>') || after.starts_with(' ') {
                let start = at + name.len() + after.find('>')? + 1;
                let rest = &xml[start..];
                let end = rest.find("</")?;
                return Some(&rest[..end]);
            }
        }
        search_from = at + name.len();
    }
    None
}

fn unescape(text: &str) -> String {
    let text = text
        .trim()
        .trim_start_matches("<![CDATA[")
        .trim_end_matches("]]>");
    text.replace("&#13;", "")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// Date and optional time of a DTSTART/DTEND value, in local time.
pub fn parse_time(params: &str, value: &str) -> Option<(Date, Option<(i8, i8)>)> {
    let value = value.trim();
    if value.len() == 8 {
        let date = DateTime::strptime("%Y%m%d%H%M%S", format!("{value}000000")).ok()?;
        return Some((date.date(), None));
    }
    let utc = value.ends_with('Z');
    let civil = DateTime::strptime("%Y%m%dT%H%M%S", value.trim_end_matches('Z')).ok()?;
    let zone = if utc {
        Some(TimeZone::UTC)
    } else {
        params
            .split(';')
            .find_map(|param| param.strip_prefix("TZID="))
            .and_then(|tzid| TimeZone::get(tzid.trim_matches('"')).ok())
    };
    let civil = match zone {
        Some(zone) => civil
            .to_zoned(zone)
            .ok()?
            .with_time_zone(TimeZone::system())
            .datetime(),
        None => civil,
    };
    Some((civil.date(), Some((civil.hour(), civil.minute()))))
}

/// Undo iCalendar TEXT escaping (`\n`, `\,`, `\;`, `\\`).
fn unescape_text(value: &str) -> String {
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

fn escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            other => out.push(other),
        }
    }
    out
}

/// Minutes of a duration such as `PT1H30M`, `P1D` or `P1W`.
fn parse_duration_minutes(value: &str) -> Option<u32> {
    let rest = value.strip_prefix('P')?;
    let mut total = 0u32;
    let mut number = String::new();
    for c in rest.chars() {
        let unit = match c {
            '0'..='9' => {
                number.push(c);
                continue;
            }
            'T' => continue,
            'W' => 7 * 24 * 60,
            'D' => 24 * 60,
            'H' => 60,
            'M' => 1,
            'S' => 0,
            _ => return None,
        };
        total = total.checked_add(number.parse::<u32>().ok()?.checked_mul(unit)?)?;
        number.clear();
    }
    number.is_empty().then_some(total)
}

/// Minutes before the start of a `TRIGGER` such as `-PT15M`. Triggers after
/// the start or at absolute times are not supported.
fn parse_trigger(value: &str) -> Option<u32> {
    let value = value.trim();
    match value.strip_prefix('-') {
        Some(rest) => parse_duration_minutes(rest),
        None => parse_duration_minutes(value.strip_prefix('+').unwrap_or(value)).filter(|m| *m == 0),
    }
}

fn trigger_value(minutes: u32) -> String {
    if minutes == 0 {
        "PT0S".to_owned()
    } else if minutes.is_multiple_of(24 * 60) {
        format!("-P{}D", minutes / (24 * 60))
    } else if minutes.is_multiple_of(60) {
        format!("-PT{}H", minutes / 60)
    } else {
        format!("-PT{minutes}M")
    }
}

fn blank_event(href: &str) -> Event {
    let mut event = Event::new(Date::constant(1970, 1, 1), "");
    event.uid.clear();
    event.href = href.to_owned();
    event
}

/// What the end of a timed event is, given its DTSTART day.
fn end_on_day(start_day: Date, end: (Date, Option<(i8, i8)>)) -> Option<(i8, i8)> {
    match end {
        (day, Some(time)) if day == start_day => Some(time),
        // Ends on a later day: show it until the end of this one.
        (day, _) if day > start_day => Some((23, 59)),
        _ => None,
    }
}

/// Events found in a block of iCalendar text. Events without a `UID` get
/// a fresh one, so any `.ics` file can be imported.
pub fn parse_events(ics: &str, href: &str) -> Vec<Event> {
    // Unfold continued lines first.
    let mut lines: Vec<String> = Vec::new();
    for line in ics.lines() {
        if let Some(continued) = line.strip_prefix([' ', '\t'])
            && let Some(last) = lines.last_mut()
        {
            last.push_str(continued);
        } else {
            lines.push(line.to_owned());
        }
    }
    let mut events = Vec::new();
    let mut current: Option<Event> = None;
    let mut dtend: Option<(Date, Option<(i8, i8)>)> = None;
    let mut duration: Option<u32> = None;
    let mut in_alarm = false;
    for line in lines {
        let line = line.trim_end();
        if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
            current = Some(blank_event(href));
            (dtend, duration, in_alarm) = (None, None, false);
        } else if line.eq_ignore_ascii_case("END:VEVENT") {
            if let Some(mut event) = current.take()
                && event.date.year() > 1970
            {
                if event.start.is_some() {
                    event.end = dtend.and_then(|end| end_on_day(event.date, end)).or_else(|| {
                        let (h, m) = event.start?;
                        let total = (i32::from(h) * 60 + i32::from(m))
                            .saturating_add(i32::try_from(duration?).ok()?);
                        let total = total.min(23 * 60 + 59);
                        Some(((total / 60) as i8, (total % 60) as i8))
                    });
                }
                if event.uid.is_empty() {
                    event.uid = new_uid();
                }
                events.push(event);
            }
        } else if line.eq_ignore_ascii_case("BEGIN:VALARM") {
            in_alarm = true;
        } else if line.eq_ignore_ascii_case("END:VALARM") {
            in_alarm = false;
        } else if let Some(event) = current.as_mut()
            && let Some((name_and_params, value)) = line.split_once(':')
        {
            let (name, params) = name_and_params
                .split_once(';')
                .unwrap_or((name_and_params, ""));
            if in_alarm {
                // Only the first alarm's TRIGGER counts; alarm text is ignored.
                if name.eq_ignore_ascii_case("TRIGGER") && event.reminder_minutes.is_none() {
                    event.reminder_minutes = parse_trigger(value);
                }
                continue;
            }
            match name.to_ascii_uppercase().as_str() {
                "UID" => event.uid = value.to_owned(),
                "SUMMARY" => event.title = unescape_text(value).replace('\n', " "),
                "LOCATION" => event.location = unescape_text(value).replace('\n', ", "),
                "DESCRIPTION" => event.notes = unescape_text(value),
                "CATEGORIES" => {
                    if let Some(first) = value.split(',').next().map(str::trim)
                        && !first.is_empty()
                    {
                        event.calendar_id = unescape_text(first);
                    }
                }
                "X-MAC-COLOR" => event.color_idx = value.trim().parse().unwrap_or(0),
                "X-MAC-HREF" if href.is_empty() => event.href = value.to_owned(),
                "RRULE" => {
                    event.recurrence = RecurrenceRule::parse(value, |until| {
                        parse_time("", until).map(|(date, _)| date)
                    })
                    .unwrap_or_default();
                }
                "DTSTART" => {
                    if let Some((date, start)) = parse_time(params, value) {
                        event.date = date;
                        event.start = start;
                    }
                }
                "DTEND" => dtend = parse_time(params, value),
                "DURATION" => duration = parse_duration_minutes(value),
                _ => {}
            }
        }
    }
    events
}

/// Break a content line at 75 octets, as iCalendar requires.
fn fold(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + 8);
    let mut width = 0;
    for c in line.chars() {
        // Continuation lines start with a space, which counts too.
        if width + c.len_utf8() > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += c.len_utf8();
    }
    out.push_str("\r\n");
    out
}

fn write_event(out: &mut String, event: &Event, local: bool) {
    let date = |d: Date| format!("{:04}{:02}{:02}", d.year(), d.month(), d.day());
    let time = |(h, m): (i8, i8)| format!("T{h:02}{m:02}00");
    let stamp = Zoned::now().with_time_zone(TimeZone::UTC);
    let mut put = |line: String| out.push_str(&fold(&line));
    put("BEGIN:VEVENT".to_owned());
    put(format!("UID:{}", event.uid));
    put(format!(
        "DTSTAMP:{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        stamp.year(),
        stamp.month(),
        stamp.day(),
        stamp.hour(),
        stamp.minute(),
        stamp.second()
    ));
    match event.start {
        Some(start) => {
            put(format!("DTSTART:{}{}", date(event.date), time(start)));
            if let Some(end) = event.end {
                put(format!("DTEND:{}{}", date(event.date), time(end)));
            }
        }
        None => {
            put(format!("DTSTART;VALUE=DATE:{}", date(event.date)));
            let next = event.date.tomorrow().unwrap_or(event.date);
            put(format!("DTEND;VALUE=DATE:{}", date(next)));
        }
    }
    put(format!("SUMMARY:{}", escape_text(&event.title)));
    if !event.location.is_empty() {
        put(format!("LOCATION:{}", escape_text(&event.location)));
    }
    if !event.notes.is_empty() {
        put(format!("DESCRIPTION:{}", escape_text(&event.notes)));
    }
    if !event.calendar_id.is_empty() {
        put(format!("CATEGORIES:{}", escape_text(&event.calendar_id)));
    }
    if let Some(rule) = event.recurrence.to_value() {
        put(format!("RRULE:{rule}"));
    }
    if local {
        put(format!("X-MAC-COLOR:{}", event.color_idx));
        if !event.href.is_empty() {
            put(format!("X-MAC-HREF:{}", event.href));
        }
    }
    if let Some(minutes) = event.reminder_minutes {
        put("BEGIN:VALARM".to_owned());
        put("ACTION:DISPLAY".to_owned());
        put(format!("DESCRIPTION:{}", escape_text(&event.title)));
        put(format!("TRIGGER:{}", trigger_value(minutes)));
        put("END:VALARM".to_owned());
    }
    put("END:VEVENT".to_owned());
}

/// A whole calendar file holding `events`. `local` adds this app's own
/// bookkeeping lines (color and server link) that a server has no use for.
pub fn events_to_ics(events: &[Event], local: bool) -> String {
    let mut out = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//mac-calendar//EN\r\n");
    for event in events {
        write_event(&mut out, event, local);
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

/// One event as the calendar resource uploaded to a server.
fn to_ics(event: &Event) -> String {
    events_to_ics(std::slice::from_ref(event), false)
}

/// All events of the calendar.
fn fetch(server: &Server, password: &str) -> Result<Vec<Event>> {
    let body = r#"<?xml version="1.0" encoding="utf-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><d:getetag/><c:calendar-data/></d:prop>
  <c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"/></c:comp-filter></c:filter>
</c:calendar-query>"#;
    let (status, text) = request(
        "REPORT",
        &server.url,
        server,
        password,
        &[("Depth", "1"), ("Content-Type", "application/xml; charset=utf-8")],
        body.as_bytes().to_vec(),
    )?;
    match status {
        200 | 207 => {}
        401 | 403 => return Err("The server rejected the user name or password.".to_owned()),
        404 => return Err("Calendar not found: check the calendar URL.".to_owned()),
        other => return Err(format!("The server answered with status {other}.")),
    }
    let mut events = Vec::new();
    for block in text.split("response>").skip(1).step_by(2) {
        let href = element(block, "href").map(unescape).unwrap_or_default();
        // An event whose link points to another server is ignored.
        let Some(href) = absolute(&server.url, &href) else {
            continue;
        };
        if let Some(data) = element(block, "calendar-data") {
            events.extend(parse_events(&unescape(data), &href));
        }
    }
    Ok(events)
}

fn upload(server: &Server, password: &str, event: &Event) -> Result<String> {
    let base = server.url.trim_end_matches('/');
    let url = format!("{base}/{}.ics", event.uid);
    let (status, _) = request(
        "PUT",
        &url,
        server,
        password,
        &[("Content-Type", "text/calendar; charset=utf-8")],
        to_ics(event).into_bytes(),
    )?;
    match status {
        200 | 201 | 204 => Ok(url),
        other => Err(format!("Could not upload \"{}\" (status {other}).", event.title)),
    }
}

pub fn delete(server: &Server, password: &str, href: &str) -> Result<()> {
    let (status, _) = request("DELETE", href, server, password, &[], Vec::new())?;
    // Already gone counts as deleted.
    matches!(status, 200 | 202 | 204 | 404)
        .then_some(())
        .ok_or_else(|| format!("Could not delete an event (status {status})."))
}

/// Bring the local events and the server in line: upload new local events
/// and the ones edited here (`edited` holds their uids), delete events
/// removed here, take the server's version of the rest, and add events that
/// only exist on the server.
pub fn sync(
    server: &Server,
    password: &str,
    local: Vec<Event>,
    deleted: Vec<String>,
    edited: &[String],
) -> Result<Vec<Event>> {
    for href in &deleted {
        delete(server, password, href)?;
    }
    let remote = fetch(server, password)?;
    let by_uid: HashMap<&str, &Event> = remote.iter().map(|e| (e.uid.as_str(), e)).collect();

    let mut merged: Vec<Event> = Vec::new();
    for event in local {
        if event.href.is_empty() {
            // New here: upload it.
            let href = upload(server, password, &event)?;
            merged.push(Event { href, ..event });
        } else if let Some(remote_event) = by_uid.get(event.uid.as_str()) {
            if edited.contains(&event.uid) {
                // Edited here since the last sync: our version wins.
                let href = upload(server, password, &event)?;
                merged.push(Event { href, ..event });
            } else {
                // Keep what the server does not store.
                merged.push(Event {
                    color_idx: event.color_idx,
                    ..(*remote_event).clone()
                });
            }
        }
        // Synced before but gone from the server: deleted elsewhere, drop it.
    }
    for event in remote {
        if !merged.iter().any(|e| e.uid == event.uid) {
            merged.push(event);
        }
    }
    merged.sort_by_key(|e| (e.date, e.start));
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recurrence::Frequency;
    use jiff::civil::{Weekday, date};

    #[test]
    fn ics_round_trip_with_rich_fields() {
        let mut ev = Event::new(date(2026, 5, 20), "Quarterly Review, Q2; Team");
        ev.start = Some((14, 30));
        ev.end = Some((16, 0));
        ev.location = "Room 4B, Building 2".to_owned();
        ev.notes = "Bring slides\nReview roadmap; check KPIs".to_owned();
        ev.calendar_id = "work".to_owned();
        ev.color_idx = 1;
        ev.reminder_minutes = Some(15);
        ev.recurrence = RecurrenceRule {
            freq: Frequency::Weekly,
            interval: 2,
            until: Some(date(2026, 12, 31)),
            count: Some(10),
            by_day: vec![Weekday::Wednesday],
        };

        let ics = events_to_ics(std::slice::from_ref(&ev), true);
        assert!(ics.contains("LOCATION:Room 4B\\, Building 2"));
        assert!(ics.contains("DESCRIPTION:Bring slides\\nReview roadmap\\; check KPIs"));
        assert!(ics.contains("CATEGORIES:work"));
        assert!(ics.contains("RRULE:FREQ=WEEKLY;INTERVAL=2;UNTIL=20261231;COUNT=10;BYDAY=WE"));
        assert!(ics.contains("BEGIN:VALARM"));
        assert!(ics.contains("TRIGGER:-PT15M"));

        let parsed = parse_events(&ics, "");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0], ev);
    }

    #[test]
    fn parses_minimal_legacy_ics_and_duration() {
        let ics = "BEGIN:VCALENDAR\r\n\
                   VERSION:2.0\r\n\
                   BEGIN:VEVENT\r\n\
                   UID:legacy-1\r\n\
                   DTSTART:20260704T090000\r\n\
                   DURATION:PT1H30M\r\n\
                   SUMMARY:Parade\r\n\
                   END:VEVENT\r\n\
                   END:VCALENDAR\r\n";
        let parsed = parse_events(ics, "");
        assert_eq!(parsed.len(), 1);
        let ev = &parsed[0];
        assert_eq!(ev.uid, "legacy-1");
        assert_eq!(ev.date, date(2026, 7, 4));
        assert_eq!(ev.start, Some((9, 0)));
        assert_eq!(ev.end, Some((10, 30)));
        assert_eq!(ev.title, "Parade");
        assert_eq!(ev.calendar_id, "personal");
        assert_eq!(ev.reminder_minutes, None);
        assert!(ev.recurrence.is_none());
    }

    #[test]
    fn parses_folded_lines_and_valarm_units() {
        let ics = "BEGIN:VCALENDAR\r\n\
                   BEGIN:VEVENT\r\n\
                   UID:fold-1\r\n\
                   DTSTART:20260801T100000\r\n\
                   DTEND:20260801T110000\r\n\
                   SUMMARY:Very long summary \r\n that wraps across lines\r\n\
                   BEGIN:VALARM\r\n\
                   ACTION:DISPLAY\r\n\
                   TRIGGER:-P1D\r\n\
                   END:VALARM\r\n\
                   END:VEVENT\r\n\
                   END:VCALENDAR\r\n";
        let parsed = parse_events(ics, "");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].title, "Very long summary that wraps across lines");
        assert_eq!(parsed[0].reminder_minutes, Some(1440));
    }
}
