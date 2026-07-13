# bakawm

A Wayland compositor/window manager built with [Rust](https://www.rust-lang.org/) and [Smithay](https://github.com/Smithay/smithay).

bakawm is an early-stage Wayland WM that uses Smithay as its compositor framework. The project is currently in active development and not yet ready for daily use.

## Build Dependencies

Before building bakawm, make sure the following system libraries and tools are installed:

- Rust toolchain (1.85+)
- pkg-config
- libudev
- libdrm
- libgbm
- libinput
- libxkbcommon
- libseat (seatd)
- libdisplay-info
- wayland-server
- libEGL (Mesa)
- libx11 (optional, for X11 backend)
- libpipewire-0.3 (optional, for screen recording)
- libdbus-1 (optional, for screen recording)

On Debian/Ubuntu:

```bash
sudo apt install build-essential pkg-config libudev-dev libdrm-dev libgbm-dev \
    libinput-dev libxkbcommon-dev libseat-dev libdisplay-info-dev \
    wayland-protocols libegl-dev libx11-dev libx11-xcb-dev \
    libpipewire-0.3-dev libdbus-1-dev
```

On Fedora:

```bash
sudo dnf install gcc pkg-config systemd-devel libdrm-devel gbm-devel \
    libinput-devel libxkbcommon-devel libseat-devel libdisplay-info-devel \
    wayland-devel mesa-libEGL-devel libX11-devel \
    pipewire-devel dbus-devel
```

On Arch Linux:

```bash
sudo pacman -S base-devel pkg-config systemd-libs libdrm gbm libinput \
    libxkbcommon seatd libdisplay-info wayland-protocols mesa libx11 \
    pipewire dbus
```

On Void Linux:

```bash
sudo xbps-install base-devel pkg-config libudev-devel libdrm-devel \
    gbm-devel libinput-devel libxkbcommon-devel seatd-devel \
    libdisplay-info-devel wayland-devel mesa-devel libX11-devel \
    pipewire-devel dbus-devel
```

On Alpine Linux:

```bash
sudo apk add build-base pkgconf eudev-dev libdrm-dev mesa-gbm-dev \
    libinput-dev libxkbcommon-dev seatd-dev libdisplay-info-dev \
    wayland-dev mesa-dev libx11-dev \
    pipewire-dev dbus-dev
```

## Building

```bash
cargo build --release
```

### Feature Flags

bakawm uses Cargo features to control which backends and integrations are compiled. The default features include systemd integration. If your system does not use systemd, you need to adjust the feature flags.

| Feature | Default | Description |
|---------|---------|-------------|
| `udev` | Yes | DRM/KMS backend for running on a TTY |
| `winit` | Yes | Nested window backend (X11/Wayland client) |
| `x11` | Yes | X11 backend |
| `egl` | Yes | EGL hardware acceleration |
| `xwayland` | Yes | XWayland support |
| `libei` | Yes | libei input emulation support |
| `systemd` | Yes | systemd service integration (sd-notify) |
| `dinit` | No | dinit service integration marker |
| `xdp-gnome-screencast` | Yes | Screen recording via PipeWire (org.gnome.Mutter.ScreenCast D-Bus interface) |

### Building without systemd

For distributions that use dinit (e.g. Artix Linux):

```bash
cargo build --release --no-default-features --features "egl,winit,x11,udev,xwayland,libei,dinit"
```

For distributions without any service manager (e.g. Void Linux, Alpine Linux):

```bash
cargo build --release --no-default-features --features "egl,winit,x11,udev,xwayland,libei"
```

### Building without screen recording

To build without PipeWire/D-Bus screen recording support:

```bash
cargo build --release --no-default-features --features "egl,winit,x11,udev,xwayland,libei,systemd"
```

## Installation

### Using Makefile

The Makefile installs the binary, session script, desktop file, and portal configuration. Service files for systemd and dinit are installed separately.

Install the binary and common resources:

```bash
sudo -E make install
```

Install systemd service files (systemd-based distributions):

```bash
sudo -E make install-systemd
```

Install dinit service files (dinit-based distributions):

```bash
sudo -E make install-dinit
```

You can customize the installation prefix:

```bash
make install PREFIX=/usr
```

### File Locations

| File | Destination |
|------|-------------|
| `bakawm` | `/usr/local/bin/` |
| `bakawm-ctl` | `/usr/local/bin/` |
| `bakawm-session` | `/usr/local/bin/` |
| `bakawm.desktop` | `/usr/local/share/wayland-sessions/` |
| `bakawm-portals.conf` | `/usr/local/share/xdg-desktop-portal/` |
| `bakawm.service` (systemd) | `/usr/local/lib/systemd/user/` |
| `bakawm-shutdown.target` (systemd) | `/usr/local/lib/systemd/user/` |
| `dinit/bakawm` (dinit) | `/usr/local/lib/dinit.d/user/` |
| `dinit/bakawm.target` (dinit) | `/usr/local/lib/dinit.d/user/` |

### Uninstall

```bash
sudo -E make uninstall
```

## Running

### From a display manager (GDM, SDDM, etc.)

After installation, bakawm will appear in your display manager's session list. Select "bakawm" and log in.

### From a TTY

With systemd or dinit:

```bash
bakawm-session
```

Without any service manager:

```bash
bakawm --session
```

### As a nested window (for testing)

```bash
bakawm --winit
```

### Configuration

The configuration file is located at `~/.config/bakawm/config.lua`. If no config file exists, bakawm will create a default one on first launch.

The config file is watched for changes and will be automatically reloaded.

## Acknowledgments

- [Smithay](https://github.com/Smithay/smithay) - The Wayland compositor framework that bakawm is built upon. 
- [niri](https://github.com/YaLTeR/niri) - A scrollable-tiling Wayland compositor. bakawm references portions of niri's source code during development. 

#### Verrrrrrrrrrrrrrrry thanks these project!!!