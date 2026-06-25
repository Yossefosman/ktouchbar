# Maintainer: KTouchBar developer

pkgname=ktouchbar
pkgver=0.0.9
pkgrel=3
pkgdesc="KTouchBar - Touch Bar daemon for KDE Plasma (split architecture)"
arch=('x86_64')
url="https://github.com/Yossefosman/ktouchbar"
license=('GPL3')
depends=(
  'cairo'
  'freetype2'
  'fontconfig'
  'librsvg'
  'libinput'
  'glibc'
  'gcc-libs'
  'systemd-libs'
  'dbus'
)
makedepends=('cargo' 'rust' 'pkg-config')
optdepends=('kde-plasma-desktop: KDE Plasma integration')
install=ktouchbar.install
source=("$pkgname-$pkgver.tar.gz")
sha256sums=('f193ed19a96c5421654a6ee9fee9dca3563ac7714410eacd200e837ec3667e7f')

build() {
  cd "$srcdir/$pkgname-$pkgver"
  cargo build --release --bin ktouchbar-system --bin ktouchbar-user
}

package() {
  cd "$srcdir/$pkgname-$pkgver"

  install -Dm755 target/release/ktouchbar-system "$pkgdir/usr/bin/ktouchbar-system"
  install -Dm755 target/release/ktouchbar-user "$pkgdir/usr/bin/ktouchbar-user"

  install -dm755 "$pkgdir/usr/share/ktouchbar/configs"
  cp -r share/ktouchbar/configs/* "$pkgdir/usr/share/ktouchbar/configs/"
  install -Dm644 share/ktouchbar/WIDGETS.md "$pkgdir/usr/share/ktouchbar/WIDGETS.md"

  install -Dm644 etc/systemd/system/ktouchbar-system.service \
    "$pkgdir/usr/lib/systemd/system/ktouchbar-system.service"
  install -Dm644 etc/systemd/user/ktouchbar-user.service \
    "$pkgdir/usr/lib/systemd/user/ktouchbar-user.service"

  install -Dm644 etc/dbus-1/system.d/org.ktouchbar.Hardware.conf \
    "$pkgdir/usr/share/dbus-1/system.d/org.ktouchbar.Hardware.conf"

  install -Dm644 etc/udev/rules.d/99-touchbar-seat.rules \
    "$pkgdir/etc/udev/rules.d/99-touchbar-seat.rules"
  install -Dm644 etc/udev/rules.d/99-touchbar-ktouchbar.rules \
    "$pkgdir/etc/udev/rules.d/99-touchbar-ktouchbar.rules"
  install -Dm644 etc/udev/rules.d/40-ktouchbar-permissions.rules \
    "$pkgdir/etc/udev/rules.d/40-ktouchbar-permissions.rules"
  install -Dm644 etc/udev/rules.d/74-ktouchbar-uinput.rules \
    "$pkgdir/etc/udev/rules.d/74-ktouchbar-uinput.rules"

  install -dm755 "$pkgdir/usr/share/kwin/scripts/ktouchbar_dynamicshortcuts/contents/code"
  install -Dm644 share/kwin/scripts/ktouchbar_dynamicshortcuts/metadata.json \
    "$pkgdir/usr/share/kwin/scripts/ktouchbar_dynamicshortcuts/metadata.json"
  install -Dm644 share/kwin/scripts/ktouchbar_dynamicshortcuts/contents/code/main.js \
    "$pkgdir/usr/share/kwin/scripts/ktouchbar_dynamicshortcuts/contents/code/main.js"

  echo "# User configs go in ~/.config/ktouchbar/configs/" \
    > "$pkgdir/usr/share/ktouchbar/configs/README"
  rm -rf "$srcdir/$pkgname-$pkgver" "$srcdir/$pkgname-$pkgver.tar.gz"
}
