// SPDX-License-Identifier: GPL-3.0-only

//! Mail account configuration, multi-account persistence (`accounts.json` with
//! automatic migration from legacy `account.conf`), keyring credential storage,
//! and trusted-sender remote image whitelist.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

use crate::oauth::{self, OAuthProvider, OAuthTokens};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// How an account authenticates to IMAP and SMTP.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    #[default]
    Password,
    OAuth2Gmail,
    OAuth2Outlook,
}

impl AuthMethod {
    pub fn oauth_provider(self) -> Option<OAuthProvider> {
        match self {
            AuthMethod::Password => None,
            AuthMethod::OAuth2Gmail => Some(OAuthProvider::Gmail),
            AuthMethod::OAuth2Outlook => Some(OAuthProvider::Outlook),
        }
    }
}

/// Configured mail account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    #[serde(default)]
    pub auth_method: AuthMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,
}

/// Credential held in memory / keyring for one account.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AccountCredential {
    Password(String),
    OAuth2(OAuthTokens),
}

impl AccountCredential {
    pub fn secret_for_imap(&self) -> &str {
        match self {
            AccountCredential::Password(pw) => pw.as_str(),
            AccountCredential::OAuth2(tokens) => tokens.access_token.as_str(),
        }
    }

    pub fn is_oauth2(&self) -> bool {
        matches!(self, AccountCredential::OAuth2(_))
    }
}

/// Multi-account configuration stored in `~/.config/mac-mail/accounts.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountsConfig {
    #[serde(default)]
    pub active_account: Option<String>,
    #[serde(default)]
    pub accounts: Vec<Account>,
}

impl AccountsConfig {
    pub fn active(&self) -> Option<&Account> {
        if let Some(active) = self.active_account.as_deref()
            && let Some(found) = self
                .accounts
                .iter()
                .find(|a| a.email.eq_ignore_ascii_case(active))
        {
            return Some(found);
        }
        self.accounts.first()
    }

    pub fn upsert(&mut self, account: Account) {
        let target = account.email.trim().to_ascii_lowercase();
        if let Some(existing) = self
            .accounts
            .iter_mut()
            .find(|a| a.email.trim().to_ascii_lowercase() == target)
        {
            *existing = account;
        } else {
            self.accounts.push(account);
        }
        self.active_account = Some(target);
    }

    pub fn remove(&mut self, email: &str) -> bool {
        let target = email.trim().to_ascii_lowercase();
        let before = self.accounts.len();
        self.accounts
            .retain(|a| a.email.trim().to_ascii_lowercase() != target);
        let removed = self.accounts.len() != before;
        if self
            .active_account
            .as_deref()
            .is_some_and(|a| a.eq_ignore_ascii_case(&target))
        {
            self.active_account = self.accounts.first().map(|a| a.email.clone());
        }
        removed
    }
}

pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("mac-mail"))
}

fn accounts_json_path(dir: &Path) -> PathBuf {
    dir.join("accounts.json")
}

fn legacy_conf_path(dir: &Path) -> PathBuf {
    dir.join("account.conf")
}

fn trusted_senders_path(dir: &Path) -> PathBuf {
    dir.join("trusted_senders.json")
}

/// Server names usually follow `imap.<domain>` and `smtp.<domain>`.
pub fn guess(email: &str) -> Account {
    let domain = email
        .split_once('@')
        .map_or("", |(_, domain)| domain.trim())
        .to_ascii_lowercase();
    let (imap, smtp, smtp_port) = match domain.as_str() {
        "gmail.com" | "googlemail.com" => ("imap.gmail.com", "smtp.gmail.com", 465),
        "outlook.com" | "hotmail.com" | "live.com" => {
            ("outlook.office365.com", "smtp.office365.com", 587)
        }
        "icloud.com" | "me.com" => ("imap.mail.me.com", "smtp.mail.me.com", 465),
        "yahoo.com" => ("imap.mail.yahoo.com", "smtp.mail.yahoo.com", 465),
        "" => ("", "", 465),
        _ => (
            &*format!("imap.{domain}").leak(),
            &*format!("smtp.{domain}").leak(),
            465,
        ),
    };
    Account {
        email: email.trim().to_owned(),
        imap_host: imap.to_owned(),
        imap_port: 993,
        smtp_host: smtp.to_owned(),
        smtp_port,
        auth_method: AuthMethod::Password,
        oauth_client_id: None,
    }
}

/// Build an [`Account`] preset for an OAuth2 provider.
pub fn oauth_account(
    provider: OAuthProvider,
    email: String,
    client_id: String,
) -> Account {
    Account {
        email,
        imap_host: provider.imap_host().to_owned(),
        imap_port: provider.imap_port(),
        smtp_host: provider.smtp_host().to_owned(),
        smtp_port: provider.smtp_port(),
        auth_method: match provider {
            OAuthProvider::Gmail => AuthMethod::OAuth2Gmail,
            OAuthProvider::Outlook => AuthMethod::OAuth2Outlook,
        },
        oauth_client_id: (!client_id.trim().is_empty()).then_some(client_id),
    }
}

fn parse_legacy_conf(contents: &str) -> Option<Account> {
    let mut account = Account::default();
    for line in contents.lines() {
        let (key, value) = line.split_once('=')?;
        match key.trim() {
            "email" => account.email = value.trim().to_owned(),
            "imap_host" => account.imap_host = value.trim().to_owned(),
            "imap_port" => account.imap_port = value.trim().parse().ok()?,
            "smtp_host" => account.smtp_host = value.trim().to_owned(),
            "smtp_port" => account.smtp_port = value.trim().parse().ok()?,
            _ => {}
        }
    }
    (!account.email.is_empty() && !account.imap_host.is_empty()).then_some(account)
}

/// Load multi-account configuration from `dir`, automatically migrating any
/// legacy `account.conf` into `accounts.json`.
pub fn load_all_from_dir(dir: &Path) -> AccountsConfig {
    let json_path = accounts_json_path(dir);
    if let Ok(bytes) = fs::read(&json_path)
        && let Ok(mut cfg) = serde_json::from_slice::<AccountsConfig>(&bytes)
    {
        cfg.accounts
            .retain(|a| !a.email.trim().is_empty() && !a.imap_host.trim().is_empty());
        if cfg
            .active_account
            .as_deref()
            .is_none_or(|active| !cfg.accounts.iter().any(|a| a.email.eq_ignore_ascii_case(active)))
        {
            cfg.active_account = cfg.accounts.first().map(|a| a.email.clone());
        }
        return cfg;
    }

    let legacy_path = legacy_conf_path(dir);
    if let Ok(contents) = fs::read_to_string(&legacy_path)
        && let Some(account) = parse_legacy_conf(&contents)
    {
        let cfg = AccountsConfig {
            active_account: Some(account.email.clone()),
            accounts: vec![account],
        };
        if save_all_to_dir(dir, &cfg).is_ok() {
            let _ = fs::remove_file(&legacy_path);
        }
        return cfg;
    }

    AccountsConfig::default()
}

pub fn save_all_to_dir(dir: &Path, config: &AccountsConfig) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(config).map_err(std::io::Error::other)?;
    write_atomic(&accounts_json_path(dir), &json)
}

pub fn load_all() -> AccountsConfig {
    let Some(dir) = config_dir() else {
        return AccountsConfig::default();
    };
    load_all_from_dir(&dir)
}

pub fn save_all(config: &AccountsConfig) -> std::io::Result<()> {
    let dir = config_dir().ok_or_else(|| std::io::Error::other("no config directory"))?;
    save_all_to_dir(&dir, config)
}

pub fn upsert_account(account: Account) -> std::io::Result<AccountsConfig> {
    let mut cfg = load_all();
    cfg.upsert(account);
    save_all(&cfg)?;
    Ok(cfg)
}

pub fn set_active_account(email: &str) -> std::io::Result<AccountsConfig> {
    let mut cfg = load_all();
    if let Some(found) = cfg
        .accounts
        .iter()
        .find(|a| a.email.eq_ignore_ascii_case(email))
        .map(|a| a.email.clone())
    {
        cfg.active_account = Some(found);
        save_all(&cfg)?;
    }
    Ok(cfg)
}

#[allow(dead_code)]
pub fn load() -> Option<Account> {
    load_all().active().cloned()
}

#[allow(dead_code)]
pub fn save(account: &Account) -> std::io::Result<()> {
    upsert_account(account.clone()).map(|_| ())
}

pub fn forget(email: &str) {
    if let Some(dir) = config_dir() {
        let mut cfg = load_all_from_dir(&dir);
        if cfg.remove(email) {
            let _ = save_all_to_dir(&dir, &cfg);
        }
        let _ = fs::remove_file(legacy_conf_path(&dir));
    }
    clear_keyring(email);
    clear_keyring(&oauth_keyring_key(email));
}

fn oauth_keyring_key(email: &str) -> String {
    format!("oauth:{}", email.trim().to_ascii_lowercase())
}

pub fn store_password(email: &str, password: &str) -> Result<(), String> {
    store_keyring_secret(email, &format!("Mail ({email})"), password)
}

pub fn load_password(email: &str) -> Option<String> {
    load_keyring_secret(email)
}

pub fn store_oauth_tokens(email: &str, tokens: &OAuthTokens) -> Result<(), String> {
    let json = serde_json::to_string(tokens).map_err(|err| err.to_string())?;
    store_keyring_secret(&oauth_keyring_key(email), &format!("Mail OAuth ({email})"), &json)
}

pub fn load_oauth_tokens(email: &str) -> Option<OAuthTokens> {
    let raw = load_keyring_secret(&oauth_keyring_key(email))?;
    serde_json::from_str(&raw).ok()
}

pub fn load_credential(account: &Account) -> Option<AccountCredential> {
    match account.auth_method {
        AuthMethod::Password => load_password(&account.email).map(AccountCredential::Password),
        AuthMethod::OAuth2Gmail | AuthMethod::OAuth2Outlook => {
            load_oauth_tokens(&account.email).map(AccountCredential::OAuth2)
        }
    }
}

pub fn store_credential(account: &Account, credential: &AccountCredential) -> Result<(), String> {
    match credential {
        AccountCredential::Password(pw) => store_password(&account.email, pw),
        AccountCredential::OAuth2(tokens) => store_oauth_tokens(&account.email, tokens),
    }
}

/// Ensure an `AccountCredential` has a valid, non-expired access token when using OAuth2.
/// Refreshes and persists updated tokens if expired or within 60 seconds of expiry.
pub fn ensure_fresh_credential(
    account: &Account,
    credential: &mut AccountCredential,
) -> Result<(), String> {
    let AccountCredential::OAuth2(tokens) = credential else {
        return Ok(());
    };
    let now = oauth::current_unix_secs();
    if !tokens.is_expired_soon(now) {
        return Ok(());
    }
    let provider = account
        .auth_method
        .oauth_provider()
        .ok_or_else(|| "Account is missing OAuth2 provider".to_owned())?;
    let refresh = tokens
        .refresh_token
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "OAuth2 token expired and no refresh token is available".to_owned())?;
    let client_id = oauth::resolve_client_id(provider, account.oauth_client_id.as_deref())?;
    let client_secret = oauth::resolve_client_secret(provider);
    let refreshed =
        oauth::refresh_access_token(provider, &client_id, client_secret.as_deref(), refresh)?;
    *tokens = refreshed.clone();
    let _ = store_oauth_tokens(&account.email, &refreshed);
    Ok(())
}

fn store_keyring_secret(key: &str, label: &str, secret: &str) -> Result<(), String> {
    let mut child = Command::new("secret-tool")
        .args(["store", "--label", label, "service", "mac-mail", "account", key])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|err| format!("secret-tool is needed to store credentials: {err}"))?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(secret.as_bytes())
        .map_err(|err| err.to_string())?;
    let status = child.wait().map_err(|err| err.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "could not store credentials in the keyring".to_owned())
}

fn load_keyring_secret(key: &str) -> Option<String> {
    let output = Command::new("secret-tool")
        .args(["lookup", "service", "mac-mail", "account", key])
        .output()
        .ok()?;
    let secret = String::from_utf8(output.stdout).ok()?;
    (output.status.success() && !secret.is_empty()).then_some(secret)
}

fn clear_keyring(key: &str) {
    let _ = Command::new("secret-tool")
        .args(["clear", "service", "mac-mail", "account", key])
        .status();
}

/// Persistent list of sender email addresses trusted to auto-load remote images.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedSenders {
    path: Option<PathBuf>,
    senders: BTreeSet<String>,
}

impl TrustedSenders {
    pub fn load_default() -> Self {
        match config_dir() {
            Some(dir) => Self::load_from_path(trusted_senders_path(&dir)),
            None => Self::default(),
        }
    }

    pub fn load_from_path(path: PathBuf) -> Self {
        let senders: BTreeSet<String> = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|s| normalize_sender(&s))
            .filter(|s| !s.is_empty())
            .collect();
        Self {
            path: Some(path),
            senders,
        }
    }

    pub fn is_trusted(&self, sender_email: &str) -> bool {
        let norm = normalize_sender(sender_email);
        !norm.is_empty() && self.senders.contains(&norm)
    }

    pub fn trust(&mut self, sender_email: &str) -> Result<bool, String> {
        let norm = normalize_sender(sender_email);
        if norm.is_empty() {
            return Ok(false);
        }
        let inserted = self.senders.insert(norm);
        if inserted && let Some(path) = &self.path {
            let list: Vec<&str> = self.senders.iter().map(String::as_str).collect();
            let json = serde_json::to_vec_pretty(&list).map_err(|err| err.to_string())?;
            write_atomic(path, &json).map_err(|err| err.to_string())?;
        }
        Ok(inserted)
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.senders.len()
    }
}

fn normalize_sender(raw: &str) -> String {
    let trimmed = raw.trim();
    if let (Some(start), Some(end)) = (trimmed.rfind('<'), trimmed.rfind('>'))
        && start < end
    {
        return trimmed[start + 1..end].trim().to_ascii_lowercase();
    }
    trimmed.to_ascii_lowercase()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::other("invalid config path"));
    };
    fs::create_dir_all(parent)?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.tmp.{}.{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config"),
        std::process::id(),
        seq
    );
    let tmp_path = parent.join(tmp_name);
    fs::write(&tmp_path, bytes)?;
    fs::rename(&tmp_path, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp_path);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "mac-mail-account-test-{name}-{}-{seq}",
            std::process::id()
        ))
    }

    #[test]
    fn migrates_legacy_account_conf_to_accounts_json() {
        let dir = temp_dir("migrate");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("account.conf"),
            "email=alice@example.com\nimap_host=imap.example.com\nimap_port=993\nsmtp_host=smtp.example.com\nsmtp_port=465\n",
        )
        .unwrap();

        let cfg = load_all_from_dir(&dir);
        assert_eq!(cfg.accounts.len(), 1);
        assert_eq!(cfg.active_account.as_deref(), Some("alice@example.com"));
        assert_eq!(cfg.accounts[0].imap_host, "imap.example.com");
        assert_eq!(cfg.accounts[0].auth_method, AuthMethod::Password);
        assert!(dir.join("accounts.json").exists());
        assert!(!dir.join("account.conf").exists());

        // Add a second account (OAuth2) and verify reload preserves both.
        let mut cfg2 = load_all_from_dir(&dir);
        cfg2.upsert(oauth_account(
            OAuthProvider::Gmail,
            "bob@gmail.com".to_owned(),
            "client-123".to_owned(),
        ));
        save_all_to_dir(&dir, &cfg2).unwrap();

        let reloaded = load_all_from_dir(&dir);
        assert_eq!(reloaded.accounts.len(), 2);
        assert_eq!(reloaded.active_account.as_deref(), Some("bob@gmail.com"));
        assert_eq!(reloaded.accounts[1].auth_method, AuthMethod::OAuth2Gmail);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn trusted_senders_persists_normalized_addresses() {
        let dir = temp_dir("trusted");
        let path = dir.join("trusted_senders.json");
        let mut store = TrustedSenders::load_from_path(path.clone());
        assert!(!store.is_trusted("newsletter@acme.org"));

        assert!(store.trust("Acme News <Newsletter@Acme.ORG>").unwrap());
        assert!(!store.trust("newsletter@acme.org").unwrap());

        let reloaded = TrustedSenders::load_from_path(path);
        assert!(reloaded.is_trusted("newsletter@acme.org"));
        assert!(reloaded.is_trusted("NEWSLETTER@ACME.ORG"));

        let _ = fs::remove_dir_all(&dir);
    }
}
