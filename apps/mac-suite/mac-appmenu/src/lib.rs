// SPDX-License-Identifier: GPL-3.0-only

//! Session D-Bus global application menu protocol (`dev.asw.AppMenu1`) and
//! ergonomic helpers for publishing and consuming app menus.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};

use futures_core::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use zbus::fdo::RequestNameFlags;
use zbus::names::WellKnownName;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::Type;
use zbus::{Connection, interface, proxy};

/// Prefix for session D-Bus names exporting `dev.asw.AppMenu1`.
pub const BUS_NAME_PREFIX: &str = "dev.asw.AppMenu.";

/// D-Bus object path exported by applications implementing `dev.asw.AppMenu1`.
pub const OBJECT_PATH: &str = "/dev/asw/AppMenu";

/// D-Bus interface name for global application menus.
pub const INTERFACE_NAME: &str = "dev.asw.AppMenu1";

/// Item ID reserved for menu separators/dividers.
pub const SEPARATOR_ID: &str = "-";

static NEXT_HANDLE_ID: AtomicU64 = AtomicU64::new(1);

/// A single item inside a top-level application menu.
///
/// D-Bus wire signature: `(sssbb)` (`item_id`, `label`, `shortcut`, `enabled`, `checked`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MenuItemSpec {
    pub id: String,
    pub label: String,
    pub shortcut: String,
    pub enabled: bool,
    pub checked: bool,
}

impl MenuItemSpec {
    /// Construct an enabled action item with no shortcut and unchecked state.
    #[must_use]
    pub fn action(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            shortcut: String::new(),
            enabled: true,
            checked: false,
        }
    }

    /// Construct a checkable action item.
    #[must_use]
    pub fn checkbox(id: impl Into<String>, label: impl Into<String>, checked: bool) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            shortcut: String::new(),
            enabled: true,
            checked,
        }
    }

    /// Construct a separator item (`id == "-"`).
    #[must_use]
    pub fn separator() -> Self {
        Self {
            id: SEPARATOR_ID.to_string(),
            label: String::new(),
            shortcut: String::new(),
            enabled: false,
            checked: false,
        }
    }

    /// Attach a formatted shortcut string (e.g. `"⌘S"` or `"Ctrl+S"` -> formatted).
    #[must_use]
    pub fn shortcut(mut self, shortcut: impl AsRef<str>) -> Self {
        self.shortcut = format_shortcut(shortcut.as_ref());
        self
    }

    /// Attach an already-formatted shortcut string verbatim.
    #[must_use]
    pub fn shortcut_raw(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = shortcut.into();
        self
    }

    /// Set whether this item is clickable/enabled.
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Set whether this item displays a checkmark.
    #[must_use]
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Returns `true` if this item represents a divider/separator.
    #[must_use]
    pub fn is_separator(&self) -> bool {
        self.id == SEPARATOR_ID
    }
}

/// A top-level menu (e.g., `File`, `Edit`, `View`) and its items.
///
/// D-Bus wire signature: `(ssa(sssbb))` (`menu_id`, `label`, `items`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MenuSpec {
    pub id: String,
    pub label: String,
    pub items: Vec<MenuItemSpec>,
}

impl MenuSpec {
    /// Create a top-level menu with the given items.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        items: Vec<MenuItemSpec>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            items: clean_separators(items),
        }
    }

    /// Create an empty top-level menu for builder-style population.
    #[must_use]
    pub fn empty(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            items: Vec::new(),
        }
    }

    /// Append a menu item, collapsing duplicate separators.
    #[must_use]
    pub fn item(mut self, item: MenuItemSpec) -> Self {
        if item.is_separator() && self.items.last().is_none_or(MenuItemSpec::is_separator) {
            return self;
        }
        self.items.push(item);
        self
    }

    /// Append a separator if the menu currently has a non-separator item at the end.
    #[must_use]
    pub fn separator(self) -> Self {
        self.item(MenuItemSpec::separator())
    }
}

/// Complete top-level menu bar layout published over `dev.asw.AppMenu1.GetLayout`.
pub type MenuLayout = Vec<MenuSpec>;

/// Modifier keys supported by shortcut formatting helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutModifier {
    Ctrl,
    Shift,
    Alt,
    Super,
}

/// Format modifier flags and a key name into a macOS-style shortcut string
/// (e.g. `[Ctrl, Shift]` + `"s"` -> `"⇧⌘S"`).
#[must_use]
pub fn format_shortcut_modifiers(modifiers: &[ShortcutModifier], key: &str) -> String {
    let has_super = modifiers.contains(&ShortcutModifier::Super);
    let has_alt = modifiers.contains(&ShortcutModifier::Alt);
    let has_shift = modifiers.contains(&ShortcutModifier::Shift);
    let has_ctrl = modifiers.contains(&ShortcutModifier::Ctrl);

    let mut out = String::new();
    if has_super {
        out.push('⌃');
    }
    if has_alt {
        out.push('⌥');
    }
    if has_shift {
        out.push('⇧');
    }
    if has_ctrl {
        out.push('⌘');
    }
    out.push_str(&format_key_token(key));
    out
}

/// Convert a COSMIC / Linux shortcut string (e.g. `"Ctrl+Shift+S"` or `"Ctrl + Space"`)
/// into a compact macOS-style shortcut label (`"⇧⌘S"`, `"⌘Space"`).
///
/// Strings that already use `⌘`/`⌥`/`⇧`/`⌃` or empty strings are returned trimmed.
#[must_use]
pub fn format_shortcut(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.contains(['⌘', '⌥', '⇧', '⌃']) {
        return trimmed.to_string();
    }

    let parts: Vec<&str> = if trimmed.contains(" + ") {
        trimmed.split(" + ").map(str::trim).collect()
    } else if trimmed.ends_with("++") || trimmed.ends_with("+ +") {
        // Handle e.g. "Ctrl++"
        let prefix = trimmed.strip_suffix('+').unwrap_or(trimmed);
        let mut p: Vec<&str> = prefix.split('+').map(str::trim).collect();
        if let Some(last) = p.last_mut()
            && last.is_empty()
        {
            *last = "+";
        }
        p
    } else if trimmed == "+" {
        vec!["+"]
    } else {
        trimmed.split('+').map(str::trim).collect()
    };

    if parts.is_empty() {
        return String::new();
    }

    let mut modifiers = Vec::new();
    let mut key_part: Option<&str> = None;

    for (idx, part) in parts.iter().enumerate() {
        let is_last = idx + 1 == parts.len();
        if !is_last {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifiers.push(ShortcutModifier::Ctrl),
                "shift" => modifiers.push(ShortcutModifier::Shift),
                "alt" | "option" => modifiers.push(ShortcutModifier::Alt),
                "super" | "meta" | "logo" => modifiers.push(ShortcutModifier::Super),
                _ => {
                    // Unknown prefix; keep literal fallback
                    return trimmed.to_string();
                }
            }
        } else {
            key_part = Some(part);
        }
    }

    let Some(key) = key_part else {
        return String::new();
    };
    format_shortcut_modifiers(&modifiers, key)
}

fn format_key_token(key: &str) -> String {
    let trimmed = key.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        " " | "space" => "Space".to_string(),
        "delete" | "del" | "backspace" => "⌫".to_string(),
        "enter" | "return" => "↩".to_string(),
        "escape" | "esc" => "esc".to_string(),
        "tab" => "⇥".to_string(),
        "arrowup" | "up" => "↑".to_string(),
        "arrowdown" | "down" => "↓".to_string(),
        "arrowleft" | "left" => "←".to_string(),
        "arrowright" | "right" => "→".to_string(),
        "equal" | "=" => "=".to_string(),
        "minus" | "-" => "-".to_string(),
        "plus" | "+" => "+".to_string(),
        "comma" | "," => ",".to_string(),
        "period" | "." => ".".to_string(),
        _ if trimmed.chars().count() == 1 => trimmed.to_ascii_uppercase(),
        _ => trimmed.to_string(),
    }
}

/// Tree-shaped menu item representation for converting app menu trees into [`MenuLayout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuTreeItem {
    Action {
        id: String,
        label: String,
        shortcut: String,
        enabled: bool,
    },
    CheckBox {
        id: String,
        label: String,
        shortcut: String,
        enabled: bool,
        checked: bool,
    },
    Divider,
    Submenu {
        label: String,
        items: Vec<MenuTreeItem>,
    },
}

/// Top-level section in a [`MenuTreeItem`] tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuTreeSection {
    pub id: String,
    pub label: String,
    pub items: Vec<MenuTreeItem>,
}

/// Convert a tree of [`MenuTreeSection`] values into a flat two-level [`MenuLayout`],
/// formatting shortcuts and flattening non-empty submenus cleanly.
#[must_use]
pub fn convert_menu_tree(sections: Vec<MenuTreeSection>) -> MenuLayout {
    sections
        .into_iter()
        .map(|sec| {
            let mut flat = Vec::new();
            flatten_tree_items(&sec.items, None, &mut flat);
            MenuSpec::new(sec.id, sec.label, flat)
        })
        .collect()
}

fn flatten_tree_items(
    items: &[MenuTreeItem],
    prefix: Option<&str>,
    out: &mut Vec<MenuItemSpec>,
) {
    for item in items {
        match item {
            MenuTreeItem::Action {
                id,
                label,
                shortcut,
                enabled,
            } => {
                let full_label = match prefix {
                    Some(p) => format!("{p}: {label}"),
                    None => label.clone(),
                };
                out.push(
                    MenuItemSpec::action(id.clone(), full_label)
                        .shortcut(shortcut)
                        .enabled(*enabled),
                );
            }
            MenuTreeItem::CheckBox {
                id,
                label,
                shortcut,
                enabled,
                checked,
            } => {
                let full_label = match prefix {
                    Some(p) => format!("{p}: {label}"),
                    None => label.clone(),
                };
                out.push(
                    MenuItemSpec::checkbox(id.clone(), full_label, *checked)
                        .shortcut(shortcut)
                        .enabled(*enabled),
                );
            }
            MenuTreeItem::Divider => {
                out.push(MenuItemSpec::separator());
            }
            MenuTreeItem::Submenu { label, items } => {
                if items.is_empty() {
                    continue;
                }
                let next_prefix = match prefix {
                    Some(p) => format!("{p} / {label}"),
                    None => label.clone(),
                };
                flatten_tree_items(items, Some(&next_prefix), out);
            }
        }
    }
}

fn clean_separators(items: Vec<MenuItemSpec>) -> Vec<MenuItemSpec> {
    let mut out: Vec<MenuItemSpec> = Vec::with_capacity(items.len());
    for item in items {
        if item.is_separator() && out.last().is_none_or(MenuItemSpec::is_separator) {
            continue;
        }
        out.push(item);
    }
    while out.last().is_some_and(MenuItemSpec::is_separator) {
        out.pop();
    }
    out
}

/// Sanitize an application ID or `.desktop` ID into a valid D-Bus bus-name suffix.
///
/// Strips a trailing `.desktop` suffix, replaces invalid characters with `_`,
/// ensures no segment starts with an ASCII digit, and falls back to `"unknown"`
/// if empty.
#[must_use]
pub fn sanitize_app_id(app_id: &str) -> String {
    let trimmed = app_id.trim();
    let stripped = trimmed
        .strip_suffix(".desktop")
        .or_else(|| trimmed.strip_suffix(".DESKTOP"))
        .unwrap_or(trimmed);

    let mut segments = Vec::new();
    for raw_seg in stripped.split('.') {
        if raw_seg.is_empty() {
            continue;
        }
        let mut seg = String::with_capacity(raw_seg.len() + 1);
        for ch in raw_seg.chars() {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                seg.push(ch);
            } else {
                seg.push('_');
            }
        }
        if seg.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            seg.insert(0, '_');
        }
        if !seg.is_empty() {
            segments.push(seg);
        }
    }

    if segments.is_empty() {
        "unknown".to_string()
    } else {
        segments.join(".")
    }
}

/// Map an application ID (or `.desktop` ID) to its `dev.asw.AppMenu.<sanitized>`
/// session D-Bus well-known name.
#[must_use]
pub fn bus_name_for_app(app_id: &str) -> String {
    let mut name = format!("{BUS_NAME_PREFIX}{}", sanitize_app_id(app_id));
    if name.len() > 255 {
        name.truncate(255);
        while name.ends_with('.') {
            name.pop();
        }
    }
    if WellKnownName::try_from(name.as_str()).is_err() {
        return format!("{BUS_NAME_PREFIX}unknown");
    }
    name
}

/// Return deduplicated candidate D-Bus well-known names to query for an active window.
///
/// Checks the raw Wayland `app_id` first, then the resolved `.desktop` ID if present.
#[must_use]
pub fn candidate_bus_names(app_id: &str, desktop_id: Option<&str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut names = Vec::new();

    if !app_id.trim().is_empty() {
        let name = bus_name_for_app(app_id);
        if seen.insert(name.clone()) {
            names.push(name);
        }
    }

    if let Some(did) = desktop_id
        && !did.trim().is_empty()
    {
        let name = bus_name_for_app(did);
        if seen.insert(name.clone()) {
            names.push(name);
        }
    }

    names
}

/// D-Bus client proxy for `dev.asw.AppMenu1`.
#[proxy(interface = "dev.asw.AppMenu1", default_path = "/dev/asw/AppMenu")]
pub trait AppMenu1 {
    #[zbus(name = "GetLayout")]
    fn get_layout(&self) -> zbus::Result<Vec<MenuSpec>>;

    #[zbus(name = "Activate")]
    fn activate(&self, item_id: &str) -> zbus::Result<()>;

    #[zbus(signal, name = "LayoutChanged")]
    fn layout_changed(&self) -> zbus::Result<()>;
}

struct AppMenuInner {
    id: u64,
    app_id: String,
    layout: RwLock<MenuLayout>,
    changed_tx: watch::Sender<u64>,
}

/// Thread-safe handle held by an application to publish and update its global menu layout.
#[derive(Clone)]
pub struct AppMenuHandle {
    inner: Arc<AppMenuInner>,
}

impl std::fmt::Debug for AppMenuHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppMenuHandle")
            .field("id", &self.inner.id)
            .field("app_id", &self.inner.app_id)
            .finish()
    }
}

impl Hash for AppMenuHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.inner.id.hash(state);
    }
}

impl PartialEq for AppMenuHandle {
    fn eq(&self, other: &Self) -> bool {
        self.inner.id == other.inner.id
    }
}

impl Eq for AppMenuHandle {}

impl AppMenuHandle {
    /// Create a new [`AppMenuHandle`] for `app_id` with an initial [`MenuLayout`].
    #[must_use]
    pub fn new(app_id: impl Into<String>, initial_layout: MenuLayout) -> Self {
        let (changed_tx, _) = watch::channel(0u64);
        Self {
            inner: Arc::new(AppMenuInner {
                id: NEXT_HANDLE_ID.fetch_add(1, Ordering::Relaxed),
                app_id: app_id.into(),
                layout: RwLock::new(initial_layout),
                changed_tx,
            }),
        }
    }

    /// Return the application ID associated with this handle.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.inner.app_id
    }

    /// Return the session D-Bus well-known name for this handle.
    #[must_use]
    pub fn bus_name(&self) -> String {
        bus_name_for_app(&self.inner.app_id)
    }

    /// Return a snapshot of the current menu layout.
    #[must_use]
    pub fn layout(&self) -> MenuLayout {
        self.inner
            .layout
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Replace the published menu layout.
    ///
    /// Emits `LayoutChanged` over D-Bus only if the new layout differs from the current one.
    pub fn set_layout(&self, new_layout: MenuLayout) {
        let changed = match self.inner.layout.write() {
            Ok(mut guard) => {
                if *guard == new_layout {
                    false
                } else {
                    *guard = new_layout;
                    true
                }
            }
            Err(_) => false,
        };
        if changed {
            self.inner.changed_tx.send_modify(|rev| {
                *rev = rev.wrapping_add(1);
            });
        }
    }

    fn subscribe_changes(&self) -> watch::Receiver<u64> {
        self.inner.changed_tx.subscribe()
    }
}

struct AppMenuService {
    inner: Arc<AppMenuInner>,
    activate_tx: mpsc::UnboundedSender<String>,
}

#[interface(name = "dev.asw.AppMenu1")]
impl AppMenuService {
    #[zbus(name = "GetLayout")]
    async fn get_layout(&self) -> Vec<MenuSpec> {
        self.inner
            .layout
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    #[zbus(name = "Activate")]
    async fn activate(&self, item_id: &str) {
        if !item_id.is_empty() && item_id != SEPARATOR_ID {
            let _ = self.activate_tx.send(item_id.to_string());
        }
    }

    #[zbus(signal, name = "LayoutChanged")]
    async fn layout_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Register the `dev.asw.AppMenu1` object on an existing D-Bus [`Connection`] and
/// forward `LayoutChanged` notifications until `handle` is dropped or the connection closes.
pub async fn serve_on_connection(
    conn: &Connection,
    handle: AppMenuHandle,
    activate_tx: mpsc::UnboundedSender<String>,
) -> zbus::Result<()> {
    let bus_name = handle.bus_name();
    let service = AppMenuService {
        inner: Arc::clone(&handle.inner),
        activate_tx,
    };

    conn.object_server().at(OBJECT_PATH, service).await?;
    conn.request_name_with_flags(
        bus_name.as_str(),
        RequestNameFlags::AllowReplacement | RequestNameFlags::ReplaceExisting,
    )
    .await?;

    let iface_ref = conn
        .object_server()
        .interface::<_, AppMenuService>(OBJECT_PATH)
        .await?;
    let mut changes = handle.subscribe_changes();
    let _ = *changes.borrow_and_update();

    while changes.changed().await.is_ok() {
        let emitter = iface_ref.signal_emitter();
        if let Err(err) = AppMenuService::layout_changed(emitter).await {
            tracing::debug!("failed to emit AppMenu1 LayoutChanged: {err}");
        }
    }

    Ok(())
}

/// Connect to the session D-Bus and serve `dev.asw.AppMenu1` for `handle`.
pub async fn serve(
    handle: AppMenuHandle,
    activate_tx: mpsc::UnboundedSender<String>,
) -> zbus::Result<()> {
    let conn = Connection::session().await?;
    serve_on_connection(&conn, handle, activate_tx).await
}

/// Stream of activated `item_id` values suitable for `iced::Subscription::run_with`.
///
/// Spawns the session D-Bus server on first poll inside the active Tokio runtime
/// and aborts the background server task when dropped.
pub struct ActivateStream {
    handle: Option<AppMenuHandle>,
    tx: Option<mpsc::UnboundedSender<String>>,
    rx: mpsc::UnboundedReceiver<String>,
    server_task: Option<JoinHandle<()>>,
}

impl Stream for ActivateStream {
    type Item = String;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let (Some(handle), Some(tx)) = (self.handle.take(), self.tx.take()) {
            let task = tokio::spawn(async move {
                if let Err(err) = serve(handle, tx).await {
                    tracing::warn!("mac-appmenu D-Bus service unavailable: {err}");
                }
            });
            self.server_task = Some(task);
        }
        self.rx.poll_recv(cx)
    }
}

impl Drop for ActivateStream {
    fn drop(&mut self) {
        if let Some(task) = self.server_task.take() {
            task.abort();
        }
    }
}

/// Create an [`ActivateStream`] for `Subscription::run_with(handle.clone(), mac_appmenu::activate_stream)`.
#[must_use]
pub fn activate_stream(handle: &AppMenuHandle) -> ActivateStream {
    let (tx, rx) = mpsc::unbounded_channel();
    ActivateStream {
        handle: Some(handle.clone()),
        tx: Some(tx),
        rx,
        server_task: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::to_bytes;

    #[test]
    fn bus_name_mapping_handles_app_ids_and_desktop_ids() {
        assert_eq!(sanitize_app_id("dev.asw.MacMail"), "dev.asw.MacMail");
        assert_eq!(
            sanitize_app_id("dev.asw.MacFiles.desktop"),
            "dev.asw.MacFiles"
        );
        assert_eq!(sanitize_app_id("org.0ad.0ad"), "org._0ad._0ad");
        assert_eq!(sanitize_app_id("my app!"), "my_app_");
        assert_eq!(sanitize_app_id(""), "unknown");

        assert_eq!(
            bus_name_for_app("dev.asw.MacNotepad"),
            "dev.asw.AppMenu.dev.asw.MacNotepad"
        );
        assert_eq!(
            bus_name_for_app("firefox.desktop"),
            "dev.asw.AppMenu.firefox"
        );

        let candidates = candidate_bus_names("cosmic-files", Some("com.system76.CosmicFiles.desktop"));
        assert_eq!(
            candidates,
            vec![
                "dev.asw.AppMenu.cosmic-files".to_string(),
                "dev.asw.AppMenu.com.system76.CosmicFiles".to_string(),
            ]
        );

        let deduped = candidate_bus_names("dev.asw.MacMail", Some("dev.asw.MacMail.desktop"));
        assert_eq!(deduped, vec!["dev.asw.AppMenu.dev.asw.MacMail".to_string()]);
    }

    #[test]
    fn shortcut_formatting_converts_ctrl_and_modifiers() {
        assert_eq!(format_shortcut("Ctrl+S"), "⌘S");
        assert_eq!(format_shortcut("Ctrl + Shift + S"), "⇧⌘S");
        assert_eq!(format_shortcut("Ctrl+Alt+Z"), "⌥⌘Z");
        assert_eq!(format_shortcut("Super+Ctrl+Q"), "⌃⌘Q");
        assert_eq!(format_shortcut("Ctrl + Space"), "⌘Space");
        assert_eq!(format_shortcut("Delete"), "⌫");
        assert_eq!(format_shortcut("⌘V"), "⌘V");
        assert_eq!(format_shortcut(""), "");

        assert_eq!(
            format_shortcut_modifiers(
                &[ShortcutModifier::Ctrl, ShortcutModifier::Shift],
                "o"
            ),
            "⇧⌘O"
        );
    }

    #[test]
    fn menu_tree_conversion_and_separator_cleanup() {
        let layout = convert_menu_tree(vec![
            MenuTreeSection {
                id: "file".into(),
                label: "File".into(),
                items: vec![
                    MenuTreeItem::Divider,
                    MenuTreeItem::Action {
                        id: "new".into(),
                        label: "New".into(),
                        shortcut: "Ctrl+N".into(),
                        enabled: true,
                    },
                    MenuTreeItem::Divider,
                    MenuTreeItem::Divider,
                    MenuTreeItem::Submenu {
                        label: "Recent".into(),
                        items: vec![],
                    },
                    MenuTreeItem::Submenu {
                        label: "Export".into(),
                        items: vec![MenuTreeItem::Action {
                            id: "export.pdf".into(),
                            label: "PDF".into(),
                            shortcut: String::new(),
                            enabled: true,
                        }],
                    },
                    MenuTreeItem::Divider,
                ],
            },
            MenuTreeSection {
                id: "view".into(),
                label: "View".into(),
                items: vec![MenuTreeItem::CheckBox {
                    id: "wrap".into(),
                    label: "Word Wrap".into(),
                    shortcut: "Alt+Z".into(),
                    enabled: true,
                    checked: true,
                }],
            },
        ]);

        assert_eq!(layout.len(), 2);
        assert_eq!(layout[0].id, "file");
        assert_eq!(layout[0].items.len(), 3);
        assert_eq!(layout[0].items[0].id, "new");
        assert_eq!(layout[0].items[0].shortcut, "⌘N");
        assert!(layout[0].items[1].is_separator());
        assert_eq!(layout[0].items[2].id, "export.pdf");
        assert_eq!(layout[0].items[2].label, "Export: PDF");

        assert!(layout[1].items[0].checked);
        assert_eq!(layout[1].items[0].shortcut, "⌥Z");
    }

    #[test]
    fn dbus_layout_signature_and_roundtrip_serialization() {
        assert_eq!(<Vec<MenuSpec> as Type>::SIGNATURE.to_string(), "a(ssa(sssbb))");

        let layout = vec![MenuSpec::new(
            "file",
            "File",
            vec![
                MenuItemSpec::action("save", "Save").shortcut("Ctrl+S"),
                MenuItemSpec::separator(),
                MenuItemSpec::checkbox("auto_save", "Auto Save", true).enabled(false),
            ],
        )];

        let ctxt = zbus::zvariant::serialized::Context::new_dbus(
            zbus::zvariant::Endian::Little,
            0,
        );
        let encoded = to_bytes(ctxt, &layout).expect("should serialize MenuLayout");
        let (decoded, _): (Vec<MenuSpec>, usize) = encoded
            .deserialize()
            .expect("should deserialize MenuLayout");
        assert_eq!(decoded, layout);
    }

    #[tokio::test]
    async fn dbus_service_handles_get_layout_and_activate_in_memory() {
        let initial = vec![MenuSpec::new(
            "file",
            "File",
            vec![MenuItemSpec::action("new", "New").shortcut("Ctrl+N")],
        )];
        let handle = AppMenuHandle::new("dev.asw.TestApp", initial.clone());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let service = AppMenuService {
            inner: Arc::clone(&handle.inner),
            activate_tx: tx,
        };

        assert_eq!(service.get_layout().await, initial);
        service.activate("-").await;
        assert!(rx.try_recv().is_err(), "separator must not activate");

        service.activate("new").await;
        assert_eq!(rx.recv().await.as_deref(), Some("new"));

        let updated = vec![MenuSpec::new(
            "edit",
            "Edit",
            vec![MenuItemSpec::action("undo", "Undo").enabled(false)],
        )];
        handle.set_layout(updated.clone());
        assert_eq!(service.get_layout().await, updated);
    }
}
