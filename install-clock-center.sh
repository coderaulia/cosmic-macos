#!/usr/bin/env bash
# Build and install the Clock Center applet for the current user, then point
# the macos-glass preset at it. Undo with: ./install-clock-center.sh --uninstall
set -euo pipefail

APP_ID="dev.asw.CosmicExtAppletClockCenter"
BIN_NAME="cosmic-ext-applet-clock-center"
ROOT="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE="$ROOT/upstream-applets"
BIN_DIR="$HOME/.local/bin"
APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
PRESET="${XDG_CONFIG_HOME:-$HOME/.config}/cosmic-presets/macos-glass/com.system76.CosmicPanel.Panel/v1"

write_panel_layout() {
  # $1 = applet shown in the panel center
  # $2 = "yes" to keep the stock notifications applet in the right wing
  local center="$1" notifications="$2"
  "$ROOT/tools/panel-layout.sh" --preset "$PRESET" \
    --set "clock=$center" \
    --set "notifications=$notifications"
}

[ -d "$PRESET" ] || { echo "macos-glass preset not found at $PRESET" >&2; exit 1; }

if [ "${1:-}" = "--uninstall" ]; then
  write_panel_layout "com.system76.CosmicAppletTime" yes
  rm -f "$BIN_DIR/$BIN_NAME" "$APPS_DIR/$APP_ID.desktop"
  echo "uninstalled; run: cosmic-preset apply macos-glass"
  exit 0
fi

# Build into this project's own folder, whatever ~/.cargo/config.toml says.
export CARGO_TARGET_DIR="$WORKSPACE/target"
(cd "$WORKSPACE" && cargo build --release -p "$BIN_NAME")

[ "$WORKSPACE/target/release/$BIN_NAME" -nt "$0" ] || [ -n "$(find "$WORKSPACE/target/release/$BIN_NAME" -mmin -30 2>/dev/null)" ] || { echo "the build output is old; the build may have gone elsewhere" >&2; exit 1; }
install -Dm755 "$WORKSPACE/target/release/$BIN_NAME" "$BIN_DIR/$BIN_NAME"
# The panel may not have ~/.local/bin in PATH, so use an absolute Exec path.
mkdir -p "$APPS_DIR"
sed "s|^Exec=.*|Exec=$BIN_DIR/$BIN_NAME|" \
  "$WORKSPACE/$BIN_NAME/data/$APP_ID.desktop" >"$APPS_DIR/$APP_ID.desktop"

# The clock center replaces both the stock clock and the notifications applet.
write_panel_layout "$APP_ID" no

echo "installed; run: cosmic-preset apply macos-glass"
