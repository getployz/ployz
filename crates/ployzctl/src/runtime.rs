//! Runtime execution for parsed CLI commands.

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::api_client::{NatsServiceRequestFailure, OperationApiClient, OperationApiClientError};
use crate::commands::PloyzctlCommand;
use crate::commands::init::{
    FirstNodeActivateCommand, FirstNodeActivationOutput, FirstNodeInitMode,
};
use crate::keeper_install::{LocalKeeperInstallError, run_keeper_first_node_install};
use ployz_core::ids::OperationId;
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::ops::{
    EventSequence, OperationEventReplayCursor, OperationEventReplayRequest, ReplayedOperationEvent,
};
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{
    NatsClientAuth, NatsClientUrl, NatsClientUrlError, NatsConnectConfig, NatsConnectError,
    NatsTlsTrust, connect_authenticated,
};
use ployz_sdk_types::{InitFirstNodeActivateError, OpsStatusRequest};
use tokio::time::sleep as async_sleep;

pub const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
pub const PLOYZ_NATS_CA_FILE_ENV: &str = "PLOYZ_NATS_CA_FILE";
pub const PLOYZ_NATS_NKEY_SEED_FILE_ENV: &str = "PLOYZ_NATS_NKEY_SEED_FILE";
pub const PLOYZ_JOIN_NKEY_SEED_FILE_ENV: &str = "PLOYZ_JOIN_NKEY_SEED_FILE";
pub const DEFAULT_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_KEEPER_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
pub const DEFAULT_OPS_WATCH_TIMEOUT: Duration = Duration::from_secs(600);
pub const DEFAULT_OPS_WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PloyzctlRuntimeConfig {
    pub nats_url: Option<String>,
    pub nats_ca_file: Option<PathBuf>,
    pub nats_seed_file: Option<PathBuf>,
    pub join_seed_file: Option<PathBuf>,
    pub nats_connect_timeout: Option<Duration>,
    pub keeper_install_timeout: Option<Duration>,
    pub ops_watch_timeout: Option<Duration>,
    pub ops_watch_poll_interval: Option<Duration>,
}

impl PloyzctlRuntimeConfig {
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            nats_url: std::env::var(PLOYZ_NATS_URL_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty()),
            nats_ca_file: std::env::var(PLOYZ_NATS_CA_FILE_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from),
            nats_seed_file: std::env::var(PLOYZ_NATS_NKEY_SEED_FILE_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from),
            join_seed_file: std::env::var(PLOYZ_JOIN_NKEY_SEED_FILE_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from),
            nats_connect_timeout: None,
            keeper_install_timeout: None,
            ops_watch_timeout: None,
            ops_watch_poll_interval: None,
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

    #[must_use]
    pub fn keeper_install_timeout(&self) -> Duration {
        self.keeper_install_timeout
            .unwrap_or(DEFAULT_KEEPER_INSTALL_TIMEOUT)
    }

    #[must_use]
    pub fn ops_watch_timeout(&self) -> Duration {
        self.ops_watch_timeout.unwrap_or(DEFAULT_OPS_WATCH_TIMEOUT)
    }

    /// Retry budget for `init activate-first-node`.
    ///
    /// Activation waits server-side for the first machine-add mint, whose
    /// `nats-server` authorization reload drops the server's in-flight
    /// response permissions — the reply to the very request that triggered
    /// the mint can be lost, timing out client-side while the activation
    /// completed. The retried request returns the already-active machine,
    /// so the budget is the operation-wait budget and must exceed a single
    /// API request timeout (regression: it was the connect timeout, which
    /// expired before the first retry could run).
    #[must_use]
    pub fn first_node_activate_retry_budget(&self) -> Duration {
        self.ops_watch_timeout()
    }

    #[must_use]
    pub fn ops_watch_poll_interval(&self) -> Duration {
        self.ops_watch_poll_interval
            .unwrap_or(DEFAULT_OPS_WATCH_POLL_INTERVAL)
    }
}

pub async fn execute_command(
    command: PloyzctlCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    match command {
        PloyzctlCommand::Deploy(command) => {
            render_api_call(
                config,
                async |api| api.deploy_submit(&command.into_request()).await,
                |accepted| {
                    crate::commands::deploy::DetachedDeployOutput::from_accepted(accepted).render()
                },
            )
            .await
        }
        PloyzctlCommand::BackupCreate(command) => {
            render_api_call(
                config,
                async |api| api.backup_create(&command.into_request()).await,
                |accepted| {
                    crate::commands::backup::BackupCreateOutput::from_accepted(accepted).render()
                },
            )
            .await
        }
        PloyzctlCommand::BackupRestorePlan(_) => Ok(PloyzctlExecutionOutput::stdout(
            crate::commands::backup::BackupRestorePlanOutput::single_core().render(),
        )),
        PloyzctlCommand::Init(command) => match &command.mode {
            FirstNodeInitMode::RunKeeperInstall {
                keeper_install,
                keeper_binary,
            } => {
                let output = run_keeper_first_node_install(
                    keeper_binary,
                    keeper_install,
                    config.keeper_install_timeout(),
                )
                .map_err(|source| PloyzctlExecutionError::KeeperFirstNodeInstall { source })?;
                Ok(PloyzctlExecutionOutput {
                    stdout: output.stdout,
                    stderr: output.stderr,
                })
            }
            FirstNodeInitMode::Summary { .. } | FirstNodeInitMode::EmitKeeperInstall(_) => {
                Ok(PloyzctlExecutionOutput::stdout(command.render()))
            }
        },
        PloyzctlCommand::InitFirstNodeActivate(command) => {
            let activation = activate_first_node_machine(&command, config).await?;
            Ok(PloyzctlExecutionOutput::stdout(activation.render()))
        }
        PloyzctlCommand::InitJoinTemplate(command) => {
            Ok(PloyzctlExecutionOutput::stdout(command.render_json()))
        }
        PloyzctlCommand::MachineAdd(command) => {
            let nats_connect = nats_connect_config(config)?;
            // The install line embeds the cluster-static Join seed
            // (deliberately low-privilege) — read it before submitting so
            // a missing seed fails fast without creating an operation.
            let join_seed = read_join_seed(config)?;
            let api = operation_api_client_with_connect(config, nats_connect).await?;
            let accepted = api
                .machine_add(&command.into_request())
                .await
                .map_err(api_error)?;

            Ok(PloyzctlExecutionOutput::stdout(
                crate::commands::machine::MachineAddOutput::from_accepted(accepted, join_seed)
                    .render(),
            ))
        }
        PloyzctlCommand::MachineList(command) => {
            render_api_call(
                config,
                async |api| api.machine_list(&command.into_request()).await,
                |result| crate::commands::machine::MachineListOutput::from_result(result).render(),
            )
            .await
        }
        PloyzctlCommand::MachineInspect(command) => {
            render_api_call(
                config,
                async |api| api.machine_inspect(&command.into_request()).await,
                |machine| crate::commands::machine::MachineInspectOutput::new(machine).render(),
            )
            .await
        }
        PloyzctlCommand::ServiceList(command) => {
            render_api_call(
                config,
                async |api| api.service_list(&command.into_request()).await,
                |result| crate::commands::service::ServiceListOutput::from_result(result).render(),
            )
            .await
        }
        PloyzctlCommand::ServiceInspect(command) => {
            render_api_call(
                config,
                async |api| api.service_inspect(&command.into_request()).await,
                |service| crate::commands::service::ServiceInspectOutput::new(service).render(),
            )
            .await
        }
        PloyzctlCommand::LogsTail(command) => {
            render_api_call(
                config,
                async |api| api.logs_tail(&command.into_request()).await,
                |result| crate::commands::logs::LogsTailOutput::new(result).render(),
            )
            .await
        }
        PloyzctlCommand::OpsStatus(command) => {
            render_api_call(
                config,
                async |api| api.ops_status(&command.into_request()).await,
                |snapshot| crate::commands::ops::StatusOutput::new(snapshot).render(),
            )
            .await
        }
        PloyzctlCommand::OpsWatch(command) => {
            let api = operation_api_client(config).await?;
            let output = command.output;
            let request = command.into_request();
            let events = watch_operation_until_terminal(
                &api,
                request,
                config.ops_watch_timeout(),
                config.ops_watch_poll_interval(),
            )
            .await?;

            Ok(PloyzctlExecutionOutput::stdout(
                crate::commands::ops::WatchOutput { events, output }.render(),
            ))
        }
    }
}

/// Connects, issues one operation API request, and renders the success value
/// to stdout; API failures arrive as one rendered execution error.
async fn render_api_call<T, E>(
    config: &PloyzctlRuntimeConfig,
    call: impl AsyncFnOnce(OperationApiClient) -> Result<T, OperationApiClientError<E>>,
    render: impl FnOnce(T) -> String,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError>
where
    E: fmt::Debug,
{
    let api = operation_api_client(config).await?;
    let value = call(api).await.map_err(api_error)?;
    Ok(PloyzctlExecutionOutput::stdout(render(value)))
}

/// Operation API failures are terminal for the CLI, so carry the rendered
/// message instead of one error variant per endpoint.
fn api_error<E>(source: OperationApiClientError<E>) -> PloyzctlExecutionError
where
    E: fmt::Debug,
{
    PloyzctlExecutionError::OperationApi {
        message: source.to_string(),
    }
}

async fn watch_operation_until_terminal(
    api: &OperationApiClient,
    mut request: OperationEventReplayRequest,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<Vec<ReplayedOperationEvent>, PloyzctlExecutionError> {
    let operation_id = request.operation_id.clone();
    let started_at = Instant::now();
    let mut events = Vec::new();

    loop {
        let page = api.ops_watch(&request).await.map_err(api_error)?;

        let cursor = page.cursor;
        if let Some(last_event) = page.events.last()
            && let Some(next_sequence) = next_event_sequence(last_event.sequence)
        {
            request.start_sequence = next_sequence;
        }
        events.extend(page.events);

        match cursor {
            OperationEventReplayCursor::More {
                next_start_sequence,
            } => {
                request.start_sequence = next_start_sequence;
                continue;
            }
            OperationEventReplayCursor::Terminal => return Ok(events),
            OperationEventReplayCursor::CaughtUp => {}
        }

        let snapshot = api
            .ops_status(&OpsStatusRequest {
                operation_id: operation_id.clone(),
            })
            .await
            .map_err(api_error)?;

        if snapshot.status.is_terminal() {
            return Ok(events);
        }

        if started_at.elapsed() >= timeout {
            return Err(PloyzctlExecutionError::OpsWatchTimedOut {
                operation_id,
                timeout,
            });
        }

        async_sleep(poll_interval).await;
    }
}

fn next_event_sequence(sequence: EventSequence) -> Option<EventSequence> {
    let next = sequence.get().checked_add(1)?;
    EventSequence::try_new(next).ok()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PloyzctlExecutionOutput {
    pub stdout: String,
    pub stderr: String,
}

impl PloyzctlExecutionOutput {
    #[must_use]
    pub fn stdout(stdout: String) -> Self {
        Self {
            stdout,
            stderr: String::new(),
        }
    }
}

async fn activate_first_node_machine(
    command: &FirstNodeActivateCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<FirstNodeActivationOutput, PloyzctlExecutionError> {
    let deadline = Instant::now() + config.first_node_activate_retry_budget();
    loop {
        match activate_first_node_machine_once(command, config).await {
            Ok(activation) => return Ok(activation),
            Err(error)
                if error.is_first_node_activation_retryable() && Instant::now() < deadline =>
            {
                async_sleep(config.ops_watch_poll_interval()).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn activate_first_node_machine_once(
    command: &FirstNodeActivateCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<FirstNodeActivationOutput, PloyzctlExecutionError> {
    let api = operation_api_client(config).await?;
    let activated = api
        .init_first_node_activate(&command.clone().into_request())
        .await
        .map_err(|source| PloyzctlExecutionError::FirstNodeActivateApi { source })?;

    Ok(FirstNodeActivationOutput {
        operation_id: activated.operation_id,
        node_id: activated.node_id,
    })
}

/// Reads the cluster-static Join seed for the `machine add` install line.
fn read_join_seed(config: &PloyzctlRuntimeConfig) -> Result<NatsUserSeed, PloyzctlExecutionError> {
    let path = config.join_seed_file.clone().unwrap_or_else(|| {
        ployz_core::install::NatsMachineMaterialPaths::in_default_state_dir().join_seed_file()
    });
    let raw =
        fs::read_to_string(&path).map_err(|error| PloyzctlExecutionError::ReadJoinSeedFile {
            path: path.clone(),
            message: error.to_string(),
        })?;
    NatsUserSeed::try_new(raw.trim())
        .map_err(|_| PloyzctlExecutionError::InvalidJoinSeedFile { path })
}

async fn operation_api_client(
    config: &PloyzctlRuntimeConfig,
) -> Result<OperationApiClient, PloyzctlExecutionError> {
    let connect = nats_connect_config(config)?;
    operation_api_client_with_connect(config, connect).await
}

async fn operation_api_client_with_connect(
    config: &PloyzctlRuntimeConfig,
    connect: NatsConnectConfig,
) -> Result<OperationApiClient, PloyzctlExecutionError> {
    connect_authenticated(&connect, config.nats_connect_timeout())
        .await
        .map(OperationApiClient::new)
        .map_err(PloyzctlExecutionError::NatsConnect)
}

fn nats_connect_config(
    config: &PloyzctlRuntimeConfig,
) -> Result<NatsConnectConfig, PloyzctlExecutionError> {
    let nats_url = config.nats_url.clone();
    let Some(nats_url) = nats_url else {
        return Err(PloyzctlExecutionError::MissingNatsUrl);
    };
    let nats_url =
        NatsClientUrl::try_new(nats_url).map_err(PloyzctlExecutionError::InvalidNatsUrl)?;
    let Some(ca_file) = config.nats_ca_file.clone() else {
        return Err(PloyzctlExecutionError::MissingNatsCaFile);
    };
    let Some(seed_file) = config.nats_seed_file.clone() else {
        return Err(PloyzctlExecutionError::MissingNatsSeedFile);
    };
    let raw_seed = fs::read_to_string(&seed_file).map_err(|error| {
        PloyzctlExecutionError::ReadNatsSeedFile {
            path: seed_file.clone(),
            message: error.to_string(),
        }
    })?;
    let seed = NatsUserSeed::try_new(raw_seed.trim()).map_err(|_| {
        PloyzctlExecutionError::InvalidNatsSeedFile {
            path: seed_file.clone(),
        }
    })?;
    Ok(NatsConnectConfig {
        url: nats_url,
        auth: NatsClientAuth::NkeySeed(seed),
        trust: NatsTlsTrust::ClusterCa(ca_file),
        principal: NatsPrincipal::User,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PloyzctlExecutionError {
    MissingNatsUrl,
    InvalidNatsUrl(NatsClientUrlError),
    MissingNatsCaFile,
    MissingNatsSeedFile,
    ReadNatsSeedFile {
        path: PathBuf,
        message: String,
    },
    InvalidNatsSeedFile {
        path: PathBuf,
    },
    ReadJoinSeedFile {
        path: PathBuf,
        message: String,
    },
    InvalidJoinSeedFile {
        path: PathBuf,
    },
    NatsConnect(NatsConnectError),
    KeeperFirstNodeInstall {
        source: Box<LocalKeeperInstallError>,
    },
    OperationApi {
        message: String,
    },
    FirstNodeActivateApi {
        source: OperationApiClientError<InitFirstNodeActivateError>,
    },
    OpsWatchTimedOut {
        operation_id: OperationId,
        timeout: Duration,
    },
}

impl PloyzctlExecutionError {
    /// Only a first-node activation reply dropped by the mint's
    /// authorization reload is retryable; every other failure is final.
    fn is_first_node_activation_retryable(&self) -> bool {
        let Self::FirstNodeActivateApi { source } = self else {
            return false;
        };
        matches!(
            source,
            OperationApiClientError::Request {
                failure: NatsServiceRequestFailure::NoResponders
                    | NatsServiceRequestFailure::TimedOut,
                ..
            }
        )
    }
}

impl fmt::Display for PloyzctlExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNatsUrl => write!(formatter, "--nats or {PLOYZ_NATS_URL_ENV} is required"),
            Self::InvalidNatsUrl(error) => {
                write!(
                    formatter,
                    "--nats or {PLOYZ_NATS_URL_ENV} is invalid: {error:?}"
                )
            }
            Self::MissingNatsCaFile => {
                write!(formatter, "{PLOYZ_NATS_CA_FILE_ENV} is required")
            }
            Self::MissingNatsSeedFile => {
                write!(formatter, "{PLOYZ_NATS_NKEY_SEED_FILE_ENV} is required")
            }
            Self::ReadNatsSeedFile { path, message } => write!(
                formatter,
                "{PLOYZ_NATS_NKEY_SEED_FILE_ENV} file {} is unreadable: {message}",
                path.display()
            ),
            Self::InvalidNatsSeedFile { path } => write!(
                formatter,
                "{PLOYZ_NATS_NKEY_SEED_FILE_ENV} file {} does not contain an SU-prefixed user seed",
                path.display()
            ),
            Self::ReadJoinSeedFile { path, message } => write!(
                formatter,
                "join seed file {} is unreadable (set {PLOYZ_JOIN_NKEY_SEED_FILE_ENV}): {message}",
                path.display()
            ),
            Self::InvalidJoinSeedFile { path } => write!(
                formatter,
                "join seed file {} does not contain an SU-prefixed user seed",
                path.display()
            ),
            Self::NatsConnect(error) => write!(formatter, "{error}"),
            Self::KeeperFirstNodeInstall { source } => write!(formatter, "{source}"),
            Self::OperationApi { message } => formatter.write_str(message),
            Self::FirstNodeActivateApi { source } => {
                write!(formatter, "first node activation failed: {source}")
            }
            Self::OpsWatchTimedOut {
                operation_id,
                timeout,
            } => write!(
                formatter,
                "operation {} did not reach a terminal state within {}s",
                operation_id.as_str(),
                timeout.as_secs()
            ),
        }
    }
}

impl std::error::Error for PloyzctlExecutionError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the activate-first-node retry budget was the NATS
    /// connect timeout, which a single timed-out request (its reply dropped
    /// by the mint's authorization reload) consumed entirely — the
    /// documented retry never ran. The budget is the operation-wait budget
    /// and must leave room for a retry after a full request timeout.
    #[test]
    fn first_node_activation_can_retry_after_a_dropped_reply() {
        let config = PloyzctlRuntimeConfig::default();
        assert_eq!(
            config.first_node_activate_retry_budget(),
            config.ops_watch_timeout()
        );
        assert!(
            config.first_node_activate_retry_budget()
                > ployz_nats::operation_api_client::DEFAULT_OPERATION_API_REQUEST_TIMEOUT
                    + config.ops_watch_poll_interval(),
            "budget must allow at least one retry after a timed-out request"
        );
    }
}
