use std::fs;
use std::os::unix::io::{AsFd, BorrowedFd};
use std::path::PathBuf;

use smithay::backend::udev::{all_gpus, primary_gpu};
use smithay::reexports::drm;
use smithay::reexports::drm::control::{
    Device as ControlDevice, ModeTypeFlags, ResourceHandles, connector,
};
use smithay::reexports::rustix::fs::OFlags;
use smithay_drm_extras::display_info;

struct Card(fs::File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}

impl ControlDevice for Card {}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let lua_output = args.iter().any(|a| a == "--lua" || a == "-l");

    let connectors = match enumerate_connectors() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            eprintln!(
                "Hint: This tool needs access to DRM devices. Try running with appropriate permissions."
            );
            std::process::exit(1);
        }
    };

    if connectors.is_empty() {
        eprintln!("No connected displays found.");
        std::process::exit(1);
    }

    if lua_output {
        print_lua_output(&connectors);
    } else {
        print_human_output(&connectors);
    }
}

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
        self.refresh_mhz as f64 / 1000.0
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

    Ok(result)
}

fn get_seat_name() -> Result<String, Box<dyn std::error::Error>> {
    if let Ok(seat) = std::env::var("XDG_SESSION_SEAT") {
        return Ok(seat);
    }
    Ok("seat0".into())
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

    Ok(paths)
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
    Ok(paths)
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
            .and_then(|i| i.make())
            .unwrap_or_else(|| "Unknown".into())
            .to_string();

        let model = info
            .as_ref()
            .and_then(|i| i.model())
            .unwrap_or_else(|| "Unknown".into())
            .to_string();

        let modes: Vec<ModeInfo> = conn_info
            .modes()
            .iter()
            .map(|mode| {
                let mode_type = mode.mode_type();
                ModeInfo {
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

    Ok(connectors)
}

fn open_drm_device(path: &PathBuf) -> Result<Card, Box<dyn std::error::Error>> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(OFlags::CLOEXEC.bits() as i32)
        .open(path)?;

    Ok(Card(file))
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

        if let Some((w, h)) = conn.physical_size {
            if w > 0 && h > 0 {
                println!("  Physical size: {}x{} mm", w, h);
            }
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
        let preferred = conn.modes.iter().find(|m| m.preferred);
        let mode = preferred.or_else(|| conn.modes.first());

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

        if let Some((w, h)) = conn.physical_size {
            if w > 0 && h > 0 {
                println!("        -- Physical size: {}x{} mm", w, h);
            }
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
