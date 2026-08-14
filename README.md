# bakawm

A Wayland compositor/window manager built with [Rust](https://www.rust-lang.org/) and [Smithay](https://github.com/Smithay/smithay).

bakawm is an early-stage Wayland WM that uses Smithay as its compositor framework. The project is currently in active development and not yet ready for daily use.

## Build Dependencies

Before building bakawm, make sure the following system libraries and tools are installed:

- Rust toolchain (1.85+)
- pkg-config
- clang
- libudev
- libdrm
- libgbm
- libinput
- libxkbcommon
- libseat (seatd)
- libdisplay-info
- wayland-server
- libEGL (Mesa / libglvnd)
- libxcb (optional, for the winit backend when running as a nested X11 window)
- libpipewire-0.3 (optional, for screen recording)
- libdbus-1 (optional, for screen recording)

> Note: several features are pure Rust and need no system library: the `libei` input-emulation backend uses the [`reis`](https://crates.io/crates/reis) crate, and the X11 backend and XWayland use [`x11rb`](https://crates.io/crates/x11rb) (no libx11/libxcb needed to build). The only X11-related system library is `libxcb`, which the `winit` backend dlopens at runtime when running as a nested X11 window.

On Debian/Ubuntu:

```bash
sudo apt install build-essential pkg-config clang libudev-dev libdrm-dev libgbm-dev \
    libinput-dev libxkbcommon-dev libseat-dev libdisplay-info-dev \
    libwayland-dev libegl-dev libpixman-1-dev \
    libpipewire-0.3-dev libdbus-1-dev
```

> `libdisplay-info-dev` needs libdisplay-info >= 0.1: Debian 13+ and Ubuntu 24.04+ work out of the box. On Debian 12 (bookworm) the packaged version is too old (0.0.x) — enable bookworm-backports; on Ubuntu 22.04 (jammy) it is not available in the official repositories.

On Fedora:

```bash
sudo dnf install gcc pkg-config clang systemd-devel libdrm-devel mesa-libgbm-devel \
    libinput-devel libxkbcommon-devel libseat-devel libdisplay-info-devel \
    wayland-devel libglvnd-devel pixman-devel \
    pipewire-devel dbus-devel
```

On Arch Linux:

```bash
sudo pacman -S base-devel clang systemd-libs libdrm libinput \
    libxkbcommon seatd libdisplay-info mesa pixman wayland \
    pipewire dbus
```

On Void Linux:

```bash
sudo xbps-install base-devel clang pkg-config eudev-libudev-devel libdrm-devel \
    libgbm-devel libinput-devel libxkbcommon-devel libseat-devel \
    libdisplay-info-devel wayland-devel MesaLib-devel pixman-devel \
    pipewire-devel dbus-devel
```

On Alpine Linux:

```bash
sudo apk add build-base pkgconf clang eudev-dev libdrm-dev mesa-gbm \
    libinput-dev libxkbcommon-dev libseat-dev libdisplay-info-dev \
    wayland-dev mesa-dev pixman-dev \
    pipewire-dev dbus-dev
```

## Distribution Packages

### Arch Linux (AUR)
- Thanks ShinKouyo

```bash
yay -S bakawm-git
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

The `systemd` feature only adds sd-notify integration; it can be swapped for `dinit` or dropped entirely without affecting any other feature. Screen recording (`xdp-gnome-screencast`) does not depend on systemd, so it stays enabled here — remove it separately if you don't need it (see below).

For distributions that use dinit (e.g. Artix Linux):

```bash
cargo build --release --no-default-features --features "egl,winit,x11,udev,xwayland,libei,dinit,xdp-gnome-screencast"
```

For distributions without any service manager (e.g. Void Linux, Alpine Linux):

```bash
cargo build --release --no-default-features --features "egl,winit,x11,udev,xwayland,libei,xdp-gnome-screencast"
```

### Building without screen recording

To build without PipeWire/D-Bus screen recording support:

```bash
cargo build --release --no-default-features --features "egl,winit,x11,udev,xwayland,libei,systemd"
```

## Installation

### Using Makefile

The Makefile installs the binary, session script, desktop file, and portal configuration. Service files for systemd and dinit are installed separately.

First build the binaries as a regular user (see [Building](#building)), then install:

```bash
sudo -E make install
```

> The install target only invokes `cargo` when the binaries are missing, so build before installing — this way `sudo make install` does not need `cargo` on `root`'s PATH.

Install systemd service files (systemd-based distributions):

```bash
sudo -E make install-systemd
```

Install dinit service files (dinit-based distributions):

```bash
sudo -E make install-dinit
```

The default installation prefix is `/usr`. You can change it, e.g. for a user-local install:

```bash
make install PREFIX=$HOME/.local
```

### File Locations

With the default prefix (`/usr`), files are installed to:

| File | Destination |
|------|-------------|
| `bakawm` | `/usr/bin/` |
| `bakawm-ctl` | `/usr/bin/` |
| `bakawm-session`¹ | `/usr/bin/` |
| `bakawm.desktop` | `/usr/share/wayland-sessions/` |
| `bakawm-portals.conf` | `/usr/share/xdg-desktop-portal/` |
| `bakawm.portal` | `/usr/share/xdg-desktop-portal/portals/` |
| `bakawm.service` (systemd) | `/usr/lib/systemd/user/` |
| `bakawm-shutdown.target` (systemd) | `/usr/lib/systemd/user/` |
| `dinit/bakawm` (dinit) | `/usr/lib/dinit.d/user/` |
| `dinit/bakawm.target` (dinit) | `/usr/lib/dinit.d/user/` |

¹ Only installed when a service manager (systemd or dinit) is detected; otherwise the desktop file runs `bakawm --tty-udev` directly.

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

- [Smithay](https://github.com/Smithay/smithay) - The Wayland compositor framework that bakawm is built upon. Neat!
- [niri](https://github.com/niri-wm/niri) - A scrollable-tiling Wayland WM. bakawm references portions of niri's source code during development. Very Thanks!
- [Hyprland](https://github.com/hyprwm/Hyprland) - I like this WM's lua config file format, so I use it.
Very Thanks!

## License

This project is licensed under the [GNU General Public License v3.0 or later](https://www.gnu.org/licenses/gpl-3.0.html).
