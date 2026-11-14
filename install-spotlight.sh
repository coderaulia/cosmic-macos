#!/usr/bin/env bash
# Build and install the Spotlight-style launcher (a restyled cosmic-launcher).
#
# It installs as ~/.local/bin/cosmic-launcher, which the session finds before
# /usr/bin, so the Super key keeps working exactly as before but opens the
# new look. Nothing in /usr is touched.
#
# Usage:
#   ./install-spotlight.sh              build and install
#   ./install-spotlight.sh --uninstall  go back to the stock launcher
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)/apps"
BIN="$HOME/.local/bin/cosmic-launcher"

if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$BIN"
  pkill -x cosmic-launcher || true
  echo "removed; the stock launcher starts at next login (or run: cosmic-launcher &)"
  exit 0
fi

export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export CARGO_PROFILE_RELEASE_INCREMENTAL=true
if command -v mold >/dev/null && command -v clang >/dev/null; then
  export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=clang
  export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-fuse-ld=mold"
fi
export CARGO_TARGET_DIR="$ROOT/.target"

(cd "$ROOT/cosmic-launcher" && cargo build --release)
install -Dm755 "$CARGO_TARGET_DIR/release/cosmic-launcher" "$BIN"

# Stop the running launcher; the session restarts it, now using the new one.
pkill -x cosmic-launcher || true
echo "installed; press Super to open it"
