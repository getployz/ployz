//! Runtime execution for parsed CLI commands.

use std::fmt;
use std::time::Duration;

use crate::api_client::{OperationApiClient, OperationApiClientError};
use crate::commands::{PloyzctlCommand, USAGE};
use ployz_sdk_types::MachineAddError;

pub const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
pub const DEFAULT_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PloyzctlRuntimeConfig {
    pub nats_url: Option<String>,
    pub nats_connect_timeout: Option<Duration>,
}

impl PloyzctlRuntimeConfig {
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            nats_url: std::env::var(PLOYZ_NATS_URL_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty()),
            nats_connect_timeout: None,
        }
    }

    #[must_use]
    pub fn with_nats_url(mut self, nats_url: Option<String>) -> Self {
        if nats_url.is_some() {
            self.nats_url = nats_url;
        }
        self
    }

    #[must_use]
    pub fn nats_connect_timeout(&self) -> Duration {
        self.nats_connect_timeout
            .unwrap_or(DEFAULT_NATS_CONNECT_TIMEOUT)
    }
}

pub async fn execute_command(
    command: PloyzctlCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<String, PloyzctlExecutionError> {
    match command {
        PloyzctlCommand::Help => Ok(format!("{USAGE}\n")),
        PloyzctlCommand::Init(command) => Ok(command.render()),
        PloyzctlCommand::MachineAdd(command) => {
            let nats_url = config.nats_url.clone();
            let Some(nats_url) = nats_url else {
                return Err(PloyzctlExecutionError::MissingNatsUrl);
            };

            let client = connect_nats(&nats_url, config.nats_connect_timeout()).await?;
            let api = OperationApiClient::new(client);
            let request = command.into_request();
            let accepted = api
                .machine_add(&request)
                .await
                .map_err(|source| PloyzctlExecutionError::MachineAddApi { source })?;

            Ok(crate::commands::machine::MachineAddOutput::from_accepted(accepted).render())
        }
    }
}

async fn connect_nats(
    nats_url: &str,
    timeout: Duration,
) -> Result<async_nats::Client, PloyzctlExecutionError> {
    match tokio::time::timeout(timeout, async_nats::connect(nats_url)).await {
        Ok(Ok(client)) => Ok(client),
        Ok(Err(error)) => Err(PloyzctlExecutionError::NatsConnect {
            url: nats_url.to_owned(),
            message: error.to_string(),
        }),
        Err(_) => Err(PloyzctlExecutionError::NatsConnectTimeout {
            url: nats_url.to_owned(),
            timeout,
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PloyzctlExecutionError {
    MissingNatsUrl,
    NatsConnect {
        url: String,
        message: String,
    },
    NatsConnectTimeout {
        url: String,
        timeout: Duration,
    },
    MachineAddApi {
        source: OperationApiClientError<MachineAddError>,
    },
}

impl fmt::Display for PloyzctlExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNatsUrl => write!(formatter, "--nats or {PLOYZ_NATS_URL_ENV} is required"),
            Self::NatsConnect { url, message } => {
                write!(formatter, "failed to connect to NATS at {url}: {message}")
            }
            Self::NatsConnectTimeout { url, timeout } => write!(
                formatter,
                "failed to connect to NATS at {url} within {}ms",
                timeout.as_millis()
            ),
            Self::MachineAddApi { source } => write!(formatter, "{source}"),
        }
    }
}

impl std::error::Error for PloyzctlExecutionError {}
