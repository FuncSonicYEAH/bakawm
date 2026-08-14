use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tracing::warn;
use zbus::fdo::RequestNameFlags;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{self, OwnedValue, Type};
use zbus::{fdo, interface};

use super::Start;
use crate::screencasting::{IpcOutput, IpcOutputMap};

pub struct DisplayConfig {
    ipc_outputs: Arc<Mutex<IpcOutputMap>>,
}

#[derive(serde::Serialize, Type)]
pub struct Monitor {
    names: (String, String, String, String),
    modes: Vec<Mode>,
    properties: HashMap<String, OwnedValue>,
}

#[derive(serde::Serialize, Type)]
pub struct Mode {
    id: String,
    width: i32,
    height: i32,
    refresh_rate: f64,
    preferred_scale: f64,
    supported_scales: Vec<f64>,
    properties: HashMap<String, OwnedValue>,
}

#[derive(serde::Serialize, Type)]
pub struct LogicalMonitor {
    x: i32,
    y: i32,
    scale: f64,
    transform: u32,
    is_primary: bool,
    monitors: Vec<(String, String, String, String)>,
    properties: HashMap<String, OwnedValue>,
}

#[derive(Deserialize, Type)]
pub struct LogicalMonitorConfiguration {
    _x: i32,
    _y: i32,
    _scale: f64,
    _transform: u32,
    _is_primary: bool,
    _monitors: Vec<(String, String, HashMap<String, OwnedValue>)>,
}

#[interface(name = "org.gnome.Mutter.DisplayConfig")]
impl DisplayConfig {
    async fn get_current_state(
        &self,
    ) -> fdo::Result<(
        u32,
        Vec<Monitor>,
        Vec<LogicalMonitor>,
        HashMap<String, OwnedValue>,
    )> {
        let mut monitors = Vec::new();
        let mut logical_monitors = Vec::new();
        let mut first_logical = true;

        for output in self.ipc_outputs.lock().unwrap().values() {
            let is_laptop_panel = is_laptop_panel(&output.name);

            let mut properties = HashMap::new();
            properties.insert(
                String::from("display-name"),
                OwnedValue::from(zvariant::Str::from(make_display_name(
                    output,
                    is_laptop_panel,
                ))),
            );
            properties.insert(
                String::from("is-builtin"),
                OwnedValue::from(is_laptop_panel),
            );

            let modes: Vec<Mode> = if let Some(logical) = &output.logical {
                logical
                    .modes
                    .iter()
                    .map(|m| {
                        let width = m.width as i32;
                        let height = m.height as i32;
                        let refresh_rate = m.refresh_rate;
                        let is_current = m.id == logical.current_mode_id;

                        let mut mode_properties = HashMap::new();
                        mode_properties
                            .insert(String::from("is-current"), OwnedValue::from(is_current));

                        Mode {
                            id: format!("{width}x{height}@{refresh_rate:.3}"),
                            width,
                            height,
                            refresh_rate,
                            preferred_scale: 1.,
                            supported_scales: supported_scales(width, height),
                            properties: mode_properties,
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };

            let connector = output.name.clone();
            let model = output.model.clone();
            let make = output.make.clone();
            let serial = if output.serial.is_empty() {
                connector.clone()
            } else {
                output.serial.clone()
            };

            let names = (connector, make, model, serial);

            if let Some(logical) = &output.logical {
                let is_primary = first_logical;
                first_logical = false;
                logical_monitors.push(LogicalMonitor {
                    x: logical.x,
                    y: logical.y,
                    scale: logical.scale,
                    transform: logical.transform,
                    is_primary,
                    monitors: vec![names.clone()],
                    properties: HashMap::new(),
                });
            }

            monitors.push(Monitor {
                names,
                modes,
                properties,
            });
        }

        monitors.sort_unstable_by(|a, b| a.names.0.cmp(&b.names.0));
        logical_monitors.sort_unstable_by(|a, b| a.monitors[0].0.cmp(&b.monitors[0].0));

        let properties = HashMap::from([(String::from("layout-mode"), OwnedValue::from(1u32))]);
        Ok((0, monitors, logical_monitors, properties))
    }

    async fn apply_monitors_config(
        &self,
        _serial: u32,
        method: u32,
        _logical_monitor_configs: Vec<LogicalMonitorConfiguration>,
        _properties: HashMap<String, OwnedValue>,
    ) -> fdo::Result<()> {
        if method == 0 {
            return Ok(());
        }

        Err(fdo::Error::Failed(
            "Applying monitor configuration is not supported".to_owned(),
        ))
    }

    #[zbus(signal)]
    pub async fn monitors_changed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property)]
    fn power_save_mode(&self) -> i32 {
        -1
    }

    #[zbus(property)]
    fn set_power_save_mode(&self, _mode: i32) -> zbus::Result<()> {
        Err(zbus::Error::Unsupported)
    }

    #[zbus(property)]
    fn panel_orientation_managed(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn apply_monitors_config_allowed(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn night_light_supported(&self) -> bool {
        false
    }

    async fn change_backlight(
        &self,
        _serial: u32,
        _connector: &str,
        _value: i32,
    ) -> fdo::Result<i32> {
        Err(fdo::Error::Failed(
            "Changing backlight is not supported".to_owned(),
        ))
    }

    async fn get_crtc_gamma(
        &self,
        _serial: u32,
        _connector: &str,
    ) -> fdo::Result<(i32, Vec<u16>, Vec<u16>, Vec<u16>)> {
        Err(fdo::Error::Failed(
            "Getting CRTC gamma is not supported".to_owned(),
        ))
    }

    async fn set_crtc_gamma(
        &self,
        _serial: u32,
        _connector: &str,
        _red: Vec<u16>,
        _green: Vec<u16>,
        _blue: Vec<u16>,
    ) -> fdo::Result<()> {
        Err(fdo::Error::Failed(
            "Setting CRTC gamma is not supported".to_owned(),
        ))
    }
}

impl DisplayConfig {
    pub fn new(ipc_outputs: Arc<Mutex<IpcOutputMap>>) -> Self {
        Self { ipc_outputs }
    }

    pub fn emit_monitors_changed(conn: &zbus::blocking::Connection) {
        let iface = match conn
            .object_server()
            .interface::<_, Self>("/org/gnome/Mutter/DisplayConfig")
        {
            Ok(iface) => iface,
            Err(err) => {
                warn!("error getting DisplayConfig interface: {err:?}");
                return;
            }
        };

        async_io::block_on(async move {
            if let Err(err) = Self::monitors_changed(iface.signal_emitter()).await {
                warn!("error emitting MonitorsChanged: {err:?}");
            }
        });
    }
}

impl Start for DisplayConfig {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection> {
        let conn = zbus::blocking::Connection::session()?;
        let flags = RequestNameFlags::AllowReplacement
            | RequestNameFlags::ReplaceExisting
            | RequestNameFlags::DoNotQueue;

        conn.object_server()
            .at("/org/gnome/Mutter/DisplayConfig", self)?;
        conn.request_name_with_flags("org.gnome.Mutter.DisplayConfig", flags)?;

        Ok(conn)
    }
}

fn is_laptop_panel(connector: &str) -> bool {
    let prefix = connector.split('-').next().unwrap_or("");
    matches!(prefix, "eDP" | "LVDS")
}

fn make_display_name(output: &IpcOutput, is_laptop_panel: bool) -> String {
    if is_laptop_panel {
        return String::from("Built-in display");
    }

    let make = &output.make;
    let model = &output.model;
    if model != "Unknown" {
        format!("{make} {model}")
    } else {
        make.clone()
    }
}

fn supported_scales(width: i32, height: i32) -> Vec<f64> {
    let mut scales = vec![1.0_f64];

    if width >= 2560 && height >= 1440 {
        scales.push(2.0);
    }
    if width >= 3840 && height >= 2160 {
        scales.push(3.0);
    }

    scales
}
