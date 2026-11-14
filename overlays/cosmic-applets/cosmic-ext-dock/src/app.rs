// SPDX-License-Identifier: GPL-3.0-only

//! A standalone dock drawn on its own layer-shell surface.
//!
//! Pinned apps come from the stock COSMIC dock's config, so pins are shared.
//! Icons grow as the pointer approaches and a launching icon bounces. The
//! surface is only as large as the idle dock until the pointer enters, so the
//! headroom the magnified icons need never blocks clicks on windows.
//!
//! The dock reserves no screen space. When a window covers its resting area
//! it slides off the bottom edge and leaves a thin strip that brings it back
//! when the pointer touches it.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use cctk::{
    sctk::reexports::{
        calloop,
        protocols::xdg::shell::client::xdg_positioner::{Anchor as PopupAnchor, Gravity},
    },
    toplevel_info::ToplevelInfo,
    wayland_client::protocol::wl_output::WlOutput,
    wayland_protocols::ext::{
        foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        workspace::v1::client::ext_workspace_handle_v1::ExtWorkspaceHandleV1,
    },
};
use std::sync::LazyLock;

use cosmic::{
    Element, Task, app,
    applet::menu_button,
    cosmic_config::{Config, CosmicConfigEntry},
    desktop::{
        IconSourceExt,
        fde::{self, DesktopEntry, get_languages_from_env},
    },
    iced::{
        Alignment, Background, Border, Color, Length, Limits, Padding, Point, Rectangle, Size,
        Subscription,
        platform_specific::shell::wayland::commands::{
            blur::blur,
            layer_surface::{Anchor, KeyboardInteractivity, Layer, get_layer_surface, set_size},
            popup::{destroy_popup, get_popup},
        },
        runtime::platform_specific::wayland::{
            layer_surface::{IcedOutput, SctkLayerSurfaceSettings},
            popup::{SctkPopupSettings, SctkPositioner},
        },
        widget::{column, container, mouse_area, pin, row, stack},
        window,
    },
    theme,
    widget::{Id, Image, autosize, button, icon, image::Handle, space, text},
};
use cosmic::iced::advanced::text::{Ellipsize, EllipsizeHeightLimit};
use cosmic_app_list_config::AppListConfig;
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1::State;

use crate::wayland_subscription::{
    OutputUpdate, ToplevelRequest, ToplevelUpdate, WaylandRequest, WaylandUpdate,
    wayland_subscription,
};

/// Icon edge length when the pointer is away, in logical pixels.
const ICON_SIZE: f32 = 48.0;
/// Largest scale an icon reaches directly under the pointer.
const MAX_SCALE: f32 = 1.6;
/// Horizontal distance over which magnification falls off to nothing.
const INFLUENCE: f32 = 150.0;
const ICON_SPACING: f32 = 6.0;
/// Thin line between the apps and the trash, like the macOS dock.
const SEPARATOR_WIDTH: f32 = 1.0;
/// Room the separator takes in the icon row.
const SEPARATOR_SLOT: f32 = SEPARATOR_WIDTH + ICON_SPACING;
const BAR_PADDING: f32 = 6.0;
const BAR_RADIUS: f32 = 20.0;
const BAR_OPACITY: f32 = 0.55;
const DOT_SIZE: f32 = 4.0;
/// Gap between the icon and its running indicator.
const DOT_GAP: f32 = 2.0;
/// Gap between the bar and the screen edge. Drawn inside the surface rather
/// than as a layer margin, so the hidden reveal strip sits on the very edge.
const MARGIN_BOTTOM: f32 = 6.0;
const BOUNCE_HEIGHT: f32 = 18.0;
const BOUNCE_DURATION: Duration = Duration::from_millis(1400);
const FRAME: Duration = Duration::from_millis(16);
/// Fraction of the remaining distance the hover animation covers per frame.
const HOVER_EASING: f32 = 0.3;
/// Fraction of the remaining distance the hide/show slide covers per frame.
const SLIDE_EASING: f32 = 0.25;

/// Height of the bar itself, which never changes.
const BAR_HEIGHT: f32 = ICON_SIZE + DOT_GAP + DOT_SIZE + 2.0 * BAR_PADDING;
/// Surface height while idle.
const IDLE_HEIGHT: f32 = BAR_HEIGHT + MARGIN_BOTTOM;
/// Surface height while the pointer is inside: room for the largest icon.
const EXPANDED_HEIGHT: f32 = IDLE_HEIGHT + ICON_SIZE * (MAX_SCALE - 1.0);
/// Surface height while hidden behind a window: a strip at the screen edge
/// that reveals the dock when the pointer touches it.
const HIDDEN_HEIGHT: f32 = 2.0;

/// How long the pointer rests on an icon before its window previews appear.
const PREVIEW_DELAY: Duration = Duration::from_millis(450);
/// How often the hover timer is checked.
const HOVER_TICK: Duration = Duration::from_millis(150);
/// How long the pointer may be away from both the dock and the preview
/// before the preview closes.
const PREVIEW_GRACE: Duration = Duration::from_millis(300);
const THUMB_WIDTH: f32 = 192.0;
const THUMB_HEIGHT: f32 = 156.0;

static AUTOSIZE_PREVIEW_ID: LazyLock<Id> = LazyLock::new(|| Id::new("dock-preview-autosize"));

const LAUNCHER_APP_ID: &str = "com.system76.CosmicAppLibrary";
/// Never matches a real window, so no window is ever grouped under the trash.
const TRASH_APP_ID: &str = "dev.asw.CosmicExtDock.Trash";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A pinned or running application.
    App,
    /// Opens the app library; always first.
    Launcher,
    /// Opens the trash in the file manager; always last.
    Trash,
}

struct Item {
    kind: Kind,
    /// The id this item is pinned under, or its window's app id.
    app_id: String,
    entry: DesktopEntry,
    pinned: bool,
    toplevels: Vec<ToplevelInfo>,
}

impl Item {
    fn is_focused(&self) -> bool {
        self.toplevels
            .iter()
            .any(|t| t.state.contains(&State::Activated))
    }
}

/// How far the pointer must travel before a press becomes a drag.
const DRAG_THRESHOLD: f32 = 8.0;

/// Whether the user's trash has any files in it.
fn trash_is_full() -> bool {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share")));
    base.and_then(|dir| std::fs::read_dir(dir.join("Trash/files")).ok())
        .is_some_and(|mut entries| entries.next().is_some())
}

/// The window preview popup above an icon.
struct Preview {
    id: window::Id,
    index: usize,
}

/// A pressed dock icon: a click if released in place, a reorder if moved.
struct Drag {
    /// Current position of the pressed item in `Dock::items`.
    index: usize,
    start_x: f32,
    moved: bool,
}

/// The right-click menu of one dock item.
struct Menu {
    id: window::Id,
    index: usize,
}

pub struct Dock {
    core: app::Core,
    surface: window::Id,
    config: AppListConfig,
    locales: Vec<String>,
    desktop_entries: Vec<DesktopEntry>,
    /// Launcher, pinned apps in config order, running unpinned apps, trash.
    items: Vec<Item>,
    wayland_tx: Option<calloop::channel::Sender<WaylandRequest>>,
    /// Pointer x within the surface while it is inside.
    pointer_x: Option<f32>,
    /// Last known pointer x, kept so icons shrink in place after it leaves.
    last_pointer_x: f32,
    /// 0.0 = idle, 1.0 = fully magnified; eased toward the target each frame.
    hover: f32,
    /// 0.0 = slid off screen, 1.0 = fully shown; eased toward the target.
    reveal: f32,
    /// Whether the surface currently has its expanded size.
    expanded: bool,
    /// Desktop id of the icon that is bouncing and when it started.
    bounce: Option<(String, Instant)>,
    menu: Option<Menu>,
    drag: Option<Drag>,
    /// Unlocked for rearranging: icons can be dragged and unpinned.
    /// Locked otherwise, so long presses never move icons by accident.
    editing: bool,
    now: Instant,
    /// Logical size of each output, used to tell whether a window covers the dock.
    outputs: HashMap<WlOutput, (i32, i32)>,
    active_workspaces: Vec<ExtWorkspaceHandleV1>,
    /// Size last requested for the surface, to avoid redundant resizes.
    surface_size: (u32, u32),
    /// Bar position last sent as the blur region (x, y, width), if any.
    blur_region: Option<(i32, i32, i32)>,
    /// Icon positions last written for the compositor's minimize animation.
    published_icons: String,
    /// Whether the trash holds anything, for its full or empty icon.
    trash_full: bool,
    /// Latest picture of each window, for the previews.
    thumbs: HashMap<ExtForeignToplevelHandleV1, crate::wayland_subscription::WaylandImage>,
    preview: Option<Preview>,
    /// The icon under the pointer and since when.
    hover_index: Option<usize>,
    hover_since: Option<Instant>,
    /// Whether the pointer is over the preview popup.
    over_preview: bool,
    /// When the pointer left the dock.
    left_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Wayland(WaylandUpdate),
    Config(AppListConfig),
    PointerMoved(Point),
    PointerLeft,
    Frame(Instant),
    Activate(usize),
    /// An icon was pressed; released in place it activates, moved it reorders.
    Press(usize),
    Released,
    OpenMenu(usize),
    MenuClosed(window::Id),
    MenuLaunch,
    MenuTogglePin,
    MenuQuit,
    /// Checks the hover timer and the preview's grace period.
    HoverTick(Instant),
    PreviewEntered,
    PreviewLeft,
    /// A preview was clicked: bring that window forward.
    PreviewActivate(ExtForeignToplevelHandleV1),
    /// Lock or unlock the dock for rearranging.
    MenuToggleEdit,
    /// Unpin the app at this index (edit mode's remove badge).
    Unpin(usize),
}

impl Dock {
    fn reload_desktop_entries(&mut self) {
        self.desktop_entries = fde::Iter::new(fde::default_paths())
            .filter_map(|path| DesktopEntry::from_path(path, Some(&self.locales)).ok())
            .collect();
    }

    fn entry_for_app_id(&mut self, app_id: &str) -> DesktopEntry {
        let lookup = |entries: &[DesktopEntry]| {
            fde::find_app_by_id(entries, fde::unicase::Ascii::new(app_id)).cloned()
        };
        if let Some(entry) = lookup(&self.desktop_entries) {
            return entry;
        }
        // The app may have been installed after the dock started.
        self.reload_desktop_entries();
        lookup(&self.desktop_entries).unwrap_or_else(|| DesktopEntry::from_appid(app_id.to_owned()))
    }

    /// Rebuild the item list from the pinned config, keeping open windows.
    fn rebuild_items(&mut self) {
        let toplevels: Vec<ToplevelInfo> = self
            .items
            .drain(..)
            .flat_map(|item| item.toplevels)
            .collect();

        let launcher = self.entry_for_app_id(LAUNCHER_APP_ID);
        self.items.push(Item {
            kind: Kind::Launcher,
            app_id: LAUNCHER_APP_ID.to_owned(),
            entry: launcher,
            pinned: true,
            toplevels: Vec::new(),
        });
        for favorite in self.config.favorites.clone() {
            let entry = self.entry_for_app_id(&favorite);
            self.items.push(Item {
                kind: Kind::App,
                app_id: favorite,
                entry,
                pinned: true,
                toplevels: Vec::new(),
            });
        }
        self.items.push(Item {
            kind: Kind::Trash,
            app_id: TRASH_APP_ID.to_owned(),
            entry: DesktopEntry::from_appid(TRASH_APP_ID.to_owned()),
            pinned: true,
            toplevels: Vec::new(),
        });

        for info in toplevels {
            self.add_toplevel(info);
        }
    }

    fn add_toplevel(&mut self, info: ToplevelInfo) {
        let entry = self.entry_for_app_id(&info.app_id);
        if let Some(item) = self
            .items
            .iter_mut()
            .find(|i| i.kind == Kind::App && i.entry.id() == entry.id())
        {
            item.toplevels.push(info);
            return;
        }
        // Running apps that are not pinned go after the pins, before the trash.
        let position = self
            .items
            .iter()
            .position(|i| i.kind == Kind::Trash)
            .unwrap_or(self.items.len());
        self.items.insert(
            position,
            Item {
                kind: Kind::App,
                app_id: info.app_id.clone(),
                entry,
                pinned: false,
                toplevels: vec![info],
            },
        );
    }

    fn update_toplevel(&mut self, info: ToplevelInfo) {
        let existing = self
            .items
            .iter_mut()
            .flat_map(|item| item.toplevels.iter_mut())
            .find(|t| t.foreign_toplevel == info.foreign_toplevel);
        match existing {
            Some(slot) => *slot = info,
            None => self.add_toplevel(info),
        }
    }

    fn remove_toplevel(&mut self, handle: &ExtForeignToplevelHandleV1) {
        for item in &mut self.items {
            item.toplevels.retain(|t| &t.foreign_toplevel != handle);
        }
        self.items
            .retain(|item| item.pinned || !item.toplevels.is_empty());
    }

    /// Width of the icon row when nothing is magnified.
    fn idle_row_width(&self) -> f32 {
        let count = self.items.len().max(1) as f32;
        count * ICON_SIZE + (count - 1.0) * ICON_SPACING + self.separator_extra()
    }

    /// Width the separator adds when the trash is shown after other icons.
    fn separator_extra(&self) -> f32 {
        if self.items.len() > 1 && self.items.last().is_some_and(|i| i.kind == Kind::Trash) {
            SEPARATOR_SLOT
        } else {
            0.0
        }
    }

    fn idle_width(&self) -> f32 {
        self.idle_row_width() + 2.0 * BAR_PADDING
    }

    /// Surface width while expanded: the idle width plus the most the row can
    /// grow, which is when the pointer sits in the middle of the influence range.
    fn expanded_width(&self) -> f32 {
        let icons_in_range = (2.0 * INFLUENCE / (ICON_SIZE + ICON_SPACING)).ceil();
        self.idle_width() + icons_in_range * ICON_SIZE * (MAX_SCALE - 1.0)
    }

    /// Horizontal center of the icon at `index` in the unmagnified layout.
    fn icon_center(&self, index: usize) -> f32 {
        let row_start = (self.surface_size.0 as f32 - self.idle_row_width()) / 2.0;
        let shift = if self.items.get(index).is_some_and(|i| i.kind == Kind::Trash) {
            self.separator_extra()
        } else {
            0.0
        };
        row_start + index as f32 * (ICON_SIZE + ICON_SPACING) + ICON_SIZE / 2.0 + shift
    }

    /// Scale of the icon at `index` for the current pointer position.
    fn icon_scale(&self, index: usize) -> f32 {
        if self.hover <= 0.0 {
            return 1.0;
        }
        let distance = (self.last_pointer_x - self.icon_center(index)).abs();
        if distance >= INFLUENCE {
            return 1.0;
        }
        let falloff = 0.5 * (1.0 + (std::f32::consts::PI * distance / INFLUENCE).cos());
        1.0 + (MAX_SCALE - 1.0) * falloff * self.hover
    }

    /// Width of the icon row at the current magnification.
    fn row_width(&self) -> f32 {
        (0..self.items.len())
            .map(|index| ICON_SIZE * self.icon_scale(index))
            .sum::<f32>()
            + self.items.len().saturating_sub(1) as f32 * ICON_SPACING
            + self.separator_extra()
    }

    /// Vertical offset of a bouncing icon: a decaying series of hops.
    fn bounce_offset(&self, item: &Item) -> f32 {
        let Some((id, started)) = self.bounce.as_ref() else {
            return 0.0;
        };
        if id != item.entry.id() {
            return 0.0;
        }
        let progress =
            self.now.duration_since(*started).as_secs_f32() / BOUNCE_DURATION.as_secs_f32();
        if progress >= 1.0 {
            return 0.0;
        }
        let hops = 3.0;
        (progress * hops * std::f32::consts::PI).sin().abs() * BOUNCE_HEIGHT * (1.0 - progress)
    }

    /// Whether the dock is being interacted with or animating an icon.
    fn is_active(&self) -> bool {
        self.pointer_x.is_some() || self.hover > 0.0 || self.bounce.is_some() || self.menu.is_some()
    }

    /// Whether a window on the active workspace covers the dock's resting area.
    fn is_covered(&self) -> bool {
        let dock_height = IDLE_HEIGHT as i32;
        let dock_width = self.idle_width() as i32;
        self.items
            .iter()
            .flat_map(|item| item.toplevels.iter())
            .filter(|t| !t.state.contains(&State::Minimized))
            .filter(|t| {
                self.active_workspaces.is_empty()
                    || t.workspace.is_empty()
                    || self
                        .active_workspaces
                        .iter()
                        .any(|workspace| t.workspace.contains(workspace))
            })
            .any(|t| {
                t.geometry.iter().any(|(output, geometry)| {
                    let Some((output_width, output_height)) = self.outputs.get(output) else {
                        return false;
                    };
                    let dock_left = (output_width - dock_width) / 2;
                    let reaches_dock = geometry.y + geometry.height > output_height - dock_height;
                    let overlaps_span = geometry.x < dock_left + dock_width
                        && geometry.x + geometry.width > dock_left;
                    reaches_dock && overlaps_span
                })
            })
    }

    /// The dock hides behind windows that cover it unless it is in use.
    fn wants_visible(&self) -> bool {
        self.is_active() || !self.is_covered()
    }

    fn hover_target(&self) -> f32 {
        if self.pointer_x.is_some() { 1.0 } else { 0.0 }
    }

    fn reveal_target(&self) -> f32 {
        if self.wants_visible() { 1.0 } else { 0.0 }
    }

    /// Whether only the reveal strip is on screen.
    fn is_hidden(&self) -> bool {
        self.reveal <= 0.0 && !self.wants_visible()
    }

    /// How far the dock is currently slid toward the bottom edge.
    fn slide_offset(&self) -> f32 {
        (1.0 - self.reveal) * IDLE_HEIGHT
    }

    fn is_animating(&self) -> bool {
        (self.hover - self.hover_target()).abs() > 0.001
            || (self.reveal - self.reveal_target()).abs() > 0.001
            || self.bounce.is_some()
    }

    /// Resize the surface for its current state: a thin reveal strip when
    /// hidden, the bar when idle, or the bar plus headroom when in use.
    fn sync_surface_size(&mut self) -> app::Task<Message> {
        self.expanded = self.is_active();
        let (width, height) = if self.expanded {
            (self.expanded_width(), EXPANDED_HEIGHT)
        } else if self.is_hidden() {
            (self.idle_width(), HIDDEN_HEIGHT)
        } else {
            (self.idle_width(), IDLE_HEIGHT)
        };
        let size = (width.ceil() as u32, height.ceil() as u32);
        if size == self.surface_size {
            return Task::none();
        }
        self.surface_size = size;
        set_size(self.surface, Some(size.0), Some(size.1))
    }

    /// Ask the compositor to blur what is behind the bar, and only the bar.
    fn sync_blur(&mut self) -> app::Task<Message> {
        let frosted = theme::active().cosmic().frosted_system_interface;
        let region = (frosted && !self.is_hidden()).then(|| {
            let width = self.row_width() + 2.0 * BAR_PADDING;
            let x = (self.surface_size.0 as f32 - width) / 2.0;
            let y = self.surface_size.1 as f32 - MARGIN_BOTTOM - BAR_HEIGHT + self.slide_offset();
            (x.round() as i32, y.round() as i32, width.round() as i32)
        });
        if region == self.blur_region {
            return Task::none();
        }
        self.blur_region = region;

        // The region is a list of rectangles, so the rounded bar is covered
        // by two overlapping ones that leave its corners out.
        let rectangles = region.map(|(x, y, width)| {
            let (x, y, width) = (x as f32, y as f32, width as f32);
            vec![
                Rectangle::new(
                    Point::new(x + BAR_RADIUS, y),
                    Size::new(width - 2.0 * BAR_RADIUS, BAR_HEIGHT),
                ),
                Rectangle::new(
                    Point::new(x, y + BAR_RADIUS),
                    Size::new(width, BAR_HEIGHT - 2.0 * BAR_RADIUS),
                ),
            ]
        });
        blur(self.surface, rectangles).discard()
    }

    /// Tell the compositor where each app's icon sits, so minimizing a window
    /// animates into its icon. The patched compositor reads this file; see
    /// `ext_dock_icon_rectangle` there for the format.
    fn publish_icons(&mut self) {
        let bottom = (MARGIN_BOTTOM + BAR_PADDING + DOT_SIZE + DOT_GAP).round() as i32;
        let half_row = self.idle_row_width() / 2.0;
        let mut lines = String::new();
        for (index, item) in self.items.iter().enumerate() {
            if item.kind != Kind::App {
                continue;
            }
            let center =
                index as f32 * (ICON_SIZE + ICON_SPACING) + ICON_SIZE / 2.0 - half_row;
            let ids = std::iter::once(item.app_id.as_str())
                .chain(std::iter::once(item.entry.id()))
                .chain(item.toplevels.iter().map(|t| t.app_id.as_str()));
            for id in ids.filter(|id| !id.is_empty() && !id.contains(['\t', '\n'])) {
                lines.push_str(&format!(
                    "{id}\t{}\t{}\t{bottom}\n",
                    center.round() as i32,
                    ICON_SIZE as i32
                ));
            }
        }
        if lines == self.published_icons {
            return;
        }
        let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
            return;
        };
        let path = std::path::Path::new(&dir).join("cosmic-ext-dock-icons");
        // Write then rename, so the compositor never reads a half-written file.
        let temp = path.with_extension("tmp");
        let result = std::fs::write(&temp, &lines).and_then(|()| std::fs::rename(&temp, &path));
        match result {
            Ok(()) => self.published_icons = lines,
            Err(err) => tracing::warn!(?err, "Failed to publish dock icon positions"),
        }
    }

    /// Bring the surface size and blur region in line with the current state.
    fn refresh(&mut self) -> app::Task<Message> {
        self.publish_icons();
        let resize = self.sync_surface_size();
        let blur = self.sync_blur();
        Task::batch([resize, blur])
    }

    fn send(&self, request: WaylandRequest) {
        if let Some(tx) = self.wayland_tx.as_ref() {
            let _ = tx.send(request);
        }
    }

    /// Start the app behind the item at `index`.
    fn launch(&mut self, index: usize, bounce: bool) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        let Some(exec) = item.entry.exec() else {
            return;
        };
        self.send(WaylandRequest::TokenRequest {
            app_id: <Self as cosmic::Application>::APP_ID.to_owned(),
            exec: exec.to_owned(),
            gpu_idx: None,
            terminal: item.entry.terminal(),
        });
        if bounce {
            let id = item.entry.id().to_owned();
            self.now = Instant::now();
            self.bounce = Some((id, self.now));
        }
    }

    /// While an icon is held, move it to the slot under the pointer. Only
    /// pinned apps can be reordered, and only among themselves.
    fn drag_to_pointer(&mut self) {
        let Some((from, start_x, moved)) = self
            .drag
            .as_ref()
            .map(|drag| (drag.index, drag.start_x, drag.moved))
        else {
            return;
        };
        if !self.editing {
            return;
        }
        let x = self.last_pointer_x;
        if !moved && (x - start_x).abs() < DRAG_THRESHOLD {
            return;
        }
        let is_pinned_app = |item: &Item| item.kind == Kind::App && item.pinned;
        if !self.items.get(from).is_some_and(is_pinned_app) {
            return;
        }
        let (Some(first), Some(last)) = (
            self.items.iter().position(is_pinned_app),
            self.items.iter().rposition(is_pinned_app),
        ) else {
            return;
        };
        let row_start = (self.surface_size.0 as f32 - self.idle_row_width()) / 2.0;
        let slot = ((x - row_start) / (ICON_SIZE + ICON_SPACING)).floor().max(0.0) as usize;
        let to = slot.clamp(first, last);
        if to != from {
            let item = self.items.remove(from);
            self.items.insert(to, item);
        }
        self.drag = Some(Drag {
            index: to,
            start_x,
            moved: true,
        });
    }

    /// End a press: save the new pin order if the icon was dragged.
    /// Returns the item to activate if it was a plain click.
    fn finish_drag(&mut self) -> Option<usize> {
        let drag = self.drag.take()?;
        if !drag.moved {
            // While unlocked, clicks only arrange; they don't open apps.
            return (!self.editing).then_some(drag.index);
        }
        let favorites: Vec<String> = self
            .items
            .iter()
            .filter(|item| item.kind == Kind::App && item.pinned)
            .map(|item| item.app_id.clone())
            .collect();
        match Config::new(cosmic_app_list_config::APP_ID, AppListConfig::VERSION) {
            Ok(config) => self.config.update_pinned(favorites, &config),
            Err(err) => tracing::error!(?err, "Failed to open dock config"),
        }
        None
    }

    /// The icon whose slot contains `x`, ignoring the magnification.
    fn hovered_index(&self, x: f32) -> Option<usize> {
        let half_slot = (ICON_SIZE + ICON_SPACING) / 2.0;
        (0..self.items.len()).find(|&index| (x - self.icon_center(index)).abs() <= half_slot)
    }

    /// Show thumbnails of the app's windows above its icon.
    fn open_preview(&mut self, index: usize) -> app::Task<Message> {
        let Some(item) = self.items.get(index) else {
            return Task::none();
        };
        if item.kind != Kind::App || item.toplevels.is_empty() {
            self.hover_since = None;
            return Task::none();
        }
        // Ask for fresh pictures of each window; they arrive as updates.
        for toplevel in &item.toplevels {
            self.send(WaylandRequest::Screencopy(toplevel.foreign_toplevel.clone()));
        }
        let id = window::Id::unique();
        self.preview = Some(Preview { id, index });
        self.over_preview = false;
        let settings = SctkPopupSettings {
            parent: self.surface,
            id,
            positioner: SctkPositioner {
                size: None,
                size_limits: Limits::NONE
                    .min_width(1.0)
                    .max_width(1200.0)
                    .min_height(1.0)
                    .max_height(400.0),
                anchor_rect: Rectangle {
                    x: (self.icon_center(index) - ICON_SIZE / 2.0) as i32,
                    y: 0,
                    width: ICON_SIZE as i32,
                    height: self.surface_size.1 as i32,
                },
                anchor: PopupAnchor::Top,
                gravity: Gravity::Top,
                constraint_adjustment: 15,
                offset: (0, -6),
                reactive: true,
            },
            parent_size: None,
            // Previews must not steal the pointer from the dock.
            grab: false,
            close_with_children: false,
            input_zone: None,
        };
        get_popup(settings)
    }

    fn close_preview(&mut self) -> app::Task<Message> {
        self.over_preview = false;
        match self.preview.take() {
            Some(preview) => destroy_popup(preview.id),
            None => Task::none(),
        }
    }

    /// The popup: one thumbnail with its title per window.
    fn preview_view(&self, preview: &Preview) -> Element<'_, Message> {
        let Some(item) = self.items.get(preview.index) else {
            return space::horizontal().into();
        };
        let mut thumbnails = row![].spacing(8);
        for toplevel in &item.toplevels {
            let picture: Element<'_, Message> = match self.thumbs.get(&toplevel.foreign_toplevel) {
                Some(image) => Image::new(Handle::from_rgba(
                    image.width,
                    image.height,
                    image.img.clone(),
                ))
                .into(),
                None => space::horizontal().into(),
            };
            let title = text::body(toplevel.title.clone())
                .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
                .width(Length::Fill);
            thumbnails = thumbnails.push(
                button::custom(
                    column![container(picture).center(Length::Fill), title]
                        .spacing(4)
                        .align_x(Alignment::Center),
                )
                .padding(6)
                .width(Length::Fixed(THUMB_WIDTH))
                .height(Length::Fixed(THUMB_HEIGHT))
                .class(theme::Button::Standard)
                .on_press(Message::PreviewActivate(toplevel.foreign_toplevel.clone())),
            );
        }

        let framed = container(thumbnails.padding(8)).class(theme::Container::custom(|theme| {
            let cosmic = theme.cosmic();
            let background = cosmic.background(theme.transparent);
            container::Style {
                text_color: Some(background.on.into()),
                icon_color: Some(background.on.into()),
                background: Some(Color::from(background.base).into()),
                border: Border {
                    radius: cosmic.corner_radii.radius_m.into(),
                    width: 1.0,
                    color: background.divider.into(),
                },
                ..Default::default()
            }
        }));

        let hover = mouse_area(framed)
            .on_enter(Message::PreviewEntered)
            .on_exit(Message::PreviewLeft);
        autosize::autosize(hover, AUTOSIZE_PREVIEW_ID.clone())
            .limits(
                Limits::NONE
                    .min_width(1.0)
                    .max_width(1200.0)
                    .min_height(1.0)
                    .max_height(400.0),
            )
            .into()
    }

    fn close_menu(&mut self) -> app::Task<Message> {
        match self.menu.take() {
            Some(menu) => destroy_popup(menu.id),
            None => Task::none(),
        }
    }

    fn icon_view(&self, index: usize, item: &Item) -> Element<'_, Message> {
        let size = (ICON_SIZE * self.icon_scale(index)).round() as u16;
        let app_icon = match item.kind {
            Kind::Trash => {
                // The coloured Pop icons exist at large sizes; the current icon
                // theme only has small ones, which would look flat here.
                let path = format!(
                    "/usr/share/icons/Pop/256x256/places/{}.svg",
                    if self.trash_full { "user-trash-full" } else { "user-trash" }
                );
                if std::path::Path::new(&path).exists() {
                    icon(icon::from_path(path.into())).size(size)
                } else {
                    icon::from_name(if self.trash_full { "user-trash-full" } else { "user-trash" })
                        .size(size)
                        .icon()
                }
            }
            Kind::App | Kind::Launcher => icon(
                fde::IconSource::from_unknown(item.entry.icon().unwrap_or_default())
                    .as_cosmic_icon(),
            )
            .size(size),
        };

        let running = !item.toplevels.is_empty();
        let focused = item.is_focused();
        let dot = container(space::horizontal().width(Length::Fixed(DOT_SIZE)))
            .height(Length::Fixed(DOT_SIZE))
            .class(theme::Container::custom(move |theme| {
                let color: Color = if focused {
                    theme.cosmic().accent_color().into()
                } else {
                    theme.cosmic().on_bg_color().into()
                };
                container::Style {
                    background: running.then_some(Background::Color(color)),
                    border: Border {
                        radius: (DOT_SIZE / 2.0).into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }));

        let content = column![
            app_icon,
            space::vertical().height(Length::Fixed(DOT_GAP + self.bounce_offset(item))),
            dot,
        ]
        .align_x(Alignment::Center);

        // Unlocked: pinned apps get a remove badge in the corner.
        let content: Element<'_, Message> = if self.editing && item.kind == Kind::App && item.pinned {
            let badge = button::icon(icon::from_name("window-close-symbolic").size(10))
                .padding(2)
                .class(theme::Button::Destructive)
                .on_press(Message::Unpin(index));
            stack![
                content,
                container(badge)
                    .width(Length::Fixed(size as f32))
                    .align_x(Alignment::Start)
            ]
            .into()
        } else {
            content.into()
        };

        mouse_area(content)
            .on_press(Message::Press(index))
            // The icon's area captures the release, so it must report it itself.
            .on_release(Message::Released)
            .on_right_press(Message::OpenMenu(index))
            .into()
    }

    fn menu_view(&self, menu: &Menu) -> Element<'_, Message> {
        let Some(item) = self.items.get(menu.index) else {
            return space::horizontal().into();
        };
        let running = !item.toplevels.is_empty();
        let edit_label = if self.editing {
            "Done Editing Dock"
        } else {
            "Edit Dock…"
        };
        if item.kind != Kind::App {
            let entries = column![
                menu_button(text::body(edit_label)).on_press(Message::MenuToggleEdit)
            ]
            .padding([8, 0]);
            return self.core.applet.popup_container(entries).into();
        }

        let mut entries = column![
            menu_button(text::body(if running { "New Window" } else { "Open" }))
                .on_press(Message::MenuLaunch),
            menu_button(text::body(if item.pinned {
                "Unpin from Dock"
            } else {
                "Pin to Dock"
            }))
            .on_press(Message::MenuTogglePin),
        ]
        .padding([8, 0]);
        if running {
            entries = entries.push(menu_button(text::body("Quit")).on_press(Message::MenuQuit));
        }
        entries = entries.push(
            menu_button(text::body(edit_label)).on_press(Message::MenuToggleEdit),
        );

        self.core.applet.popup_container(entries).into()
    }
}

impl cosmic::Application for Dock {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    const APP_ID: &'static str = "dev.asw.CosmicExtDock";

    fn init(core: app::Core, _flags: Self::Flags) -> (Self, app::Task<Self::Message>) {
        let surface = window::Id::unique();
        let mut dock = Self {
            core,
            surface,
            config: AppListConfig::default(),
            locales: get_languages_from_env(),
            desktop_entries: Vec::new(),
            items: Vec::new(),
            wayland_tx: None,
            pointer_x: None,
            last_pointer_x: 0.0,
            hover: 0.0,
            reveal: 1.0,
            expanded: false,
            bounce: None,
            menu: None,
            drag: None,
            editing: false,
            now: Instant::now(),
            outputs: HashMap::new(),
            active_workspaces: Vec::new(),
            surface_size: (0, 0),
            blur_region: None,
            published_icons: String::new(),
            trash_full: trash_is_full(),
            thumbs: HashMap::new(),
            preview: None,
            hover_index: None,
            hover_since: None,
            over_preview: false,
            left_at: None,
        };
        // Blur is requested for the bar alone, not the whole surface.
        dock.core.set_auto_blur(Default::default());
        dock.reload_desktop_entries();
        dock.rebuild_items();
        dock.surface_size = (dock.idle_width().ceil() as u32, IDLE_HEIGHT.ceil() as u32);

        let settings = SctkLayerSurfaceSettings {
            id: surface,
            layer: Layer::Top,
            keyboard_interactivity: KeyboardInteractivity::None,
            anchor: Anchor::BOTTOM,
            output: IcedOutput::Active,
            namespace: "dock".to_owned(),
            size: Some((Some(dock.surface_size.0), Some(dock.surface_size.1))),
            // Reserve no screen space: windows may cover the dock, which then hides.
            exclusive_zone: 0,
            ..Default::default()
        };
        // The blur region is sent by the first refresh, once the surface exists.
        (dock, get_layer_surface(settings))
    }

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            wayland_subscription().map(Message::Wayland),
            self.core
                .watch_config::<AppListConfig>(cosmic_app_list_config::APP_ID)
                .map(|update| Message::Config(update.config)),
        ];
        if self.is_animating() {
            subscriptions.push(cosmic::iced::time::every(FRAME).map(Message::Frame));
        }
        if self.hover_index.is_some() || self.preview.is_some() {
            subscriptions.push(cosmic::iced::time::every(HOVER_TICK).map(Message::HoverTick));
        }
        Subscription::batch(subscriptions)
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::Config(config) => {
                self.config = config;
                self.rebuild_items();
                self.refresh()
            }
            Message::Wayland(update) => match update {
                WaylandUpdate::Init(tx) => {
                    self.wayland_tx = Some(tx);
                    Task::none()
                }
                WaylandUpdate::Toplevel(event) => {
                    match event {
                        ToplevelUpdate::Add(info) => {
                            // The app has a window now, so stop its launch bounce.
                            let id = self.entry_for_app_id(&info.app_id).id().to_owned();
                            if self.bounce.as_ref().is_some_and(|(b, _)| *b == id) {
                                self.bounce = None;
                            }
                            self.add_toplevel(info);
                        }
                        ToplevelUpdate::Update(info) => self.update_toplevel(info),
                        ToplevelUpdate::Remove(handle) => self.remove_toplevel(&handle),
                    }
                    // Item count and whether a window covers the dock may have changed.
                    self.refresh()
                }
                WaylandUpdate::Workspace(workspaces) => {
                    self.active_workspaces = workspaces;
                    self.refresh()
                }
                WaylandUpdate::Output(event) => {
                    match event {
                        OutputUpdate::Add(output, info) | OutputUpdate::Update(output, info) => {
                            if let Some(size) = info.logical_size {
                                self.outputs.insert(output, size);
                            }
                        }
                        OutputUpdate::Remove(output) => {
                            self.outputs.remove(&output);
                        }
                    }
                    self.refresh()
                }
                WaylandUpdate::ActivationToken {
                    token,
                    app_id,
                    exec,
                    terminal,
                    ..
                } => {
                    let mut envs = Vec::new();
                    if let Some(token) = token {
                        envs.push(("XDG_ACTIVATION_TOKEN".to_owned(), token.clone()));
                        envs.push(("DESKTOP_STARTUP_ID".to_owned(), token));
                    }
                    tokio::spawn(async move {
                        cosmic::desktop::spawn_desktop_exec(exec, envs, app_id.as_deref(), terminal)
                            .await;
                    });
                    Task::none()
                }
                WaylandUpdate::Finished => {
                    tracing::error!("Wayland connection ended; window tracking stopped");
                    self.wayland_tx = None;
                    Task::none()
                }
                WaylandUpdate::Image(handle, image) => {
                    self.thumbs.insert(handle, image);
                    Task::none()
                }
            },
            Message::PointerMoved(position) => {
                let entered = self.pointer_x.is_none();
                if entered {
                    // Check the trash when the pointer arrives, not every frame.
                    self.trash_full = trash_is_full();
                }
                self.pointer_x = Some(position.x);
                self.last_pointer_x = position.x;
                if entered && !self.expanded {
                    // The surface is about to widen symmetrically, so shift the
                    // pointer into the expanded surface's coordinates.
                    self.last_pointer_x += (self.expanded_width() - self.idle_width()) / 2.0;
                    self.pointer_x = Some(self.last_pointer_x);
                }
                self.left_at = None;
                // Track which icon the pointer rests on, for the previews.
                let hovered = self.hovered_index(self.last_pointer_x);
                if hovered != self.hover_index {
                    self.hover_index = hovered;
                    self.hover_since = hovered.map(|_| Instant::now());
                    if self.preview.as_ref().is_some_and(|p| Some(p.index) != hovered) {
                        let close = self.close_preview();
                        self.drag_to_pointer();
                        return Task::batch([close, self.refresh()]);
                    }
                }
                self.drag_to_pointer();
                self.refresh()
            }
            Message::PointerLeft => {
                self.left_at = Some(Instant::now());
                self.hover_index = None;
                self.hover_since = None;
                self.pointer_x = None;
                // Leaving mid-drag keeps the new order; leaving mid-click cancels it.
                if self.drag.as_ref().is_some_and(|drag| drag.moved) {
                    self.finish_drag();
                }
                self.drag = None;
                self.refresh()
            }
            Message::Press(index) => {
                self.drag = Some(Drag {
                    index,
                    start_x: self.last_pointer_x,
                    moved: false,
                });
                Task::none()
            }
            Message::Released => match self.finish_drag() {
                Some(index) => self.update(Message::Activate(index)),
                None => self.refresh(),
            },
            Message::Frame(now) => {
                self.now = now;
                let hover_target = self.hover_target();
                self.hover += (hover_target - self.hover) * HOVER_EASING;
                if (self.hover - hover_target).abs() < 0.01 {
                    self.hover = hover_target;
                }
                if self
                    .bounce
                    .as_ref()
                    .is_some_and(|(_, started)| now.duration_since(*started) >= BOUNCE_DURATION)
                {
                    self.bounce = None;
                }
                let reveal_target = self.reveal_target();
                self.reveal += (reveal_target - self.reveal) * SLIDE_EASING;
                if (self.reveal - reveal_target).abs() < 0.01 {
                    self.reveal = reveal_target;
                }
                self.refresh()
            }
            Message::Activate(index) => {
                let Some(item) = self.items.get(index) else {
                    return Task::none();
                };
                match item.kind {
                    Kind::Trash => {
                        let mut command = std::process::Command::new("cosmic-files");
                        command.arg("--trash");
                        tokio::spawn(cosmic::process::spawn(command));
                    }
                    // The app library is not a window, so nothing ends a bounce.
                    Kind::Launcher => self.launch(index, false),
                    Kind::App => {
                        if !item.toplevels.is_empty() {
                            // Clicking the focused app minimizes all its windows;
                            // otherwise its most relevant window is brought forward.
                            let requests: Vec<ToplevelRequest> = if item.is_focused() {
                                item.toplevels
                                    .iter()
                                    .filter(|t| !t.state.contains(&State::Minimized))
                                    .map(|t| ToplevelRequest::Minimize(t.foreign_toplevel.clone()))
                                    .collect()
                            } else {
                                let target = item
                                    .toplevels
                                    .iter()
                                    .find(|t| !t.state.contains(&State::Minimized))
                                    .or_else(|| item.toplevels.first())
                                    .map(|t| t.foreign_toplevel.clone());
                                target.into_iter().map(ToplevelRequest::Activate).collect()
                            };
                            for request in requests {
                                self.send(WaylandRequest::Toplevel(request));
                            }
                        } else {
                            self.launch(index, true);
                        }
                    }
                }
                self.refresh()
            }
            Message::OpenMenu(index) => {
                let close = Task::batch([self.close_menu(), self.close_preview()]);
                if index >= self.items.len() {
                    return close;
                }

                let id = window::Id::unique();
                self.menu = Some(Menu { id, index });
                let settings = SctkPopupSettings {
                    parent: self.surface,
                    id,
                    positioner: SctkPositioner {
                        size: None,
                        size_limits: Limits::NONE
                            .min_width(180.0)
                            .max_width(280.0)
                            .min_height(1.0)
                            .max_height(400.0),
                        anchor_rect: Rectangle {
                            x: (self.icon_center(index) - ICON_SIZE / 2.0) as i32,
                            y: 0,
                            width: ICON_SIZE as i32,
                            height: self.surface_size.1 as i32,
                        },
                        anchor: PopupAnchor::Top,
                        gravity: Gravity::Top,
                        // slide_x, slide_y, flip_x, flip_y
                        constraint_adjustment: 15,
                        offset: (0, -4),
                        reactive: true,
                    },
                    parent_size: None,
                    grab: true,
                    close_with_children: false,
                    input_zone: None,
                };
                Task::batch([close, get_popup(settings)])
            }
            Message::MenuClosed(id) => {
                if self.menu.as_ref().is_some_and(|menu| menu.id == id) {
                    self.menu = None;
                }
                if self.preview.as_ref().is_some_and(|preview| preview.id == id) {
                    self.preview = None;
                    self.over_preview = false;
                }
                self.refresh()
            }
            Message::MenuLaunch => {
                let index = self.menu.as_ref().map(|menu| menu.index);
                let close = self.close_menu();
                if let Some(index) = index {
                    self.launch(index, true);
                }
                Task::batch([close, self.refresh()])
            }
            Message::MenuTogglePin => {
                let target = self
                    .menu
                    .as_ref()
                    .and_then(|menu| self.items.get(menu.index))
                    .map(|item| (item.app_id.clone(), item.pinned));
                let close = self.close_menu();
                if let Some((app_id, pinned)) = target {
                    match Config::new(cosmic_app_list_config::APP_ID, AppListConfig::VERSION) {
                        Ok(config) if pinned => self.config.remove_pinned(&app_id, &config),
                        Ok(config) => self.config.add_pinned(app_id, &config),
                        Err(err) => tracing::error!(?err, "Failed to open dock config"),
                    }
                    self.rebuild_items();
                }
                Task::batch([close, self.refresh()])
            }
            Message::HoverTick(now) => {
                if self.preview.is_some() {
                    let gone = self.pointer_x.is_none()
                        && !self.over_preview
                        && self.left_at.is_some_and(|t| now.duration_since(t) >= PREVIEW_GRACE);
                    if gone {
                        return self.close_preview();
                    }
                    return Task::none();
                }
                if let (Some(index), Some(since)) = (self.hover_index, self.hover_since)
                    && now.duration_since(since) >= PREVIEW_DELAY
                    && !self.editing
                    && self.menu.is_none()
                    && self.drag.is_none()
                {
                    return self.open_preview(index);
                }
                Task::none()
            }
            Message::PreviewEntered => {
                self.over_preview = true;
                Task::none()
            }
            Message::PreviewLeft => {
                self.over_preview = false;
                self.left_at = Some(Instant::now());
                Task::none()
            }
            Message::PreviewActivate(handle) => {
                self.send(WaylandRequest::Toplevel(ToplevelRequest::Activate(handle)));
                return self.close_preview();
            }
            Message::MenuToggleEdit => {
                self.editing = !self.editing;
                self.drag = None;
                let close = self.close_menu();
                Task::batch([close, self.refresh()])
            }
            Message::Unpin(index) => {
                let app_id = self
                    .items
                    .get(index)
                    .filter(|item| item.kind == Kind::App && item.pinned)
                    .map(|item| item.app_id.clone());
                if let Some(app_id) = app_id {
                    match Config::new(cosmic_app_list_config::APP_ID, AppListConfig::VERSION) {
                        Ok(config) => self.config.remove_pinned(&app_id, &config),
                        Err(err) => tracing::error!(?err, "Failed to open dock config"),
                    }
                    self.drag = None;
                    self.rebuild_items();
                }
                self.refresh()
            }
            Message::MenuQuit => {
                let handles: Vec<_> = self
                    .menu
                    .as_ref()
                    .and_then(|menu| self.items.get(menu.index))
                    .map(|item| {
                        item.toplevels
                            .iter()
                            .map(|t| t.foreign_toplevel.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                for handle in handles {
                    self.send(WaylandRequest::Toplevel(ToplevelRequest::Quit(handle)));
                }
                let close = self.close_menu();
                Task::batch([close, self.refresh()])
            }
        }
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::MenuClosed(id))
    }

    /// Unused: the dock has no main window, only the layer surface.
    fn view(&self) -> Element<'_, Message> {
        space::horizontal().into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        if let Some(menu) = self.menu.as_ref().filter(|menu| menu.id == id) {
            return self.menu_view(menu);
        }
        if let Some(preview) = self.preview.as_ref().filter(|preview| preview.id == id) {
            return self.preview_view(preview);
        }

        if self.is_hidden() {
            // Hidden: an invisible strip that reveals the dock on pointer contact.
            return mouse_area(
                container(space::horizontal().width(Length::Fill)).height(Length::Fill),
            )
            .on_move(Message::PointerMoved)
            .on_exit(Message::PointerLeft)
            .into();
        }

        let mut icon_row = row![].spacing(ICON_SPACING).align_y(Alignment::End);
        for (index, item) in self.items.iter().enumerate() {
            if item.kind == Kind::Trash && index > 0 {
                icon_row = icon_row.push(
                    container(space::horizontal().width(Length::Fixed(SEPARATOR_WIDTH)))
                        .height(Length::Fixed(ICON_SIZE * 0.7))
                        .class(theme::Container::custom(|theme| container::Style {
                            background: Some(Background::Color({
                                let mut color: Color = theme.cosmic().on_bg_color().into();
                                color.a = 0.3;
                                color
                            })),
                            ..Default::default()
                        })),
                );
            }
            icon_row = icon_row.push(self.icon_view(index, item));
        }

        // The bar keeps its idle height and only widens with the icons, so
        // magnified icons rise above it.
        let bar_width = self.row_width() + 2.0 * BAR_PADDING;
        let bar = container(space::horizontal().width(Length::Fixed(bar_width)))
            .height(Length::Fixed(BAR_HEIGHT))
            .class(theme::Container::custom(|theme| {
                let mut color: Color = theme.cosmic().bg_color().into();
                color.a = BAR_OPACITY;
                container::Style {
                    background: Some(Background::Color(color)),
                    border: Border {
                        radius: BAR_RADIUS.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }));

        let layers = stack![
            container(bar)
                .center_x(Length::Fill)
                .align_bottom(Length::Fill)
                .padding(Padding {
                    top: 0.0,
                    right: 0.0,
                    bottom: MARGIN_BOTTOM,
                    left: 0.0,
                }),
            container(icon_row)
                .center_x(Length::Fill)
                .align_bottom(Length::Fill)
                .padding(Padding {
                    top: 0.0,
                    right: 0.0,
                    bottom: MARGIN_BOTTOM + BAR_PADDING,
                    left: 0.0,
                }),
        ];

        // While sliding, the whole dock is pushed toward the bottom edge and
        // the part past the edge is simply off the surface.
        let offset = self.slide_offset();
        let content: Element<'_, Message> = if offset > 0.0 {
            pin(layers)
                .width(Length::Fill)
                .height(Length::Fill)
                .y(offset)
                .overflow(true)
                .into()
        } else {
            layers.into()
        };

        mouse_area(content)
            .on_move(Message::PointerMoved)
            .on_release(Message::Released)
            .on_exit(Message::PointerLeft)
            .into()
    }
}
