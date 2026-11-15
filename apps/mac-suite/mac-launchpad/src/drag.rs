// SPDX-License-Identifier: GPL-3.0-only

//! Pointer drag-and-drop state machine and grid hit-test / hover-zone math.

use std::time::{Duration, Instant};

pub const HOLD_TO_DRAG: Duration = Duration::from_millis(300);
pub const MOVE_TO_DRAG_PX: f32 = 6.0;
pub const FOLDER_MERGE_HOLD: Duration = Duration::from_millis(500);
pub const EDGE_FLIP_HOLD: Duration = Duration::from_millis(700);
pub const DRAG_SCALE: f32 = 1.1;
pub const NEIGHBOR_SLIDE_DURATION: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point2 {
    pub x: f32,
    pub y: f32,
}

impl Point2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn distance(self, other: Self) -> f32 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect2 {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect2 {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(self, p: Point2) -> bool {
        p.x >= self.x && p.x <= self.x + self.w && p.y >= self.y && p.y <= self.y + self.h
    }

    pub fn center(self) -> Point2 {
        Point2::new(self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
}

/// Deterministic pixel geometry of a rendered icon grid (either the main
/// Launchpad page grid or an open folder's grid).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridGeometry {
    pub origin_x: f32,
    pub origin_y: f32,
    pub columns: usize,
    pub rows: usize,
    pub cell_w: f32,
    pub cell_h: f32,
    pub spacing: f32,
}

impl GridGeometry {
    pub fn total_width(self) -> f32 {
        let cols = self.columns.max(1) as f32;
        cols * self.cell_w + (cols - 1.0) * self.spacing
    }

    pub fn total_height(self) -> f32 {
        let rows = self.rows.max(1) as f32;
        rows * self.cell_h + (rows - 1.0) * self.spacing
    }

    pub fn slot_rect(self, slot: usize) -> Rect2 {
        let cols = self.columns.max(1);
        let col = slot % cols;
        let row = slot / cols;
        Rect2::new(
            self.origin_x + col as f32 * (self.cell_w + self.spacing),
            self.origin_y + row as f32 * (self.cell_h + self.spacing),
            self.cell_w,
            self.cell_h,
        )
    }

    #[cfg(test)]
    pub fn slot_center(self, slot: usize) -> Point2 {
        self.slot_rect(slot).center()
    }

    /// Returns which item slot `0..item_count` contains `pointer`, if any.
    pub fn hit_test_cell(self, pointer: Point2, item_count: usize) -> Option<usize> {
        (0..item_count).find(|&i| self.slot_rect(i).contains(pointer))
    }

    /// Computes whether `pointer` is in the center folder-merge zone of an
    /// existing item or in a reorder insertion slot.
    pub fn hover_zone(
        self,
        pointer: Point2,
        item_count: usize,
        dragged_idx_on_page: Option<usize>,
        allow_folder_merge: bool,
    ) -> HoverZone {
        if item_count == 0 {
            return HoverZone::Reorder { drop_index: 0 };
        }

        if allow_folder_merge {
            for i in 0..item_count {
                if Some(i) == dragged_idx_on_page {
                    continue;
                }
                let rect = self.slot_rect(i);
                let center = rect.center();
                let merge_rx = rect.w * 0.28;
                let merge_ry = rect.h * 0.28;
                if (pointer.x - center.x).abs() <= merge_rx
                    && (pointer.y - center.y).abs() <= merge_ry
                {
                    return HoverZone::FolderMerge { target_index: i };
                }
            }
        }

        let drop_index = self.drop_index(pointer, item_count, dragged_idx_on_page.is_some());
        HoverZone::Reorder { drop_index }
    }

    /// Computes the target insertion index for `pointer` in a grid containing
    /// `item_count` items.
    ///
    /// When `same_page_drag` is true, returns `0..item_count` (the visual slot
    /// index the dragged item should occupy on this page). When `same_page_drag`
    /// is false (inserting from another page or out of a folder), returns
    /// `0..=item_count`.
    pub fn drop_index(self, pointer: Point2, item_count: usize, same_page_drag: bool) -> usize {
        if item_count == 0 {
            return 0;
        }
        let cols = self.columns.max(1);
        let rows = self.rows.max(1);
        let step_x = (self.cell_w + self.spacing).max(1.0);
        let step_y = (self.cell_h + self.spacing).max(1.0);

        let rel_y = pointer.y - self.origin_y + self.spacing * 0.5;
        let row = (rel_y / step_y).floor().clamp(0.0, (rows - 1) as f32) as usize;

        let rel_x = pointer.x - self.origin_x + self.spacing * 0.5;
        let max_col = if same_page_drag { cols - 1 } else { cols };
        let col = (rel_x / step_x).floor().clamp(0.0, max_col as f32) as usize;

        let raw_slot = row.saturating_mul(cols).saturating_add(col);
        if same_page_drag {
            raw_slot.min(item_count - 1)
        } else {
            // If pointing past the midpoint of the last cell, append at item_count.
            let last_idx = item_count - 1;
            let last_rect = self.slot_rect(last_idx);
            if raw_slot > last_idx
                || (raw_slot == last_idx && pointer.x > last_rect.x + last_rect.w * 0.5)
            {
                item_count
            } else {
                raw_slot.min(item_count)
            }
        }
    }
}

/// Computes the visual slot index for existing item `item_idx` when a drag is
/// previewing `drop_idx` on the page. Returns `None` for the lifted item
/// itself (rendered in the floating overlay).
pub fn preview_visual_slot(
    item_idx: usize,
    dragged_idx_on_page: Option<usize>,
    drop_idx: usize,
) -> Option<usize> {
    match dragged_idx_on_page {
        Some(from) if item_idx == from => None,
        Some(from) => {
            if from < drop_idx && item_idx > from && item_idx <= drop_idx {
                Some(item_idx - 1)
            } else if drop_idx < from && item_idx >= drop_idx && item_idx < from {
                Some(item_idx + 1)
            } else {
                Some(item_idx)
            }
        }
        None => {
            if item_idx >= drop_idx {
                Some(item_idx + 1)
            } else {
                Some(item_idx)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeSide {
    Left,
    Right,
}

/// Determines whether `pointer_x` is within the left/right page-flip edge zone.
pub fn edge_flip_zone(
    pointer_x: f32,
    viewport_w: f32,
    current_page: usize,
    total_pages: usize,
) -> Option<EdgeSide> {
    if viewport_w <= 0.0 || total_pages <= 1 {
        return None;
    }
    let margin = (viewport_w * 0.06).clamp(36.0, 80.0);
    if pointer_x <= margin && current_page > 0 {
        Some(EdgeSide::Left)
    } else if pointer_x >= viewport_w - margin && current_page + 1 < total_pages {
        Some(EdgeSide::Right)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoverZone {
    Reorder { drop_index: usize },
    FolderMerge { target_index: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragSource {
    MainGrid { page: usize, index: usize },
    Folder {
        folder_page: usize,
        folder_index: usize,
        folder_app_page: usize,
        app_index: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderHover {
    pub page: usize,
    pub target_index: usize,
    pub since: Instant,
    pub armed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeHover {
    pub side: EdgeSide,
    pub since: Instant,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DragPhase {
    Idle,
    Pending {
        source: DragSource,
        press_pos: Point2,
        current_pos: Point2,
        grab_offset: Point2,
        pressed_at: Instant,
        is_app: bool,
    },
    Dragging {
        source: DragSource,
        pointer: Point2,
        grab_offset: Point2,
        is_app: bool,
        in_folder: bool,
        drop_index: usize,
        folder_hover: Option<FolderHover>,
        edge_hover: Option<EdgeHover>,
    },
}

/// Context passed into drag evaluation methods so `drag.rs` stays pure and
/// testable without UI coupling.
#[derive(Debug, Clone, Copy)]
pub struct DragEvalContext {
    pub viewport_w: f32,
    pub current_page: usize,
    pub total_pages: usize,
    pub page_item_count: usize,
    pub main_grid: GridGeometry,
    /// Present only while a folder overlay is open.
    pub folder_panel_bounds: Option<Rect2>,
    pub folder_grid: Option<GridGeometry>,
    pub folder_page_offset: usize,
    pub folder_page_item_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragTickOutcome {
    pub started_drag: bool,
    pub flip_page: Option<isize>,
    pub armed_changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseOutcome {
    None,
    Click(DragSource),
    DropMainReorder {
        from_page: usize,
        from_index: usize,
        to_page: usize,
        to_index: usize,
    },
    DropMergeIntoFolder {
        from_page: usize,
        from_index: usize,
        target_page: usize,
        target_index: usize,
    },
    DropFolderReorder {
        folder_page: usize,
        folder_index: usize,
        from_app_idx: usize,
        to_app_idx: usize,
    },
    DropOutOfFolder {
        folder_page: usize,
        folder_index: usize,
        app_idx: usize,
        to_page: usize,
        to_index: usize,
    },
}

pub struct DragController {
    phase: DragPhase,
}

impl Default for DragController {
    fn default() -> Self {
        Self::new()
    }
}

impl DragController {
    pub fn new() -> Self {
        Self {
            phase: DragPhase::Idle,
        }
    }

    pub fn phase(&self) -> &DragPhase {
        &self.phase
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.phase, DragPhase::Idle)
    }

    pub fn is_dragging(&self) -> bool {
        matches!(self.phase, DragPhase::Dragging { .. })
    }

    pub fn needs_ticks(&self) -> bool {
        self.is_active()
    }

    pub fn cancel(&mut self) {
        self.phase = DragPhase::Idle;
    }

    /// Starts a pending press if search is inactive.
    pub fn press(
        &mut self,
        source: DragSource,
        pos: Point2,
        grab_offset: Point2,
        is_app: bool,
        now: Instant,
        search_active: bool,
    ) {
        if search_active {
            self.phase = DragPhase::Idle;
            return;
        }
        self.phase = DragPhase::Pending {
            source,
            press_pos: pos,
            current_pos: pos,
            grab_offset,
            pressed_at: now,
            is_app,
        };
    }

    /// Updates pointer position; promotes Pending -> Dragging on >= 6px move,
    /// or updates hover zones while Dragging. Returns `true` if a drag inside
    /// an open folder just exited the folder panel onto the main grid.
    pub fn pointer_moved(&mut self, pos: Point2, now: Instant, ctx: &DragEvalContext) -> bool {
        match &mut self.phase {
            DragPhase::Idle => false,
            DragPhase::Pending {
                source,
                press_pos,
                current_pos,
                grab_offset,
                is_app,
                ..
            } => {
                *current_pos = pos;
                if pos.distance(*press_pos) >= MOVE_TO_DRAG_PX {
                    let s = *source;
                    let go = *grab_offset;
                    let app = *is_app;
                    self.start_dragging(s, pos, go, app, now, ctx);
                }
                false
            }
            DragPhase::Dragging {
                pointer, in_folder, ..
            } => {
                *pointer = pos;
                let exited_folder = *in_folder
                    && ctx
                        .folder_panel_bounds
                        .is_some_and(|panel| !panel.contains(pos));
                if exited_folder {
                    *in_folder = false;
                }
                self.refresh_hover(now, ctx);
                exited_folder
            }
        }
    }

    /// Advances time-based transitions (300ms hold-to-drag, 500ms folder-merge
    /// arming, 700ms edge page flip).
    pub fn tick(&mut self, now: Instant, ctx: &DragEvalContext) -> DragTickOutcome {
        let mut outcome = DragTickOutcome {
            started_drag: false,
            flip_page: None,
            armed_changed: false,
        };

        match &mut self.phase {
            DragPhase::Idle => {}
            DragPhase::Pending {
                source,
                current_pos,
                grab_offset,
                pressed_at,
                is_app,
                ..
            } => {
                if now.duration_since(*pressed_at) >= HOLD_TO_DRAG {
                    let s = *source;
                    let pos = *current_pos;
                    let go = *grab_offset;
                    let app = *is_app;
                    self.start_dragging(s, pos, go, app, now, ctx);
                    outcome.started_drag = true;
                }
            }
            DragPhase::Dragging {
                folder_hover,
                edge_hover,
                in_folder,
                ..
            } => {
                if let Some(fh) = folder_hover
                    && !fh.armed
                    && now.duration_since(fh.since) >= FOLDER_MERGE_HOLD
                {
                    fh.armed = true;
                    outcome.armed_changed = true;
                }
                if !*in_folder
                    && let Some(eh) = edge_hover
                    && now.duration_since(eh.since) >= EDGE_FLIP_HOLD
                {
                    outcome.flip_page = Some(match eh.side {
                        EdgeSide::Left => -1,
                        EdgeSide::Right => 1,
                    });
                    eh.since = now;
                }
            }
        }

        outcome
    }

    /// Refreshes hover zones after a page flip during drag.
    pub fn on_page_changed(&mut self, now: Instant, ctx: &DragEvalContext) {
        if matches!(self.phase, DragPhase::Dragging { .. }) {
            self.refresh_hover(now, ctx);
        }
    }

    /// Completes a press/drag and returns the resulting action.
    pub fn release(&mut self, ctx: &DragEvalContext) -> ReleaseOutcome {
        let phase = std::mem::replace(&mut self.phase, DragPhase::Idle);
        match phase {
            DragPhase::Idle => ReleaseOutcome::None,
            DragPhase::Pending { source, .. } => ReleaseOutcome::Click(source),
            DragPhase::Dragging {
                source,
                in_folder,
                drop_index,
                folder_hover,
                ..
            } => match source {
                DragSource::MainGrid { page, index } => {
                    if let Some(fh) = folder_hover
                        && fh.armed
                        && (fh.page, fh.target_index) != (page, index)
                    {
                        return ReleaseOutcome::DropMergeIntoFolder {
                            from_page: page,
                            from_index: index,
                            target_page: fh.page,
                            target_index: fh.target_index,
                        };
                    }
                    ReleaseOutcome::DropMainReorder {
                        from_page: page,
                        from_index: index,
                        to_page: ctx.current_page,
                        to_index: drop_index,
                    }
                }
                DragSource::Folder {
                    folder_page,
                    folder_index,
                    folder_app_page,
                    app_index,
                } => {
                    if in_folder {
                        let to_app_idx = folder_app_page
                            .saturating_mul(crate::layout::FOLDER_PAGE_SIZE)
                            .saturating_add(drop_index);
                        ReleaseOutcome::DropFolderReorder {
                            folder_page,
                            folder_index,
                            from_app_idx: app_index,
                            to_app_idx,
                        }
                    } else {
                        ReleaseOutcome::DropOutOfFolder {
                            folder_page,
                            folder_index,
                            app_idx: app_index,
                            to_page: ctx.current_page,
                            to_index: drop_index,
                        }
                    }
                }
            },
        }
    }

    fn start_dragging(
        &mut self,
        source: DragSource,
        pos: Point2,
        grab_offset: Point2,
        is_app: bool,
        now: Instant,
        ctx: &DragEvalContext,
    ) {
        let in_folder = matches!(source, DragSource::Folder { .. })
            && ctx
                .folder_panel_bounds
                .is_none_or(|panel| panel.contains(pos));

        self.phase = DragPhase::Dragging {
            source,
            pointer: pos,
            grab_offset,
            is_app,
            in_folder,
            drop_index: 0,
            folder_hover: None,
            edge_hover: None,
        };
        self.refresh_hover(now, ctx);
    }

    fn refresh_hover(&mut self, now: Instant, ctx: &DragEvalContext) {
        let DragPhase::Dragging {
            source,
            pointer,
            is_app,
            in_folder,
            drop_index,
            folder_hover,
            edge_hover,
            ..
        } = &mut self.phase
        else {
            return;
        };

        if *in_folder && let Some(fgrid) = ctx.folder_grid {
            let local_from = match *source {
                DragSource::Folder { app_index, .. } => {
                    app_index.checked_sub(ctx.folder_page_offset)
                }
                DragSource::MainGrid { .. } => None,
            };
            let local_from = local_from.filter(|&i| i < ctx.folder_page_item_count);
            *drop_index = fgrid.drop_index(*pointer, ctx.folder_page_item_count, local_from.is_some());
            *folder_hover = None;
            *edge_hover = None;
            return;
        }

        let dragged_idx_on_page = match *source {
            DragSource::MainGrid { page, index } if page == ctx.current_page => Some(index),
            _ => None,
        };

        let zone = ctx.main_grid.hover_zone(
            *pointer,
            ctx.page_item_count,
            dragged_idx_on_page,
            *is_app,
        );

        match zone {
            HoverZone::FolderMerge { target_index } => {
                let same_target = folder_hover.is_some_and(|fh| {
                    fh.page == ctx.current_page && fh.target_index == target_index
                });
                if !same_target {
                    *folder_hover = Some(FolderHover {
                        page: ctx.current_page,
                        target_index,
                        since: now,
                        armed: false,
                    });
                }
                // Keep reorder drop_index aligned to target_index in case the user
                // releases before the 500ms folder-merge threshold.
                *drop_index = target_index.min(ctx.page_item_count);
            }
            HoverZone::Reorder { drop_index: idx } => {
                *folder_hover = None;
                *drop_index = idx;
            }
        }

        let new_edge = edge_flip_zone(
            pointer.x,
            ctx.viewport_w,
            ctx.current_page,
            ctx.total_pages,
        );
        match (new_edge, *edge_hover) {
            (Some(side), Some(prev)) if prev.side == side => {}
            (Some(side), _) => {
                *edge_hover = Some(EdgeHover { side, since: now });
            }
            (None, _) => {
                *edge_hover = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_grid() -> GridGeometry {
        GridGeometry {
            origin_x: 100.0,
            origin_y: 100.0,
            columns: 4,
            rows: 3,
            cell_w: 100.0,
            cell_h: 100.0,
            spacing: 10.0,
        }
    }

    fn sample_ctx(page: usize, total_pages: usize, item_count: usize) -> DragEvalContext {
        DragEvalContext {
            viewport_w: 1000.0,
            current_page: page,
            total_pages,
            page_item_count: item_count,
            main_grid: sample_grid(),
            folder_panel_bounds: None,
            folder_grid: None,
            folder_page_offset: 0,
            folder_page_item_count: 0,
        }
    }

    #[test]
    fn hover_zone_distinguishes_center_folder_merge_from_reorder() {
        let g = sample_grid();
        // Slot 1 rect: x=[210, 310], y=[100, 200], center=(260, 150)
        assert_eq!(
            g.hover_zone(Point2::new(260.0, 150.0), 6, Some(0), true),
            HoverZone::FolderMerge { target_index: 1 }
        );
        // Outer edge of slot 1 (outside inner 56% box) -> Reorder at 1
        assert_eq!(
            g.hover_zone(Point2::new(212.0, 105.0), 6, Some(0), true),
            HoverZone::Reorder { drop_index: 1 }
        );
        // Dragging a folder itself never merges into another folder
        assert_eq!(
            g.hover_zone(Point2::new(260.0, 150.0), 6, Some(0), false),
            HoverZone::Reorder { drop_index: 1 }
        );
        // Hovering own original slot does not trigger self-merge
        assert_eq!(
            g.hover_zone(Point2::new(150.0, 150.0), 6, Some(0), true),
            HoverZone::Reorder { drop_index: 0 }
        );
    }

    #[test]
    fn drop_index_handles_same_page_and_cross_page() {
        let g = sample_grid();
        // 6 items on 4x3 grid (row 0: 0..3, row 1: 4..5)
        // Pointer at slot 5 center -> 5 on same page
        let c5 = g.slot_center(5);
        assert_eq!(g.drop_index(c5, 6, true), 5);
        // Pointer far below/right clamps to 5 on same page, 6 on cross-page insert
        assert_eq!(g.drop_index(Point2::new(800.0, 600.0), 6, true), 5);
        assert_eq!(g.drop_index(Point2::new(800.0, 600.0), 6, false), 6);
        // Empty page always drops at 0
        assert_eq!(g.drop_index(Point2::new(300.0, 300.0), 0, false), 0);
    }

    #[test]
    fn preview_visual_slot_shifts_neighbors_around_gap() {
        // Dragging item 0 to slot 2: items 1 and 2 shift left; item 3 stays.
        assert_eq!(preview_visual_slot(0, Some(0), 2), None);
        assert_eq!(preview_visual_slot(1, Some(0), 2), Some(0));
        assert_eq!(preview_visual_slot(2, Some(0), 2), Some(1));
        assert_eq!(preview_visual_slot(3, Some(0), 2), Some(3));

        // Dragging item 3 to slot 1: items 1 and 2 shift right; item 0 stays.
        assert_eq!(preview_visual_slot(0, Some(3), 1), Some(0));
        assert_eq!(preview_visual_slot(1, Some(3), 1), Some(2));
        assert_eq!(preview_visual_slot(2, Some(3), 1), Some(3));
        assert_eq!(preview_visual_slot(3, Some(3), 1), None);
    }

    #[test]
    fn drag_starts_after_300ms_or_6px_and_disabled_during_search() {
        let t0 = Instant::now();
        let ctx = sample_ctx(0, 2, 6);
        let mut ctrl = DragController::new();

        // Active search disables dragging.
        ctrl.press(
            DragSource::MainGrid { page: 0, index: 0 },
            Point2::new(150.0, 150.0),
            Point2::default(),
            true,
            t0,
            true,
        );
        assert!(!ctrl.is_active());

        // Quick release before 300ms / 6px -> Click
        ctrl.press(
            DragSource::MainGrid { page: 0, index: 0 },
            Point2::new(150.0, 150.0),
            Point2::default(),
            true,
            t0,
            false,
        );
        assert!(!ctrl.is_dragging());
        let outcome = ctrl.tick(t0 + Duration::from_millis(299), &ctx);
        assert!(!outcome.started_drag);
        assert_eq!(
            ctrl.release(&ctx),
            ReleaseOutcome::Click(DragSource::MainGrid { page: 0, index: 0 })
        );

        // Hold 300ms -> starts drag
        ctrl.press(
            DragSource::MainGrid { page: 0, index: 0 },
            Point2::new(150.0, 150.0),
            Point2::default(),
            true,
            t0,
            false,
        );
        let outcome = ctrl.tick(t0 + HOLD_TO_DRAG, &ctx);
        assert!(outcome.started_drag);
        assert!(ctrl.is_dragging());

        // Escape cancels drag cleanly
        ctrl.cancel();
        assert!(!ctrl.is_active());

        // Move >= 6px -> starts drag immediately
        ctrl.press(
            DragSource::MainGrid { page: 0, index: 0 },
            Point2::new(150.0, 150.0),
            Point2::default(),
            true,
            t0,
            false,
        );
        ctrl.pointer_moved(Point2::new(156.5, 150.0), t0, &ctx);
        assert!(ctrl.is_dragging());
    }

    #[test]
    fn folder_merge_arms_at_500ms_and_edge_flips_at_700ms() {
        let t0 = Instant::now();
        let ctx = sample_ctx(0, 3, 6);
        let mut ctrl = DragController::new();
        ctrl.press(
            DragSource::MainGrid { page: 0, index: 0 },
            Point2::new(150.0, 150.0),
            Point2::default(),
            true,
            t0,
            false,
        );
        // Move over slot 1 center
        let c1 = sample_grid().slot_center(1);
        ctrl.pointer_moved(c1, t0, &ctx);
        assert!(ctrl.is_dragging());

        // Before 500ms: not armed yet -> release would reorder
        assert!(!ctrl.tick(t0 + Duration::from_millis(450), &ctx).armed_changed);
        // At 500ms: armed!
        assert!(ctrl.tick(t0 + FOLDER_MERGE_HOLD, &ctx).armed_changed);
        assert_eq!(
            ctrl.release(&ctx),
            ReleaseOutcome::DropMergeIntoFolder {
                from_page: 0,
                from_index: 0,
                target_page: 0,
                target_index: 1,
            }
        );

        // Edge flip test: hold near right edge on page 0 -> flips +1 after 700ms
        ctrl.press(
            DragSource::MainGrid { page: 0, index: 0 },
            Point2::new(150.0, 150.0),
            Point2::default(),
            true,
            t0,
            false,
        );
        ctrl.pointer_moved(Point2::new(985.0, 300.0), t0, &ctx);
        assert_eq!(
            ctrl.tick(t0 + Duration::from_millis(650), &ctx).flip_page,
            None
        );
        assert_eq!(
            ctrl.tick(t0 + EDGE_FLIP_HOLD, &ctx).flip_page,
            Some(1)
        );
    }

    #[test]
    fn dragging_out_of_open_folder_transitions_to_main_grid() {
        let t0 = Instant::now();
        let panel = Rect2::new(250.0, 150.0, 500.0, 400.0);
        let fgrid = GridGeometry {
            origin_x: 280.0,
            origin_y: 220.0,
            columns: 4,
            rows: 3,
            cell_w: 100.0,
            cell_h: 100.0,
            spacing: 10.0,
        };
        let mut ctx = sample_ctx(0, 1, 6);
        ctx.folder_panel_bounds = Some(panel);
        ctx.folder_grid = Some(fgrid);
        ctx.folder_page_item_count = 3;

        let mut ctrl = DragController::new();
        ctrl.press(
            DragSource::Folder {
                folder_page: 0,
                folder_index: 1,
                folder_app_page: 0,
                app_index: 0,
            },
            Point2::new(330.0, 270.0),
            Point2::default(),
            true,
            t0,
            false,
        );
        // Move inside folder -> stays in folder
        assert!(!ctrl.pointer_moved(Point2::new(450.0, 270.0), t0, &ctx));
        assert!(ctrl.is_dragging());

        // Move outside folder panel -> exits folder onto main grid at slot 0
        assert!(ctrl.pointer_moved(Point2::new(150.0, 150.0), t0, &ctx));
        ctx.folder_panel_bounds = None;
        ctx.folder_grid = None;
        assert_eq!(
            ctrl.release(&ctx),
            ReleaseOutcome::DropOutOfFolder {
                folder_page: 0,
                folder_index: 1,
                app_idx: 0,
                to_page: 0,
                to_index: 0,
            }
        );
    }
}
