// SPDX-License-Identifier: GPL-3.0-only

//! Sensitive content filtering, ignored-app detection, transient-clear guard,
//! and optional high-entropy secret heuristic.

use std::time::{Duration, Instant};

pub const SELF_OWNER_MIME: &str = "application/x-mac-clipboard-owner";

pub const SENSITIVE_MIME_TYPES: &[&str] = &[
    "x-kde-passwordmanagerhint",
    "application/x-kde-passwordmanagerhint",
    "x-gnome-sensitive",
];

pub const TRANSIENT_CLEAR_WINDOW: Duration = Duration::from_secs(1);
pub const SECRET_EXPIRY_MS: i64 = 60_000;

/// Returns true if the offer advertises our own clipboard-owner marker.
#[must_use]
pub fn is_self_offer(mime_types: &[String]) -> bool {
    mime_types.iter().any(|mime| {
        let essence = mime.split(';').next().unwrap_or(mime).trim();
        essence.eq_ignore_ascii_case(SELF_OWNER_MIME)
    })
}

/// Returns true if the offer advertises any password-manager or sensitive hint.
/// Only inspects the offered MIME type list; never reads offer bytes.
#[must_use]
pub fn is_sensitive_offer(mime_types: &[String]) -> bool {
    mime_types.iter().any(|mime| {
        let essence = mime.split(';').next().unwrap_or(mime).trim();
        SENSITIVE_MIME_TYPES
            .iter()
            .any(|hint| essence.eq_ignore_ascii_case(hint))
    })
}

/// Returns true if `app_id` matches any entry in `ignored_apps`.
#[must_use]
pub fn is_ignored_app(app_id: Option<&str>, ignored_apps: &[String]) -> bool {
    let Some(app_id) = app_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    let normalized = app_id.strip_suffix(".desktop").unwrap_or(app_id);
    ignored_apps.iter().any(|entry| {
        let entry = entry.trim();
        let entry_norm = entry.strip_suffix(".desktop").unwrap_or(entry);
        !entry_norm.is_empty() && normalized.eq_ignore_ascii_case(entry_norm)
    })
}

/// Decision for whether an incoming clipboard offer should be recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferFilterDecision {
    Allow,
    SkipSelf,
    SkipSensitiveMime,
    SkipPaused,
    SkipIgnoredApp,
}

/// Evaluates whether an offer should be recorded before reading any bytes.
#[must_use]
pub fn filter_offer(
    mime_types: &[String],
    focused_app: Option<&str>,
    ignored_apps: &[String],
    paused: bool,
) -> OfferFilterDecision {
    if is_self_offer(mime_types) {
        return OfferFilterDecision::SkipSelf;
    }
    if is_sensitive_offer(mime_types) {
        return OfferFilterDecision::SkipSensitiveMime;
    }
    if paused {
        return OfferFilterDecision::SkipPaused;
    }
    if is_ignored_app(focused_app, ignored_apps) {
        return OfferFilterDecision::SkipIgnoredApp;
    }
    OfferFilterDecision::Allow
}

/// Tracks the most recent clipboard event so keep-alive never resurrects a
/// sensitive item or one cleared within 1 second (e.g. password manager timeout).
#[derive(Debug, Default, Clone)]
pub struct TransientClearGuard {
    last_event: Option<LastOfferState>,
}

#[derive(Debug, Clone, Copy)]
struct LastOfferState {
    at: Instant,
    sensitive_or_ignored: bool,
}

impl TransientClearGuard {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_allowed(&mut self, at: Instant) {
        self.last_event = Some(LastOfferState {
            at,
            sensitive_or_ignored: false,
        });
    }

    pub fn record_sensitive_or_ignored(&mut self, at: Instant) {
        self.last_event = Some(LastOfferState {
            at,
            sensitive_or_ignored: true,
        });
    }

    /// Returns true if keep-alive is permitted when the clipboard is cleared at `now`.
    #[must_use]
    pub fn allow_keep_alive(&self, now: Instant) -> bool {
        let Some(last) = self.last_event else {
            return true;
        };
        if last.sensitive_or_ignored {
            return false;
        }
        now.saturating_duration_since(last.at) >= TRANSIENT_CLEAR_WINDOW
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretDetection {
    pub reason: &'static str,
    pub masked_preview: String,
    pub ttl_ms: i64,
}

/// Optional heuristic (off by default) that detects likely API keys, private
/// keys, or high-entropy single tokens so they can be masked and expired in 60 s.
#[must_use]
pub fn detect_secret(text: &str) -> Option<SecretDetection> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.contains("-----BEGIN ") && trimmed.contains("PRIVATE KEY-----") {
        return Some(SecretDetection {
            reason: "private_key",
            masked_preview: "•••••••• (private key, expires in 60s)".to_string(),
            ttl_ms: SECRET_EXPIRY_MS,
        });
    }

    const PREFIXES: &[&str] = &[
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "sk-",
        "AKIA",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxr-",
        "xoxs-",
    ];

    if !trimmed.chars().any(char::is_whitespace) {
        for prefix in PREFIXES {
            if trimmed.starts_with(prefix) && trimmed.len() >= prefix.len() + 12 {
                let shown: String = trimmed.chars().take(prefix.len() + 2).collect();
                return Some(SecretDetection {
                    reason: "api_key",
                    masked_preview: format!("{shown}•••••••• (secret, expires in 60s)"),
                    ttl_ms: SECRET_EXPIRY_MS,
                });
            }
        }

        // High-entropy single token (exclude URLs and filesystem paths).
        if trimmed.len() >= 32
            && !trimmed.starts_with("http://")
            && !trimmed.starts_with("https://")
            && !trimmed.starts_with("ftp://")
            && !trimmed.starts_with("mailto:")
            && !trimmed.starts_with('/')
            && !trimmed.starts_with("~/")
            && has_mixed_classes(trimmed)
            && shannon_entropy(trimmed) >= 4.0
        {
            return Some(SecretDetection {
                reason: "high_entropy_token",
                masked_preview: "•••••••• (suspected secret, expires in 60s)".to_string(),
                ttl_ms: SECRET_EXPIRY_MS,
            });
        }
    }

    None
}

fn has_mixed_classes(s: &str) -> bool {
    let lower = s.chars().any(|c| c.is_ascii_lowercase());
    let upper = s.chars().any(|c| c.is_ascii_uppercase());
    let digit = s.chars().any(|c| c.is_ascii_digit());
    let symbol = s.chars().any(|c| !c.is_ascii_alphanumeric());
    let count = usize::from(lower) + usize::from(upper) + usize::from(digit) + usize::from(symbol);
    count >= 3
}

fn shannon_entropy(s: &str) -> f64 {
    let mut counts = [0usize; 256];
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return 0.0;
    }
    for &b in bytes {
        counts[usize::from(b)] += 1;
    }
    let len = bytes.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}
