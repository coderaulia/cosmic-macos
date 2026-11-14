// SPDX-License-Identifier: GPL-3.0-only

//! Plain-text-with-simple-formatting to clean email HTML converter, plus
//! plain-text and HTML quote assembly for replies and forwards.

/// Original message context displayed as a collapsible quote below the editor
/// and appended to the outgoing plain-text and HTML bodies on send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuotedOriginal {
    pub is_forward: bool,
    pub from: String,
    pub date: String,
    pub subject: String,
    pub body: String,
}

impl QuotedOriginal {
    /// Attribution header shown above the quoted block in replies.
    pub fn attribution_line(&self) -> String {
        if self.is_forward {
            "---------- Forwarded message ----------".to_owned()
        } else if self.date.is_empty() {
            format!("On an earlier date, {} wrote:", self.from)
        } else {
            format!("On {}, {} wrote:", self.date, self.from)
        }
    }
}

/// Escape HTML special characters so arbitrary user text cannot inject markup.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Return `true` if `url` uses a safe link scheme for outgoing emails.
fn is_safe_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
}

/// Render inline markdown-like formatting (`**bold**`, `*italic*`,
/// `` `code` ``, `[label](url)`, and bare `http(s)://` links) into clean HTML.
pub fn render_inline_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while !rest.is_empty() {
        // 1. Markdown link: [label](url)
        if rest.starts_with('[')
            && let Some((label, url, consumed)) = parse_markdown_link(rest)
            && is_safe_url(url)
        {
            out.push_str("<a href=\"");
            out.push_str(&escape_html(url.trim()));
            out.push_str("\" style=\"color: #2563eb; text-decoration: underline;\">");
            out.push_str(&render_inline_html(label));
            out.push_str("</a>");
            rest = &rest[consumed..];
            continue;
        }

        // 2. Bold: **text**
        if let Some(after_open) = rest.strip_prefix("**")
            && let Some((inner, consumed)) = find_closing_marker(after_open, "**")
        {
            out.push_str("<strong>");
            out.push_str(&render_inline_html(inner));
            out.push_str("</strong>");
            rest = &after_open[consumed..];
            continue;
        }

        // 3. Italic: *text* (single asterisk, not **)
        if rest.starts_with('*')
            && !rest.starts_with("**")
            && let Some(after_open) = rest.strip_prefix('*')
            && let Some((inner, consumed)) = find_closing_single_star(after_open)
        {
            out.push_str("<em>");
            out.push_str(&render_inline_html(inner));
            out.push_str("</em>");
            rest = &after_open[consumed..];
            continue;
        }

        // 4. Inline code: `text`
        if let Some(after_open) = rest.strip_prefix('`')
            && let Some(end) = after_open.find('`')
            && end > 0
        {
            out.push_str("<code style=\"background: #f1f5f9; padding: 1px 4px; border-radius: 3px; font-family: monospace;\">");
            out.push_str(&escape_html(&after_open[..end]));
            out.push_str("</code>");
            rest = &after_open[end + 1..];
            continue;
        }

        // 5. Bare URL: https://... or http://...
        if (rest.starts_with("https://") || rest.starts_with("http://"))
            && let Some((url, consumed)) = scan_bare_url(rest)
        {
            let escaped = escape_html(url);
            out.push_str("<a href=\"");
            out.push_str(&escaped);
            out.push_str("\" style=\"color: #2563eb; text-decoration: underline;\">");
            out.push_str(&escaped);
            out.push_str("</a>");
            rest = &rest[consumed..];
            continue;
        }

        // Default: emit one escaped character.
        let ch = rest.chars().next().expect("non-empty rest");
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
        rest = &rest[ch.len_utf8()..];
    }

    out
}

fn parse_markdown_link(input: &str) -> Option<(&str, &str, usize)> {
    let after_bracket = input.strip_prefix('[')?;
    let mut depth = 1usize;
    let mut close_bracket = None;
    for (idx, ch) in after_bracket.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close_bracket = Some(idx);
                    break;
                }
            }
            '\n' => return None,
            _ => {}
        }
    }
    let close_bracket = close_bracket?;
    let label = &after_bracket[..close_bracket];
    let after_label = &after_bracket[close_bracket + 1..];
    let after_paren = after_label.strip_prefix('(')?;

    let mut paren_depth = 1usize;
    let mut close_paren = None;
    for (idx, ch) in after_paren.char_indices() {
        match ch {
            '(' => paren_depth += 1,
            ')' => {
                paren_depth -= 1;
                if paren_depth == 0 {
                    close_paren = Some(idx);
                    break;
                }
            }
            '\n' | ' ' | '\t' => return None,
            _ => {}
        }
    }
    let close_paren = close_paren?;
    let url = &after_paren[..close_paren];
    if label.is_empty() || url.is_empty() {
        return None;
    }
    // Total bytes consumed from `input`: '[' (1) + close_bracket + "](" (2) + close_paren + ')' (1)
    let consumed = 1 + close_bracket + 2 + close_paren + 1;
    Some((label, url, consumed))
}

fn find_closing_marker<'a>(after_open: &'a str, marker: &str) -> Option<(&'a str, usize)> {
    if after_open.starts_with(char::is_whitespace) {
        return None;
    }
    let mut search_from = 0usize;
    while let Some(pos) = after_open[search_from..].find(marker) {
        let abs = search_from + pos;
        let inner = &after_open[..abs];
        if !inner.is_empty() && !inner.ends_with(char::is_whitespace) {
            return Some((inner, abs + marker.len()));
        }
        search_from = abs + marker.len();
    }
    None
}

fn find_closing_single_star(after_open: &str) -> Option<(&str, usize)> {
    if after_open.starts_with(char::is_whitespace) {
        return None;
    }
    let bytes = after_open.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'*' {
            // Skip `**` runs when looking for a single `*`.
            if i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                i += 2;
                continue;
            }
            let inner = &after_open[..i];
            if !inner.is_empty() && !inner.ends_with(char::is_whitespace) {
                return Some((inner, i + 1));
            }
        }
        let ch = after_open[i..].chars().next()?;
        i += ch.len_utf8();
    }
    None
}

fn scan_bare_url(input: &str) -> Option<(&str, usize)> {
    let mut end = 0usize;
    for (idx, ch) in input.char_indices() {
        if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'') {
            break;
        }
        end = idx + ch.len_utf8();
    }
    let mut raw = &input[..end];
    while let Some(last) = raw.chars().last() {
        let strip = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | ']' => true,
            ')' => {
                let opens = raw.chars().filter(|&c| c == '(').count();
                let closes = raw.chars().filter(|&c| c == ')').count();
                closes > opens
            }
            _ => false,
        };
        if strip {
            raw = &raw[..raw.len() - last.len_utf8()];
        } else {
            break;
        }
    }
    if raw == "http://" || raw == "https://" || raw.is_empty() {
        return None;
    }
    Some((raw, raw.len()))
}

fn parse_unordered_item(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(' ');
    let leading_spaces = line.len() - trimmed.len();
    if leading_spaces > 3 {
        return None;
    }
    trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
}

fn parse_ordered_item(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(' ');
    let leading_spaces = line.len() - trimmed.len();
    if leading_spaces > 3 {
        return None;
    }
    let digits_len = trimmed
        .bytes()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if digits_len == 0 {
        return None;
    }
    trimmed[digits_len..].strip_prefix(". ")
}

fn parse_quote_line(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(' ');
    if let Some(rest) = trimmed.strip_prefix("> ") {
        Some(rest)
    } else {
        trimmed.strip_prefix('>')
    }
}

/// Convert plain text with simple formatting (`**bold**`, `*italic*`, links,
/// bulleted/numbered lists, and blockquotes) into clean HTML fragments.
pub fn plain_to_html_fragment(input: &str) -> String {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut out = String::new();
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];

        if line.trim().is_empty() {
            i += 1;
            continue;
        }

        // Unordered list
        if parse_unordered_item(line).is_some() {
            out.push_str("<ul style=\"margin: 0 0 12px 0; padding-left: 20px;\">\n");
            while i < lines.len()
                && let Some(item) = parse_unordered_item(lines[i])
            {
                out.push_str("  <li>");
                out.push_str(&render_inline_html(item));
                out.push_str("</li>\n");
                i += 1;
            }
            out.push_str("</ul>\n");
            continue;
        }

        // Ordered list
        if parse_ordered_item(line).is_some() {
            out.push_str("<ol style=\"margin: 0 0 12px 0; padding-left: 20px;\">\n");
            while i < lines.len()
                && let Some(item) = parse_ordered_item(lines[i])
            {
                out.push_str("  <li>");
                out.push_str(&render_inline_html(item));
                out.push_str("</li>\n");
                i += 1;
            }
            out.push_str("</ol>\n");
            continue;
        }

        // Blockquote
        if parse_quote_line(line).is_some() {
            let mut inner_lines = Vec::new();
            while i < lines.len()
                && let Some(inner) = parse_quote_line(lines[i])
            {
                inner_lines.push(inner);
                i += 1;
            }
            let inner_html = plain_to_html_fragment(&inner_lines.join("\n"));
            out.push_str("<blockquote style=\"margin: 0 0 12px 0; padding-left: 12px; border-left: 2px solid #cbd5e1; color: #475569;\">\n");
            out.push_str(&inner_html);
            out.push_str("</blockquote>\n");
            continue;
        }

        // Regular paragraph (breaks on blank line or start of list/quote)
        let mut para_lines = Vec::new();
        while i < lines.len() {
            let cur = lines[i];
            if cur.trim().is_empty()
                || parse_unordered_item(cur).is_some()
                || parse_ordered_item(cur).is_some()
                || parse_quote_line(cur).is_some()
            {
                break;
            }
            para_lines.push(render_inline_html(cur));
            i += 1;
        }
        out.push_str("<p style=\"margin: 0 0 12px 0;\">");
        out.push_str(&para_lines.join("<br>\n"));
        out.push_str("</p>\n");
    }

    out
}

/// Build the complete plain-text message body, appending any quoted original
/// below the user's reply/forward text.
pub fn build_email_plain(new_body: &str, quote: Option<&QuotedOriginal>) -> String {
    let trimmed = new_body.trim_end_matches(['\r', '\n']);
    let Some(quote) = quote else {
        return trimmed.to_owned();
    };
    let mut out = String::new();
    if !trimmed.is_empty() {
        out.push_str(trimmed);
        out.push_str("\n\n");
    }
    if quote.is_forward {
        out.push_str("---------- Forwarded message ----------\n");
        out.push_str(&format!("From: {}\n", quote.from));
        if !quote.date.is_empty() {
            out.push_str(&format!("Date: {}\n", quote.date));
        }
        out.push_str(&format!("Subject: {}\n\n", quote.subject));
        out.push_str(quote.body.trim_end_matches(['\r', '\n']));
    } else {
        out.push_str(&quote.attribution_line());
        out.push('\n');
        for line in quote.body.lines() {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Build the complete, simple, email-safe HTML document for the outgoing message.
pub fn build_email_html(new_body: &str, quote: Option<&QuotedOriginal>) -> String {
    let body_html = plain_to_html_fragment(new_body);
    let quote_html = match quote {
        None => String::new(),
        Some(q) if q.is_forward => {
            let inner = plain_to_html_fragment(&q.body);
            format!(
                "<div style=\"margin-top: 16px; padding-top: 12px; border-top: 1px solid #cbd5e1;\">\n\
                 <p style=\"margin: 0 0 8px 0; color: #475569; font-size: 13px;\">\
                 <strong>---------- Forwarded message ----------</strong><br>\n\
                 From: {}<br>\n\
                 Date: {}<br>\n\
                 Subject: {}</p>\n\
                 {inner}\
                 </div>\n",
                escape_html(&q.from),
                escape_html(&q.date),
                escape_html(&q.subject),
            )
        }
        Some(q) => {
            let inner = plain_to_html_fragment(&q.body);
            format!(
                "<div style=\"margin-top: 16px;\">\n\
                 <p style=\"margin: 0 0 8px 0; color: #475569;\">{}</p>\n\
                 <blockquote style=\"margin: 0; padding-left: 12px; border-left: 2px solid #cbd5e1; color: #475569;\">\n\
                 {inner}\
                 </blockquote>\n\
                 </div>\n",
                escape_html(&q.attribution_line()),
            )
        }
    };

    format!(
        "<!DOCTYPE html>\n\
         <html>\n\
         <head><meta charset=\"utf-8\"></head>\n\
         <body style=\"font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif; font-size: 14px; line-height: 1.5; color: #1e293b;\">\n\
         {body_html}{quote_html}\
         </body>\n\
         </html>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_bold_italic_and_escapes_html() {
        let html = plain_to_html_fragment("Hello **world** & *friends* <script>alert(1)</script>");
        assert!(html.contains("<strong>world</strong>"));
        assert!(html.contains("<em>friends</em>"));
        assert!(html.contains("&amp;"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn converts_markdown_and_bare_links_and_blocks_unsafe_schemes() {
        let html = plain_to_html_fragment(
            "Check [Rust](https://www.rust-lang.org) or https://example.com/docs?a=1&b=2. Also [bad](javascript:alert(1)).",
        );
        assert!(html.contains(
            "<a href=\"https://www.rust-lang.org\" style=\"color: #2563eb; text-decoration: underline;\">Rust</a>"
        ));
        assert!(html.contains(
            "<a href=\"https://example.com/docs?a=1&amp;b=2\" style=\"color: #2563eb; text-decoration: underline;\">https://example.com/docs?a=1&amp;b=2</a>."
        ));
        assert!(!html.contains("href=\"javascript:"));
        assert!(html.contains("[bad](javascript:alert(1))"));
    }

    #[test]
    fn converts_bulleted_and_numbered_lists() {
        let input = "Items:\n\n- **First** item\n* *Second* item\n\nSteps:\n1. One\n2. Two\n";
        let html = plain_to_html_fragment(input);
        assert!(html.contains("<ul"));
        assert!(html.contains("<li><strong>First</strong> item</li>"));
        assert!(html.contains("<li><em>Second</em> item</li>"));
        assert!(html.contains("<ol"));
        assert!(html.contains("<li>One</li>"));
        assert!(html.contains("<li>Two</li>"));
    }

    #[test]
    fn converts_paragraphs_and_blockquotes() {
        let input = "Line 1\nLine 2\n\n> Quoted **text**\n> Second line\n\nClosing";
        let html = plain_to_html_fragment(input);
        assert!(html.contains("<p style=\"margin: 0 0 12px 0;\">Line 1<br>\nLine 2</p>"));
        assert!(html.contains("<blockquote"));
        assert!(html.contains("<strong>text</strong>"));
        assert!(html.contains("<p style=\"margin: 0 0 12px 0;\">Closing</p>"));
    }

    #[test]
    fn builds_reply_plain_and_html_with_quote_below_new_text() {
        let quote = QuotedOriginal {
            is_forward: false,
            from: "Alice <alice@example.com>".to_owned(),
            date: "2026-11-10 14:48".to_owned(),
            subject: "Project update".to_owned(),
            body: "Looks good!\nSee you tomorrow.".to_owned(),
        };
        let plain = build_email_plain("Thanks **Alice**!", Some(&quote));
        assert!(plain.starts_with("Thanks **Alice**!\n\nOn 2026-11-10 14:48, Alice <alice@example.com> wrote:\n"));
        assert!(plain.contains("> Looks good!\n> See you tomorrow.\n"));

        let html = build_email_html("Thanks **Alice**!", Some(&quote));
        let body_idx = html.find("<strong>Alice</strong>").unwrap();
        let quote_idx = html.find("<blockquote").unwrap();
        assert!(body_idx < quote_idx);
        assert!(html.contains("On 2026-11-10 14:48, Alice &lt;alice@example.com&gt; wrote:"));
    }
}
