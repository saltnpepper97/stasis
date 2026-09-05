<p align="center">
  <img src="assets/stasis.png" alt="Stasis Logo" width="200"/>
</p>

<h1 align="center">Stasis</h1>

<p align="center">
  <strong>A modern Wayland idle manager that knows when to step back.</strong>
</p>

<p align="center">
  Keep your session balanced by preventing idle when you are busy and letting it happen when you are not.
</p>

<p align="center">
  <img src="https://img.shields.io/github/last-commit/saltnpepper97/stasis?style=for-the-badge&color=%2328A745" alt="GitHub last commit"/>
  <img src="https://img.shields.io/aur/version/stasis?style=for-the-badge" alt="AUR version">
  <img src="https://img.shields.io/badge/License-GPLv3-E5534B?style=for-the-badge" alt="GPL-3.0 License"/>
  <img src="https://img.shields.io/badge/Wayland-00BFFF?style=for-the-badge&logo=wayland&logoColor=white" alt="Wayland"/>
  <img src="https://img.shields.io/badge/Rust-1.89+-orange?style=for-the-badge&logo=rust&logoColor=white" alt="Rust"/>
</p>

<p align="center">
  <a href="#features">Features</a> •
  <a href="#installation">Installation</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#architecture">Architecture</a> •
  <a href="#cli-usage">CLI Usage</a> •
  <a href="#compositor-support">Compositor Support</a> •
  <a href="#contributing">Contributing</a>
</p>

---

## Features

Stasis is not a simple timer-based screen locker.  
It is a **context-aware, event-driven idle manager** built around explicit state and decisions.

- 🧠 Smart idle detection with sequential, configurable timeouts
- 🎵 Media-aware idle handling
  - Optional audio-based detection
  - Differentiates active, paused, and muted streams
- 🚫 Application-specific inhibitors
  - Prevent idle when selected apps are running
  - Or block only automatic suspend while earlier idle actions continue
  - Regex-based matching supported
- ⏸️ Wayland idle inhibitor support
  - Honors compositor and application inhibitors
- 🛌 Laptop-aware power handling
  - Optional D-Bus integration for lid events, suspend/resume, session inhibit traffic, and login1 idle inhibitors
- ⚙️ Flexible action plans
  - Startup steps, sequential steps, instant actions, resume hooks
- 🔁 Manual idle inhibition
  - Toggle idle on/off via CLI, status bars (Waybar-friendly JSON), or the optional tray frontend
- 📝 Clean configuration
  - Uses the expressive [RUNE](https://github.com/saltnpepper97/rune-cfg) configuration language
- ⚡ Live reload
  - Reload configuration without restarting the daemon
- 📜 Structured logging
  - Powered by [eventline](https://github.com/saltnpepper97/eventline) for journaling and traceable logs

---

## Architecture

Stasis is built around a deterministic, event-driven state machine.

There are no hidden timers, background polling loops, or implicit behavior.

    External signals
      ↓
    Event (pure data)
      ↓
    Manager (decision logic)
      ↓
    State (authoritative)
      ↓
    Actions (declarative)
      ↓
    Services (side effects)

Design principles:

- State is authoritative
- Events are pure data
- Managers decide, services act
- Side effects are isolated
- Data flows strictly forward

---

## Installation

### Arch Linux (AUR)

    yay -S stasis
    yay -S stasis-git

### Nix / NixOS (Flakes)

    nix build 'github:saltnpepper97/stasis#stasis'

#### NixOS Notes

**swaylock PAM configuration**

If you use swaylock as your screen locker on NixOS, you must add the following to your NixOS configuration or swaylock will lock the screen but never accept your password to unlock it:

```nix
security.pam.services.swaylock = {};
```

---

### From Source

Dependencies:
- rust / cargo (build)
- wayland (runtime)
- dbus (runtime, strongly recommended; required for full feature set)
  - used for session and login1 inhibit handling (`enable_dbus_inhibit`)
  - used for portal/browser inhibit traffic
  - used for lid events and suspend/resume integration
- pulseaudio or pipewire-pulse (runtime, recommended for media/call detection via `pactl`)
- libnotify (optional, desktop notifications)

Build & install:

    git clone https://github.com/saltnpepper97/stasis
    cd stasis
    cargo build --release --locked
    sudo install -Dm755 target/release/stasis /usr/local/bin/stasis
    sudo install -Dm644 assets/stasis.png /usr/local/share/icons/hicolor/256x256/apps/stasis.png

---

## Quick Start

Start the daemon:

    stasis

The full quick-start guide, configuration reference, and integration examples
are available at https://saltnpepper97.github.io/stasis-site/.

### Screen-lock tracking

Stasis automatically chooses the strongest lock-state source available:

- A foreground locker is tracked until its process exits.
- A positive login1 `LockedHint` takes authority for that lock episode and is
  followed until the real unlock. This supports service-backed lockers such as
  Veila without locker-specific configuration.
- A locker that forks into the background without publishing `LockedHint`
  cannot expose a reliable unlock state and should be run in the foreground.

login1 `Lock` and `Unlock` signals are requests rather than completed state.
`enable_loginctl_integration` therefore controls optional login1 sleep/wake
integration, not the lock-tracking method. `LockedHint` monitoring is automatic
through the login1 interface provided by systemd-logind and eLogind.

> [!IMPORTANT]
> **D-Bus session startup is required for full D-Bus features.**
> If you want `enable_dbus_inhibit` and other session-bus driven behavior to work reliably, start your compositor within a real D-Bus session (for example `niri-session`, `dbus-run-session`, or your compositor/distribution's recommended session launcher).
> If the compositor is not running in a proper session, inhibit monitoring may not activate.

---

## D-Bus Inhibit Support

Stasis supports inhibit messages from session D-Bus, including:

- `org.freedesktop.ScreenSaver` `Inhibit` / `UnInhibit`
- `org.gnome.SessionManager` `Inhibit` / `Uninhibit`
- `org.freedesktop.portal.Inhibit` (`Inhibit` / `CreateMonitor`) with release via `org.freedesktop.portal.Request.Close`

On the system bus, Stasis also polls login1 `ListInhibitors` for blocking
`idle` entries. These are authoritative lifetime-scoped holds (for example,
Codex holds one only while a turn is active). Stasis maps them to suspend-only
policy: lock, DPMS, and other earlier plan steps continue, while automatic
suspend waits until the inhibitor disappears.

Config key:

- `enable_dbus_inhibit true|false` (default true)

Use this when you want Stasis to honor session-bus inhibit requests from browsers, Steam, and portal clients, plus login1 blocking `idle` inhibitors.

Important separation:

- `enable_dbus_inhibit` covers browser/app inhibit traffic from session D-Bus and blocking login1 `idle` inhibitors from the system bus.
- `monitor_media` is only for non-browser media/audio state.
- Browser media inhibit is not handled by `monitor_media`; it is handled by D-Bus inhibit monitoring.

To let the display turn off without automatically suspending:

```rune
default:
  monitor_media true
  suspend_inhibit_media ["spotify" "mpd"]
  suspend_inhibit_apps ["handbrake"]
end
```

`suspend_inhibit_apps` and `suspend_inhibit_media` block only the automatic
suspend step. Monitored media that does not match `suspend_inhibit_media` keeps
the historical behavior and pauses the full idle plan. Media blacklist and
remote-player filters are applied first. When a suspend-only inhibitor clears,
Stasis resumes the remaining suspend timeout; manual `stasis trigger suspend`
still runs immediately.

---

## CLI Usage

    stasis info [--json]
    stasis blame [--json]
    stasis watch
    stasis tray
    stasis pause [for <duration> | until <time>]
    stasis resume
    stasis toggle-inhibit
    stasis trigger <step|all>
    stasis list actions
    stasis list profiles
    stasis profile <name|none>
    stasis report [today|week]
    stasis reload
    stasis stop

`stasis blame` explains why an idle action is held. It names active
manual/system pauses, matched applications and media, suspend-only blockers,
live D-Bus inhibit cookies or portal request handles, and login1 idle holds.
The `--json` form is a versioned snapshot for scripts. Active login1 idle holds
are also included structurally in `stasis info --json`.

`stasis tray` runs an optional StatusNotifier tray frontend. It does not replace
`stasis info --json`; Waybar and other status bars can keep using the JSON output
directly. Tray users should run both the daemon and tray frontend, for example
with `stasis.service` plus the optional `stasis-tray.service`.

The tray requires a StatusNotifier tray host, such as Waybar's tray module, KDE
Plasma, or another panel. The daemon remains headless and does not launch the
tray automatically.

### Event-driven shell integration

`stasis watch` writes one JSON object immediately, then another only when the
shell-facing state changes. This is intended for Quickshell and other shells
that need to react to Stasis without polling:

```json
{"state":"manual","paused":true,"manually_paused":true,"profile":"work"}
```

`state` is one of `waiting`, `active`, `inhibited`, `locked`, or `manual`.
The command stays connected until Stasis stops; each object is one line, so a
long-running process can parse the stream incrementally.

Quickshell can consume it with one long-running process:

```qml
import Quickshell.Io

Process {
  running: true
  command: ["stasis", "watch"]
  stdout: SplitParser {
    onRead: message => root.stasis = JSON.parse(message)
  }
}
```

---

## Compositor Support (app-inhibit)

Stasis integrates with each compositor's available IPC and standard Wayland protocols.

| Compositor | Support Status | Notes |
|-----------|----------------|-------|
| **Halley** | ✅ Full Support | Native IPC via `halleyctl`; matches window `app_id` |
| **Niri** | ✅ Full Support | Tested and working perfectly |
| **Hyprland** | ✅ Full Support | Native IPC integration |
| **labwc** | ⚠️ Limited | Process-based fallback |
| **River** | ⚠️ Limited | Process-based fallback |
| **Your Favorite** | 🤝 PRs Welcome | Help us expand support |

### Halley Notes

When running inside a Halley session, Stasis uses `halleyctl node list --json`
for app-inhibit tracking. `inhibit_apps` and `suspend_inhibit_apps` patterns
match Halley window `app_id` values, such as `firefox`, `kitty`, or
`steam_app_123`.

### River & labwc Notes

These compositors have IPC limitations that affect window enumeration.

- Stasis falls back to process-based detection
- Regex patterns may need adjustment
- Enable verbose logging to inspect detected applications

---

## Contributing

Thank you for helping improve Stasis!

Guidelines:
1. Bug reports and feature requests must start as issues
2. Packaging and compositor support PRs are welcome directly
3. Other changes should be discussed before submission

---

## ❤️ Support Development

If you find this project useful, consider sponsoring its development.

GitHub Sponsors helps ensure continued maintenance, faster bug fixes, and long-term improvements.

➡ https://github.com/sponsors/saltnpepper97

---

## License

Released under the GPL-3.0 License.

---

<p align="center">
  <sub>Built with ❤️ for the Wayland community</sub><br>
  <sub><i>Keeping your session in perfect balance between active and idle</i></sub>
</p>
