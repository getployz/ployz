//! NATS client connection setup.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

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

pub async fn connect_with_timeout(
    nats_url: &str,
    timeout: Duration,
) -> Result<async_nats::Client, NatsConnectError> {
    match tokio::time::timeout(timeout, async_nats::connect(nats_url)).await {
        Ok(Ok(client)) => Ok(client),
        Ok(Err(error)) => Err(NatsConnectError::Connect {
            url: nats_url.to_owned(),
            message: error.to_string(),
        }),
        Err(_) => Err(NatsConnectError::Timeout {
            url: nats_url.to_owned(),
            timeout,
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsConnectError {
    Connect { url: String, message: String },
    Timeout { url: String, timeout: Duration },
}

impl fmt::Display for NatsConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect { url, message } => {
                write!(formatter, "failed to connect to NATS at {url}: {message}")
            }
            Self::Timeout { url, timeout } => write!(
                formatter,
                "failed to connect to NATS at {url} within {}ms",
                timeout.as_millis()
            ),
        }
    }
}

impl std::error::Error for NatsConnectError {}
