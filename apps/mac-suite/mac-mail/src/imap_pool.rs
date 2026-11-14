// SPDX-License-Identifier: GPL-3.0-only

//! Persistent IMAP session manager and background INBOX IDLE watcher.
//!
//! Keeps one authenticated IMAP connection open across folder switches, message
//! fetches, flag updates, and moves, reconnecting automatically if the server
//! closes an idle socket or times out. A separate dedicated connection runs
//! `IDLE` on `INBOX` so real-time new-mail alerts never block interactive clicks.

use std::{
    io,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use imap::{
    Session,
    extensions::idle::WaitOutcome,
    types::{Appended, Mailbox, UnsolicitedResponse},
};
use mail_parser::MessageParser;

use crate::{
    account::{self, Account, AccountCredential},
    oauth::XOAuth2Authenticator,
};

/// How long a pooled connection can sit idle before a `NOOP` health check is sent.
const KEEPALIVE_CHECK_AFTER: Duration = Duration::from_secs(45);
/// How long each `IDLE` cycle waits before waking to check the stop flag and refresh.
const IDLE_CYCLE_TIMEOUT: Duration = Duration::from_secs(25);
/// Polling interval when a server does not advertise `IDLE`.
const POLL_FALLBACK_INTERVAL: Duration = Duration::from_secs(30);

/// Capabilities detected on the IMAP server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServerCapabilities {
    pub has_idle: bool,
    pub has_move: bool,
    pub has_uidplus: bool,
}

/// Information passed when new mail arrives in `INBOX`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMailNotification {
    pub account_email: String,
    pub folder: String,
    pub uid: Option<u32>,
    pub from: String,
    pub subject: String,
    pub total_exists: u32,
}

struct PoolState {
    account: Account,
    credential: AccountCredential,
    session: Option<Session<imap::Connection>>,
    selected_folder: Option<String>,
    last_mailbox: Option<Mailbox>,
    capabilities: ServerCapabilities,
    last_activity: Instant,
}

/// Thread-safe persistent IMAP session holder with automatic reconnect.
#[derive(Clone)]
pub struct ImapPool {
    inner: Arc<Mutex<PoolState>>,
}

impl ImapPool {
    pub fn new(account: Account, credential: AccountCredential) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolState {
                account,
                credential,
                session: None,
                selected_folder: None,
                last_mailbox: None,
                capabilities: ServerCapabilities::default(),
                last_activity: Instant::now(),
            })),
        }
    }

    pub fn account(&self) -> Account {
        self.inner
            .lock()
            .expect("imap pool mutex poisoned")
            .account
            .clone()
    }

    pub fn credential(&self) -> AccountCredential {
        self.inner
            .lock()
            .expect("imap pool mutex poisoned")
            .credential
            .clone()
    }

    /// Replace credentials and drop any active connection so the next operation re-authenticates.
    pub fn set_credentials(&self, account: Account, credential: AccountCredential) {
        let mut guard = self.inner.lock().expect("imap pool mutex poisoned");
        if let Some(mut session) = guard.session.take() {
            let _ = session.logout();
        }
        guard.account = account;
        guard.credential = credential;
        guard.selected_folder = None;
        guard.last_mailbox = None;
        guard.capabilities = ServerCapabilities::default();
    }

    /// Whether an authenticated IMAP session is currently open.
    pub fn is_connected(&self) -> bool {
        self.inner
            .lock()
            .expect("imap pool mutex poisoned")
            .session
            .is_some()
    }

    /// Cached server capabilities (populated after first connect).
    pub fn capabilities(&self) -> ServerCapabilities {
        self.inner
            .lock()
            .expect("imap pool mutex poisoned")
            .capabilities
    }

    /// Cleanly close the persistent IMAP session.
    pub fn disconnect(&self) {
        let mut guard = self.inner.lock().expect("imap pool mutex poisoned");
        if let Some(mut session) = guard.session.take() {
            let _ = session.logout();
        }
        guard.selected_folder = None;
        guard.last_mailbox = None;
    }

    /// Clear the cached selected folder so the next call re-`SELECT`s it.
    pub fn invalidate_selected(&self) {
        let mut guard = self.inner.lock().expect("imap pool mutex poisoned");
        guard.selected_folder = None;
        guard.last_mailbox = None;
    }

    /// Run an IMAP operation using the persistent session.
    ///
    /// - Reuses the open session if healthy (issuing `NOOP` if idle >45s).
    /// - Only `SELECT`s `folder` if not already selected (unless `force_select` is true).
    /// - If the operation fails due to a dropped/timed-out connection, reconnects once and retries.
    pub fn with_session<T, F>(
        &self,
        folder: Option<&str>,
        force_select: bool,
        mut op: F,
    ) -> Result<T, String>
    where
        F: FnMut(&mut Session<imap::Connection>, Option<&Mailbox>, ServerCapabilities) -> imap::Result<T>,
    {
        let mut guard = self.inner.lock().expect("imap pool mutex poisoned");

        for attempt in 0..2 {
            ensure_connected(&mut guard)?;

            let need_select = match folder {
                Some(target) => {
                    force_select || guard.selected_folder.as_deref() != Some(target)
                }
                None => false,
            };

            if need_select && let Some(target) = folder {
                let select_res = {
                    let session = guard.session.as_mut().expect("session connected");
                    session.select(target)
                };
                match select_res {
                    Ok(mailbox) => {
                        guard.selected_folder = Some(target.to_owned());
                        guard.last_mailbox = Some(mailbox);
                        guard.last_activity = Instant::now();
                    }
                    Err(err) if attempt == 0 && is_connection_error(&err) => {
                        drop_dead_session(&mut guard);
                        continue;
                    }
                    Err(err) => return Err(format_imap_error(&err)),
                }
            }

            let caps = guard.capabilities;
            let mailbox_snapshot = if folder.is_some() {
                guard.last_mailbox.clone()
            } else {
                None
            };
            let op_res = {
                let session = guard.session.as_mut().expect("session connected");
                op(session, mailbox_snapshot.as_ref(), caps)
            };

            match op_res {
                Ok(val) => {
                    guard.last_activity = Instant::now();
                    return Ok(val);
                }
                Err(err) if attempt == 0 && is_connection_error(&err) => {
                    drop_dead_session(&mut guard);
                    continue;
                }
                Err(err) => return Err(format_imap_error(&err)),
            }
        }

        Err("IMAP connection lost after retry".to_owned())
    }
}

fn ensure_connected(guard: &mut PoolState) -> Result<(), String> {
    if let Some(session) = guard.session.as_mut() {
        if guard.last_activity.elapsed() >= KEEPALIVE_CHECK_AFTER {
            if let Err(err) = session.noop() {
                tracing::debug!("IMAP keepalive noop failed ({err}); reconnecting");
                drop_dead_session(guard);
            } else {
                guard.last_activity = Instant::now();
                return Ok(());
            }
        } else {
            return Ok(());
        }
    }

    account::ensure_fresh_credential(&guard.account, &mut guard.credential)?;
    let (session, caps) = connect_and_probe(&guard.account, &guard.credential)?;
    guard.session = Some(session);
    guard.selected_folder = None;
    guard.last_mailbox = None;
    guard.capabilities = caps;
    guard.last_activity = Instant::now();
    Ok(())
}

fn drop_dead_session(guard: &mut PoolState) {
    guard.session = None;
    guard.selected_folder = None;
    guard.last_mailbox = None;
}

pub(crate) fn connect_session(
    account: &Account,
    credential: &AccountCredential,
) -> Result<Session<imap::Connection>, String> {
    let client = imap::ClientBuilder::new(account.imap_host.as_str(), account.imap_port)
        .connect()
        .map_err(|err| format!("Could not reach {}: {err}", account.imap_host))?;
    match credential {
        AccountCredential::Password(password) => client
            .login(&account.email, password)
            .map_err(|(err, _)| format!("Sign-in failed: {err}")),
        AccountCredential::OAuth2(tokens) => {
            let auth = XOAuth2Authenticator {
                user: account.email.clone(),
                access_token: tokens.access_token.clone(),
            };
            client
                .authenticate("XOAUTH2", &auth)
                .map_err(|(err, _)| format!("XOAUTH2 sign-in failed: {err}"))
        }
    }
}

fn connect_and_probe(
    account: &Account,
    credential: &AccountCredential,
) -> Result<(Session<imap::Connection>, ServerCapabilities), String> {
    let mut session = connect_session(account, credential)?;
    let caps = match session.capabilities() {
        Ok(c) => ServerCapabilities {
            has_idle: c.has_str("IDLE"),
            has_move: c.has_str("MOVE"),
            has_uidplus: c.has_str("UIDPLUS"),
        },
        Err(_) => ServerCapabilities::default(),
    };
    Ok((session, caps))
}

/// Whether an `imap::Error` indicates a broken, dropped, or timed-out connection
/// rather than a permanent protocol/logic error (like bad mailbox name).
pub fn is_connection_error(err: &imap::Error) -> bool {
    match err {
        imap::Error::ConnectionLost | imap::Error::Bye(_) | imap::Error::TagMismatch(_) => true,
        imap::Error::Io(io_err) => matches!(
            io_err.kind(),
            io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::NotConnected
                | io::ErrorKind::TimedOut
                | io::ErrorKind::UnexpectedEof
                | io::ErrorKind::WouldBlock
        ) || io_err.kind() == io::ErrorKind::Other,
        _ => false,
    }
}

fn format_imap_error(err: &imap::Error) -> String {
    match err {
        imap::Error::No(no) => no.information.clone(),
        imap::Error::Bad(bad) => bad.information.clone(),
        imap::Error::Bye(bye) => format!("Server closed connection: {}", bye.information),
        imap::Error::ConnectionLost => "Connection to mail server lost".to_owned(),
        other => other.to_string(),
    }
}

/// Extract a single appended UID from `UIDPLUS` `APPENDUID` response metadata.
pub fn parse_appended_uid(appended: &Appended) -> Option<u32> {
    let first = appended.uids.as_ref()?.first()?;
    parse_uid_set_member_dbg(&format!("{first:?}"))
}

fn parse_uid_set_member_dbg(dbg: &str) -> Option<u32> {
    if let Some(inner) = dbg.strip_prefix("Uid(").and_then(|s| s.strip_suffix(')')) {
        return inner.parse().ok();
    }
    if let Some(inner) = dbg
        .strip_prefix("UidRange(")
        .and_then(|s| s.strip_suffix(')'))
        && let Some((start, _)) = inner.split_once("..=")
    {
        return start.parse().ok();
    }
    None
}

/// Background handle monitoring `INBOX` via IMAP `IDLE` (with polling fallback).
pub struct IdleWatcher {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl IdleWatcher {
    /// Start watching `INBOX` on a dedicated connection.
    ///
    /// Calls `on_new_mail` and emits a desktop notification whenever new messages arrive.
    pub fn start<F>(account: Account, credential: AccountCredential, on_new_mail: F) -> Self
    where
        F: Fn(NewMailNotification) + Send + Sync + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("mac-mail-idle".to_owned())
            .spawn(move || {
                run_idle_loop(&account, credential, &stop_flag, &on_new_mail);
            })
            .ok();
        Self { stop, handle }
    }

    /// Signal the background watcher to stop.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            // Do not block the UI thread for 25s if the socket is mid-IDLE; detach cleanly.
            let _ = handle;
        }
    }
}

impl Drop for IdleWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn run_idle_loop<F>(
    account: &Account,
    mut credential: AccountCredential,
    stop: &AtomicBool,
    on_new_mail: &F,
) where
    F: Fn(NewMailNotification),
{
    let mut backoff_secs = 2u64;

    while !stop.load(Ordering::Relaxed) {
        if account::ensure_fresh_credential(account, &mut credential).is_err() {
            sleep_interruptible(Duration::from_secs(backoff_secs), stop);
            backoff_secs = (backoff_secs * 2).min(60);
            continue;
        }
        let Ok((mut session, caps)) = connect_and_probe(account, &credential) else {
            sleep_interruptible(Duration::from_secs(backoff_secs), stop);
            backoff_secs = (backoff_secs * 2).min(60);
            continue;
        };

        let Ok(mailbox) = session.select("INBOX") else {
            let _ = session.logout();
            sleep_interruptible(Duration::from_secs(backoff_secs), stop);
            backoff_secs = (backoff_secs * 2).min(60);
            continue;
        };

        backoff_secs = 2;
        let mut known_exists = mailbox.exists;

        while !stop.load(Ordering::Relaxed) {
            if caps.has_idle {
                let mut idle_changed = false;
                let outcome = {
                    let mut handle = session.idle();
                    handle.timeout(IDLE_CYCLE_TIMEOUT).keepalive(false);
                    handle.wait_while(|resp| match resp {
                        UnsolicitedResponse::Exists(count) => {
                            if count > known_exists {
                                idle_changed = true;
                                false
                            } else {
                                true
                            }
                        }
                        UnsolicitedResponse::Recent(n) if n > 0 => {
                            idle_changed = true;
                            false
                        }
                        _ => true,
                    })
                };

                match outcome {
                    Ok(WaitOutcome::MailboxChanged) => {
                        if let Some(notif) =
                            check_inbox_new_messages(&account.email, &mut session, &mut known_exists)
                        {
                            send_desktop_notification(&notif.from, &notif.subject);
                            on_new_mail(notif);
                        }
                    }
                    Ok(WaitOutcome::TimedOut) => {
                        // Also check if exists changed silently during IDLE cycle.
                        if let Some(notif) =
                            check_inbox_new_messages(&account.email, &mut session, &mut known_exists)
                        {
                            send_desktop_notification(&notif.from, &notif.subject);
                            on_new_mail(notif);
                        }
                    }
                    Err(err) => {
                        tracing::debug!("IMAP IDLE loop disconnected: {err}");
                        break;
                    }
                }
            } else {
                sleep_interruptible(POLL_FALLBACK_INTERVAL, stop);
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                match session.noop() {
                    Ok(()) => {
                        if let Some(notif) =
                            check_inbox_new_messages(&account.email, &mut session, &mut known_exists)
                        {
                            send_desktop_notification(&notif.from, &notif.subject);
                            on_new_mail(notif);
                        }
                    }
                    Err(_) => break,
                }
            }
        }

        let _ = session.logout();
    }
}

fn check_inbox_new_messages(
    account_email: &str,
    session: &mut Session<imap::Connection>,
    known_exists: &mut u32,
) -> Option<NewMailNotification> {
    let mailbox = session.select("INBOX").ok()?;
    if mailbox.exists <= *known_exists {
        *known_exists = mailbox.exists;
        return None;
    }
    let newest_seq = mailbox.exists;
    *known_exists = newest_seq;

    let fetched = session
        .fetch(newest_seq.to_string(), "(UID RFC822.HEADER)")
        .ok()?;
    let fetch = fetched.iter().next()?;
    let uid = fetch.uid;
    let parsed = fetch
        .header()
        .and_then(|hdr| MessageParser::new().parse(hdr));
    let from = parsed
        .as_ref()
        .and_then(|m| m.from())
        .and_then(|list| list.first())
        .and_then(|mb| {
            mb.name()
                .filter(|n| !n.is_empty())
                .or(mb.address())
        })
        .unwrap_or("New message")
        .to_owned();
    let subject = parsed
        .as_ref()
        .and_then(|m| m.subject())
        .unwrap_or("(no subject)")
        .to_owned();

    Some(NewMailNotification {
        account_email: account_email.to_owned(),
        folder: "INBOX".to_owned(),
        uid,
        from,
        subject,
        total_exists: newest_seq,
    })
}

fn sleep_interruptible(duration: Duration, stop: &AtomicBool) {
    let step = Duration::from_millis(250);
    let mut remaining = duration;
    while !remaining.is_zero() && !stop.load(Ordering::Relaxed) {
        let slice = remaining.min(step);
        thread::sleep(slice);
        remaining = remaining.saturating_sub(slice);
    }
}

/// Emit a best-effort desktop notification using `notify-send`.
pub fn send_desktop_notification(title: &str, body: &str) {
    let summary = if title.is_empty() { "New mail" } else { title };
    let _ = Command::new("notify-send")
        .args([
            "--app-name=Mail",
            "--icon=internet-mail",
            summary,
            body,
        ])
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    #[test]
    fn connection_error_classification() {
        assert!(is_connection_error(&imap::Error::ConnectionLost));
        assert!(is_connection_error(&imap::Error::Io(io::Error::new(
            ErrorKind::BrokenPipe,
            "broken pipe"
        ))));
        assert!(is_connection_error(&imap::Error::Io(io::Error::new(
            ErrorKind::TimedOut,
            "timed out"
        ))));
        assert!(is_connection_error(&imap::Error::Io(io::Error::new(
            ErrorKind::UnexpectedEof,
            "eof"
        ))));
    }

    #[test]
    fn pool_tracks_credentials_and_connection_state() {
        let pool = ImapPool::new(
            Account {
                email: "alice@example.com".to_owned(),
                imap_host: "imap.example.com".to_owned(),
                imap_port: 993,
                smtp_host: "smtp.example.com".to_owned(),
                smtp_port: 465,
                ..Default::default()
            },
            AccountCredential::Password("secret".to_owned()),
        );
        assert!(!pool.is_connected());
        assert_eq!(pool.account().email, "alice@example.com");

        pool.set_credentials(
            Account {
                email: "bob@example.com".to_owned(),
                imap_host: "imap.example.com".to_owned(),
                imap_port: 993,
                smtp_host: "smtp.example.com".to_owned(),
                smtp_port: 465,
                ..Default::default()
            },
            AccountCredential::Password("secret2".to_owned()),
        );
        assert_eq!(pool.account().email, "bob@example.com");
        assert!(!pool.is_connected());
    }

    #[test]
    fn appended_uid_parser_handles_uid_and_range() {
        assert_eq!(parse_appended_uid(&Appended::default()), None);
        assert_eq!(parse_uid_set_member_dbg("Uid(421)"), Some(421));
        assert_eq!(parse_uid_set_member_dbg("UidRange(99..=101)"), Some(99));
        assert_eq!(parse_uid_set_member_dbg("Unknown"), None);
    }
}
