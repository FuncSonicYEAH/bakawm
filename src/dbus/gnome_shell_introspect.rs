use std::collections::HashMap;

use tracing::warn;
use zbus::fdo::{self, RequestNameFlags};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{SerializeDict, Type, Value};

use super::Start;

pub struct Introspect {
    to_state: calloop::channel::Sender<IntrospectToState>,
    from_state: async_channel::Receiver<StateToIntrospect>,
}

pub enum IntrospectToState {
    GetWindows,
}

pub enum StateToIntrospect {
    Windows(HashMap<u64, WindowProperties>),
}

#[derive(Debug, SerializeDict, Type, Value)]
#[zvariant(signature = "dict")]
pub struct WindowProperties {
    pub title: String,
    #[zvariant(rename = "app-id")]
    pub app_id: String,
}

#[interface(name = "org.gnome.Shell.Introspect")]
impl Introspect {
    async fn get_windows(&self) -> fdo::Result<HashMap<u64, WindowProperties>> {
        if let Err(err) = self.to_state.send(IntrospectToState::GetWindows) {
            warn!("error sending message to state: {err:?}");
            return Err(fdo::Error::Failed("internal error".to_owned()));
        }

        match self.from_state.recv().await {
            Ok(StateToIntrospect::Windows(windows)) => Ok(windows),
            Err(err) => {
                warn!("error receiving message from state: {err:?}");
                Err(fdo::Error::Failed("internal error".to_owned()))
            }
        }
    }

    #[zbus(signal)]
    pub async fn windows_changed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property)]
    fn animations_enabled(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
}

impl Introspect {
    pub fn new(
        to_state: calloop::channel::Sender<IntrospectToState>,
        from_state: async_channel::Receiver<StateToIntrospect>,
    ) -> Self {
        Self {
            to_state,
            from_state,
        }
    }
}

impl Start for Introspect {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection> {
        let conn = zbus::blocking::Connection::session()?;
        let flags = RequestNameFlags::AllowReplacement
            | RequestNameFlags::ReplaceExisting
            | RequestNameFlags::DoNotQueue;

        conn.object_server()
            .at("/org/gnome/Shell/Introspect", self)?;
        conn.request_name_with_flags("org.gnome.Shell.Introspect", flags)?;

        Ok(conn)
    }
}
