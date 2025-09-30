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
  <a href="#getting-started">Getting Started</a> •
  <a href="#contributing">Contributing</a>
</p>

## Features

- **🧠 Smart idle detection** with configurable timeouts
- **🎵 Media-aware idle handling** - automatically detects media playback
- **🚫 Application-specific inhibitors** - prevent idle when specific apps are running
- **⏸️ Idle inhibitor respect** - honors Wayland idle inhibitor protocols
- **⚙️ Flexible action system** - supports named action blocks and custom commands
- **🔍 Regex pattern matching** - powerful app filtering with regular expressions
- **📝 Clean configuration** - uses the intuitive [RUNE](https://github.com/saltnpepper97/rune-cfg) configuration language
- **🔧 CLI options** - verbose logging, version info, and live config reloading
- **⚡ Live reload** - update configuration without restarting the daemon

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

Available on the AUR as `stasis` or `stasis-git` for latest commit.

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
cargo build --release --locked --features "wlroots_virtual_keyboard"
sudo install -Dm755 target/release/stasis /usr/local/bin/stasis
```

Or install to your local bin directory:
```bash
install -Dm755 target/release/stasis ~/.local/bin/stasis
```

## Getting started

### Visit the [wiki](https://github.com/saltnpepper97/stasis/wiki)

## About RUNE

Stasis uses **[RUNE](https://github.com/saltnpepper97/rune-cfg)**, a configuration language designed to be simple but effective. RUNE features:

- **Clean, readable syntax** - Easy to write and understand
- **Variable support** - Define and reference variables
- **Nested configuration blocks** - Organize complex configurations
- **Array and string literals** - Flexible data types
- **Raw string support** - Use `r"pattern"` syntax for regex patterns
- **Comments** - Document your config with `#`
- **Type safety and validation** - Catch errors early
- **Metadata** - Use `@` symbol to denote metadata

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
  <i>keeping your Wayland session in perfect balance between active and idle.</i>
</p>


