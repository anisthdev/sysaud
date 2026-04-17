# sysaud-rs: Rust Rewrite of sysaud

## Development Guidelines

### Incremental Implementation
When implementing a feature or fixing a bug, work in small incremental steps. Don't try to implement multiple detectors or complex systems all at once — debugging becomes overwhelming and errors compound. Instead:
- Implement one detector at a time
- Get it to compile and basic functionality working before moving on
- Test each piece before combining

### Library Usage
When using an unfamiliar library:
- Check the official documentation (docs.rs, GitHub README, the library's book)
- Browse the source code directly in `~/.cargo/registry/src/` to understand the actual API
- Avoid guessing at API signatures or behavior — zbus 4.x is quite different from 3.x

---

## Project Overview

**Location:** `/home/asif/Projects/sysaud-rs`

**Original Project:** `/home/asif/Projects/event_detection` (Python daemon called `sysaud`)

**Purpose:** A Linux system event sound and notification daemon. It listens for system events (via D-Bus, udev, etc.) and fires sounds, desktop notifications, or custom scripts in response. Configured via YAML.

**License:** MIT

**Repository:** https://github.com/asifb/sysaud

---

## What Was Built

### Completed

1. **Project Scaffold**
   - Cargo workspace with proper dependencies
   - Modular source tree structure

2. **CLI** (`src/cli.rs`)
   - `sysaud` — Start daemon (default)
   - `sysaud check` — Validate config
   - `sysaud list-events` — List all supported events
   - Flags: `-v` (verbose), `--no-sound`, `--no-notify`, `--json`, `--config <path>`

3. **Config** (`src/config/`)
   - YAML config loading via `serde_yaml`
   - Split into modules: `mod.rs`, `debounce.rs`, `sounds.rs`, `notifications.rs`
   - Config path: `~/.config/sysaud/config.yaml`

4. **Event System** (`src/event/`)
   - `Event` struct with `EventId` (category.name), detail HashMap
   - `EventRouter` with debounce logic
   - `EventHandler` trait for strategy pattern
   - Debounce windows configurable per-event

5. **Action Dispatcher** (`src/actions/`)
   - `ActionDispatcher` routes events to handlers
   - `SoundHandler` — plays audio via `pw-play`/`paplay`/`aplay`
   - `NotifyHandler` — sends desktop notifications via D-Bus
   - `ScriptHandler` — runs shell commands with `SYSAUD_*` env vars, 10s timeout

6. **D-Bus Integration** (`src/dbus/mod.rs`)
   - Session/system bus connections via `zbus`
   - Connection factory functions (no singletons)

7. **Detector Framework** (`src/detectors/`)
   - `Detector` trait with `start()`, `stop()`, supervision/restart logic
   - `DetectorRegistry` manages detector lifecycle
   - `DetectorContext` for dependency injection
   - All 10 detectors implemented:
     - Session: `session.rs`, `suspend.rs`
     - Network: `network.rs`, `vpn.rs`
     - Power: `power.rs`, `battery.rs`
     - Other: `bluetooth.rs`, `usb.rs`, `disk.rs`, `display.rs`

8. **Signal Handling** (`src/main.rs`)
   - SIGTERM, SIGINT triggers graceful shutdown
   - SIGHUP triggers config hot reload via `SharedConfig::reload()`

9. **Binary**
   - Built at `target/release/sysaud`
   - Tested: `--help`, `check`, `list-events`, `list-events --json`

### SOLID Principles Applied
- **SRP**: Actions split into separate handlers
- **OCP**: Config-driven sound/notification defaults
- **ISP**: Minimal `Detector` trait
- **DIP**: `DetectorContext` for dependency injection
- **LSP**: Any detector can implement the trait

---

## Memory Comparison

| Version | RSS Memory |
|---------|-----------|
| Python sysaud (original) | ~63 MB |
| **Rust sysaud (new)** | **~7 MB** |

**Reduction: ~89%**

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                         main.rs                             │
│  tokio::main ──► run_daemon()                              │
│                  ├── config::load()                         │
│                  ├── dbus::session_bus()                    │
│                  ├── mpsc::channel<Event>                   │
│                  ├── SharedConfig (Arc<Config>)             │
│                  ├── DetectorRegistry::attach_all()         │
│                  │   └── tokio::spawn(MessageStream loop)  │
│                  └── event_rx handler                       │
│                        └── EventRouter::receive()          │
│                            └── ActionDispatcher             │
│                                ├── LoggerHandler            │
│                                ├── SoundHandler             │
│                                ├── NotifyHandler            │
│                                └── ScriptHandler            │
└─────────────────────────────────────────────────────────────┘
```

---

## What Needs to Be Done

### All Items Completed ✅

All planned features have been implemented:

1. ~~**PID Lock File**~~ ✅ **Implemented**
   - Uses `libc::syscall` with `SYS_pidfd_open` on Linux (kernel 5.6+)
   - Falls back to file-based lock at `$XDG_RUNTIME_DIR/sysaud/sysaud.pid`
   - Proper pidfd cleanup on drop

2. ~~**Config Hot Reload on SIGHUP**~~ ✅ **Implemented**
   - `SharedConfig` wraps `Arc<Config>` for shared state
   - All handlers read config at event time (no stale config)
   - `SessionDetector` re-reads `lock_app` on each poll

3. ~~**Event Logging**~~ ✅ **Implemented**
   - `LoggerHandler` writes events to optional log file
   - JSON format with timestamp, event, category, name, detail
   - Enabled via `daemon.log_file` in config

4. ~~**Systemd Unit File**~~ ✅ **Implemented**
   - `systemd/sysaud.service` user unit
   - Supports `ExecReload=/bin/kill -HUP $MAINPID` for config hot reload

5. ~~**PKGBUILD for AUR**~~ ✅ **Implemented**
   - `PKGBUILD` for Arch Linux distribution
   - Installs binary, systemd unit, and license

6. ~~**Replace Blocking Threads**~~ ✅ **Implemented**
   - USB and display detectors use `tokio::task::spawn_blocking` for blocking file I/O
   - udev monitoring loops kept in separate threads (event-driven)

---

## Dependencies

```toml
tokio = { version = "1", features = ["full"] }
zbus = "4"
zvariant = "4"
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
clap = { version = "4", features = ["derive"] }
once_cell = "1"
anyhow = "1"
thiserror = "1"
dirs = "5"
chrono = "0.4"
which = "6"
serde_json = "1"
futures-util = "0.3"
udev = "0.9"
mio = "1"
parking_lot = "0.12"
nix = "0.29"
```

---

## Project Structure

```
sysaud-rs/
├── src/
│   ├── main.rs           # Entry point, signal handling, daemon setup
│   ├── cli.rs            # CLI argument parsing
│   ├── config/           # Configuration loading and SharedConfig
│   ├── dbus/             # D-Bus connection factories
│   ├── event/            # Event system (router, handlers)
│   ├── actions/          # Action handlers (logger, sound, notify, script)
│   ├── detectors/        # All event detectors
│   └── pidlock.rs        # PID file locking
├── systemd/
│   └── sysaud.service    # User systemd unit
├── PKGBUILD             # Arch AUR package
├── LICENSE              # MIT license
└── Cargo.toml
```

---

## Original Python Project Reference

**Location:** `/home/asif/Projects/event_detection`

The Python version (`sysaud`) is the reference implementation. Key files:

- `sysaud/__main__.py` — CLI entry, daemon startup
- `sysaud/config.py` — YAML config
- `sysaud/router.py` — EventRouter with debounce
- `sysaud/detectors/session.py` — logind detection
- `sysaud/actions/` — sound, notify, script

---

## Running the Daemon

```bash
# Build
cd /home/asif/Projects/sysaud-rs
cargo build --release

# Run
./target/release/sysaud                    # Daemon mode
./target/release/sysaud check             # Validate config
./target/release/sysaud list-events       # List events
./target/release/sysaud -v                # Verbose
./target/release/sysaud --no-sound        # Disable sounds
./target/release/sysaud --config /path     # Custom config
```

---

## Config File Format

Located at `~/.config/sysaud/config.yaml`:

```yaml
daemon:
  lock_app: hyprlock
  log_file: ~/.local/share/sysaud/events.log

sound:
  enabled: true
  theme: freedesktop
  custom_dir: ~/sounds

notify:
  enabled: true
  timeout_ms: 4000

events:
  power.ac-plugged:
    sound: power-plug
    icon: battery-full-charging
    notify: true
  power.ac-unplugged:
    enabled: false  # silence this event
```

---

## Environment

- **Rust:** 1.88.0
- **Platform:** Linux x86_64
- **Audio:** PipeWire + WirePlumber
- **Session Bus:** Available (D-Bus)
- **System Bus:** Available (D-Bus)

---

## Notes

- The `event.rs` module has been split into `src/event/` directory with `mod.rs`, `handler.rs`, `router.rs`
- `zbus` 4.x API is async-based, using `MessageStream` for receiving signals
- Config hot reload uses `SharedConfig` with `Arc<Config>` shared across all handlers
- Event logging writes JSON lines to optional file configured via `daemon.log_file`
