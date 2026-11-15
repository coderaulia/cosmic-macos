#!/usr/bin/env bash
# Build and install the macOS panel toolbar applets (lord-menu, app-menu,
# spotlight, control-center) and update the macos-glass panel preset geometry
# and wings while keeping dev.asw.CosmicExtAppletClockCenter in plugins_center.
# Undo with: ./install-panel.sh --uninstall
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
WORKSPACE="$ROOT/upstream-applets"
BIN_DIR="$HOME/.local/bin"
APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
PRESET="${XDG_CONFIG_HOME:-$HOME/.config}/cosmic-presets/macos-glass/com.system76.CosmicPanel.Panel/v1"

APPLETS=(
  "cosmic-ext-applet-lord-menu|dev.asw.CosmicExtAppletLordMenu"
  "cosmic-ext-applet-app-menu|dev.asw.CosmicExtAppletAppMenu"
  "cosmic-ext-applet-spotlight|dev.asw.CosmicExtAppletSpotlight"
  "cosmic-ext-applet-control-center|dev.asw.CosmicExtAppletControlCenter"
  "cosmic-ext-applet-power-menu|dev.asw.CosmicExtAppletPowerMenu"
)

if [ "${1:-}" = "--uninstall" ]; then
  if [ -d "$PRESET" ]; then
    "$ROOT/tools/panel-layout.sh" --preset "$PRESET" --legacy-wings --legacy-geometry
  fi
  for entry in "${APPLETS[@]}"; do
    IFS='|' read -r bin_name app_id <<<"$entry"
    pkill -x "$bin_name" || true
    rm -f "$BIN_DIR/$bin_name" "$APPS_DIR/$app_id.desktop"
  done
  echo "uninstalled; run: cosmic-preset apply macos-glass"
  exit 0
fi

[ -d "$PRESET" ] || { echo "macos-glass preset not found at $PRESET" >&2; exit 1; }

export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
export CARGO_TARGET_DIR="$WORKSPACE/target"

mkdir -p "$BIN_DIR" "$APPS_DIR"
for entry in "${APPLETS[@]}"; do
  IFS='|' read -r bin_name app_id <<<"$entry"
  echo "==> building $bin_name"
  (cd "$WORKSPACE" && cargo build --release -p "$bin_name")
  [ "$WORKSPACE/target/release/$bin_name" -nt "$0" ] || [ -n "$(find "$WORKSPACE/target/release/$bin_name" -mmin -30 2>/dev/null)" ] || { echo "the build output for $bin_name is old; the build may have gone elsewhere" >&2; exit 1; }
  install -Dm755 "$WORKSPACE/target/release/$bin_name" "$BIN_DIR/$bin_name"
  sed "s|^Exec=.*|Exec=$BIN_DIR/$bin_name|" \
    "$WORKSPACE/$bin_name/data/$app_id.desktop" >"$APPS_DIR/$app_id.desktop"
done

"$ROOT/tools/panel-layout.sh" --preset "$PRESET" --toolbar --geometry

echo "installed; run: cosmic-preset apply macos-glass"
