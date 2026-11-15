#!/usr/bin/env bash
# Refresh patches/ and overlays/ from the working copies, so the repository
# matches what is built on this machine. Run it before committing.
#
#   patches/<name>.patch   changes to an upstream COSMIC project, against the
#                          release tag that setup.sh clones
#   overlays/<project>/    new source directories that live inside an upstream
#                          project (the dock and the clock applet)
#   presets/macos-glass    the macOS preset for cosmic-preset
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
mkdir -p patches overlays presets

# patch_of <patch name> <directory> [pathspec to leave out ...]
patch_of() {
  local name="$1" dir="$2"
  shift 2
  [ -d "$dir/.git" ] || { echo "skip $name: $dir is not set up"; return; }
  local base
  base="$(git -C "$dir" rev-list --max-parents=0 HEAD)"
  # Make new files visible to diff without committing anything.
  git -C "$dir" add -N . 2>/dev/null || true
  local excludes=()
  for path in "$@"; do excludes+=(":(exclude)$path"); done
  git -C "$dir" diff --binary "$base" -- . "${excludes[@]}" >"$ROOT/patches/$name.patch"
  echo "patches/$name.patch  ($(wc -l <"$ROOT/patches/$name.patch") lines)"
}

APPLET_OVERLAYS=(
  cosmic-ext-dock
  cosmic-ext-mac-applet-style
  cosmic-ext-applet-clock-center
  cosmic-ext-applet-lord-menu
  cosmic-ext-applet-power-menu
  cosmic-ext-applet-app-menu
  cosmic-ext-applet-clipboard
  cosmic-ext-applet-spotlight
  cosmic-ext-applet-control-center
)

patch_of cosmic-files apps/cosmic-files
patch_of cosmic-edit apps/cosmic-edit
patch_of cosmic-viewer apps/cosmic-viewer
patch_of cosmic-launcher apps/cosmic-launcher
patch_of cosmic-comp cosmic-comp
patch_of cosmic-applets upstream-applets "${APPLET_OVERLAYS[@]}"

# Our applets and shared style crate are new directories inside cosmic-applets.
mkdir -p overlays/cosmic-applets
for dir in "${APPLET_OVERLAYS[@]}"; do
  [ -d "upstream-applets/$dir" ] || continue
  rsync -a --delete --exclude target "upstream-applets/$dir/" "overlays/cosmic-applets/$dir/"
done
echo "overlays/cosmic-applets/"

# The toolkit fork: only the header bar differs from upstream libcosmic.
if [ -d libcosmic-mac ] && command -v git >/dev/null; then
  upstream="$(ls -d "$HOME"/.cargo/git/checkouts/libcosmic-*/430f3d4 2>/dev/null | head -1 || true)"
  if [ -n "$upstream" ]; then
    : >patches/libcosmic.patch
    (cd "$upstream" && for f in src/widget/header_bar.rs cosmic-theme/src/model/theme.rs; do
      diff -u --label "a/$f" --label "b/$f" "$f" "$ROOT/libcosmic-mac/$f" || true
    done) >patches/libcosmic.patch
    echo "patches/libcosmic.patch  ($(wc -l <patches/libcosmic.patch) lines)"
  fi
fi

# The macOS preset, without the machine-specific current marker.
if [ -d "$HOME/.config/cosmic-presets/macos-glass" ]; then
  rsync -a --delete "$HOME/.config/cosmic-presets/macos-glass/" presets/macos-glass/
  echo "presets/macos-glass/"
fi
cp "$HOME/.local/bin/cosmic-preset" bin/cosmic-preset 2>/dev/null || true
