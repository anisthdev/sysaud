# Maintainer: asifb
pkgname=sysaud
pkgver=0.1.1
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
sha256sums=('3a1ac8a40f2ce1a4f70e184dfd8c78bd34774bbe1c2e170aa8bb7099918a1030')

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
