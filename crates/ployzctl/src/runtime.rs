//! Runtime execution for parsed CLI commands.

use std::fmt;
use std::time::Duration;

use crate::api_client::{OperationApiClient, OperationApiClientError};
use crate::commands::{PloyzctlCommand, USAGE};
use ployz_nats::connect::{NatsConnectError, connect_with_timeout};
use ployz_sdk_types::{DeploySubmitError, MachineAddError, OpsWatchError};

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
        PloyzctlCommand::Deploy(command) => {
            let api = operation_api_client(config).await?;
            let request = command.into_request();
            let accepted = api
                .deploy_submit(&request)
                .await
                .map_err(|source| PloyzctlExecutionError::DeploySubmitApi { source })?;

            Ok(crate::commands::deploy::DetachedDeployOutput::from_accepted(accepted).render())
        }
        PloyzctlCommand::Init(command) => Ok(command.render()),
        PloyzctlCommand::MachineAdd(command) => {
            let api = operation_api_client(config).await?;
            let request = command.into_request();
            let accepted = api
                .machine_add(&request)
                .await
                .map_err(|source| PloyzctlExecutionError::MachineAddApi { source })?;

            Ok(crate::commands::machine::MachineAddOutput::from_accepted(accepted).render())
        }
        PloyzctlCommand::OpsWatch(command) => {
            let api = operation_api_client(config).await?;
            let request = command.into_request();
            let page = api
                .ops_watch(&request)
                .await
                .map_err(|source| PloyzctlExecutionError::OpsWatchApi { source })?;

            Ok(crate::commands::ops::WatchOutput {
                events: page.events,
            }
            .render())
        }
    }
}

async fn operation_api_client(
    config: &PloyzctlRuntimeConfig,
) -> Result<OperationApiClient, PloyzctlExecutionError> {
    let nats_url = config.nats_url.clone();
    let Some(nats_url) = nats_url else {
        return Err(PloyzctlExecutionError::MissingNatsUrl);
    };

    connect_with_timeout(&nats_url, config.nats_connect_timeout())
        .await
        .map(OperationApiClient::new)
        .map_err(PloyzctlExecutionError::NatsConnect)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PloyzctlExecutionError {
    MissingNatsUrl,
    NatsConnect(NatsConnectError),
    DeploySubmitApi {
        source: OperationApiClientError<DeploySubmitError>,
    },
    MachineAddApi {
        source: OperationApiClientError<MachineAddError>,
    },
    OpsWatchApi {
        source: OperationApiClientError<OpsWatchError>,
    },
}

impl fmt::Display for PloyzctlExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNatsUrl => write!(formatter, "--nats or {PLOYZ_NATS_URL_ENV} is required"),
            Self::NatsConnect(error) => write!(formatter, "{error}"),
            Self::DeploySubmitApi { source } => write!(formatter, "{source}"),
            Self::MachineAddApi { source } => write!(formatter, "{source}"),
            Self::OpsWatchApi { source } => write!(formatter, "{source}"),
        }
    }
}

impl std::error::Error for PloyzctlExecutionError {}
