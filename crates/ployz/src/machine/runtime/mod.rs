//! Execution of Machine installation and lifecycle commands.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use crate::api_client::{NatsServiceRequestFailure, OperationApiClientError};
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::{
    CommandExit, ExecutionSupportError, PLOYZ_JOIN_NKEY_SEED_FILE_ENV, PloyzctlExecutionOutput,
    api_error, nats_connect_config, operation_api_client, operation_api_client_with_connect,
    render_api_call, with_cluster_context_from_disk,
};
use crate::machine::command::{
    AcceptedOperationOutput, MachineAddCommand, MachineAddOutput, MachineBuildCachePruneCommand,
    MachineInspectCommand, MachineInspectOutput, MachineLifecycleCommand, MachineListCommand,
    MachineListOutput, MachineStoragePrepareCommand, MachineUpdateCommand,
};
use crate::machine::founder::join_template::MachineJoinTemplateCommand;
use crate::machine::founder::{
    FirstMachineActivateCommand, FirstMachineInitCommand, FirstMachineInitMode,
};
use crate::operation::runtime::watch_accepted;
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::operation::MachineAddOperationStateName;
use ployz_nats::connect::NatsConnectError;
use ployz_sdk_types::{InitFirstMachineActivateError, MachineJoinRedeemError};
use tokio::time::sleep as async_sleep;

pub mod host_runner;
pub mod remote;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineExecutionError {
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
    #[error("{source}")]
    HostRunnerFirstMachineInstall {
        source: Box<host_runner::LocalHostRunnerInstallError>,
    },
    #[error("{source}")]
    RemoteMachine {
        source: Box<remote::RemoteMachineExecutionError>,
    },
    #[error("first machine activation failed: {source}")]
    FirstMachineActivateApi {
        source: OperationApiClientError<InitFirstMachineActivateError>,
    },
}

impl From<MachineExecutionError> for PloyzctlExecutionError {
    fn from(error: MachineExecutionError) -> Self {
        Self::Machine(error)
    }
}

pub(super) async fn activate_first_machine(
    command: &FirstMachineActivateCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<crate::machine::founder::FirstMachineActivationOutput, PloyzctlExecutionError> {
    let deadline = Instant::now() + config.first_machine_activate_retry_budget();
    loop {
        match activate_first_machine_once(command, config).await {
            Ok(activation) => return Ok(activation),
            Err(error) if activation_retryable(&error) && Instant::now() < deadline => {
                async_sleep(config.ops_watch_poll_interval()).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn activate_first_machine_once(
    command: &FirstMachineActivateCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<crate::machine::founder::FirstMachineActivationOutput, PloyzctlExecutionError> {
    let api = operation_api_client(config).await?;
    let activated = api
        .init_first_machine_activate(&command.clone().into_request())
        .await
        .map_err(|source| MachineExecutionError::FirstMachineActivateApi { source })?;

    Ok(crate::machine::founder::FirstMachineActivationOutput {
        operation_id: activated.operation_id,
        machine_id: activated.machine_id,
    })
}

fn activation_retryable(error: &PloyzctlExecutionError) -> bool {
    if let PloyzctlExecutionError::Support(ExecutionSupportError::NatsConnect(
        NatsConnectError::Connect { .. } | NatsConnectError::Timeout { .. },
    )) = error
    {
        return true;
    }
    let PloyzctlExecutionError::Machine(MachineExecutionError::FirstMachineActivateApi { source }) =
        error
    else {
        return false;
    };
    matches!(
        source,
        OperationApiClientError::Request {
            failure: NatsServiceRequestFailure::NoResponders | NatsServiceRequestFailure::TimedOut,
            ..
        } | OperationApiClientError::Domain {
            error: InitFirstMachineActivateError::JoinRedeem {
                failure: MachineJoinRedeemError::UnknownJoinToken
                    | MachineJoinRedeemError::OperationNotPending {
                        current: MachineAddOperationStateName::Completed,
                        ..
                    },
            },
            ..
        }
    )
}

pub(super) fn read_join_seed(
    config: &PloyzctlRuntimeConfig,
) -> Result<NatsUserSeed, PloyzctlExecutionError> {
    let Some(path) = config.join_seed_file.clone() else {
        return Err(MachineExecutionError::MissingJoinSeedFile.into());
    };
    let raw =
        fs::read_to_string(&path).map_err(|error| MachineExecutionError::ReadJoinSeedFile {
            path: path.clone(),
            message: error.to_string(),
        })?;
    NatsUserSeed::try_new(raw.trim())
        .map_err(|_| MachineExecutionError::InvalidJoinSeedFile { path }.into())
}

pub(crate) fn founder_init(
    command: FirstMachineInitCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    match &command.mode {
        FirstMachineInitMode::RunHostRunnerInstall {
            host_runner_install,
            host_runner_binary,
        } => {
            let output = host_runner::run_host_runner_first_machine_install(
                host_runner_binary,
                host_runner_install,
                config.host_runner_install_timeout(),
            )
            .map_err(|source| MachineExecutionError::HostRunnerFirstMachineInstall { source })?;
            Ok(PloyzctlExecutionOutput {
                stdout: output.stdout,
                stderr: output.stderr,
                exit: CommandExit::Success,
            })
        }
        FirstMachineInitMode::Summary { .. } | FirstMachineInitMode::EmitHostRunnerInstall(_) => {
            Ok(PloyzctlExecutionOutput::stdout(command.render()))
        }
    }
}

pub(crate) async fn activate_founder(
    command: FirstMachineActivateCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let activation = activate_first_machine(&command, config).await?;
    Ok(PloyzctlExecutionOutput::stdout(activation.render()))
}

pub(crate) fn render_join_template(command: MachineJoinTemplateCommand) -> PloyzctlExecutionOutput {
    PloyzctlExecutionOutput::stdout(command.render_json())
}

pub(crate) async fn add(
    command: MachineAddCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let config = with_cluster_context_from_disk(config.clone())?;
    let nats_connect = nats_connect_config(&config)?;
    // The cluster-static Join seed is deliberately low privilege. Reading it
    // before submission keeps a missing seed from creating an operation.
    let join_seed = read_join_seed(&config)?;
    let api = operation_api_client_with_connect(&config, nats_connect).await?;
    let accepted = api
        .machine_add(&command.into_request())
        .await
        .map_err(api_error)?;

    Ok(PloyzctlExecutionOutput::stdout(
        MachineAddOutput::from_accepted(accepted, join_seed).render(),
    ))
}

pub(crate) async fn update(
    command: MachineUpdateCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let detach = command.detach;
    let api = operation_api_client(config).await?;
    let accepted = api
        .machine_update(&command.into_request())
        .await
        .map_err(api_error)?;
    if detach {
        return Ok(PloyzctlExecutionOutput::stdout(
            AcceptedOperationOutput::from_accepted(accepted).render(),
        ));
    }
    watch_accepted(&api, accepted.operation_id, config).await
}

pub(crate) async fn storage_prepare(
    command: MachineStoragePrepareCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let detach = command.detach;
    let api = operation_api_client(config).await?;
    let accepted = api
        .machine_storage_prepare(&command.into_request())
        .await
        .map_err(api_error)?;
    if detach {
        return Ok(PloyzctlExecutionOutput::stdout(
            AcceptedOperationOutput::from_accepted(accepted).render(),
        ));
    }
    watch_accepted(&api, accepted.operation_id, config).await
}

pub(crate) async fn build_cache_prune(
    command: MachineBuildCachePruneCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let detach = command.detach;
    let api = operation_api_client(config).await?;
    let accepted = api
        .machine_build_cache_prune(&command.into_request())
        .await
        .map_err(api_error)?;
    if detach {
        return Ok(PloyzctlExecutionOutput::stdout(
            AcceptedOperationOutput::from_accepted(accepted).render(),
        ));
    }
    watch_accepted(&api, accepted.operation_id, config).await
}

pub(crate) async fn lifecycle(
    command: MachineLifecycleCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let detach = command.detach;
    let target = command.target;
    let api = operation_api_client(config).await?;
    let request = command.into_request();
    let accepted = match target {
        ployz_core::machine::MachineLifecycle::Draining => api.machine_drain(&request).await,
        ployz_core::machine::MachineLifecycle::Active => api.machine_resume(&request).await,
    }
    .map_err(api_error)?;
    if detach {
        return Ok(PloyzctlExecutionOutput::stdout(
            AcceptedOperationOutput::from_accepted(accepted).render(),
        ));
    }
    watch_accepted(&api, accepted.operation_id, config).await
}

pub(crate) async fn list(
    command: MachineListCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    Ok(render_api_call(
        config,
        async |api| api.machine_list(&command.into_request()).await,
        |result| MachineListOutput::from_result(result).render(),
    )
    .await?)
}

pub(crate) async fn inspect(
    command: MachineInspectCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    Ok(render_api_call(
        config,
        async |api| api.machine_inspect(&command.into_request()).await,
        |machine| MachineInspectOutput::new(machine).render(),
    )
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::OperationId;

    #[test]
    fn machine_add_join_seed_requires_context_or_explicit_seed_path() {
        assert_eq!(
            read_join_seed(&PloyzctlRuntimeConfig::default()),
            Err(MachineExecutionError::MissingJoinSeedFile.into())
        );
    }

    #[test]
    fn first_machine_activation_budget_allows_a_retry_after_a_dropped_reply() {
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

    #[test]
    fn first_machine_activation_retries_consumed_token_replay() {
        let error = MachineExecutionError::FirstMachineActivateApi {
            source: OperationApiClientError::Domain {
                endpoint: ployz_nats::subjects::OperationApiEndpoint::InitFirstMachineActivate,
                error: InitFirstMachineActivateError::JoinRedeem {
                    failure: MachineJoinRedeemError::UnknownJoinToken,
                },
            },
        }
        .into();

        assert!(activation_retryable(&error));
    }

    #[test]
    fn first_machine_activation_retries_completed_operation_replay() {
        let error = MachineExecutionError::FirstMachineActivateApi {
            source: OperationApiClientError::Domain {
                endpoint: ployz_nats::subjects::OperationApiEndpoint::InitFirstMachineActivate,
                error: InitFirstMachineActivateError::JoinRedeem {
                    failure: MachineJoinRedeemError::OperationNotPending {
                        operation_id: OperationId::try_new("op_init_core_1")
                            .expect("valid operation id"),
                        current: MachineAddOperationStateName::Completed,
                    },
                },
            },
        }
        .into();

        assert!(activation_retryable(&error));
    }
}
