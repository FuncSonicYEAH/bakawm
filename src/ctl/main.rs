// Explicit returns are mandated project-wide; needless_return contradicts it.
#![warn(clippy::implicit_return)]
#![allow(clippy::needless_return)]

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::os::unix::io::{AsFd, BorrowedFd};
use std::path::PathBuf;

use smithay::backend::udev::{all_gpus, primary_gpu};
use smithay::reexports::drm;
use smithay::reexports::drm::control::{
    Device as ControlDevice, ModeTypeFlags, ResourceHandles, connector,
};
use smithay::reexports::rustix::fs::OFlags;
use smithay_drm_extras::display_info;

use bakawm::ipc::{IpcRequest, IpcResponse, send_request};

// ---------------------------------------------------------------------------
// DRM card wrapper (kept from the original output-detection tool)
// ---------------------------------------------------------------------------

struct Card(fs::File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        return self.0.as_fd()
    }
}

impl drm::Device for Card {}

impl ControlDevice for Card {}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        print_usage();
        std::process::exit(0);
    }

    let result = match args[0].as_str() {
        "output" | "detect" => cmd_output(&args[1..]),
        "outputs" => cmd_outputs(&args[1..]),
        "window" | "windows" => cmd_window(&args[1..]),
        "screenshot" => cmd_screenshot(&args[1..]),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        // Backward compatibility: bare `--lua` / `-l` triggers output detection.
        "--lua" | "-l" => cmd_output(&args),
        _ => {
            eprintln!("Unknown command: {}\n", args[0]);
            print_usage();
            std::process::exit(1);
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn print_usage() {
    eprintln!("bakawm-ctl — control utility for the bakawm compositor");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    bakawm-ctl <COMMAND> [OPTIONS]");
    eprintln!();
    eprintln!("COMMANDS:");
    eprintln!("    output [--lua|-l]            Detect connected DRM outputs (hardware probe)");
    eprintln!("    outputs                      List runtime outputs via IPC");
    eprintln!("    window <SUBCOMMAND>          Manage windows");
    eprintln!("    screenshot [OPTIONS]         Capture a screenshot");
    eprintln!("    help                         Show this help message");
    eprintln!();
    eprintln!("WINDOW SUBCOMMANDS:");
    eprintln!("    list                         List all windows");
    eprintln!("    focus <id>                   Focus a window");
    eprintln!("    close <id>                   Close a window");
    eprintln!("    move <id> <x> <y>            Move a window");
    eprintln!("    resize <id> <w> <h>          Resize a window");
    eprintln!();
    eprintln!("SCREENSHOT OPTIONS:");
    eprintln!("    --output <name>              Capture a specific output (default: first)");
    eprintln!("    --file <path>                Save PNG to file (default: stdout)");
    eprintln!();
    eprintln!("EXAMPLES:");
    eprintln!("    bakawm-ctl window list");
    eprintln!("    bakawm-ctl window close 2");
    eprintln!("    bakawm-ctl screenshot | swappy -");
    eprintln!("    bakawm-ctl screenshot --output HDMI-A-1 > shot.png");
}

// ---------------------------------------------------------------------------
// `output` command — DRM connector detection (preserved from original)
// ---------------------------------------------------------------------------

fn cmd_output(args: &[String]) -> Result<(), String> {
    let lua_output = args.iter().any(|a| return a == "--lua" || a == "-l");

    let connectors = enumerate_connectors().map_err(|e| {
        return format!("{e}\nHint: This tool needs access to DRM devices. Try running with appropriate permissions.")
    })?;

    if connectors.is_empty() {
        eprintln!("No connected displays found.");
        std::process::exit(1);
    }

    if lua_output {
        print_lua_output(&connectors);
    } else {
        print_human_output(&connectors);
    }
    return Ok(())
}

// ---------------------------------------------------------------------------
// `outputs` command — list runtime outputs via IPC
// ---------------------------------------------------------------------------

fn cmd_outputs(_args: &[String]) -> Result<(), String> {
    let (response, _) = send_request(&IpcRequest::ListOutputs)
        .map_err(|e| format!("Failed to query outputs: {e}"))?;

    match response {
        IpcResponse::Outputs { outputs } => {
            if outputs.is_empty() {
                println!("No outputs available.");
                return Ok(());
            }
            for (i, out) in outputs.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                println!("Output: {}", out.name);
                println!("  Resolution: {}x{}", out.width, out.height);
                println!("  Scale: {}", out.scale);
            }
            return Ok(())
        }
        IpcResponse::Error { message } => return Err(format!("compositor error: {message}")),
        _ => return Err("unexpected response from compositor".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// `window` command — window management via IPC
// ---------------------------------------------------------------------------

fn cmd_window(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        eprintln!("Usage: bakawm-ctl window <list|focus|close|move|resize> ...");
        std::process::exit(1);
    }

    match args[0].as_str() {
        "list" => return window_list(),
        "focus" => {
            let id = parse_id(args.get(1))?;
            return window_dispatch(IpcRequest::FocusWindow { id })
        }
        "close" => {
            let id = parse_id(args.get(1))?;
            return window_dispatch(IpcRequest::CloseWindow { id })
        }
        "move" => {
            let id = parse_id(args.get(1))?;
            let x = parse_i32(args.get(2), "x")?;
            let y = parse_i32(args.get(3), "y")?;
            return window_dispatch(IpcRequest::MoveWindow { id, x, y })
        }
        "resize" => {
            let id = parse_id(args.get(1))?;
            let w = parse_i32(args.get(2), "width")?;
            let h = parse_i32(args.get(3), "height")?;
            return window_dispatch(IpcRequest::ResizeWindow { id, w, h })
        }
        _ => {
            eprintln!("Unknown window subcommand: {}", args[0]);
            eprintln!("Usage: bakawm-ctl window <list|focus|close|move|resize> ...");
            std::process::exit(1);
        }
    }
}

fn window_list() -> Result<(), String> {
    let (response, _) = send_request(&IpcRequest::ListWindows)
        .map_err(|e| format!("Failed to query windows: {e}"))?;

    match response {
        IpcResponse::Windows { windows } => {
            if windows.is_empty() {
                println!("No windows.");
                return Ok(());
            }
            // Compute column widths for nice alignment.
            println!(
                "{:<6} {:<24} {:<24} Geometry",
                "ID", "Title", "App ID"
            );
            for w in &windows {
                let geo = w
                    .geometry
                    .map(|(x, y, w, h)| format!("{x},{y} {w}x{h}"))
                    .unwrap_or_else(|| return "—".to_owned());
                let title = truncate(&w.title, 24);
                let app_id = truncate(&w.app_id, 24);
                println!("{:<6} {:<24} {:<24} {}", w.id, title, app_id, geo);
            }
            return Ok(())
        }
        IpcResponse::Error { message } => return Err(format!("compositor error: {message}")),
        _ => return Err("unexpected response from compositor".to_owned()),
    }
}

fn window_dispatch(request: IpcRequest) -> Result<(), String> {
    let (response, _) =
        send_request(&request).map_err(|e| format!("Failed to send request: {e}"))?;

    match response {
        IpcResponse::Ok => return Ok(()),
        IpcResponse::Error { message } => return Err(format!("compositor error: {message}")),
        _ => return Err("unexpected response from compositor".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// `screenshot` command — capture via IPC, write PNG to stdout or file
// ---------------------------------------------------------------------------

fn cmd_screenshot(args: &[String]) -> Result<(), String> {
    let mut output_name: Option<String> = None;
    let mut file_path: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--output" => {
                i += 1;
                output_name = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| return "--output requires a name".to_owned())?,
                );
            }
            "--file" => {
                i += 1;
                file_path = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| return "--file requires a path".to_owned())?,
                );
            }
            "--help" | "-h" => {
                eprintln!("Usage: bakawm-ctl screenshot [--output <name>] [--file <path>]");
                eprintln!("  By default the PNG is written to stdout for piping, e.g.:");
                eprintln!("    bakawm-ctl screenshot | swappy -");
                return Ok(());
            }
            other => return Err(format!("unknown option: {other}")),
        }
        i += 1;
    }

    let request = IpcRequest::Screenshot {
        output: output_name,
    };
    let (response, binary) =
        send_request(&request).map_err(|e| format!("Failed to request screenshot: {e}"))?;

    match (response, binary) {
        (IpcResponse::Screenshot { width, height, .. }, Some(png_bytes)) => {
            if let Some(path) = file_path {
                fs::write(&path, &png_bytes)
                    .map_err(|e| format!("failed to write '{path}': {e}"))?;
                eprintln!("Screenshot ({}x{}) saved to {}", width, height, path);
            } else {
                // Refuse to dump binary to an interactive terminal.
                if io::stdout().is_terminal() {
                    return Err("refusing to write binary PNG to a terminal. \
                         Pipe into another program (e.g. `bakawm-ctl screenshot | swappy -`) \
                         or use --file <path>."
                        .to_owned());
                }
                let stdout = io::stdout();
                let mut lock = stdout.lock();
                lock.write_all(&png_bytes)
                    .map_err(|e| format!("failed to write to stdout: {e}"))?;
                let _ = lock.flush();
            }
            return Ok(())
        }
        (IpcResponse::Error { message }, _) => return Err(format!("compositor error: {message}")),
        _ => return Err("unexpected response from compositor".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// Small parsing helpers
// ---------------------------------------------------------------------------

fn parse_id(arg: Option<&String>) -> Result<u64, String> {
    return arg.ok_or_else(|| return "missing window id".to_owned())?
        .parse::<u64>()
        .map_err(|e| format!("invalid window id: {e}"))
}

fn parse_i32(arg: Option<&String>, name: &str) -> Result<i32, String> {
    return arg.ok_or_else(|| format!("missing {name}"))?
        .parse::<i32>()
        .map_err(|e| format!("invalid {name}: {e}"))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        return out
    }
}

// ---------------------------------------------------------------------------
// DRM connector enumeration (preserved from the original tool)
// ---------------------------------------------------------------------------

struct ConnectorInfo {
    name: String,
    make: String,
    model: String,
    modes: Vec<ModeInfo>,
    physical_size: Option<(u32, u32)>,
    connector_type: String,
}

struct ModeInfo {
    width: u32,
    height: u32,
    refresh_mhz: u32,
    preferred: bool,
}

impl ModeInfo {
    fn refresh_hz(&self) -> f64 {
        return self.refresh_mhz as f64 / 1000.0
    }
}

fn enumerate_connectors() -> Result<Vec<ConnectorInfo>, Box<dyn std::error::Error>> {
    let seat_name = get_seat_name()?;

    let gpu_paths = get_gpu_paths(&seat_name)?;

    let mut result = Vec::new();

    for path in &gpu_paths {
        if let Ok(connectors) = scan_drm_device(path) {
            result.extend(connectors);
        }
    }

    if result.is_empty() {
        let card_paths = find_card_devices()?;
        for path in &card_paths {
            if let Ok(connectors) = scan_drm_device(path) {
                result.extend(connectors);
            }
        }
    }

    return Ok(result)
}

fn get_seat_name() -> Result<String, Box<dyn std::error::Error>> {
    if let Ok(seat) = std::env::var("XDG_SESSION_SEAT") {
        return Ok(seat);
    }
    return Ok("seat0".into())
}

fn get_gpu_paths(seat: &str) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut paths = Vec::new();

    if let Ok(var) = std::env::var("ANVIL_DRM_DEVICE") {
        paths.push(PathBuf::from(var));
        return Ok(paths);
    }

    if let Ok(Some(primary)) = primary_gpu(seat) {
        paths.push(primary);
    }

    if let Ok(all) = all_gpus(seat) {
        for p in all {
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
    }

    return Ok(paths)
}

fn find_card_devices() -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir("/dev/dri")? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("card") {
            paths.push(entry.path());
        }
    }
    paths.sort();
    return Ok(paths)
}

fn scan_drm_device(path: &PathBuf) -> Result<Vec<ConnectorInfo>, Box<dyn std::error::Error>> {
    let card = open_drm_device(path)?;

    let res_handles: ResourceHandles = card.resource_handles()?;

    let mut connectors = Vec::new();

    for &conn_handle in res_handles.connectors() {
        let conn_info = match card.get_connector(conn_handle, false) {
            Ok(info) => info,
            Err(_) => continue,
        };

        if conn_info.state() != connector::State::Connected {
            continue;
        }

        let name = format!(
            "{}-{}",
            conn_info.interface().as_str(),
            conn_info.interface_id()
        );

        let info = display_info::for_connector(&card, conn_handle);

        let make = info
            .as_ref()
            .and_then(|i| return i.make())
            .unwrap_or_else(|| return "Unknown".into())
            .to_string();

        let model = info
            .as_ref()
            .and_then(|i| return i.model())
            .unwrap_or_else(|| return "Unknown".into())
            .to_string();

        let modes: Vec<ModeInfo> = conn_info
            .modes()
            .iter()
            .map(|mode| {
                let mode_type = mode.mode_type();
                return ModeInfo {
                    width: mode.size().0 as u32,
                    height: mode.size().1 as u32,
                    refresh_mhz: mode.vrefresh(),
                    preferred: mode_type.contains(ModeTypeFlags::PREFERRED),
                }
            })
            .collect();

        let physical_size = conn_info.size();

        let connector_type = format!("{:?}", conn_info.interface());

        connectors.push(ConnectorInfo {
            name,
            make,
            model,
            modes,
            physical_size,
            connector_type,
        });
    }

    return Ok(connectors)
}

fn open_drm_device(path: &PathBuf) -> Result<Card, Box<dyn std::error::Error>> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(OFlags::CLOEXEC.bits() as i32)
        .open(path)?;

    return Ok(Card(file))
}

fn print_human_output(connectors: &[ConnectorInfo]) {
    for (i, conn) in connectors.iter().enumerate() {
        if i > 0 {
            println!();
        }

        println!("Output: {}", conn.name);
        println!("  Type: {}", conn.connector_type);
        println!("  Make:  {}", conn.make);
        println!("  Model: {}", conn.model);

        if let Some((w, h)) = conn.physical_size
            && w > 0 && h > 0 {
                println!("  Physical size: {}x{} mm", w, h);
            }

        println!("  Modes:");
        for mode in &conn.modes {
            let pref = if mode.preferred { " (preferred)" } else { "" };
            println!(
                "    {}x{} @ {:.0} Hz{}",
                mode.width,
                mode.height,
                mode.refresh_hz(),
                pref
            );
        }
    }

    println!();
    println!("Tip: Use --lua or -l flag to output in Lua config format.");
}

fn print_lua_output(connectors: &[ConnectorInfo]) {
    println!("-- Auto-detected output configuration for bakawm");
    println!("-- Copy the relevant entries into your config.lua");
    println!();
    println!("outputs = {{");

    for (i, conn) in connectors.iter().enumerate() {
        let preferred = conn.modes.iter().find(|m| return m.preferred);
        let mode = preferred.or_else(|| return conn.modes.first());

        if i > 0 {
            println!(",");
        }

        println!("    {{");
        println!("        name = \"{}\",", conn.name);

        if let Some(m) = mode {
            let refresh = if m.refresh_hz() == m.refresh_hz().round() {
                format!("{}", m.refresh_hz() as i32)
            } else {
                format!("{:.1}", m.refresh_hz())
            };
            println!(
                "        mode = {{ width = {}, height = {}, refresh = {} }},",
                m.width, m.height, refresh
            );
        }

        if let Some((w, h)) = conn.physical_size
            && w > 0 && h > 0 {
                println!("        -- Physical size: {}x{} mm", w, h);
            }

        println!("        -- Make: {}, Model: {}", conn.make, conn.model);

        if conn.modes.len() > 1 {
            println!("        -- Available modes:");
            for m in &conn.modes {
                let pref = if m.preferred { " (preferred)" } else { "" };
                println!(
                    "        --   {}x{} @ {:.0} Hz{}",
                    m.width,
                    m.height,
                    m.refresh_hz(),
                    pref
                );
            }
        }

        print!("    }}");
    }

    println!();
    println!("}},");
}
