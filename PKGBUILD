# Maintainer: coderaulia <https://github.com/coderaulia/cosmic-macos>
pkgname=cosmic-macos-git
pkgver=r0.0000000
pkgrel=1
pkgdesc="macOS-style desktop for COSMIC: dock, menu bar, Spotlight, Launchpad, Mission Control, Quick Look, Screenshot, widgets, Calendar, Mail"
arch=('x86_64')
url="https://github.com/coderaulia/cosmic-macos"
license=('GPL-3.0-only')
depends=(
  'cosmic-comp'
  'cosmic-panel'
  'cosmic-session'
  'libsecret'
  'libxkbcommon'
  'rsync'
  'wayland'
)
makedepends=(
  'cargo'
  'clang'
  'cmake'
  'git'
  'mold'
  'nasm'
  'pkgconf'
  'rust'
)
optdepends=(
  'libnotify: desktop notifications for cosmic-preset'
  'gstreamer: screen recording in mac-screenshot'
  'gst-plugins-good: screen recording (mp4mux)'
  'gst-plugin-pipewire: screen recording (pipewiresrc)'
  'poppler: PDF previews in mac-quicklook'
  'ffmpegthumbnailer: video previews in mac-quicklook'
  'upower: battery widget'
  'xdg-utils: "Open with" in mac-quicklook'
  'whitesur-icon-theme: macOS-style application and system icons'
)
provides=('cosmic-macos')
conflicts=('cosmic-macos')
source=("${pkgname}::git+https://github.com/coderaulia/cosmic-macos.git")
sha256sums=('SKIP')

pkgver() {
  cd "$srcdir/$pkgname"
  printf "r%s.%s" "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}

build() {
  cd "$srcdir/$pkgname"
  ./setup.sh libcosmic
  export CARGO_TARGET_DIR="$srcdir/$pkgname/apps/.target"
  cargo build --release --manifest-path apps/mac-suite/Cargo.toml
}

package() {
  cd "$srcdir/$pkgname"

  install -Dm755 bin/cosmic-preset "$pkgdir/usr/bin/cosmic-preset"

  local bin
  for bin in apps/.target/release/mac-*; do
    [ -f "$bin" ] && [ -x "$bin" ] || continue
    case "$bin" in
      *.d|*.rlib|*.so|*.a) continue ;;
    esac
    install -Dm755 "$bin" "$pkgdir/usr/bin/$(basename "$bin")"
  done

  local desktop
  for desktop in apps/mac-suite/*/data/*.desktop; do
    install -Dm644 "$desktop" "$pkgdir/usr/share/applications/$(basename "$desktop")"
  done

  install -dm755 "$pkgdir/usr/share/cosmic-macos/presets"
  cp -a presets/macos-glass "$pkgdir/usr/share/cosmic-macos/presets/"
  if [ -d presets/macos-light ]; then
    cp -a presets/macos-light "$pkgdir/usr/share/cosmic-macos/presets/"
  fi

  install -Dm755 setup.sh "$pkgdir/usr/share/cosmic-macos/setup.sh"
  install -Dm755 install.sh "$pkgdir/usr/share/cosmic-macos/install.sh"
  # Forks of COSMIC apps (Files, Mission Control, compositor, panel applets)
  # replace system binaries, so they stay per-user: run these scripts from
  # /usr/share/cosmic-macos as your user.
  local script
  for script in install-*.sh; do
    install -Dm755 "$script" "$pkgdir/usr/share/cosmic-macos/$script"
  done
  cp -a tools patches overlays "$pkgdir/usr/share/cosmic-macos/"

  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"
}
