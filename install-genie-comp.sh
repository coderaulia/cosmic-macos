#!/usr/bin/env bash
# Build the genie-patched cosmic-comp and make the session use it at next login.
#
# Nothing in /usr is touched: the patched compositor goes to
# ~/.local/lib/cosmic-comp-genie/ and a small launcher named cosmic-comp goes
# to ~/.local/bin, which comes before /usr/bin in the session's PATH.
#
# Crash guard: if the compositor is started twice within 90 seconds (a crash
# drops you back to the login screen and you log in again), the launcher
# starts the stock /usr/bin/cosmic-comp instead.
#
# Usage:
#   ./install-genie-comp.sh              build and install
#   ./install-genie-comp.sh --disable    use stock compositor, keep the build
#   ./install-genie-comp.sh --enable     use the patched compositor again
#   ./install-genie-comp.sh --uninstall  remove everything
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
SOURCE="$ROOT/cosmic-comp"
LIB_DIR="$HOME/.local/lib/cosmic-comp-genie"
LAUNCHER="$HOME/.local/bin/cosmic-comp"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/cosmic-comp-genie"

case "${1:-}" in
  --disable)
    mkdir -p "$STATE" && touch "$STATE/disabled"
    echo "stock compositor will be used from next login"
    exit 0
    ;;
  --enable)
    rm -f "$STATE/disabled" "$STATE/last-start"
    echo "patched compositor will be used from next login"
    exit 0
    ;;
  --uninstall)
    rm -f "$LAUNCHER"
    rm -rf "$LIB_DIR" "$STATE"
    echo "removed; stock compositor will be used from next login"
    exit 0
    ;;
esac

# The source must match the installed compositor version.
installed="$(pacman -Q cosmic-comp 2>/dev/null | awk '{print $2}')"
case "$installed" in
  *1.11.0*) ;;
  *) echo "installed cosmic-comp is '$installed', patch is for 1.11.0; aborting" >&2; exit 1 ;;
esac

# The "genie" profile skips upstream's fat LTO, and mold links much faster
# than the default linker, so rebuilds after a change take seconds to a
# minute instead of many minutes.
LINK_FLAGS=""
if command -v mold >/dev/null && command -v clang >/dev/null; then
  export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=clang
  LINK_FLAGS="-C link-arg=-fuse-ld=mold"
fi
# Build into this project's own folder, whatever ~/.cargo/config.toml says.
export CARGO_TARGET_DIR="$SOURCE/target"
(cd "$SOURCE" && RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}" \
  RUSTFLAGS="${RUSTFLAGS:-} $LINK_FLAGS" cargo build --profile genie)

[ "$SOURCE/target/genie/cosmic-comp" -nt "$0" ] || [ -n "$(find "$SOURCE/target/genie/cosmic-comp" -mmin -30 2>/dev/null)" ] || { echo "the build output is old; the build may have gone elsewhere" >&2; exit 1; }
install -Dm755 "$SOURCE/target/genie/cosmic-comp" "$LIB_DIR/cosmic-comp"

mkdir -p "$(dirname "$LAUNCHER")" "$STATE"
cat >"$LAUNCHER" <<'EOF'
#!/bin/sh
# Starts the genie-patched compositor, or the stock one if it was disabled or
# seems to have crashed (started again within 90 seconds).
case "$1" in --version|-V|--help|-h) exec /usr/bin/cosmic-comp "$@" ;; esac
state="${XDG_STATE_HOME:-$HOME/.local/state}/cosmic-comp-genie"
mkdir -p "$state"
now=$(date +%s)
last=$(cat "$state/last-start" 2>/dev/null || echo 0)
echo "$now" >"$state/last-start"
if [ -e "$state/disabled" ] || [ $((now - last)) -lt 90 ] \
  || [ ! -x "$HOME/.local/lib/cosmic-comp-genie/cosmic-comp" ]; then
  exec /usr/bin/cosmic-comp "$@"
fi
exec "$HOME/.local/lib/cosmic-comp-genie/cosmic-comp" "$@"
EOF
chmod 755 "$LAUNCHER"
rm -f "$STATE/last-start"

echo "installed; log out and back in to use it"
