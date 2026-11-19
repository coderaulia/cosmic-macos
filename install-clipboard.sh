#!/usr/bin/env bash
# Build and install mac-clipboard (daemon + quick-paste window) and
# cosmic-ext-applet-clipboard (panel applet), register D-Bus activation,
# autostart, and Super+V shortcut, and assign the clipboard slot in macos-glass.
# Undo with: ./install-clipboard.sh --uninstall
set -euo pipefail

APP_ID="dev.asw.CosmicExtAppletClipboard"
APPLET_BIN="cosmic-ext-applet-clipboard"
DAEMON_BIN="mac-clipboard"
DBUS_NAME="dev.asw.MacClipboard"

ROOT="$(cd "$(dirname "$0")" && pwd)"
APPLET_WORKSPACE="$ROOT/upstream-applets"
SUITE_DIR="$ROOT/apps/mac-suite"
BIN_DIR="$HOME/.local/bin"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
APPS_DIR="$DATA_HOME/applications"
DBUS_DIR="$DATA_HOME/dbus-1/services"
AUTOSTART_DIR="$CONFIG_HOME/autostart"
SHORTCUTS_FILE="$CONFIG_HOME/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom"
PRESET="$CONFIG_HOME/cosmic-presets/macos-glass/com.system76.CosmicPanel.Panel/v1"
STORE_DIR="$DATA_HOME/mac-clipboard"

add_shortcut() {
  mkdir -p "$(dirname "$SHORTCUTS_FILE")"
  if [ ! -f "$SHORTCUTS_FILE" ] || [ ! -s "$SHORTCUTS_FILE" ]; then
    cat >"$SHORTCUTS_FILE" <<EOF
{
    (
        modifiers: [
            Super,
        ],
        key: "v",
    ): Spawn("$DAEMON_BIN toggle"),
}
EOF
    return
  fi
  if grep -Fq "Spawn(\"$DAEMON_BIN toggle\")" "$SHORTCUTS_FILE"; then
    return
  fi
  python3 - "$SHORTCUTS_FILE" "$DAEMON_BIN toggle" <<'PY'
import sys, pathlib

path = pathlib.Path(sys.argv[1])
cmd = sys.argv[2]
text = path.read_text()
entry = (
    '    (\n'
    '        modifiers: [\n'
    '            Super,\n'
    '        ],\n'
    '        key: "v",\n'
    f'    ): Spawn("{cmd}"),\n'
)
idx = text.rfind('}')
if idx == -1:
    sys.exit(1)
prefix = text[:idx]
if prefix.rstrip().endswith('{'):
    new_text = prefix + "\n" + entry + text[idx:]
else:
    if not prefix.rstrip().endswith(','):
        prefix = prefix.rstrip() + ",\n"
    elif not prefix.endswith('\n'):
        prefix = prefix + "\n"
    new_text = prefix + entry + text[idx:]
path.write_text(new_text)
PY
}

remove_shortcut() {
  [ -f "$SHORTCUTS_FILE" ] || return 0
  grep -Fq "Spawn(\"$DAEMON_BIN toggle\")" "$SHORTCUTS_FILE" || return 0
  python3 - "$SHORTCUTS_FILE" "$DAEMON_BIN toggle" <<'PY'
import re, sys, pathlib

path = pathlib.Path(sys.argv[1])
cmd = re.escape(sys.argv[2])
text = path.read_text()
pattern = re.compile(
    r'\n?[ \t]*\(\s*modifiers:\s*\[[^\]]*\],\s*key:\s*"[^"]+",\s*\):\s*Spawn\("' + cmd + r'"\),?[ \t]*\n?',
    re.MULTILINE,
)
new_text = pattern.sub('\n', text)
path.write_text(new_text)
PY
}

if [ "${1:-}" = "--uninstall" ]; then
  pkill -x "$DAEMON_BIN" || true
  pkill -f "(^|/)$APPLET_BIN( |$)" || true
  rm -f \
    "$BIN_DIR/$DAEMON_BIN" \
    "$BIN_DIR/$APPLET_BIN" \
    "$APPS_DIR/$APP_ID.desktop" \
    "$DBUS_DIR/$DBUS_NAME.service" \
    "$AUTOSTART_DIR/$DAEMON_BIN.desktop"
  remove_shortcut
  if [ -d "$PRESET" ]; then
    "$ROOT/tools/panel-layout.sh" --preset "$PRESET" --set clipboard=io.github.k33wee.clippy-land
  fi
  if [ -d "$STORE_DIR" ]; then
    if [ -t 0 ]; then
      read -r -p "Remove clipboard history at $STORE_DIR? [y/N] " ans
      case "$ans" in
        [yY]|[yY][eE][sS]) rm -rf "$STORE_DIR" ;;
      esac
    else
      echo "kept clipboard history at $STORE_DIR"
    fi
  fi
  echo "uninstalled; run: cosmic-preset apply macos-glass"
  exit 0
fi

[ -d "$PRESET" ] || { echo "macos-glass preset not found at $PRESET" >&2; exit 1; }

export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export CARGO_PROFILE_RELEASE_INCREMENTAL=true
if command -v mold >/dev/null && command -v clang >/dev/null; then
  export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=clang
  export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-fuse-ld=mold"
fi

# 1. Build mac-clipboard daemon + quick-paste UI
export CARGO_TARGET_DIR="$ROOT/apps/.target"
(cd "$SUITE_DIR" && cargo build --release -p "$DAEMON_BIN")
[ "$CARGO_TARGET_DIR/release/$DAEMON_BIN" -nt "$0" ] || [ -n "$(find "$CARGO_TARGET_DIR/release/$DAEMON_BIN" -mmin -30 2>/dev/null)" ] || { echo "the build output for $DAEMON_BIN is old; the build may have gone elsewhere" >&2; exit 1; }
install -Dm755 "$CARGO_TARGET_DIR/release/$DAEMON_BIN" "$BIN_DIR/$DAEMON_BIN"

# 2. Build panel applet
export CARGO_TARGET_DIR="$APPLET_WORKSPACE/target"
(cd "$APPLET_WORKSPACE" && cargo build --release -p "$APPLET_BIN")
[ "$CARGO_TARGET_DIR/release/$APPLET_BIN" -nt "$0" ] || [ -n "$(find "$CARGO_TARGET_DIR/release/$APPLET_BIN" -mmin -30 2>/dev/null)" ] || { echo "the build output for $APPLET_BIN is old; the build may have gone elsewhere" >&2; exit 1; }
install -Dm755 "$CARGO_TARGET_DIR/release/$APPLET_BIN" "$BIN_DIR/$APPLET_BIN"

# 3. Desktop entry (absolute Exec path)
mkdir -p "$APPS_DIR"
sed "s|^Exec=.*|Exec=$BIN_DIR/$APPLET_BIN|" \
  "$APPLET_WORKSPACE/$APPLET_BIN/data/$APP_ID.desktop" >"$APPS_DIR/$APP_ID.desktop"

# 4. D-Bus user service activation & autostart
mkdir -p "$DBUS_DIR" "$AUTOSTART_DIR"
cat >"$DBUS_DIR/$DBUS_NAME.service" <<EOF
[D-BUS Service]
Name=$DBUS_NAME
Exec=$BIN_DIR/$DAEMON_BIN daemon
EOF

cat >"$AUTOSTART_DIR/$DAEMON_BIN.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Mac Clipboard Daemon
Comment=Clipboard history daemon for COSMIC
Exec=$BIN_DIR/$DAEMON_BIN daemon
NoDisplay=true
EOF

# 5. Merge Super+V shortcut and assign panel clipboard slot
add_shortcut
"$ROOT/tools/panel-layout.sh" --preset "$PRESET" --set clipboard="$APP_ID"

echo "installed; run: cosmic-preset apply macos-glass"
