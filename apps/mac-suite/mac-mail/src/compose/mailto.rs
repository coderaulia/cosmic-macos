// SPDX-License-Identifier: GPL-3.0-only

//! RFC 6068 `mailto:` URI parser for CLI launch arguments.

/// Parsed fields from a `mailto:` URI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailtoUri {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
}

/// Find and parse the first `mailto:` URI in a CLI argument iterator.
pub fn extract_mailto_from_args<I, S>(args: I) -> Option<MailtoUri>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .find_map(|arg| parse_mailto_uri(arg.as_ref()))
}

/// Parse a `mailto:` URI (RFC 6068), decoding `to`, `cc`, `bcc`, `subject`, and `body`.
pub fn parse_mailto_uri(raw: &str) -> Option<MailtoUri> {
    let trimmed = raw.trim();
    let rest = trimmed
        .get(..7)
        .filter(|prefix| prefix.eq_ignore_ascii_case("mailto:"))
        .map(|_| &trimmed[7..])?;

    let (raw_path, raw_query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };

    let mut to_parts = Vec::new();
    let path_decoded = percent_decode(raw_path);
    for addr in path_decoded.split(',') {
        let clean = addr.trim();
        if !clean.is_empty() {
            to_parts.push(clean.to_owned());
        }
    }

    let mut cc_parts = Vec::new();
    let mut bcc_parts = Vec::new();
    let mut subject = String::new();
    let mut body = String::new();

    if let Some(query) = raw_query {
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (raw_k, raw_v) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => (pair, ""),
            };
            let key = percent_decode(raw_k).trim().to_ascii_lowercase();
            let val = percent_decode(raw_v);
            match key.as_str() {
                "to" => {
                    for addr in val.split(',') {
                        let clean = addr.trim();
                        if !clean.is_empty() {
                            to_parts.push(clean.to_owned());
                        }
                    }
                }
                "cc" => {
                    for addr in val.split(',') {
                        let clean = addr.trim();
                        if !clean.is_empty() {
                            cc_parts.push(clean.to_owned());
                        }
                    }
                }
                "bcc" => {
                    for addr in val.split(',') {
                        let clean = addr.trim();
                        if !clean.is_empty() {
                            bcc_parts.push(clean.to_owned());
                        }
                    }
                }
                "subject" => subject = val,
                "body" => body = val,
                _ => {}
            }
        }
    }

    Some(MailtoUri {
        to: to_parts.join(", "),
        cc: cc_parts.join(", "),
        bcc: bcc_parts.join(", "),
        subject,
        body,
    })
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mailto_path_and_query_parameters() {
        let parsed = parse_mailto_uri(
            "MAILTO:alice%2Bwork@example.com,bob@example.com?cc=carol@example.com&bcc=dave@example.com&subject=Hello%20World&body=Line%201%0ALine%202",
        )
        .unwrap();
        assert_eq!(parsed.to, "alice+work@example.com, bob@example.com");
        assert_eq!(parsed.cc, "carol@example.com");
        assert_eq!(parsed.bcc, "dave@example.com");
        assert_eq!(parsed.subject, "Hello World");
        assert_eq!(parsed.body, "Line 1\nLine 2");
    }

    #[test]
    fn parses_mailto_with_to_query_param_and_ignores_non_mailto_args() {
        let args = [
            "mac-mail",
            "--flag",
            "mailto:?to=team@example.org&subject=Status%20%26%20Plan",
        ];
        let parsed = extract_mailto_from_args(args).unwrap();
        assert_eq!(parsed.to, "team@example.org");
        assert_eq!(parsed.subject, "Status & Plan");
        assert!(extract_mailto_from_args(["mac-mail", "https://example.com"]).is_none());
    }
}
