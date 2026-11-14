// SPDX-License-Identifier: GPL-3.0-only

//! Pure UI formatting, folder mapping/sorting, initials, and preview snippet
//! helpers for Phase 3 (Look and feel).

use jiff::{Timestamp, Zoned, civil::DateTime, tz::TimeZone};

/// Canonical friendly folder names for common IMAP folder conventions.
pub fn friendly_folder_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.eq_ignore_ascii_case("INBOX") {
        return "Inbox".to_owned();
    }

    let stripped = strip_folder_prefix(trimmed);
    let lower = stripped.to_lowercase();

    match lower.as_str() {
        "inbox" => "Inbox".to_owned(),
        "drafts" | "draft" => "Drafts".to_owned(),
        "sent" | "sent mail" | "sent items" | "sent messages" => "Sent".to_owned(),
        "archive" | "archives" | "all mail" => "Archive".to_owned(),
        "trash" | "deleted" | "deleted items" | "deleted messages" | "bin" => "Trash".to_owned(),
        "spam" | "junk" | "junk e-mail" | "junk mail" | "bulk mail" => "Spam".to_owned(),
        "starred" => "Starred".to_owned(),
        "important" => "Important".to_owned(),
        "outbox" => "Outbox".to_owned(),
        _ => {
            // Replace hierarchy separators with readable spacing if nested.
            if stripped.contains('/') {
                stripped
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" / ")
            } else if stripped.contains('.') {
                stripped
                    .split('.')
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" / ")
            } else {
                stripped.to_owned()
            }
        }
    }
}

fn strip_folder_prefix(name: &str) -> &str {
    let prefixes = [
        "[Gmail]/",
        "[Google Mail]/",
        "[Gmail].",
        "[Google Mail].",
        "INBOX.",
        "INBOX/",
    ];
    let mut current = name;
    for prefix in prefixes {
        if current.len() > prefix.len()
            && current[..prefix.len()].eq_ignore_ascii_case(prefix)
        {
            current = &current[prefix.len()..];
            break;
        }
    }
    current
}

/// Logical sidebar ordering key for a raw IMAP folder name:
/// Inbox, Drafts, Sent, Archive, Spam, Trash, Starred, Important, Outbox,
/// followed by custom folders in alphabetical order.
pub fn folder_sort_key(raw: &str) -> (u8, String, String) {
    let friendly = friendly_folder_name(raw);
    let priority = match friendly.as_str() {
        "Inbox" => 0,
        "Drafts" => 1,
        "Sent" => 2,
        "Archive" => 3,
        "Spam" => 4,
        "Trash" => 5,
        "Starred" => 6,
        "Important" => 7,
        "Outbox" => 8,
        _ => 100,
    };
    (priority, friendly.to_lowercase(), raw.to_lowercase())
}

/// Sort folder names in logical mailbox order.
pub fn sort_folders(folders: &mut [String]) {
    folders.sort_by_key(|name| folder_sort_key(name));
}

/// Choose a symbolic icon name for a mailbox.
pub fn folder_icon(raw: &str) -> &'static str {
    let friendly = friendly_folder_name(raw);
    match friendly.as_str() {
        "Inbox" => "mail-inbox-symbolic",
        "Drafts" => "document-edit-symbolic",
        "Sent" => "mail-send-symbolic",
        "Archive" => "mail-archive-symbolic",
        "Spam" => "mail-mark-junk-symbolic",
        "Trash" => "user-trash-symbolic",
        "Starred" => "starred-symbolic",
        "Important" => "emblem-important-symbolic",
        "Outbox" => "mail-send-symbolic",
        _ => {
            let lower = raw.to_lowercase();
            if lower.contains("sent") {
                "mail-send-symbolic"
            } else if lower.contains("draft") {
                "document-edit-symbolic"
            } else if lower.contains("trash") || lower.contains("deleted") || lower.contains("bin")
            {
                "user-trash-symbolic"
            } else if lower.contains("junk") || lower.contains("spam") {
                "mail-mark-junk-symbolic"
            } else if lower.contains("archive") || lower.contains("all mail") {
                "mail-archive-symbolic"
            } else {
                "folder-symbolic"
            }
        }
    }
}

const MONTH_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn weekday_short(w: jiff::civil::Weekday) -> &'static str {
    match w {
        jiff::civil::Weekday::Monday => "Mon",
        jiff::civil::Weekday::Tuesday => "Tue",
        jiff::civil::Weekday::Wednesday => "Wed",
        jiff::civil::Weekday::Thursday => "Thu",
        jiff::civil::Weekday::Friday => "Fri",
        jiff::civil::Weekday::Saturday => "Sat",
        jiff::civil::Weekday::Sunday => "Sun",
    }
}

/// Format a message timestamp relative to `now`:
/// - Same day: `Today 14:48`
/// - Previous day: `Yesterday`
/// - Within the past 6 days: `Mon`, `Tue`, ...
/// - Same year: `Nov 3`
/// - Earlier year: `Nov 3, 2025`
pub fn format_relative_date_at(msg_dt: &Zoned, now: &Zoned) -> String {
    let msg_date = msg_dt.date();
    let today = now.date();

    if msg_date == today {
        return format!("Today {:02}:{:02}", msg_dt.hour(), msg_dt.minute());
    }
    if today.yesterday().ok() == Some(msg_date) {
        return "Yesterday".to_owned();
    }

    let days_ago = today
        .since(msg_date)
        .map(|span| span.get_days())
        .unwrap_or(i32::MAX);

    if (2..=6).contains(&days_ago) {
        return weekday_short(msg_date.weekday()).to_owned();
    }

    let month = MONTH_SHORT
        .get((msg_date.month() - 1) as usize)
        .copied()
        .unwrap_or("Jan");
    if msg_date.year() == today.year() {
        format!("{month} {}", msg_date.day())
    } else {
        format!("{month} {}, {}", msg_date.day(), msg_date.year())
    }
}

/// Format a date using either an optional Unix timestamp or a fallback date string
/// (`YYYY-MM-DD HH:MM` or RFC 3339), relative to `now`.
pub fn format_relative_date_with_now(
    timestamp: Option<i64>,
    date_str: &str,
    now: &Zoned,
) -> String {
    if let Some(ts) = timestamp
        && let Ok(parsed) = Timestamp::from_second(ts)
    {
        let zoned = parsed.to_zoned(now.time_zone().clone());
        return format_relative_date_at(&zoned, now);
    }
    if let Some(zoned) = parse_date_string(date_str, now.time_zone()) {
        return format_relative_date_at(&zoned, now);
    }
    date_str.to_owned()
}

/// Convenience wrapper using the current local time.
pub fn format_relative_date(timestamp: Option<i64>, date_str: &str) -> String {
    format_relative_date_with_now(timestamp, date_str, &Zoned::now())
}

/// Format a detailed date/time for the reading pane header.
pub fn format_header_date_with_now(
    timestamp: Option<i64>,
    date_str: &str,
    now: &Zoned,
) -> String {
    let zoned = timestamp
        .and_then(|ts| Timestamp::from_second(ts).ok())
        .map(|t| t.to_zoned(now.time_zone().clone()))
        .or_else(|| parse_date_string(date_str, now.time_zone()));

    let Some(dt) = zoned else {
        return date_str.to_owned();
    };
    let date = dt.date();
    let today = now.date();
    let time = format!("{:02}:{:02}", dt.hour(), dt.minute());

    if date == today {
        format!("Today at {time}")
    } else if today.yesterday().ok() == Some(date) {
        format!("Yesterday at {time}")
    } else {
        let month = MONTH_SHORT
            .get((date.month() - 1) as usize)
            .copied()
            .unwrap_or("Jan");
        if date.year() == today.year() {
            format!("{month} {} at {time}", date.day())
        } else {
            format!("{month} {}, {} at {time}", date.day(), date.year())
        }
    }
}

pub fn format_header_date(timestamp: Option<i64>, date_str: &str) -> String {
    format_header_date_with_now(timestamp, date_str, &Zoned::now())
}

fn parse_date_string(date_str: &str, tz: &TimeZone) -> Option<Zoned> {
    let trimmed = date_str.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(ts) = trimmed.parse::<Timestamp>() {
        return Some(ts.to_zoned(tz.clone()));
    }
    if let Ok(zoned) = trimmed.parse::<Zoned>() {
        return Some(zoned.with_time_zone(tz.clone()));
    }
    // Parse "YYYY-MM-DD HH:MM" or "YYYY-MM-DD HH:MM:SS"
    let normalized = trimmed.replacen(' ', "T", 1);
    if let Ok(civil) = normalized.parse::<DateTime>() {
        return civil.to_zoned(tz.clone()).ok();
    }
    None
}

/// Generate 1-2 uppercase initials from a sender display name or email address.
pub fn sender_initials(name: &str, email: &str) -> String {
    let cleaned = name
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '<' || c == '>');

    let words: Vec<&str> = cleaned
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .collect();

    if words.len() >= 2 {
        let first = words[0].chars().find(|c| c.is_alphanumeric());
        let last = words[words.len() - 1].chars().find(|c| c.is_alphanumeric());
        if let (Some(a), Some(b)) = (first, last) {
            return format!(
                "{}{}",
                a.to_uppercase().collect::<String>(),
                b.to_uppercase().collect::<String>()
            );
        }
    } else if let Some(word) = words.first()
        && let Some(ch) = word.chars().find(|c| c.is_alphanumeric())
    {
        return ch.to_uppercase().collect();
    }

    // Fall back to email local part.
    let local = email.trim().split('@').next().unwrap_or("");
    let parts: Vec<&str> = local
        .split(['.', '_', '-'])
        .filter(|p| p.chars().any(char::is_alphanumeric))
        .collect();
    if parts.len() >= 2 {
        let first = parts[0].chars().find(|c| c.is_alphanumeric());
        let second = parts[1].chars().find(|c| c.is_alphanumeric());
        if let (Some(a), Some(b)) = (first, second) {
            return format!(
                "{}{}",
                a.to_uppercase().collect::<String>(),
                b.to_uppercase().collect::<String>()
            );
        }
    } else if let Some(part) = parts.first()
        && let Some(ch) = part.chars().find(|c| c.is_alphanumeric())
    {
        return ch.to_uppercase().collect();
    }

    "?".to_owned()
}

/// Deterministic pleasant avatar background RGB color (0.0..=1.0) for a sender.
pub fn avatar_color_rgb(seed: &str) -> [f32; 3] {
    const PALETTE: [[f32; 3]; 8] = [
        [0.21, 0.47, 0.85], // blue
        [0.37, 0.36, 0.80], // indigo
        [0.57, 0.33, 0.76], // purple
        [0.12, 0.57, 0.56], // teal
        [0.20, 0.58, 0.36], // green
        [0.78, 0.45, 0.16], // amber
        [0.80, 0.32, 0.28], // coral
        [0.72, 0.27, 0.49], // rose
    ];
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in seed.trim().to_lowercase().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    PALETTE[(hash as usize) % PALETTE.len()]
}

/// Extract a clean, single-line-normalized preview snippet (up to `max_chars`
/// characters) from plain text or HTML body content.
pub fn extract_preview_snippet(
    plain_text: Option<&str>,
    html: Option<&str>,
    max_chars: usize,
) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let raw = if let Some(text) = plain_text.filter(|t| !t.trim().is_empty()) {
        text.to_owned()
    } else if let Some(html) = html.filter(|h| !h.trim().is_empty()) {
        strip_html_for_preview(html)
    } else {
        return String::new();
    };

    let mut collapsed = String::with_capacity(raw.len().min(max_chars * 2));
    let mut prev_space = true;
    for ch in raw.chars() {
        if matches!(ch, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{00AD}') {
            continue;
        }
        if ch.is_whitespace() {
            if !prev_space {
                collapsed.push(' ');
                prev_space = true;
            }
        } else {
            collapsed.push(ch);
            prev_space = false;
        }
    }
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    truncate_snippet(trimmed, max_chars)
}

fn truncate_snippet(text: &str, max_chars: usize) -> String {
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return text.to_owned();
    }
    if max_chars <= 1 {
        return "…".to_owned();
    }
    let limit = max_chars - 1;
    let byte_end = text
        .char_indices()
        .nth(limit)
        .map_or(text.len(), |(idx, _)| idx);
    let slice = &text[..byte_end];
    // Back up to the last whitespace if it doesn't throw away too much of the line.
    let cut = match slice.rfind(' ') {
        Some(pos) if pos >= byte_end / 2 => slice[..pos].trim_end(),
        _ => slice.trim_end(),
    };
    format!("{cut}…")
}

fn strip_html_for_preview(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;

    while let Some(lt) = rest.find('<') {
        out.push_str(&decode_basic_entities(&rest[..lt]));
        let after_lt = &rest[lt + 1..];
        let Some(gt) = after_lt.find('>') else {
            break;
        };
        let tag = after_lt[..gt].trim();
        let tag_lower = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_lowercase();

        // Skip <style>, <script>, and <head> blocks entirely.
        if !tag.starts_with('/') && matches!(tag_lower.as_str(), "style" | "script" | "head") {
            let end_tag = format!("</{tag_lower}");
            let remaining = &after_lt[gt + 1..];
            if let Some(end_pos) = find_ascii_case_insensitive(remaining, &end_tag) {
                let after_end = &remaining[end_pos + end_tag.len()..];
                if let Some(close_gt) = after_end.find('>') {
                    rest = &after_end[close_gt + 1..];
                    out.push(' ');
                    continue;
                }
            }
        }

        if matches!(
            tag_lower.as_str(),
            "p" | "div" | "br" | "tr" | "td" | "th" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                | "blockquote"
                | "hr"
        ) {
            out.push(' ');
        }

        rest = &after_lt[gt + 1..];
    }
    out.push_str(&decode_basic_entities(rest));
    out
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let needle_bytes = needle.as_bytes();
    haystack
        .as_bytes()
        .windows(needle_bytes.len())
        .position(|w| w.eq_ignore_ascii_case(needle_bytes))
}

fn decode_basic_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
        .replace("&zwnj;", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zoned(s: &str) -> Zoned {
        let dt: DateTime = s.parse().unwrap();
        dt.to_zoned(TimeZone::UTC).unwrap()
    }

    #[test]
    fn friendly_folder_names_map_common_conventions() {
        assert_eq!(friendly_folder_name("INBOX"), "Inbox");
        assert_eq!(friendly_folder_name("inbox"), "Inbox");
        assert_eq!(friendly_folder_name("INBOX.Drafts"), "Drafts");
        assert_eq!(friendly_folder_name("[Gmail]/Drafts"), "Drafts");
        assert_eq!(friendly_folder_name("[Gmail]/Sent Mail"), "Sent");
        assert_eq!(friendly_folder_name("INBOX.Sent Items"), "Sent");
        assert_eq!(friendly_folder_name("[Gmail]/All Mail"), "Archive");
        assert_eq!(friendly_folder_name("INBOX.Archive"), "Archive");
        assert_eq!(friendly_folder_name("INBOX.Trash"), "Trash");
        assert_eq!(friendly_folder_name("[Gmail]/Bin"), "Trash");
        assert_eq!(friendly_folder_name("Deleted Items"), "Trash");
        assert_eq!(friendly_folder_name("INBOX.Junk"), "Spam");
        assert_eq!(friendly_folder_name("[Gmail]/Spam"), "Spam");
        assert_eq!(friendly_folder_name("INBOX.Receipts"), "Receipts");
        assert_eq!(friendly_folder_name("Work/Projects"), "Work / Projects");
    }

    #[test]
    fn folders_sort_in_logical_order() {
        let mut folders = vec![
            "Work".to_owned(),
            "[Gmail]/Trash".to_owned(),
            "INBOX.Drafts".to_owned(),
            "Alpha".to_owned(),
            "[Gmail]/Spam".to_owned(),
            "INBOX".to_owned(),
            "[Gmail]/All Mail".to_owned(),
            "[Gmail]/Sent Mail".to_owned(),
        ];
        sort_folders(&mut folders);
        let friendly: Vec<String> = folders.iter().map(|f| friendly_folder_name(f)).collect();
        assert_eq!(
            friendly,
            vec![
                "Inbox", "Drafts", "Sent", "Archive", "Spam", "Trash", "Alpha", "Work"
            ]
        );
    }

    #[test]
    fn folder_icons_match_roles() {
        assert_eq!(folder_icon("INBOX"), "mail-inbox-symbolic");
        assert_eq!(folder_icon("INBOX.Drafts"), "document-edit-symbolic");
        assert_eq!(folder_icon("[Gmail]/Sent Mail"), "mail-send-symbolic");
        assert_eq!(folder_icon("[Gmail]/All Mail"), "mail-archive-symbolic");
        assert_eq!(folder_icon("[Gmail]/Spam"), "mail-mark-junk-symbolic");
        assert_eq!(folder_icon("INBOX.Trash"), "user-trash-symbolic");
        assert_eq!(folder_icon("Projects"), "folder-symbolic");
    }

    #[test]
    fn relative_date_formats_today_yesterday_weekday_and_short_date() {
        // 2026-11-14 is a Saturday.
        let now = zoned("2026-11-14T16:30:00");

        assert_eq!(
            format_relative_date_at(&zoned("2026-11-14T14:48:00"), &now),
            "Today 14:48"
        );
        assert_eq!(
            format_relative_date_at(&zoned("2026-11-13T09:15:00"), &now),
            "Yesterday"
        );
        // 2..=6 days ago -> weekday name
        assert_eq!(
            format_relative_date_at(&zoned("2026-11-12T12:00:00"), &now),
            "Thu"
        );
        assert_eq!(
            format_relative_date_at(&zoned("2026-11-09T08:00:00"), &now),
            "Mon"
        );
        // Older in the same year -> short date
        assert_eq!(
            format_relative_date_at(&zoned("2026-11-03T10:00:00"), &now),
            "Nov 3"
        );
        // Earlier year -> short date with year
        assert_eq!(
            format_relative_date_at(&zoned("2025-12-25T10:00:00"), &now),
            "Dec 25, 2025"
        );
    }

    #[test]
    fn relative_date_parses_string_fallback() {
        let now = zoned("2026-11-14T16:30:00");
        assert_eq!(
            format_relative_date_with_now(None, "2026-11-14 14:48", &now),
            "Today 14:48"
        );
        assert_eq!(
            format_relative_date_with_now(None, "2026-11-13 23:59", &now),
            "Yesterday"
        );
    }

    #[test]
    fn initials_generation_handles_names_and_emails() {
        assert_eq!(sender_initials("Alice Smith", "alice@example.com"), "AS");
        assert_eq!(
            sender_initials("\"Jean-Luc Picard\"", "picard@starfleet.org"),
            "JP"
        );
        assert_eq!(sender_initials("Dr. Jane R. Doe", "jdoe@example.com"), "DD");
        assert_eq!(sender_initials("Cher", "cher@example.com"), "C");
        assert_eq!(sender_initials("", "alice.smith@example.com"), "AS");
        assert_eq!(sender_initials("", "bob_jones@example.com"), "BJ");
        assert_eq!(sender_initials("", "root@localhost"), "R");
        assert_eq!(sender_initials("", ""), "?");
    }

    #[test]
    fn preview_snippet_extraction_cleans_text_and_html() {
        let plain = "  Hello\n\nthere,\t\tthis is   a   test message\nwith multiple lines. ";
        assert_eq!(
            extract_preview_snippet(Some(plain), None, 100),
            "Hello there, this is a test message with multiple lines."
        );

        let html = "<html><head><style>body { color: red; }</style></head>\
            <body><p>Hi&nbsp;team,</p><p>The build &amp; tests passed.</p></body></html>";
        assert_eq!(
            extract_preview_snippet(None, Some(html), 100),
            "Hi team, The build & tests passed."
        );

        let long = "This is a much longer preview snippet that should truncate cleanly on a word boundary";
        let truncated = extract_preview_snippet(Some(long), None, 35);
        assert!(truncated.ends_with('…'));
        assert!(truncated.chars().count() <= 35);
    }
}
