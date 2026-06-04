//! NATS client connection setup.

use std::net::{IpAddr, SocketAddr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsClientEndpoint {
    Socket(SocketAddr),
    Host { host: String, port: u16 },
}

impl NatsClientEndpoint {
    #[must_use]
    pub fn loopback(port: u16) -> Self {
        Self::Socket(SocketAddr::new(IpAddr::from([127, 0, 0, 1]), port))
    }

    #[must_use]
    pub fn tcp(host: impl AsRef<str>, port: u16) -> Self {
        match host.as_ref().parse::<IpAddr>() {
            Ok(ip) => Self::Socket(SocketAddr::new(ip, port)),
            Err(_) => Self::Host {
                host: host.as_ref().to_owned(),
                port,
            },
        }
    }

    #[must_use]
    pub fn from_socket(socket: SocketAddr) -> Self {
        Self::Socket(socket)
    }

    #[must_use]
    pub fn url(&self) -> String {
        match self {
            Self::Socket(socket) => match socket.ip() {
                IpAddr::V4(ip) => format!("nats://{}:{}", ip, socket.port()),
                IpAddr::V6(ip) => format!("nats://[{}]:{}", ip, socket.port()),
            },
            Self::Host { host, port } => format!("nats://{host}:{port}"),
        }
    }

    #[must_use]
    pub fn socket_addr(&self) -> Option<SocketAddr> {
        match self {
            Self::Socket(socket) => Some(*socket),
            Self::Host { .. } => None,
        }
    }
}
