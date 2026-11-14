// SPDX-License-Identifier: GPL-3.0-only

//! Fuzzy search (`nucleo-matcher`) and filter pills for clipboard items.

use std::cmp::Reverse;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use url::Url;

use crate::model::{ClipboardItem, ItemKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterPill {
    #[default]
    All,
    Text,
    Images,
    Links,
    Files,
    Pinned,
}

impl FilterPill {
    pub const ALL: [Self; 6] = [
        Self::All,
        Self::Text,
        Self::Images,
        Self::Links,
        Self::Files,
        Self::Pinned,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Text => "Text",
            Self::Images => "Images",
            Self::Links => "Links",
            Self::Files => "Files",
            Self::Pinned => "Pinned",
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Text => "text",
            Self::Images => "images",
            Self::Links => "links",
            Self::Files => "files",
            Self::Pinned => "pinned",
        }
    }

    #[must_use]
    pub fn from_filter_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "text" | "rich_text" => Self::Text,
            "image" | "images" => Self::Images,
            "link" | "links" => Self::Links,
            "file" | "files" => Self::Files,
            "pin" | "pinned" => Self::Pinned,
            _ => Self::All,
        }
    }

    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::All => Self::Text,
            Self::Text => Self::Images,
            Self::Images => Self::Links,
            Self::Links => Self::Files,
            Self::Files => Self::Pinned,
            Self::Pinned => Self::All,
        }
    }

    #[must_use]
    pub const fn prev(self) -> Self {
        match self {
            Self::All => Self::Pinned,
            Self::Text => Self::All,
            Self::Images => Self::Text,
            Self::Links => Self::Images,
            Self::Files => Self::Links,
            Self::Pinned => Self::Files,
        }
    }

    #[must_use]
    pub fn matches(self, item: &ClipboardItem) -> bool {
        match self {
            Self::All => true,
            Self::Text => matches!(item.kind, ItemKind::Text | ItemKind::RichText),
            Self::Images => item.kind == ItemKind::Image,
            Self::Links => item.kind == ItemKind::Link,
            Self::Files => item.kind == ItemKind::Files,
            Self::Pinned => item.pinned,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchHit<'a> {
    pub item: &'a ClipboardItem,
    pub tier: u8,
    pub score: u32,
    pub matched_indices: Vec<u32>,
}

/// Filters `items` by `filter`, optionally performs fuzzy matching against
/// previews, file names, link hosts, and `source_app`, and ranks by
/// match tier (exact > prefix > fuzzy), nucleo score, pinned status, and recency.
#[must_use]
pub fn search_items<'a>(
    items: &'a [ClipboardItem],
    filter: FilterPill,
    query: &str,
    limit: usize,
) -> Vec<SearchHit<'a>> {
    let q = query.trim();
    if q.is_empty() {
        let iter = items
            .iter()
            .filter(|item| filter.matches(item))
            .map(|item| SearchHit {
                item,
                tier: 0,
                score: 0,
                matched_indices: Vec::new(),
            });
        return if limit > 0 {
            iter.take(limit).collect()
        } else {
            iter.collect()
        };
    }

    let q_lower = q.to_ascii_lowercase();
    let pattern = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut hits = Vec::new();

    for item in items.iter().filter(|i| filter.matches(i)) {
        let fields = searchable_fields(item);
        let mut best_score: Option<u32> = None;
        let mut best_tier: u8 = 0;

        for field in &fields {
            buf.clear();
            let hay = Utf32Str::new(field, &mut buf);
            if let Some(score) = pattern.score(hay, &mut matcher) {
                let field_lower = field.to_ascii_lowercase();
                let tier = if field_lower == q_lower {
                    3
                } else if field_lower.starts_with(&q_lower) {
                    2
                } else {
                    1
                };
                if tier > best_tier || (tier == best_tier && score > best_score.unwrap_or(0)) {
                    best_tier = tier;
                }
                best_score = Some(best_score.map_or(score, |prev| prev.max(score)));
            }
        }

        if let Some(score) = best_score {
            let mut indices = Vec::new();
            buf.clear();
            let preview_hay = Utf32Str::new(&item.preview, &mut buf);
            let _ = pattern.indices(preview_hay, &mut matcher, &mut indices);
            indices.sort_unstable();
            indices.dedup();
            hits.push(SearchHit {
                item,
                tier: best_tier,
                score,
                matched_indices: indices,
            });
        }
    }

    hits.sort_by_key(|h| {
        (
            Reverse(h.tier),
            Reverse(h.score),
            Reverse(h.item.pinned),
            Reverse(h.item.last_used),
        )
    });

    if limit > 0 && hits.len() > limit {
        hits.truncate(limit);
    }
    hits
}

fn searchable_fields(item: &ClipboardItem) -> Vec<String> {
    let mut fields = Vec::with_capacity(4);
    if !item.preview.is_empty() {
        fields.push(item.preview.clone());
    }
    if item.kind == ItemKind::Link
        && let Ok(url) = Url::parse(item.preview.trim())
        && let Some(host) = url.host_str()
    {
        fields.push(host.to_string());
    }
    if let Some(files) = &item.files_meta {
        for path in &files.paths {
            if let Some(name) = std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
            {
                fields.push(name.to_string());
            }
            fields.push(path.clone());
        }
    }
    if let Some(app) = &item.source_app {
        fields.push(app.clone());
    }
    fields
}
