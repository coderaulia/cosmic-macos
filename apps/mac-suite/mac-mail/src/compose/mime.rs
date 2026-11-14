// SPDX-License-Identifier: GPL-3.0-only

//! RFC 5322 / MIME message builder: `multipart/alternative` (plain text + HTML)
//! wrapped in `multipart/mixed` when attachments are present, with `In-Reply-To`
//! and `References` threading headers.

use std::path::Path;

use lettre::{
    Message,
    message::{Attachment, MultiPart, SinglePart, header::{self, ContentType}},
};

use super::recipients::EmailAddress;

/// An attachment loaded in memory, ready to embed into a MIME message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentData {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

impl AttachmentData {
    pub fn from_path(path: &Path) -> Result<Self, String> {
        let bytes =
            std::fs::read(path).map_err(|err| format!("Could not read {}: {err}", path.display()))?;
        let filename = path
            .file_name()
            .map_or_else(|| "attachment".to_owned(), |n| n.to_string_lossy().into_owned());
        let content_type = guess_mime_type(&filename).to_owned();
        Ok(Self {
            filename,
            content_type,
            bytes,
        })
    }
}

/// Complete outgoing message payload before MIME encoding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutgoingMessage {
    pub from: String,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,
    pub subject: String,
    pub plain_body: String,
    pub html_body: String,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub attachments: Vec<AttachmentData>,
    /// When `true` (saving to Drafts), empty `To`/`Cc`/`Bcc` are allowed.
    pub allow_empty_recipients: bool,
}

/// Built MIME message ready for SMTP transport and/or IMAP `APPEND`.
#[derive(Clone, Debug)]
pub struct BuiltMessage {
    /// Present whenever at least one recipient is specified (required by SMTP).
    pub message: Option<Message>,
    pub raw_rfc822: Vec<u8>,
    pub message_id: String,
}

/// Format a byte count into a human-readable string (`B`, `KB`, `MB`, `GB`).
pub fn format_byte_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < MB {
        format!("{:.1} KB", b / KB)
    } else if b < GB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.1} GB", b / GB)
    }
}

/// Guess a reasonable MIME `Content-Type` string from a filename extension.
pub fn guess_mime_type(filename: &str) -> &'static str {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("txt" | "log") => "text/plain; charset=utf-8",
        Some("md") => "text/markdown; charset=utf-8",
        Some("csv") => "text/csv; charset=utf-8",
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("json") => "application/json",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("zip") => "application/zip",
        Some("gz" | "gzip") => "application/gzip",
        Some("tar") => "application/x-tar",
        Some("doc") => "application/msword",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xls") => "application/vnd.ms-excel",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}

/// Generate a globally unique RFC 5322 `Message-ID` header value (`<id@domain>`).
pub fn generate_message_id(from_email: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let rand_part = fastrand_u64();
    let domain = from_email
        .split_once('@')
        .map(|(_, d)| d.trim().trim_end_matches('>'))
        .filter(|d| !d.is_empty())
        .unwrap_or("mac-mail.local");
    format!("<{nanos:x}.{rand_part:016x}.{pid:x}@{domain}>", pid = std::process::id())
}

fn fastrand_u64() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    nanos ^ (seq.wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

/// Build a `multipart/alternative` (plain text + HTML) MIME message, wrapped
/// in `multipart/mixed` when attachments are present.
pub fn build_mime_message(payload: &OutgoingMessage) -> Result<BuiltMessage, String> {
    let has_recipients = !payload.to.is_empty() || !payload.cc.is_empty() || !payload.bcc.is_empty();
    if !payload.allow_empty_recipients && !has_recipients {
        return Err("Add at least one recipient in To, Cc, or Bcc.".to_owned());
    }

    let from_mbox = super::recipients::parse_single_address(&payload.from)?
        .to_mailbox()
        .map_err(|_| "Your From address is not valid.".to_owned())?;
    let message_id = generate_message_id(from_mbox.email.as_ref());

    let mut builder = Message::builder()
        .from(from_mbox.clone())
        .message_id(Some(message_id.clone()))
        .subject(payload.subject.as_str());

    if has_recipients {
        for addr in &payload.to {
            builder = builder.to(addr.to_mailbox()?);
        }
        for addr in &payload.cc {
            builder = builder.cc(addr.to_mailbox()?);
        }
        for addr in &payload.bcc {
            builder = builder.bcc(addr.to_mailbox()?);
        }
    } else {
        // lettre's builder requires an envelope recipient; add a placeholder
        // and strip the To header after MIME formatting for recipientless drafts.
        builder = builder.to(from_mbox);
    }
    if let Some(in_reply_to) = payload.in_reply_to.as_ref().filter(|s| !s.trim().is_empty()) {
        builder = builder.in_reply_to(in_reply_to.trim().to_owned());
    }
    if let Some(references) = payload.references.as_ref().filter(|s| !s.trim().is_empty()) {
        builder = builder.references(references.trim().to_owned());
    }

    let alternative = MultiPart::alternative()
        .singlepart(SinglePart::plain(payload.plain_body.clone()))
        .singlepart(SinglePart::html(payload.html_body.clone()));

    let mut message = if payload.attachments.is_empty() {
        builder.multipart(alternative)
    } else {
        let mut mixed = MultiPart::mixed().multipart(alternative);
        for att in &payload.attachments {
            let content_type = ContentType::parse(&att.content_type)
                .unwrap_or_else(|_| ContentType::parse("application/octet-stream").unwrap());
            mixed = mixed.singlepart(Attachment::new(att.filename.clone()).body(att.bytes.clone(), content_type));
        }
        builder.multipart(mixed)
    }
    .map_err(|err| format!("Could not build message: {err}"))?;

    if !has_recipients {
        message.headers_mut().remove::<header::To>();
    }

    let raw_rfc822 = message.formatted();
    Ok(BuiltMessage {
        message: has_recipients.then_some(message),
        raw_rfc822,
        message_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_parser::MessageParser;

    fn sample_payload(with_attachment: bool) -> OutgoingMessage {
        OutgoingMessage {
            from: "Me <me@example.com>".to_owned(),
            to: vec![EmailAddress::new(Some("Alice".to_owned()), "alice@example.com")],
            cc: vec![EmailAddress::new(None, "bob@example.com")],
            bcc: vec![EmailAddress::new(None, "secret@example.com")],
            subject: "Re: Quarterly report".to_owned(),
            plain_body: "Hello **Alice**!".to_owned(),
            html_body: "<p>Hello <strong>Alice</strong>!</p>".to_owned(),
            in_reply_to: Some("<parent-1@example.com>".to_owned()),
            references: Some("<root-0@example.com> <parent-1@example.com>".to_owned()),
            attachments: if with_attachment {
                vec![AttachmentData {
                    filename: "report.pdf".to_owned(),
                    content_type: "application/pdf".to_owned(),
                    bytes: b"%PDF-1.4 sample".to_vec(),
                }]
            } else {
                Vec::new()
            },
            allow_empty_recipients: false,
        }
    }

    #[test]
    fn builds_multipart_alternative_with_threading_headers() {
        let built = build_mime_message(&sample_payload(false)).unwrap();
        let raw_str = String::from_utf8_lossy(&built.raw_rfc822);

        assert!(raw_str.contains("multipart/alternative"));
        assert!(!raw_str.contains("multipart/mixed"));
        assert!(raw_str.contains("In-Reply-To: <parent-1@example.com>"));
        assert!(raw_str.contains("References: <root-0@example.com> <parent-1@example.com>"));

        let parsed = MessageParser::new().parse(&built.raw_rfc822).unwrap();
        assert_eq!(parsed.subject(), Some("Re: Quarterly report"));
        assert_eq!(parsed.body_text(0).as_deref(), Some("Hello **Alice**!"));
        assert_eq!(
            parsed.body_html(0).as_deref(),
            Some("<p>Hello <strong>Alice</strong>!</p>")
        );
        assert_eq!(parsed.attachments().count(), 0);
    }

    #[test]
    fn builds_multipart_mixed_when_attachments_present() {
        let built = build_mime_message(&sample_payload(true)).unwrap();
        let raw_str = String::from_utf8_lossy(&built.raw_rfc822);

        assert!(raw_str.contains("multipart/mixed"));
        assert!(raw_str.contains("multipart/alternative"));

        let parsed = MessageParser::new().parse(&built.raw_rfc822).unwrap();
        assert_eq!(parsed.body_text(0).as_deref(), Some("Hello **Alice**!"));
        assert_eq!(
            parsed.body_html(0).as_deref(),
            Some("<p>Hello <strong>Alice</strong>!</p>")
        );
        let atts: Vec<_> = parsed.attachments().collect();
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].attachment_name(), Some("report.pdf"));
        assert_eq!(atts[0].decoded().as_ref(), b"%PDF-1.4 sample");
    }

    #[test]
    fn rejects_send_without_recipients_but_allows_draft() {
        let mut payload = sample_payload(false);
        payload.to.clear();
        payload.cc.clear();
        payload.bcc.clear();
        assert!(build_mime_message(&payload).is_err());

        payload.allow_empty_recipients = true;
        let draft = build_mime_message(&payload).unwrap();
        assert!(draft.message.is_none());
        let parsed = MessageParser::new().parse(&draft.raw_rfc822).unwrap();
        assert!(parsed.to().is_none());
        assert_eq!(parsed.subject(), Some("Re: Quarterly report"));
    }

    #[test]
    fn formats_byte_sizes_human_readably() {
        assert_eq!(format_byte_size(0), "0 B");
        assert_eq!(format_byte_size(512), "512 B");
        assert_eq!(format_byte_size(1536), "1.5 KB");
        assert_eq!(format_byte_size(2 * 1024 * 1024), "2.0 MB");
        assert_eq!(format_byte_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
