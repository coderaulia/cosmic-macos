# Repository Guidance (`cosmic-macos`)

`cosmic-macos` builds and installs a macOS-inspired desktop experience on top of the COSMIC desktop (`cosmic-comp`, `libcosmic`, `cosmic-applets`, `cosmic-launcher`, and COSMIC apps) plus a companion Rust workspace (`apps/mac-suite`). Everything installs strictly into the current user's `$HOME` (`~/.local` and `~/.config`); nothing in `/usr` is ever modified.

## 1. Architecture & Repository Layout

- **`apps/mac-suite/`**: Cargo workspace containing 12 custom crates (edition `2024`, license `GPL-3.0-only`):
  - `mac-appearance`: `mac_appearance` library + `mac-appearance` daemon for automatic light/dark switching, offline sunrise/sunset calculation, dynamic wallpaper schedules, and per-screen wallpaper settings.
  - `mac-appmenu`: Session D-Bus global application menu protocol (`dev.asw.AppMenu1`) and client/server helpers.
  - `mac-calendar`: Calendar application (`dev.asw.MacCalendar`) with month, week, and day views, CalDAV sync, and `dev.asw.AppMenu1` menus.
  - `mac-clipboard`: Clipboard history daemon, CLI, and quick-paste Spotlight overlay (`dev.asw.MacClipboard`, `dev.asw.MacClipboard.Overlay`).
  - `mac-desktop-widgets`: macOS Sonoma-style desktop widgets rendered on the Wayland bottom layer (`dev.asw.MacDesktopWidgets`), reading `mac-calendar` agenda and `mac-widgets` weather/stats.
  - `mac-launchpad`: Full-screen application grid overlay (`dev.asw.MacLaunchpad`, toggled on `Super+A`) with drag-to-reorder, app folders, animations, and persistent layout.
  - `mac-mail`: Three-pane IMAP/SMTP mail client (`dev.asw.MacMail`) with local caching, HTML sanitization, contact harvesting, OAuth2 support, and `dev.asw.AppMenu1` menus.
  - `mac-oauth`: Shared OAuth2 PKCE loopback sign-in helper used by `mac-suite` apps.
  - `mac-quicklook`: Floating file preview (`dev.asw.MacQuickLook`) for images, PDF, text/code, video posters and file info; single instance over `mac-quicklook.sock`, opened by Space in Files.
  - `mac-screenshot`: Screenshot and screen-recording overlay, toolbar, and floating thumbnail (`dev.asw.MacScreenshot`, `dev.asw.MacScreenshot.Recording`, `dev.asw.MacScreenshot.Thumbnail`, bound to `Super+Shift+S`).
  - `mac-theme`: CLI and GUI (`mac-theme --settings`) configuring COSMIC system accent color, corner radii, appearance mode/schedule, and per-display wallpaper.
  - `mac-widgets`: Shared settings, weather cache, and system info helpers used by `cosmic-ext-applet-clock-center`, `cosmic-ext-applet-sysmon`, and `mac-desktop-widgets`.
- **`overlays/cosmic-applets/`**: Authoritative source trees for 10 custom COSMIC applets/crates that get synced into `upstream-applets/`:
  - `cosmic-ext-applet-app-menu`: Top-panel active application name and live `File`/`Edit`/`View`/app menus via `dev.asw.AppMenu1` (falling back to window actions when an app does not export `dev.asw.AppMenu1`).
  - `cosmic-ext-applet-clipboard`: Menu bar clipboard history applet talking to `dev.asw.MacClipboard`.
  - `cosmic-ext-applet-clock-center`: Centered clock applet with unified calendar, weather, Up Next, Now Playing controls, and notifications popup (`dev.asw.CosmicExtAppletClockCenter`).
  - `cosmic-ext-applet-control-center`: macOS-style Control Center panel applet.
  - `cosmic-ext-applet-lord-menu`: System ("LordOS") menu applet (`dev.asw.CosmicExtAppletLordMenu`).
  - `cosmic-ext-applet-power-menu`: Right-side power menu applet backed by `logind-zbus`.
  - `cosmic-ext-applet-spotlight`: Spotlight search trigger pill applet in the top panel.
  - `cosmic-ext-applet-sysmon`: Top-panel system monitor applet (CPU, memory, swap, disk, GPU, battery; `dev.asw.CosmicExtAppletSysmon`).
  - `cosmic-ext-dock`: Standalone dock with hover magnification and launch bounce.
  - `cosmic-ext-mac-applet-style`: Shared menu bar pill styling, frosted popover container, and symbolic SVG icons for the overlay applets.
- **`patches/*.patch` & Working Copies**:
  - `./setup.sh` fetches upstream COSMIC repos into `cosmic-comp/`, `cosmic-launcher/`, `upstream-applets/`, `libcosmic-mac/`, `apps/cosmic-files/`, `apps/cosmic-edit/`, `apps/cosmic-viewer/`, and `apps/cosmic-workspaces/`, then applies:
    - `patches/cosmic-comp.patch` (genie minimize and fullscreen restore animations, compositor tweaks)
    - `patches/cosmic-files.patch` (`File`/`Edit`/`View`/`Sort` app menu export, Files toolbar/look, Space opens `mac-quicklook`)
    - `patches/cosmic-workspaces.patch` (Mission Control layout for the workspace overview; installed by `install-mission-control.sh`, `Super+Up`)
    - `patches/cosmic-edit.patch` (Notepad `dev.asw.AppMenu1` global menu export, editor look)
    - `patches/cosmic-viewer.patch` (Viewer `dev.asw.AppMenu1` global menu export, toolbar/look)
    - `patches/cosmic-launcher.patch` (Spotlight-style launcher overlay)
    - `patches/cosmic-applets.patch` (workspace wiring inside `upstream-applets/`)
    - `patches/libcosmic.patch` (shared macOS-style toolkit tweaks in `libcosmic-mac/`)
- **`presets/` & `bin/cosmic-preset`**:
  - `presets/macos-glass/` and `presets/macos-light/` store COSMIC configuration snapshots (`com.system76.CosmicPanel.*`, `CosmicTheme.*`, `CosmicFiles`, `CosmicComp`, `CosmicTk`, `CosmicTheme.Mode/v1/is_dark`) plus preset markers (`use-ext-dock`, `use-desktop-widgets`).
  - `bin/cosmic-preset` manages saving, applying, reverting (`undo`, `restore-full`), and syncing companion daemons (`dock-sync`, `widgets-sync`, `login-check`).
- **`tools/`**:
  - `tools/panel-layout.sh`: Slot-based reader/writer for `com.system76.CosmicPanel.Panel/v1` (`plugins_center`, `plugins_wings`, `--size`, `--font-size`).
  - `tools/export.sh`: Regenerates `patches/*.patch` from working copies, rsyncs `upstream-applets/` back into `overlays/cosmic-applets/`, and snapshots installed `presets/macos-glass/` + `bin/cosmic-preset`.
  - `tools/verify.sh`: Runs `bash -n` on all scripts/`PKGBUILD`, `tools/test-panel-layout.sh`, overlay sync `diff -ru -x target`, and preset sanity checks.
  - `tools/shortcut.sh`, `tools/set-default-apps.sh`: User shortcut and MIME default helpers.

## 2. Build, Test, Lint, and Install Commands

### Setup & Verification
```bash
./setup.sh                  # Fetch upstream COSMIC repos & apply patches (idempotent)
./tools/verify.sh           # Shell syntax, panel-layout tests, overlay sync, preset checks
```

### Cargo Check / Test / Clippy
To keep CPU and memory pressure bounded when building large COSMIC dependency graphs alongside `libcosmic-mac`, prefer checking/testing **one crate at a time** with `CARGO_BUILD_JOBS=2`:

```bash
# Single crate in apps/mac-suite (tests + strict clippy)
CARGO_BUILD_JOBS=2 cargo test --manifest-path apps/mac-suite/Cargo.toml -p <crate> --offline
CARGO_BUILD_JOBS=2 cargo clippy --manifest-path apps/mac-suite/Cargo.toml -p <crate> --all-targets --no-deps -- -D warnings

# Overlay applet crates in upstream-applets
CARGO_BUILD_JOBS=2 cargo test --manifest-path upstream-applets/Cargo.toml -p <applet-crate> --offline
CARGO_BUILD_JOBS=2 cargo clippy --manifest-path upstream-applets/Cargo.toml -p <applet-crate> --all-targets --no-deps -- -D warnings

# Patched COSMIC working copies
CARGO_BUILD_JOBS=2 cargo check --manifest-path cosmic-comp/Cargo.toml --offline
CARGO_BUILD_JOBS=2 cargo check --manifest-path apps/cosmic-edit/Cargo.toml --offline
CARGO_BUILD_JOBS=2 cargo check --manifest-path apps/cosmic-files/Cargo.toml --offline
CARGO_BUILD_JOBS=2 cargo check --manifest-path apps/cosmic-viewer/Cargo.toml --offline
```

### Exporting Patches & Overlays
After editing any working copy (`cosmic-comp/`, `apps/cosmic-*/`, `cosmic-launcher/`, `libcosmic-mac/`, or `upstream-applets/`), run:
```bash
./tools/export.sh
```
Note: `tools/export.sh` also snapshots `~/.config/cosmic-presets/macos-glass/` and `~/.local/bin/cosmic-preset` if present on the machine. Always inspect `git status` and `git diff` after running `./tools/export.sh` so un-updated host files do not overwrite intentional repo changes to `bin/cosmic-preset` or `presets/`.

### Installing Components
```bash
./install.sh --list         # List available components
./install.sh --check        # Verify required build/runtime tools
./install.sh all            # Build & install all user components
./install.sh --uninstall    # Remove installed user components
```
Standalone sub-installers also exist for targeted installation (`install-panel.sh`, `install-clock-center.sh`, `install-dock.sh`, `install-spotlight.sh`, `install-genie-comp.sh`, `install-clipboard.sh`, `install-theme.sh`, `install-apps.sh`, `install-extras.sh`). **Do not run `./install.sh` during automated verification unless explicitly asked**, as it overwrites live `~/.local` and `~/.config` state.

## 3. D-Bus Protocols & Config/Data Paths

### Session D-Bus Services
- **`dev.asw.AppMenu1`** (exported on `dev.asw.AppMenu.<sanitized_app_id>` at object path `/dev/asw/AppMenu`):
  - Methods: `GetLayout() -> a(ssa(sssbb))`, `Activate(s item_id)`
  - Signal: `LayoutChanged()`
  - Consumed by `cosmic-ext-applet-app-menu`; exported by `mac-mail`, `mac-calendar`, `cosmic-files`, `cosmic-edit`, and `cosmic-viewer`.
- **`dev.asw.MacClipboard`** & **`dev.asw.MacClipboard.Overlay`**:
  - Background clipboard history daemon (`mac-clipboard daemon`), quick-paste overlay (`dev.asw.MacClipboard.Overlay`), CLI client, and panel applet (`cosmic-ext-applet-clipboard`).

### Config, Data, Cache & Runtime Paths
Respect `$XDG_CONFIG_HOME`, `$XDG_DATA_HOME`, `$XDG_CACHE_HOME`, and `$XDG_RUNTIME_DIR` when present:
- `~/.config/mac-suite/`: `appearance.conf`, `location.conf`, `widgets.conf`, `screenshot.conf`, `desktop-widgets.ron`
- `~/.config/mac-launchpad/layout.ron`: persistent Launchpad grid/folder layout
- `~/.config/mac-mail/accounts.json`: multi-account IMAP/SMTP configuration
- `~/.config/cosmic/` & `~/.config/cosmic-presets/`: live COSMIC config and `original` / `macos-glass` / `macos-light` presets
- `~/.local/bin/`: installed user binaries (`mac-*`, `cosmic-ext-*`, `cosmic-preset`, and PATH wrappers for `cosmic-comp` / `cosmic-launcher`)
- `~/.local/share/applications/`: `dev.asw.*.desktop` entries
- `~/.local/share/mac-clipboard/`, `~/.local/share/mac-calendar/`, `~/.local/share/mac-mail/`: persisted app data (history, `events.ics`, `contacts.tsv`)
- `~/.cache/mac-suite/` & `$XDG_CACHE_HOME/mac-mail/<account-key>/`: IPC socket fallback (`mac-launchpad.sock`, `mac-desktop-widgets.sock`), temporary screenshot/output cache, and IMAP offline cache
- `~/.config/systemd/user/mac-appearance.service` & `~/.config/autostart/`: `dev.asw.MacLaunchpad.desktop`, `dev.asw.MacDesktopWidgets.desktop`

## 4. Strict Repository Rules

1. **Always synchronize patches and overlays**: After modifying any upstream working copy (`upstream-applets/`, `cosmic-comp/`, `apps/cosmic-*/`, `cosmic-launcher/`, `libcosmic-mac/`), run `./tools/export.sh` and verify with `./tools/verify.sh`. Keep `overlays/cosmic-applets/*` identical to `upstream-applets/*` (excluding `target`).
2. **Use `tools/panel-layout.sh` for panel changes**: Never hand-edit `plugins_center`, `plugins_wings`, panel size, or font size directly across preset files; use `tools/panel-layout.sh`.
3. **Never touch `/usr`**: All binaries, wrappers, `.desktop` files, systemd units, and configs live strictly inside `$HOME` (`~/.local` and `~/.config`).
4. **Never push or deploy without explicit instruction**: Do not run `git push`, create PRs, or trigger deployments unless the user explicitly instructs you to do so. Keep commits verified and tests/clippy green first.
