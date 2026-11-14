# Panel toolbar plan

Goal: make the top panel read as a macOS menu bar: a thin frosted strip with
the system menu and the active app's name on the left, the clock in the center,
a short row of monochrome status icons, Spotlight and Control Center on the
right, and soft pill highlights on hover. Everything stays in the `macos-glass`
preset, so `cosmic-preset revert` still brings the stock panel back.

## Decisions

| Question | Choice |
|---|---|
| Height | 28 px: `size: Custom(28)` (14 px symbolic icons, 7 px padding) instead of `XS` (32 px) |
| System menu | Our own applet, `cosmic-ext-applet-lord-menu`, replacing the third-party `cosmic-ext-applet-logomenu` |
| App name and menu | New applet, `cosmic-ext-applet-app-menu`: bold app name plus an app/window menu built from toplevel info and the `.desktop` file. No File/Edit/View menus (COSMIC has no global-menu protocol) |
| Status icons | Stock Network, Audio and Battery applets stay as individual icons; Bluetooth, tiling, power and system monitor move out of the bar |
| Control Center | New applet, `cosmic-ext-applet-control-center`: one icon, one popover with Wi-Fi, Bluetooth, Focus, Display, Sound, Now Playing tiles |
| Clock | The existing Clock Center, kept in the centre (`plugins_center`), with a macOS date format |
| Clipboard | `dev.asw.CosmicExtAppletClipboard` from `docs/clipboard-manager-plan.md` takes the `clippy-land` slot |
| Hover pills | Our applets draw their own pill; stock applets get it through an optional applet-style patch (phase 4) |
| Layout writes | One helper, `tools/panel-layout.sh`, owns `plugins_wings`/`plugins_center`; install scripts change only their slot |

## Where it stands

- `presets/macos-glass/com.system76.CosmicPanel.Panel/v1`: `anchor: Top`,
  `size: XS` (32 px), `expand_to_edges: true`, `background: Dark`,
  `opacity: 0.65`, `border_radius: 0`, `border_width: 0.0`, `spacing: 0`,
  `padding: 0`. Frosting comes from the theme (`frosted_panel: true`,
  `frosted: Medium`) and the compositor's blur pass
  (`cosmic-comp/src/backend/render/mod.rs`, strength from `frosted`).
- Left wing: `dev.cappsy.CosmicExtAppletLogoMenu` (AUR),
  `CosmicPanelLauncherButton`, `CosmicPanelWorkspacesButton`,
  `CosmicPanelAppButton`.
- Centre: `dev.asw.CosmicExtAppletClockCenter` (ours, in
  `overlays/cosmic-applets/`): clock, month calendar with Calendar app events,
  now playing, notifications and a Do Not Disturb toggle. It also replaces the
  notifications applet (`X-NotificationsApplet=true`).
- Right wing: `minimon` (AUR), `clippy-land` (AUR), Tiling, Status Area
  (tray), Audio, Network, Bluetooth, Battery, Power — nine items, all
  stock-styled.
- The layout is written in two places that can disagree: the preset files and
  `install-clock-center.sh`'s `write_panel_layout`, which rewrites the whole
  `plugins_wings` each time.
- `cosmic-preset` manages the panel directory as a whole
  (`MANAGED_DIRS`) and warns about applets that are not installed
  (`check_applets`).
- No `cosmic-panel` working copy or patch exists; `setup.sh` fetches files,
  edit, viewer, launcher, comp, applets and libcosmic.

## Target layout

```
┌──────────────────────────────────────────────────────────────────────────────────────┐
│ ◆  Files   Window               Sat 14 Nov 12:46          📋  🔊  🔋  📶  🔍  ⊜      │
└──────────────────────────────────────────────────────────────────────────────────────┘
  │   │                                  │                   │   │   │   │   │   └ Control Center
  │   └ App menu (bold active app name)  └ Clock Center      │   │   │   │   └ Spotlight
  └ Lord menu (logo)                                         │   │   │   └ Network
                                                             │   │   └ Battery
                                                             │   └ Audio
                                                             └ Clipboard
```

`plugins_center` keeps `dev.asw.CosmicExtAppletClockCenter` centered in the middle:

```ron
// plugins_center
Some([
    "dev.asw.CosmicExtAppletClockCenter",
])

// plugins_wings
Some(([
    "dev.asw.CosmicExtAppletLordMenu",
    "dev.asw.CosmicExtAppletAppMenu",
], [
    "com.system76.CosmicAppletStatusArea",
    "dev.asw.CosmicExtAppletClipboard",
    "com.system76.CosmicAppletAudio",
    "com.system76.CosmicAppletBattery",
    "com.system76.CosmicAppletNetwork",
    "dev.asw.CosmicExtAppletSpotlight",
    "dev.asw.CosmicExtAppletControlCenter",
]))
```

The tray (Status Area) stays leftmost on the right, where macOS puts
third-party menu extras. Battery hides itself on desktops (stock behaviour).

## Bar geometry and glass

| Key | Now | New | Why |
|---|---|---|---|
| `size` | `XS` | `Custom(28)` | 28 px bar; `PanelSize::Custom` gives icon `28/2 = 14`, padding `28/4 = 7` |
| `background` | `Dark` | `ThemeDefault` | Light bar in light mode, dark in dark mode, like macOS |
| `opacity` | `0.65` | `0.55` | More wallpaper through the blur; tune on light and dark wallpapers |
| `spacing` | `0` | `2` | Small gap between pills |
| `padding` | `0` | `0` (left/right inset comes from the first/last applet: 10 px) | Keep the bar tight to the screen edge |
| `border_width` | `0.0` | `1.0` (try first) | Hairline; see below |
| `border_radius` | `0` | `0` | Full-width strip |
| `autohover_delay_ms` | `500` | `150` | Moving between open menus feels instant, as on macOS |

Theme (`com.system76.CosmicTheme.{Dark,Light}/v2`, written by `mac-theme`):
keep `frosted_panel: true` and `frosted_applets: true`, try `frosted: High`
for the panel/popovers if the compositor's blur strength allows a per-surface
level; otherwise keep `Medium` everywhere.

**Bottom hairline.** `border_width` draws around the whole panel. With
`expand_to_edges` the top, left and right edges sit on the screen border, so
in practice only the bottom line shows. Check that on a real session first. If
the side lines are visible, phase 4 adds a small `cosmic-panel` patch
(`setup.sh panel`, `patches/cosmic-panel.patch`) that draws only the edge
opposite the anchor: 1 px, white at 8% in dark mode, black at 12% in light
mode.

**Text.** 13 px, the theme's interface font; the app name in bold (weight 700),
everything else regular. The clock uses tabular figures so it does not jitter.

## Pill hover states

- Every item in the bar is a pill: height 22 px (bar minus 3 px top and
  bottom), horizontal padding 8 px (6 px for icon-only items), corner radius
  6 px (`radius_s` minus 2).
- Idle: transparent. Hover: foreground at 12% alpha. Open (its popover or
  menu is showing): foreground at 20% alpha, stays until the popover closes.
- Our applets get this from one shared widget, `mac_pill_button`, in a tiny
  crate `overlays/cosmic-applets/cosmic-ext-mac-applet-style/` used by the
  lord menu, app menu, Control Center, Spotlight button, clipboard and the
  Clock Center (whose current `menu_button`/`padded_control` styling it
  replaces).
- Stock applets (Audio, Network, Battery, Status Area) keep libcosmic's applet
  button until phase 4, which builds them from `upstream-applets` with a small
  `cosmic-applets.patch` addition that swaps their button class for the same
  pill, installed under `~/.local/bin` and pointed at by override desktop files
  in `~/.local/share/applications` (the technique the clock already uses).
- Popovers: frosted, `radius_m` (12 px) corners, 6 px gap below the bar, 1 px
  inner stroke at 10% alpha, opened with `X-CosmicHoverPopup=Auto` so moving
  across the bar switches menus without another click.

## Left zone

### Lord menu (`cosmic-ext-applet-lord-menu`, `dev.asw.CosmicExtAppletLordMenu`)

Logo glyph (our own symbolic SVG in `data/icons`, configurable to the distro
logo via `cosmic-config`), 14 px, in a pill. Menu:

| Item | Action |
|---|---|
| About This Desktop | Small About window: logo, distro and COSMIC version, hostname, CPU, memory, GPU, display, "More Info…" → `cosmic-settings about` |
| — | |
| System Settings… | `cosmic-settings` |
| App Store… | `cosmic-store` (hidden if not installed) |
| — | |
| Recent Items ▸ | Last 10 apps launched (from the dock's/launcher's history if available; else hidden) |
| — | |
| Force Quit… | List of running apps from toplevel info; quit sends close, "Force Quit" kills the PID after confirm |
| — | |
| Sleep | logind `Suspend` (as the stock power applet does) |
| Restart… | `cosmic-osd restart` (fallback: logind `Reboot`) |
| Shut Down… | `cosmic-osd shutdown` (fallback: logind `PowerOff`) |
| — | |
| Lock Screen   ⌃⌘Q | logind `LockSession` |
| Log Out <user>…   ⇧⌘Q | `cosmic-osd log-out` |

The power actions copy the stock `cosmic-applet-power` code paths (cosmic-osd
confirm dialog, logind fallback), so the stock Power applet can leave the bar.

### App menu (`cosmic-ext-applet-app-menu`, `dev.asw.CosmicExtAppletAppMenu`)

- Shows the active app's name in bold, taken from the activated toplevel
  (cosmic toplevel info + `ext-foreign-toplevel-list`, the same handler code
  as `cosmic-ext-dock/src/wayland_handler.rs`; the applet desktop file sets
  `X-HostWaylandDisplay=true`). The name is the `.desktop` `Name=` for the
  app id, falling back to the window title. With no window focused it shows
  "Files" (the desktop belongs to the file manager on macOS) — configurable,
  or "Desktop".
- Click opens the app menu:
  - About <App> (name, icon, version from the desktop file / AppStream if
    present)
  - New Window (desktop action `new-window` if present, else relaunch)
  - other desktop actions (`Actions=` in the `.desktop` file)
  - Hide <App> (minimise all its windows), Show All
  - Minimize, Zoom (maximise toggle), Enter Full Screen — for the focused window
  - Move to Workspace ▸ (workspaces leave the bar but stay reachable here)
  - Close Window, Quit <App> (close all its windows)
- Next to the app name, a second pill "Window" opens the same window
  actions, so the bar has at least two menus like macOS. Real
  File/Edit/View menus need a global-menu protocol COSMIC does not have; a
  later phase can let our own mac-suite apps publish menus over D-Bus
  (`dev.asw.AppMenu`) and render them here.
- Long names are elided at 220 px; the applet is `X-CosmicShrinkable`.

The left wing loses the Launcher, Workspaces and App buttons. Spotlight moves
right, workspaces live in the app menu and in the dock's right-click menu, and
the app grid stays on its keyboard shortcut.

## Right zone

### Status icons

- Audio, Battery, Network: stock applets, monochrome symbolic icons at 14 px.
  Clicking opens their stock popovers (those already have volume, output
  device, battery details, network lists).
- Bluetooth, Tiling, Power, minimon: removed from the bar. Bluetooth and Focus
  are in Control Center; Power is in the lord menu; tiling is a Control Center
  tile; minimon becomes optional (README tells how to add it back).
- `cosmic-ext-applet-spotlight` (`dev.asw.CosmicExtAppletSpotlight`): a
  magnifier pill that runs `cosmic-launcher` (our restyled Spotlight). A
  20-line applet; the stock Launcher button cannot change its icon per preset.

### Control Center (`cosmic-ext-applet-control-center`, `dev.asw.CosmicExtAppletControlCenter`)

Icon: two stacked toggles (own SVG). Popover, 320 px wide, macOS grid:

```
┌──────────────────────────────┐
│ ┌─────────────┐ ┌──────────┐ │
│ │ 📶 Wi-Fi    │ │ 🌙 Focus │ │
│ │   Home-5G  ›│ │   Off    │ │
│ │ ᛒ Bluetooth │ ├──────────┤ │
│ │   On       ›│ │ ⊞ Tiling │ │
│ └─────────────┘ └──────────┘ │
│ Display                      │
│ ☀ ━━━━━━━━━━━○──────────    │
│ Sound                        │
│ 🔊 ━━━━━━━━○────────────  › │
│ ┌──────────────────────────┐ │
│ │ ♪ Song — Artist   ⏮ ⏯ ⏭ │ │
│ └──────────────────────────┘ │
│ Edit Controls…               │
└──────────────────────────────┘
```

| Tile | Source |
|---|---|
| Wi-Fi on/off, current network, `›` → network list | NetworkManager over D-Bus (`cosmic-dbus-networkmanager`, as the stock network applet) |
| Bluetooth on/off, `›` → device list | BlueZ over D-Bus (as the stock bluetooth applet) |
| Focus / Do Not Disturb | `cosmic-notifications-config` `do_not_disturb` (the same key the Clock Center toggles) |
| Tiling | `cosmic-comp` config `autotile` (as the stock tiling applet) |
| Display brightness | `com.system76.CosmicSettingsDaemon` brightness (as the stock battery applet); hidden when no backlight |
| Sound volume, `›` → output devices | PulseAudio/PipeWire (copy of the stock audio applet's `pulse` module) |
| Now Playing | MPRIS (reuse the Clock Center's `mpris_subscription.rs`) |
| Edit Controls… / chevrons with no inline list | `cosmic-settings wireless`/`bluetooth`/`displays`/`sound`/`notifications` |

The code for each tile is copied from the stock applets in
`upstream-applets` (same GPL-3.0 licence) into
`cosmic-ext-applet-control-center/src/{network,bluetooth,audio,brightness}.rs`,
since their internals are private to each applet crate. Each tile hides
itself when its service is missing (no Bluetooth adapter, no backlight).

### Clipboard

`dev.asw.CosmicExtAppletClipboard`, defined in
`docs/clipboard-manager-plan.md`. It uses the shared pill style; this plan
only reserves its slot.

### Clock Center (existing)

- Stays in the centre slot (`plugins_center: Some(["dev.asw.CosmicExtAppletClockCenter"])`).
- Format: `Sat 14 Nov 12:46` (weekday, day, month, time; follows the
  locale's 12/24-hour setting and the `cosmic-applet-time` config).
- The pill uses the shared style; the popover keeps calendar, events, now
  playing and notifications. When Control Center is installed, Now Playing in
  the clock popover is hidden to avoid showing it twice (config flag).
- Keeps `X-NotificationsApplet=true`, so the login check in
  `cosmic-preset login-check` keeps working.

## Files to touch

| File | Change |
|---|---|
| `presets/macos-glass/com.system76.CosmicPanel.Panel/v1/{size,background,opacity,spacing,border_width,autohover_delay_ms,plugins_wings,plugins_center}` | New geometry and layout |
| `presets/macos-glass/com.system76.CosmicTheme.*/v2/frosted*` | Only if tuning shows it is needed |
| `overlays/cosmic-applets/cosmic-ext-mac-applet-style/` | New: shared pill button, popover container, icons |
| `overlays/cosmic-applets/cosmic-ext-applet-lord-menu/` | New applet |
| `overlays/cosmic-applets/cosmic-ext-applet-app-menu/` | New applet |
| `overlays/cosmic-applets/cosmic-ext-applet-control-center/` | New applet |
| `overlays/cosmic-applets/cosmic-ext-applet-spotlight/` | New applet |
| `overlays/cosmic-applets/cosmic-ext-applet-clock-center/src/window.rs` | Shared pill, date format, hide Now Playing flag |
| `patches/cosmic-applets.patch` | Workspace members for the new crates; phase 4: pill class in stock Audio/Network/Battery/Status Area |
| `setup.sh` (`setup_applets`), `tools/export.sh` | New overlay directory names |
| `tools/panel-layout.sh` | New: the only writer of `plugins_wings`/`plugins_center` |
| `install-panel.sh`, `install.sh` (`panel` component, `needs: applets`) | Build and install the four applets, desktop files with absolute `Exec`, layout |
| `install-clock-center.sh` | Use `tools/panel-layout.sh` instead of `write_panel_layout` |
| `bin/cosmic-preset` | Nothing for phases 1–3 (the panel dir is already managed); phase 4 may add override desktop files to its checks |
| `README.md`, `install.sh --check` | Drop `logomenu`, `minimon`, `clippy-land` from requirements; new component row |
| `patches/cosmic-panel.patch`, `setup.sh panel` | Phase 4 only, if the hairline needs it |

`apps/mac-suite/` is not touched by this plan.

## Phases

### Phase 1: Geometry and layout

- `tools/panel-layout.sh` with named slots; `install-clock-center.sh` moved to
  it.
- Preset: `Custom(28)`, `ThemeDefault`, opacity, spacing, hover delay,
  `border_width: 1.0`; clock kept in `plugins_center`; Bluetooth, Tiling,
  Power, minimon, Launcher, Workspaces and App buttons out of the layout.
- Done when: `cosmic-preset apply macos-glass` gives a 28 px bar with the
  clock in the centre and no layout regressions on reinstalling the clock.

### Phase 2: Left zone

- Shared style crate with the pill button.
- Lord menu (with About window and Force Quit list).
- App menu with the active app name, app and window menus.
- `logomenu` removed from the layout, README and `--check`.

### Phase 3: Right zone

- Spotlight button, Control Center with all tiles, Clock Center restyle and
  date format, clipboard slot filled once its applet exists (until then
  `clippy-land` keeps the slot).

### Phase 4: Finish

- Pill style for stock status applets (patched build, override desktop files).
- `cosmic-panel` hairline patch if `border_width` shows side lines.
- Global menus for our own apps over `dev.asw.AppMenu` (optional).

## How it will be checked

- `cargo build -p` / `cargo test -p` for each new crate in `upstream-applets`;
  unit tests for the app-name lookup (app id → desktop file, with
  `StartupWMClass` and reverse-DNS ids), the date formatter (12/24 h, several
  locales), desktop-action parsing and `tools/panel-layout.sh` (a shell test
  that runs it on a copy of the preset and diffs the result).
- `cosmic-preset apply macos-glass` prints no "applet not installed" warning
  for the new layout.
- Manual checks in a COSMIC session, dark and light mode, on a busy and a plain
  wallpaper:
  - The bar measures 28 px (screenshot), blur shows through, the hairline is
    visible only at the bottom.
  - Hover each item: pill appears; open a popover: pill stays; with one menu
    open, move to the next item: it opens after the short hover delay.
  - App name follows focus across Files, Firefox, Terminal and an X11 app;
    Quit, Hide, Zoom and New Window work; nothing focused shows "Files".
  - Lord menu: each item does what it says; Restart/Shut Down/Log Out show the
    cosmic-osd confirm dialog; Sleep suspends and resumes.
  - Control Center: Wi-Fi and Bluetooth toggles match the stock applets and
    settings; brightness and volume sliders move the hardware and follow
    changes from keyboard keys; Focus mirrors the Clock Center toggle; Now
    Playing controls a running player.
  - Desktop without battery or Bluetooth: those tiles and icons hide.
  - Two monitors (`output: All`): both bars correct; popovers open on the
    clicked monitor.
  - `pkill cosmic-panel` and a re-login: everything comes back (clock login
    check still works).
  - `cosmic-preset revert` restores the stock panel; `./install-panel.sh
    --uninstall` puts back the previous layout.

## Risks

- **28 px is small for touch and for 14 px icons on HiDPI**; keep `Custom(30)`
  or `XS` as a quick fallback in the preset notes.
- **Stock applet look**: until phase 4 the stock status icons keep libcosmic's
  square hover, which will look slightly different next to our pills.
- **No real app menus**: the app menu only offers app and window actions;
  users expecting File/Edit/View will not find them in the bar.
- **Copied tile code** (network, audio, bluetooth) drifts from upstream; a
  COSMIC release bump means re-checking it along with the patches.
- **Panel config keys** (`Custom(n)`, `border_width` drawing) can change
  between COSMIC releases; `setup.sh`'s `TAG` bump checklist should include
  the panel preset.
