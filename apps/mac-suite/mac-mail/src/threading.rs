// SPDX-License-Identifier: GPL-3.0-only

//! Conversation threading across single-folder and unified-inbox message lists.
//!
//! Groups messages into threads using `Message-ID`, `In-Reply-To`, and
//! `References` headers, with a normalized-subject fallback (`Re:`/`Fwd:`/`Fw:`
//! prefixes stripped) for messages lacking threading headers.

use std::collections::HashMap;

use crate::cache::MessageSummary;

/// Unique identifier for a message across accounts and folders.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MessageKey {
    pub account_email: String,
    pub folder: String,
    pub uid: u32,
}

impl MessageKey {
    pub fn new(
        account_email: impl Into<String>,
        folder: impl Into<String>,
        uid: u32,
    ) -> Self {
        Self {
            account_email: account_email.into(),
            folder: folder.into(),
            uid,
        }
    }
}

/// A message summary tagged with its owning account and folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailItem {
    pub account_email: String,
    pub folder: String,
    pub summary: MessageSummary,
}

impl MailItem {
    pub fn key(&self) -> MessageKey {
        MessageKey::new(
            self.account_email.clone(),
            self.folder.clone(),
            self.summary.uid,
        )
    }
}

/// A conversation thread of one or more messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailThread {
    pub id: String,
    /// Messages in chronological order (oldest -> newest).
    pub messages: Vec<MailItem>,
    pub latest_idx: usize,
    pub latest_timestamp: Option<i64>,
    pub has_unread: bool,
    pub has_attachments: bool,
}

impl MailThread {
    pub fn latest(&self) -> &MailItem {
        &self.messages[self.latest_idx]
    }

    pub fn contains_key(&self, key: &MessageKey) -> bool {
        self.messages.iter().any(|m| {
            m.summary.uid == key.uid
                && m.folder == key.folder
                && m.account_email.eq_ignore_ascii_case(&key.account_email)
        })
    }
}

/// Sort individual messages newest-first for flat list mode.
pub fn sort_flat_items(items: &mut [MailItem]) {
    items.sort_by(|a, b| {
        b.summary
            .timestamp
            .unwrap_or(0)
            .cmp(&a.summary.timestamp.unwrap_or(0))
            .then_with(|| b.summary.uid.cmp(&a.summary.uid))
            .then_with(|| a.account_email.cmp(&b.account_email))
    });
}

/// Normalize a `Message-ID` token by trimming whitespace and angle brackets.
pub fn normalize_message_id(raw: &str) -> Option<String> {
    let inner = raw.trim().trim_start_matches('<').trim_end_matches('>').trim();
    (!inner.is_empty()).then(|| inner.to_owned())
}

/// Strip leading reply/forward prefixes (`Re:`, `Fwd:`, `Fw:`, `Re[2]:`, `Aw:`, `Sv:`)
/// and normalize whitespace/casing for subject-based thread fallback.
pub fn normalize_subject(subject: &str) -> String {
    let mut current = subject.trim();
    loop {
        let lower = current.to_ascii_lowercase();
        let matched_len = ["re", "fwd", "fw", "aw", "sv"]
            .into_iter()
            .find_map(|prefix| strip_reply_prefix_len(&lower, prefix));
        if let Some(len) = matched_len {
            current = current[len..].trim_start();
        } else {
            break;
        }
    }
    current
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn strip_reply_prefix_len(lower: &str, prefix: &str) -> Option<usize> {
    let rest = lower.strip_prefix(prefix)?;
    let trimmed = rest.trim_start();
    // Handle `re:`, `re[2]:`, `re (2):`
    if let Some(after_colon) = trimmed.strip_prefix(':') {
        let consumed = lower.len() - after_colon.len();
        return Some(consumed);
    }
    if let Some(after_open) = trimmed.strip_prefix('[')
        && let Some(close_idx) = after_open.find(']')
        && after_open[..close_idx].chars().all(|c| c.is_ascii_digit())
    {
        let after_bracket = after_open[close_idx + 1..].trim_start();
        if let Some(after_colon) = after_bracket.strip_prefix(':') {
            let consumed = lower.len() - after_colon.len();
            return Some(consumed);
        }
    }
    if let Some(after_open) = trimmed.strip_prefix('(')
        && let Some(close_idx) = after_open.find(')')
        && after_open[..close_idx].chars().all(|c| c.is_ascii_digit())
    {
        let after_paren = after_open[close_idx + 1..].trim_start();
        if let Some(after_colon) = after_paren.strip_prefix(':') {
            let consumed = lower.len() - after_colon.len();
            return Some(consumed);
        }
    }
    None
}

struct DisjointSet {
    parent: Vec<usize>,
}

impl DisjointSet {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut cur = x;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[rb] = ra;
        }
    }
}

/// Group `items` into conversation threads and sort threads by newest message timestamp.
pub fn build_threads(items: Vec<MailItem>) -> Vec<MailThread> {
    if items.is_empty() {
        return Vec::new();
    }

    let mut dsu = DisjointSet::new(items.len());
    let mut id_owner: HashMap<String, usize> = HashMap::new();
    let mut subject_owner: HashMap<String, usize> = HashMap::new();

    for (idx, item) in items.iter().enumerate() {
        let s = &item.summary;
        let mut all_ids = Vec::new();
        if let Some(mid) = s.message_id.as_deref().and_then(normalize_message_id) {
            all_ids.push(mid);
        }
        if let Some(irt) = s.in_reply_to.as_deref() {
            for token in irt.split_whitespace() {
                if let Some(id) = normalize_message_id(token) {
                    all_ids.push(id);
                }
            }
        }
        for r in &s.references {
            for token in r.split_whitespace() {
                if let Some(id) = normalize_message_id(token) {
                    all_ids.push(id);
                }
            }
        }

        for id in all_ids {
            if let Some(&owner) = id_owner.get(&id) {
                dsu.union(owner, idx);
            } else {
                id_owner.insert(id, idx);
            }
        }

        let norm_subj = normalize_subject(&s.subject);
        if !norm_subj.is_empty() && norm_subj != "(no subject)" {
            if let Some(&owner) = subject_owner.get(&norm_subj) {
                dsu.union(owner, idx);
            } else {
                subject_owner.insert(norm_subj, idx);
            }
        }
    }

    let mut groups: HashMap<usize, Vec<MailItem>> = HashMap::new();
    for (idx, item) in items.into_iter().enumerate() {
        let root = dsu.find(idx);
        groups.entry(root).or_default().push(item);
    }

    let mut threads: Vec<MailThread> = groups
        .into_values()
        .map(|mut messages| {
            messages.sort_by(|a, b| {
                a.summary
                    .timestamp
                    .unwrap_or(0)
                    .cmp(&b.summary.timestamp.unwrap_or(0))
                    .then_with(|| a.summary.uid.cmp(&b.summary.uid))
                    .then_with(|| a.account_email.cmp(&b.account_email))
            });
            let latest_idx = messages.len() - 1;
            let latest_timestamp = messages[latest_idx].summary.timestamp;
            let has_unread = messages.iter().any(|m| m.summary.unread);
            let has_attachments = messages.iter().any(|m| m.summary.has_attachments);
            let id = messages[0]
                .summary
                .message_id
                .as_deref()
                .and_then(normalize_message_id)
                .unwrap_or_else(|| {
                    format!(
                        "{}:{}:{}",
                        messages[0].account_email, messages[0].folder, messages[0].summary.uid
                    )
                });
            MailThread {
                id,
                messages,
                latest_idx,
                latest_timestamp,
                has_unread,
                has_attachments,
            }
        })
        .collect();

    threads.sort_by(|a, b| {
        b.latest_timestamp
            .unwrap_or(0)
            .cmp(&a.latest_timestamp.unwrap_or(0))
            .then_with(|| b.latest().summary.uid.cmp(&a.latest().summary.uid))
            .then_with(|| a.id.cmp(&b.id))
    });

    threads
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item(
        account: &str,
        uid: u32,
        subject: &str,
        ts: i64,
        msg_id: Option<&str>,
        in_reply_to: Option<&str>,
        refs: &[&str],
    ) -> MailItem {
        MailItem {
            account_email: account.to_owned(),
            folder: "INBOX".to_owned(),
            summary: MessageSummary {
                uid,
                from: "User".to_owned(),
                from_address: "user@example.com".to_owned(),
                address: "user@example.com".to_owned(),
                to: vec![],
                cc: vec![],
                subject: subject.to_owned(),
                date: String::new(),
                timestamp: Some(ts),
                unread: uid % 2 == 1,
                has_attachments: uid == 2,
                preview: String::new(),
                message_id: msg_id.map(str::to_owned),
                in_reply_to: in_reply_to.map(str::to_owned),
                references: refs.iter().map(|s| (*s).to_owned()).collect(),
            },
        }
    }

    #[test]
    fn normalizes_re_and_fwd_subject_prefixes() {
        assert_eq!(normalize_subject("Re: Project Plan"), "project plan");
        assert_eq!(
            normalize_subject("Re[2]: Fwd: FW:  Project   Plan "),
            "project plan"
        );
        assert_eq!(normalize_subject("Re (3): Aw: Budget"), "budget");
        assert_eq!(normalize_subject("(no subject)"), "(no subject)");
        assert_eq!(normalize_subject(""), "");
    }

    #[test]
    fn groups_by_message_id_in_reply_to_and_references_out_of_order() {
        let items = vec![
            // Reply #2 arrives first, references root and reply #1 (even with changed subject)
            make_item(
                "a@ex.com",
                3,
                "Updated topic",
                300,
                Some("<m3@ex.com>"),
                Some("<m2@ex.com>"),
                &["m1@ex.com", "m2@ex.com"],
            ),
            // Unrelated message
            make_item(
                "a@ex.com",
                4,
                "Lunch today?",
                250,
                Some("<other@ex.com>"),
                None,
                &[],
            ),
            // Root message
            make_item(
                "a@ex.com",
                1,
                "Original topic",
                100,
                Some("<m1@ex.com>"),
                None,
                &[],
            ),
            // Reply #1 using only In-Reply-To
            make_item(
                "a@ex.com",
                2,
                "Re: Original topic",
                200,
                Some("<m2@ex.com>"),
                Some("<m1@ex.com>"),
                &[],
            ),
        ];

        let threads = build_threads(items);
        assert_eq!(threads.len(), 2);

        // First thread has latest_timestamp = 300 (3 messages sorted chronologically)
        assert_eq!(threads[0].messages.len(), 3);
        assert_eq!(threads[0].latest_timestamp, Some(300));
        assert!(threads[0].has_unread);
        assert!(threads[0].has_attachments);
        let uids: Vec<u32> = threads[0].messages.iter().map(|m| m.summary.uid).collect();
        assert_eq!(uids, vec![1, 2, 3]);
        assert_eq!(threads[0].latest().summary.uid, 3);

        // Second thread has 1 message at ts = 250
        assert_eq!(threads[1].messages.len(), 1);
        assert_eq!(threads[1].latest().summary.uid, 4);
    }

    #[test]
    fn subject_fallback_groups_missing_headers_but_never_merges_no_subject() {
        let items = vec![
            make_item("a@ex.com", 10, "Quarterly Review", 100, None, None, &[]),
            make_item("a@ex.com", 11, "Re: Fwd: Quarterly Review", 200, None, None, &[]),
            make_item("a@ex.com", 12, "(no subject)", 150, None, None, &[]),
            make_item("a@ex.com", 13, "(no subject)", 160, None, None, &[]),
        ];

        let threads = build_threads(items);
        assert_eq!(threads.len(), 3);
        assert_eq!(threads[0].messages.len(), 2);
        assert_eq!(threads[0].latest().summary.uid, 11);
        assert_eq!(threads[1].messages.len(), 1);
        assert_eq!(threads[2].messages.len(), 1);
    }
}
