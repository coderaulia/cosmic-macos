// SPDX-License-Identifier: GPL-3.0-only

//! Talking to the mail server. Every function blocks, so the UI runs them on
//! a worker thread.

use imap::{Session, types::Flag};
use lettre::{
    Message, SmtpTransport, Transport, transport::smtp::authentication::Credentials,
};
use mail_parser::MessageParser;

use crate::account::Account;

type Result<T> = std::result::Result<T, String>;

/// How many of the newest messages a folder shows.
const PAGE: u32 = 60;

#[derive(Clone, Debug)]
pub struct Summary {
    pub uid: u32,
    pub from: String,
    /// Where a reply goes: the Reply-To address, else the sender's.
    pub address: String,
    pub subject: String,
    pub date: String,
    pub unread: bool,
}

fn connect(account: &Account, password: &str) -> Result<Session<imap::Connection>> {
    let client = imap::ClientBuilder::new(account.imap_host.as_str(), account.imap_port)
        .connect()
        .map_err(|err| format!("Could not reach {}: {err}", account.imap_host))?;
    client
        .login(&account.email, password)
        .map_err(|(err, _)| format!("Sign-in failed: {err}"))
}

/// Folder names the server offers, with INBOX first.
pub fn folders(account: &Account, password: &str) -> Result<Vec<String>> {
    let mut session = connect(account, password)?;
    let names = session.list(None, Some("*")).map_err(|err| err.to_string())?;
    let mut folders: Vec<String> = names
        .iter()
        .filter(|name| {
            !name
                .attributes()
                .iter()
                // The attribute type is not re-exported, so compare by name.
                .any(|attribute| format!("{attribute:?}").contains("NoSelect"))
        })
        .map(|name| name.name().to_owned())
        .collect();
    let _ = session.logout();
    folders.sort_by_key(|name| (!name.eq_ignore_ascii_case("INBOX"), name.to_lowercase()));
    Ok(folders)
}

/// The newest messages of a folder, newest first.
pub fn headers(account: &Account, password: &str, folder: &str) -> Result<Vec<Summary>> {
    let mut session = connect(account, password)?;
    let mailbox = session.select(folder).map_err(|err| err.to_string())?;
    if mailbox.exists == 0 {
        let _ = session.logout();
        return Ok(Vec::new());
    }
    let first = mailbox.exists.saturating_sub(PAGE - 1).max(1);
    let fetched = session
        .fetch(format!("{first}:{}", mailbox.exists), "(UID FLAGS RFC822.HEADER)")
        .map_err(|err| err.to_string())?;
    let mut summaries = summarize(&fetched);
    let _ = session.logout();
    summaries.sort_by(|a, b| b.uid.cmp(&a.uid));
    Ok(summaries)
}

/// Turn fetched headers into list entries.
fn summarize(fetched: &imap::types::Fetches) -> Vec<Summary> {
    let parser = MessageParser::new();
    fetched
        .iter()
        .filter_map(|fetch| {
            let uid = fetch.uid?;
            let message = parser.parse(fetch.header()?)?;
            let from = message
                .from()
                .and_then(|list| list.first())
                .map(|mailbox| {
                    mailbox
                        .name()
                        .filter(|name| !name.is_empty())
                        .or(mailbox.address())
                        .unwrap_or_default()
                        .to_owned()
                })
                .unwrap_or_default();
            let address = message
                .reply_to()
                .or_else(|| message.from())
                .and_then(|list| list.first())
                .and_then(|mailbox| mailbox.address())
                .unwrap_or_default()
                .to_owned();
            let date = message
                .date()
                .map(|d| format!("{:04}-{:02}-{:02} {:02}:{:02}", d.year, d.month, d.day, d.hour, d.minute))
                .unwrap_or_default();
            Some(Summary {
                uid,
                from,
                address,
                subject: message.subject().unwrap_or("(no subject)").to_owned(),
                date,
                unread: !fetch.flags().iter().any(|flag| matches!(flag, Flag::Seen)),
            })
        })
        .collect()
}

/// Messages of a folder whose text, subject or sender contain `query`.
pub fn search(account: &Account, password: &str, folder: &str, query: &str) -> Result<Vec<Summary>> {
    // Quotes and backslashes would end the search string early.
    let query: String = query.chars().filter(|c| !matches!(c, '"' | '\\')).collect();
    let mut session = connect(account, password)?;
    session.select(folder).map_err(|err| err.to_string())?;
    let uids = session
        .uid_search(format!("TEXT \"{query}\""))
        .map_err(|err| err.to_string())?;
    if uids.is_empty() {
        let _ = session.logout();
        return Ok(Vec::new());
    }
    let mut uids: Vec<u32> = uids.into_iter().collect();
    uids.sort_unstable_by(|a, b| b.cmp(a));
    uids.truncate(PAGE as usize);
    let set = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let fetched = session
        .uid_fetch(set, "(UID FLAGS RFC822.HEADER)")
        .map_err(|err| err.to_string())?;
    let mut summaries = summarize(&fetched);
    let _ = session.logout();
    summaries.sort_by(|a, b| b.uid.cmp(&a.uid));
    Ok(summaries)
}

/// Move a message to `target`, or delete it for good when there is none.
pub fn move_message(
    account: &Account,
    password: &str,
    folder: &str,
    uid: u32,
    target: Option<&str>,
) -> Result<()> {
    let mut session = connect(account, password)?;
    session.select(folder).map_err(|err| err.to_string())?;
    if let Some(target) = target {
        session
            .uid_copy(uid.to_string(), format!("\"{target}\""))
            .map_err(|err| err.to_string())?;
    }
    session
        .uid_store(uid.to_string(), "+FLAGS (\\Deleted)")
        .map_err(|err| err.to_string())?;
    session.expunge().map_err(|err| err.to_string())?;
    let _ = session.logout();
    Ok(())
}

/// An opened message: its parsed content, plain-text fallback, and attachment names.
#[derive(Clone, Debug)]
pub struct Opened {
    #[allow(dead_code)]
    pub text: String,
    pub attachments: Vec<String>,
    pub content: mac_mail::html_mail::MailContent,
}

/// Fetch the raw message.
fn raw_message(session: &mut Session<imap::Connection>, uid: u32) -> Result<Vec<u8>> {
    let fetched = session
        .uid_fetch(uid.to_string(), "BODY.PEEK[]")
        .map_err(|err| err.to_string())?;
    fetched
        .iter()
        .next()
        .and_then(|fetch| fetch.body())
        .map(<[u8]>::to_vec)
        .ok_or_else(|| "The message could not be read".to_owned())
}

/// Save attachment number `index` of a message to the Downloads folder.
pub fn save_attachment(
    account: &Account,
    password: &str,
    folder: &str,
    uid: u32,
    index: usize,
) -> Result<String> {
    let mut session = connect(account, password)?;
    session.select(folder).map_err(|err| err.to_string())?;
    let raw = raw_message(&mut session, uid)?;
    let _ = session.logout();
    let message = MessageParser::new()
        .parse(&raw)
        .ok_or("The message could not be read")?;
    let part = message
        .attachments()
        .nth(index)
        .ok_or("Attachment not found")?;
    let name = part
        .attachment_name()
        .unwrap_or("attachment")
        .replace(['/', '\\'], "_");
    let dir = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or("No home folder")?
        .join("Downloads");
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    // Never overwrite an existing file.
    let mut path = dir.join(&name);
    let mut n = 1;
    while path.exists() {
        n += 1;
        path = dir.join(format!("{n}-{name}"));
    }
    std::fs::write(&path, part.decoded()).map_err(|err| err.to_string())?;
    Ok(path.display().to_string())
}

/// The text of one message; also marks it read.
pub fn body(account: &Account, password: &str, folder: &str, uid: u32) -> Result<Opened> {
    let mut session = connect(account, password)?;
    session.select(folder).map_err(|err| err.to_string())?;
    let fetched = session
        .uid_fetch(uid.to_string(), "BODY.PEEK[]")
        .map_err(|err| err.to_string())?;
    let opened = fetched
        .iter()
        .next()
        .and_then(|fetch| fetch.body())
        .map(|raw| {
            let content = mac_mail::html_mail::MailContent::from_raw_mime(raw);
            let text = if content.plain_text.is_empty() {
                "(this message has no text)".to_owned()
            } else {
                content.plain_text.clone()
            };
            let attachments = MessageParser::new()
                .parse(raw)
                .map(|message| {
                    message
                        .attachments()
                        .map(|part| part.attachment_name().unwrap_or("attachment").to_owned())
                        .collect()
                })
                .unwrap_or_default();
            Opened {
                text,
                attachments,
                content,
            }
        })
        .unwrap_or_else(|| Opened {
            text: "(this message has no text)".to_owned(),
            attachments: Vec::new(),
            content: mac_mail::html_mail::MailContent::from_plain_text("(this message has no text)"),
        });
    let _ = session.uid_store(uid.to_string(), "+FLAGS (\\Seen)");
    let _ = session.logout();
    Ok(opened)
}

pub fn send(
    account: &Account,
    password: &str,
    to: &str,
    subject: &str,
    body: &str,
    attachments: &[String],
) -> Result<()> {
    use lettre::message::{Attachment, MultiPart, SinglePart, header::ContentType};

    let builder = Message::builder()
        .from(account.email.parse().map_err(|_| "Your address is not valid".to_owned())?)
        .to(to.parse().map_err(|_| "The recipient address is not valid".to_owned())?)
        .subject(subject);
    let message = if attachments.is_empty() {
        builder.body(body.to_owned())
    } else {
        let mut parts = MultiPart::mixed().singlepart(SinglePart::plain(body.to_owned()));
        for path in attachments {
            let data = std::fs::read(path).map_err(|err| format!("Could not read {path}: {err}"))?;
            let name = std::path::Path::new(path)
                .file_name()
                .map_or_else(|| "attachment".to_owned(), |n| n.to_string_lossy().into_owned());
            let content_type = ContentType::parse("application/octet-stream")
                .map_err(|err| err.to_string())?;
            parts = parts.singlepart(Attachment::new(name).body(data, content_type));
        }
        builder.multipart(parts)
    }
    .map_err(|err| err.to_string())?;
    let builder = if account.smtp_port == 465 {
        SmtpTransport::relay(&account.smtp_host)
    } else {
        SmtpTransport::starttls_relay(&account.smtp_host)
    }
    .map_err(|err| err.to_string())?;
    builder
        .port(account.smtp_port)
        .credentials(Credentials::new(account.email.clone(), password.to_owned()))
        .build()
        .send(&message)
        .map(|_| ())
        .map_err(|err| format!("Could not send: {err}"))
}
