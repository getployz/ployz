//! Runtime execution for parsed CLI commands.

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::api_client::{NatsServiceRequestFailure, OperationApiClient, OperationApiClientError};
use crate::commands::PloyzctlCommand;
use crate::commands::init::{
    FirstMachineActivateCommand, FirstMachineActivationOutput, FirstMachineInitMode,
};
use crate::config::{ClusterContext, ClusterContextError, load_cluster_context};
use crate::keeper_install::{LocalKeeperInstallError, run_keeper_first_machine_install};
use crate::remote_machine_runtime::{
    RemoteMachineExecutionError, execute_machine_add_remote, execute_machine_init,
};
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
use ployz_sdk_types::{InitFirstMachineActivateError, OpsStatusRequest};
use tokio::time::sleep as async_sleep;

pub const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
pub const PLOYZ_NATS_CA_FILE_ENV: &str = "PLOYZ_NATS_CA_FILE";
pub const PLOYZ_NATS_NKEY_SEED_FILE_ENV: &str = "PLOYZ_NATS_NKEY_SEED_FILE";
pub const PLOYZ_JOIN_NKEY_SEED_FILE_ENV: &str = "PLOYZ_JOIN_NKEY_SEED_FILE";
/// Stand-in for the system `ssh` (test/automation seam for the remote
/// machine bootstrap commands).
pub const PLOYZ_SSH_PROGRAM_ENV: &str = "PLOYZ_SSH_PROGRAM";
pub const DEFAULT_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_KEEPER_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
pub const DEFAULT_OPS_WATCH_TIMEOUT: Duration = Duration::from_secs(600);
pub const DEFAULT_OPS_WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Budget for the remote installer phase (artifact downloads plus the keeper
/// install); much longer than the per-command probe timeout.
pub const DEFAULT_SSH_INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

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
    /// Program to run instead of the system `ssh` (test seam).
    pub ssh_program: Option<PathBuf>,
    /// Per-command budget for the remote installer phase.
    pub ssh_install_timeout: Option<Duration>,
    /// Where `machine init` records the local cluster context (test seam;
    /// defaults to the user config directory).
    pub cluster_context_path: Option<PathBuf>,
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
            ssh_program: std::env::var(PLOYZ_SSH_PROGRAM_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from),
            ssh_install_timeout: None,
            cluster_context_path: None,
        }
    }

    #[must_use]
    pub fn with_nats_url(mut self, nats_url: Option<String>) -> Self {
        if nats_url.is_some() {
            self.nats_url = nats_url;
        }
        self
    }

    /// Fills connection fields from the local cluster context without
    /// overriding `--nats` or environment variables: apply this after
    /// [`Self::from_env`] and before [`Self::with_nats_url`].
    #[must_use]
    pub fn with_cluster_context(mut self, context: Option<ClusterContext>) -> Self {
        let Some(context) = context else {
            return self;
        };
        let ClusterContext {
            nats_url,
            nats_ca_file,
            operator_seed_file,
            join_seed_file,
            machines: _,
        } = context;
        if self.nats_url.is_none() {
            self.nats_url = Some(nats_url.as_str().to_owned());
        }
        if self.nats_ca_file.is_none() {
            self.nats_ca_file = Some(nats_ca_file);
        }
        if self.nats_seed_file.is_none() {
            self.nats_seed_file = Some(operator_seed_file);
        }
        if self.join_seed_file.is_none() {
            self.join_seed_file = join_seed_file;
        }
        self
    }

    pub(crate) fn with_cluster_context_from_disk(self) -> Result<Self, PloyzctlExecutionError> {
        if !self.needs_cluster_context_from_disk() {
            return Ok(self);
        }
        let Some(path) = self
            .cluster_context_path
            .clone()
            .or_else(crate::config::default_cluster_context_path)
        else {
            return Ok(self);
        };
        let context = load_cluster_context(&path)
            .map_err(|source| PloyzctlExecutionError::ClusterContext { source })?;
        Ok(self.with_cluster_context(context))
    }

    fn needs_cluster_context_from_disk(&self) -> bool {
        self.nats_url.is_none()
            || self.nats_ca_file.is_none()
            || self.nats_seed_file.is_none()
            || self.join_seed_file.is_none()
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

    #[must_use]
    pub fn ssh_install_timeout(&self) -> Duration {
        self.ssh_install_timeout
            .unwrap_or(DEFAULT_SSH_INSTALL_TIMEOUT)
    }

    /// Retry budget for `init activate-first-machine`.
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
    pub fn first_machine_activate_retry_budget(&self) -> Duration {
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
            let detach = command.detach;
            let api = operation_api_client(config).await?;
            let accepted = api
                .deploy_submit(&command.into_request())
                .await
                .map_err(api_error)?;
            if detach {
                return Ok(PloyzctlExecutionOutput::stdout(
                    crate::commands::deploy::DeployOutput::from_accepted(accepted).render(),
                ));
            }
            watch_accepted_operation(&api, accepted.operation_id, config).await
        }
        PloyzctlCommand::InternalInit(command) => match &command.mode {
            FirstMachineInitMode::RunKeeperInstall {
                keeper_install,
                keeper_binary,
            } => {
                let output = run_keeper_first_machine_install(
                    keeper_binary,
                    keeper_install,
                    config.keeper_install_timeout(),
                )
                .map_err(|source| PloyzctlExecutionError::KeeperFirstMachineInstall { source })?;
                Ok(PloyzctlExecutionOutput {
                    stdout: output.stdout,
                    stderr: output.stderr,
                })
            }
            FirstMachineInitMode::Summary { .. } | FirstMachineInitMode::EmitKeeperInstall(_) => {
                Ok(PloyzctlExecutionOutput::stdout(command.render()))
            }
        },
        PloyzctlCommand::InitFirstMachineActivate(command) => {
            let activation = activate_first_machine(&command, config).await?;
            Ok(PloyzctlExecutionOutput::stdout(activation.render()))
        }
        PloyzctlCommand::InitJoinTemplate(command) => {
            Ok(PloyzctlExecutionOutput::stdout(command.render_json()))
        }
        PloyzctlCommand::MachineInit(command) => execute_machine_init(command, config).await,
        PloyzctlCommand::MachineAddRemote(command) => {
            execute_machine_add_remote(command, config).await
        }
        PloyzctlCommand::MachineAdd(command) => {
            let config = config.clone().with_cluster_context_from_disk()?;
            let nats_connect = nats_connect_config(&config)?;
            // The install line embeds the cluster-static Join seed
            // (deliberately low-privilege) — read it before submitting so
            // a missing seed fails fast without creating an operation.
            let join_seed = read_join_seed(&config)?;
            let api = operation_api_client_with_connect(&config, nats_connect).await?;
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
        PloyzctlCommand::OpsList(command) => {
            render_api_call(
                config,
                async |api| api.ops_list(&command.into_request()).await,
                |result| crate::commands::ops::ListOutput::from_result(result).render(),
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

async fn watch_accepted_operation(
    api: &OperationApiClient,
    operation_id: OperationId,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let events = watch_operation_until_terminal(
        api,
        OperationEventReplayRequest {
            operation_id,
            start_sequence: EventSequence::try_new(1).expect("one is a valid event sequence"),
            limit: ployz_core::ops::OperationEventReplayLimit::try_new(
                ployz_core::ops::MAX_OPERATION_EVENT_REPLAY_LIMIT,
            )
            .expect("max replay limit is valid"),
        },
        config.ops_watch_timeout(),
        config.ops_watch_poll_interval(),
    )
    .await?;
    Ok(PloyzctlExecutionOutput::stdout(
        crate::commands::ops::WatchOutput {
            events,
            output: crate::commands::ops::OpsWatchOutput::Text,
        }
        .render(),
    ))
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
pub(crate) fn api_error<E>(source: OperationApiClientError<E>) -> PloyzctlExecutionError
where
    E: fmt::Debug,
{
    PloyzctlExecutionError::OperationApi {
        message: source.to_string(),
    }
}

pub(crate) async fn watch_operation_until_terminal(
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

pub(crate) async fn activate_first_machine(
    command: &FirstMachineActivateCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<FirstMachineActivationOutput, PloyzctlExecutionError> {
    let deadline = Instant::now() + config.first_machine_activate_retry_budget();
    loop {
        match activate_first_machine_once(command, config).await {
            Ok(activation) => return Ok(activation),
            Err(error)
                if error.is_first_machine_activation_retryable() && Instant::now() < deadline =>
            {
                async_sleep(config.ops_watch_poll_interval()).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn activate_first_machine_once(
    command: &FirstMachineActivateCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<FirstMachineActivationOutput, PloyzctlExecutionError> {
    let api = operation_api_client(config).await?;
    let activated = api
        .init_first_machine_activate(&command.clone().into_request())
        .await
        .map_err(|source| PloyzctlExecutionError::FirstMachineActivateApi { source })?;

    Ok(FirstMachineActivationOutput {
        operation_id: activated.operation_id,
        machine_id: activated.machine_id,
    })
}

/// Reads the cluster-static Join seed for the `machine add` install line.
pub(crate) fn read_join_seed(
    config: &PloyzctlRuntimeConfig,
) -> Result<NatsUserSeed, PloyzctlExecutionError> {
    let Some(path) = config.join_seed_file.clone() else {
        return Err(PloyzctlExecutionError::MissingJoinSeedFile);
    };
    let raw =
        fs::read_to_string(&path).map_err(|error| PloyzctlExecutionError::ReadJoinSeedFile {
            path: path.clone(),
            message: error.to_string(),
        })?;
    NatsUserSeed::try_new(raw.trim())
        .map_err(|_| PloyzctlExecutionError::InvalidJoinSeedFile { path })
}

pub(crate) async fn operation_api_client(
    config: &PloyzctlRuntimeConfig,
) -> Result<OperationApiClient, PloyzctlExecutionError> {
    let config = config.clone().with_cluster_context_from_disk()?;
    let connect = nats_connect_config(&config)?;
    operation_api_client_with_connect(&config, connect).await
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
    MissingJoinSeedFile,
    ReadJoinSeedFile {
        path: PathBuf,
        message: String,
    },
    InvalidJoinSeedFile {
        path: PathBuf,
    },
    NatsConnect(NatsConnectError),
    KeeperFirstMachineInstall {
        source: Box<LocalKeeperInstallError>,
    },
    ClusterContext {
        source: ClusterContextError,
    },
    RemoteMachine {
        source: Box<RemoteMachineExecutionError>,
    },
    OperationApi {
        message: String,
    },
    FirstMachineActivateApi {
        source: OperationApiClientError<InitFirstMachineActivateError>,
    },
    OpsWatchTimedOut {
        operation_id: OperationId,
        timeout: Duration,
    },
}

impl PloyzctlExecutionError {
    /// Only a first-machine activation reply dropped by the mint's
    /// authorization reload is retryable; every other failure is final.
    fn is_first_machine_activation_retryable(&self) -> bool {
        let Self::FirstMachineActivateApi { source } = self else {
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
            Self::MissingNatsUrl => write!(
                formatter,
                "no cluster context: run `ployzctl machine init USER@HOST` to create one, pass --nats, or set {PLOYZ_NATS_URL_ENV}"
            ),
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
            Self::MissingJoinSeedFile => write!(
                formatter,
                "machine add requires a join seed from the cluster context or {PLOYZ_JOIN_NKEY_SEED_FILE_ENV}; run `ployzctl machine init USER@HOST` with the current CLI to refresh the context"
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
            Self::KeeperFirstMachineInstall { source } => write!(formatter, "{source}"),
            Self::ClusterContext { source } => write!(formatter, "{source}"),
            Self::RemoteMachine { source } => write!(formatter, "{source}"),
            Self::OperationApi { message } => formatter.write_str(message),
            Self::FirstMachineActivateApi { source } => {
                write!(formatter, "first machine activation failed: {source}")
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

    fn cluster_context() -> ClusterContext {
        ClusterContext {
            nats_url: ployz_nats::connect::NatsClientUrl::try_new("tls://203.0.113.10:4222")
                .expect("test NATS URL is valid"),
            nats_ca_file: PathBuf::from("/context/ca.pem"),
            operator_seed_file: PathBuf::from("/context/operator.seed"),
            join_seed_file: Some(PathBuf::from("/context/join.seed")),
            machines: Vec::new(),
        }
    }

    #[test]
    fn cluster_context_fills_missing_connection_fields() {
        let config = PloyzctlRuntimeConfig::default().with_cluster_context(Some(cluster_context()));

        assert_eq!(config.nats_url.as_deref(), Some("tls://203.0.113.10:4222"));
        assert_eq!(config.nats_ca_file, Some(PathBuf::from("/context/ca.pem")));
        assert_eq!(
            config.nats_seed_file,
            Some(PathBuf::from("/context/operator.seed"))
        );
        assert_eq!(
            config.join_seed_file,
            Some(PathBuf::from("/context/join.seed"))
        );
    }

    #[test]
    fn environment_values_win_over_cluster_context() {
        let env_config = PloyzctlRuntimeConfig {
            nats_url: Some("tls://env.example:4222".to_owned()),
            nats_ca_file: Some(PathBuf::from("/env/ca.pem")),
            nats_seed_file: Some(PathBuf::from("/env/operator.seed")),
            join_seed_file: Some(PathBuf::from("/env/join.seed")),
            nats_connect_timeout: None,
            keeper_install_timeout: None,
            ops_watch_timeout: None,
            ops_watch_poll_interval: None,
            ssh_program: None,
            ssh_install_timeout: None,
            cluster_context_path: None,
        };

        let config = env_config.with_cluster_context(Some(cluster_context()));

        assert_eq!(config.nats_url.as_deref(), Some("tls://env.example:4222"));
        assert_eq!(config.nats_ca_file, Some(PathBuf::from("/env/ca.pem")));
        assert_eq!(
            config.nats_seed_file,
            Some(PathBuf::from("/env/operator.seed"))
        );
        assert_eq!(config.join_seed_file, Some(PathBuf::from("/env/join.seed")));
    }

    #[test]
    fn nats_flag_wins_over_cluster_context() {
        let config = PloyzctlRuntimeConfig::default()
            .with_cluster_context(Some(cluster_context()))
            .with_nats_url(Some("tls://flag.example:4222".to_owned()));

        assert_eq!(config.nats_url.as_deref(), Some("tls://flag.example:4222"));
    }

    #[test]
    fn missing_cluster_context_changes_nothing() {
        let config = PloyzctlRuntimeConfig::default().with_cluster_context(None);

        assert_eq!(config, PloyzctlRuntimeConfig::default());
    }

    #[test]
    fn machine_add_join_seed_requires_context_or_explicit_seed_path() {
        assert_eq!(
            read_join_seed(&PloyzctlRuntimeConfig::default()),
            Err(PloyzctlExecutionError::MissingJoinSeedFile)
        );
    }

    /// Regression: the activate-first-machine retry budget was the NATS
    /// connect timeout, which a single timed-out request (its reply dropped
    /// by the mint's authorization reload) consumed entirely — the
    /// documented retry never ran. The budget is the operation-wait budget
    /// and must leave room for a retry after a full request timeout.
    #[test]
    fn first_machine_activation_can_retry_after_a_dropped_reply() {
        let config = PloyzctlRuntimeConfig::default();
        assert_eq!(
            config.first_machine_activate_retry_budget(),
            config.ops_watch_timeout()
        );
        assert!(
            config.first_machine_activate_retry_budget()
                > ployz_nats::operation_api_client::DEFAULT_OPERATION_API_REQUEST_TIMEOUT
                    + config.ops_watch_poll_interval(),
            "budget must allow at least one retry after a timed-out request"
        );
    }
}
