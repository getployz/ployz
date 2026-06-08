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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsClientUrl(String);

impl NatsClientUrl {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NatsClientUrlError> {
        let value = value.into();
        if value.is_empty() {
            return Err(NatsClientUrlError::Empty);
        }
        if value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        {
            return Err(NatsClientUrlError::UnsupportedEnvironmentValue { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn from_endpoint(endpoint: &NatsClientEndpoint) -> Self {
        Self::try_new(endpoint.url()).expect("endpoint-rendered NATS URL is valid")
    }

    #[must_use]
    pub fn loopback(port: u16) -> Self {
        Self::from_endpoint(&NatsClientEndpoint::loopback(port))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for NatsClientUrl {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsClientUrlError {
    Empty,
    UnsupportedEnvironmentValue { value: String },
}

pub async fn connect_with_timeout(
    nats_url: &NatsClientUrl,
    timeout: Duration,
) -> Result<async_nats::Client, NatsConnectError> {
    match tokio::time::timeout(timeout, async_nats::connect(nats_url.as_str())).await {
        Ok(Ok(client)) => Ok(client),
        Ok(Err(error)) => Err(NatsConnectError::Connect {
            url: nats_url.as_str().to_owned(),
            message: error.to_string(),
        }),
        Err(_) => Err(NatsConnectError::Timeout {
            url: nats_url.as_str().to_owned(),
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
