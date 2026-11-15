#!/usr/bin/env bash
# Build and install the extra macOS-style tools for the current user:
#   launchpad   full-screen app grid on Super+A
#   screenshot  capture tool on Super+Shift+S
#   appearance  automatic light/dark mode and wallpaper (settings: mac-theme --settings)
#   widgets     weather, Up Next and system cards in the clock popup
#   desktop-widgets  macOS Sonoma-style desktop widgets on the wallpaper
#
# Usage: ./install-extras.sh [launchpad|screenshot|appearance|widgets|desktop-widgets]...
#        ./install-extras.sh --uninstall
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
SUITE="$ROOT/apps/mac-suite"
BIN_DIR="$HOME/.local/bin"
APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
AUTOSTART="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
PRESETS="${XDG_CONFIG_HOME:-$HOME/.config}/cosmic-presets"
ALL=(launchpad screenshot appearance widgets desktop-widgets)

if [ "${1:-}" = "--uninstall" ]; then
  "$ROOT/tools/shortcut.sh" unset Super a || true
  "$ROOT/tools/shortcut.sh" unset "Super,Shift" s || true
  systemctl --user disable --now mac-appearance.service 2>/dev/null || true
  pkill -x mac-launchpad || true
  pkill -x mac-desktop-widgets || true
  rm -f "$BIN_DIR"/mac-{launchpad,screenshot,appearance,desktop-widgets} \
    "$APPS_DIR"/dev.asw.Mac{Launchpad,Screenshot,Theme}.desktop \
    "$UNIT_DIR/mac-appearance.service" "$AUTOSTART/dev.asw.MacLaunchpad.desktop" \
    "$AUTOSTART/dev.asw.MacDesktopWidgets.desktop" \
    "$PRESETS/macos-glass/use-desktop-widgets" "$PRESETS/macos-light/use-desktop-widgets"
  systemctl --user daemon-reload 2>/dev/null || true
  echo "uninstalled (the clock widgets stay until: ./install-clock-center.sh)"
  exit 0
fi

selected=("$@")
[ ${#selected[@]} -gt 0 ] || selected=("${ALL[@]}")

export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export CARGO_PROFILE_RELEASE_INCREMENTAL=true
if command -v mold >/dev/null && command -v clang >/dev/null; then
  export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=clang
  export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-fuse-ld=mold"
fi
export CARGO_TARGET_DIR="$ROOT/apps/.target"
mkdir -p "$BIN_DIR" "$APPS_DIR"

build() { (cd "$SUITE" && cargo build --release -p "$1"); install -Dm755 "$CARGO_TARGET_DIR/release/$1" "$BIN_DIR/$1"; }
desktop() { sed "s|^Exec=\([^ ]*\)|Exec=$BIN_DIR/\1|" "$1" >"$APPS_DIR/$(basename "$1")"; }

for name in "${selected[@]}"; do
  echo "==> $name"
  case "$name" in
    launchpad)
      build mac-launchpad
      desktop "$SUITE/mac-launchpad/data/dev.asw.MacLaunchpad.desktop"
      # Start hidden at login so the first Super+A opens instantly.
      mkdir -p "$AUTOSTART"
      printf '[Desktop Entry]\nType=Application\nName=Launchpad daemon\nExec=%s --daemon\nNoDisplay=true\n' \
        "$BIN_DIR/mac-launchpad" >"$AUTOSTART/dev.asw.MacLaunchpad.desktop"
      "$ROOT/tools/shortcut.sh" set Super a "Spawn(\"$BIN_DIR/mac-launchpad\")"
      pgrep -x mac-launchpad >/dev/null || setsid -f "$BIN_DIR/mac-launchpad" --daemon >/dev/null 2>&1
      ;;
    screenshot)
      build mac-screenshot
      desktop "$SUITE/mac-screenshot/data/dev.asw.MacScreenshot.desktop"
      "$ROOT/tools/shortcut.sh" set "Super,Shift" s "Spawn(\"$BIN_DIR/mac-screenshot\")"
      ;;
    appearance)
      build mac-appearance
      build mac-theme
      install -Dm644 "$SUITE/mac-appearance/data/mac-appearance.service" "$UNIT_DIR/mac-appearance.service"
      systemctl --user daemon-reload
      cat >"$APPS_DIR/dev.asw.MacTheme.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Appearance
Comment=Light and dark mode, schedule, location and wallpaper
Exec=$BIN_DIR/mac-theme --settings
Icon=preferences-desktop-theme
Terminal=false
StartupWMClass=dev.asw.MacTheme
Categories=Settings;
EOF
      echo "    turn on automatic switching in Appearance (mac-theme --settings)"
      ;;
    widgets)
      "$ROOT/install-clock-center.sh"
      ;;
    desktop-widgets)
      build mac-desktop-widgets
      mkdir -p "$AUTOSTART"
      printf '[Desktop Entry]\nType=Application\nName=Desktop Widgets\nExec=sh -c "%s/cosmic-preset widgets-sync"\nNoDisplay=true\n' \
        "$BIN_DIR" >"$AUTOSTART/dev.asw.MacDesktopWidgets.desktop"
      [ -d "$PRESETS/macos-glass" ] && touch "$PRESETS/macos-glass/use-desktop-widgets"
      [ -d "$PRESETS/macos-light" ] && touch "$PRESETS/macos-light/use-desktop-widgets"
      if [ -x "$BIN_DIR/cosmic-preset" ]; then
        "$BIN_DIR/cosmic-preset" widgets-sync || true
      fi
      ;;
    *) echo "unknown: $name (choose from: ${ALL[*]})" >&2; exit 1 ;;
  esac
done
echo "done"
