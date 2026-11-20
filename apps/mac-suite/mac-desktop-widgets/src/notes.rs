// SPDX-License-Identifier: GPL-3.0-only
//! Persistent checklist for the Notes widget (`~/.config/mac-suite/notes.ron`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Longest accepted item, in characters.
pub const MAX_ITEM_CHARS: usize = 120;
/// Maximum number of stored items.
pub const MAX_ITEMS: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteItem {
    pub text: String,
    #[serde(default)]
    pub done: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notes {
    #[serde(default)]
    pub items: Vec<NoteItem>,
}

/// Path to `~/.config/mac-suite/notes.ron`.
pub fn notes_path() -> Option<PathBuf> {
    mac_widgets::settings::config_dir().map(|dir| dir.join("notes.ron"))
}

impl Notes {
    pub fn load() -> Self {
        notes_path()
            .and_then(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        ron::from_str(&text).ok()
    }

    pub fn save(&self) {
        if let Some(path) = notes_path()
            && let Err(err) = self.save_to(&path)
        {
            tracing::error!(?err, ?path, "Failed to save notes.ron");
        }
    }

    /// Atomic write: temp file in the same directory, then rename.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let pretty = ron::ser::PrettyConfig::new().struct_names(false);
        let body = ron::ser::to_string_pretty(self, pretty).map_err(io::Error::other)?;
        let tmp = path.with_extension(format!("ron.{}.tmp", std::process::id()));
        fs::write(&tmp, body)?;
        if let Err(err) = fs::rename(&tmp, path) {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
        Ok(())
    }

    /// Appends a trimmed, length-limited item. Returns `false` if rejected.
    pub fn add(&mut self, text: &str) -> bool {
        let text: String = text.trim().chars().take(MAX_ITEM_CHARS).collect();
        if text.is_empty() || self.items.len() >= MAX_ITEMS {
            return false;
        }
        self.items.push(NoteItem { text, done: false });
        true
    }

    pub fn toggle(&mut self, index: usize) -> bool {
        match self.items.get_mut(index) {
            Some(item) => {
                item.done = !item.done;
                true
            }
            None => false,
        }
    }

    pub fn remove(&mut self, index: usize) -> bool {
        if index < self.items.len() {
            self.items.remove(index);
            true
        } else {
            false
        }
    }

    /// Indices to display: open items first (stable), then completed ones.
    pub fn display_order(&self) -> Vec<usize> {
        let open = self.items.iter().enumerate().filter(|(_, i)| !i.done);
        let done = self.items.iter().enumerate().filter(|(_, i)| i.done);
        open.chain(done).map(|(idx, _)| idx).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_trims_limits_and_rejects_empty() {
        let mut n = Notes::default();
        assert!(!n.add("   "));
        assert!(n.add("  milk  "));
        assert_eq!(n.items[0].text, "milk");
        assert!(n.add(&"x".repeat(500)));
        assert_eq!(n.items[1].text.chars().count(), MAX_ITEM_CHARS);
    }

    #[test]
    fn toggle_remove_and_order() {
        let mut n = Notes::default();
        for t in ["a", "b", "c"] {
            n.add(t);
        }
        assert!(n.toggle(0));
        assert_eq!(n.display_order(), vec![1, 2, 0]);
        assert!(!n.toggle(9));
        assert!(n.remove(1));
        assert!(!n.remove(5));
        assert_eq!(n.items.len(), 2);
    }

    #[test]
    fn save_load_roundtrip_is_atomic() {
        let dir = std::env::temp_dir().join(format!("mac-notes-test-{}", std::process::id()));
        let path = dir.join("notes.ron");
        let mut n = Notes::default();
        n.add("one");
        n.add("two");
        n.toggle(1);
        n.save_to(&path).unwrap();
        assert_eq!(Notes::load_from(&path).unwrap(), n);
        let leftovers = fs::read_dir(&dir)
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_minimal_file() {
        let n: Notes = ron::from_str(r#"(items: [(text: "x")])"#).unwrap();
        assert!(!n.items[0].done);
    }
}
