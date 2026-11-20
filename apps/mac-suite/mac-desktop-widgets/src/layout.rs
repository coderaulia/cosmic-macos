// SPDX-License-Identifier: GPL-3.0-only
//! Persistent desktop widget layout (`~/.config/mac-suite/desktop-widgets.ron`)
//! and 20 px grid snapping / placement helpers.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Grid snap step in logical pixels.
pub const GRID_STEP: i32 = 20;

/// Default margin from screen edges when auto-placing widgets.
const EDGE_MARGIN: i32 = 40;
/// Top offset to clear the macOS-style top menu bar.
const TOP_MARGIN: i32 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WidgetKind {
    Weather,
    UpNext,
    System,
    Clock,
    CalendarMonth,
    Battery,
    Notes,
}

impl WidgetKind {
    pub const ALL: [WidgetKind; 7] = [
        WidgetKind::Weather,
        WidgetKind::UpNext,
        WidgetKind::System,
        WidgetKind::Clock,
        WidgetKind::CalendarMonth,
        WidgetKind::Battery,
        WidgetKind::Notes,
    ];

    pub fn label(self) -> &'static str {
        match self {
            WidgetKind::Weather => "Weather",
            WidgetKind::UpNext => "Up Next",
            WidgetKind::System => "System",
            WidgetKind::Clock => "Clock",
            WidgetKind::CalendarMonth => "Calendar",
            WidgetKind::Battery => "Batteries",
            WidgetKind::Notes => "Notes",
        }
    }

    pub fn icon_name(self) -> &'static str {
        match self {
            WidgetKind::Weather => "weather-few-clouds-symbolic",
            WidgetKind::UpNext => "x-office-calendar-symbolic",
            WidgetKind::System => "utilities-system-monitor-symbolic",
            WidgetKind::Clock => "preferences-system-time-symbolic",
            WidgetKind::CalendarMonth => "office-calendar-symbolic",
            WidgetKind::Battery => "battery-symbolic",
            WidgetKind::Notes => "accessories-text-editor-symbolic",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum WidgetSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl WidgetSize {
    pub const ALL: [WidgetSize; 3] = [WidgetSize::Small, WidgetSize::Medium, WidgetSize::Large];

    pub fn label(self) -> &'static str {
        match self {
            WidgetSize::Small => "Small",
            WidgetSize::Medium => "Medium",
            WidgetSize::Large => "Large",
        }
    }

    /// Logical `(width, height)` in pixels, aligned to the 20 px grid.
    pub fn dimensions(self) -> (u32, u32) {
        match self {
            WidgetSize::Small => (180, 180),
            WidgetSize::Medium => (360, 180),
            WidgetSize::Large => (360, 360),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetPlacement {
    pub kind: WidgetKind,
    pub size: WidgetSize,
    pub output: String,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopWidgetsLayout {
    pub widgets: Vec<WidgetPlacement>,
}

/// Rounds `value` to the nearest `GRID_STEP` (20 px) multiple.
pub fn snap_to_grid(value: i32) -> i32 {
    ((value as f32 / GRID_STEP as f32).round() as i32) * GRID_STEP
}

/// Snaps `(x, y)` to the 20 px grid and clamps within `[0, output_w - w]` x `[0, output_h - h]`.
pub fn clamp_and_snap(
    x: i32,
    y: i32,
    size: WidgetSize,
    output_w: u32,
    output_h: u32,
) -> (i32, i32) {
    let (w, h) = size.dimensions();
    let max_x = output_w.saturating_sub(w) as i32;
    let max_y = output_h.saturating_sub(h) as i32;
    let snapped_x = snap_to_grid(x).clamp(0, max_x);
    let snapped_y = snap_to_grid(y).clamp(0, max_y);
    (snapped_x, snapped_y)
}

/// Path to `~/.config/mac-suite/desktop-widgets.ron`.
pub fn layout_path() -> Option<PathBuf> {
    mac_widgets::settings::config_dir().map(|dir| dir.join("desktop-widgets.ron"))
}

impl DesktopWidgetsLayout {
    pub fn parse(text: &str) -> Result<Self, ron::error::SpannedError> {
        ron::from_str(text)
    }

    pub fn to_ron(&self) -> Result<String, ron::Error> {
        let pretty = ron::ser::PrettyConfig::new()
            .depth_limit(3)
            .struct_names(false);
        ron::ser::to_string_pretty(self, pretty)
    }

    pub fn load_from(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        Self::parse(&text).ok()
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let body = self.to_ron().map_err(io::Error::other)?;
        let tmp = path.with_extension(format!("ron.{}.tmp", std::process::id()));
        fs::write(&tmp, body)?;
        if let Err(err) = fs::rename(&tmp, path) {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
        Ok(())
    }

    /// Loads `desktop-widgets.ron` if it exists; otherwise creates sensible
    /// defaults in the top-right corner of `primary_output` and returns `true`
    /// for the `created` flag.
    pub fn load_or_default_at(
        path: Option<&Path>,
        primary_output: &str,
        output_w: u32,
        output_h: u32,
    ) -> (Self, bool) {
        if let Some(path) = path
            && path.exists()
            && let Some(loaded) = Self::load_from(path)
        {
            return (loaded, false);
        }
        let defaults = Self::default_for_output(primary_output, output_w, output_h);
        if let Some(path) = path {
            let _ = defaults.save_to(path);
        }
        (defaults, true)
    }

    /// Sensible default widgets (Clock, Weather, UpNext) placed in the
    /// top-right corner on the 20 px grid (away from top-left desktop icons).
    pub fn default_for_output(output: &str, output_w: u32, output_h: u32) -> Self {
        let (med_w, med_h) = WidgetSize::Medium.dimensions();
        let right_x = snap_to_grid(output_w.saturating_sub(med_w + EDGE_MARGIN as u32) as i32)
            .max(EDGE_MARGIN);
        let mut widgets = Vec::new();
        let candidates = [
            (WidgetKind::Clock, WidgetSize::Medium, TOP_MARGIN),
            (
                WidgetKind::Weather,
                WidgetSize::Medium,
                TOP_MARGIN + med_h as i32 + GRID_STEP,
            ),
            (
                WidgetKind::UpNext,
                WidgetSize::Medium,
                TOP_MARGIN + (med_h as i32 + GRID_STEP) * 2,
            ),
        ];
        for (kind, size, y) in candidates {
            let (sx, sy) = clamp_and_snap(right_x, y, size, output_w, output_h);
            widgets.push(WidgetPlacement {
                kind,
                size,
                output: output.to_owned(),
                x: sx,
                y: sy,
            });
        }
        Self { widgets }
    }

    /// Adds a widget snapped to the 20 px grid and returns its index.
    pub fn add_widget(
        &mut self,
        kind: WidgetKind,
        size: WidgetSize,
        output: impl Into<String>,
        x: i32,
        y: i32,
        output_bounds: (u32, u32),
    ) -> usize {
        let (sx, sy) = clamp_and_snap(x, y, size, output_bounds.0, output_bounds.1);
        self.widgets.push(WidgetPlacement {
            kind,
            size,
            output: output.into(),
            x: sx,
            y: sy,
        });
        self.widgets.len() - 1
    }

    /// Removes the widget at `index`, returning `true` if one existed.
    pub fn remove_widget(&mut self, index: usize) -> bool {
        if index < self.widgets.len() {
            self.widgets.remove(index);
            true
        } else {
            false
        }
    }

    /// Changes the size of the widget at `index` and re-clamps to output bounds.
    pub fn resize_widget(
        &mut self,
        index: usize,
        size: WidgetSize,
        output_w: u32,
        output_h: u32,
    ) -> bool {
        let Some(w) = self.widgets.get_mut(index) else {
            return false;
        };
        w.size = size;
        let (sx, sy) = clamp_and_snap(w.x, w.y, size, output_w, output_h);
        w.x = sx;
        w.y = sy;
        true
    }

    /// Moves the widget at `index` to `(x, y)` snapped to the 20 px grid.
    pub fn move_widget(
        &mut self,
        index: usize,
        x: i32,
        y: i32,
        output_w: u32,
        output_h: u32,
    ) -> bool {
        let Some(w) = self.widgets.get_mut(index) else {
            return false;
        };
        let (sx, sy) = clamp_and_snap(x, y, w.size, output_w, output_h);
        w.x = sx;
        w.y = sy;
        true
    }

    /// Finds an open 20 px grid-aligned position for a new widget on `output`.
    pub fn next_free_slot(&self, output: &str, size: WidgetSize, output_w: u32, output_h: u32) -> (i32, i32) {
        let (w, h) = size.dimensions();
        let existing: Vec<(i32, i32, i32, i32)> = self
            .widgets
            .iter()
            .filter(|p| p.output == output)
            .map(|p| {
                let (pw, ph) = p.size.dimensions();
                (p.x, p.y, pw as i32, ph as i32)
            })
            .collect();

        // Try columns from right to left so newly added widgets sit near the
        // default top-right corner without overlapping existing ones.
        let mut x = snap_to_grid(output_w.saturating_sub(w + EDGE_MARGIN as u32) as i32);
        while x >= EDGE_MARGIN {
            let mut y = TOP_MARGIN;
            while y + h as i32 <= output_h.saturating_sub(EDGE_MARGIN as u32) as i32 {
                let overlaps = existing.iter().any(|&(ex, ey, ew, eh)| {
                    x < ex + ew && x + w as i32 > ex && y < ey + eh && y + h as i32 > ey
                });
                if !overlaps {
                    return clamp_and_snap(x, y, size, output_w, output_h);
                }
                y += GRID_STEP;
            }
            x -= w as i32 + GRID_STEP;
        }
        clamp_and_snap(EDGE_MARGIN, TOP_MARGIN, size, output_w, output_h)
    }

    pub fn has_kind(&self, kind: WidgetKind) -> bool {
        self.widgets.iter().any(|w| w.kind == kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::env::temp_dir().join(format!(
            "mac-desktop-widgets-test-{name}-{}-{nanos}.ron",
            std::process::id()
        ))
    }

    #[test]
    fn grid_snapping_rounds_to_20px_and_clamps() {
        assert_eq!(snap_to_grid(0), 0);
        assert_eq!(snap_to_grid(9), 0);
        assert_eq!(snap_to_grid(10), 20);
        assert_eq!(snap_to_grid(29), 20);
        assert_eq!(snap_to_grid(31), 40);
        assert_eq!(snap_to_grid(-15), -20);

        assert_eq!(clamp_and_snap(43, 58, WidgetSize::Medium, 1920, 1080), (40, 60));
        assert_eq!(clamp_and_snap(-30, -10, WidgetSize::Small, 1920, 1080), (0, 0));
        // Medium is 360x180 -> max x = 1560, max y = 900
        assert_eq!(
            clamp_and_snap(2000, 1500, WidgetSize::Medium, 1920, 1080),
            (1560, 900)
        );
    }

    #[test]
    fn ron_load_save_roundtrip_and_plan_format() {
        let raw = r#"( widgets: [ (kind: Weather, size: Medium, output: "DP-2", x: 40, y: 60), (kind: UpNext, size: Small, output: "DP-2", x: 420, y: 60) ] )"#;
        let parsed = DesktopWidgetsLayout::parse(raw).unwrap();
        assert_eq!(parsed.widgets.len(), 2);
        assert_eq!(parsed.widgets[0].kind, WidgetKind::Weather);
        assert_eq!(parsed.widgets[0].size, WidgetSize::Medium);
        assert_eq!(parsed.widgets[0].output, "DP-2");
        assert_eq!((parsed.widgets[0].x, parsed.widgets[0].y), (40, 60));

        let path = temp_file("roundtrip");
        parsed.save_to(&path).unwrap();
        let reloaded = DesktopWidgetsLayout::load_from(&path).unwrap();
        assert_eq!(reloaded, parsed);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn first_launch_creates_defaults_and_empty_file_is_preserved() {
        let path = temp_file("defaults");
        let _ = fs::remove_file(&path);

        let (layout, created) =
            DesktopWidgetsLayout::load_or_default_at(Some(&path), "DP-2", 1920, 1080);
        assert!(created);
        assert_eq!(layout.widgets.len(), 3);
        assert!(layout.widgets.iter().all(|w| w.output == "DP-2"));
        assert!(layout.widgets.iter().all(|w| w.x % GRID_STEP == 0 && w.y % GRID_STEP == 0));

        // Saving an intentionally empty layout must not regenerate defaults on reload.
        let empty = DesktopWidgetsLayout::default();
        empty.save_to(&path).unwrap();
        let (reloaded, created_again) =
            DesktopWidgetsLayout::load_or_default_at(Some(&path), "DP-2", 1920, 1080);
        assert!(!created_again);
        assert!(reloaded.widgets.is_empty());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn new_kinds_listed_and_old_layouts_still_parse() {
        for kind in [WidgetKind::CalendarMonth, WidgetKind::Battery, WidgetKind::Notes] {
            assert!(WidgetKind::ALL.contains(&kind));
            assert!(!kind.label().is_empty());
            assert!(kind.icon_name().ends_with("-symbolic"));
        }
        let raw = r#"(widgets: [(kind: Notes, size: Large, output: "DP-1", x: 0, y: 0), (kind: Battery, size: Small, output: "DP-1", x: 0, y: 0), (kind: CalendarMonth, size: Medium, output: "DP-1", x: 0, y: 0)])"#;
        let parsed = DesktopWidgetsLayout::parse(raw).unwrap();
        assert_eq!(parsed.widgets[0].kind, WidgetKind::Notes);
        let again = DesktopWidgetsLayout::parse(&parsed.to_ron().unwrap()).unwrap();
        assert_eq!(again, parsed);
    }

    #[test]
    fn add_move_resize_and_remove_widgets() {
        let mut layout = DesktopWidgetsLayout::default();
        let idx = layout.add_widget(WidgetKind::System, WidgetSize::Small, "DP-1", 53, 71, (1920, 1080));
        assert_eq!(idx, 0);
        assert_eq!((layout.widgets[0].x, layout.widgets[0].y), (60, 80));

        assert!(layout.move_widget(0, 1690, 990, 1920, 1080));
        assert_eq!((layout.widgets[0].x, layout.widgets[0].y), (1700, 900));

        // Resizing from Small (180x180) to Large (360x360) re-clamps into 1920x1080.
        assert!(layout.resize_widget(0, WidgetSize::Large, 1920, 1080));
        assert_eq!(layout.widgets[0].size, WidgetSize::Large);
        assert_eq!((layout.widgets[0].x, layout.widgets[0].y), (1560, 720));

        assert!(!layout.resize_widget(9, WidgetSize::Medium, 1920, 1080));
        assert!(layout.remove_widget(0));
        assert!(!layout.remove_widget(0));
        assert!(layout.widgets.is_empty());
    }
}
