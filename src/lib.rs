#![warn(rust_2018_idioms)]
#![allow(clippy::collapsible_match)]
// If no backend is enabled, a large portion of the codebase is unused.
// So silence this useless warning for the CI.
#![cfg_attr(
    not(any(feature = "winit", feature = "x11", feature = "udev")),
    allow(dead_code, unused_imports)
)]

pub mod animation;
pub mod config;
#[cfg(any(feature = "udev", feature = "xwayland"))]
pub mod cursor;
#[cfg(feature = "xdp-gnome-screencast")]
pub mod dbus;
pub mod drawing;
pub mod focus;
pub mod input_handler;
pub mod ipc;
pub mod layout;
#[cfg(feature = "libei")]
pub mod libei;
#[cfg(feature = "xdp-gnome-screencast")]
pub mod protocols;
pub mod render;
pub mod render_helpers;
#[cfg(feature = "xdp-gnome-screencast")]
pub mod screencasting;
pub mod screencopy;
pub mod shell;
pub mod state;
#[cfg(feature = "udev")]
pub mod udev;
#[cfg(feature = "winit")]
pub mod winit;
#[cfg(feature = "x11")]
pub mod x11;

pub use state::{AnvilState, ClientState};
