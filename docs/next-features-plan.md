# Next features plan

Six features to build next, plus small fixes found while testing the current
build. Each feature ships on its own, keeps `cosmic-preset revert` working, and
stays inside `~/.local` and `~/.config`.

## 0. Fixes from the current build (do first, small)

Found in the screenshots of Launchpad, Appearance, the clock popup and the
system stats applet (15 Nov 2026):

| Where | Issue | Fix |
|---|---|---|
| Launchpad | Same app twice (two "ArtCraft Launcher" entries) | Dedupe by desktop-file id, then by `Name` + `Exec`; prefer `~/.local/share/applications` over `/usr/share` |
| Launchpad | `COSMIC Preset: …` helper entries fill the first pages | Mark our helper `.desktop` files `NoDisplay=true` or move them to a "Utilities" folder once folders exist |
| Clock popup | System card repeats the new System Stats applet | Default `show_system=false` in `widgets.conf` when `dev.asw.CosmicExtAppletSysmon` is in the panel layout |
| Clock popup | CPU shows 0% on first open | Take the first CPU sample when the applet starts, not when the popup opens |

## 1. Launchpad phase 2: folders, drag-to-reorder, animation

**Goal:** behaves like macOS Launchpad: you arrange the grid yourself, drop one
app on another to make a folder, and it zooms in and out.

**Layout file:** `~/.config/mac-launchpad/layout.ron`

```ron
(
  version: 1,
  pages: [
    [ App("firefox.desktop"), Folder(name: "Utilities", apps: ["org.gnome.Calculator.desktop", ...]), ... ],
    ...
  ],
)
```

- Apps not in the file (newly installed) are added at the end of the last page.
- Apps whose `.desktop` file is gone are dropped silently.
- Written with a temp-file and rename, after a 1 s debounce.

**Drag and drop (in `ui.rs`):**
- Press and hold for 300 ms, or press and move 6 px, to start a drag. The icon
  lifts (scale 1.1, shadow) and follows the pointer. The other icons slide to
  make room, with about 150 ms easing.
- Hold the dragged icon over another icon's centre for 500 ms to make a folder.
  Hold it near the left or right edge for 700 ms to flip pages.
- Escape cancels the drag. Searching disables dragging.
- No real Wayland DnD is needed: it all happens inside one surface, using
  `mouse_area` and pointer coordinates mapped to grid cells.

**Folders:**
- A folder shows a rounded tile with a 3×3 mini-icon preview.
- Click opens an overlay panel with the folder's own grid, an editable title and
  pages if it holds more than 12 apps.
- Dragging an app out of the open folder closes it and drops the app on the
  main grid. A folder left with one app turns back into that app.
- The default name comes from the shared freedesktop category (`Utility` →
  "Utilities", `Development` → "Developer").

**Animation:**
- Opening: the background fades 0→1 while the grid scales 1.08→1.0 over 220 ms.
  Closing reverses it in 160 ms.
- Page change: the grid slides with a 250 ms ease-out, and a two-finger swipe
  (scroll delta) follows the finger.
- Uses an `iced::time::every(16ms)` tick only while animating; idle stays at 0%
  CPU.

**Edit mode:** right-click on empty space shows "Reset layout" and "Sort A–Z".

**Tests:**
- Unit tests for layout merge (new, removed and duplicate apps), folder
  create/dissolve, and drop-index maths.
- Manual check: drag across pages, folder rename, restart keeps the layout.

**Risks:** iced has no built-in drag in a grid, so we track pointer state
ourselves. Keep it in a small `drag.rs` state machine with unit tests.

**Size:** M–L (3–4 sessions).

## 2. Screen recording (Record in Screenshot)

**Goal:** the `Super+Shift+S` toolbar gains "Record Entire Screen" and "Record
Selected Portion", like macOS ⇧⌘5.

**How:**
- Ask `xdg-desktop-portal-cosmic` (installed, 1.11) for a ScreenCast session
  over D-Bus with `ashpd` (`org.freedesktop.portal.ScreenCast`, monitor source,
  cursor embedded if "Show pointer" is on). The portal returns a PipeWire node
  id and fd. On COSMIC the picker is skipped when we pass the output we want.
- Encode with GStreamer, which is installed with `gst-plugin-pipewire`:
  `pipewiresrc fd=… path=… ! videoconvert ! videocrop (region) ! x264enc or vaapih264enc / nvh264enc (when present) ! mp4mux ! filesink`.
  The pipeline is built in code (the `gstreamer` crate) so we can pick the
  hardware encoder at run time. For your GTX 1050 Ti that is `nvh264enc`;
  otherwise it falls back to `x264enc speed-preset=veryfast`.
- Audio (optional, off by default): system output via `pulsesrc` monitor, or the
  microphone, chosen in Options.

**UI:**
- While recording, a small red "■ 00:12" stop pill sits in the top-right
  corner, as a layer-shell surface on the overlay layer.
- `Super+Shift+S` again also stops, through the single-instance lock: a second
  start sends "stop" over a socket in the private runtime dir.
- The finished file shows the same floating thumbnail as a screenshot. Click
  opens it in the default video player.
- Files are saved as `~/Videos/Screen Recordings/Screen Recording YYYY-MM-DD at HH.MM.SS.mp4`.

**Files:** new `mac-screenshot/src/record.rs`, toolbar buttons in
`ui/overlay.rs`, a stop pill in `ui/recording.rs`, and Cargo features
`gstreamer` and `ashpd`.

**Tests:**
- Unit tests for region→crop maths and file naming.
- Manual checks: 10 s of full screen, a region, a multi-monitor output, the
  NVIDIA and software encoders, stop through the pill and through the shortcut.

**Risks:**
- The portal may still show a picker. Accept it the first time; the "restore
  token" lets later recordings skip it, so we store that token in
  `screenshot.conf`.
- Damage-based frame rates can make the video choppy when the screen is idle.
  `videorate` fixes this at 30 fps.

**Size:** M (2 sessions).

## 3. Genie restore for fullscreen windows

**Goal:** a minimized fullscreen window grows back out of its dock icon, like
the floating and tiled windows already do.

**Today:**
- `Workspace::unminimize` (`cosmic-comp/src/shell/workspace.rs`) pushes the
  `FullscreenSurface` back with `start_at: None`, so it appears at once.
- `Shell::unminimize_request` passes `from`, the dock icon rect, but the
  fullscreen branch ignores it.

**Change:**
- Add `restoring_since: Option<Instant>` beside `minimizing_since`. In
  `unminimize`, for `MinimizedWindow::Fullscreen`, set
  `previous_geometry = Some(from)` (the icon) and `restoring_since = Some(now)`.
- In `render_fullscreen`, when `restoring_since` is set, build
  `GenieRenderElement` with `progress = 1 - elapsed/MINIMIZE_ANIMATION_DURATION`,
  the same warp run backwards.
- Clear it in `update_animation_state` after the duration, and mark the
  workspace dirty each frame while it runs (as the minimize path does).
- Keep `previous_geometry` restored to the real pre-fullscreen geometry
  afterwards, so un-fullscreening later still returns the window to its old
  size. This means storing the icon rect in a separate field (`genie_target`)
  instead of reusing `previous_geometry`, and making the minimize path use that
  field too.

**Tests:** manual: F11 browser → minimize → click dock icon; a video player in
fullscreen; restore during the minimize animation (interrupt). Run
`cargo check` and keep the crash-guard launcher.

**Size:** S (1 session).

## 4. Appearance: per-screen wallpaper

**Goal:** a different wallpaper (or light/dark pair, or dynamic set) for each
monitor, like macOS.

**cosmic-bg format:**
- `com.system76.CosmicBackground/v1/all` holds the shared entry, and
  `same-on-all` is `true`.
- For per-screen wallpapers, set `same-on-all=false`. cosmic-bg then reads one
  entry per output, keyed by connector name, in `backgrounds` plus a file named
  `output.<connector>` (to be confirmed against the `cosmic-bg-config` crate
  before coding, by reading its source in the cargo checkout).

**Changes:**
- `mac-appearance/config.rs`: settings gain `[output.<connector>]` sections
  with the same keys (`wallpaper`, `wallpaper_light`, `wallpaper_dark`,
  `dynamic_dir`), and missing ones fall back to the global keys.
- `apply.rs` writes one entry per output plus `all` (for screens plugged in
  later). It only sets `same-on-all=false` when at least one per-output
  setting exists.
- Settings UI (`mac-theme/src/appearance_ui.rs`): a "Screens" dropdown listing
  outputs from `wl_output` names ("All screens" by default), and each screen
  shows its own wallpaper row. A small thumbnail preview of each choice would
  be nice to have.
- Output names come from `cosmic-randr list` or the Wayland `wl_output` name
  event. Use the latter: it is already in libcosmic through cctk.

**Tests:**
- Unit tests for config parsing with output sections and the fallback order.
- A golden-file test of the generated cosmic-bg entries.
- Manual check with your DP-2 + HDMI-A-2 / DP-1 + HDMI-A-1 + DVI-D-1 outputs.

**Size:** S–M (1–2 sessions).

## 5. Desktop widgets

**Goal:** Weather, Up Next, System and a big Clock on the desktop, like macOS
Sonoma widgets, sitting on the wallpaper and under windows.

**How:**
- New app `mac-desktop-widgets` in `apps/mac-suite`. It is a layer-shell
  surface on the `Bottom` layer, one per output, with no keyboard focus and an
  input region covering only the widget cards. Clicks elsewhere reach the
  desktop.
- It reuses `mac-widgets` for weather, stats and settings. The libcosmic card
  views are copied from the clock applet's `widgets.rs` into a shared module.
  The clock applet uses a different libcosmic, so the views can't be one crate;
  only the data crate is shared.
- Up Next reads the Calendar app's local store directly, the same file the
  clock applet uses, instead of going through the applet.

**Layout:** `~/.config/mac-suite/desktop-widgets.ron`

```ron
( widgets: [ (kind: Weather, size: Medium, output: "DP-2", x: 40, y: 60), (kind: UpNext, size: Small, ...), ... ] )
```

**Editing:**
- Right-click on a widget offers Small / Medium / Large and Remove.
- "Edit Widgets…" (from the clock popup and the desktop right-click) shows a
  gallery strip at the bottom. Drag a widget out of it to place it, and drag
  placed widgets to move them. Positions snap to a 20 px grid and are saved
  per output.

**Look:**
- Frosted cards that follow the theme. Widgets dim slightly when a window is
  focused, as macOS does.
- Refresh: stats every 2 s only while visible, and weather from the shared
  30-minute cache.

**Install:** `./install.sh desktop-widgets` adds an autostart entry. It is off
in the `original` preset, through a preset marker file like the dock's.

**Risks:**
- COSMIC's own desktop icons (Files desktop) also live on the bottom
  layer/background. Check the stacking order, and keep widgets in a corner by
  default.
- Partial input regions need `set_input_region`. Check that libcosmic exposes
  it on layer surfaces; if not, use one small surface per widget.

**Size:** M–L (2–3 sessions).

## 6. Real app menus (File / Edit / View …)

**Goal:** the panel's App menu shows the focused app's own menus, like the
macOS menu bar, at least for our own apps (Files, Notepad, Calendar, Mail,
Image Viewer).

**Protocol** (D-Bus, session bus, our own and simple):
- Each app owns the name `dev.asw.AppMenu.<app id>` and exports
  `/dev/asw/AppMenu` with interface `dev.asw.AppMenu1`:
  - `GetLayout() -> a(s s a(s s s b b))`: menus (id, label) → items (id,
    label, shortcut, enabled, checked); separators are id `-`.
  - `Activate(s item_id)`
  - signal `LayoutChanged()` (for enabled/checked changes, e.g. Undo
    available).
- The panel applet (`cosmic-ext-applet-app-menu`) watches the focused toplevel
  (it already tracks it through cctk). If `dev.asw.AppMenu.<app_id>` exists on
  the bus, it shows those menus as pills (File, Edit, View, Window, Help) next
  to the bold app name. If not, it keeps today's App/Window menus.
- It activates through `Activate`. The app runs the action and keeps its own
  keyboard shortcuts.
- Alternative considered: the KDE/Canonical `com.canonical.dbusmenu` standard
  works with Qt/GTK apps too. It is more work (a tree with properties and
  layout revisions), so plan v2 as "also read dbusmenu" if wanted later.

**App side:**
- A small helper crate `mac-appmenu` (zbus, no UI) used by mac-suite apps and
  by our forks of cosmic-files, cosmic-edit and cosmic-viewer.
- The app builds its menu from the same actions as its in-window menu bar.
  libcosmic apps already have a `menu::Tree`/key-binds list, so one function
  converts that into the D-Bus layout. With the global menu active, apps may
  hide their in-window menu bar (a setting, off by default).

**Panel side:** popovers reuse `mac_menu_item`/`mac_menu_divider`, so each menu
pill opens a popover, and hovering the next pill moves to it (macOS-style menu
tracking). Shortcuts are shown right-aligned (⌘ shown as Ctrl).

**Order:**
1. Protocol, helper crate and the panel side, with a demo in Notepad.
2. Files, Calendar and Mail.
3. Image Viewer.
4. Optionally, dbusmenu for third-party Qt/GTK apps.

**Tests:**
- Unit tests for layout (de)serialisation and the libcosmic-menu → layout
  conversion.
- Manual checks: focus switching updates the menus within 100 ms, a disabled
  item can't be clicked, a shortcut label matches the key bind.

**Risks:** the `app_id` from toplevel info must match the D-Bus name. Our apps
use `dev.asw.Mac*` ids, so that holds for ours. For forks, set it explicitly.

**Size:** L (3–4 sessions; step 1 alone is 1–2).

## Suggested order

| # | Work | Why first |
|---|---|---|
| 0 | Fixes from the current build | Small, visible right now |
| 3 | Genie restore for fullscreen | Small, finishes an existing feature |
| 4 | Per-screen wallpaper | Small–medium; you have several monitors |
| 1 | Launchpad phase 2 | Most-used new tool |
| 2 | Screen recording | Self-contained |
| 5 | Desktop widgets | Builds on `mac-widgets` |
| 6 | Real app menus | Biggest; touches many apps |

Items 1, 2, 4 and 5 touch separate crates and can run in parallel (one agent
each, as with the last swarm). Items 3 and 6 touch shared code (compositor,
panel applet plus several apps) and should run alone.
