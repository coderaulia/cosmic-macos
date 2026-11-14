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

use crate::Event;

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
fn parse_time(params: &str, value: &str) -> Option<(Date, Option<(i8, i8)>)> {
    let value = value.trim();
    if value.len() == 8 {
        let date = DateTime::strptime("%Y%m%d%H%M%S", &format!("{value}000000")).ok()?;
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

/// Events found in a block of iCalendar text.
fn parse_events(ics: &str, href: &str) -> Vec<Event> {
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
    for line in lines {
        let line = line.trim_end();
        if line == "BEGIN:VEVENT" {
            current = Some(Event {
                date: Date::constant(1970, 1, 1),
                start: None,
                end: None,
                title: String::new(),
                uid: String::new(),
                href: href.to_owned(),
            });
        } else if line == "END:VEVENT" {
            if let Some(event) = current.take()
                && !event.uid.is_empty()
                && event.date.year() > 1970
            {
                events.push(event);
            }
        } else if let Some(event) = current.as_mut()
            && let Some((name_and_params, value)) = line.split_once(':')
        {
            let (name, params) = name_and_params
                .split_once(';')
                .unwrap_or((name_and_params, ""));
            match name {
                "UID" => event.uid = value.to_owned(),
                "SUMMARY" => {
                    event.title = value
                        .replace("\\,", ",")
                        .replace(r"\;", ";")
                        .replace("\\n", " ")
                        .replace("\\\\", "\\");
                }
                "DTSTART" => {
                    if let Some((date, start)) = parse_time(params, value) {
                        event.date = date;
                        event.start = start;
                    }
                }
                "DTEND" => {
                    if let Some((_, end)) = parse_time(params, value) {
                        event.end = end;
                    }
                }
                _ => {}
            }
        }
    }
    events
}

fn to_ics(event: &Event) -> String {
    let date = |d: Date| format!("{:04}{:02}{:02}", d.year(), d.month(), d.day());
    let time = |(h, m): (i8, i8)| format!("T{h:02}{m:02}00");
    let stamp = Zoned::now().with_time_zone(TimeZone::UTC);
    let mut out = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//mac-calendar//EN\r\nBEGIN:VEVENT\r\n");
    out.push_str(&format!("UID:{}\r\n", event.uid));
    out.push_str(&format!(
        "DTSTAMP:{:04}{:02}{:02}T{:02}{:02}{:02}Z\r\n",
        stamp.year(),
        stamp.month(),
        stamp.day(),
        stamp.hour(),
        stamp.minute(),
        stamp.second()
    ));
    match event.start {
        Some(start) => {
            out.push_str(&format!("DTSTART:{}{}\r\n", date(event.date), time(start)));
            if let Some(end) = event.end {
                out.push_str(&format!("DTEND:{}{}\r\n", date(event.date), time(end)));
            }
        }
        None => {
            out.push_str(&format!("DTSTART;VALUE=DATE:{}\r\n", date(event.date)));
            let next = event.date.tomorrow().unwrap_or(event.date);
            out.push_str(&format!("DTEND;VALUE=DATE:{}\r\n", date(next)));
        }
    }
    let title = event
        .title
        .replace('\\', "\\\\")
        .replace(',', "\\,")
        .replace(';', r"\;");
    out.push_str(&format!("SUMMARY:{title}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"));
    out
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

/// Bring the local events and the server in line: upload new local events,
/// delete events removed here, take the server's version of the rest, and
/// add events that only exist on the server.
pub fn sync(
    server: &Server,
    password: &str,
    local: Vec<Event>,
    deleted: Vec<String>,
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
            merged.push((*remote_event).clone());
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
