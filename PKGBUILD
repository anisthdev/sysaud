# Maintainer: asifb
pkgname=sysaud
pkgver=0.1.3
pkgrel=1
pkgdesc="Linux system event sound and notification daemon (Rust)"
arch=('x86_64')
url="https://github.com/anisthdev/sysaud"
license=('MIT')
depends=(
    'dbus'
    'udev'
    'pipewire'
)
optdepends=(
    'pipewire-pulse: audio playback via pw-play'
    'pulseaudio: audio playback via paplay'
    'alsa-utils: audio playback via aplay (fallback)'
    'libpulse: audio playback via paplay (fallback)'
)
makedepends=('cargo' 'pkg-config')
source=("$pkgname-$pkgver.tar.gz::$url/archive/v$pkgver.tar.gz")
sha256sums=('eb9b5ecd0452617cd4463a41844ae7002589b1eef502b3f1752c904d8f8ee385')

build() {
    cd "$pkgname-$pkgver"
    cargo build --release --locked
}

package() {
    cd "$pkgname-$pkgver"
    install -Dm755 target/release/sysaud "$pkgdir/usr/bin/sysaud"
    install -Dm644 systemd/sysaud.service \
        "$pkgdir/usr/lib/systemd/user/sysaud.service"
    install -Dm644 man/sysaud.1 "$pkgdir/usr/share/man/man1/sysaud.1"
    install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
    install -Dm644 data/sounds.toml "$pkgdir/usr/share/sysaud/sounds.toml"
    install -Dm644 data/notifications.toml "$pkgdir/usr/share/sysaud/notifications.toml"
}
