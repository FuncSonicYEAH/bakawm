// Explicit returns are mandated project-wide; needless_return contradicts it.
#![warn(clippy::implicit_return)]
#![allow(clippy::needless_return)]

use std::env;
use std::sync::atomic::{AtomicBool, Ordering};

static IS_SESSION: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "systemd")]
static IS_SYSTEMD_SERVICE: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
static POSSIBLE_BACKENDS: &[&str] = &[
    #[cfg(feature = "winit")]
    "--winit : Run bakawm as a X11 or Wayland client using winit.",
    #[cfg(feature = "udev")]
    "--tty-udev : Run bakawm as a tty udev client (requires root if without logind).",
    #[cfg(feature = "x11")]
    "--x11 : Run bakawm as an X11 client.",
];

const USAGE: &str = "\
USAGE: bakawm [OPTIONS]

OPTIONS:
    --session      Import environment globally to service manager and D-Bus, run D-Bus services.
                   Set this flag when running as your main compositor instance on a TTY.
                   Do not set when running as a nested window.
    --version      Print version information.

BACKENDS:
    --winit        Run as a nested X11 or Wayland client using winit.
    --tty-udev     Run as a tty udev client (requires root if without logind).
    --x11          Run as an X11 client.

If no backend is specified and --session is set, bakawm will auto-detect the
appropriate backend (udev on a TTY, winit in a graphical session).
";

#[cfg(feature = "profile-with-tracy-mem")]
#[global_allocator]
static GLOBAL: profiling::tracy_client::ProfiledAllocator<std::alloc::System> =
    profiling::tracy_client::ProfiledAllocator::new(std::alloc::System, 10);

fn setup_session() {
    if env::var_os("WSL_DISTRO_NAME").is_none() {
        if env::var_os("DISPLAY").is_some() {
            tracing::warn!("running as a session but DISPLAY is set, removing it");
            unsafe { env::remove_var("DISPLAY") };
        }
        if env::var_os("WAYLAND_DISPLAY").is_some() {
            tracing::warn!("running as a session but WAYLAND_DISPLAY is set, removing it");
            unsafe { env::remove_var("WAYLAND_DISPLAY") };
        }
        if env::var_os("WAYLAND_SOCKET").is_some() {
            tracing::warn!("running as a session but WAYLAND_SOCKET is set, removing it");
            unsafe { env::remove_var("WAYLAND_SOCKET") };
        }
    }

    unsafe {
        env::set_var("XDG_CURRENT_DESKTOP", "bakawm:gnome");
        env::set_var("XDG_SESSION_TYPE", "wayland");
    }

    if env::var_os("DISPLAY").is_none() && env::var_os("WAYLAND_DISPLAY").is_none()
        && let Ok(output) = std::process::Command::new("dbus-update-activation-environment")
            .arg("--all")
            .output()
            && !output.status.success() {
                tracing::warn!("failed to update D-Bus activation environment");
            }

    #[cfg(feature = "systemd")]
    {
        if IS_SYSTEMD_SERVICE.load(Ordering::Relaxed)
            && let Ok(output) = std::process::Command::new("systemctl")
                .args(["--user", "import-environment"])
                .args([
                    "WAYLAND_DISPLAY",
                    "DISPLAY",
                    "XDG_SESSION_TYPE",
                    "XDG_CURRENT_DESKTOP",
                ])
                .output()
                && !output.status.success() {
                    tracing::warn!("failed to import environment into systemd user manager");
                }
    }
}

#[allow(clippy::uninlined_format_args)]
fn main() {
    if env::var_os("RUST_BACKTRACE").is_none() {
        unsafe { env::set_var("RUST_BACKTRACE", "1") };
    }

    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt()
            .compact()
            .with_env_filter(env_filter)
            .init();
    } else {
        tracing_subscriber::fmt().compact().init();
    }

    #[cfg(feature = "systemd")]
    {
        if env::var_os("NOTIFY_SOCKET").is_some() {
            IS_SYSTEMD_SERVICE.store(true, Ordering::Relaxed);
        }
    }

    #[cfg(feature = "profile-with-tracy")]
    profiling::tracy_client::Client::start();

    profiling::register_thread!("Main Thread");

    #[cfg(feature = "profile-with-puffin")]
    let _server =
        puffin_http::Server::new(&format!("0.0.0.0:{}", puffin_http::DEFAULT_PORT)).unwrap();
    #[cfg(feature = "profile-with-puffin")]
    profiling::puffin::set_scopes_on(true);

    let args: Vec<String> = env::args().collect();
    let mut is_session = false;

    for arg in &args[1..] {
        match arg.as_str() {
            "--session" => {
                is_session = true;
                IS_SESSION.store(true, Ordering::Relaxed);
            }
            "--version" => {
                println!("bakawm {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--help" | "-h" => {
                print!("{USAGE}");
                return;
            }
            _ => {}
        }
    }

    if is_session {
        setup_session();
    }

    let backend_arg = args.iter().position(|a| return a.starts_with("--")).and_then(|i| {
        let arg = &args[i];
        match arg.as_str() {
            "--session" | "--version" | "--help" | "-h" => return None,
            _ => return Some(arg.clone()),
        }
    });

    match backend_arg.as_deref() {
        #[cfg(feature = "winit")]
        Some("--winit") => {
            tracing::info!("Starting winit backend");
            bakawm::winit::run_winit();
        }
        #[cfg(feature = "udev")]
        Some("--tty-udev") => {
            tracing::info!("Starting on a tty using udev");
            bakawm::udev::run_udev();
        }
        #[cfg(feature = "x11")]
        Some("--x11") => {
            tracing::info!("Starting with x11 backend");
            bakawm::x11::run_x11();
        }
        Some(other) => {
            tracing::error!("Unknown option: {}", other);
            eprintln!("Unknown option: {}", other);
            eprintln!();
            print!("{USAGE}");
            std::process::exit(1);
        }
        None => {
            if is_session {
                #[cfg(feature = "udev")]
                {
                    if env::var_os("DISPLAY").is_none()
                        && env::var_os("WAYLAND_DISPLAY").is_none()
                        && env::var_os("WAYLAND_SOCKET").is_none()
                    {
                        tracing::info!("Auto-detecting: starting on a tty using udev");
                        bakawm::udev::run_udev();
                        return;
                    }
                }
                #[cfg(feature = "winit")]
                {
                    tracing::info!("Auto-detecting: starting as nested window using winit");
                    bakawm::winit::run_winit();
                }
            } else {
                #[allow(clippy::disallowed_macros)]
                {
                    print!("{USAGE}");
                }
            }
        }
    }
}
