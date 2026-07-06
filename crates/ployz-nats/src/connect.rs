//! NATS client connection setup.
//!
//! Product connections go through [`connect_authenticated`]: TLS against the
//! cluster CA plus NKey-seed authentication, with the request-reply inbox
//! prefix derived from the connecting principal. The bare plaintext
//! [`connect_with_timeout`] exists only for test fixtures.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Once;
use std::time::Duration;

use ployz_core::nats_config::NatsUserSeed;
use ployz_core::permissions::inbox_prefix;
use ployz_core::security::NatsPrincipal;

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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NatsClientUrlError {
    #[error("NATS client URL is empty")]
    Empty,
    #[error("unsupported NATS client URL environment value: {value:?}")]
    UnsupportedEnvironmentValue { value: String },
}

/// How a client authenticates to the cluster.
///
/// Single variant today; the next credential form becomes a variant, not an
/// optional field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsClientAuth {
    NkeySeed(NatsUserSeed),
}

/// Which roots the client verifies the server's TLS certificate against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsTlsTrust {
    ClusterCa(PathBuf),
}

/// Everything needed for one authenticated product connection.
///
/// The inbox prefix is not a field: [`connect_authenticated`] derives it from
/// `principal` via [`inbox_prefix`], so a connection with a mismatched prefix
/// is unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsConnectConfig {
    pub url: NatsClientUrl,
    pub auth: NatsClientAuth,
    pub trust: NatsTlsTrust,
    pub principal: NatsPrincipal,
}

/// The one place authenticated connect options are assembled: NKey seed
/// auth, required TLS against the cluster CA, and the principal's derived
/// inbox prefix. Exposed so tests can attach observation callbacks to the
/// exact option set the product connects with.
#[must_use]
pub fn authenticated_connect_options(config: &NatsConnectConfig) -> async_nats::ConnectOptions {
    install_rustls_crypto_provider();
    let NatsConnectConfig {
        url: _,
        auth,
        trust,
        principal,
    } = config;
    let options = match auth {
        NatsClientAuth::NkeySeed(seed) => {
            async_nats::ConnectOptions::with_nkey(seed.secret().to_owned())
        }
    };
    let options = match trust {
        NatsTlsTrust::ClusterCa(ca_path) => options.add_root_certificates(ca_path.clone()),
    };
    options
        .require_tls(true)
        // The failover server pool is operator-controlled (the configured core
        // plus Reachable Machines from mirrored intent); don't merge servers the
        // NATS server advertises, which would pollute the recovery candidate set,
        // and keep the pool's order so the configured core stays tried first.
        .ignore_discovered_servers()
        .retain_servers_order()
        .custom_inbox_prefix(inbox_prefix(principal))
}

pub async fn connect_authenticated(
    config: &NatsConnectConfig,
    timeout: Duration,
) -> Result<async_nats::Client, NatsConnectError> {
    let options = authenticated_connect_options(config);
    match tokio::time::timeout(timeout, options.connect(config.url.as_str())).await {
        Ok(Ok(client)) => Ok(client),
        Ok(Err(error)) => Err(NatsConnectError::Connect {
            url: config.url.as_str().to_owned(),
            message: error.to_string(),
        }),
        Err(_) => Err(NatsConnectError::Timeout {
            url: config.url.as_str().to_owned(),
            timeout,
        }),
    }
}

/// Connect authenticated to a whole failover pool (the configured core plus
/// Reachable Machines from the cached mirror), tried in order. Used at machine
/// startup so a reboot *during* a core outage still reaches a promoted core
/// rather than timing out on a dead seed. `servers` must be non-empty and lead
/// with the configured core.
pub async fn connect_authenticated_pool(
    config: &NatsConnectConfig,
    servers: &[String],
    timeout: Duration,
) -> Result<async_nats::Client, NatsConnectError> {
    let options = authenticated_connect_options(config);
    match tokio::time::timeout(timeout, options.connect(servers.to_vec())).await {
        Ok(Ok(client)) => Ok(client),
        Ok(Err(error)) => Err(NatsConnectError::Connect {
            url: servers.join(","),
            message: error.to_string(),
        }),
        Err(_) => Err(NatsConnectError::Timeout {
            url: servers.join(","),
            timeout,
        }),
    }
}

/// Fixture-only plaintext connect. Product callers use
/// [`connect_authenticated`]; the remaining product call sites flip in the
/// keeper/ployzd auth wiring steps.
#[doc(hidden)]
pub async fn connect_with_timeout(
    nats_url: &NatsClientUrl,
    timeout: Duration,
) -> Result<async_nats::Client, NatsConnectError> {
    install_rustls_crypto_provider();
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

fn install_rustls_crypto_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _already_installed = rustls::crypto::ring::default_provider().install_default();
    });
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NatsConnectError {
    #[error("failed to connect to NATS at {url}: {message}")]
    Connect { url: String, message: String },
    #[error("failed to connect to NATS at {url} within {}ms", timeout.as_millis())]
    Timeout { url: String, timeout: Duration },
}
