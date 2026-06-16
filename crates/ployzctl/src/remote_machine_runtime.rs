//! Runtime execution for remote machine bootstrap commands.
//!
//! This module owns the SSH-driven `machine init` and remote `machine add`
//! flows. `runtime.rs` stays responsible for command dispatch and shared
//! NATS plumbing.

use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;

use crate::client_ids::{ClientOperationKind, generate_client_operation_ids};
use crate::commands::init::FirstNodeActivateCommand;
use crate::commands::machine::{
    MachineAddOutput, MachineAddRemoteCommand, MachineAddRemoteOutput, MachineIdentity,
    MachineIdentityError, MachineInitCommand, MachineInitOutput,
};
use crate::config::{
    ClusterContextError, ClusterContextMaterial, default_cluster_context_path,
    publish_cluster_context,
};
use crate::remote_bootstrap::{self, ReleaseArtifactManifest, RemoteBootstrapError};
use crate::runtime::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, PloyzctlRuntimeConfig,
    activate_first_node_machine, api_error, operation_api_client, read_join_seed,
    watch_operation_until_terminal,
};
use crate::ssh::{DEFAULT_SSH_COMMAND_TIMEOUT, SshClient, SshCommandError, SshPhase, SshTarget};
use ployz_core::ids::OperationId;
use ployz_core::machine::MachineAddOperationState;
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::ops::{
    EventSequence, MAX_OPERATION_EVENT_REPLAY_LIMIT, OperationEventReplayLimit,
    OperationEventReplayRequest, OperationStatus,
};
use ployz_nats::connect::{NatsClientUrl, NatsClientUrlError};
use ployz_sdk_types::{MachineAddRequest, OpsStatusRequest};

fn remote_machine_error(source: RemoteMachineExecutionError) -> PloyzctlExecutionError {
    PloyzctlExecutionError::RemoteMachine {
        source: Box::new(source),
    }
}

fn ssh_error(source: Box<SshCommandError>) -> PloyzctlExecutionError {
    remote_machine_error(RemoteMachineExecutionError::Ssh { source })
}

fn remote_error(source: RemoteBootstrapError) -> PloyzctlExecutionError {
    remote_machine_error(RemoteMachineExecutionError::RemoteBootstrap { source })
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

/// Resolves the release manifest for the remote machine: an explicit
/// `--release-manifest` URL, or the default Linux manifest for the remote
/// architecture. The manifest is fetched on the remote machine.
fn resolve_release_manifest(
    client: &SshClient,
    target: &SshTarget,
    command: &MachineInitCommand,
) -> Result<(String, ReleaseArtifactManifest), PloyzctlExecutionError> {
    let manifest_url = match &command.release_manifest_url {
        Some(url) => url.clone(),
        None => {
            let arch = client
                .run(
                    target,
                    SshPhase::ResolveRelease,
                    remote_bootstrap::READ_ARCH_COMMAND,
                )
                .map_err(ssh_error)?;
            let slug =
                remote_bootstrap::release_arch_slug(&arch.stdout.text).map_err(remote_error)?;
            remote_bootstrap::default_release_manifest_url(&command.version, slug)
        }
    };
    let fetched = client
        .run(
            target,
            SshPhase::ResolveRelease,
            &remote_bootstrap::fetch_manifest_command(&manifest_url),
        )
        .map_err(ssh_error)?;
    let manifest = remote_bootstrap::parse_release_manifest(&fetched.stdout.text, &manifest_url)
        .map_err(remote_error)?;
    Ok((manifest_url, manifest))
}

/// Reads one small remote file completely (CA, seeds); truncation is an
/// explicit error rather than silent corruption.
fn read_remote_text(
    client: &SshClient,
    target: &SshTarget,
    path: &str,
) -> Result<String, PloyzctlExecutionError> {
    let output = client
        .run(
            target,
            SshPhase::CollectOperatorMaterial,
            &remote_bootstrap::read_remote_file_command(path),
        )
        .map_err(ssh_error)?;
    if output.stdout.truncated {
        return Err(remote_machine_error(
            RemoteMachineExecutionError::CollectedRemoteFileTruncated {
                path: path.to_owned(),
            },
        ));
    }
    Ok(output.stdout.text)
}

/// Renders the machine-join template and writes it on the remote machine.
fn write_join_template_over_ssh(
    client: &SshClient,
    target: &SshTarget,
    command: &MachineInitCommand,
    ca_pem: &str,
    manifest: &ReleaseArtifactManifest,
) -> Result<(), PloyzctlExecutionError> {
    let runtime_nats_url =
        remote_bootstrap::runtime_nats_url(target.host()).map_err(remote_error)?;
    let template = remote_bootstrap::build_machine_join_template(
        command.cluster_name.clone(),
        runtime_nats_url,
        ca_pem,
        manifest,
    )
    .map_err(remote_error)?;
    let json = serde_json::to_string_pretty(&template).expect("machine join template serializes");
    client
        .run(
            target,
            SshPhase::PrepareMachine,
            &remote_bootstrap::write_remote_file_command(
                remote_bootstrap::REMOTE_JOIN_TEMPLATE_PATH,
                &json,
            ),
        )
        .map_err(ssh_error)?;
    Ok(())
}

/// Where `machine init` records the local cluster context.
fn cluster_context_path(config: &PloyzctlRuntimeConfig) -> Result<PathBuf, PloyzctlExecutionError> {
    config
        .cluster_context_path
        .clone()
        .or_else(default_cluster_context_path)
        .ok_or_else(|| remote_machine_error(RemoteMachineExecutionError::NoConfigDirectory))
}

/// Validates and normalizes a collected seed before it is published in the
/// local context material generation.
fn normalize_collected_seed(
    remote_path: &str,
    raw: &str,
) -> Result<String, PloyzctlExecutionError> {
    let trimmed = raw.trim();
    let Ok(_) = NatsUserSeed::try_new(trimmed) else {
        return Err(remote_machine_error(
            RemoteMachineExecutionError::CollectedSeedInvalid {
                remote_path: remote_path.to_owned(),
            },
        ));
    };
    Ok(format!("{trimmed}\n"))
}

/// `ployzctl machine init USER@HOST`: read the remote identity, build the
/// existing first-node install spec internally, run the installer in
/// first-node mode over SSH, collect the operator NATS material, record
/// the local cluster context, and activate the first node through the
/// existing API.
pub(crate) async fn execute_machine_init(
    command: MachineInitCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let probe = ssh_client(config, DEFAULT_SSH_COMMAND_TIMEOUT);
    let installer = ssh_client(config, config.ssh_install_timeout());
    let target = command.target.clone();

    let identity = derive_remote_identity(&probe, &target, command.identity_override.clone())?;
    let (manifest_url, manifest) = resolve_release_manifest(&probe, &target, &command)?;

    let nats_output = installer
        .run(
            &target,
            SshPhase::PrepareMachine,
            &remote_bootstrap::ensure_nats_server_command(),
        )
        .map_err(ssh_error)?;
    let nats_server_sha256 =
        remote_bootstrap::parse_sha256_output(&nats_output.stdout.text).map_err(remote_error)?;

    let node_public_ip: Option<IpAddr> = target.host().parse().ok();
    let spec = remote_bootstrap::build_first_node_install_spec(
        identity.node_id.clone(),
        command.roles,
        node_public_ip,
        command.bootstrap_url.clone(),
        &manifest,
        nats_server_sha256,
    );
    let spec_json =
        serde_json::to_string_pretty(&spec).expect("first-node install spec serializes");
    probe
        .run(
            &target,
            SshPhase::PrepareMachine,
            &remote_bootstrap::write_remote_file_command(
                remote_bootstrap::REMOTE_FIRST_NODE_SPEC_PATH,
                &spec_json,
            ),
        )
        .map_err(ssh_error)?;

    write_join_template_over_ssh(
        &probe,
        &target,
        &command,
        remote_bootstrap::PLACEHOLDER_CA_PEM,
        &manifest,
    )?;

    installer
        .run(
            &target,
            SshPhase::RunInstaller,
            &remote_bootstrap::first_node_installer_command(
                &command.installer_source(),
                &manifest_url,
                &command.version,
            ),
        )
        .map_err(ssh_error)?;

    let material_paths = remote_bootstrap::remote_material_paths();
    let remote_ca_path = material_paths.ca_file();
    let remote_ca_path = remote_ca_path.to_string_lossy();
    let ca_pem = read_remote_text(&probe, &target, &remote_ca_path)?;
    write_join_template_over_ssh(&probe, &target, &command, &ca_pem, &manifest)?;
    probe
        .run(
            &target,
            SshPhase::RestartControl,
            remote_bootstrap::RESTART_CONTROL_COMMAND,
        )
        .map_err(ssh_error)?;

    let remote_operator_seed_path = material_paths.operator_seed_file();
    let remote_operator_seed_path = remote_operator_seed_path.to_string_lossy();
    let remote_join_seed_path = material_paths.join_seed_file();
    let remote_join_seed_path = remote_join_seed_path.to_string_lossy();
    let operator_seed = read_remote_text(&probe, &target, &remote_operator_seed_path)?;
    let join_seed = read_remote_text(&probe, &target, &remote_join_seed_path)?;

    let context_path = cluster_context_path(config)?;
    let operator_seed = normalize_collected_seed(&remote_operator_seed_path, &operator_seed)?;
    let join_seed = normalize_collected_seed(&remote_join_seed_path, &join_seed)?;

    let context_nats_url = format!(
        "tls://{}:{}",
        target.host(),
        remote_bootstrap::MACHINE_NATS_PORT
    );
    let nats_url = NatsClientUrl::try_new(context_nats_url.clone()).map_err(|error| {
        remote_machine_error(RemoteMachineExecutionError::InvalidClusterContextUrl {
            url: context_nats_url,
            error,
        })
    })?;
    let context = publish_cluster_context(
        &context_path,
        nats_url,
        ClusterContextMaterial {
            ca_pem: &ca_pem,
            operator_seed: &operator_seed,
            join_seed: Some(&join_seed),
        },
    )
    .map_err(|source| {
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
    RemoteBootstrap {
        source: RemoteBootstrapError,
    },
    CollectedRemoteFileTruncated {
        path: String,
    },
    CollectedSeedInvalid {
        remote_path: String,
    },
    ClusterContext {
        source: ClusterContextError,
    },
    NoConfigDirectory,
    InvalidClusterContextUrl {
        url: String,
        error: NatsClientUrlError,
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
            Self::RemoteBootstrap { source } => write!(formatter, "{source}"),
            Self::CollectedRemoteFileTruncated { path } => write!(
                formatter,
                "remote file {path} was truncated while collecting operator material"
            ),
            Self::CollectedSeedInvalid { remote_path } => write!(
                formatter,
                "collected seed file {remote_path} does not contain an SU-prefixed user seed"
            ),
            Self::ClusterContext { source } => write!(formatter, "{source}"),
            Self::NoConfigDirectory => write!(
                formatter,
                "cannot determine the config directory for the cluster context (set HOME or XDG_CONFIG_HOME)"
            ),
            Self::InvalidClusterContextUrl { url, error } => write!(
                formatter,
                "cannot record cluster context NATS URL {url}: {error:?}"
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
