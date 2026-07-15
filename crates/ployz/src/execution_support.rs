//! Shared authenticated CLI connection and operation execution support.

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::api_client::{OperationApiClient, OperationApiClientError};
use crate::dispatcher::PloyzctlRuntimeConfig;
#[cfg(test)]
use crate::machine::operator_context::ClusterContext;
use crate::machine::operator_context::{ClusterContextError, load_cluster_context};
use crate::machine::runtime::host_runner::LocalHostRunnerInstallError;
use crate::machine::runtime::remote::RemoteMachineExecutionError;
use ployz_core::ids::OperationId;
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::operation::{
    EventSequence, OperationEventReplayCursor, OperationEventReplayRequest, OperationOutcome,
    ReplayedOperationEvent,
};
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{
    NatsClientAuth, NatsClientUrl, NatsClientUrlError, NatsConnectConfig, NatsConnectError,
    NatsTlsTrust, connect_authenticated,
};
use ployz_sdk_types::{InitFirstMachineActivateError, OpsStatusRequest};
use tokio::time::sleep as async_sleep;

mod client_ids;
mod output;

pub(crate) use client_ids::*;
pub use output::{CommandExit, PloyzctlExecutionOutput};

pub const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
pub const PLOYZ_NATS_CA_FILE_ENV: &str = "PLOYZ_NATS_CA_FILE";
pub const PLOYZ_NATS_NKEY_SEED_FILE_ENV: &str = "PLOYZ_NATS_NKEY_SEED_FILE";
pub const PLOYZ_JOIN_NKEY_SEED_FILE_ENV: &str = "PLOYZ_JOIN_NKEY_SEED_FILE";
pub const DEFAULT_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_OPS_WATCH_TIMEOUT: Duration = Duration::from_secs(600);
pub const DEFAULT_OPS_WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) fn with_cluster_context_from_disk(
    config: PloyzctlRuntimeConfig,
) -> Result<PloyzctlRuntimeConfig, PloyzctlExecutionError> {
    if config.nats_url.is_some()
        && config.nats_ca_file.is_some()
        && config.nats_seed_file.is_some()
        && config.join_seed_file.is_some()
    {
        return Ok(config);
    }
    let Some(path) = config
        .cluster_context_path
        .clone()
        .or_else(crate::machine::operator_context::default_cluster_context_path)
    else {
        return Ok(config);
    };
    let context = load_cluster_context(&path)
        .map_err(|source| PloyzctlExecutionError::ClusterContext { source })?;
    Ok(config.with_cluster_context(context))
}

/// Connects, issues one operation API request, and renders the success value
/// to stdout; API failures arrive as one rendered execution error.
pub(crate) async fn render_api_call<T, E>(
    config: &PloyzctlRuntimeConfig,
    call: impl AsyncFnOnce(OperationApiClient) -> Result<T, OperationApiClientError<E>>,
    render: impl FnOnce(T) -> String,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError>
where
    E: fmt::Display,
{
    let api = operation_api_client(config).await?;
    let value = call(api).await.map_err(api_error)?;
    Ok(PloyzctlExecutionOutput::stdout(render(value)))
}

/// Operation API failures are terminal for the CLI, so carry the rendered
/// message instead of one error variant per endpoint.
pub(crate) fn api_error<E>(source: OperationApiClientError<E>) -> PloyzctlExecutionError
where
    E: fmt::Display,
{
    let message = match source {
        OperationApiClientError::Domain { error, .. } => error.to_string(),
        error @ (OperationApiClientError::EncodeRequest { .. }
        | OperationApiClientError::Request { .. }
        | OperationApiClientError::Service { .. }
        | OperationApiClientError::ServiceProtocol { .. }
        | OperationApiClientError::DecodeResponse { .. }) => error.to_string(),
    };
    PloyzctlExecutionError::OperationApi { message }
}

pub(crate) async fn watch_operation_until_terminal(
    api: &OperationApiClient,
    request: OperationEventReplayRequest,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<(Vec<ReplayedOperationEvent>, Option<OperationOutcome>), PloyzctlExecutionError> {
    watch_operation_until_terminal_with(api, request, timeout, poll_interval, |_| Ok(())).await
}

pub(crate) async fn watch_operation_until_terminal_with(
    api: &OperationApiClient,
    mut request: OperationEventReplayRequest,
    timeout: Duration,
    poll_interval: Duration,
    mut observe_page: impl FnMut(&[ReplayedOperationEvent]) -> Result<(), PloyzctlExecutionError>,
) -> Result<(Vec<ReplayedOperationEvent>, Option<OperationOutcome>), PloyzctlExecutionError> {
    let operation_id = request.operation_id.clone();
    let started_at = Instant::now();
    let mut events = Vec::new();

    loop {
        let page = api.ops_watch(&request).await.map_err(api_error)?;

        let cursor = page.cursor;
        observe_page(&page.events)?;
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
            OperationEventReplayCursor::Terminal => {
                let outcome = operation_terminal_outcome(api, &operation_id).await?;
                return Ok((events, outcome));
            }
            OperationEventReplayCursor::CaughtUp => {}
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

/// The terminal outcome recorded for an operation the event stream reports as
/// terminal, or `None` if its status snapshot has not caught up yet.
async fn operation_terminal_outcome(
    api: &OperationApiClient,
    operation_id: &OperationId,
) -> Result<Option<OperationOutcome>, PloyzctlExecutionError> {
    let snapshot = api
        .ops_status(&OpsStatusRequest {
            operation_id: operation_id.clone(),
        })
        .await
        .map_err(api_error)?;
    Ok(snapshot.status.terminal_outcome())
}

fn next_event_sequence(sequence: EventSequence) -> Option<EventSequence> {
    let next = sequence.get().checked_add(1)?;
    EventSequence::try_new(next).ok()
}

pub(crate) fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) async fn operation_api_client(
    config: &PloyzctlRuntimeConfig,
) -> Result<OperationApiClient, PloyzctlExecutionError> {
    let config = with_cluster_context_from_disk(config.clone())?;
    let connect = nats_connect_config(&config)?;
    operation_api_client_with_connect(&config, connect).await
}

pub(crate) async fn operation_api_client_with_connect(
    config: &PloyzctlRuntimeConfig,
    connect: NatsConnectConfig,
) -> Result<OperationApiClient, PloyzctlExecutionError> {
    connect_authenticated(&connect, config.nats_connect_timeout())
        .await
        .map(OperationApiClient::new)
        .map_err(PloyzctlExecutionError::NatsConnect)
}

pub(crate) fn nats_connect_config(
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
        principal: NatsPrincipal::Operator,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PloyzctlExecutionError {
    #[error("telemetry preferences are handled locally by the ployz binary")]
    LocalCommand,
    #[error(
        "ployz {command} is reserved for Ployz Cloud and no Cloud connection is configured; configure a Ployz Cloud connection before using Cloud verbs"
    )]
    CloudUnconfigured { command: &'static str },
    #[error(
        "no cluster context: run `ployz init USER@HOST` to create one, pass --nats, or set {PLOYZ_NATS_URL_ENV}"
    )]
    MissingNatsUrl,
    #[error("--nats or {PLOYZ_NATS_URL_ENV} is invalid: {0:?}")]
    InvalidNatsUrl(NatsClientUrlError),
    #[error("{PLOYZ_NATS_CA_FILE_ENV} is required")]
    MissingNatsCaFile,
    #[error("{PLOYZ_NATS_NKEY_SEED_FILE_ENV} is required")]
    MissingNatsSeedFile,
    #[error(
        "{PLOYZ_NATS_NKEY_SEED_FILE_ENV} file {} is unreadable: {message}",
        path.display()
    )]
    ReadNatsSeedFile { path: PathBuf, message: String },
    #[error(
        "{PLOYZ_NATS_NKEY_SEED_FILE_ENV} file {} does not contain an SU-prefixed user seed",
        path.display()
    )]
    InvalidNatsSeedFile { path: PathBuf },
    #[error(
        "machine add requires a join seed from the cluster context or {PLOYZ_JOIN_NKEY_SEED_FILE_ENV}; run `ployz init USER@HOST` with the current CLI to refresh the context"
    )]
    MissingJoinSeedFile,
    #[error(
        "join seed file {} is unreadable (set {PLOYZ_JOIN_NKEY_SEED_FILE_ENV}): {message}",
        path.display()
    )]
    ReadJoinSeedFile { path: PathBuf, message: String },
    #[error("join seed file {} does not contain an SU-prefixed user seed", path.display())]
    InvalidJoinSeedFile { path: PathBuf },
    #[error("{0}")]
    NatsConnect(NatsConnectError),
    #[error("{source}")]
    HostRunnerFirstMachineInstall {
        source: Box<LocalHostRunnerInstallError>,
    },
    #[error("{source}")]
    ClusterContext { source: ClusterContextError },
    #[error("{source}")]
    RemoteMachine {
        source: Box<RemoteMachineExecutionError>,
    },
    #[error("{message}")]
    OperationApi { message: String },
    #[error("image push failed: {source}")]
    ImagePush {
        source: crate::deploy::image_push::ImagePushError,
    },
    #[error("registry credential lookup failed: {source}")]
    RegistryAuth {
        source: crate::deploy::registry_auth::RegistryAuthError,
    },
    #[error("namespace rm {} was not confirmed", namespace_id.as_str())]
    NamespaceRemoveNotConfirmed {
        namespace_id: ployz_core::ids::NamespaceId,
    },
    #[error("failed to read namespace rm confirmation: {message}")]
    ReadNamespaceRemoveConfirmation { message: String },
    #[error(
        "volume rm {}/{} was not confirmed",
        namespace_id.as_str(),
        volume_name.as_str()
    )]
    VolumeRemoveNotConfirmed {
        namespace_id: ployz_core::ids::NamespaceId,
        volume_name: ployz_core::deploy::VolumeName,
    },
    #[error("failed to read volume rm confirmation: {message}")]
    ReadVolumeRemoveConfirmation { message: String },
    #[error("first machine activation failed: {source}")]
    FirstMachineActivateApi {
        source: OperationApiClientError<InitFirstMachineActivateError>,
    },
    #[error(
        "operation {} did not reach a terminal state within {}s",
        operation_id.as_str(),
        timeout.as_secs()
    )]
    OpsWatchTimedOut {
        operation_id: OperationId,
        timeout: Duration,
    },
    #[error("failed to write deploy progress: {message}")]
    WriteDeployProgress { message: String },
    #[error("could not generate client operation ids: {message}")]
    GenerateClientOperationIds { message: String },
    #[error("{message}")]
    DeployHistory { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_renders_domain_errors_as_clean_evidence() {
        let error = api_error(OperationApiClientError::Domain {
            endpoint: ployz_nats::subjects::OperationApiEndpoint::DeploySubmit,
            error: ployz_sdk_types::DeploySubmitError::Unavailable {
                operation_id: ployz_core::ids::OperationId::try_new("op_123")
                    .expect("valid operation id"),
                message: "operation status CAS conflict: contended".to_owned(),
            },
        });
        let PloyzctlExecutionError::OperationApi { message } = error else {
            panic!("api_error maps to the operation API execution error");
        };
        assert_eq!(
            message,
            "deploy submit op_123 unavailable: operation status CAS conflict: contended"
        );
    }

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
            host_runner_install_timeout: None,
            ops_watch_timeout: None,
            ops_watch_poll_interval: None,
            ssh_program: None,
            ssh_install_timeout: None,
            cluster_context_path: None,
            deploy_history_root: None,
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
}
