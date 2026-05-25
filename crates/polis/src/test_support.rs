use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::corrosion_agent::{CorrosionAgentStartMode, CorrosionRootCleanup};
use crate::{CorrosionAgentAddresses, CorrosionAgentConfig, CorrosionAgentError as AgentError};
pub use crate::{CorrosionAgentError, LocalCorrosionAgent};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

pub fn corrosion_agent_addresses() -> Result<CorrosionAgentAddresses, AgentError> {
    free_corrosion_addresses()
}

#[derive(Debug, Clone)]
pub struct CorrosionAgentFixtureConfig {
    config: CorrosionAgentConfig,
    root_cleanup: CorrosionRootCleanup,
    refresh_addresses_on_retry: bool,
}

impl CorrosionAgentFixtureConfig {
    pub fn isolated() -> Result<Self, AgentError> {
        Ok(Self {
            config: CorrosionAgentConfig::for_fixture_root_dir(
                unique_temp_dir()?,
                free_corrosion_addresses()?,
            ),
            root_cleanup: CorrosionRootCleanup::RemoveOnDrop,
            refresh_addresses_on_retry: true,
        })
    }

    #[must_use]
    pub fn persistent_from_config(config: CorrosionAgentConfig) -> Self {
        Self {
            config,
            root_cleanup: CorrosionRootCleanup::Keep,
            refresh_addresses_on_retry: false,
        }
    }

    #[must_use]
    pub fn with_binary_path(mut self, binary_path: impl Into<PathBuf>) -> Self {
        self.config = self.config.with_binary_path(binary_path);
        self
    }

    #[must_use]
    pub fn with_bootstrap(mut self, bootstrap: Vec<SocketAddr>) -> Self {
        self.config = self.config.with_bootstrap(bootstrap);
        self
    }

    #[must_use]
    pub fn with_schema_file(
        mut self,
        name: impl Into<String>,
        contents: impl Into<String>,
    ) -> Self {
        self.config = self.config.with_schema_file(name, contents);
        self
    }

    #[must_use]
    pub fn with_readiness_timeout(mut self, readiness_timeout: Duration) -> Self {
        self.config = self.config.with_readiness_timeout(readiness_timeout);
        self
    }

    #[must_use]
    pub fn api_addr(&self) -> SocketAddr {
        self.config.api_addr()
    }

    #[must_use]
    pub fn gossip_addr(&self) -> SocketAddr {
        self.config.gossip_addr()
    }

    pub async fn start(self) -> Result<LocalCorrosionAgent, AgentError> {
        let mut last_error = None;
        let mut config = self.config;
        let mode = CorrosionAgentStartMode::fixture(self.root_cleanup);
        let attempts = if self.refresh_addresses_on_retry {
            3
        } else {
            1
        };

        for attempt in 0..attempts {
            if attempt > 0 {
                config.refresh_addresses(free_corrosion_addresses()?);
            }

            match LocalCorrosionAgent::start_with_mode(config.clone(), mode.clone()).await {
                Ok(agent) => return Ok(agent),
                Err(error) if error.is_retryable_startup_failure() => {
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }

        Err(last_error.unwrap_or_else(|| AgentError::Setup {
            message: "corrosion fixture startup retry failed without an error".to_string(),
        }))
    }
}

fn free_corrosion_addresses() -> Result<CorrosionAgentAddresses, AgentError> {
    Ok(CorrosionAgentAddresses::new(
        free_tcp_addr()?,
        free_udp_addr()?,
        free_tcp_addr()?,
    ))
}

fn free_tcp_addr() -> Result<SocketAddr, AgentError> {
    use std::net::TcpListener;

    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map_err(setup_error)
}

fn free_udp_addr() -> Result<SocketAddr, AgentError> {
    use std::net::UdpSocket;

    UdpSocket::bind("[::1]:0")
        .and_then(|socket| socket.local_addr())
        .map_err(setup_error)
}

fn setup_error(error: std::io::Error) -> AgentError {
    AgentError::Setup {
        message: error.to_string(),
    }
}

fn unique_temp_dir() -> Result<PathBuf, AgentError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| AgentError::Setup {
            message: format!("system clock before unix epoch: {error}"),
        })?;
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);

    Ok(PathBuf::from("/tmp").join(format!(
        "pcorr-{}-{}-{id}",
        std::process::id(),
        elapsed.as_millis()
    )))
}
