#!/usr/bin/env bash
# Build and install the standalone magnifying dock for the current user, then
# make the macos-glass preset use it instead of the COSMIC dock.
# Undo with: ./install-dock.sh --uninstall
set -euo pipefail

BIN_NAME="cosmic-ext-dock"
ROOT="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE="$ROOT/upstream-applets"
BIN_DIR="$HOME/.local/bin"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
PRESET="$CONFIG_HOME/cosmic-presets/macos-glass"
ENTRIES="$PRESET/com.system76.CosmicPanel/v1/entries"
AUTOSTART="$CONFIG_HOME/autostart/cosmic-ext-dock.desktop"

[ -d "$PRESET" ] || { echo "macos-glass preset not found at $PRESET" >&2; exit 1; }
mkdir -p "$(dirname "$ENTRIES")"

if [ "${1:-}" = "--uninstall" ]; then
  printf '[\n    "Panel",\n    "Dock",\n]' >"$ENTRIES"
  rm -f "$PRESET/use-ext-dock" "$AUTOSTART"
  pkill -x "$BIN_NAME" || true
  rm -f "$BIN_DIR/$BIN_NAME"
  echo "uninstalled; run: cosmic-preset apply macos-glass"
  exit 0
fi

# Use the installed stable toolchain rather than downloading the pinned one.
(cd "$WORKSPACE" && RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}" cargo build --release -p "$BIN_NAME")

pkill -x "$BIN_NAME" || true
install -Dm755 "$WORKSPACE/target/release/$BIN_NAME" "$BIN_DIR/$BIN_NAME"

# The preset hides the COSMIC dock and marks that the standalone one runs.
printf '[\n    "Panel",\n]' >"$ENTRIES"
touch "$PRESET/use-ext-dock"

# Start the dock at login whenever the active preset asks for it.
mkdir -p "$(dirname "$AUTOSTART")"
cat >"$AUTOSTART" <<EOF
[Desktop Entry]
Type=Application
Name=Magnifying Dock
Exec=$BIN_DIR/cosmic-preset dock-sync
NoDisplay=true
EOF

# Restart the dock now if the active preset uses it.
"$BIN_DIR/cosmic-preset" dock-sync || true

echo "installed; if macos-glass is not active, run: cosmic-preset apply macos-glass"
