//! Runtime execution for remote machine bootstrap commands.
//!
//! This module owns the SSH-driven `machine init` and remote `machine add`
//! flows. `runtime.rs` stays responsible for command dispatch and shared
//! NATS plumbing.

use std::fmt;
use std::path::PathBuf;

use crate::bootstrap_command::{FounderBootstrapCommand, MACHINE_NATS_PORT};
use crate::client_ids::generate_client_machine_add_ids;
use crate::commands::init::FirstMachineActivateCommand;
use crate::commands::machine::{
    MachineAddOutput, MachineAddRemoteCommand, MachineAddRemoteDetachedOutput,
    MachineAddRemoteOutput, MachineIdentity, MachineIdentityError, MachineInitCommand,
    MachineInitOutput,
};
use crate::config::{
    ClusterContextError, ClusterContextMaterial, default_cluster_context_path,
    publish_cluster_context, save_cluster_context, save_cluster_context_machine_ssh,
};
use crate::runtime::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, PloyzctlRuntimeConfig, activate_first_machine,
    api_error, operation_api_client, read_join_seed, watch_operation_until_terminal,
};
use crate::ssh::{DEFAULT_SSH_COMMAND_TIMEOUT, SshClient, SshCommandError, SshPhase, SshTarget};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::install::MachineJoinRuntimeNatsUrl;
use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
use ployz_core::ops::MachineAddOperationState;
use ployz_core::ops::{
    EventSequence, MAX_OPERATION_EVENT_REPLAY_LIMIT, OperationEventReplayLimit,
    OperationEventReplayRequest, OperationStatus,
};
use ployz_nats::connect::NatsClientUrl;
use ployz_sdk_types::{MachineAddRequest, OpsStatusRequest};
use serde::Deserialize;

const FIRST_MACHINE_BOOTSTRAP_RESULT_BEGIN: &str = "ployz-first-machine-bootstrap-result begin";
const FIRST_MACHINE_BOOTSTRAP_RESULT_END: &str = "ployz-first-machine-bootstrap-result end";

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
    machine_id: ployz_core::ids::MachineId,
    target: SshTarget,
) -> Result<(), ClusterContextError> {
    let Some(path) = optional_cluster_context_path(config) else {
        return Ok(());
    };
    save_cluster_context_machine_ssh(&path, machine_id, target)?;
    Ok(())
}

fn machine_ssh_context_warning(
    machine_id: &MachineId,
    target: &SshTarget,
    source: &ClusterContextError,
) -> String {
    format!(
        "warning: remote machine operation completed, but local SSH mapping for {} at {} was not saved: {source}\n",
        machine_id.as_str(),
        target.destination()
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FirstMachineBootstrapResult {
    machine_id: MachineId,
    nats_url: NatsClientUrl,
    ca_pem: String,
    operator_seed: String,
    join_seed: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FirstMachineBootstrapResultFile {
    machine_id: String,
    nats_url: String,
    ca_pem: String,
    operator_seed: String,
    join_seed: String,
}

fn parse_first_machine_bootstrap_result(
    stdout: &str,
    stdout_truncated: bool,
) -> Result<FirstMachineBootstrapResult, PloyzctlExecutionError> {
    if stdout_truncated {
        return Err(remote_machine_error(
            RemoteMachineExecutionError::FirstMachineBootstrapOutputTruncated,
        ));
    }

    let mut lines = stdout.lines();
    while let Some(line) = lines.next() {
        if line.trim() != FIRST_MACHINE_BOOTSTRAP_RESULT_BEGIN {
            continue;
        }

        let Some(json_line) = lines.next() else {
            return Err(invalid_first_machine_bootstrap_result(
                "bootstrap result marker was not followed by JSON",
            ));
        };
        let Some(end_line) = lines.next() else {
            return Err(invalid_first_machine_bootstrap_result(
                "bootstrap result JSON was not followed by an end marker",
            ));
        };
        if end_line.trim() != FIRST_MACHINE_BOOTSTRAP_RESULT_END {
            return Err(invalid_first_machine_bootstrap_result(
                "bootstrap result end marker was missing",
            ));
        }

        let file: FirstMachineBootstrapResultFile =
            serde_json::from_str(json_line).map_err(|error| {
                remote_machine_error(
                    RemoteMachineExecutionError::InvalidFirstMachineBootstrapResult {
                        message: error.to_string(),
                    },
                )
            })?;
        return first_machine_bootstrap_result_from_file(file);
    }

    Err(remote_machine_error(
        RemoteMachineExecutionError::MissingFirstMachineBootstrapResult,
    ))
}

fn first_machine_bootstrap_result_from_file(
    file: FirstMachineBootstrapResultFile,
) -> Result<FirstMachineBootstrapResult, PloyzctlExecutionError> {
    let machine_id = MachineId::try_new(file.machine_id).map_err(|error| {
        remote_machine_error(
            RemoteMachineExecutionError::InvalidFirstMachineBootstrapResult {
                message: format!("invalid machine_id: {error}"),
            },
        )
    })?;
    let nats_url = NatsClientUrl::try_new(file.nats_url).map_err(|error| {
        remote_machine_error(
            RemoteMachineExecutionError::InvalidFirstMachineBootstrapResult {
                message: format!("invalid nats_url: {error:?}"),
            },
        )
    })?;
    NatsCaCertificatePem::try_new(file.ca_pem.as_str()).map_err(|error| {
        remote_machine_error(
            RemoteMachineExecutionError::InvalidFirstMachineBootstrapResult {
                message: format!("invalid ca_pem: {error}"),
            },
        )
    })?;
    Ok(FirstMachineBootstrapResult {
        machine_id,
        nats_url,
        ca_pem: file.ca_pem,
        operator_seed: normalize_bootstrap_seed("operator_seed", &file.operator_seed)?,
        join_seed: normalize_bootstrap_seed("join_seed", &file.join_seed)?,
    })
}

fn invalid_first_machine_bootstrap_result(message: &str) -> PloyzctlExecutionError {
    remote_machine_error(
        RemoteMachineExecutionError::InvalidFirstMachineBootstrapResult {
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
    let install_command = FounderBootstrapCommand {
        installer: command.installer(),
        release: command.release.clone(),
        release_manifest_url: command.release_manifest_url.clone(),
        machine_id: identity.machine_id.clone(),
        roles: command.roles,
        bootstrap_url: command.bootstrap_url.clone(),
        cluster_name: command.cluster_name.clone(),
        runtime_nats_url: runtime_nats_url_for_target(&target)?,
    }
    .render();
    let install_output = installer
        .run(&target, SshPhase::RunInstaller, &install_command)
        .map_err(ssh_error)?;
    let bootstrap_result = parse_first_machine_bootstrap_result(
        &install_output.stdout.text,
        install_output.stdout.truncated,
    )?;
    if bootstrap_result.machine_id != identity.machine_id {
        return Err(remote_machine_error(
            RemoteMachineExecutionError::FirstMachineBootstrapIdentityMismatch {
                expected: identity.machine_id,
                actual: bootstrap_result.machine_id,
            },
        ));
    }

    let context_path = cluster_context_path(config)?;
    let context = publish_cluster_context(
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

    let activate_config = config.clone().with_cluster_context(Some(context.clone()));
    let activation = activate_first_machine(
        &FirstMachineActivateCommand::new(identity.machine_id.clone(), command.roles),
        &activate_config,
    )
    .await?;
    let context = context.with_machine_ssh(identity.machine_id.clone(), target.clone());
    let mut output = PloyzctlExecutionOutput::stdout(
        MachineInitOutput {
            operation_id: activation.operation_id,
            machine_id: activation.machine_id,
            context_path: context_path.clone(),
        }
        .render(),
    );
    // Surface keeper's install stderr: when PLOYZ_RECOVERY_SECRET is unset, keeper
    // generates a one-time cluster recovery secret and prints it there, and the
    // operator needs it to core-promote later (ADR 0031). The remote path would
    // otherwise swallow it.
    let install_stderr = install_output.stderr.text.trim();
    if !install_stderr.is_empty() {
        output.stderr = format!("{install_stderr}\n");
    }
    if let Err(source) = save_cluster_context(&context_path, &context) {
        output.stderr.push_str(&machine_ssh_context_warning(
            &identity.machine_id,
            &target,
            &source,
        ));
    }

    Ok(output)
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

    let generated_ids = generate_client_machine_add_ids(&identity.machine_id)
        .map_err(client_generated_ids_error)?;
    let operation_id = generated_ids.operation_id;
    let accepted = api
        .machine_add(&MachineAddRequest {
            operation_id: operation_id.clone(),
            idempotency_key: generated_ids.idempotency_key,
            machine_id: identity.machine_id.clone(),
            name: identity.name.clone(),
            roles: command.roles,
        })
        .await
        .map_err(api_error)?;
    let output = MachineAddOutput::from_accepted(accepted, join_seed);

    let install_command = output.install_command(&command.installer());
    if let Err(source) = installer.run(&target, SshPhase::RunInstaller, &install_command) {
        return Err(remote_machine_error(
            RemoteMachineExecutionError::RemoteJoinInstall {
                operation_id,
                source,
            },
        ));
    }

    if command.detach {
        return Ok(PloyzctlExecutionOutput::stdout(
            MachineAddRemoteDetachedOutput {
                operation_id,
                machine_id: identity.machine_id,
            }
            .render(),
        ));
    }

    watch_operation_until_terminal(
        &api,
        OperationEventReplayRequest {
            operation_id: operation_id.clone(),
            start_sequence: EventSequence::first(),
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
        MachineAddOperationState::Completed => {
            let mut output = PloyzctlExecutionOutput::stdout(
                MachineAddRemoteOutput {
                    operation_id,
                    machine_id: identity.machine_id.clone(),
                }
                .render(),
            );
            if let Err(source) = record_machine_ssh_if_context_exists(
                &config,
                identity.machine_id.clone(),
                target.clone(),
            ) {
                output.stderr = machine_ssh_context_warning(&identity.machine_id, &target, &source);
            }
            Ok(output)
        }
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
    FirstMachineBootstrapOutputTruncated,
    MissingFirstMachineBootstrapResult,
    InvalidFirstMachineBootstrapResult {
        message: String,
    },
    FirstMachineBootstrapIdentityMismatch {
        expected: MachineId,
        actual: MachineId,
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
            Self::FirstMachineBootstrapOutputTruncated => write!(
                formatter,
                "first-machine bootstrap output was truncated before the result could be collected"
            ),
            Self::MissingFirstMachineBootstrapResult => write!(
                formatter,
                "first-machine bootstrap output did not contain a structured result"
            ),
            Self::InvalidFirstMachineBootstrapResult { message } => {
                write!(
                    formatter,
                    "first-machine bootstrap result is invalid: {message}"
                )
            }
            Self::FirstMachineBootstrapIdentityMismatch { expected, actual } => write!(
                formatter,
                "first-machine bootstrap result reported machine {} but {} was requested",
                actual.as_str(),
                expected.as_str()
            ),
            Self::BootstrapSeedInvalid { field } => write!(
                formatter,
                "first-machine bootstrap result field {field} does not contain an SU-prefixed user seed"
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
