use std::os::unix::net::UnixStream;

use zbus::{fdo, interface, zvariant};

use super::Start;

pub struct ServiceChannel {
    to_state: calloop::channel::Sender<UnixStream>,
}

#[interface(name = "org.gnome.Mutter.ServiceChannel")]
impl ServiceChannel {
    async fn open_wayland_service_connection(
        &mut self,
        service_client_type: u32,
    ) -> fdo::Result<zvariant::OwnedFd> {
        if service_client_type != 1 {
            return Err(fdo::Error::InvalidArgs(
                "Invalid service client type".to_owned(),
            ));
        }

        let (sock1, sock2) = UnixStream::pair().map_err(|e| {
            fdo::Error::Failed(format!("failed to create socket pair: {e}"))
        })?;

        if let Err(err) = self.to_state.send(sock2) {
            tracing::warn!("error sending service channel client to state: {err:?}");
            return Err(fdo::Error::Failed("internal error".to_owned()));
        }

        Ok(zvariant::OwnedFd::from(std::os::fd::OwnedFd::from(sock1)))
    }
}

impl ServiceChannel {
    pub fn new(to_state: calloop::channel::Sender<UnixStream>) -> Self {
        Self { to_state }
    }
}

impl Start for ServiceChannel {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection> {
        let conn = zbus::blocking::connection::Builder::session()?
            .name("org.gnome.Mutter.ServiceChannel")?
            .serve_at("/org/gnome/Mutter/ServiceChannel", self)?
            .build()?;
        Ok(conn)
    }
}