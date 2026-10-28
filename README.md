# sysaud

`sysaud` is a Linux system event sound and notification daemon written in Rust.
It listens for desktop and system events over D-Bus, udev, and related system
interfaces, then fires sounds, notifications, or custom scripts in response.

## Features

- Plays event sounds for common desktop and hardware events
- Sends desktop notifications with event-specific icons and text
- Runs custom scripts with event metadata in environment variables
- Supports YAML configuration with per-event overrides
- Reloads configuration on `SIGHUP`
- Ships a systemd user service

## Supported Events

- Power: `power.ac-plugged`, `power.ac-unplugged`, `power.battery-low`, `power.battery-critical`, `power.battery-action`, `power.battery-level`
- Network: `network.wifi-connected`, `network.network-connected`, `network.network-disconnected`, `network.vpn-connected`, `network.vpn-disconnected`
- Bluetooth: `bluetooth.device-connected`, `bluetooth.device-disconnected`, `bluetooth.adapter-on`, `bluetooth.adapter-off`
- Audio: `audio.mute-changed`, `audio.volume-changed`, `audio.default-sink-changed`, `audio.default-source-changed`
- USB: `usb.device-plugged`, `usb.device-unplugged`
- Session: `session.login`, `session.screen-locked`, `session.screen-unlocked`, `session.suspend`, `session.resume`
- Other: `display.brightness-changed`, `disk.mounted`, `disk.unmounted`

You can list them from the binary with:

```bash
sysaud list-events
sysaud --json list-events
```

## Build

Requirements:

- Rust toolchain
- `pkg-config`
- Linux with D-Bus and udev available

Build the release binary:

```bash
cargo build --release
```

The resulting binary will be at:

```bash
target/release/sysaud
```

## Installation

### Arch Linux / pacman package

This repository includes a `PKGBUILD`.

Build and install it with:

```bash
makepkg -si
```

Installed paths:

- Binary: `/usr/bin/sysaud`
- User service: `/usr/lib/systemd/user/sysaud.service`
- Man page: `/usr/share/man/man1/sysaud.1`
- Default sounds map: `/usr/share/sysaud/sounds.toml`
- Default notifications map: `/usr/share/sysaud/notifications.toml`

### Manual run from the repo

You can also run directly without packaging:

```bash
cargo run --release
```

Or:

```bash
./target/release/sysaud
```

## systemd User Service

After package installation, enable and start the user service with:

```bash
systemctl --user enable --now sysaud.service
```

Useful commands:

```bash
systemctl --user status sysaud.service
systemctl --user restart sysaud.service
systemctl --user stop sysaud.service
```

Reload configuration without restarting the daemon:

```bash
systemctl --user reload sysaud.service
```

Or send `SIGHUP` directly:

```bash
kill -HUP "$(pidof sysaud)"
```

To override the packaged unit locally, place drop-ins under:

```bash
~/.config/systemd/user/sysaud.service.d/
```

## Configuration

Default user config path:

```bash
~/.config/sysaud/config.yaml
```

If the file does not exist, `sysaud` starts with built-in defaults.

Example:

```yaml
daemon:
  lock_app: hyprlock
  log_file: ~/.local/share/sysaud/events.log

sound:
  enabled: true
  theme: freedesktop
  custom_dir: ~/sounds
  volume_debounce_ms: 250

notify:
  enabled: true
  timeout_ms: 4000

events:
  power.ac-plugged:
    sound: power-plug
    icon: battery-full-charging
    notify: true
  power.ac-unplugged:
    enabled: false
```

Notes:

- `sound.custom_dir` can point at your own sound files
- `sound.volume_debounce_ms` is how long the volume must stay still before its sound plays (default 250)
- `events.<event-id>.enabled: false` disables an event entirely
- `events.<event-id>.script` can run a custom command for that event
- `daemon.log_file` enables JSON-line event logging

## Default Data Files

The daemon loads packaged defaults from:

- `/usr/share/sysaud/sounds.toml`
- `/usr/share/sysaud/notifications.toml`

When running from a development checkout, it falls back to:

- `data/sounds.toml`
- `data/notifications.toml`

## CLI

```bash
sysaud
sysaud check
sysaud list-events
sysaud --json list-events
sysaud -v
sysaud --no-sound
sysaud --no-notify
sysaud --config /path/to/config.yaml
```

## Packaging Releases

The Arch package fetches GitHub's source tarball for a matching tag:

```bash
https://github.com/anisthdev/sysaud/archive/v0.1.0.tar.gz
```

For a new release:

1. Bump `pkgver` in `PKGBUILD`
2. Push the target commit
3. Create and push the matching tag, for example `v0.1.1`
4. Update `sha256sums` in `PKGBUILD` to match the generated GitHub tarball

## License

MIT. See [LICENSE](LICENSE).
