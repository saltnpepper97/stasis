<p align="center">
  <img src="assets/stasis.png" alt="Stasis Logo" width="200"/>
</p>

<h1 align="center">Stasis</h1>

<p align="center">
  <strong>A modern Wayland idle manager that knows when to step back.</strong>
</p>

<p align="center">
  Keep your session in perfect balance—automatically preventing idle when it matters, allowing it when it doesn't.
</p>

<p align="center">
  <img src="https://img.shields.io/github/last-commit/saltnpepper97/stasis?style=flat-square&color=%2328A745" alt="GitHub last commit"/>
  <img src="https://img.shields.io/aur/version/stasis?style=flat-square" alt="AUR Version"/>
  <img src="https://img.shields.io/badge/License-MIT-E5534B?style=flat-square" alt="MIT License"/>
  <img src="https://img.shields.io/badge/Wayland-00BFFF?style=flat-square&logo=wayland&logoColor=white" alt="Wayland"/>
  <img src="https://img.shields.io/badge/Rust-1.89+-orange?style=flat-square&logo=rust&logoColor=white" alt="Rust"/>
</p>

<p align="center">
  <a href="#-features">Features</a> •
  <a href="#-installation">Installation</a> •
  <a href="#-quick-start">Quick Start</a> •
  <a href="#compositor-support">Compositor Support</a> •
  <a href="#-contributing">Contributing</a>
</p>

---

## ✨ Features

Stasis doesn't just lock your screen after a timer—it understands context. Watching a video? Reading a document? Playing music? Stasis detects these scenarios and intelligently manages idle behavior, so you never have to jiggle your mouse to prevent an unwanted screen lock.

- **🧠 Smart idle detection** with configurable timeouts
- **🎵 Media-aware idle handling** – automatically detects media playback
- **🚫 Application-specific inhibitors** – prevent idle when specific apps are running
- **⏸️ Idle inhibitor respect** – honors Wayland idle inhibitor protocols
- **🛌 Lid events via DBus** – detect laptop lid open/close events to manage idle
- **⚙️ Flexible action system** – supports named action blocks and custom commands
- **🔍 Regex pattern matching** – powerful app filtering with regular expressions
- **📝 Clean configuration** – uses the intuitive [RUNE](https://github.com/saltnpepper97/rune-cfg) configuration language
- **⚡ Live reload** – update configuration without restarting the daemon

## 📦 Installation

### Arch Linux (AUR)

Install the stable release or latest development version:

```bash
# Stable release
yay -S stasis

# Or latest git version
yay -S stasis-git
```

Works with `paru` too:
```bash
paru -S stasis
```

### From Source

Build and install manually for maximum control:

```bash
# Clone and build
git clone https://github.com/saltnpepper97/stasis
cd stasis
cargo build --release --locked

# Install system-wide
sudo install -Dm755 target/release/stasis /usr/local/bin/stasis

# Or install to user directory
install -Dm755 target/release/stasis ~/.local/bin/stasis
```

## 🚀 Quick Start

1. **Install Stasis** using one of the methods above

2. **Create your configuration** at `~/.config/stasis/stasis.rune`

3. **Check the [wiki](https://github.com/saltnpepper97/stasis/wiki)** for detailed configuration examples

4. **Start the daemon** and enjoy intelligent idle management!

For configuration examples, CLI options, and advanced usage, visit the [full documentation](https://github.com/saltnpepper97/stasis/wiki).

## Compositor Support

Stasis integrates with each compositor's native IPC protocol for optimal app detection and inhibition.

| Compositor | Support Status | Notes |
|------------|---------------|-------|
| **Niri** | ✅ Full Support | Tested and working perfectly |
| **Hyprland** | ✅ Full Support | Native IPC integration |
| **labwc** | ⚠️ Limited | Process-based fallback (details below) |
| **River** | ⚠️ Limited | Process-based fallback (details below) |
| **Your Favorite** | 🤝 PRs Welcome | Help us expand support! |

### 📌 River & labwc Compatibility Notes

Both River and labwc have IPC protocol limitations that affect Stasis functionality:

- **Limited window enumeration:** These compositors don't provide complete window lists via IPC
- **Fallback mode:** Stasis uses process-based detection (sysinfo) for app inhibition
- **Pattern adjustments:** Executable names may differ from app IDs—check logs and adjust regex patterns accordingly

> **💡 Tip:** When using River or labwc, include both exact executable names and flexible regex patterns in your `inhibit_apps` configuration. Enable verbose logging to see which apps are detected.

### Want to Add Compositor Support?

We welcome contributions! Adding support typically involves:

1. Implementing the compositor's native IPC protocol
2. Adding window/app detection functionality  
3. Testing with common applications

Check existing implementations in the codebase for reference, and don't hesitate to open an issue if you need guidance.
### Want to Add Compositor Support?

We welcome contributions! Adding support typically involves:

1. Implementing the compositor's native IPC protocol
2. Adding window/app detection functionality  
3. Testing with common applications

Check existing implementations in the codebase for reference, and don't hesitate to open an issue if you need guidance.

## 🔧 About RUNE Configuration

Stasis uses **[RUNE](https://github.com/saltnpepper97/rune-cfg)**—a purpose-built configuration language that's both powerful and approachable.

**Why RUNE?**
- 📖 **Human-readable:** Clean syntax that makes sense at a glance
- 🔢 **Variables:** Define once, reference anywhere
- 🎯 **Type-safe:** Catch configuration errors before runtime
- 📦 **Nested blocks:** Organize complex configurations naturally
- 🔤 **Raw strings:** Use `r"regex.*"` for patterns without escaping hell
- 💬 **Comments:** Document your config with `#`
- 🏷️ **Metadata:** Add context with `@` annotations

RUNE makes configuration feel less like programming and more like describing what you want—because that's what a config should be.

## 🤝 Contributing

Contributions make Stasis better for everyone! Here's how you can help:

### Ways to Contribute

- 🐛 **Report bugs** – Open an issue with reproduction steps
- 💡 **Suggest features** – Share your use cases and ideas
- 🔧 **Submit PRs** – Fix bugs, add features, or improve code
- 📦 **Package for distros** – Make Stasis available to more users
- 📖 **Improve docs** – Better explanations, examples, and guides
- 🖥️ **Add compositor support** – Expand Wayland ecosystem compatibility

## 📄 License

Released under the [MIT License](LICENSE) – free to use, modify, and distribute.

---

<p align="center">
  <sub>Built with ❤️ for the Wayland community</sub><br>
  <sub><i>Keeping your session in perfect balance between active and idle</i></sub>
</p>
