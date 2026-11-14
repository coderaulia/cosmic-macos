// SPDX-License-Identifier: GPL-3.0-only

//! Address book & autocomplete suggestions populated from people already
//! mailed and contacts harvested from existing messages.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
};

use super::recipients::{EmailAddress, split_address_tokens};

#[derive(Clone, Debug, Default)]
pub struct AddressBook {
    contacts: HashMap<String, EmailAddress>,
    persistence_path: Option<PathBuf>,
}

impl AddressBook {
    /// Load persisted contacts from `$XDG_DATA_HOME/mac-mail/contacts.tsv`.
    pub fn load_default() -> Self {
        let path = default_contacts_path();
        let mut book = Self {
            contacts: HashMap::new(),
            persistence_path: path.clone(),
        };
        if let Some(path) = path
            && let Ok(text) = fs::read_to_string(path)
        {
            book.load_tsv(&text);
        }
        book
    }

    /// Create an in-memory address book (useful for unit tests).
    #[allow(dead_code)]
    pub fn in_memory() -> Self {
        Self::default()
    }

    pub fn load_tsv(&mut self, contents: &str) {
        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let (email, name) = match trimmed.split_once('\t') {
                Some((email, name)) => (email.trim(), Some(name.trim().to_owned())),
                None => (trimmed, None),
            };
            if !email.is_empty() && email.contains('@') {
                self.insert_one(EmailAddress::new(name, email));
            }
        }
    }

    pub fn to_tsv(&self) -> String {
        let mut items: Vec<&EmailAddress> = self.contacts.values().collect();
        items.sort_by_key(|a| a.normalized_address());
        items
            .into_iter()
            .map(|c| {
                let name = c
                    .name
                    .as_deref()
                    .unwrap_or_default()
                    .replace(['\t', '\r', '\n'], " ");
                format!("{}\t{name}\n", c.address)
            })
            .collect()
    }

    fn insert_one(&mut self, addr: EmailAddress) -> bool {
        let norm = addr.normalized_address();
        if norm.is_empty() || !norm.contains('@') {
            return false;
        }
        match self.contacts.get_mut(&norm) {
            Some(existing) => {
                if existing.name.is_none() && addr.name.is_some() {
                    existing.name = addr.name;
                    true
                } else {
                    false
                }
            }
            None => {
                self.contacts.insert(norm, addr);
                true
            }
        }
    }

    /// Add contacts (e.g. harvested from messages or sent recipients) and persist if configured.
    pub fn merge(&mut self, addrs: impl IntoIterator<Item = EmailAddress>) {
        let mut changed = false;
        for addr in addrs {
            if self.insert_one(addr) {
                changed = true;
            }
        }
        if changed {
            self.save();
        }
    }

    pub fn save(&self) {
        let Some(path) = self.persistence_path.as_ref() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(path, self.to_tsv());
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.contacts.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.contacts.is_empty()
    }

    /// Return up to `limit` address suggestions matching the partial token at
    /// the end of `field_text`, excluding addresses already listed earlier in `field_text`.
    pub fn suggest(&self, field_text: &str, limit: usize) -> Vec<EmailAddress> {
        let tokens = split_address_tokens(field_text);
        let Some(last) = tokens.last() else {
            return Vec::new();
        };
        let query = last.trim().to_ascii_lowercase();
        if query.is_empty() {
            return Vec::new();
        }

        let mut existing = HashSet::new();
        for tok in tokens.iter().take(tokens.len().saturating_sub(1)) {
            if let Ok(parsed) = super::recipients::parse_single_address(tok) {
                existing.insert(parsed.normalized_address());
            } else {
                existing.insert(tok.trim().to_ascii_lowercase());
            }
        }

        let mut scored: Vec<(u8, String, EmailAddress)> = self
            .contacts
            .values()
            .filter(|c| !existing.contains(&c.normalized_address()))
            .filter_map(|c| {
                let email_lower = c.normalized_address();
                let name_lower = c
                    .name
                    .as_deref()
                    .map(str::to_ascii_lowercase)
                    .unwrap_or_default();
                let score = if email_lower.starts_with(&query) {
                    0
                } else if name_lower.starts_with(&query) {
                    1
                } else if email_lower.contains(&query) {
                    2
                } else if name_lower.contains(&query) {
                    3
                } else {
                    return None;
                };
                Some((score, email_lower, c.clone()))
            })
            .collect();

        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        scored.into_iter().take(limit).map(|(_, _, c)| c).collect()
    }

    /// Replace the active trailing partial token in `field_text` with the chosen suggestion.
    pub fn apply_suggestion(field_text: &str, suggestion: &EmailAddress) -> String {
        let tokens = split_address_tokens(field_text);
        let mut prefix: Vec<String> = tokens
            .iter()
            .take(tokens.len().saturating_sub(1))
            .filter(|t| !t.trim().is_empty())
            .cloned()
            .collect();
        prefix.push(suggestion.format_display());
        format!("{}, ", prefix.join(", "))
    }
}

fn default_contacts_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
        })?;
    Some(base.join("mac-mail/contacts.tsv"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_matching_contacts_excluding_already_entered() {
        let mut book = AddressBook::in_memory();
        book.merge([
            EmailAddress::new(Some("Alice Smith".to_owned()), "alice@example.com"),
            EmailAddress::new(Some("Bob Jones".to_owned()), "bob@example.com"),
            EmailAddress::new(Some("Alicia".to_owned()), "alicia@work.org"),
        ]);

        let suggestions = book.suggest("ali", 5);
        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[0].address, "alice@example.com");
        assert_eq!(suggestions[1].address, "alicia@work.org");

        let next = AddressBook::apply_suggestion("bob@example.com, ali", &suggestions[0]);
        assert_eq!(next, "bob@example.com, Alice Smith <alice@example.com>, ");

        let filtered = book.suggest("Alice Smith <alice@example.com>, al", 5);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].address, "alicia@work.org");
    }

    #[test]
    fn tsv_roundtrip_preserves_names_and_emails() {
        let mut book = AddressBook::in_memory();
        book.merge([
            EmailAddress::new(Some("Alice".to_owned()), "alice@example.com"),
            EmailAddress::new(None, "noreply@example.org"),
        ]);
        let tsv = book.to_tsv();
        let mut loaded = AddressBook::in_memory();
        loaded.load_tsv(&tsv);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.suggest("alice", 5)[0].name.as_deref(), Some("Alice"));
    }
}
