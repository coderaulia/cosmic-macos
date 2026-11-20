// SPDX-License-Identifier: GPL-3.0-only

//! Persistent grid layout (`~/.config/mac-launchpad/layout.ron`), merge logic,
//! folder operations, and freedesktop category naming.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::apps::{self, AppEntry};

pub const CURRENT_VERSION: u32 = 1;
pub const FOLDER_PAGE_SIZE: usize = 12;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutItem {
    App(String),
    Folder { name: String, apps: Vec<String> },
}

impl LayoutItem {
    /// Stable identifier key for tracking per-tile slide animations.
    pub fn anim_key(&self) -> String {
        match self {
            LayoutItem::App(id) => format!("app:{}", apps::normalize_desktop_id(id)),
            LayoutItem::Folder { name, apps } => {
                let first = apps
                    .first()
                    .map(|a| apps::normalize_desktop_id(a))
                    .unwrap_or_default();
                format!("folder:{name}:{first}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedLayout {
    pub version: u32,
    pub pages: Vec<Vec<LayoutItem>>,
    /// Normalized ids of apps hidden from Launchpad (never uninstalled).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    /// Normalized ids of apps already known/launched. `None` means a layout
    /// written before this field existed: everything installed counts as seen.
    #[serde(default)]
    pub seen: Option<Vec<String>>,
}

impl Default for SavedLayout {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            pages: vec![Vec::new()],
            hidden: Vec::new(),
            seen: None,
        }
    }
}

/// Path to `~/.config/mac-launchpad/layout.ron` (respecting `$XDG_CONFIG_HOME`).
pub fn layout_path() -> Option<PathBuf> {
    if let Some(cfg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(cfg).join("mac-launchpad/layout.ron"));
    }
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(home).join(".config/mac-launchpad/layout.ron"))
}

pub fn load_from_path(path: &Path) -> Option<SavedLayout> {
    let text = fs::read_to_string(path).ok()?;
    let parsed: SavedLayout = ron::from_str(&text).ok()?;
    (parsed.version == CURRENT_VERSION).then_some(parsed)
}

/// Atomically writes `layout` to `path` via a temp file in the same directory
/// followed by `rename`.
pub fn save_atomic(path: &Path, layout: &SavedLayout) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let pretty = ron::ser::PrettyConfig::new()
        .depth_limit(4)
        .struct_names(false);
    let data = ron::ser::to_string_pretty(layout, pretty)
        .map_err(|e| std::io::Error::other(format!("ron serialize failed: {e}")))?;

    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.tmp.{}.{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("layout.ron"),
        std::process::id(),
        seq
    );
    let tmp_path = parent.join(tmp_name);

    let write_res = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(data.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    })();

    if write_res.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    write_res
}

fn sanitize_folder_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "Folder".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Maps normalized desktop id -> index in `available`.
pub fn build_app_index(available: &[AppEntry]) -> HashMap<String, usize> {
    let mut map = HashMap::with_capacity(available.len());
    for (idx, app) in available.iter().enumerate() {
        map.entry(apps::normalize_desktop_id(&app.id))
            .or_insert(idx);
    }
    map
}

/// Merges a saved layout against the currently installed `available` apps:
/// - Uninstalled apps (not in `available`) are dropped silently.
/// - Duplicate references to the same app keep only the first occurrence.
/// - Folders reduced to 1 app dissolve into a single `LayoutItem::App`; empty
///   folders are dropped.
/// - Newly installed apps not present in `saved` are appended to the last page
///   (overflowing onto additional pages of size `per_page`).
pub fn merge_layout(
    saved: Option<SavedLayout>,
    available: &[AppEntry],
    per_page: usize,
) -> SavedLayout {
    let per_page = per_page.max(1);
    let lookup = build_app_index(available);
    // Hidden apps are pre-marked as placed so neither saved pages nor the
    // "newly installed" pass put them back on the grid.
    let mut hidden: Vec<String> = Vec::new();
    let mut seen_ids: Option<Vec<String>> = None;
    let mut seen: HashSet<String> = HashSet::new();
    let mut pages: Vec<Vec<LayoutItem>> = Vec::new();

    if let Some(saved) = saved
        && saved.version == CURRENT_VERSION
    {
        for id in &saved.hidden {
            let key = apps::normalize_desktop_id(id);
            if !hidden.contains(&key) {
                seen.insert(key.clone());
                hidden.push(key);
            }
        }
        seen_ids = saved.seen.clone();
        for saved_page in saved.pages {
            let mut page = Vec::new();
            for item in saved_page {
                match item {
                    LayoutItem::App(id) => {
                        let key = apps::normalize_desktop_id(&id);
                        if let Some(&idx) = lookup.get(&key)
                            && seen.insert(key)
                        {
                            page.push(LayoutItem::App(available[idx].desktop_file_id()));
                        }
                    }
                    LayoutItem::Folder { name, apps: f_apps } => {
                        let mut kept = Vec::new();
                        for id in f_apps {
                            let key = apps::normalize_desktop_id(&id);
                            if let Some(&idx) = lookup.get(&key)
                                && seen.insert(key)
                            {
                                kept.push(available[idx].desktop_file_id());
                            }
                        }
                        match kept.len() {
                            0 => {}
                            1 => page.push(LayoutItem::App(kept.pop().expect("len is 1"))),
                            _ => page.push(LayoutItem::Folder {
                                name: sanitize_folder_name(&name),
                                apps: kept,
                            }),
                        }
                    }
                }
            }
            if !page.is_empty() {
                pages.push(page);
            }
        }
    }

    let mut newly_added: Vec<LayoutItem> = Vec::new();
    for app in available {
        let key = apps::normalize_desktop_id(&app.id);
        if seen.insert(key) {
            newly_added.push(LayoutItem::App(app.desktop_file_id()));
        }
    }

    if !newly_added.is_empty() {
        if pages.is_empty() {
            pages.push(Vec::new());
        }
        let mut iter = newly_added.into_iter().peekable();
        // Fill remaining capacity on the last page first.
        if let Some(last) = pages.last_mut() {
            while last.len() < per_page {
                let Some(next) = iter.next() else {
                    break;
                };
                last.push(next);
            }
        }
        while iter.peek().is_some() {
            let chunk: Vec<LayoutItem> = iter.by_ref().take(per_page).collect();
            pages.push(chunk);
        }
    }

    if pages.is_empty() {
        pages.push(Vec::new());
    }

    // First run (or a pre-`seen` layout): everything installed now is known.
    let seen_ids = seen_ids.unwrap_or_else(|| {
        let mut all: Vec<String> = lookup.keys().cloned().collect();
        all.sort();
        all
    });

    SavedLayout {
        version: CURRENT_VERSION,
        pages,
        hidden,
        seen: Some(seen_ids),
    }
}

/// Builds a fresh default A-Z layout with no folders.
pub fn default_layout(available: &[AppEntry], per_page: usize) -> SavedLayout {
    merge_layout(None, available, per_page)
}

/// Sorts top-level items (and apps inside each folder) A-Z by display name,
/// preserving existing folders and repaginating by `per_page`.
pub fn sort_az(layout: &SavedLayout, available: &[AppEntry], per_page: usize) -> SavedLayout {
    let per_page = per_page.max(1);
    let lookup = build_app_index(available);

    let display_name = |id: &str| -> String {
        let key = apps::normalize_desktop_id(id);
        lookup
            .get(&key)
            .map(|&idx| available[idx].name.to_lowercase())
            .unwrap_or_else(|| key)
    };

    let mut all_items: Vec<LayoutItem> = Vec::new();
    for page in &layout.pages {
        for item in page {
            match item {
                LayoutItem::App(id) => all_items.push(LayoutItem::App(id.clone())),
                LayoutItem::Folder { name, apps } => {
                    let mut sorted_apps = apps.clone();
                    sorted_apps.sort_by(|a, b| {
                        display_name(a).cmp(&display_name(b)).then_with(|| a.cmp(b))
                    });
                    all_items.push(LayoutItem::Folder {
                        name: name.clone(),
                        apps: sorted_apps,
                    });
                }
            }
        }
    }

    all_items.sort_by(|a, b| {
        let na = match a {
            LayoutItem::App(id) => display_name(id),
            LayoutItem::Folder { name, .. } => name.to_lowercase(),
        };
        let nb = match b {
            LayoutItem::App(id) => display_name(id),
            LayoutItem::Folder { name, .. } => name.to_lowercase(),
        };
        na.cmp(&nb)
    });

    let pages = if all_items.is_empty() {
        vec![Vec::new()]
    } else {
        all_items.chunks(per_page).map(|c| c.to_vec()).collect()
    };

    SavedLayout {
        version: CURRENT_VERSION,
        pages,
        hidden: layout.hidden.clone(),
        seen: layout.seen.clone(),
    }
}

/// Folder order used by [`organize_by_category`].
const CATEGORY_ORDER: [&str; 9] = [
    "Productivity",
    "Internet",
    "Media",
    "Graphics",
    "Developer",
    "Games",
    "Education",
    "Utilities",
    "System",
];

/// The Launchpad folder an app belongs in, from its freedesktop categories.
pub fn app_folder(categories: &[String]) -> Option<&'static str> {
    // Specific categories (e.g. "TerminalEmulator") win over broad main ones
    // (e.g. "System"), so look at the non-main categories first.
    const MAIN: [&str; 11] = [
        "AudioVideo",
        "Audio",
        "Video",
        "Development",
        "Education",
        "Game",
        "Graphics",
        "Network",
        "Office",
        "Settings",
        "System",
    ];
    let mapped = |main: bool| {
        categories
            .iter()
            .map(|c| c.trim())
            .filter(|c| MAIN.contains(c) == main)
            .find_map(map_freedesktop_category)
    };
    // "Utility" is a main category too, but it is the macOS catch-all, so it
    // only applies when nothing more specific matched.
    let main_first = categories
        .iter()
        .any(|c| c.trim() != "Utility" && MAIN.contains(&c.trim()));
    if main_first {
        mapped(true).or_else(|| mapped(false))
    } else {
        mapped(false).or_else(|| mapped(true))
    }
}

/// Puts every visible app into a folder named after its category (macOS
/// "Utilities", "Games", ...). Categories with a single app and apps without a
/// known category stay loose. Folders come first in a fixed order, then loose
/// apps A-Z. Hidden apps and the seen list are kept.
pub fn organize_by_category(
    layout: &SavedLayout,
    available: &[AppEntry],
    per_page: usize,
) -> SavedLayout {
    let per_page = per_page.max(1);
    let lookup = build_app_index(available);
    let entry = |id: &str| {
        lookup
            .get(&apps::normalize_desktop_id(id))
            .map(|&i| &available[i])
    };
    let name = |id: &str| {
        entry(id)
            .map(|a| a.name.to_lowercase())
            .unwrap_or_else(|| id.to_owned())
    };

    let mut ids: Vec<String> = Vec::new();
    for page in &layout.pages {
        for item in page {
            match item {
                LayoutItem::App(id) => ids.push(id.clone()),
                LayoutItem::Folder { apps, .. } => ids.extend(apps.iter().cloned()),
            }
        }
    }

    let mut groups: HashMap<&'static str, Vec<String>> = HashMap::new();
    let mut loose: Vec<String> = Vec::new();
    for id in ids {
        match entry(&id).and_then(|a| app_folder(&a.categories)) {
            Some(folder) => groups.entry(folder).or_default().push(id),
            None => loose.push(id),
        }
    }

    let mut items: Vec<LayoutItem> = Vec::new();
    for folder in CATEGORY_ORDER {
        let Some(mut apps) = groups.remove(folder) else {
            continue;
        };
        if apps.len() == 1 {
            loose.append(&mut apps);
            continue;
        }
        apps.sort_by_key(|a| name(a));
        items.push(LayoutItem::Folder {
            name: folder.to_owned(),
            apps,
        });
    }
    loose.sort_by_key(|a| name(a));
    items.extend(loose.into_iter().map(LayoutItem::App));

    let pages = if items.is_empty() {
        vec![Vec::new()]
    } else {
        items.chunks(per_page).map(|c| c.to_vec()).collect()
    };
    SavedLayout {
        version: CURRENT_VERSION,
        pages,
        hidden: layout.hidden.clone(),
        seen: layout.seen.clone(),
    }
}

/// Maps a freedesktop category string to a Launchpad folder display name.
pub fn map_freedesktop_category(cat: &str) -> Option<&'static str> {
    match cat {
        "Utility" | "Archiving" | "Compression" | "Calculator" | "Clock" | "TextTools"
        | "Accessibility" => Some("Utilities"),
        "Development" | "IDE" | "Debugger" | "Building" | "GUIDesigner" | "Profiling"
        | "RevisionControl" | "Translation" | "WebDevelopment" => Some("Developer"),
        "Game" | "ActionGame" | "AdventureGame" | "ArcadeGame" | "BoardGame" | "BlocksGame"
        | "CardGame" | "KidsGame" | "LogicGame" | "RolePlaying" | "Shooter" | "Simulation"
        | "SportsGame" | "StrategyGame" => Some("Games"),
        "Graphics" | "2DGraphics" | "3DGraphics" | "RasterGraphics" | "VectorGraphics"
        | "Photography" | "Scanning" | "OCR" | "Publishing" | "Viewer" => Some("Graphics"),
        "AudioVideo" | "Audio" | "Video" | "Midi" | "Mixer" | "Sequencer" | "Tuner" | "TV"
        | "AudioVideoEditing" | "Player" | "Recorder" | "DiscBurning" | "Music" => Some("Media"),
        "Network" | "WebBrowser" | "Email" | "Chat" | "InstantMessaging" | "FileTransfer"
        | "IRCClient" | "Feed" | "News" | "P2P" | "RemoteAccess" | "Telephony"
        | "VideoConference" => Some("Internet"),
        "Office" | "Calendar" | "ContactManagement" | "Database" | "Dictionary" | "Chart"
        | "Finance" | "FlowChart" | "PDA" | "ProjectManagement" | "Presentation"
        | "Spreadsheet" | "WordProcessor" => Some("Productivity"),
        "Education" | "Art" | "Construction" | "Languages" | "Literature" | "Science"
        | "Astronomy" | "Biology" | "Chemistry" | "ComputerScience" | "DataVisualization"
        | "Economy" | "Electricity" | "Geography" | "Geology" | "Geoscience" | "History"
        | "Humanities" | "ImageProcessing" | "Math" | "MedicalSoftware" | "NumericalAnalysis"
        | "ParallelComputing" | "Physics" | "Robotics" | "Spirituality" | "Sports" => {
            Some("Education")
        }
        "System" | "Settings" | "DesktopSettings" | "HardwareSettings" | "PackageManager"
        | "Emulator" | "FileManager" | "FileTools" | "Filesystem" | "Monitor" | "Security"
        | "TerminalEmulator" => Some("System"),
        _ => None,
    }
}

fn is_generic_category(cat: &str) -> bool {
    let lc = cat.to_ascii_lowercase();
    matches!(
        lc.as_str(),
        "" | "application" | "gtk" | "qt" | "cosmic" | "gnome" | "kde" | "xfce" | "consoleonly"
    ) || lc.starts_with("x-")
}

/// Computes the default folder name when combining two apps using their
/// freedesktop categories (shared mapped category first, then shared raw
/// category, then target's mapped category, then dragged's, then `"Folder"`).
pub fn default_folder_name(cats_target: &[String], cats_dragged: &[String]) -> String {
    let mapped_target: Vec<&'static str> = cats_target
        .iter()
        .filter_map(|c| map_freedesktop_category(c.trim()))
        .collect();
    let mapped_dragged: Vec<&'static str> = cats_dragged
        .iter()
        .filter_map(|c| map_freedesktop_category(c.trim()))
        .collect();

    for m in &mapped_target {
        if mapped_dragged.contains(m) {
            return (*m).to_owned();
        }
    }

    for ct in cats_target {
        let ct_trimmed = ct.trim();
        if is_generic_category(ct_trimmed) {
            continue;
        }
        if cats_dragged
            .iter()
            .any(|cd| cd.trim().eq_ignore_ascii_case(ct_trimmed))
        {
            return ct_trimmed.to_owned();
        }
    }

    if let Some(&first) = mapped_target.first() {
        return first.to_owned();
    }
    if let Some(&first) = mapped_dragged.first() {
        return first.to_owned();
    }

    "Folder".to_owned()
}

impl SavedLayout {
    /// `true` if `id` is installed but has not been seen/launched yet.
    pub fn is_new(&self, id: &str) -> bool {
        let key = apps::normalize_desktop_id(id);
        self.seen.as_ref().is_some_and(|s| !s.contains(&key))
    }

    /// Records `id` as seen. Returns `true` if the layout changed.
    pub fn mark_seen(&mut self, id: &str) -> bool {
        let key = apps::normalize_desktop_id(id);
        let seen = self.seen.get_or_insert_with(Vec::new);
        if seen.contains(&key) {
            return false;
        }
        seen.push(key);
        true
    }

    /// Hides `id` from the grid (removing it from pages and folders) and
    /// remembers it in `hidden`. Returns `true` if anything changed.
    pub fn hide_app(&mut self, id: &str) -> bool {
        let key = apps::normalize_desktop_id(id);
        let mut changed = false;
        if !self.hidden.contains(&key) {
            self.hidden.push(key.clone());
            changed = true;
        }
        for page in &mut self.pages {
            let before = page.len();
            let mut next = Vec::with_capacity(before);
            for item in page.drain(..) {
                match item {
                    LayoutItem::App(a) if apps::normalize_desktop_id(&a) == key => {
                        changed = true;
                    }
                    LayoutItem::Folder { name, apps: mut f } => {
                        let n = f.len();
                        f.retain(|a| apps::normalize_desktop_id(a) != key);
                        changed |= f.len() != n;
                        match f.len() {
                            0 => {}
                            1 => next.push(LayoutItem::App(f.remove(0))),
                            _ => next.push(LayoutItem::Folder { name, apps: f }),
                        }
                    }
                    other => next.push(other),
                }
            }
            *page = next;
        }
        self.prune_empty_pages();
        changed
    }

    /// Clears the hidden list; call `merge_layout` afterwards to re-place them.
    pub fn clear_hidden(&mut self) -> bool {
        let changed = !self.hidden.is_empty();
        self.hidden.clear();
        changed
    }

    /// Removes completely empty pages while always retaining at least one page.
    pub fn prune_empty_pages(&mut self) {
        self.pages.retain(|p| !p.is_empty());
        if self.pages.is_empty() {
            self.pages.push(Vec::new());
        }
    }

    /// Cascades excess items from `start_page` onward so no page exceeds `per_page`.
    fn cascade_overflow(&mut self, start_page: usize, per_page: usize) {
        let per_page = per_page.max(1);
        let mut p = start_page;
        while p < self.pages.len() {
            if self.pages[p].len() <= per_page {
                break;
            }
            let overflow: Vec<LayoutItem> = self.pages[p].drain(per_page..).collect();
            if p + 1 >= self.pages.len() {
                self.pages.push(overflow);
            } else {
                let next = &mut self.pages[p + 1];
                for (i, item) in overflow.into_iter().enumerate() {
                    next.insert(i, item);
                }
            }
            p += 1;
        }
    }

    /// Moves an item from `(from_page, from_idx)` to `(to_page, to_idx)`.
    pub fn move_item(
        &mut self,
        from_page: usize,
        from_idx: usize,
        to_page: usize,
        to_idx: usize,
        per_page: usize,
    ) -> bool {
        let Some(src_page) = self.pages.get_mut(from_page) else {
            return false;
        };
        if from_idx >= src_page.len() {
            return false;
        }
        let item = src_page.remove(from_idx);

        while self.pages.len() <= to_page {
            self.pages.push(Vec::new());
        }

        let target_len = self.pages[to_page].len();
        let insert_idx = to_idx.min(target_len);
        self.pages[to_page].insert(insert_idx, item);
        self.cascade_overflow(to_page, per_page);
        self.prune_empty_pages();
        true
    }

    /// Merges an `App` at `(from_page, from_idx)` into the item at
    /// `(target_page, target_idx)` (creating a new folder or adding to an
    /// existing folder). Returns the resulting `(page, index)` of the folder.
    pub fn merge_into_folder(
        &mut self,
        from_page: usize,
        from_idx: usize,
        target_page: usize,
        target_idx: usize,
        available: &[AppEntry],
    ) -> Option<(usize, usize)> {
        if (from_page, from_idx) == (target_page, target_idx) {
            return None;
        }
        let dragged_id = match self.pages.get(from_page)?.get(from_idx)? {
            LayoutItem::App(id) => id.clone(),
            LayoutItem::Folder { .. } => return None,
        };
        // Verify target exists and is valid before mutating.
        let target_snapshot = self.pages.get(target_page)?.get(target_idx)?.clone();

        let lookup = build_app_index(available);
        let cats_for = |id: &str| -> &[String] {
            lookup
                .get(&apps::normalize_desktop_id(id))
                .map(|&i| available[i].categories.as_slice())
                .unwrap_or(&[])
        };

        let new_target = match target_snapshot {
            LayoutItem::App(target_id) => {
                let name = default_folder_name(cats_for(&target_id), cats_for(&dragged_id));
                LayoutItem::Folder {
                    name,
                    apps: vec![target_id, dragged_id],
                }
            }
            LayoutItem::Folder { name, mut apps } => {
                let dragged_key = apps::normalize_desktop_id(&dragged_id);
                if !apps
                    .iter()
                    .any(|a| apps::normalize_desktop_id(a) == dragged_key)
                {
                    apps.push(dragged_id);
                }
                LayoutItem::Folder { name, apps }
            }
        };

        self.pages[target_page][target_idx] = new_target;
        self.pages[from_page].remove(from_idx);

        let mut adjusted_page = target_page;
        let mut adjusted_idx = target_idx;
        if from_page == target_page && from_idx < target_idx {
            adjusted_idx -= 1;
        }
        if from_page < target_page && self.pages[from_page].is_empty() {
            adjusted_page -= 1;
        }

        self.prune_empty_pages();
        Some((
            adjusted_page.min(self.pages.len().saturating_sub(1)),
            adjusted_idx,
        ))
    }

    /// Reorders an app within an open folder.
    pub fn reorder_in_folder(
        &mut self,
        folder_page: usize,
        folder_idx: usize,
        from_app_idx: usize,
        to_app_idx: usize,
    ) -> bool {
        let Some(LayoutItem::Folder { apps, .. }) = self
            .pages
            .get_mut(folder_page)
            .and_then(|p| p.get_mut(folder_idx))
        else {
            return false;
        };
        if from_app_idx >= apps.len() {
            return false;
        }
        let app = apps.remove(from_app_idx);
        let insert_idx = to_app_idx.min(apps.len());
        apps.insert(insert_idx, app);
        true
    }

    /// Moves an app out of an open folder onto the main grid at `(to_page, to_idx)`.
    /// If the folder is left with 1 app, it dissolves back into a single `App`.
    pub fn move_out_of_folder(
        &mut self,
        folder_page: usize,
        folder_idx: usize,
        app_idx: usize,
        to_page: usize,
        to_idx: usize,
        per_page: usize,
    ) -> bool {
        let Some(page_items) = self.pages.get_mut(folder_page) else {
            return false;
        };
        let Some(LayoutItem::Folder { apps, .. }) = page_items.get_mut(folder_idx) else {
            return false;
        };
        if app_idx >= apps.len() {
            return false;
        }
        let moved_id = apps.remove(app_idx);
        let dissolved_into_removed = match apps.len() {
            0 => {
                page_items.remove(folder_idx);
                true
            }
            1 => {
                let remaining = apps.pop().expect("len is 1");
                page_items[folder_idx] = LayoutItem::App(remaining);
                false
            }
            _ => false,
        };

        while self.pages.len() <= to_page {
            self.pages.push(Vec::new());
        }
        let mut insert_idx = to_idx;
        if dissolved_into_removed && folder_page == to_page && folder_idx < insert_idx {
            insert_idx = insert_idx.saturating_sub(1);
        }
        insert_idx = insert_idx.min(self.pages[to_page].len());
        self.pages[to_page].insert(insert_idx, LayoutItem::App(moved_id));
        self.cascade_overflow(to_page, per_page);
        self.prune_empty_pages();
        true
    }

    /// Updates a folder's title. Empty/whitespace names fall back to `"Folder"`
    /// when `finalize` is true.
    pub fn set_folder_name(
        &mut self,
        folder_page: usize,
        folder_idx: usize,
        name: String,
        finalize: bool,
    ) -> bool {
        let Some(LayoutItem::Folder { name: target, .. }) = self
            .pages
            .get_mut(folder_page)
            .and_then(|p| p.get_mut(folder_idx))
        else {
            return false;
        };
        *target = if finalize {
            sanitize_folder_name(&name)
        } else {
            name
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_apps() -> Vec<AppEntry> {
        vec![
            AppEntry::for_test("firefox", "Firefox", &["Network", "WebBrowser"]),
            AppEntry::for_test("org.gnome.Calculator", "Calculator", &["Utility"]),
            AppEntry::for_test("org.gnome.TextEditor", "Text Editor", &["Utility"]),
            AppEntry::for_test("code", "VS Code", &["Development", "IDE"]),
            AppEntry::for_test("rustrover", "RustRover", &["Development"]),
        ]
    }

    #[test]
    fn ron_roundtrip_matches_expected_schema() {
        let sample = r#"(
            version: 1,
            pages: [
                [
                    App("firefox.desktop"),
                    Folder(name: "Utilities", apps: ["org.gnome.Calculator.desktop", "org.gnome.TextEditor.desktop"]),
                ],
            ],
        )"#;
        let parsed: SavedLayout = ron::from_str(sample).expect("parse sample ron");
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.pages.len(), 1);
        assert_eq!(
            parsed.pages[0][1],
            LayoutItem::Folder {
                name: "Utilities".into(),
                apps: vec![
                    "org.gnome.Calculator.desktop".into(),
                    "org.gnome.TextEditor.desktop".into()
                ]
            }
        );

        let dir = std::env::temp_dir().join(format!("mac-launchpad-test-{}", std::process::id()));
        let path = dir.join("layout.ron");
        save_atomic(&path, &parsed).expect("atomic save");
        let reloaded = load_from_path(&path).expect("reload saved ron");
        assert_eq!(reloaded, parsed);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_drops_uninstalled_dedupes_dissolves_single_and_appends_new() {
        let apps = test_apps();
        let saved = SavedLayout {
            version: CURRENT_VERSION,
            hidden: Vec::new(),
            seen: None,
            pages: vec![vec![
                LayoutItem::App("firefox.desktop".into()),
                // Duplicate firefox should be dropped
                LayoutItem::App("firefox".into()),
                // Uninstalled app should be dropped silently
                LayoutItem::App("gone-app.desktop".into()),
                // Folder with one installed + one uninstalled -> dissolves to App
                LayoutItem::Folder {
                    name: "Utilities".into(),
                    apps: vec![
                        "org.gnome.Calculator.desktop".into(),
                        "uninstalled.desktop".into(),
                    ],
                },
            ]],
        };

        // per_page = 3 so newly added apps overflow cleanly onto page 2
        let merged = merge_layout(Some(saved), &apps, 3);
        assert_eq!(
            merged.pages,
            vec![
                vec![
                    LayoutItem::App("firefox.desktop".into()),
                    LayoutItem::App("org.gnome.Calculator.desktop".into()),
                    LayoutItem::App("org.gnome.TextEditor.desktop".into()),
                ],
                vec![
                    LayoutItem::App("code.desktop".into()),
                    LayoutItem::App("rustrover.desktop".into()),
                ],
            ]
        );
    }

    #[test]
    fn folder_creation_and_dissolution_works() {
        let apps = test_apps();
        let mut layout = default_layout(&apps, 10);
        // Combine Calculator (idx 1) and Text Editor (idx 2) -> "Utilities"
        let folder_pos = layout
            .merge_into_folder(0, 2, 0, 1, &apps)
            .expect("folder created");
        assert_eq!(folder_pos, (0, 1));
        assert_eq!(
            layout.pages[0][1],
            LayoutItem::Folder {
                name: "Utilities".into(),
                apps: vec![
                    "org.gnome.Calculator.desktop".into(),
                    "org.gnome.TextEditor.desktop".into()
                ]
            }
        );

        // Dragging one app out of the 2-app folder dissolves it back into the remaining app.
        assert!(layout.move_out_of_folder(0, 1, 1, 0, 0, 10));
        assert_eq!(
            layout.pages[0][0],
            LayoutItem::App("org.gnome.TextEditor.desktop".into())
        );
        assert_eq!(
            layout.pages[0][2],
            LayoutItem::App("org.gnome.Calculator.desktop".into())
        );
    }

    #[test]
    fn category_naming_prefers_shared_mapped_category() {
        assert_eq!(
            default_folder_name(
                &["GTK".into(), "Utility".into()],
                &["Utility".into(), "System".into()]
            ),
            "Utilities"
        );
        assert_eq!(
            default_folder_name(
                &["Development".into(), "IDE".into()],
                &["Development".into()]
            ),
            "Developer"
        );
        assert_eq!(
            default_folder_name(&["Game".into()], &["ActionGame".into()]),
            "Games"
        );
        // Falls back to target's mapped category when disjoint
        assert_eq!(
            default_folder_name(&["Graphics".into()], &["AudioVideo".into()]),
            "Graphics"
        );
        assert_eq!(default_folder_name(&[], &[]), "Folder");
    }

    #[test]
    fn move_across_pages_cascades_overflow_and_sort_az_works() {
        let apps = test_apps();
        let mut layout = default_layout(&apps, 3);
        // Page 0 has 3 items, Page 1 has 2 items.
        assert_eq!(layout.pages[0].len(), 3);
        assert_eq!(layout.pages[1].len(), 2);

        // Move item from page 1 idx 0 into page 0 idx 0 -> cascades last item of page 0 to page 1.
        assert!(layout.move_item(1, 0, 0, 0, 3));
        assert_eq!(layout.pages[0].len(), 3);
        assert_eq!(layout.pages[1].len(), 2);

        // Sort A-Z orders by app display name.
        let sorted = sort_az(&layout, &apps, 3);
        assert_eq!(
            sorted.pages[0][0],
            LayoutItem::App("org.gnome.Calculator.desktop".into())
        );
    }

    #[test]
    fn old_layout_without_new_fields_parses_and_marks_all_seen() {
        let old = r#"(version: 1, pages: [[App("firefox.desktop")]])"#;
        let parsed: SavedLayout = ron::from_str(old).expect("old ron parses");
        assert!(parsed.hidden.is_empty());
        assert!(parsed.seen.is_none());
        let merged = merge_layout(Some(parsed), &test_apps(), 10);
        for a in test_apps() {
            assert!(!merged.is_new(&a.id));
        }
    }

    #[test]
    fn newly_installed_app_is_new_until_seen_and_roundtrips() {
        let mut apps = test_apps();
        let first = merge_layout(None, &apps, 10);
        assert!(!first.is_new("firefox"));
        apps.push(AppEntry::for_test("gimp", "GIMP", &["Graphics"]));
        let mut second = merge_layout(Some(first), &apps, 10);
        assert!(second.is_new("gimp.desktop"));
        assert!(!second.is_new("firefox.desktop"));
        let text = ron::to_string(&second).unwrap();
        let mut reloaded: SavedLayout = ron::from_str(&text).unwrap();
        assert!(reloaded.is_new("GIMP"));
        assert!(reloaded.mark_seen("gimp.desktop"));
        assert!(!reloaded.mark_seen("gimp"));
        assert!(!reloaded.is_new("gimp"));
        assert!(second.mark_seen("gimp"));
    }

    #[test]
    fn hide_removes_everywhere_and_survives_merge_until_cleared() {
        let apps = test_apps();
        let mut layout = default_layout(&apps, 10);
        layout
            .merge_into_folder(0, 2, 0, 1, &apps)
            .expect("folder created");
        assert!(layout.hide_app("org.gnome.Calculator.desktop"));
        // Folder dissolved into the remaining app.
        assert!(
            layout.pages[0]
                .iter()
                .all(|i| !matches!(i, LayoutItem::Folder { .. }))
        );
        assert!(!layout.hide_app("org.gnome.Calculator"));
        assert_eq!(layout.hidden, vec!["org.gnome.calculator".to_owned()]);

        let mut merged = merge_layout(Some(layout), &apps, 10);
        let flat: Vec<_> = merged.pages.iter().flatten().collect();
        assert_eq!(flat.len(), apps.len() - 1);
        assert!(!flat.contains(&&LayoutItem::App("org.gnome.Calculator.desktop".into())));

        assert!(merged.clear_hidden());
        let restored = merge_layout(Some(merged), &apps, 10);
        assert_eq!(
            restored.pages.iter().map(Vec::len).sum::<usize>(),
            apps.len()
        );
        assert!(restored.hidden.is_empty());
    }

    #[test]
    fn hiding_last_app_on_page_prunes_page() {
        let apps = test_apps();
        let mut layout = default_layout(&apps, 2);
        let pages = layout.pages.len();
        let last = layout.pages.last().unwrap().clone();
        for item in last {
            if let LayoutItem::App(id) = item {
                layout.hide_app(&id);
            }
        }
        assert_eq!(layout.pages.len(), pages - 1);
    }

    #[test]
    fn organize_groups_apps_into_category_folders() {
        let avail = vec![
            AppEntry::for_test("firefox", "Firefox", &["Network", "WebBrowser"]),
            AppEntry::for_test("thunderbird", "Thunderbird", &["Network", "Email"]),
            AppEntry::for_test("term", "Terminal", &["System", "TerminalEmulator"]),
            AppEntry::for_test("calc", "Calculator", &["Utility", "Calculator"]),
            AppEntry::for_test("notes", "Notes", &["Utility", "TextEditor"]),
            AppEntry::for_test("steam", "Steam", &["Game"]),
            AppEntry::for_test("odd", "Odd App", &["X-Custom"]),
        ];
        let mut layout = default_layout(&avail, 30);
        layout.hidden = vec!["hidden".to_owned()];
        let out = organize_by_category(&layout, &avail, 30);
        assert_eq!(out.hidden, layout.hidden);
        let items = &out.pages[0];
        assert_eq!(
            items[0],
            LayoutItem::Folder {
                name: "Internet".into(),
                apps: vec!["firefox.desktop".into(), "thunderbird.desktop".into()],
            }
        );
        assert_eq!(
            items[1],
            LayoutItem::Folder {
                name: "Utilities".into(),
                apps: vec!["calc.desktop".into(), "notes.desktop".into()],
            }
        );
        // Single-app categories and unknown ones stay loose, A-Z.
        let loose: Vec<_> = items[2..].to_vec();
        assert_eq!(
            loose,
            vec![
                LayoutItem::App("odd.desktop".into()),
                LayoutItem::App("steam.desktop".into()),
                LayoutItem::App("term.desktop".into()),
            ]
        );
    }

    #[test]
    fn app_folder_prefers_specific_over_utility() {
        let cats = |c: &[&str]| c.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            app_folder(&cats(&["Utility", "Calculator"])),
            Some("Utilities")
        );
        assert_eq!(
            app_folder(&cats(&["GTK", "Development", "IDE"])),
            Some("Developer")
        );
        assert_eq!(
            app_folder(&cats(&["Utility", "Archiving", "Office"])),
            Some("Productivity")
        );
        assert_eq!(app_folder(&cats(&["GTK"])), None);
    }
}
