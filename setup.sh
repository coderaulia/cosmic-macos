#!/usr/bin/env bash
# Fetch the upstream COSMIC projects at the release this repository targets
# and apply our patches, producing the working copies the install scripts
# build from. Safe to run again: projects that already exist are left alone.
#
#   ./setup.sh               all projects
#   ./setup.sh files comp    only the named ones
#
# Projects: files edit viewer launcher comp applets libcosmic
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

TAG="epoch-1.11.0"
# libcosmic revision the 1.11 apps are built against.
LIBCOSMIC_REV="430f3d48a97d8779f9c1985473912e594ccca1ae"

identity=(-c user.name=cosmic-macos -c user.email=cosmic-macos@localhost)

# fetch <dir> <url> <patch name>
fetch() {
  local dir="$1" url="$2" patch="$3"
  if [ -d "$dir/.git" ]; then
    echo "have $dir"
    return
  fi
  echo "==> cloning $url ($TAG) into $dir"
  mkdir -p "$(dirname "$dir")"
  git clone --quiet --depth 1 --branch "$TAG" "$url" "$dir" 2>&1 | grep -v 'detached HEAD\|^$\|switch\|Turn off\|changes and\|do so\|Or undo\|state without\|If you want\|Example' || true
  if [ -s "patches/$patch.patch" ]; then
    git -C "$dir" apply --whitespace=nowarn "$ROOT/patches/$patch.patch"
    git -C "$dir" "${identity[@]}" add -A
    git -C "$dir" "${identity[@]}" commit --quiet -m "macOS-style changes ($patch.patch)"
    echo "    applied patches/$patch.patch"
  fi
}

setup_files()    { fetch apps/cosmic-files    https://github.com/pop-os/cosmic-files    cosmic-files; }
setup_edit()     { fetch apps/cosmic-edit     https://github.com/pop-os/cosmic-edit     cosmic-edit; }
setup_viewer()   { fetch apps/cosmic-viewer   https://github.com/pop-os/cosmic-viewer   cosmic-viewer; }
setup_launcher() { fetch apps/cosmic-launcher https://github.com/pop-os/cosmic-launcher cosmic-launcher; }
setup_comp()     { fetch cosmic-comp          https://github.com/pop-os/cosmic-comp     cosmic-comp; }

setup_applets() {
  local had=0
  [ -d upstream-applets/.git ] && had=1
  fetch upstream-applets https://github.com/pop-os/cosmic-applets cosmic-applets
  # Our applets and shared style crate live as new directories inside cosmic-applets.
  for dir in \
    cosmic-ext-dock \
    cosmic-ext-mac-applet-style \
    cosmic-ext-applet-clock-center \
    cosmic-ext-applet-lord-menu \
    cosmic-ext-applet-app-menu \
    cosmic-ext-applet-clipboard \
    cosmic-ext-applet-spotlight \
    cosmic-ext-applet-control-center \
    cosmic-ext-applet-power-menu; do
    if [ ! -d "upstream-applets/$dir" ] && [ -d "overlays/cosmic-applets/$dir" ]; then
      cp -r "overlays/cosmic-applets/$dir" "upstream-applets/$dir"
      echo "    added $dir"
    fi
  done
  [ "$had" = 1 ] || true
}

setup_libcosmic() {
  if [ -d libcosmic-mac/src ]; then
    echo "have libcosmic-mac"
    return
  fi
  echo "==> cloning libcosmic at ${LIBCOSMIC_REV:0:8} into libcosmic-mac"
  git clone --quiet https://github.com/pop-os/libcosmic libcosmic-mac
  git -C libcosmic-mac checkout --quiet "$LIBCOSMIC_REV"
  git -C libcosmic-mac submodule update --init --recursive --quiet
  patch -d libcosmic-mac -p1 --quiet <patches/libcosmic.patch
  echo "    applied patches/libcosmic.patch"
}

targets=("$@")
[ ${#targets[@]} -gt 0 ] || targets=(libcosmic files edit viewer launcher comp applets)
for target in "${targets[@]}"; do
  case "$target" in
    files|edit|viewer|launcher|comp|applets|libcosmic) "setup_$target" ;;
    *) echo "unknown project: $target" >&2; exit 1 ;;
  esac
done
echo "setup done"
