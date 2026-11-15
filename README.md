# cosmic-macos

A macOS-style desktop for [COSMIC](https://system76.com/cosmic) (Epoch 1.11): a
magnifying dock, a Spotlight-style launcher, a genie minimize animation, and
Finder-, Preview-, Notes-, Calendar- and Mail-style apps. Everything installs
next to the stock COSMIC apps (own app ids, own settings) under `~/.local`, and
one command switches the look back.

## Install

```bash
./install.sh --check        # which build tools are missing
./install.sh all            # everything, or pick pieces:
./install.sh dock files mail
cosmic-preset apply macos-glass
```

Components (`./install.sh --list`):

| Component | What it is |
|---|---|
| `preset` | `cosmic-preset`: switch between your original desktop and the macOS preset |
| `dock` | Magnifying dock: bounce, slide-hide, window previews on hover, drag to reorder (after "Edit Dock…"), right-click menu, trash |
| `panel` | macOS-style top menu bar applets (lord menu, active app menu, workspaces, resource monitor, Spotlight button, Control Center, power menu) |
| `clock` | Panel clock with a calendar that shows your events, now playing, and notifications |
| `clipboard` | Clipboard history daemon, quick-paste window (`Super+V`), and panel applet |
| `spotlight` | Spotlight-style launcher on the Super key, with result headings and a details pane |
| `files` | Files with an inline tree, Finder-style columns with preview, Quick Look on Space |
| `notepad` | Notepad as a searchable Notes app (notes in `~/Documents/Notes`, saved automatically) |
| `viewer` | Image viewer with a Preview-style toolbar |
| `calendar` | Calendar (day/week/month) with multiple calendars, recurring events, `.ics` import/export, search, reminders, CalDAV sync and Google Calendar sign-in |
| `mail` | Mail with native HTML rendering, multi-window compose, conversation threading, multiple accounts, OAuth2/password sign-in (secrets in the keyring), local cache and IMAP IDLE |
| `theme` | macOS blue accent and rounder corners, stored in the preset |
| `launchpad` | Launchpad: full-screen app grid with search and pages (`Super+A`) |
| `screenshot` | Screenshot tool: region, window or whole screen, floating thumbnail (`Super+Shift+S`) |
| `appearance` | Automatic light/dark mode (sunset/sunrise or schedule) and light/dark or dynamic wallpaper; settings in the Appearance app |
| `widgets` | Weather (Open-Meteo), Up Next and system cards in the clock popup |
| `comp` | Compositor with the genie minimize/restore animation (log out and in afterwards) |

Each piece builds on its own. `./install.sh files` fetches only what Files
needs. A stock-COSMIC app that a piece replaces (Files, Text Editor, Viewer) is
hidden from the launcher by an override in `~/.local/share/applications`; delete
that file to bring it back.

### Going back

```bash
cosmic-preset revert        # original look, original theme, stock dock
./install.sh --uninstall    # remove everything that was installed
```

The compositor installs to `~/.local/lib` and is chosen by a launcher in
`~/.local/bin`. If it crashes twice within 90 seconds the session falls back to
the stock compositor by itself. `./install-genie-comp.sh --disable` also does it.

## How the repository is organised

Our own code is kept as plain source. Changes to upstream COSMIC projects are
kept as small patches against the release tag, so the repository stays small and
the upstream history is not copied.

| Path | Contents |
|---|---|
| `apps/mac-suite/` | Calendar, Mail, Clipboard, and the theme tool (our own Rust workspace) |
| `overlays/cosmic-applets/` | Our custom applets (`cosmic-ext-dock`, `cosmic-ext-mac-applet-style`, and the panel/clipboard applets) |
| `patches/*.patch` | Our changes to cosmic-files, cosmic-edit, cosmic-viewer, cosmic-launcher, cosmic-comp, cosmic-applets, and libcosmic |
| `presets/macos-glass/` | The macOS preset for `cosmic-preset` |
| `bin/cosmic-preset` | The preset switcher |
| `setup.sh` | Clones the upstream projects at the tag and applies the patches |
| `install-*.sh`, `install.sh` | Build and install scripts |
| `tools/export.sh` | Regenerates `patches/`, `overlays/` and `presets/` from the working copies |

`setup.sh` creates the working copies (`apps/cosmic-*`, `cosmic-comp`,
`upstream-applets`, `libcosmic-mac`), which are not tracked. Window buttons on the
left come from a patched copy of libcosmic (`libcosmic-mac`) used only by the
macOS-style apps.

## Working on it

Edit in the working copies, build with the install scripts, then:

```bash
./tools/export.sh           # turn the working copies back into patches
git add -A && git commit
```

To move to a new COSMIC release, change `TAG` and `LIBCOSMIC_REV` in `setup.sh`,
run it in a clean directory, and fix the patches that no longer apply.

## Requirements

Rust (via `rustup`), `git`, `rsync`, `libsecret` (`secret-tool`), `clang` and
`mold` for fast links, `nasm` for the image viewer, and COSMIC 1.11. Optionally
install the WhiteSur icon theme for macOS-style icons. Missing applets are
reported by `cosmic-preset apply`.

## Roadmap

The Mail, Calendar, Clipboard Manager, and Panel Toolbar work is complete: Mail
now includes sanitized native HTML rendering, multi-window compose, conversation
threading, multiple accounts with a unified inbox, OAuth2 PKCE sign-in,
`mailto:` handling, and a persistent IMAP connection with local cache and IDLE
alerts (see [docs/mail-rework-plan.md](docs/mail-rework-plan.md)); Calendar
includes rich events (`LOCATION`, `DESCRIPTION`, `VALARM`), `RRULE` recurrence,
multiple calendars, `.ics` import/export, an event inspector, search, and
reminder notifications; Clipboard adds a Wayland history daemon, Spotlight-style
quick-paste window (`Super+V`), and panel popover (see
[docs/clipboard-manager-plan.md](docs/clipboard-manager-plan.md)); and the
Panel Toolbar provides a macOS-style top bar with Lord Menu, App Menu,
centered Clock Center, workspace indicator, system stats (CPU, memory, swap, disk, GPU), Spotlight button, Control Center, and a power menu (see
[docs/panel-toolbar-plan.md](docs/panel-toolbar-plan.md)).

## Troubleshooting

- **An installed piece does not change after a rebuild.** A `target-dir` set in
  `~/.cargo/config.toml` sends build output elsewhere. The install scripts set
  their own build folder and stop if the binary they would copy is not fresh,
  but check that setting if you build by hand.
- **The clock applet is missing after login.** The panel can start before the
  notification daemon is ready. A login check restarts the panel once; you can
  also run `cosmic-preset reload`.
- **Mail says "Invalid credentials" for Gmail or Google Workspace.** Use browser
  OAuth2 sign-in (with an OAuth client ID) or an app password (2-Step
  Verification must be on and the admin must allow it), not your normal
  password.

## Limits

- Browser OAuth2 sign-in for Gmail and Outlook/Microsoft 365 requires providing
  your own OAuth client ID in the sign-in screen (or via environment variable);
  otherwise use an app password.
- Heavy marketing HTML emails render simplified in Mail's native view; use
  "Open in browser" for complex layouts.
- The genie animation covers floating, tiled and fullscreen windows when minimizing; restoring a fullscreen window has no genie yet.
- After a `cosmic-comp` package update, run `./install.sh comp` again.
- Calendar sync works with servers that accept a password login (Nextcloud,
  Radicale, Baikal, Fastmail, iCloud with an app password) and with Google
  through "Sign in with Google". Google needs your own Desktop OAuth client ID
  with the CalDAV API enabled (and `MAC_MAIL_GMAIL_CLIENT_SECRET` if Google asks
  for a secret). Microsoft calendars need the Graph API and are not supported.

- Launchpad, Screenshot, Appearance and the clock widgets are new and have not
  been tried on every setup yet: Launchpad has no folders or drag-to-reorder,
  Screenshot has no screen recording, Appearance only sets the wallpaper for all
  screens together, and weather needs a location set in the Appearance app.

## License

GPL-3.0-only for our code. The patched projects keep their own licenses
(GPL-3.0 for the COSMIC applications, MPL-2.0 for libcosmic).
