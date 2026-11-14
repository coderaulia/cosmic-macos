// SPDX-License-Identifier: GPL-3.0-only

//! Email address parsing, Reply / Reply-All recipient selection, subject
//! prefixing, and RFC 5322 threading headers (`In-Reply-To` / `References`).

use std::{collections::HashSet, fmt};

use lettre::message::Mailbox;

/// A parsed email address with an optional display name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EmailAddress {
    pub name: Option<String>,
    pub address: String,
}

impl EmailAddress {
    pub fn new(name: Option<String>, address: impl Into<String>) -> Self {
        let name = name
            .map(|n| n.trim().trim_matches('"').to_owned())
            .filter(|n| !n.is_empty());
        Self {
            name,
            address: address.into().trim().to_owned(),
        }
    }

    pub fn normalized_address(&self) -> String {
        self.address.trim().to_ascii_lowercase()
    }

    /// Format as `Name <addr>` (quoting name if it contains commas) or `addr`.
    pub fn format_display(&self) -> String {
        match &self.name {
            Some(name) if !name.is_empty() => {
                if name.contains([',', ';', '<', '>', '"']) {
                    let escaped = name.replace('"', "\\\"");
                    format!("\"{escaped}\" <{}>", self.address)
                } else {
                    format!("{name} <{}>", self.address)
                }
            }
            _ => self.address.clone(),
        }
    }

    /// Convert to a `lettre::message::Mailbox`.
    pub fn to_mailbox(&self) -> Result<Mailbox, String> {
        let addr: lettre::Address = self
            .address
            .trim()
            .parse()
            .map_err(|_| format!("Invalid email address: {}", self.address))?;
        Ok(Mailbox::new(self.name.clone(), addr))
    }
}

impl fmt::Display for EmailAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format_display())
    }
}

/// Split a comma-separated recipient string while respecting quoted display names.
pub fn split_address_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut angle_depth = 0usize;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => {
                current.push(ch);
                escaped = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            '<' if !in_quotes => {
                angle_depth += 1;
                current.push(ch);
            }
            '>' if !in_quotes => {
                angle_depth = angle_depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if !in_quotes && angle_depth == 0 => {
                tokens.push(current.trim().to_owned());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let tail = current.trim();
    if !tail.is_empty() || !tokens.is_empty() {
        tokens.push(tail.to_owned());
    }
    tokens
}

/// Parse a single address token such as `Alice <alice@example.com>` or `alice@example.com`.
pub fn parse_single_address(token: &str) -> Result<EmailAddress, String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err("Empty email address".to_owned());
    }
    if let Ok(mbox) = trimmed.parse::<Mailbox>() {
        return Ok(EmailAddress::new(mbox.name, mbox.email.to_string()));
    }
    // Fallback for lightly malformed display names like `Alice (Work) <alice@example.com>`
    if let (Some(open), Some(close)) = (trimmed.rfind('<'), trimmed.rfind('>'))
        && open < close
    {
        let addr_str = trimmed[open + 1..close].trim();
        let _: lettre::Address = addr_str
            .parse()
            .map_err(|_| format!("Invalid email address: {trimmed}"))?;
        let name = trimmed[..open].trim();
        return Ok(EmailAddress::new(
            (!name.is_empty()).then(|| name.to_owned()),
            addr_str,
        ));
    }
    Err(format!("Invalid email address: {trimmed}"))
}

/// Parse a comma-separated list of recipient addresses.
pub fn parse_address_list(input: &str) -> Result<Vec<EmailAddress>, String> {
    let mut out = Vec::new();
    for token in split_address_tokens(input) {
        if token.is_empty() {
            continue;
        }
        out.push(parse_single_address(&token)?);
    }
    Ok(out)
}

/// Format a slice of `EmailAddress` items into a comma-separated header field value.
pub fn format_address_list(addrs: &[EmailAddress]) -> String {
    addrs
        .iter()
        .map(EmailAddress::format_display)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Original message metadata and body needed to initialize Reply, Reply All, or Forward.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OriginalMessage {
    pub uid: u32,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub from: Vec<EmailAddress>,
    pub reply_to: Vec<EmailAddress>,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub subject: String,
    pub date: String,
    pub body_text: String,
}

impl OriginalMessage {
    pub fn primary_sender_display(&self) -> String {
        self.from
            .first()
            .map(EmailAddress::format_display)
            .unwrap_or_default()
    }
}

/// Computed `To` and `Cc` recipient lists for a Reply or Reply All.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplyRecipients {
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
}

/// Compute recipients for a standard Reply (`Reply-To` if present, otherwise `From`).
pub fn reply_recipients(original: &OriginalMessage, own_email: &str) -> ReplyRecipients {
    let own = own_email.trim().to_ascii_lowercase();
    let base = if !original.reply_to.is_empty() {
        &original.reply_to
    } else {
        &original.from
    };

    let mut seen = HashSet::new();
    let mut to = Vec::new();
    for addr in base {
        let norm = addr.normalized_address();
        if !norm.is_empty() && seen.insert(norm) {
            to.push(addr.clone());
        }
    }

    // If replying to a message you sent yourself (e.g. from Sent) with no Reply-To,
    // prefer the original `To` recipients when available.
    if original.reply_to.is_empty()
        && !own.is_empty()
        && to.iter().all(|a| a.normalized_address() == own)
    {
        let others: Vec<EmailAddress> = original
            .to
            .iter()
            .filter(|a| !a.address.trim().is_empty() && a.normalized_address() != own)
            .cloned()
            .collect();
        if !others.is_empty() {
            to = others;
        }
    }

    ReplyRecipients { to, cc: Vec::new() }
}

/// Compute `To` and `Cc` recipients for Reply All, stripping `own_email` and
/// deduplicating addresses case-insensitively across `To` and `Cc`.
pub fn reply_all_recipients(original: &OriginalMessage, own_email: &str) -> ReplyRecipients {
    let own = own_email.trim().to_ascii_lowercase();
    let mut seen = HashSet::new();
    if !own.is_empty() {
        seen.insert(own);
    }

    let mut to = Vec::new();
    let primary_reply = if !original.reply_to.is_empty() {
        &original.reply_to
    } else {
        &original.from
    };
    for addr in primary_reply.iter().chain(original.to.iter()) {
        let norm = addr.normalized_address();
        if !norm.is_empty() && seen.insert(norm) {
            to.push(addr.clone());
        }
    }

    let mut cc = Vec::new();
    for addr in &original.cc {
        let norm = addr.normalized_address();
        if !norm.is_empty() && seen.insert(norm) {
            cc.push(addr.clone());
        }
    }

    ReplyRecipients { to, cc }
}

/// Compute `Re:` or `Fwd:` prefixed subject without duplicating an existing prefix.
pub fn reply_subject(subject: &str, is_forward: bool) -> String {
    let trimmed = subject.trim();
    if is_forward {
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("fwd:") || lower.starts_with("fw:") {
            trimmed.to_owned()
        } else if trimmed.is_empty() {
            "Fwd:".to_owned()
        } else {
            format!("Fwd: {trimmed}")
        }
    } else if trimmed.to_ascii_lowercase().starts_with("re:") {
        trimmed.to_owned()
    } else if trimmed.is_empty() {
        "Re:".to_owned()
    } else {
        format!("Re: {trimmed}")
    }
}

/// Normalize a Message-ID into `<id>` form, returning `None` if empty.
pub fn normalize_message_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let inner = trimmed.trim_start_matches('<').trim_end_matches('>').trim();
    if inner.is_empty() {
        None
    } else {
        Some(format!("<{inner}>"))
    }
}

/// Threading headers computed for a reply (`In-Reply-To` and `References`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadingHeaders {
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
}

/// Compute `In-Reply-To` and `References` headers for a reply to `original`.
pub fn threading_headers(original: &OriginalMessage) -> ThreadingHeaders {
    let parent_id = original
        .message_id
        .as_deref()
        .and_then(normalize_message_id);

    let mut refs = Vec::new();
    let mut seen = HashSet::new();

    for r in &original.references {
        if let Some(norm) = normalize_message_id(r)
            && seen.insert(norm.clone())
        {
            refs.push(norm);
        }
    }

    if refs.is_empty()
        && let Some(irt) = original
            .in_reply_to
            .as_deref()
            .and_then(normalize_message_id)
        && seen.insert(irt.clone())
    {
        refs.push(irt);
    }

    if let Some(ref pid) = parent_id
        && seen.insert(pid.clone())
    {
        refs.push(pid.clone());
    }

    ThreadingHeaders {
        in_reply_to: parent_id,
        references: (!refs.is_empty()).then(|| refs.join(" ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(name: Option<&str>, email: &str) -> EmailAddress {
        EmailAddress::new(name.map(str::to_owned), email)
    }

    #[test]
    fn parses_quoted_and_comma_separated_addresses() {
        let parsed = parse_address_list(
            "\"Doe, Alice\" <alice@example.com>, bob@example.com, Carol Smith <carol@example.org>",
        )
        .unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].name.as_deref(), Some("Doe, Alice"));
        assert_eq!(parsed[0].address, "alice@example.com");
        assert_eq!(parsed[1].name, None);
        assert_eq!(parsed[1].address, "bob@example.com");
        assert_eq!(parsed[2].name.as_deref(), Some("Carol Smith"));
        assert_eq!(parsed[2].address, "carol@example.org");
    }

    #[test]
    fn reply_all_strips_own_email_and_deduplicates_case_insensitively() {
        let orig = OriginalMessage {
            from: vec![addr(Some("Alice"), "alice@example.com")],
            reply_to: vec![],
            to: vec![
                addr(Some("Me"), "ME@Example.com"),
                addr(Some("Bob"), "bob@example.com"),
                addr(Some("Alice Dup"), "ALICE@example.com"),
            ],
            cc: vec![
                addr(Some("Bob"), "BOB@example.com"),
                addr(Some("Carol"), "carol@example.com"),
                addr(None, "me@example.com"),
            ],
            ..Default::default()
        };

        let result = reply_all_recipients(&orig, "me@example.com");
        assert_eq!(
            result
                .to
                .iter()
                .map(|a| a.address.as_str())
                .collect::<Vec<_>>(),
            vec!["alice@example.com", "bob@example.com"]
        );
        assert_eq!(
            result
                .cc
                .iter()
                .map(|a| a.address.as_str())
                .collect::<Vec<_>>(),
            vec!["carol@example.com"]
        );
    }

    #[test]
    fn reply_all_uses_reply_to_when_present() {
        let orig = OriginalMessage {
            from: vec![addr(Some("NoReply"), "noreply@example.com")],
            reply_to: vec![addr(Some("Support"), "support@example.com")],
            to: vec![addr(None, "me@example.com"), addr(None, "team@example.com")],
            cc: vec![addr(None, "observer@example.com")],
            ..Default::default()
        };

        let result = reply_all_recipients(&orig, "me@example.com");
        assert_eq!(
            result
                .to
                .iter()
                .map(|a| a.address.as_str())
                .collect::<Vec<_>>(),
            vec!["support@example.com", "team@example.com"]
        );
        assert_eq!(
            result
                .cc
                .iter()
                .map(|a| a.address.as_str())
                .collect::<Vec<_>>(),
            vec!["observer@example.com"]
        );
    }

    #[test]
    fn reply_to_own_sent_message_targets_original_recipients() {
        let orig = OriginalMessage {
            from: vec![addr(None, "me@example.com")],
            to: vec![addr(Some("Alice"), "alice@example.com")],
            ..Default::default()
        };
        let result = reply_recipients(&orig, "me@example.com");
        assert_eq!(result.to.len(), 1);
        assert_eq!(result.to[0].address, "alice@example.com");
    }

    #[test]
    fn computes_threading_headers_preserving_order_and_appending_parent() {
        let orig = OriginalMessage {
            message_id: Some("msg-3@example.com".to_owned()),
            references: vec![
                "<msg-1@example.com>".to_owned(),
                "msg-2@example.com".to_owned(),
            ],
            ..Default::default()
        };
        let headers = threading_headers(&orig);
        assert_eq!(headers.in_reply_to.as_deref(), Some("<msg-3@example.com>"));
        assert_eq!(
            headers.references.as_deref(),
            Some("<msg-1@example.com> <msg-2@example.com> <msg-3@example.com>")
        );
    }

    #[test]
    fn threading_headers_fall_back_to_in_reply_to_when_references_empty() {
        let orig = OriginalMessage {
            message_id: Some("<msg-2@example.com>".to_owned()),
            in_reply_to: Some("<msg-1@example.com>".to_owned()),
            references: vec![],
            ..Default::default()
        };
        let headers = threading_headers(&orig);
        assert_eq!(headers.in_reply_to.as_deref(), Some("<msg-2@example.com>"));
        assert_eq!(
            headers.references.as_deref(),
            Some("<msg-1@example.com> <msg-2@example.com>")
        );
    }

    #[test]
    fn subject_prefix_does_not_duplicate() {
        assert_eq!(reply_subject("Hello", false), "Re: Hello");
        assert_eq!(reply_subject("Re: Hello", false), "Re: Hello");
        assert_eq!(reply_subject("re: Hello", false), "re: Hello");
        assert_eq!(reply_subject("Hello", true), "Fwd: Hello");
        assert_eq!(reply_subject("Fwd: Hello", true), "Fwd: Hello");
    }
}
