//! Runtime execution for remote machine bootstrap commands.
//!
//! This module owns the SSH-driven `machine init` and remote `machine add`
//! flows. `runtime.rs` stays responsible for command dispatch and shared
//! NATS plumbing.

use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;

use crate::bootstrap_command::{FounderBootstrapCommand, MACHINE_NATS_PORT};
use crate::client_ids::{ClientOperationKind, generate_client_operation_ids};
use crate::commands::init::FirstNodeActivateCommand;
use crate::commands::machine::{
    MachineAddOutput, MachineAddRemoteCommand, MachineAddRemoteOutput, MachineIdentity,
    MachineIdentityError, MachineInitCommand, MachineInitOutput,
};
use crate::config::{
    ClusterContextError, ClusterContextMaterial, default_cluster_context_path,
    publish_cluster_context, save_cluster_context, save_cluster_context_machine_ssh,
};
use crate::runtime::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, PloyzctlRuntimeConfig,
    activate_first_node_machine, api_error, operation_api_client, read_join_seed,
    watch_operation_until_terminal,
};
use crate::ssh::{DEFAULT_SSH_COMMAND_TIMEOUT, SshClient, SshCommandError, SshPhase, SshTarget};
use ployz_core::ids::{NodeId, OperationId};
use ployz_core::install::MachineJoinRuntimeNatsUrl;
use ployz_core::machine::MachineAddOperationState;
use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
use ployz_core::ops::{
    EventSequence, MAX_OPERATION_EVENT_REPLAY_LIMIT, OperationEventReplayLimit,
    OperationEventReplayRequest, OperationStatus,
};
use ployz_nats::connect::NatsClientUrl;
use ployz_sdk_types::{MachineAddRequest, OpsStatusRequest};
use serde::Deserialize;

const FIRST_NODE_BOOTSTRAP_RESULT_BEGIN: &str = "ployz-first-node-bootstrap-result begin";
const FIRST_NODE_BOOTSTRAP_RESULT_END: &str = "ployz-first-node-bootstrap-result end";

fn remote_machine_error(source: RemoteMachineExecutionError) -> PloyzctlExecutionError {
    PloyzctlExecutionError::RemoteMachine {
        source: Box::new(source),
    }
}

fn ssh_error(source: Box<SshCommandError>) -> PloyzctlExecutionError {
    remote_machine_error(RemoteMachineExecutionError::Ssh { source })
}

fn client_generated_ids_error(source: impl fmt::Display) -> PloyzctlExecutionError {
    remote_machine_error(RemoteMachineExecutionError::ClientGeneratedIds {
        message: source.to_string(),
    })
}

fn ssh_client(config: &PloyzctlRuntimeConfig, timeout: std::time::Duration) -> SshClient {
    match &config.ssh_program {
        Some(program) => SshClient::with_program(program.clone(), timeout),
        None => SshClient::system(timeout),
    }
}

/// Picks the quick-start machine identity: the `--name` override when given,
/// the remote hostname otherwise. Always resolved before any install.
fn derive_remote_identity(
    client: &SshClient,
    target: &SshTarget,
    identity_override: Option<MachineIdentity>,
) -> Result<MachineIdentity, PloyzctlExecutionError> {
    if let Some(identity) = identity_override {
        return Ok(identity);
    }
    let hostname = client.read_remote_hostname(target).map_err(ssh_error)?;
    MachineIdentity::from_remote_hostname(&hostname).map_err(|source| {
        remote_machine_error(RemoteMachineExecutionError::MachineIdentity { source })
    })
}

/// Where `machine init` records the local cluster context.
fn cluster_context_path(config: &PloyzctlRuntimeConfig) -> Result<PathBuf, PloyzctlExecutionError> {
    config
        .cluster_context_path
        .clone()
        .or_else(default_cluster_context_path)
        .ok_or_else(|| remote_machine_error(RemoteMachineExecutionError::NoConfigDirectory))
}

fn optional_cluster_context_path(config: &PloyzctlRuntimeConfig) -> Option<PathBuf> {
    config
        .cluster_context_path
        .clone()
        .or_else(default_cluster_context_path)
}

fn record_machine_ssh_if_context_exists(
    config: &PloyzctlRuntimeConfig,
    node_id: ployz_core::ids::NodeId,
    target: SshTarget,
) -> Result<(), PloyzctlExecutionError> {
    let Some(path) = optional_cluster_context_path(config) else {
        return Ok(());
    };
    save_cluster_context_machine_ssh(&path, node_id, target).map_err(|source| {
        remote_machine_error(RemoteMachineExecutionError::ClusterContext { source })
    })?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FirstNodeBootstrapResult {
    node_id: NodeId,
    nats_url: NatsClientUrl,
    ca_pem: String,
    operator_seed: String,
    join_seed: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FirstNodeBootstrapResultFile {
    node_id: String,
    nats_url: String,
    ca_pem: String,
    operator_seed: String,
    join_seed: String,
}

fn parse_first_node_bootstrap_result(
    stdout: &str,
    stdout_truncated: bool,
) -> Result<FirstNodeBootstrapResult, PloyzctlExecutionError> {
    if stdout_truncated {
        return Err(remote_machine_error(
            RemoteMachineExecutionError::FirstNodeBootstrapOutputTruncated,
        ));
    }

    let mut lines = stdout.lines();
    while let Some(line) = lines.next() {
        if line.trim() != FIRST_NODE_BOOTSTRAP_RESULT_BEGIN {
            continue;
        }

        let Some(json_line) = lines.next() else {
            return Err(invalid_first_node_bootstrap_result(
                "bootstrap result marker was not followed by JSON",
            ));
        };
        let Some(end_line) = lines.next() else {
            return Err(invalid_first_node_bootstrap_result(
                "bootstrap result JSON was not followed by an end marker",
            ));
        };
        if end_line.trim() != FIRST_NODE_BOOTSTRAP_RESULT_END {
            return Err(invalid_first_node_bootstrap_result(
                "bootstrap result end marker was missing",
            ));
        }

        let file: FirstNodeBootstrapResultFile =
            serde_json::from_str(json_line).map_err(|error| {
                remote_machine_error(
                    RemoteMachineExecutionError::InvalidFirstNodeBootstrapResult {
                        message: error.to_string(),
                    },
                )
            })?;
        return first_node_bootstrap_result_from_file(file);
    }

    Err(remote_machine_error(
        RemoteMachineExecutionError::MissingFirstNodeBootstrapResult,
    ))
}

fn first_node_bootstrap_result_from_file(
    file: FirstNodeBootstrapResultFile,
) -> Result<FirstNodeBootstrapResult, PloyzctlExecutionError> {
    let node_id = NodeId::try_new(file.node_id).map_err(|error| {
        remote_machine_error(
            RemoteMachineExecutionError::InvalidFirstNodeBootstrapResult {
                message: format!("invalid node_id: {error}"),
            },
        )
    })?;
    let nats_url = NatsClientUrl::try_new(file.nats_url).map_err(|error| {
        remote_machine_error(
            RemoteMachineExecutionError::InvalidFirstNodeBootstrapResult {
                message: format!("invalid nats_url: {error:?}"),
            },
        )
    })?;
    NatsCaCertificatePem::try_new(file.ca_pem.as_str()).map_err(|error| {
        remote_machine_error(
            RemoteMachineExecutionError::InvalidFirstNodeBootstrapResult {
                message: format!("invalid ca_pem: {error}"),
            },
        )
    })?;
    Ok(FirstNodeBootstrapResult {
        node_id,
        nats_url,
        ca_pem: file.ca_pem,
        operator_seed: normalize_bootstrap_seed("operator_seed", &file.operator_seed)?,
        join_seed: normalize_bootstrap_seed("join_seed", &file.join_seed)?,
    })
}

fn invalid_first_node_bootstrap_result(message: &str) -> PloyzctlExecutionError {
    remote_machine_error(
        RemoteMachineExecutionError::InvalidFirstNodeBootstrapResult {
            message: message.to_owned(),
        },
    )
}

/// Validates and normalizes a collected seed before it is published in the
/// local context material generation.
fn normalize_bootstrap_seed(field: &str, raw: &str) -> Result<String, PloyzctlExecutionError> {
    let trimmed = raw.trim();
    let Ok(_) = NatsUserSeed::try_new(trimmed) else {
        return Err(remote_machine_error(
            RemoteMachineExecutionError::BootstrapSeedInvalid {
                field: field.to_owned(),
            },
        ));
    };
    Ok(format!("{trimmed}\n"))
}

fn runtime_nats_url_for_target(
    target: &SshTarget,
) -> Result<MachineJoinRuntimeNatsUrl, PloyzctlExecutionError> {
    let url = format!("tls://{}:{MACHINE_NATS_PORT}", target.host());
    MachineJoinRuntimeNatsUrl::try_new(url).map_err(|error| {
        remote_machine_error(
            RemoteMachineExecutionError::InvalidBootstrapRuntimeNatsUrl {
                host: target.host().to_owned(),
                message: error.to_string(),
            },
        )
    })
}

/// `ployzctl machine init USER@HOST`: derive the remote identity, render
/// the founder bootstrap command, deliver it over SSH, parse the machine
/// local bootstrap result, record local context, and activate the first
/// machine through NATS.
pub(crate) async fn execute_machine_init(
    command: MachineInitCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let probe = ssh_client(config, DEFAULT_SSH_COMMAND_TIMEOUT);
    let installer = ssh_client(config, config.ssh_install_timeout());
    let target = command.target.clone();

    let identity = derive_remote_identity(&probe, &target, command.identity_override.clone())?;
    let node_public_ip: Option<IpAddr> = target.host().parse().ok();
    let install_command = FounderBootstrapCommand {
        installer: command.installer(),
        release: command.release.clone(),
        release_manifest_url: command.release_manifest_url.clone(),
        node_id: identity.node_id.clone(),
        roles: command.roles,
        bootstrap_url: command.bootstrap_url.clone(),
        cluster_name: command.cluster_name.clone(),
        runtime_nats_url: runtime_nats_url_for_target(&target)?,
        node_public_ip,
    }
    .render();
    let install_output = installer
        .run(&target, SshPhase::RunInstaller, &install_command)
        .map_err(ssh_error)?;
    let bootstrap_result = parse_first_node_bootstrap_result(
        &install_output.stdout.text,
        install_output.stdout.truncated,
    )?;
    if bootstrap_result.node_id != identity.node_id {
        return Err(remote_machine_error(
            RemoteMachineExecutionError::FirstNodeBootstrapIdentityMismatch {
                expected: identity.node_id,
                actual: bootstrap_result.node_id,
            },
        ));
    }

    let context_path = cluster_context_path(config)?;
    let mut context = publish_cluster_context(
        &context_path,
        bootstrap_result.nats_url,
        ClusterContextMaterial {
            ca_pem: &bootstrap_result.ca_pem,
            operator_seed: &bootstrap_result.operator_seed,
            join_seed: Some(&bootstrap_result.join_seed),
        },
    )
    .map_err(|source| {
        remote_machine_error(RemoteMachineExecutionError::ClusterContext { source })
    })?;
    context = context.with_machine_ssh(identity.node_id.clone(), target.clone());
    save_cluster_context(&context_path, &context).map_err(|source| {
        remote_machine_error(RemoteMachineExecutionError::ClusterContext { source })
    })?;

    let activate_config = config.clone().with_cluster_context(Some(context));
    let activation = activate_first_node_machine(
        &FirstNodeActivateCommand::new(identity.node_id.clone(), command.roles),
        &activate_config,
    )
    .await?;

    Ok(PloyzctlExecutionOutput::stdout(
        MachineInitOutput {
            operation_id: activation.operation_id,
            node_id: activation.node_id,
            context_path,
        }
        .render(),
    ))
}

/// `ployzctl machine add USER@HOST`: derive the identity from the remote
/// hostname, submit the existing machine-add operation, run the installer
/// join mode over SSH with the join bundle, and watch the operation to a
/// terminal state.
pub(crate) async fn execute_machine_add_remote(
    command: MachineAddRemoteCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let probe = ssh_client(config, DEFAULT_SSH_COMMAND_TIMEOUT);
    let installer = ssh_client(config, config.ssh_install_timeout());
    let target = command.target.clone();

    let identity = derive_remote_identity(&probe, &target, command.identity_override.clone())?;
    let config = config.clone().with_cluster_context_from_disk()?;
    let join_seed = read_join_seed(&config)?;
    let api = operation_api_client(&config).await?;

    let generated_ids = generate_client_operation_ids(ClientOperationKind::MachineAdd {
        node_id: &identity.node_id,
    })
    .map_err(client_generated_ids_error)?;
    let operation_id = generated_ids.operation_id;
    let accepted = api
        .machine_add(&MachineAddRequest {
            operation_id: operation_id.clone(),
            idempotency_key: generated_ids.idempotency_key,
            node_id: identity.node_id.clone(),
            name: identity.name.clone(),
            roles: command.roles,
        })
        .await
        .map_err(api_error)?;
    let output = MachineAddOutput::from_accepted(accepted, join_seed);
    record_machine_ssh_if_context_exists(&config, identity.node_id.clone(), target.clone())?;

    let node_public_ip: Option<IpAddr> = target.host().parse().ok();
    let install_command = output.install_command(&command.installer(), node_public_ip);
    if let Err(source) = installer.run(&target, SshPhase::RunInstaller, &install_command) {
        return Err(remote_machine_error(
            RemoteMachineExecutionError::RemoteJoinInstall {
                operation_id,
                source,
            },
        ));
    }

    watch_operation_until_terminal(
        &api,
        OperationEventReplayRequest {
            operation_id: operation_id.clone(),
            start_sequence: EventSequence::try_new(1).expect("one is a valid event sequence"),
            limit: OperationEventReplayLimit::try_new(MAX_OPERATION_EVENT_REPLAY_LIMIT)
                .expect("max replay limit is valid"),
        },
        config.ops_watch_timeout(),
        config.ops_watch_poll_interval(),
    )
    .await?;
    let snapshot = api
        .ops_status(&OpsStatusRequest {
            operation_id: operation_id.clone(),
        })
        .await
        .map_err(api_error)?;
    let OperationStatus::MachineAdd { state, .. } = &snapshot.status else {
        return Err(remote_machine_error(
            RemoteMachineExecutionError::MachineAddNotCompleted {
                operation_id,
                state: format!("{:?}", snapshot.status),
            },
        ));
    };
    match state {
        MachineAddOperationState::Completed => Ok(PloyzctlExecutionOutput::stdout(
            MachineAddRemoteOutput {
                operation_id,
                node_id: identity.node_id,
            }
            .render(),
        )),
        MachineAddOperationState::Pending { .. }
        | MachineAddOperationState::Joining { .. }
        | MachineAddOperationState::Failed { .. }
        | MachineAddOperationState::Cancelled { .. } => Err(remote_machine_error(
            RemoteMachineExecutionError::MachineAddNotCompleted {
                operation_id,
                state: format!("{state:?}"),
            },
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteMachineExecutionError {
    /// A phase-labeled remote bootstrap SSH failure.
    Ssh {
        source: Box<SshCommandError>,
    },
    MachineIdentity {
        source: MachineIdentityError,
    },
    FirstNodeBootstrapOutputTruncated,
    MissingFirstNodeBootstrapResult,
    InvalidFirstNodeBootstrapResult {
        message: String,
    },
    FirstNodeBootstrapIdentityMismatch {
        expected: NodeId,
        actual: NodeId,
    },
    BootstrapSeedInvalid {
        field: String,
    },
    ClusterContext {
        source: ClusterContextError,
    },
    NoConfigDirectory,
    InvalidBootstrapRuntimeNatsUrl {
        host: String,
        message: String,
    },
    ClientGeneratedIds {
        message: String,
    },
    /// The remote installer failed after the machine-add operation was
    /// created: the operation id stays visible as evidence next to the SSH
    /// phase and remote output.
    RemoteJoinInstall {
        operation_id: OperationId,
        source: Box<SshCommandError>,
    },
    MachineAddNotCompleted {
        operation_id: OperationId,
        state: String,
    },
}

impl fmt::Display for RemoteMachineExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ssh { source } => write!(formatter, "{source}"),
            Self::MachineIdentity { source } => write!(formatter, "{source}"),
            Self::FirstNodeBootstrapOutputTruncated => write!(
                formatter,
                "first-node bootstrap output was truncated before the result could be collected"
            ),
            Self::MissingFirstNodeBootstrapResult => write!(
                formatter,
                "first-node bootstrap output did not contain a structured result"
            ),
            Self::InvalidFirstNodeBootstrapResult { message } => {
                write!(
                    formatter,
                    "first-node bootstrap result is invalid: {message}"
                )
            }
            Self::FirstNodeBootstrapIdentityMismatch { expected, actual } => write!(
                formatter,
                "first-node bootstrap result reported machine {} but {} was requested",
                actual.as_str(),
                expected.as_str()
            ),
            Self::BootstrapSeedInvalid { field } => write!(
                formatter,
                "first-node bootstrap result field {field} does not contain an SU-prefixed user seed"
            ),
            Self::ClusterContext { source } => write!(formatter, "{source}"),
            Self::NoConfigDirectory => write!(
                formatter,
                "cannot determine the config directory for the cluster context (set HOME or XDG_CONFIG_HOME)"
            ),
            Self::InvalidBootstrapRuntimeNatsUrl { host, message } => write!(
                formatter,
                "cannot build founder bootstrap runtime NATS URL from host {host:?}: {message}"
            ),
            Self::ClientGeneratedIds { message } => {
                write!(
                    formatter,
                    "could not generate client operation ids: {message}"
                )
            }
            Self::RemoteJoinInstall {
                operation_id,
                source,
            } => write!(
                formatter,
                "machine add operation {} remote join install failed: {source}",
                operation_id.as_str()
            ),
            Self::MachineAddNotCompleted {
                operation_id,
                state,
            } => write!(
                formatter,
                "machine add operation {} did not complete: {state}",
                operation_id.as_str()
            ),
        }
    }
}

impl std::error::Error for RemoteMachineExecutionError {}
