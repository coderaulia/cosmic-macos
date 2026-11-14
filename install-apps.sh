#!/usr/bin/env bash
# Build and install the macOS-style apps for the current user. They install
# alongside the stock COSMIC apps (own app ids, own config) under ~/.local.
#
# Usage:
#   ./install-apps.sh                  build and install all apps
#   ./install-apps.sh files mail       only the named apps
#   ./install-apps.sh --uninstall      remove all of them
#
# Apps: files notepad viewer calendar mail
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)/apps"
BIN_DIR="$HOME/.local/bin"
APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ALL=(files notepad viewer calendar mail)

# name | source dir | cargo package | built binary | app id | label | icon | mime | comment
app_info() {
  case "$1" in
    files) echo "cosmic-files|cosmic-files|cosmic-files|dev.asw.MacFiles|Files|com.system76.CosmicFiles|stock:com.system76.CosmicFiles|Browse files and folders" ;;
    notepad) echo "cosmic-edit|cosmic-edit|cosmic-edit|dev.asw.MacNotepad|Notepad|com.system76.CosmicEdit|stock:com.system76.CosmicEdit|Write and edit text" ;;
    viewer) echo "cosmic-viewer|cosmic-viewer|cosmic-viewer|dev.asw.MacViewer|Image Viewer|com.system76.CosmicViewer|stock:com.system76.CosmicViewer|View images" ;;
    calendar) echo "mac-suite|mac-calendar|mac-calendar|dev.asw.MacCalendar|Calendar|x-office-calendar|text/calendar;application/ics;x-scheme-handler/webcal;x-scheme-handler/webcals;|Day, week and month calendar" ;;
    mail) echo "mac-suite|mac-mail|mac-mail|dev.asw.MacMail|Mail|internet-mail|x-scheme-handler/mailto;|Read and send email" ;;
    *) return 1 ;;
  esac
}

if [ "${1:-}" = "--uninstall" ]; then
  for name in "${ALL[@]}"; do
    IFS='|' read -r _ _ _ id _ _ _ _ <<<"$(app_info "$name")"
    rm -f "$BIN_DIR/mac-$name" "$APPS_DIR/$id.desktop"
  done
  echo "uninstalled"
  exit 0
fi

selected=("$@")
[ ${#selected[@]} -gt 0 ] || selected=("${ALL[@]}")

# Fast builds: upstream release profiles use LTO, which makes every rebuild
# slow. Override it from the environment instead of editing each project.
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export CARGO_PROFILE_RELEASE_INCREMENTAL=true
if command -v mold >/dev/null && command -v clang >/dev/null; then
  export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=clang
  export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-fuse-ld=mold"
fi

# One build folder for all apps: libraries they share compile only once.
export CARGO_TARGET_DIR="$ROOT/.target"

mkdir -p "$BIN_DIR" "$APPS_DIR"
failed=()
for name in "${selected[@]}"; do
  info="$(app_info "$name")" || { echo "unknown app: $name (choose from: ${ALL[*]})" >&2; exit 1; }
  IFS='|' read -r dir package binary id label icon mime comment <<<"$info"

  echo "==> building $label"
  # Keep going if one app fails, and report it at the end.
  if ! (cd "$ROOT/$dir" && cargo build --release -p "$package"); then
    failed+=("$name")
    continue
  fi

  # "stock:<id>": open the same file types as the stock app this replaces.
  if [[ "$mime" == stock:* ]]; then
    mime="$(sed -n 's/^MimeType=//p' "/usr/share/applications/${mime#stock:}.desktop" | head -1)"
  fi

  install -Dm755 "$CARGO_TARGET_DIR/release/$binary" "$BIN_DIR/mac-$name"
  cat >"$APPS_DIR/$id.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=$label
Comment=$comment
Exec=$BIN_DIR/mac-$name %U
Icon=$icon
Terminal=false
StartupNotify=true
StartupWMClass=$id
Categories=Utility;
MimeType=$mime
EOF
  # Make it the default for the file types it handles.
  "$ROOT/../tools/set-default-apps.sh" "$id.desktop" "$mime" || true
  echo "    installed $label as mac-$name"
done

command -v update-desktop-database >/dev/null && update-desktop-database "$APPS_DIR" 2>/dev/null || true
if [ ${#failed[@]} -gt 0 ]; then
  echo "failed: ${failed[*]}" >&2
  exit 1
fi
echo "done"
