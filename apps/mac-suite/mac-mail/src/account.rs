// SPDX-License-Identifier: GPL-3.0-only

//! The mail account: server settings in a small text file, the password in
//! the desktop keyring (through `secret-tool`), never on disk.

use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Clone, Debug, Default)]
pub struct Account {
    pub email: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("mac-mail/account.conf"))
}

/// Server names usually follow `imap.<domain>` and `smtp.<domain>`.
pub fn guess(email: &str) -> Account {
    let domain = email.split_once('@').map_or("", |(_, domain)| domain);
    let (imap, smtp) = match domain {
        "gmail.com" | "googlemail.com" => ("imap.gmail.com", "smtp.gmail.com"),
        "outlook.com" | "hotmail.com" | "live.com" => {
            ("outlook.office365.com", "smtp.office365.com")
        }
        "icloud.com" | "me.com" => ("imap.mail.me.com", "smtp.mail.me.com"),
        "yahoo.com" => ("imap.mail.yahoo.com", "smtp.mail.yahoo.com"),
        "" => ("", ""),
        _ => (&*format!("imap.{domain}").leak(), &*format!("smtp.{domain}").leak()),
    };
    Account {
        email: email.to_owned(),
        imap_host: imap.to_owned(),
        imap_port: 993,
        smtp_host: smtp.to_owned(),
        smtp_port: 465,
    }
}

pub fn load() -> Option<Account> {
    let contents = fs::read_to_string(path()?).ok()?;
    let mut account = Account::default();
    for line in contents.lines() {
        let (key, value) = line.split_once('=')?;
        match key {
            "email" => account.email = value.to_owned(),
            "imap_host" => account.imap_host = value.to_owned(),
            "imap_port" => account.imap_port = value.parse().ok()?,
            "smtp_host" => account.smtp_host = value.to_owned(),
            "smtp_port" => account.smtp_port = value.parse().ok()?,
            _ => {}
        }
    }
    (!account.email.is_empty() && !account.imap_host.is_empty()).then_some(account)
}

pub fn save(account: &Account) -> std::io::Result<()> {
    let path = path().ok_or_else(|| std::io::Error::other("no config directory"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(
        path,
        format!(
            "email={}\nimap_host={}\nimap_port={}\nsmtp_host={}\nsmtp_port={}\n",
            account.email, account.imap_host, account.imap_port, account.smtp_host, account.smtp_port
        ),
    )
}

pub fn forget(email: &str) {
    if let Some(path) = path() {
        let _ = fs::remove_file(path);
    }
    let _ = Command::new("secret-tool")
        .args(["clear", "service", "mac-mail", "account", email])
        .status();
}

pub fn store_password(email: &str, password: &str) -> Result<(), String> {
    let mut child = Command::new("secret-tool")
        .args([
            "store",
            "--label",
            &format!("Mail ({email})"),
            "service",
            "mac-mail",
            "account",
            email,
        ])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|err| format!("secret-tool is needed to store the password: {err}"))?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(password.as_bytes())
        .map_err(|err| err.to_string())?;
    let status = child.wait().map_err(|err| err.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "could not store the password in the keyring".to_owned())
}

pub fn load_password(email: &str) -> Option<String> {
    let output = Command::new("secret-tool")
        .args(["lookup", "service", "mac-mail", "account", email])
        .output()
        .ok()?;
    let password = String::from_utf8(output.stdout).ok()?;
    (output.status.success() && !password.is_empty()).then_some(password)
}
