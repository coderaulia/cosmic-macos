#!/usr/bin/env bash
# Give the desktop a macOS-like theme (system blue accent, rounder corners)
# and make it part of the macos-glass preset, so `cosmic-preset revert`
# switches back to the original theme.
#
# Usage: ./install-theme.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)/apps/mac-suite"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
LIVE="$CONFIG_HOME/cosmic"
STORE="$CONFIG_HOME/cosmic-presets"
THEME_DIRS=(
  com.system76.CosmicTheme.Dark/v2
  com.system76.CosmicTheme.Dark.Builder/v2
  com.system76.CosmicTheme.Light/v2
  com.system76.CosmicTheme.Light.Builder/v2
)

[ -d "$STORE/original" ] || { echo "no 'original' preset; set up presets first" >&2; exit 1; }

# 1. The original preset keeps today's theme, before anything changes it.
for dir in "${THEME_DIRS[@]}"; do
  if [ ! -d "$STORE/original/$dir" ] && [ -d "$LIVE/$dir" ]; then
    mkdir -p "$STORE/original/$dir"
    rsync -a "$LIVE/$dir/" "$STORE/original/$dir/"
  fi
done

# 2. Build and run the theme tool.
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export CARGO_PROFILE_RELEASE_INCREMENTAL=true
if command -v mold >/dev/null && command -v clang >/dev/null; then
  export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=clang
  export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-fuse-ld=mold"
fi
export CARGO_TARGET_DIR="$ROOT/../.target"
(cd "$ROOT" && cargo build --release -p mac-theme)
"$CARGO_TARGET_DIR/release/mac-theme"

# 3. The macos-glass preset gets the new theme.
for dir in "${THEME_DIRS[@]}"; do
  mkdir -p "$STORE/macos-glass/$dir"
  rsync -a --delete "$LIVE/$dir/" "$STORE/macos-glass/$dir/"
done
echo "done; revert with: cosmic-preset revert  (re-apply: cosmic-preset apply macos-glass)"
