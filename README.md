<p align="center">
  <img src="assets/stasis.png" alt="Stasis Logo" width="200"/>
</p>

<h1 align="center">Stasis</h1>

<p align="center">
  <i>A modern Wayland idle manager designed for simplicity and effectiveness.</i>
</p>

<p align="center">
  <a href="#features">Features</a> •
  <a href="#installation">Installation</a> •
  <a href="#configuration">Configuration</a> •
  <a href="#usage">Usage</a> •
  <a href="#contributing">Contributing</a>
</p>

## Features

- **🧠 Smart idle detection** with configurable timeouts
- **🎵 Media-aware idle handling** - automatically detects media playback
- **🚫 Application-specific inhibitors** - prevent idle when specific apps are running
- **⏸️ Idle inhibitor respect** - honors Wayland idle inhibitor protocols
- **⚙️ Flexible action system** - supports custom commands and timeouts for any action (lock, suspend, hibernate, shutdown, etc.)
- **🔍 Regex pattern matching** - powerful app filtering with regular expressions
- **📝 Clean configuration** - uses the intuitive [RUNE](https://github.com/saltnpepper97/rune-cfg) configuration language

## Compositor Support

Stasis uses each compositor's native IPC protocol for app inhibiting functionality.

| Compositor | app_inhibit Support | Status |
|------------|-------------------|--------|
| Niri | ✅ | Tested & Working |
| River | ✅ | Implemented (see below) |
| Hyprland | ✅ | Implemented |
| Others | ❌ | Send a PR! |

### River Support Notes

- **Limited window enumeration:** Unlike Niri or Hyprland, River does not provide a full IPC interface to list all windows. Stasis cannot reliably enumerate every active application via River.
- **Fallback mechanism:** When using River, Stasis falls back to process-based detection (sysinfo) for app inhibition.
- **Regex and app names may differ:** Because process-based detection relies on executable names and paths, some regex patterns or app IDs from Niri/Hyprland may not match exactly. Users may need to adjust inhibit_apps patterns for River.
- **Logging:** Stasis will log which apps were detected for inhibition, helping users refine their patterns.

> **Tip:** For best results with River, include both exact executable names and regex patterns for applications you want to inhibit.

**Want to add support for your compositor?** We welcome pull requests! Stasis integrates with each compositor's native IPC protocol, so adding support typically involves implementing the specific IPC calls for window/app detection.

## Installation

### Arch Linux (AUR)

Available on the AUR as `stasis`.

Using `yay`:
```bash
yay -S stasis
```

Using `paru`:
```bash
paru -S stasis
```

### From Source

```bash
git clone https://github.com/saltnpepper97/stasis
cd stasis
cargo build --release --locked
sudo install -Dm755 target/release/stasis /usr/local/bin/stasis
```

Or install to your local bin directory:
```bash
install -Dm755 target/release/stasis ~/.local/bin/stasis
```

## Configuration

Stasis uses a configuration file written in **[RUNE](https://github.com/saltnpepper97/rune-cfg)**, a simple but effective configuration language designed for clarity and ease of use.

### Configuration File Location

Create your config file at:
```
~/.config/stasis/stasis.rune
```

The default config can be found at:
```
/usr/share/doc/stasis/stasis.rune
```

To get started quickly, copy the default config:
```bash
mkdir -p ~/.config/stasis
cp /usr/share/doc/stasis/stasis.rune ~/.config/stasis/stasis.rune
```

### Example Configuration

Create your configuration at `~/.config/stasis/stasis.rune`:

```rune
@description "Stasis configuration example"

app_default_timeout 300

idle:
  resume_command "systemctl resume-sessions"
  monitor_media true
  respect_idle_inhibitors true
  
  inhibit_apps [
    "spotify"
    "mpv"
    r".*\.exe"
    r"steam_app_*\.*"
  ]
  
  lock_screen:
    timeout = app_default_timeout # 5 minutes
    command "hyprlock"
  end
  
  suspend:
    timeout 1800 # 30 minutes
    command "systemctl suspend"
  end
end
```

> **📖 For complete configuration details, see:** `man 5 stasis`

### Configuration Options

#### Global Settings
- `app_default_timeout` - Default timeout in seconds for actions

#### Idle Block Configuration
- `resume_command` - Command to run when resuming from idle
- `monitor_media` - Automatically detect media playback to prevent idle
- `respect_idle_inhibitors` - Honor Wayland idle inhibitor protocols  
- `inhibit_apps` - Array of application names/patterns to prevent idle

#### Built-in Actions

##### Lock Screen
- `timeout` - Time in seconds before locking (can reference variables)
- `command` - Command to execute for screen locking

##### Suspend
- `timeout` - Time in seconds before suspending
- `command` - Command to execute for system suspend

#### Custom Actions

You can define any custom action block with its own timeout and command:

```rune
idle:
  hibernate:
    timeout 3600  # 1 hour
    command "systemctl hibernate"
  end
  
  shutdown:
    timeout 7200  # 2 hours
    command "systemctl poweroff"
  end
  
  notify_long_idle:
    timeout 600   # 10 minutes
    command "notify-send 'Long idle detected'"
  end
end
```

### App Inhibitor Patterns

The `inhibit_apps` array supports both literal app names and regex patterns:

```rune
inhibit_apps [
  "spotify"              # Exact match
  "mpv"                  # Exact match
  r".*\.exe"            # Regex: any .exe file
  r"steam_app_*\.*"     # Regex: Steam applications
  r"firefox.*"          # Regex: Firefox and variants
]
```

## Usage

### Running Manually
```bash
stasis
```

### Systemd Service (Recommended)

Create `~/.config/systemd/user/stasis.service`:

```ini
[Unit]
Description=Stasis Wayland Idle Manager
After=graphical-session.target

[Service]
ExecStart=%h/.local/bin/stasis
# Use /usr/local/bin/stasis if installed system-wide
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

Enable and start the service:
```bash
systemctl --user enable stasis.service
systemctl --user start stasis.service
```

Check service status:
```bash
systemctl --user status stasis.service
```

## About RUNE

Stasis uses **[RUNE](https://github.com/saltnpepper97/rune-cfg)**, a configuration language designed to be simple but effective. RUNE features:

- **Clean, readable syntax** - Easy to write and understand
- **Variable support** - Define and reference variables
- **Nested configuration blocks** - Organize complex configurations
- **Array and string literals** - Flexible data types
- **Regex pattern support** - Use `r"pattern"` syntax for powerful matching
- **Comments** - Document your config with `#`
- **Type safety and validation** - Catch errors early

## Troubleshooting

### Common Issues

**Stasis not detecting apps:**
- Ensure your compositor is supported (see [Compositor Support](#compositor-support))
- Check that the app names in `inhibit_apps` match the actual application names

**Service not starting:**
- Verify the `ExecStart` path in your systemd service file
- Check service logs: `journalctl --user -u stasis.service`

**Configuration errors:**
- Validate your RUNE syntax
- Check the manual: `man 5 stasis`

## Contributing

We welcome contributions! Here's how you can help:

- 🐛 **Report bugs** by opening an issue
- 💡 **Request features** with detailed use cases  
- 🔧 **Submit pull requests** for bug fixes or new features
- 📦 **Package for your distro** and let us know so we can link to it
- 📖 **Improve documentation** or add examples

### Adding Compositor Support

Want to add support for your favorite compositor? We'd love your help! Adding support typically involves:

1. Implementing the compositor's native IPC protocol
2. Adding window/app detection functionality
3. Testing with real applications

Check out the existing implementations in the source code for reference.

## License

[MIT License](LICENSE)

---

<p align="center">
  <i>Stasis - keeping your Wayland session in perfect balance between active and idle.</i>
</p>
