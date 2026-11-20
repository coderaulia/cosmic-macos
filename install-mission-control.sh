#!/usr/bin/env bash
# Build the Mission Control fork of cosmic-workspaces (the workspace overview)
# and install it for the current user.
#
# Nothing in /usr is touched. The binary goes to ~/.local/bin/cosmic-workspaces.
# cosmic-comp runs the WorkspaceOverview shortcut action as the plain command
# "cosmic-workspaces" (see /usr/share/cosmic/com.system76.CosmicSettings.Shortcuts/
# v1/system_actions), resolved through the session PATH, where ~/.local/bin comes
# before /usr/bin, so this build takes over without any other change.
#
# The overview is a single-instance daemon: an already running stock
# cosmic-workspaces keeps answering until it is restarted, so this script stops
# it and starts the new binary (log out and back in does the same).
#
# Super+Up is bound to the overview (Mission Control key). Super+W stays bound
# to it too by the COSMIC defaults.
#
# Usage:
#   ./install-mission-control.sh              build and install
#   ./install-mission-control.sh --no-restart install without restarting the daemon
#   ./install-mission-control.sh --uninstall  remove the binary and the shortcut
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
SOURCE="$ROOT/apps/cosmic-workspaces"
BIN="$HOME/.local/bin/cosmic-workspaces"
NAME='(^|/)cosmic-workspaces( |$)'

stop_daemon() { pkill -f "$NAME" 2>/dev/null || true; }

restart_daemon() {
  stop_daemon
  sleep 1
  # cosmic-session restarts the overview by name through PATH; only start one
  # ourselves when nothing came back (no session supervising it).
  if [ -n "${WAYLAND_DISPLAY:-}" ] && ! pgrep -f "$NAME" >/dev/null; then
    setsid -f "${1:-cosmic-workspaces}" >/dev/null 2>&1 || true
  fi
}

case "${1:-}" in
  --uninstall)
    "$ROOT/tools/shortcut.sh" unset "Super" Up || true
    rm -f "$BIN"
    restart_daemon /usr/bin/cosmic-workspaces
    echo "removed; stock workspace overview is used again"
    exit 0
    ;;
esac

if [ ! -d "$SOURCE/.git" ]; then
  "$ROOT/setup.sh" workspaces
fi

# Fast builds: upstream release profiles use LTO, which makes every rebuild
# slow. Override it from the environment instead of editing the project.
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
export CARGO_PROFILE_RELEASE_LTO=false
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
export CARGO_PROFILE_RELEASE_INCREMENTAL=true
if command -v mold >/dev/null && command -v clang >/dev/null; then
  export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=clang
  export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-fuse-ld=mold"
fi
# One build folder for all forked apps, shared with install-apps.sh.
export CARGO_TARGET_DIR="$ROOT/apps/.target"

echo "==> building Mission Control (cosmic-workspaces)"
(cd "$SOURCE" && cargo build --release -p cosmic-workspaces)

built="$CARGO_TARGET_DIR/release/cosmic-workspaces"
[ -x "$built" ] || { echo "no build output at $built" >&2; exit 1; }
# Refuse a stale binary if the build went elsewhere.
if ! find "$built" -mmin -30 | grep -q .; then
  echo "the build output is old; the build may have gone elsewhere" >&2
  exit 1
fi

install -Dm755 "$built" "$BIN"
"$ROOT/tools/shortcut.sh" set "Super" Up 'System(WorkspaceOverview)'
echo "installed $BIN; Super+Up opens Mission Control"

if [ "${1:-}" = "--no-restart" ]; then
  echo "restart the running overview to use it: pkill -f '$NAME'; then press Super+Up"
else
  restart_daemon "$BIN"
  echo "restarted the workspace overview daemon"
fi
