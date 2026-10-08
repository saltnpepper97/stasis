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
- 🎮 Gamepad input activity, including hotplug and stick-drift filtering
- 🎮 Automatic game detection with an optional blacklist; no per-game inhibit rules needed for detected games
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

Services collect external signals, including periodic application, game, and
media observations. The state machine makes timing and pause decisions from
explicit events.

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
- PipeWire with `pw-dump`, or native PulseAudio using `pactl` (runtime, recommended for media/microphone detection)
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

### Automatic game detection

```rune
default:
  monitor_games true
  game_blacklist [ ]
  extra_games [ ]
end
```

`monitor_games` defaults to `true`. Stasis uses
[`lib_game_detector`](https://github.com/Rolv-Apneseth/lib_game_detector) to
discover installed titles from Steam (including shortcuts), Heroic, Lutris,
Bottles, Prism/ATLauncher Minecraft instances, Itch, and Faugus. The catalogue
refreshes every 60 seconds and on reload/profile changes. Runtime observations
are polled once per second. Discovery is local and never launches games.

An installed title alone does not prevent idle. A running game identified by
its executable or script path, Proton/Wine game identity, Minecraft instance
arguments, or a Steam app window pauses the full idle plan. Helpers such as
Steam's UI and `wineserver` do not count as games. Halley, Hyprland, and Niri
provide a Steam app-ID window fallback when catalogue metadata is missing;
process detection works independently of those compositor integrations.

To allow particular games or launcher-registered tools to idle:

```rune
default:
  monitor_games true
  game_blacklist ["aseprite" "picocad" "steam:440" r"^minecraft - .*"]
  extra_games [ ]
end
```

The blacklist uses the same syntax as `media_blacklist`: lowercase-normalized
substring literals or regular expressions. It matches each game's title,
identity (`steam:440` or `steam_app_440` for Steam), source, and installation
directory. Profiles can replace the list or clear it with `game_blacklist [ ]`,
and can disable detection with `monitor_games false`. Changes take effect on
reload without restarting Stasis.

For a manually installed game outside the discovered catalogue, add a fallback:

```rune
default:
  monitor_games true
  game_blacklist [ ]
  extra_games ["jazz2.exe" "/games/GOG/My Game" r"^my_game$"]
end
```

`extra_games` accepts exact executable names or compositor app IDs, absolute
installation paths, and regexes matched against executable names/paths or app
IDs. Names are case-insensitive; a name without `.exe` can match its Windows
executable. A path matches the game executable/script or Minecraft instance
arguments within that directory. Generic Wine/Java hosts and runtime helpers
alone do not count. GOG titles registered with a supported launcher such as
Heroic can already be discovered automatically.

The blacklist applies to automatic and extra game observations alike, and the
Boolean disables both. Profiles replace or clear `extra_games` just as they do
the blacklist. Extra observations show their executable/app identity (or the
configured directory name) and source `Extra games` in status.

`stasis info`, its JSON `games` object, and the tray tooltip show catalogue size,
running games, and detection errors. The JSON object also lists blacklisted observations
under `ignored`; `stasis blame` reports eligible game holds separately.

Unsupported sources, missing paths, and ambiguous shared directories can need
an `extra_games` entry. If Steam metadata is missing, the fallback shows
`Steam app <id>`; blacklist it by ID. Existing `inhibit_apps`, media, D-Bus, and
compositor inhibitors remain independent. Remove old broad game rules such as
`r"steam_app_.*"` or `r".*\.exe"` if you want `game_blacklist` to control the
automatic game hold. The blacklist does not suppress physical controller input.

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

## Lid-close Grace Period

By default, `pause_on_lid_close true` pauses the plan while a laptop lid is
closed. Set it to `false` to start a fresh countdown on lid closure, even if
the compositor has not reported idle. Opening the lid cancels that countdown,
runs `lid_open_action` and any applicable plan resume commands, and waits for
normal compositor idle again. Manual pauses and inhibitors still apply.

`lid_close_action` runs immediately. For lock + display-off followed by suspend
after 10 seconds, use a close command that returns promptly and a suspend step
with `timeout 10`. See [the complete laptop example](examples/lid-grace-period.rune).
This runs the same configured plan used for ordinary idle, including its
sequential step timeouts and notifications; it does not create a separate lid plan.

Stasis reads UPower's current lid state on startup. Enable
`enable_loginctl_integration true` for wake handling: waking with the lid still
closed reruns the close command and starts another countdown. Opening the lid
does not unlock the session.

For Stasis to control the grace period, logind and any desktop power manager
must allow lid closure without immediately suspending. See
[logind's lid settings](https://www.freedesktop.org/software/systemd/man/latest/logind.conf.html).
Stasis does not change those system settings.

## Gamepad Activity

`monitor_gamepad true` (the default) counts Linux controller buttons, D-pad,
sticks, and triggers as user activity independently of the compositor. No
per-game configuration is needed for this input detection. Controllers can be
plugged in or disconnected while Stasis is running.

Held controls keep resetting the idle timer. A connected controller at rest
does not prevent idle; timers restart from the last input when controls are
released. Sticks use a 15% deadzone on either side of centre, triggers use 15%
of their range, and larger driver-reported deadzones are respected. Controller
motion sensors and touchpads are ignored.

Stasis needs read access to the controller's `/dev/input/event*` device. It
logs inaccessible controllers and retries if access becomes available. Desktop
sessions commonly grant access through logind/udev device ACLs; otherwise use
your distribution's controller-specific udev access rules. Stasis never grabs
the device, so games continue receiving input normally.

Set `monitor_gamepad false` under `default:` or a profile to disable monitoring.
Changes take effect on `stasis reload` or profile selection. Existing configs
receive the missing setting through the usual backup-preserving migration;
explicit values are kept.

Controller input works outside games too. The tray tooltip and `stasis info`
show monitored controller names and whether input was detected recently.
`stasis info --json` includes a `gamepad` object with `monitoring`, `devices`, `input_recent`, and `last_activity_ms`.
Recent input remains visible for three seconds so short presses can be seen
across tray refreshes. Input resets the idle timer rather than setting a manual
pause, so `Paused: no` is normal while controls are being used.

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

### Audio backends

Stasis prefers native PipeWire and reads its stream graph through
[`pw-dump`](https://docs.pipewire.org/page_man_pw-dump_1.html). It connects to
PipeWire directly; Stasis does not require `pipewire-pulse` or `pactl` on a
PipeWire setup. If native PipeWire is unavailable, it can use `pactl` against a
genuine PulseAudio server. A PulseAudio compatibility server backed by PipeWire
is rejected as a fallback. Install the native tools for your selected server;
the NixOS and Home Manager modules include both tool sets in the service PATH.
This controls Stasis's monitoring connection; applications may still use
`pipewire-pulse` for their own audio.

The backend is selected automatically without a new config setting. Both
non-browser media monitoring (`monitor_media`) and browser microphone detection
(`enable_dbus_inhibit`) use this selection. `PIPEWIRE_REMOTE` and PulseAudio
server/session environment variables are inherited from the Stasis process.
Startup logs identify the selected backend. Failed queries preserve the last
valid observation, have a two-second command timeout, and retry connection;
discovery retries every five seconds while no backend is available.

Media monitoring counts running, unmuted playback streams. Capture streams,
idle/paused playback, browsers, games, synthetic speech, and system sounds do
not become media holds. `media_blacklist`, `ignore_remote_media`, and
`suspend_inhibit_media` retain their filtering behavior. Browser microphone
holds require a running, unmuted browser capture stream and remain independent
of non-browser playback and D-Bus request lifetimes.

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

The right-click menu shows the current status and tray actions. Controller and
game details remain in the hover tooltip.

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

Stasis source is released under GPL-3.0-only. The compiled program includes
`lib_game_detector` under AGPL-3.0-only; see [third-party licensing](THIRD_PARTY.md)
for the combined-work terms and distribution notes.

---

<p align="center">
  <sub>Built with ❤️ for the Wayland community</sub><br>
  <sub><i>Keeping your session in perfect balance between active and idle</i></sub>
</p>
