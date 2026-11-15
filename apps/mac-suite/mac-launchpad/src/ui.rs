// SPDX-License-Identifier: GPL-3.0-only

//! Full-screen layer-shell overlay with the application grid, folders,
//! drag-and-drop reordering, and animations.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use cosmic::desktop::spawn_desktop_exec;
use cosmic::iced::alignment::Horizontal;
use cosmic::iced::core::text::{Ellipsize, EllipsizeHeightLimit};
use cosmic::iced::core::{
    Clipboard, Layout, Shell, Transformation, Vector, Widget, layout as core_layout,
    overlay as core_overlay, renderer as core_renderer, widget as core_widget,
};
use cosmic::iced::event::{Status, listen_raw};
use cosmic::iced::keyboard::{Key, Modifiers, key::Named};
use cosmic::iced::mouse::{self, ScrollDelta};
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::platform_specific::shell::wayland::commands::layer_surface::{
    Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface,
};
use cosmic::iced::runtime::core::event::{PlatformSpecific, wayland};
use cosmic::iced::runtime::core::window::Id as SurfaceId;
use cosmic::iced::runtime::{Action, platform_specific, task};
use cosmic::iced::widget::{column, container, mouse_area, pin, row, stack};
use cosmic::iced::{
    Alignment, Border, Color, Length, Point, Rectangle, Shadow, Size, Subscription, stream, time,
    window,
};
use cosmic::theme::{Button, Container};
use cosmic::widget::{button, icon, space::vertical as vertical_space, text, text_input};
use cosmic::{Element, Renderer, Theme, app::{self, Task}};

use crate::apps::{self, AppEntry};
use crate::drag::{
    self, DragController, DragEvalContext, DragPhase, DragSource, DragTickOutcome, GridGeometry,
    Point2, Rect2, ReleaseOutcome,
};
use crate::ipc::{self, Command};
use crate::layout::{self, FOLDER_PAGE_SIZE, LayoutItem, SavedLayout};

const COLUMNS: usize = 7;
const ROWS: usize = 5;
const FOLDER_COLS: usize = 4;
const FOLDER_ROWS: usize = 3;
const ICON_SIZE: u16 = 96;
const CELL_W: f32 = 150.0;
const CELL_H: f32 = 140.0;
const SEARCH_BAR_H: f32 = 44.0;
const DOTS_H: f32 = 20.0;
const WHEEL_COOLDOWN: Duration = Duration::from_millis(250);
const OPEN_DURATION: Duration = Duration::from_millis(220);
const CLOSE_DURATION: Duration = Duration::from_millis(160);
const PAGE_SLIDE_DURATION: Duration = Duration::from_millis(250);
const SWIPE_SETTLE_DELAY: Duration = Duration::from_millis(110);
const SAVE_DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridLayout {
    pub columns: usize,
    pub rows: usize,
    pub cell_w: f32,
    pub cell_h: f32,
    pub icon_size: u16,
    pub spacing: f32,
    pub search_w: f32,
    pub top_pad: u16,
    pub bottom_pad: u16,
}

impl Default for GridLayout {
    fn default() -> Self {
        Self {
            columns: COLUMNS,
            rows: ROWS,
            cell_w: CELL_W,
            cell_h: CELL_H,
            icon_size: ICON_SIZE,
            spacing: 8.0,
            search_w: 360.0,
            top_pad: 40,
            bottom_pad: 36,
        }
    }
}

impl GridLayout {
    pub fn per_page(self) -> usize {
        self.columns.saturating_mul(self.rows).max(1)
    }

    pub fn grid_width(self) -> f32 {
        let cols = self.columns.max(1) as f32;
        cols * self.cell_w + (cols - 1.0) * self.spacing
    }

    pub fn grid_height(self) -> f32 {
        let rows = self.rows.max(1) as f32;
        rows * self.cell_h + (rows - 1.0) * self.spacing
    }

    pub fn main_geometry(self, viewport: Size) -> GridGeometry {
        let top_area = f32::from(self.top_pad) + SEARCH_BAR_H;
        let bottom_area = DOTS_H + f32::from(self.bottom_pad);
        let avail_h = (viewport.height - top_area - bottom_area).max(self.grid_height());
        let grid_w = self.grid_width();
        let grid_h = self.grid_height();
        let origin_x = ((viewport.width - grid_w) * 0.5).max(0.0);
        let origin_y = top_area + ((avail_h - grid_h) * 0.5).max(0.0);
        GridGeometry {
            origin_x,
            origin_y,
            columns: self.columns,
            rows: self.rows,
            cell_w: self.cell_w,
            cell_h: self.cell_h,
            spacing: self.spacing,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct FolderGeometry {
    panel: Rect2,
    grid: GridGeometry,
    icon_size: u16,
    pad_x: f32,
    pad_y: f32,
    header_h: f32,
    dots_h: f32,
}

fn compute_folder_geometry(layout: GridLayout, viewport: Size) -> FolderGeometry {
    let pad_x = 28.0_f32;
    let pad_y = 20.0_f32;
    let header_h = 52.0_f32;
    let dots_h = 22.0_f32;

    let max_panel_w = (viewport.width * 0.88).max(240.0);
    let max_panel_h = (viewport.height * 0.84).max(240.0);
    let avail_grid_w = (max_panel_w - pad_x * 2.0).max(160.0);
    let avail_grid_h = (max_panel_h - pad_y * 2.0 - header_h - dots_h).max(140.0);

    let spacing = layout.spacing;
    let max_cell_w = ((avail_grid_w - (FOLDER_COLS as f32 - 1.0) * spacing) / FOLDER_COLS as f32)
        .max(64.0);
    let max_cell_h = ((avail_grid_h - (FOLDER_ROWS as f32 - 1.0) * spacing) / FOLDER_ROWS as f32)
        .max(64.0);

    let cell_w = layout.cell_w.min(max_cell_w);
    let cell_h = layout.cell_h.min(max_cell_h);
    let scale = (cell_w / layout.cell_w).min(cell_h / layout.cell_h).min(1.0);
    let icon_size = ((f32::from(layout.icon_size) * scale).round() as u16).max(24);

    let grid_w = FOLDER_COLS as f32 * cell_w + (FOLDER_COLS as f32 - 1.0) * spacing;
    let grid_h = FOLDER_ROWS as f32 * cell_h + (FOLDER_ROWS as f32 - 1.0) * spacing;

    let panel_w = grid_w + pad_x * 2.0;
    let panel_h = grid_h + pad_y * 2.0 + header_h + dots_h;
    let panel_x = ((viewport.width - panel_w) * 0.5).max(0.0);
    let panel_y = ((viewport.height - panel_h) * 0.5).max(0.0);

    let grid = GridGeometry {
        origin_x: panel_x + pad_x,
        origin_y: panel_y + pad_y + header_h,
        columns: FOLDER_COLS,
        rows: FOLDER_ROWS,
        cell_w,
        cell_h,
        spacing,
    };

    FolderGeometry {
        panel: Rect2::new(panel_x, panel_y, panel_w, panel_h),
        grid,
        icon_size,
        pad_x,
        pad_y,
        header_h,
        dots_h,
    }
}

fn compute_layout(viewport_w: f32, viewport_h: f32) -> GridLayout {
    if !viewport_w.is_finite()
        || !viewport_h.is_finite()
        || viewport_w <= 0.0
        || viewport_h <= 0.0
    {
        return GridLayout::default();
    }
    let scale = (viewport_w / 1200.0).min(viewport_h / 900.0).clamp(0.55, 1.0);
    let spacing = (8.0 * scale).round().clamp(4.0, 8.0);
    let top_pad = (viewport_h * 0.045).round().clamp(8.0, 40.0) as u16;
    let bottom_pad = (viewport_h * 0.04).round().clamp(8.0, 36.0) as u16;
    let search_w = 360.0_f32
        .min((viewport_w - 32.0).max(120.0))
        .min(viewport_w.max(1.0));
    let chrome_h = f32::from(top_pad) + f32::from(bottom_pad) + SEARCH_BAR_H + DOTS_H;
    let horiz_margin = (viewport_w * 0.04).clamp(16.0, 64.0);
    let avail_w = (viewport_w - horiz_margin).max(1.0);
    let avail_h = (viewport_h - chrome_h).max(1.0);

    let cell_w = (CELL_W * scale).clamp(84.0, CELL_W).min(avail_w);
    let cell_h = (CELL_H * scale).clamp(78.0, CELL_H).min(avail_h);
    let icon_size = (f32::from(ICON_SIZE) * scale)
        .round()
        .clamp(16.0, f32::from(ICON_SIZE))
        .min((cell_h * (f32::from(ICON_SIZE) / CELL_H)).round().max(16.0)) as u16;

    let columns = (((avail_w + spacing) / (cell_w + spacing)).floor() as usize).clamp(1, COLUMNS);
    let rows = (((avail_h + spacing) / (cell_h + spacing)).floor() as usize).clamp(1, ROWS);

    GridLayout {
        columns,
        rows,
        cell_w,
        cell_h,
        icon_size,
        spacing,
        search_w,
        top_pad,
        bottom_pad,
    }
}

static INPUT_ID: LazyLock<cosmic::widget::Id> =
    LazyLock::new(|| cosmic::widget::Id::new("launchpad-search"));
static FOLDER_TITLE_ID: LazyLock<cosmic::widget::Id> =
    LazyLock::new(|| cosmic::widget::Id::new("launchpad-folder-title"));

fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[derive(Clone, Copy, Debug)]
enum WindowAnim {
    Opening { started_at: Instant, from: f32 },
    Closing { started_at: Instant, from: f32 },
}

#[derive(Clone, Copy, Debug)]
struct PageSlideAnim {
    from_offset: f32,
    started_at: Instant,
    duration: Duration,
    outgoing_page: Option<usize>,
    outgoing_delta: f32,
}

#[derive(Clone, Copy, Debug)]
struct TileSlide {
    from_x: f32,
    from_y: f32,
    to_x: f32,
    to_y: f32,
    started_at: Instant,
}

impl TileSlide {
    fn current(&self, now: Instant) -> (f32, f32, bool) {
        let elapsed = now.saturating_duration_since(self.started_at);
        let t = (elapsed.as_secs_f32() / drag::NEIGHBOR_SLIDE_DURATION.as_secs_f32()).clamp(0.0, 1.0);
        let e = ease_out_cubic(t);
        let x = self.from_x + (self.to_x - self.from_x) * e;
        let y = self.from_y + (self.to_y - self.from_y) * e;
        (x, y, t < 1.0)
    }
}

#[derive(Clone, Debug)]
struct OpenFolder {
    page: usize,
    index: usize,
    folder_page: usize,
    title_draft: String,
}

#[derive(Clone, Debug)]
pub enum Message {
    Command(Command),
    Loaded(Vec<AppEntry>),
    Query(String),
    Submit,
    Launch(usize),
    CellPress(DragSource),
    CursorMoved(Point),
    PointerReleased,
    BackgroundPress,
    BackgroundRightClick,
    ResetLayout,
    SortAZ,
    FolderTitleChanged(String),
    FolderTitleSubmit,
    FolderPage(isize),
    GoFolderPage(usize),
    CloseFolder,
    Page(isize),
    GoPage(usize),
    Scrolled(ScrollDelta),
    Resized(SurfaceId, Size),
    Key(Key, Modifiers, Status),
    Unfocused(SurfaceId),
    Tick(Instant),
    FlushSave(u64),
    Noop,
}

pub struct Launchpad {
    core: app::Core,
    window_id: SurfaceId,
    visible: bool,
    viewport: Size,
    apps: Vec<AppEntry>,
    app_lookup: HashMap<String, usize>,
    saved_layout: SavedLayout,
    query: String,
    /// Indices into `apps` for the current search query.
    matches: Vec<usize>,
    page: usize,
    layout: GridLayout,
    last_wheel: Option<Instant>,
    last_scroll_at: Option<Instant>,
    swipe_offset: f32,
    window_anim: Option<WindowAnim>,
    page_anim: Option<PageSlideAnim>,
    drag: DragController,
    last_cursor: Point,
    tile_targets: HashMap<String, (f32, f32)>,
    tile_slides: HashMap<String, TileSlide>,
    open_folder: Option<OpenFolder>,
    context_menu: Option<Point>,
    save_dirty: bool,
    save_gen: u64,
}

pub fn run(show: bool) -> cosmic::iced::Result {
    let settings = app::Settings::default()
        .antialiasing(true)
        .client_decorations(false)
        .default_text_size(14.0)
        .scale_factor(1.0)
        .no_main_window(true)
        .exit_on_close(false);
    app::run::<Launchpad>(settings, show)
}

fn page_count(n: usize, per_page: usize) -> usize {
    n.div_ceil(per_page.max(1)).max(1)
}

fn step_page(page: usize, delta: isize, count: usize) -> usize {
    page.saturating_add_signed(delta).min(count.saturating_sub(1))
}

fn page_slice(matches: &[usize], page: usize, per_page: usize) -> &[usize] {
    if matches.is_empty() {
        return &[];
    }
    let per_page = per_page.max(1);
    let clamped_page = page.min(page_count(matches.len(), per_page).saturating_sub(1));
    let start = clamped_page.saturating_mul(per_page).min(matches.len());
    let end = start.saturating_add(per_page).min(matches.len());
    &matches[start..end]
}

impl Launchpad {
    fn search_active(&self) -> bool {
        !self.query.trim().is_empty()
    }

    fn pages(&self) -> usize {
        if self.search_active() {
            page_count(self.matches.len(), self.layout.per_page())
        } else {
            self.saved_layout.pages.len().max(1)
        }
    }

    fn is_animating(&self) -> bool {
        self.window_anim.is_some()
            || self.page_anim.is_some()
            || self.swipe_offset != 0.0
            || !self.tile_slides.is_empty()
            || self.drag.needs_ticks()
    }

    fn window_progress(&self, now: Instant) -> f32 {
        match self.window_anim {
            Some(WindowAnim::Opening { started_at, from }) => {
                let t = (now.saturating_duration_since(started_at).as_secs_f32()
                    / OPEN_DURATION.as_secs_f32())
                .clamp(0.0, 1.0);
                (from + (1.0 - from) * ease_out_cubic(t)).clamp(0.0, 1.0)
            }
            Some(WindowAnim::Closing { started_at, from }) => {
                let t = (now.saturating_duration_since(started_at).as_secs_f32()
                    / CLOSE_DURATION.as_secs_f32())
                .clamp(0.0, 1.0);
                (from * (1.0 - ease_out_cubic(t))).clamp(0.0, 1.0)
            }
            None => {
                if self.visible {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }

    fn current_page_offset(&self, now: Instant) -> f32 {
        let anim_offset = match self.page_anim {
            Some(anim) => {
                let t = (now.saturating_duration_since(anim.started_at).as_secs_f32()
                    / anim.duration.as_secs_f32())
                .clamp(0.0, 1.0);
                anim.from_offset * (1.0 - ease_out_cubic(t))
            }
            None => 0.0,
        };
        anim_offset + self.swipe_offset
    }

    fn refilter(&mut self) {
        self.matches = apps::filter(&self.apps, &self.query);
        self.page = self.page.min(self.pages().saturating_sub(1));
    }

    fn rebuild_layout_from_disk(&mut self) {
        let saved = layout::layout_path().and_then(|p| layout::load_from_path(&p));
        self.saved_layout = layout::merge_layout(saved, &self.apps, self.layout.per_page());
        self.page = self.page.min(self.pages().saturating_sub(1));
        self.reset_tile_tracking();
    }

    fn reset_tile_tracking(&mut self) {
        self.tile_targets.clear();
        self.tile_slides.clear();
    }

    fn schedule_save(&mut self) -> Task<Message> {
        self.save_dirty = true;
        self.save_gen = self.save_gen.wrapping_add(1);
        let gen_id = self.save_gen;
        Task::perform(
            async move {
                tokio::time::sleep(SAVE_DEBOUNCE).await;
                gen_id
            },
            |g| cosmic::Action::App(Message::FlushSave(g)),
        )
    }

    fn flush_save_now(&mut self) {
        if !self.save_dirty {
            return;
        }
        self.save_dirty = false;
        if let Some(path) = layout::layout_path()
            && let Err(e) = layout::save_atomic(&path, &self.saved_layout)
        {
            tracing::warn!("failed to write {}: {e}", path.display());
        }
    }

    fn load_task() -> Task<Message> {
        Task::perform(
            async {
                tokio::task::spawn_blocking(|| apps::into_entries(apps::load()))
                    .await
                    .unwrap_or_default()
            },
            |apps| cosmic::Action::App(Message::Loaded(apps)),
        )
    }

    fn blur_task(&self) -> Task<Message> {
        let blur = self
            .core
            .system_theme()
            .cosmic()
            .frosted_system_interface
            .then(|| {
                vec![Rectangle {
                    x: 0.0,
                    y: 0.0,
                    width: f32::MAX,
                    height: f32::MAX,
                }]
            });
        task::effect(Action::PlatformSpecific(platform_specific::Action::Wayland(
            cosmic::iced::runtime::platform_specific::wayland::Action::BlurSurface(
                self.window_id,
                blur,
            ),
        )))
    }

    fn show(&mut self) -> Task<Message> {
        let now = Instant::now();
        if self.visible {
            if let Some(WindowAnim::Closing { .. }) = self.window_anim {
                let progress = self.window_progress(now);
                self.window_anim = Some(WindowAnim::Opening {
                    started_at: now,
                    from: progress,
                });
            }
            return Task::none();
        }
        self.visible = true;
        self.window_id = SurfaceId::unique();
        self.query.clear();
        self.page = 0;
        self.swipe_offset = 0.0;
        self.page_anim = None;
        self.drag.cancel();
        self.open_folder = None;
        self.context_menu = None;
        self.reset_tile_tracking();
        self.window_anim = Some(WindowAnim::Opening {
            started_at: now,
            from: 0.0,
        });
        self.refilter();
        let surface = get_layer_surface(SctkLayerSurfaceSettings {
            id: self.window_id,
            layer: Layer::Overlay,
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
            anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            output: IcedOutput::Active,
            namespace: "mac-launchpad".into(),
            size: Some((None, None)),
            exclusive_zone: -1,
            ..Default::default()
        });
        Task::batch([
            surface,
            self.blur_task(),
            text_input::focus(INPUT_ID.clone()),
            Self::load_task(),
        ])
    }

    fn begin_hide(&mut self) -> Task<Message> {
        if !self.visible {
            return Task::none();
        }
        self.commit_open_folder_title();
        self.flush_save_now();
        self.drag.cancel();
        self.context_menu = None;
        self.open_folder = None;

        let now = Instant::now();
        if matches!(self.window_anim, Some(WindowAnim::Closing { .. })) {
            return Task::none();
        }
        let progress = self.window_progress(now);
        self.window_anim = Some(WindowAnim::Closing {
            started_at: now,
            from: progress,
        });
        Task::none()
    }

    fn finish_hide(&mut self) -> Task<Message> {
        if !self.visible {
            return Task::none();
        }
        self.flush_save_now();
        self.visible = false;
        self.window_anim = None;
        self.page_anim = None;
        self.swipe_offset = 0.0;
        self.drag.cancel();
        self.open_folder = None;
        self.context_menu = None;
        destroy_layer_surface(self.window_id)
    }

    fn launch(&mut self, app_idx: usize) -> Task<Message> {
        let Some(app) = self.apps.get(app_idx) else {
            return Task::none();
        };
        let (exec, id, terminal) = (app.exec.clone(), app.id.clone(), app.terminal);
        Task::batch([
            self.finish_hide(),
            Task::perform(
                async move {
                    spawn_desktop_exec(exec, Vec::<(String, String)>::new(), Some(&id), terminal)
                        .await;
                },
                |()| cosmic::Action::App(Message::Noop),
            ),
        ])
    }

    fn lookup_app(&self, desktop_id: &str) -> Option<usize> {
        self.app_lookup
            .get(&apps::normalize_desktop_id(desktop_id))
            .copied()
    }

    fn folder_geometry(&self) -> FolderGeometry {
        compute_folder_geometry(self.layout, self.viewport)
    }

    fn drag_eval_context(&self) -> DragEvalContext {
        let main_grid = self.layout.main_geometry(self.viewport);
        let page_item_count = self
            .saved_layout
            .pages
            .get(self.page)
            .map_or(0, Vec::len);

        let (folder_panel_bounds, folder_grid, folder_page_offset, folder_page_item_count) =
            if let Some(open) = &self.open_folder
                && let Some(LayoutItem::Folder { apps: f_apps, .. }) = self
                    .saved_layout
                    .pages
                    .get(open.page)
                    .and_then(|p| p.get(open.index))
            {
                let fg = self.folder_geometry();
                let offset = open.folder_page.saturating_mul(FOLDER_PAGE_SIZE);
                let count = f_apps.len().saturating_sub(offset).min(FOLDER_PAGE_SIZE);
                (Some(fg.panel), Some(fg.grid), offset, count)
            } else {
                (None, None, 0, 0)
            };

        DragEvalContext {
            viewport_w: self.viewport.width.max(1.0),
            current_page: self.page,
            total_pages: self.pages(),
            page_item_count,
            main_grid,
            folder_panel_bounds,
            folder_grid,
            folder_page_offset,
            folder_page_item_count,
        }
    }

    fn sync_tile_slides(&mut self, now: Instant) {
        if self.search_active() {
            self.tile_targets.clear();
            self.tile_slides.clear();
            return;
        }
        let Some(page_items) = self.saved_layout.pages.get(self.page) else {
            return;
        };
        let geom = self.layout.main_geometry(self.viewport);
        let (dragged_idx_on_page, drop_idx) = match self.drag.phase() {
            DragPhase::Dragging {
                source,
                in_folder: false,
                drop_index,
                ..
            } => {
                let local = match *source {
                    DragSource::MainGrid { page, index } if page == self.page => Some(index),
                    _ => None,
                };
                (local, Some(*drop_index))
            }
            _ => (None, None),
        };

        let mut next_targets = HashMap::with_capacity(page_items.len());
        for (i, item) in page_items.iter().enumerate() {
            let slot = match drop_idx {
                Some(d) => match drag::preview_visual_slot(i, dragged_idx_on_page, d) {
                    Some(s) => s,
                    None => continue,
                },
                None => i,
            };
            let rect = geom.slot_rect(slot);
            let local_x = rect.x - geom.origin_x;
            let local_y = rect.y - geom.origin_y;
            let key = item.anim_key();

            if let Some(&(prev_tx, prev_ty)) = self.tile_targets.get(&key)
                && ((prev_tx - local_x).abs() > 0.5 || (prev_ty - local_y).abs() > 0.5)
            {
                let (cur_x, cur_y) = self
                    .tile_slides
                    .get(&key)
                    .map(|s| {
                        let (x, y, _) = s.current(now);
                        (x, y)
                    })
                    .unwrap_or((prev_tx, prev_ty));
                self.tile_slides.insert(
                    key.clone(),
                    TileSlide {
                        from_x: cur_x,
                        from_y: cur_y,
                        to_x: local_x,
                        to_y: local_y,
                        started_at: now,
                    },
                );
            }
            next_targets.insert(key, (local_x, local_y));
        }

        self.tile_targets = next_targets;
        self.tile_slides.retain(|k, s| {
            self.tile_targets.contains_key(k) && s.current(now).2
        });
    }

    fn go_to_page(&mut self, target_page: usize, now: Instant) {
        let count = self.pages();
        let clamped = target_page.min(count.saturating_sub(1));
        if clamped == self.page {
            return;
        }
        let stride = self.viewport.width.max(400.0);
        let dir = if clamped > self.page { 1.0 } else { -1.0 };
        let prev = self.page;
        let carry = self.current_page_offset(now);
        self.page = clamped;
        self.swipe_offset = 0.0;
        self.page_anim = Some(PageSlideAnim {
            from_offset: dir * stride + carry,
            started_at: now,
            duration: PAGE_SLIDE_DURATION,
            outgoing_page: Some(prev),
            outgoing_delta: -dir * stride,
        });
        self.reset_tile_tracking();
        if self.drag.is_dragging() {
            let ctx = self.drag_eval_context();
            self.drag.on_page_changed(now, &ctx);
            self.sync_tile_slides(now);
        }
    }

    fn commit_open_folder_title(&mut self) {
        if let Some(open) = &mut self.open_folder {
            let finalized = open.title_draft.trim();
            let finalized = if finalized.is_empty() {
                "Folder".to_owned()
            } else {
                finalized.to_owned()
            };
            open.title_draft = finalized.clone();
            self.saved_layout
                .set_folder_name(open.page, open.index, finalized, true);
        }
    }

    fn is_pointer_over_interactive(&self, pos: Point) -> bool {
        let p = Point2::new(pos.x, pos.y);
        // Search bar region
        let search_x = (self.viewport.width - self.layout.search_w) * 0.5;
        let search_rect = Rect2::new(
            search_x,
            f32::from(self.layout.top_pad),
            self.layout.search_w,
            SEARCH_BAR_H,
        );
        if search_rect.contains(p) {
            return true;
        }
        if self.open_folder.is_some() {
            return true;
        }
        let geom = self.layout.main_geometry(self.viewport);
        let count = if self.search_active() {
            page_slice(&self.matches, self.page, self.layout.per_page()).len()
        } else {
            self.saved_layout.pages.get(self.page).map_or(0, Vec::len)
        };
        geom.hit_test_cell(p, count).is_some()
    }

    fn app_cell_visual(&self, app_idx: usize, icon_size: u16, cell_w: f32, cell_h: f32, hovered_highlight: bool) -> Element<'_, Message> {
        let app = &self.apps[app_idx];
        let content = column![
            icon::icon(app.icon.clone()).size(icon_size),
            text::body(app.name.as_str())
                .width(Length::Fill)
                .align_x(Horizontal::Center)
                .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
        ]
        .spacing(8)
        .align_x(Alignment::Center)
        .width(Length::Fill);

        container(content)
            .width(Length::Fixed(cell_w))
            .height(Length::Fixed(cell_h))
            .padding([10, 6])
            .center_x(Length::Fixed(cell_w))
            .class(Container::custom(move |t| {
                let c = t.cosmic();
                container::Style {
                    background: hovered_highlight
                        .then(|| Color::from(c.on_bg_color()).scale_alpha(0.14).into()),
                    text_color: Some(c.on_bg_color().into()),
                    icon_color: Some(c.on_bg_color().into()),
                    border: Border {
                        radius: c.corner_radii.radius_m.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }))
            .into()
    }

    fn folder_cell_visual<'a>(
        &'a self,
        name: &'a str,
        app_ids: &[String],
        targeted: bool,
        armed: bool,
    ) -> Element<'a, Message> {
        let icon_size = self.layout.icon_size;
        let pad = 7_u16;
        let gap = 3_u16;
        let inner = icon_size.saturating_sub(pad * 2 + gap * 2);
        let mini = (inner / 3).max(8);

        let mut rows_vec: Vec<Element<'_, Message>> = Vec::with_capacity(3);
        for r in 0..3 {
            let mut cols_vec: Vec<Element<'_, Message>> = Vec::with_capacity(3);
            for c in 0..3 {
                let idx = r * 3 + c;
                let cell_el: Element<'_, Message> = if let Some(id) = app_ids.get(idx)
                    && let Some(app_idx) = self.lookup_app(id)
                {
                    icon::icon(self.apps[app_idx].icon.clone())
                        .size(mini)
                        .into()
                } else {
                    container(vertical_space().height(Length::Fixed(f32::from(mini))))
                        .width(Length::Fixed(f32::from(mini)))
                        .height(Length::Fixed(f32::from(mini)))
                        .into()
                };
                cols_vec.push(cell_el);
            }
            rows_vec.push(
                row(cols_vec)
                    .spacing(f32::from(gap))
                    .align_y(Alignment::Center)
                    .into(),
            );
        }

        let radius = f32::from(icon_size) * 0.22;
        let tile = container(
            column(rows_vec)
                .spacing(f32::from(gap))
                .align_x(Alignment::Center),
        )
        .width(Length::Fixed(f32::from(icon_size)))
        .height(Length::Fixed(f32::from(icon_size)))
        .center_x(Length::Fixed(f32::from(icon_size)))
        .center_y(Length::Fixed(f32::from(icon_size)))
        .class(Container::custom(move |t| {
            let tc = t.cosmic();
            let base = Color::from(tc.on_bg_color());
            let accent = Color::from(tc.accent_color());
            container::Style {
                background: Some(
                    base.scale_alpha(if armed {
                        0.32
                    } else if targeted {
                        0.24
                    } else {
                        0.15
                    })
                    .into(),
                ),
                border: Border {
                    radius: radius.into(),
                    width: if armed { 2.0 } else { 1.0 },
                    color: if armed {
                        accent
                    } else {
                        base.scale_alpha(0.22)
                    },
                },
                ..Default::default()
            }
        }));

        let content = column![
            tile,
            text::body(name)
                .width(Length::Fill)
                .align_x(Horizontal::Center)
                .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
        ]
        .spacing(8)
        .align_x(Alignment::Center)
        .width(Length::Fill);

        container(content)
            .width(Length::Fixed(self.layout.cell_w))
            .height(Length::Fixed(self.layout.cell_h))
            .padding([10, 6])
            .center_x(Length::Fixed(self.layout.cell_w))
            .into()
    }

    fn interactive_search_cell(&self, app_idx: usize) -> Element<'_, Message> {
        let app = &self.apps[app_idx];
        let content = column![
            icon::icon(app.icon.clone()).size(self.layout.icon_size),
            text::body(app.name.as_str())
                .width(Length::Fill)
                .align_x(Horizontal::Center)
                .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
        ]
        .spacing(8)
        .align_x(Alignment::Center)
        .width(Length::Fill);

        button::custom(content)
            .width(Length::Fixed(self.layout.cell_w))
            .height(Length::Fixed(self.layout.cell_h))
            .padding([10, 6])
            .on_press(Message::Launch(app_idx))
            .class(Button::Custom {
                active: Box::new(|_, t| cell_style(t, false)),
                hovered: Box::new(|_, t| cell_style(t, true)),
                pressed: Box::new(|_, t| cell_style(t, true)),
                disabled: Box::new(|t| cell_style(t, false)),
            })
            .into()
    }

    fn render_layout_item_cell<'a>(
        &'a self,
        item: &'a LayoutItem,
        targeted: bool,
        armed: bool,
    ) -> Element<'a, Message> {
        match item {
            LayoutItem::App(id) => {
                if let Some(app_idx) = self.lookup_app(id) {
                    if targeted {
                        let base = self.app_cell_visual(
                            app_idx,
                            self.layout.icon_size,
                            self.layout.cell_w,
                            self.layout.cell_h,
                            false,
                        );
                        container(base)
                            .width(Length::Fixed(self.layout.cell_w))
                            .height(Length::Fixed(self.layout.cell_h))
                            .class(Container::custom(move |t| {
                                let tc = t.cosmic();
                                let accent = Color::from(tc.accent_color());
                                let on_bg = Color::from(tc.on_bg_color());
                                container::Style {
                                    background: Some(
                                        on_bg.scale_alpha(if armed { 0.24 } else { 0.14 }).into(),
                                    ),
                                    border: Border {
                                        radius: tc.corner_radii.radius_m.into(),
                                        width: if armed { 2.0 } else { 1.0 },
                                        color: if armed {
                                            accent
                                        } else {
                                            on_bg.scale_alpha(0.35)
                                        },
                                    },
                                    ..Default::default()
                                }
                            }))
                            .into()
                    } else {
                        self.app_cell_visual(
                            app_idx,
                            self.layout.icon_size,
                            self.layout.cell_w,
                            self.layout.cell_h,
                            false,
                        )
                    }
                } else {
                    vertical_space()
                        .height(Length::Fixed(self.layout.cell_h))
                        .into()
                }
            }
            LayoutItem::Folder { name, apps } => self.folder_cell_visual(name, apps, targeted, armed),
        }
    }

    fn render_main_page(&self, page_idx: usize, now: Instant, interactive: bool) -> Element<'_, Message> {
        let geom = self.layout.main_geometry(self.viewport);
        let grid_w = geom.total_width();
        let grid_h = geom.total_height();

        if self.search_active() {
            if self.matches.is_empty() {
                return container(text::title3("No results"))
                    .width(Length::Fixed(grid_w))
                    .height(Length::Fixed(grid_h))
                    .center_x(Length::Fixed(grid_w))
                    .center_y(Length::Fixed(grid_h))
                    .into();
            }
            let slice = page_slice(&self.matches, page_idx, self.layout.per_page());
            let mut children: Vec<Element<'_, Message>> = Vec::with_capacity(slice.len());
            for (slot, &app_idx) in slice.iter().enumerate() {
                let rect = geom.slot_rect(slot);
                let cell = self.interactive_search_cell(app_idx);
                children.push(
                    pin(cell)
                        .x(rect.x - geom.origin_x)
                        .y(rect.y - geom.origin_y)
                        .into(),
                );
            }
            return stack(children)
                .width(Length::Fixed(grid_w))
                .height(Length::Fixed(grid_h))
                .into();
        }

        let Some(page_items) = self.saved_layout.pages.get(page_idx) else {
            return container(vertical_space().height(Length::Fixed(1.0)))
                .width(Length::Fixed(grid_w))
                .height(Length::Fixed(grid_h))
                .into();
        };

        let (dragged_idx_on_page, drop_idx, hover_target) = match self.drag.phase() {
            DragPhase::Dragging {
                source,
                in_folder: false,
                drop_index,
                folder_hover,
                ..
            } if page_idx == self.page => {
                let local = match *source {
                    DragSource::MainGrid { page, index } if page == page_idx => Some(index),
                    _ => None,
                };
                let target = folder_hover
                    .filter(|fh| fh.page == page_idx)
                    .map(|fh| (fh.target_index, fh.armed));
                (local, Some(*drop_index), target)
            }
            _ => (None, None, None),
        };

        let mut children: Vec<Element<'_, Message>> = Vec::with_capacity(page_items.len());
        for (i, item) in page_items.iter().enumerate() {
            let slot = match drop_idx {
                Some(d) => match drag::preview_visual_slot(i, dragged_idx_on_page, d) {
                    Some(s) => s,
                    None => continue,
                },
                None => i,
            };

            let rect = geom.slot_rect(slot);
            let default_x = rect.x - geom.origin_x;
            let default_y = rect.y - geom.origin_y;
            let (pos_x, pos_y) = if page_idx == self.page
                && let Some(slide) = self.tile_slides.get(&item.anim_key())
            {
                let (x, y, active) = slide.current(now);
                if active { (x, y) } else { (default_x, default_y) }
            } else {
                (default_x, default_y)
            };

            let (targeted, armed) = match hover_target {
                Some((t_idx, is_armed)) if t_idx == i => (true, is_armed),
                _ => (false, false),
            };
            let visual = self.render_layout_item_cell(item, targeted, armed);
            let widget: Element<'_, Message> = if interactive {
                mouse_area(visual)
                    .on_press(Message::CellPress(DragSource::MainGrid {
                        page: page_idx,
                        index: i,
                    }))
                    .interaction(mouse::Interaction::Pointer)
                    .into()
            } else {
                visual
            };

            children.push(pin(widget).x(pos_x).y(pos_y).into());
        }

        stack(children)
            .width(Length::Fixed(grid_w))
            .height(Length::Fixed(grid_h))
            .into()
    }

    fn grid(&self, now: Instant) -> Element<'_, Message> {
        let geom = self.layout.main_geometry(self.viewport);
        let grid_w = geom.total_width();
        let grid_h = geom.total_height();
        let offset_x = self.current_page_offset(now);
        let total = self.pages();

        if offset_x.abs() < 0.5 {
            return container(self.render_main_page(self.page, now, true))
                .width(Length::Fixed(grid_w))
                .height(Length::Fixed(grid_h))
                .into();
        }

        let stride = self.viewport.width.max(400.0);
        let mut layers: Vec<Element<'_, Message>> = Vec::with_capacity(2);

        if let Some(anim) = self.page_anim
            && let Some(out_page) = anim.outgoing_page
            && out_page < total
        {
            let out_x = offset_x + anim.outgoing_delta;
            layers.push(
                pin(self.render_main_page(out_page, now, false))
                    .x(geom.origin_x + out_x)
                    .y(geom.origin_y)
                    .overflow(true)
                    .into(),
            );
        } else if self.swipe_offset < 0.0 && self.page + 1 < total {
            layers.push(
                pin(self.render_main_page(self.page + 1, now, false))
                    .x(geom.origin_x + offset_x + stride)
                    .y(geom.origin_y)
                    .overflow(true)
                    .into(),
            );
        } else if self.swipe_offset > 0.0 && self.page > 0 {
            layers.push(
                pin(self.render_main_page(self.page - 1, now, false))
                    .x(geom.origin_x + offset_x - stride)
                    .y(geom.origin_y)
                    .overflow(true)
                    .into(),
            );
        }

        layers.push(
            pin(self.render_main_page(self.page, now, false))
                .x(geom.origin_x + offset_x)
                .y(geom.origin_y)
                .overflow(true)
                .into(),
        );

        container(
            stack(layers)
                .width(Length::Fixed(self.viewport.width))
                .height(Length::Fixed(grid_h)),
        )
        .width(Length::Fixed(self.viewport.width))
        .height(Length::Fixed(grid_h))
        .clip(true)
        .into()
    }

    fn dots(&self) -> Element<'_, Message> {
        let count = self.pages();
        if count < 2 {
            return vertical_space().height(Length::Fixed(DOTS_H)).into();
        }
        row((0..count).map(|i| {
            let active = i == self.page;
            mouse_area(
                container(vertical_space().height(Length::Fixed(0.0)))
                    .width(Length::Fixed(10.0))
                    .height(Length::Fixed(10.0))
                    .class(Container::custom(move |t| {
                        let c = Color::from(t.cosmic().on_bg_color());
                        container::Style {
                            background: Some(c.scale_alpha(if active { 0.9 } else { 0.3 }).into()),
                            border: Border {
                                radius: 5.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }
                    })),
            )
            .on_press(Message::GoPage(i))
            .into()
        }))
        .spacing(12)
        .height(Length::Fixed(DOTS_H))
        .align_y(Alignment::Center)
        .into()
    }

    fn folder_overlay<'a>(&'a self, open: &'a OpenFolder) -> Option<Element<'a, Message>> {
        let LayoutItem::Folder { apps: f_apps, .. } = self
            .saved_layout
            .pages
            .get(open.page)?
            .get(open.index)?
        else {
            return None;
        };

        let fg = self.folder_geometry();
        let folder_pages = page_count(f_apps.len(), FOLDER_PAGE_SIZE);
        let cur_fpage = open.folder_page.min(folder_pages.saturating_sub(1));
        let start = cur_fpage * FOLDER_PAGE_SIZE;
        let end = (start + FOLDER_PAGE_SIZE).min(f_apps.len());
        let slice = &f_apps[start..end];

        let (dragged_local, drop_idx) = match self.drag.phase() {
            DragPhase::Dragging {
                source:
                    DragSource::Folder {
                        folder_page,
                        folder_index,
                        app_index,
                        ..
                    },
                in_folder: true,
                drop_index,
                ..
            } if *folder_page == open.page && *folder_index == open.index => {
                (app_index.checked_sub(start), Some(*drop_index))
            }
            _ => (None, None),
        };

        let mut grid_children: Vec<Element<'_, Message>> = Vec::with_capacity(slice.len());
        for (local_idx, app_id) in slice.iter().enumerate() {
            let slot = match drop_idx {
                Some(d) => match drag::preview_visual_slot(local_idx, dragged_local, d) {
                    Some(s) => s,
                    None => continue,
                },
                None => local_idx,
            };
            let Some(app_idx) = self.lookup_app(app_id) else {
                continue;
            };
            let rect = fg.grid.slot_rect(slot);
            let cell = self.app_cell_visual(
                app_idx,
                fg.icon_size,
                fg.grid.cell_w,
                fg.grid.cell_h,
                false,
            );
            let clickable = mouse_area(cell)
                .on_press(Message::CellPress(DragSource::Folder {
                    folder_page: open.page,
                    folder_index: open.index,
                    folder_app_page: cur_fpage,
                    app_index: start + local_idx,
                }))
                .interaction(mouse::Interaction::Pointer);
            grid_children.push(
                pin(clickable)
                    .x(rect.x - fg.grid.origin_x)
                    .y(rect.y - fg.grid.origin_y)
                    .into(),
            );
        }

        let folder_grid = stack(grid_children)
            .width(Length::Fixed(fg.grid.total_width()))
            .height(Length::Fixed(fg.grid.total_height()));

        let title_input = text_input::text_input("Folder", &open.title_draft)
            .id(FOLDER_TITLE_ID.clone())
            .on_input(Message::FolderTitleChanged)
            .on_submit(|_| Message::FolderTitleSubmit)
            .width(Length::Fixed((fg.panel.w * 0.65).max(160.0)))
            .size(20.0)
            .padding([6, 12]);

        let folder_dots: Element<'_, Message> = if folder_pages < 2 {
            vertical_space().height(Length::Fixed(fg.dots_h)).into()
        } else {
            row((0..folder_pages).map(|i| {
                let active = i == cur_fpage;
                mouse_area(
                    container(vertical_space().height(Length::Fixed(0.0)))
                        .width(Length::Fixed(8.0))
                        .height(Length::Fixed(8.0))
                        .class(Container::custom(move |t| {
                            let c = Color::from(t.cosmic().on_bg_color());
                            container::Style {
                                background: Some(
                                    c.scale_alpha(if active { 0.9 } else { 0.3 }).into(),
                                ),
                                border: Border {
                                    radius: 4.0.into(),
                                    ..Default::default()
                                },
                                ..Default::default()
                            }
                        })),
                )
                .on_press(Message::GoFolderPage(i))
                .into()
            }))
            .spacing(10)
            .height(Length::Fixed(fg.dots_h))
            .align_y(Alignment::Center)
            .into()
        };

        let panel_body = column![
            container(title_input)
                .height(Length::Fixed(fg.header_h))
                .center_x(Length::Fill)
                .center_y(Length::Fixed(fg.header_h)),
            folder_grid,
            container(folder_dots)
                .center_x(Length::Fill)
                .height(Length::Fixed(fg.dots_h)),
        ]
        .align_x(Alignment::Center)
        .width(Length::Fill)
        .height(Length::Fill);

        let panel = mouse_area(
            container(panel_body)
                .width(Length::Fixed(fg.panel.w))
                .height(Length::Fixed(fg.panel.h))
                .padding([fg.pad_y as u16, fg.pad_x as u16])
                .class(Container::custom(|t| {
                    let tc = t.cosmic();
                    let bg = Color::from(tc.bg_color());
                    let on_bg = Color::from(tc.on_bg_color());
                    container::Style {
                        background: Some(bg.scale_alpha(0.88).into()),
                        text_color: Some(on_bg),
                        icon_color: Some(on_bg),
                        border: Border {
                            radius: 24.0.into(),
                            width: 1.0,
                            color: on_bg.scale_alpha(0.18),
                        },
                        shadow: Shadow {
                            color: Color::from_rgba(0.0, 0.0, 0.0, 0.45),
                            offset: Vector::new(0.0, 12.0),
                            blur_radius: 36.0,
                        },
                        ..Default::default()
                    }
                })),
        )
        .on_press(Message::Noop);

        let backdrop = mouse_area(
            container(vertical_space().height(Length::Fill))
                .width(Length::Fill)
                .height(Length::Fill)
                .class(Container::custom(|_| container::Style {
                    background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.35).into()),
                    ..Default::default()
                })),
        )
        .on_press(Message::CloseFolder);

        Some(
            stack([
                backdrop.into(),
                pin(panel).x(fg.panel.x).y(fg.panel.y).into(),
            ])
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        )
    }

    fn context_menu_overlay(&self, pos: Point) -> Element<'_, Message> {
        let menu_w = 180.0_f32;
        let menu_h = 84.0_f32;
        let x = pos.x.clamp(8.0, (self.viewport.width - menu_w - 8.0).max(8.0));
        let y = pos.y.clamp(8.0, (self.viewport.height - menu_h - 8.0).max(8.0));

        let item_btn = |label: &'static str, msg: Message| -> Element<'_, Message> {
            button::custom(
                text::body(label)
                    .width(Length::Fill)
                    .align_x(Horizontal::Left),
            )
            .width(Length::Fill)
            .padding([8, 12])
            .on_press(msg)
            .class(Button::Custom {
                active: Box::new(|_, t| cell_style(t, false)),
                hovered: Box::new(|_, t| cell_style(t, true)),
                pressed: Box::new(|_, t| cell_style(t, true)),
                disabled: Box::new(|t| cell_style(t, false)),
            })
            .into()
        };

        let menu = container(
            column![
                item_btn("Reset layout", Message::ResetLayout),
                item_btn("Sort A–Z", Message::SortAZ),
            ]
            .spacing(2)
            .width(Length::Fill),
        )
        .width(Length::Fixed(menu_w))
        .padding([6, 6])
        .class(Container::custom(|t| {
            let tc = t.cosmic();
            let bg = Color::from(tc.bg_color());
            let on_bg = Color::from(tc.on_bg_color());
            container::Style {
                background: Some(bg.scale_alpha(0.94).into()),
                text_color: Some(on_bg),
                icon_color: Some(on_bg),
                border: Border {
                    radius: tc.corner_radii.radius_m.into(),
                    width: 1.0,
                    color: on_bg.scale_alpha(0.18),
                },
                shadow: Shadow {
                    color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
                    offset: Vector::new(0.0, 6.0),
                    blur_radius: 18.0,
                },
                ..Default::default()
            }
        }));

        pin(menu).x(x).y(y).into()
    }

    fn dragged_overlay(&self) -> Option<Element<'_, Message>> {
        let DragPhase::Dragging {
            source,
            pointer,
            grab_offset,
            in_folder,
            ..
        } = self.drag.phase()
        else {
            return None;
        };

        let fg = self.folder_geometry();
        let (cell_w, cell_h, visual): (f32, f32, Element<'_, Message>) = match *source {
            DragSource::MainGrid { page, index } => {
                let item = self.saved_layout.pages.get(page)?.get(index)?;
                (
                    self.layout.cell_w,
                    self.layout.cell_h,
                    self.render_layout_item_cell(item, false, false),
                )
            }
            DragSource::Folder {
                folder_page,
                folder_index,
                app_index,
                ..
            } => {
                let LayoutItem::Folder { apps: f_apps, .. } =
                    self.saved_layout.pages.get(folder_page)?.get(folder_index)?
                else {
                    return None;
                };
                let id = f_apps.get(app_index)?;
                let app_idx = self.lookup_app(id)?;
                if *in_folder {
                    (
                        fg.grid.cell_w,
                        fg.grid.cell_h,
                        self.app_cell_visual(
                            app_idx,
                            fg.icon_size,
                            fg.grid.cell_w,
                            fg.grid.cell_h,
                            false,
                        ),
                    )
                } else {
                    (
                        self.layout.cell_w,
                        self.layout.cell_h,
                        self.app_cell_visual(
                            app_idx,
                            self.layout.icon_size,
                            self.layout.cell_w,
                            self.layout.cell_h,
                            false,
                        ),
                    )
                }
            }
        };

        let shadow_box = container(visual)
            .width(Length::Fixed(cell_w))
            .height(Length::Fixed(cell_h))
            .class(Container::custom(|t| {
                let tc = t.cosmic();
                container::Style {
                    shadow: Shadow {
                        color: Color::from_rgba(0.0, 0.0, 0.0, 0.45),
                        offset: Vector::new(0.0, 10.0),
                        blur_radius: 22.0,
                    },
                    border: Border {
                        radius: tc.corner_radii.radius_m.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }));

        let scaled = ScaleBox::new(shadow_box, drag::DRAG_SCALE);
        let x = pointer.x - grab_offset.x;
        let y = pointer.y - grab_offset.y;

        Some(
            pin(scaled)
                .x(x)
                .y(y)
                .overflow(true)
                .into(),
        )
    }
}

fn cell_style(theme: &Theme, hovered: bool) -> button::Style {
    let c = theme.cosmic();
    button::Style {
        background: hovered.then(|| Color::from(c.on_bg_color()).scale_alpha(0.14).into()),
        text_color: Some(c.on_bg_color().into()),
        icon_color: Some(c.on_bg_color().into()),
        border_radius: c.corner_radii.radius_m.into(),
        ..Default::default()
    }
}

/// Lightweight transformation wrapper for smooth grid zoom and lifted drag scale.
struct ScaleBox<'a, Message> {
    content: Element<'a, Message>,
    scale: f32,
}

impl<'a, Message> ScaleBox<'a, Message> {
    fn new(content: impl Into<Element<'a, Message>>, scale: f32) -> Self {
        Self {
            content: content.into(),
            scale,
        }
    }
}

impl<Message> Widget<Message, Theme, Renderer> for ScaleBox<'_, Message> {
    fn tag(&self) -> core_widget::tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> core_widget::tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<core_widget::Tree> {
        self.content.as_widget().children()
    }

    fn diff(&mut self, tree: &mut core_widget::Tree) {
        self.content.as_widget_mut().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut core_widget::Tree,
        renderer: &Renderer,
        limits: &core_layout::Limits,
    ) -> core_layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut core_widget::Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn core_widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut core_widget::Tree,
        event: &cosmic::iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &core_widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn draw(
        &self,
        tree: &core_widget::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &core_renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if (self.scale - 1.0).abs() <= 0.0005 {
            self.content
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
            return;
        }
        let bounds = layout.bounds();
        let cx = bounds.x + bounds.width * 0.5;
        let cy = bounds.y + bounds.height * 0.5;
        let transform = Transformation::translate(cx, cy)
            * Transformation::scale(self.scale)
            * Transformation::translate(-cx, -cy);
        let inverse = transform.inverse();
        use core_renderer::Renderer as _;
        renderer.with_layer(*viewport, |renderer| {
            renderer.with_transformation(transform, |renderer| {
                self.content.as_widget().draw(
                    tree,
                    renderer,
                    theme,
                    style,
                    layout,
                    cursor * inverse,
                    &(*viewport * inverse),
                );
            });
        });
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut core_widget::Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        offset: Vector,
    ) -> Option<core_overlay::Element<'a, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, offset)
    }
}

impl<'a, Message: 'a> From<ScaleBox<'a, Message>> for Element<'a, Message> {
    fn from(s: ScaleBox<'a, Message>) -> Self {
        Element::new(s)
    }
}

impl cosmic::Application for Launchpad {
    type Message = Message;
    type Executor = cosmic::executor::single::Executor;
    type Flags = bool;
    const APP_ID: &'static str = "dev.asw.MacLaunchpad";

    fn init(mut core: app::Core, show: bool) -> (Self, Task<Self::Message>) {
        core.set_app_type(cosmic::core::AppType::System);
        core.set_keyboard_nav(false);
        let mut this = Self {
            core,
            window_id: SurfaceId::unique(),
            visible: false,
            viewport: Size::new(1920.0, 1080.0),
            apps: Vec::new(),
            app_lookup: HashMap::new(),
            saved_layout: SavedLayout::default(),
            query: String::new(),
            matches: Vec::new(),
            page: 0,
            layout: GridLayout::default(),
            last_wheel: None,
            last_scroll_at: None,
            swipe_offset: 0.0,
            window_anim: None,
            page_anim: None,
            drag: DragController::new(),
            last_cursor: Point::ORIGIN,
            tile_targets: HashMap::new(),
            tile_slides: HashMap::new(),
            open_folder: None,
            context_menu: None,
            save_dirty: false,
            save_gen: 0,
        };
        // Preload so the first show is instant; refreshed again on every show.
        let task = if show {
            this.show()
        } else {
            Self::load_task()
        };
        (this, task)
    }

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        let now = Instant::now();
        match message {
            Message::Command(Command::Toggle) => {
                return if self.visible && !matches!(self.window_anim, Some(WindowAnim::Closing { .. }))
                {
                    self.begin_hide()
                } else {
                    self.show()
                };
            }
            Message::Command(Command::Show) => return self.show(),
            Message::Command(Command::Hide) => return self.begin_hide(),
            Message::Loaded(apps) => {
                self.apps = apps;
                self.app_lookup = layout::build_app_index(&self.apps);
                self.rebuild_layout_from_disk();
                self.refilter();
            }
            Message::Query(q) => {
                if self.drag.is_active() {
                    self.drag.cancel();
                }
                self.context_menu = None;
                self.query = q;
                self.page = 0;
                self.swipe_offset = 0.0;
                self.page_anim = None;
                self.refilter();
            }
            Message::Submit => {
                if let Some(&i) = self.matches.first() {
                    return self.launch(i);
                }
            }
            Message::Launch(i) => return self.launch(i),
            Message::CellPress(source) => {
                if self.context_menu.take().is_some() {
                    return Task::none();
                }
                let cursor = Point2::new(self.last_cursor.x, self.last_cursor.y);
                let (grab_offset, is_app) = match source {
                    DragSource::MainGrid { page, index } => {
                        let geom = self.layout.main_geometry(self.viewport);
                        let rect = geom.slot_rect(index);
                        let go = Point2::new(
                            (cursor.x - rect.x).clamp(0.0, rect.w),
                            (cursor.y - rect.y).clamp(0.0, rect.h),
                        );
                        let is_app = matches!(
                            self.saved_layout.pages.get(page).and_then(|p| p.get(index)),
                            Some(LayoutItem::App(_))
                        );
                        (go, is_app)
                    }
                    DragSource::Folder {
                        folder_app_page,
                        app_index,
                        ..
                    } => {
                        let fg = self.folder_geometry();
                        let local = app_index.saturating_sub(folder_app_page * FOLDER_PAGE_SIZE);
                        let rect = fg.grid.slot_rect(local);
                        let go = Point2::new(
                            (cursor.x - rect.x).clamp(0.0, rect.w),
                            (cursor.y - rect.y).clamp(0.0, rect.h),
                        );
                        (go, true)
                    }
                };
                self.drag
                    .press(source, cursor, grab_offset, is_app, now, self.search_active());
            }
            Message::CursorMoved(pos) => {
                self.last_cursor = pos;
                if self.visible && self.drag.is_active() {
                    let ctx = self.drag_eval_context();
                    let exited_folder =
                        self.drag
                            .pointer_moved(Point2::new(pos.x, pos.y), now, &ctx);
                    if exited_folder {
                        self.commit_open_folder_title();
                        self.open_folder = None;
                    }
                    self.sync_tile_slides(now);
                }
            }
            Message::PointerReleased => {
                if !self.visible || !self.drag.is_active() {
                    return Task::none();
                }
                let ctx = self.drag_eval_context();
                match self.drag.release(&ctx) {
                    ReleaseOutcome::None => {}
                    ReleaseOutcome::Click(DragSource::MainGrid { page, index }) => {
                        match self
                            .saved_layout
                            .pages
                            .get(page)
                            .and_then(|p| p.get(index))
                            .cloned()
                        {
                            Some(LayoutItem::App(id)) => {
                                if let Some(app_idx) = self.lookup_app(&id) {
                                    return self.launch(app_idx);
                                }
                            }
                            Some(LayoutItem::Folder { name, .. }) => {
                                self.open_folder = Some(OpenFolder {
                                    page,
                                    index,
                                    folder_page: 0,
                                    title_draft: name,
                                });
                            }
                            None => {}
                        }
                    }
                    ReleaseOutcome::Click(DragSource::Folder {
                        folder_page,
                        folder_index,
                        app_index,
                        ..
                    }) => {
                        if let Some(LayoutItem::Folder { apps: f_apps, .. }) = self
                            .saved_layout
                            .pages
                            .get(folder_page)
                            .and_then(|p| p.get(folder_index))
                            && let Some(id) = f_apps.get(app_index)
                            && let Some(app_idx) = self.lookup_app(id)
                        {
                            return self.launch(app_idx);
                        }
                    }
                    ReleaseOutcome::DropMainReorder {
                        from_page,
                        from_index,
                        to_page,
                        to_index,
                    } => {
                        if (from_page, from_index) != (to_page, to_index)
                            && self.saved_layout.move_item(
                                from_page,
                                from_index,
                                to_page,
                                to_index,
                                self.layout.per_page(),
                            )
                        {
                            self.page = self.page.min(self.pages().saturating_sub(1));
                            self.sync_tile_slides(now);
                            return self.schedule_save();
                        }
                    }
                    ReleaseOutcome::DropMergeIntoFolder {
                        from_page,
                        from_index,
                        target_page,
                        target_index,
                    } => {
                        if self
                            .saved_layout
                            .merge_into_folder(
                                from_page,
                                from_index,
                                target_page,
                                target_index,
                                &self.apps,
                            )
                            .is_some()
                        {
                            self.page = self.page.min(self.pages().saturating_sub(1));
                            self.reset_tile_tracking();
                            return self.schedule_save();
                        }
                    }
                    ReleaseOutcome::DropFolderReorder {
                        folder_page,
                        folder_index,
                        from_app_idx,
                        to_app_idx,
                    } => {
                        if from_app_idx != to_app_idx
                            && self.saved_layout.reorder_in_folder(
                                folder_page,
                                folder_index,
                                from_app_idx,
                                to_app_idx,
                            )
                        {
                            return self.schedule_save();
                        }
                    }
                    ReleaseOutcome::DropOutOfFolder {
                        folder_page,
                        folder_index,
                        app_idx,
                        to_page,
                        to_index,
                    } => {
                        if self.saved_layout.move_out_of_folder(
                            folder_page,
                            folder_index,
                            app_idx,
                            to_page,
                            to_index,
                            self.layout.per_page(),
                        ) {
                            self.page = self.page.min(self.pages().saturating_sub(1));
                            self.reset_tile_tracking();
                            return self.schedule_save();
                        }
                    }
                }
            }
            Message::BackgroundPress => {
                if self.context_menu.take().is_some() {
                    return Task::none();
                }
                if self.open_folder.is_some() {
                    return self.update(Message::CloseFolder);
                }
                return self.begin_hide();
            }
            Message::BackgroundRightClick => {
                if self.visible
                    && !self.drag.is_active()
                    && self.open_folder.is_none()
                    && !self.is_pointer_over_interactive(self.last_cursor)
                {
                    self.context_menu = Some(self.last_cursor);
                }
            }
            Message::ResetLayout => {
                self.context_menu = None;
                self.open_folder = None;
                self.saved_layout = layout::default_layout(&self.apps, self.layout.per_page());
                self.page = 0;
                self.reset_tile_tracking();
                return self.schedule_save();
            }
            Message::SortAZ => {
                self.context_menu = None;
                self.open_folder = None;
                self.saved_layout =
                    layout::sort_az(&self.saved_layout, &self.apps, self.layout.per_page());
                self.page = 0;
                self.reset_tile_tracking();
                return self.schedule_save();
            }
            Message::FolderTitleChanged(title) => {
                if let Some(open) = &mut self.open_folder {
                    open.title_draft = title.clone();
                    self.saved_layout
                        .set_folder_name(open.page, open.index, title, false);
                    return self.schedule_save();
                }
            }
            Message::FolderTitleSubmit => {
                self.commit_open_folder_title();
                return self.schedule_save();
            }
            Message::FolderPage(d) => {
                if let Some(open) = &mut self.open_folder
                    && let Some(LayoutItem::Folder { apps: f_apps, .. }) = self
                        .saved_layout
                        .pages
                        .get(open.page)
                        .and_then(|p| p.get(open.index))
                {
                    let total = page_count(f_apps.len(), FOLDER_PAGE_SIZE);
                    open.folder_page = step_page(open.folder_page, d, total);
                }
            }
            Message::GoFolderPage(p) => {
                if let Some(open) = &mut self.open_folder
                    && let Some(LayoutItem::Folder { apps: f_apps, .. }) = self
                        .saved_layout
                        .pages
                        .get(open.page)
                        .and_then(|p| p.get(open.index))
                {
                    let total = page_count(f_apps.len(), FOLDER_PAGE_SIZE);
                    open.folder_page = p.min(total.saturating_sub(1));
                }
            }
            Message::CloseFolder => {
                self.commit_open_folder_title();
                self.open_folder = None;
                return Task::batch([
                    self.schedule_save(),
                    text_input::focus(INPUT_ID.clone()),
                ]);
            }
            Message::Page(d) => {
                let target = step_page(self.page, d, self.pages());
                self.go_to_page(target, now);
            }
            Message::GoPage(p) => {
                self.go_to_page(p, now);
            }
            Message::Scrolled(delta) => {
                if !self.visible {
                    return Task::none();
                }
                if self.open_folder.is_some() {
                    let (x, y) = match delta {
                        ScrollDelta::Lines { x, y } | ScrollDelta::Pixels { x, y } => (x, y),
                    };
                    let d = if y.abs() >= x.abs() { y } else { x };
                    let ready = self.last_wheel.is_none_or(|t| t.elapsed() >= WHEEL_COOLDOWN);
                    if ready && d != 0.0 {
                        self.last_wheel = Some(now);
                        return self.update(Message::FolderPage(if d < 0.0 { 1 } else { -1 }));
                    }
                    return Task::none();
                }

                match delta {
                    ScrollDelta::Lines { x, y } => {
                        let d = if y.abs() >= x.abs() { y } else { x };
                        let ready = self.last_wheel.is_none_or(|t| t.elapsed() >= WHEEL_COOLDOWN);
                        if ready && d != 0.0 {
                            self.last_wheel = Some(now);
                            let target = step_page(self.page, if d < 0.0 { 1 } else { -1 }, self.pages());
                            self.go_to_page(target, now);
                        }
                    }
                    ScrollDelta::Pixels { x, y } => {
                        let d = if x.abs() >= y.abs() { x } else { y };
                        if d == 0.0 {
                            return Task::none();
                        }
                        self.last_scroll_at = Some(now);
                        let total = self.pages();
                        let at_start = self.page == 0 && (self.swipe_offset + d) > 0.0;
                        let at_end = self.page + 1 >= total && (self.swipe_offset + d) < 0.0;
                        let factor = if at_start || at_end { 0.25 } else { 1.0 };
                        self.swipe_offset += d * factor;

                        let threshold = (self.layout.grid_width() * 0.22).clamp(100.0, 220.0);
                        let ready = self.last_wheel.is_none_or(|t| t.elapsed() >= WHEEL_COOLDOWN);
                        if ready && self.swipe_offset <= -threshold && self.page + 1 < total {
                            self.last_wheel = Some(now);
                            self.go_to_page(self.page + 1, now);
                        } else if ready && self.swipe_offset >= threshold && self.page > 0 {
                            self.last_wheel = Some(now);
                            self.go_to_page(self.page - 1, now);
                        }
                    }
                }
            }
            Message::Resized(id, size) => {
                if id == self.window_id {
                    self.viewport = size;
                    self.layout = compute_layout(size.width, size.height);
                    self.page = self.page.min(self.pages().saturating_sub(1));
                    self.reset_tile_tracking();
                }
            }
            Message::Key(key, mods, _status) => {
                if !self.visible {
                    return Task::none();
                }
                match key {
                    Key::Named(Named::Escape) => {
                        if self.drag.is_active() {
                            self.drag.cancel();
                            self.sync_tile_slides(now);
                            return Task::none();
                        }
                        if self.context_menu.take().is_some() {
                            return Task::none();
                        }
                        if self.open_folder.is_some() {
                            return self.update(Message::CloseFolder);
                        }
                        if !self.query.is_empty() {
                            self.query.clear();
                            self.page = 0;
                            self.refilter();
                            return Task::none();
                        }
                        return self.begin_hide();
                    }
                    Key::Named(Named::PageDown) => {
                        if self.open_folder.is_some() {
                            return self.update(Message::FolderPage(1));
                        }
                        return self.update(Message::Page(1));
                    }
                    Key::Named(Named::PageUp) => {
                        if self.open_folder.is_some() {
                            return self.update(Message::FolderPage(-1));
                        }
                        return self.update(Message::Page(-1));
                    }
                    Key::Named(Named::ArrowRight)
                        if self.open_folder.is_none() && (self.query.is_empty() || mods.control()) =>
                    {
                        return self.update(Message::Page(1));
                    }
                    Key::Named(Named::ArrowLeft)
                        if self.open_folder.is_none() && (self.query.is_empty() || mods.control()) =>
                    {
                        return self.update(Message::Page(-1));
                    }
                    _ => {}
                }
            }
            Message::Unfocused(id) => {
                if id == self.window_id {
                    return self.begin_hide();
                }
            }
            Message::Tick(tick_now) => {
                if let Some(anim) = self.window_anim {
                    let done = match anim {
                        WindowAnim::Opening { started_at, .. } => {
                            tick_now.saturating_duration_since(started_at) >= OPEN_DURATION
                        }
                        WindowAnim::Closing { started_at, .. } => {
                            if tick_now.saturating_duration_since(started_at) >= CLOSE_DURATION {
                                return self.finish_hide();
                            }
                            false
                        }
                    };
                    if done {
                        self.window_anim = None;
                    }
                }

                if let Some(anim) = self.page_anim
                    && tick_now.saturating_duration_since(anim.started_at) >= anim.duration
                {
                    self.page_anim = None;
                }

                if self.swipe_offset != 0.0
                    && self
                        .last_scroll_at
                        .is_some_and(|t| tick_now.saturating_duration_since(t) >= SWIPE_SETTLE_DELAY)
                {
                    let offset = self.swipe_offset;
                    self.swipe_offset = 0.0;
                    self.page_anim = Some(PageSlideAnim {
                        from_offset: offset,
                        started_at: tick_now,
                        duration: Duration::from_millis(200),
                        outgoing_page: None,
                        outgoing_delta: 0.0,
                    });
                }

                if self.drag.is_active() {
                    let ctx = self.drag_eval_context();
                    let tick_out: DragTickOutcome = self.drag.tick(tick_now, &ctx);
                    if tick_out.started_drag {
                        self.sync_tile_slides(tick_now);
                    }
                    if let Some(delta) = tick_out.flip_page {
                        let target = step_page(self.page, delta, self.pages());
                        self.go_to_page(target, tick_now);
                    }
                }

                self.tile_slides.retain(|_, s| s.current(tick_now).2);
            }
            Message::FlushSave(gen_id) => {
                if gen_id == self.save_gen {
                    self.flush_save_now();
                }
            }
            Message::Noop => {}
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        unreachable!("layer-shell overlay uses view_window")
    }

    fn view_window(&self, id: SurfaceId) -> Element<'_, Self::Message> {
        if id != self.window_id || !self.visible {
            return vertical_space().height(Length::Fixed(1.0)).into();
        }

        let now = Instant::now();
        let progress = self.window_progress(now);
        let bg_alpha = 0.55 * progress;
        let grid_scale = 1.0 + 0.08 * (1.0 - progress);

        let search = text_input::search_input("Search", &self.query)
            .id(INPUT_ID.clone())
            .on_input(Message::Query)
            .on_paste(Message::Query)
            .on_submit(|_| Message::Submit)
            .width(Length::Fixed(self.layout.search_w))
            .size(16.0)
            .padding([8, 12])
            .always_active();

        let top_area_h = f32::from(self.layout.top_pad) + SEARCH_BAR_H;
        let bottom_area_h = DOTS_H + f32::from(self.layout.bottom_pad);

        let body = column![
            container(search)
                .height(Length::Fixed(top_area_h))
                .center_x(Length::Fill)
                .padding([self.layout.top_pad, 0, 0, 0]),
            container(ScaleBox::new(self.grid(now), grid_scale))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill),
            container(self.dots())
                .height(Length::Fixed(bottom_area_h))
                .center_x(Length::Fill)
                .padding([0, 0, self.layout.bottom_pad, 0]),
        ]
        .width(Length::Fill)
        .height(Length::Fill);

        let base: Element<'_, Message> = mouse_area(
            container(body)
                .width(Length::Fill)
                .height(Length::Fill)
                .class(Container::custom(move |theme| {
                    let t = theme.cosmic();
                    container::Style {
                        text_color: Some(t.on_bg_color().into()),
                        icon_color: Some(t.on_bg_color().into()),
                        background: Some(Color::from(t.bg_color()).scale_alpha(bg_alpha).into()),
                        ..Default::default()
                    }
                })),
        )
        .on_press(Message::BackgroundPress)
        .on_right_press(Message::BackgroundRightClick)
        .into();

        let mut layers: Vec<Element<'_, Message>> = vec![base];

        if let Some(open) = &self.open_folder
            && let Some(overlay) = self.folder_overlay(open)
        {
            layers.push(overlay);
        }

        if let Some(pos) = self.context_menu {
            layers.push(self.context_menu_overlay(pos));
        }

        if let Some(drag_el) = self.dragged_overlay() {
            layers.push(drag_el);
        }

        stack(layers)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let mut subs = vec![
            listen_raw(|event, status, id| match event {
                cosmic::iced::Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::Layer(wayland::LayerEvent::Unfocused, _, layer_id),
                )) => Some(Message::Unfocused(layer_id)),
                cosmic::iced::Event::Window(window::Event::Resized(size)) => {
                    Some(Message::Resized(id, size))
                }
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                    key,
                    modifiers,
                    ..
                }) => Some(Message::Key(key, modifiers, status)),
                cosmic::iced::Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    Some(Message::CursorMoved(position))
                }
                cosmic::iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    Some(Message::PointerReleased)
                }
                cosmic::iced::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                    Some(Message::Scrolled(delta))
                }
                _ => None,
            }),
            Subscription::run(socket_stream),
        ];

        if self.is_animating() {
            subs.push(time::every(Duration::from_millis(16)).map(Message::Tick));
        }

        Subscription::batch(subs)
    }
}

fn socket_stream() -> impl cosmic::iced::futures::Stream<Item = Message> {
    stream::channel(
        8,
        |mut tx: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            use tokio::io::{AsyncBufReadExt, BufReader};

            let Some(path) = ipc::socket_path() else {
                tracing::error!("no private runtime directory; Launchpad cannot be toggled");
                return;
            };
            let listener = match tokio::net::UnixListener::bind(&path) {
                Ok(l) => l,
                Err(first) => {
                    // A live peer owns the socket: we lost a start-up race, so
                    // quit instead of lingering as a deaf second daemon.
                    if tokio::net::UnixStream::connect(&path).await.is_ok() {
                        std::process::exit(0);
                    }
                    // Stale socket file from a crashed instance: replace it.
                    let _ = std::fs::remove_file(&path);
                    match tokio::net::UnixListener::bind(&path) {
                        Ok(l) => l,
                        Err(e) => {
                            tracing::error!("cannot bind {}: {first}; {e}", path.display());
                            std::process::exit(1);
                        }
                    }
                }
            };
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let mut line = String::new();
                if BufReader::new(stream).read_line(&mut line).await.is_ok()
                    && let Some(cmd) = Command::parse(&line)
                {
                    let _ = tx.try_send(Message::Command(cmd));
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_count_rounds_up_and_is_at_least_one() {
        let per_page = COLUMNS * ROWS;
        assert_eq!(page_count(0, per_page), 1);
        assert_eq!(page_count(per_page, per_page), 1);
        assert_eq!(page_count(per_page + 1, per_page), 2);
        assert_eq!(page_count(10, 0), 10);
    }

    #[test]
    fn step_page_clamps_and_avoids_underflow() {
        assert_eq!(step_page(0, -1, 0), 0);
        assert_eq!(step_page(5, 1, 0), 0);
        assert_eq!(step_page(0, -1, 3), 0);
        assert_eq!(step_page(1, 1, 3), 2);
        assert_eq!(step_page(2, 1, 3), 2);
    }

    #[test]
    fn page_slice_handles_empty_and_out_of_bounds_pages() {
        assert_eq!(page_slice(&[], 0, 35), &[] as &[usize]);
        assert_eq!(page_slice(&[], 4, 0), &[] as &[usize]);
        let items: Vec<usize> = (0..10).collect();
        assert_eq!(page_slice(&items, 0, 4), &[0, 1, 2, 3]);
        assert_eq!(page_slice(&items, 2, 4), &[8, 9]);
        // Out-of-bounds page clamps to the last valid page rather than panicking.
        assert_eq!(page_slice(&items, 99, 4), &[8, 9]);
        assert_eq!(page_slice(&items, usize::MAX, 4), &[8, 9]);
    }

    #[test]
    fn compute_layout_adapts_to_viewports_and_fractional_scales() {
        let fhd = compute_layout(1920.0, 1080.0);
        assert_eq!((fhd.columns, fhd.rows, fhd.icon_size), (7, 5, 96));

        // 1920x1080 at 150% fractional scale -> 1280x720 logical.
        let hidpi_150 = compute_layout(1280.0, 720.0);
        assert!(hidpi_150.columns >= 5 && hidpi_150.rows >= 4);
        assert!(hidpi_150.cell_w * hidpi_150.columns as f32 <= 1280.0);
        assert!(hidpi_150.cell_h * hidpi_150.rows as f32 <= 720.0);

        // 1920x1080 at 200% scale -> 960x540 logical.
        let hidpi_200 = compute_layout(960.0, 540.0);
        assert!(hidpi_200.columns >= 1 && hidpi_200.rows >= 1);
        assert!(hidpi_200.cell_w * hidpi_200.columns as f32 <= 960.0);

        // Tiny and invalid viewports remain safe and non-zero.
        for (w, h) in [
            (240.0, 180.0),
            (80.0, 60.0),
            (0.0, 0.0),
            (-100.0, 500.0),
            (f32::NAN, 720.0),
        ] {
            let l = compute_layout(w, h);
            assert!(l.columns >= 1);
            assert!(l.rows >= 1);
            assert!(l.per_page() >= 1);
            assert!(l.icon_size >= 16);
            assert!(l.cell_w > 0.0 && l.cell_h > 0.0);
        }
    }

    #[test]
    fn folder_geometry_fits_inside_viewport() {
        for (w, h) in [(1920.0, 1080.0), (1280.0, 720.0), (800.0, 600.0)] {
            let l = compute_layout(w, h);
            let fg = compute_folder_geometry(l, Size::new(w, h));
            assert!(fg.panel.w <= w);
            assert!(fg.panel.h <= h);
            assert!(fg.grid.origin_x >= fg.panel.x);
            assert!(fg.grid.origin_y >= fg.panel.y);
            assert!(fg.grid.total_width() <= fg.panel.w);
            assert!(fg.grid.total_height() <= fg.panel.h);
        }
    }
}
