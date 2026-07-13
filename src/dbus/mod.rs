use std::sync::Arc;

use tracing::warn;
use zbus::blocking::Connection;
use zbus::object_server::Interface;

use crate::state::AnvilState;
use crate::udev::UdevData;

pub mod gnome_shell_introspect;
use gnome_shell_introspect::Introspect;

pub mod mutter_display_config;
use mutter_display_config::DisplayConfig;

pub mod mutter_screen_cast;
use mutter_screen_cast::ScreenCast;

pub mod mutter_service_channel;
use mutter_service_channel::ServiceChannel;

trait Start: Interface {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection>;
}

pub struct DBusServers {
    pub conn_service_channel: Option<Connection>,
    pub conn_display_config: Option<Connection>,
    pub conn_screen_cast: Option<Connection>,
    pub conn_introspect: Option<Connection>,
}

impl Default for DBusServers {
    fn default() -> Self {
        Self {
            conn_service_channel: None,
            conn_display_config: None,
            conn_screen_cast: None,
            conn_introspect: None,
        }
    }
}

impl std::fmt::Debug for DBusServers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DBusServers")
            .field("conn_service_channel", &self.conn_service_channel.is_some())
            .field("conn_display_config", &self.conn_display_config.is_some())
            .field("conn_screen_cast", &self.conn_screen_cast.is_some())
            .field("conn_introspect", &self.conn_introspect.is_some())
            .finish()
    }
}

impl DBusServers {
    pub fn start(state: &mut AnvilState<UdevData>) {
        let backend = &state.backend_data;

        let mut dbus = Self::default();

        let (to_state, from_service_channel) = calloop::channel::channel();
        state.handle.insert_source(from_service_channel, {
            move |event, _, state| match event {
                calloop::channel::Event::Msg(client_stream) => {
                    let client_state = crate::state::ClientState::default();
                    if let Err(err) = state
                        .display_handle
                        .insert_client(client_stream, Arc::new(client_state))
                    {
                        warn!("Error adding service channel client: {}", err);
                    }
                }
                calloop::channel::Event::Closed => (),
            }
        }).unwrap();
        let service_channel = ServiceChannel::new(to_state);
        dbus.conn_service_channel = try_start(service_channel);

        let display_config = DisplayConfig::new(backend.ipc_outputs());
        dbus.conn_display_config = try_start(display_config);

        let (to_state_sc, from_screen_cast) = calloop::channel::channel();
        state.handle.insert_source(from_screen_cast, {
            move |event, _, state| match event {
                calloop::channel::Event::Msg(msg) => state.on_screen_cast_msg(msg),
                calloop::channel::Event::Closed => (),
            }
        }).unwrap();
        let screen_cast = ScreenCast::new(backend.ipc_outputs(), to_state_sc);
        dbus.conn_screen_cast = try_start(screen_cast);

        let (to_state_introspect, from_introspect) = calloop::channel::channel();
        let (to_introspect, from_state_introspect) = async_channel::unbounded();
        state.handle.insert_source(from_introspect, {
            move |event, _, state| match event {
                calloop::channel::Event::Msg(msg) => {
                    state.on_introspect_msg(&to_introspect, msg)
                }
                calloop::channel::Event::Closed => (),
            }
        }).unwrap();
        let introspect = Introspect::new(to_state_introspect, from_state_introspect);
        dbus.conn_introspect = try_start(introspect);

        state.dbus = Some(dbus);
    }
}

fn try_start<I: Start>(iface: I) -> Option<Connection> {
    match iface.start() {
        Ok(conn) => Some(conn),
        Err(err) => {
            warn!("error starting {}: {err:?}", I::name());
            None
        }
    }
}