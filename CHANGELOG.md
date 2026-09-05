# Changelog
All notable changes to this project will be documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.6.1] - 2026-09-05

### Fixed

- The manually paused tray title now says exactly `Stasis paused (manually)`.

## [1.6.0] - 2026-09-05

### Added

- `stasis blame` reports the current manual, system, application, media, suspend-only, and D-Bus blockers, with a stable `--json` form for tooling.
- Blocking login1 `idle` inhibitors are now discovered directly from systemd-logind and treated as suspend-only holds, so lock and DPMS can still run while tools such as Codex prevent automatic sleep ([#98](https://github.com/saltnpepper97/stasis/issues/98)).

### Fixed

- Closed desktop-portal inhibit requests are now removed immediately instead of remaining latched while unrelated browser source capture is active.
- The tray presents manual pause once as `Stasis paused` instead of repeating the `manual` state.
- Configuration upgrades now preserve customized files and symlinks, rename only known legacy keys, and backfill compatibility fields instead of replacing the entire file with the current bootstrap ([#99](https://github.com/saltnpepper97/stasis/issues/99)).

## [1.5.1] - 2026-08-15

### Fixed

- `stasis tray` now allows only one instance per desktop session. Additional launches exit cleanly without registering a duplicate tray item.

## [1.5.0] - 2026-08-13

### Added

- `suspend_inhibit_apps` lets selected running applications block only automatic suspend while allowing earlier idle-plan actions such as lock, DPMS, and hardware low-power mode ([#96](https://github.com/saltnpepper97/stasis/issues/96)).
- `suspend_inhibit_media` applies the same suspend-only behavior to selected monitored media sources; other eligible media retains the historical full-plan inhibit behavior.
- Suspend-only holds preserve the remaining suspend timeout and resume it when the last matching inhibitor clears.

### Changed

- Configuration migration now preserves outdated or invalid files as numbered backups and installs the current bootstrap unchanged. It no longer maintains a separate field-by-field parser or overwrites an existing backup.

### Fixed

- Idle plans now require verified inhibitor-aware compositor idle state, preventing browser-inactive, lid-open, sleep-resume, and startup paths from advancing toward lock while the user is active ([#95](https://github.com/saltnpepper97/stasis/issues/95)).
- Compositors supporting `ext-idle-notify-v1` version 2 now use its inhibitor-independent input notification alongside the inhibitor-aware notification. Version 1 compositors retain a conservative fallback.
- The Quit Tray menu entry no longer requests a theme icon that some tray hosts render as a missing texture.

## [1.4.1] - 2026-07-30

### Fixed

- Desktop bootstrap configs now include the hardware low-power settings already
  present in the laptop template.
- Builds targeting musl libc no longer link against glibc's `malloc_trim`.
- The NixOS service now includes `libnotify` in its runtime path so generated
  desktop notifications can invoke `notify-send`.
- Lock tracking now treats a positive login1 `LockedHint` as authoritative for
  that episode, preventing short-lived clients such as `veila lock --wait-ready`
  from producing a false unlock when they exit. Foreground process lifetime
  remains the fallback when no positive hint is observed.
- login1 `Lock` and `Unlock` request signals no longer masquerade as completed
  session state; `enable_loginctl_integration` now gates only sleep/wake
  monitoring while `LockedHint` remains automatic.

### Changed

- Configuration reload responses now clearly state whether Stasis retained the
  active profile or returned to the base configuration.

## [1.4.0] - 2026-07-12

### Added

- **Event-driven shell integration with `stasis watch`**
  - `stasis watch` keeps a local IPC connection open and emits newline-delimited JSON: the current state immediately, then updates only when the shell-facing state changes.
  - Events include `state`, `paused`, `manually_paused`, and the selected `profile`, covering pause/resume, profile changes, lock state, and inhibitor transitions without polling.

- **Logind LockedHint tracking**
  - Stasis now monitors logind's `LockedHint` session property via `PropertiesChanged`, independent of `enable_loginctl_integration`.
  - This enables lock tracking for compositors and lock screens that set `LockedHint` (e.g. a Quickshell fork with the lockhint feature) without requiring loginctl signal mode.
  - The property is read once at startup (emitting `SessionLocked` if already locked) and watched for ongoing changes.
  - Non-fatal: if the property is unavailable or never set, Stasis falls through to existing tracking methods (loginctl signals, process tracking) with no regression.

- **Hardware low-power mode**
  - New `low_power_when_idle` and `low_power_when_idle_timeout` config keys (global, with profile override).
  - After the DPMS step fires and the timeout elapses, Stasis applies conservative power-saving to supported hardware: GPU runtime PM (`power/control` → `auto`) and, for amdgpu, dynamic power management (`power_dpm_force_performance_level` → `auto`).
  - Snapshot-based: every value changed is recorded and restored exactly on any resume path (activity, unlock, lid open, profile change, config reload) and on daemon shutdown.
  - Requires write access to GPU sysfs files (elevated privileges or a udev rule); unwritable files are skipped and logged with no broken state left behind.

- **Power-saving telemetry & `stasis report`**
  - The daemon now records completed idle episodes (display off, hardware low-power, suspend) to `~/.local/state/stasis/report.jsonl`.
  - New `stasis report [today|week]` subcommand aggregates time spent in each state and prints a rough estimated energy savings (kWh).
  - `stasis report` reads the telemetry file directly; it does not require the daemon to be running. Estimates are conservative approximations, not wall-metered measurements.

### Changed

- **Config key rename: `enable_loginctl` → `enable_loginctl_integration`**
  - Renamed at the global and per-profile level for clarity. The built-in migrator rewrites the legacy `enable_loginctl` key automatically on first launch.

---

## [1.3.0] - 2026-06-11

### Added

- **Notification icon support**
  - Stasis-generated notifications now pass an icon to `notify-send` with `-i`.
  - The default notification icon is `stasis`.
  - Added `notification_icon` config support globally and per step.
  - Setting `notification_icon ""` disables the default icon.
  - Packaged builds install `assets/stasis.png` as `share/icons/hicolor/256x256/apps/stasis.png`.

### Changed

- **Notification execution**
  - Notifications now invoke `notify-send` directly with arguments instead of via `sh -lc`, avoiding shell escaping issues.

- **Dependencies**
  - Updated `rune-cfg` from `0.4.3` to `0.5.0`.

---

## [1.2.0] - 2026-05-25

### Added

- **Halley app-inhibit support**
  - Stasis now detects Halley sessions and queries `halleyctl node list --json` for app tracking.
  - `inhibit_apps` patterns match Halley window `app_id` values such as `firefox`, `kitty`, or `steam_app_123`.
  - Halley IPC failures keep the previous inhibitor count, matching the existing compositor backend behavior.

- **Optional StatusNotifier tray frontend**
  - Added `stasis tray`, an optional system tray frontend that leaves the daemon and Waybar JSON integration unchanged.
  - The tray shows current daemon state in its tooltip and provides menu actions for toggle inhibit, pause, resume, reload config, and quitting only the tray process.
  - Added a dedicated tray icon asset and optional `stasis-tray.service` systemd user unit.
  - NixOS and Home Manager modules can enable the tray with `services.stasis.tray.enable`.

- **Updated app icon**
  - Refreshed the main Stasis icon asset used by README/project branding.

### Changed

- **Pre-action notification gating aligned with true idle state**
  - Tick processing now hard-gates while `debounce_pending` is active, so `notify_before_action` and step actions cannot fire while Stasis is still waiting for a real idle edge.
  - `notify_on_unpause` behavior remains scoped to `PauseExpired` (auto-resume from `stasis pause for/until`) and is not used for generic inhibitor transitions.

- **`stasis info` state text simplification**
  - Waybar/`--json` `text` now emits short, intuitive state labels: `waiting`, `active`, `inhibited`, `locked`, and `manual` (for explicit manual pause).
  - Human-readable status/tooltip state lines were shortened to match (`State: waiting`, `State: manual`, etc.).

- **Portal D-Bus inhibit tracking now uses request handles**
  - Session portal inhibits are tracked per returned request handle (from `org.freedesktop.portal.Inhibit.Inhibit` method returns).
  - `org.freedesktop.portal.Request.Close` now clears the matching handle rather than relying on coarse sender-only state.
  - This reduces incorrect clear/retain behavior when browsers recycle inhibit requests.

- **Runtime browser-call close guard**
  - On portal handle close edges, Stasis now applies a browser source-output guard before final inactive transitions.
  - This helps avoid mid-call inhibit drops when browser/portal close behavior is noisy.

- **`stasis info` adds D-Bus inhibition info**
  - Adds a line to the human-readable status/tooltip about the current state of D-Bus based inhibition.

### Fixes

- **Reset idle timer when compositor sends multiple CompositorIdled events without a CompositorResumed**
  - This fixes the behavior on niri where Stasis would just ignore keyboard and mouse activity and lock the screen anyway

### Notes

- **Web Discord limitation (no mic attached)**
  - Browser/portal may still uninhibit during a Discord web call when no microphone source-output is attached.
  - This behavior currently cannot be fixed reliably from Stasis side alone.

- **Startup media gating investigation**
  - Investigating a future startup-only audio gate so pre-existing playback at daemon start can keep Stasis paused until activity is clearly inactive.

---

## [1.1.0] - 2026-03-05

### Changed

- **Session D-Bus inhibit support restored and expanded**
  - Stasis now monitors session-bus inhibit method calls again:
    - `org.freedesktop.ScreenSaver` `Inhibit` / `UnInhibit`
    - `org.gnome.SessionManager` `Inhibit` / `Uninhibit`
    - `org.freedesktop.portal.Inhibit` `Inhibit`
    - release via `org.freedesktop.portal.Request.Close`
  - Inhibit tracking is sender-based to avoid drift from unbalanced inhibit/uninhibit calls.
  - Portal sender state is released by explicit close/disconnect rather than timeout expiry.

- **Config key cleanup for D-Bus inhibit gate**
  - Canonical config key is now `enable_dbus_inhibit`.
  - Legacy key parsing fallback was removed from runtime config loading.
  - Built-in migration rewrites legacy `listen_browser_dbus_inhibit` to `enable_dbus_inhibit`.

- **Config parser naming cleanup**
  - Removed misleading `legacy_*` naming in plan parse internals where behavior is not legacy-only.

- `media.rs`: replaced `sh -lc pactl` invocation with a direct `pactl` call, removing the unnecessary shell wrapper.

- **media: complete overhaul of sink-input and source-output detection**
  - Removed `pactl list sinks` RUNNING gate — sink state persists after leaving a Discord call, causing false positives that held inhibitors open indefinitely.
  - Added `pactl list source-outputs` parsing. Any active (uncorked) source-output counts as a call inhibitor (`call` bucket), independent of sink-input state. This correctly reflects mic capture as an active session signal.
  - Firefox sink-inputs are now deduped by `media.name` (tab title) rather than `object.serial`, preventing PipeWire from double-counting the same tab via multiple sink-input blocks.
  - Firefox sink-inputs whose `media.name` contains `"discord"` are always suppressed (covers `"• Discord | General | …"` tab titles).
  - Any browser PID found in `capturing_pids` (has an active source-output) has its generic-named sink-inputs suppressed. Real media titles (YouTube, etc.) are not affected and always pass through.
  - Chromium/Vivaldi sink-inputs with generic `media.name` values (`"Playback"`, `"AudioStream"`, etc.) are suppressed when the PID is actively capturing — correctly handling Vivaldi's Discord call audio zombie without blocking legitimate YouTube playback.
  - `playing_streams_total` and `playing_streams_chromium` heuristic counters are now incremented **after** filtering, ensuring the chromium single-stream heuristic fires correctly even when a filtered-out Firefox Discord tab is simultaneously uncorked.
  - Reduced `chromium_single_grace_ms` from 5 000 ms to 1 500 ms for snappier post-call cleanup.
  - Replaced large closure-with-18-parameters pattern with `macro_rules! flush!()` in both sink-input and source-output parsers, improving readability and eliminating borrow-checker friction.

- **Logging noise reduction (IPC & event scopes)**
  - Gated `eventline::scope!("event")` behind `--verbose`, eliminating `done: event#N` spam during normal operation.
  - Gated per-request IPC scopes behind `--verbose`, preventing excessive log output caused by frequent `stasis info --json` polling (e.g. Waybar modules).
  - Normal daemon mode now produces clean, stable logs while preserving full tracing in verbose mode.

- **Bootstrap configuration defaults**
  - Updated generated default configs to better reflect current suspend/lock semantics.
  - Clarified `pre_suspend_command` usage in generated templates and documentation.
  - Added explicit `enable_dbus_inhibit` knob documentation in generated templates.
  - Desktop and laptop templates now more clearly separate lock-step behavior from suspend behavior.

- **Documentation consistency**
  - README and man pages now consistently document `enable_dbus_inhibit`.
  - Added an explicit warning that compositors should be launched in a real session context (e.g. `niri-session`, `dbus-run-session`, or compositor-recommended launcher) for reliable session D-Bus features.

- **Suspend semantics clarification**
  - `pre_suspend_command` is now documented as intended for use with backgrounded (`daemonize`) suspend commands.
  - Users with a `lock_screen:` plan step no longer need `pre_suspend_command` in most cases.
  - Documentation updated to prevent misconfiguration where suspend races ahead of the locker.

- **IPC stability polish**
  - Reduced log overhead during frequent `info` calls.
  - Improved daemon cleanliness under heavy polling scenarios.

- **Release binary size optimization profile**
  - Added a `profile.release` configuration in `Cargo.toml` tuned for smaller binaries (`opt-level = "z"`, `lto`, single codegen unit, symbol stripping, and `panic = "abort"`).

### Fixed

- Fixed a browser-activity edge case at timestamp `0` where startup idle-edge handling could be skipped due to inclusive activity expiry comparison.

- Eliminated excessive `done: event#…` log lines during normal operation.
- Prevented Waybar polling from flooding daemon logs.
- Reduced log churn under steady-state idle operation.
- Fixed lingering daemon/zombie behavior when started from a terminal and the terminal session closed by handling `SIGHUP` and `SIGTERM` as clean shutdown signals (not only `SIGINT`).
- Fixed inhibitor count staying permanently elevated after leaving a Discord call in any browser.
- Fixed Firefox counting one playing tab as two due to PipeWire creating duplicate sink-input blocks.
- Fixed Chromium/Vivaldi Discord zombie stream holding `local=1` after a call ends.
- Fixed chromium single-stream heuristic never firing when a filtered Firefox Discord tab was simultaneously uncorked (inflating `streams_total` and blocking the heuristic).
- Fixed session inhibit handling regressions where D-Bus `Inhibit`/`UnInhibit` traffic was not being honored.
- Fixed portal inhibit state dropping during long playback sessions (notably on labwc, and intermittently on niri) by removing timeout-based expiry and honoring explicit close/disconnect edges.

---

## [1.0.0] - 2026-02-26

### Highlights
- Complete event-driven rewrite
- Improved memory handling and streamlined internals
- Services moved out of `core/`
- Eventline refactor and cleanup
- Built-in configuration migrator
- New logo and visual identity
- Logs moved to XDG-compliant state directory

### Added
- **Event-driven architecture** — timers, system signals, lid events, loginctl events, IPC pauses, and media state changes are now coordinated through a structured event system, replacing sequential and implicit flow. Results in more predictable state transitions, cleaner internal boundaries, reduced memory overhead, improved long-running stability, and a more extensible foundation for future features.
- **Built-in config migrator** — on first launch of 1.0.0, Stasis automatically migrates existing Rune configurations to the latest schema. Most users will not need to manually edit their configuration after upgrading.

### Changed
- **Media monitoring** — the browser-based media bridge has been removed. Stasis now relies exclusively on `pactl` for media detection. `pipewire-pulse` or `pulseaudio` must be installed and available.
- **`use_loginctl` → `enable_loginctl_integration`** — renamed and moved to the top level under `default:` in the Rune configuration. No longer defined inside a nested block.
  ```rune
  default:
    enable_loginctl_integration true
  end
  ```
- **Log directory** — logs now live in `~/.local/state/stasis/` (previously `~/.cache/stasis/`), aligning with the XDG Base Directory Specification.
- **Services** moved out of `core/`.
- **Eventline** received structural updates and cleanup.

### Fixed
- Resolved memory issues related to event handling
- Eliminated instability from the legacy media bridge
- Improved long-running session stability
- Streamlined internal code paths and reduced state drift
