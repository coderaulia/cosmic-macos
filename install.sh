#!/usr/bin/env bash
# Install the macOS-style COSMIC desktop pieces, all at once or one by one.
#
#   ./install.sh --list              show the components
#   ./install.sh --check             check the tools the builds need
#   ./install.sh all                 everything
#   ./install.sh dock files mail     only these
#   ./install.sh --uninstall         remove what was installed
#
# Upstream sources are fetched on first use (see setup.sh). Nothing in /usr is
# changed; everything goes under ~/.local and ~/.config.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

ALL=(preset dock panel clock clipboard spotlight files notepad viewer calendar mail theme comp)

describe() {
  case "$1" in
    preset)    echo "cosmic-preset: switch between your original desktop and the macOS preset" ;;
    dock)      echo "magnifying dock with previews, genie targets and drag-to-reorder" ;;
    panel)     echo "macOS-style top menu bar applets (lord menu, app menu, spotlight, control center)" ;;
    clock)     echo "clock applet with calendar events, now playing and notifications" ;;
    clipboard) echo "clipboard history daemon, quick-paste window (Super+V) and panel applet" ;;
    spotlight) echo "Spotlight-style launcher on the Super key" ;;
    files)     echo "Files with tree list, column view and Finder-style sidebar" ;;
    notepad)   echo "Notepad as a searchable Notes app" ;;
    viewer)    echo "Image viewer with a Preview-style toolbar" ;;
    calendar)  echo "Calendar with CalDAV sync" ;;
    mail)      echo "Mail for any IMAP/SMTP account" ;;
    theme)     echo "macOS blue accent and rounder corners (part of the preset)" ;;
    comp)      echo "compositor with the genie minimize animation (needs a re-login)" ;;
  esac
}

# Which upstream projects a component is built from.
needs() {
  case "$1" in
    dock|panel|clock) echo applets ;;
    clipboard) echo "libcosmic applets" ;;
    spotlight) echo launcher ;;
    comp) echo comp ;;
    files) echo "libcosmic files" ;;
    notepad) echo "libcosmic edit" ;;
    viewer) echo "libcosmic viewer" ;;
    calendar|mail|theme) echo libcosmic ;;
    preset) echo "" ;;
  esac
}

check() {
  local missing=0
  for tool in git cargo rustup rsync secret-tool clang mold nasm pkg-config; do
    if command -v "$tool" >/dev/null; then
      printf '  ok       %s\n' "$tool"
    else
      printf '  MISSING  %s\n' "$tool"
      missing=1
    fi
  done
  echo
  echo "On Arch: sudo pacman -S --needed git rustup rsync libsecret clang mold nasm pkgconf"
  echo "Optional icon theme for the macOS preset: WhiteSur icon theme."
  return $missing
}

install_preset() {
  mkdir -p "$HOME/.local/bin" "$HOME/.config/cosmic-presets"
  install -Dm755 bin/cosmic-preset "$HOME/.local/bin/cosmic-preset"
  local store="$HOME/.config/cosmic-presets"
  # Keep today's desktop as the preset to go back to.
  if [ ! -d "$store/original" ]; then
    "$HOME/.local/bin/cosmic-preset" save original
    [ -d "$HOME/.config/cosmic" ] && [ ! -d "$store/.full-backup" ] && cp -a "$HOME/.config/cosmic" "$store/.full-backup"
  fi
  [ -d "$store/macos-glass" ] || cp -a presets/macos-glass "$store/macos-glass"
  echo "installed cosmic-preset; presets: original, macos-glass"
}

run() {
  local component="$1"
  echo
  echo "=== $component: $(describe "$component")"
  for project in $(needs "$component"); do ./setup.sh "$project"; done
  case "$component" in
    preset)    install_preset ;;
    dock)      ./install-dock.sh ;;
    panel)     ./install-panel.sh ;;
    clock)     ./install-clock-center.sh ;;
    clipboard) ./install-clipboard.sh ;;
    spotlight) ./install-spotlight.sh ;;
    comp)      ./install-genie-comp.sh ;;
    theme)     ./install-theme.sh ;;
    files|notepad|viewer|calendar|mail) ./install-apps.sh "$component" ;;
  esac
}

case "${1:-}" in
  ""|-h|--help)
    sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
    ;;
  --list)
    for component in "${ALL[@]}"; do printf '  %-10s %s\n' "$component" "$(describe "$component")"; done
    ;;
  --check)
    check
    ;;
  --uninstall)
    ./install-dock.sh --uninstall || true
    ./install-panel.sh --uninstall || true
    ./install-clock-center.sh --uninstall || true
    ./install-clipboard.sh --uninstall || true
    ./install-spotlight.sh --uninstall || true
    ./install-genie-comp.sh --uninstall || true
    ./install-apps.sh --uninstall || true
    echo
    echo "Switch the look back with: cosmic-preset revert"
    ;;
  *)
    components=("$@")
    [ "${components[0]}" = all ] && components=("${ALL[@]}")
    # The preset must exist before the pieces that write into it.
    [ -d "$HOME/.config/cosmic-presets/original" ] || { [ "${components[0]}" = preset ] || components=(preset "${components[@]}"); }
    for component in "${components[@]}"; do
      case " ${ALL[*]} " in
        *" $component "*) run "$component" ;;
        *) echo "unknown component: $component (see ./install.sh --list)" >&2; exit 1 ;;
      esac
    done
    echo
    echo "Done. Apply the macOS look with:  cosmic-preset apply macos-glass"
    echo "Switch back at any time with:     cosmic-preset revert"
    ;;
esac
