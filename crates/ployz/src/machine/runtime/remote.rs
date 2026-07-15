//! Runtime execution for remote machine bootstrap commands.
//!
//! This module owns the SSH-driven `machine init` and remote `machine add`
//! flows. Shared authenticated NATS and operation mechanics live in
//! `execution_support`; the Machine runtime dispatches parsed commands.

use std::fmt;
use std::path::PathBuf;

use crate::core::command::{CorePromoteCommand, CoreReplaceCommand};
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_support::generate_client_machine_add_ids;
use crate::execution_support::{
    CommandExit, PloyzctlExecutionError, PloyzctlExecutionOutput, api_error, operation_api_client,
    watch_operation_until_terminal, with_cluster_context_from_disk,
};
use crate::machine::bootstrap::{
    BootstrapInstaller, BootstrapRelease, FounderBootstrapCommand, MACHINE_NATS_PORT,
};
use crate::machine::command::{
    MachineAddInstaller, MachineAddOutput, MachineAddRemoteCommand, MachineAddRemoteDetachedOutput,
    MachineAddRemoteOutput, MachineIdentity, MachineIdentityError, MachineInitCommand,
    MachineInitOutput,
};
use crate::machine::founder::FirstMachineActivateCommand;
use crate::machine::local_release::LocalReleaseStageError;
use crate::machine::operator_context::{
    ClusterContextError, ClusterContextMaterial, default_cluster_context_path,
    load_cluster_context, publish_cluster_context, save_cluster_context,
    save_cluster_context_machine_ssh,
};
use crate::machine::ssh::{
    DEFAULT_SSH_COMMAND_TIMEOUT, SshClient, SshCommandError, SshPhase, SshTarget,
};
use crate::shell::shell_quote;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::install::MachineJoinRuntimeNatsUrl;
use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
use ployz_core::ops::{CoreReplaceFailure, FailureMessage, MachineAddOperationState};
use ployz_core::ops::{
    EventSequence, MAX_OPERATION_EVENT_REPLAY_LIMIT, OperationEventReplayLimit,
    OperationEventReplayRequest, OperationStatus,
};
use ployz_nats::connect::NatsClientUrl;
use ployz_sdk_types::{
    CoreReplaceReportOutcome, CoreReplaceReportRequest, CoreReplaceRequest, MachineAddRequest,
    MachineListRequest, OpsStatusRequest,
};
use serde::Deserialize;
use std::net::IpAddr;

use super::{activate_first_machine, read_join_seed};

const FIRST_MACHINE_BOOTSTRAP_RESULT_BEGIN: &str = "ployz-first-machine-bootstrap-result begin";
const FIRST_MACHINE_BOOTSTRAP_RESULT_END: &str = "ployz-first-machine-bootstrap-result end";
const CORE_PROMOTE_RESULT_BEGIN: &str = "ployz-core-promote-result begin";
const CORE_PROMOTE_RESULT_END: &str = "ployz-core-promote-result end";

enum MarkerBlockError {
    Missing,
    Malformed(&'static str),
}

/// Finds the `begin`/`end` marker block in remote stdout and returns the JSON
/// line between the markers.
fn extract_marker_json<'a>(
    stdout: &'a str,
    begin: &str,
    end: &str,
) -> Result<&'a str, MarkerBlockError> {
    let mut lines = stdout.lines();
    while let Some(line) = lines.next() {
        if line.trim() != begin {
            continue;
        }
        let Some(json_line) = lines.next() else {
            return Err(MarkerBlockError::Malformed(
                "result marker was not followed by JSON",
            ));
        };
        let Some(end_line) = lines.next() else {
            return Err(MarkerBlockError::Malformed(
                "result JSON was not followed by an end marker",
            ));
        };
        if end_line.trim() != end {
            return Err(MarkerBlockError::Malformed("result end marker was missing"));
        }
        return Ok(json_line);
    }
    Err(MarkerBlockError::Missing)
}

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

pub(crate) async fn execute_core_promote_remote(
    command: CorePromoteCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let recovery_secret = std::env::var("PLOYZ_RECOVERY_SECRET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| remote_machine_error(RemoteMachineExecutionError::MissingRecoverySecret))?;
    let client = ssh_client(config, config.ssh_install_timeout());
    let remote_command = "sudo sh -c 'IFS= read -r PLOYZ_RECOVERY_SECRET; export PLOYZ_RECOVERY_SECRET; exec ployz host core-promote'";
    let recovery_secret_stdin = format!("{recovery_secret}\n");
    let output = client
        .run_with_stdin(
            &command.target,
            SshPhase::CorePromote,
            remote_command,
            recovery_secret_stdin.as_bytes(),
        )
        .map_err(ssh_error)?;
    let promoted = parse_core_promote_result(&output.stdout.text, output.stdout.truncated)?;
    let mut stderr = output.stderr.text;

    match update_context_after_core_promote(config, &promoted)? {
        Some(path) => {
            stderr.push_str(&format!(
                "context {} now points at {}\n",
                path.display(),
                promoted.primary_nats_url().as_str()
            ));
        }
        None => {
            stderr.push_str(
                "warning: no local cluster context found; use --nats with one of the promoted URLs\n",
            );
        }
    }

    Ok(PloyzctlExecutionOutput {
        stdout: promoted.render(&command.target),
        stderr,
        exit: CommandExit::Success,
    })
}

pub(crate) async fn execute_core_replace_remote(
    command: CoreReplaceCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let config = with_cluster_context_from_disk(config.clone())?;
    let api = operation_api_client(&config).await?;
    let machine_id = resolve_core_replace_machine_id(&command.target, &config, &api).await?;
    let operation_id = crate::execution_support::generate_client_core_replace_id(&machine_id)
        .map_err(|error| client_generated_ids_error(error.to_string()))?
        .operation_id;
    let successor_nats_url = config
        .nats_url
        .as_deref()
        .and_then(|url| NatsClientUrl::try_new(url).ok())
        .ok_or(PloyzctlExecutionError::MissingNatsUrl)?;
    let successor_runtime_nats_url =
        MachineJoinRuntimeNatsUrl::try_new(successor_nats_url.as_str()).map_err(|error| {
            remote_machine_error(
                RemoteMachineExecutionError::InvalidBootstrapRuntimeNatsUrl {
                    host: successor_nats_url.as_str().to_owned(),
                    message: error.to_string(),
                },
            )
        })?;
    let accepted = api
        .core_replace(&CoreReplaceRequest {
            operation_id: operation_id.clone(),
            machine_id: machine_id.clone(),
            successor_nats_url: successor_runtime_nats_url,
        })
        .await
        .map_err(api_error)?;

    let client = ssh_client(&config, config.ssh_install_timeout());
    let mut remote_command = "sudo ployz host internal-core-demote".to_owned();
    remote_command.push_str(" --successor-nats-url ");
    remote_command.push_str(&shell_quote(successor_nats_url.as_str()));
    let ssh_result = client.run(&command.target, SshPhase::CoreReplace, &remote_command);
    let outcome = match &ssh_result {
        Ok(_) => CoreReplaceReportOutcome::Completed,
        Err(source) => CoreReplaceReportOutcome::Failed {
            failure: CoreReplaceFailure::DemoteFailed {
                message: failure_message(source.to_string()),
            },
        },
    };
    api.core_replace_report(&CoreReplaceReportRequest {
        operation_id: operation_id.clone(),
        machine_id: machine_id.clone(),
        outcome,
    })
    .await
    .map_err(api_error)?;

    if let Err(source) = ssh_result {
        return Err(ssh_error(source));
    }

    let (_events, outcome) = watch_operation_until_terminal(
        &api,
        OperationEventReplayRequest {
            operation_id: accepted.operation_id,
            start_sequence: EventSequence::first(),
            limit: OperationEventReplayLimit::try_new(MAX_OPERATION_EVENT_REPLAY_LIMIT)
                .expect("max replay limit is valid"),
        },
        config.ops_watch_timeout(),
        config.ops_watch_poll_interval(),
    )
    .await?;

    Ok(PloyzctlExecutionOutput::stdout(format!(
        "core demoted on {}\n",
        command.target.destination()
    ))
    .with_operation_outcome(outcome))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CorePromoteResult {
    nats_urls: Vec<NatsClientUrl>,
}

impl CorePromoteResult {
    fn primary_nats_url(&self) -> &NatsClientUrl {
        self.nats_urls
            .first()
            .expect("core promote result validation requires at least one URL")
    }

    fn render(&self, target: &SshTarget) -> String {
        format!(
            "core promoted on {}\nnats {}\nca-pem printed by remote Host Runner\n",
            target.destination(),
            self.nats_urls
                .iter()
                .map(NatsClientUrl::as_str)
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorePromoteResultFile {
    nats_urls: Vec<String>,
    ca_pem: String,
}

fn parse_core_promote_result(
    stdout: &str,
    stdout_truncated: bool,
) -> Result<CorePromoteResult, PloyzctlExecutionError> {
    if stdout_truncated {
        return Err(remote_machine_error(
            RemoteMachineExecutionError::CorePromoteOutputTruncated,
        ));
    }

    let json_line = extract_marker_json(stdout, CORE_PROMOTE_RESULT_BEGIN, CORE_PROMOTE_RESULT_END)
        .map_err(|error| match error {
            MarkerBlockError::Missing => {
                remote_machine_error(RemoteMachineExecutionError::MissingCorePromoteResult)
            }
            MarkerBlockError::Malformed(message) => invalid_core_promote_result(message),
        })?;
    let file: CorePromoteResultFile = serde_json::from_str(json_line).map_err(|error| {
        remote_machine_error(RemoteMachineExecutionError::InvalidCorePromoteResult {
            message: error.to_string(),
        })
    })?;
    core_promote_result_from_file(file)
}

fn core_promote_result_from_file(
    file: CorePromoteResultFile,
) -> Result<CorePromoteResult, PloyzctlExecutionError> {
    NatsCaCertificatePem::try_new(file.ca_pem.as_str()).map_err(|error| {
        remote_machine_error(RemoteMachineExecutionError::InvalidCorePromoteResult {
            message: format!("invalid ca_pem: {error}"),
        })
    })?;
    let nats_urls = file
        .nats_urls
        .into_iter()
        .map(|url| {
            NatsClientUrl::try_new(url).map_err(|error| {
                remote_machine_error(RemoteMachineExecutionError::InvalidCorePromoteResult {
                    message: format!("invalid nats_urls entry: {error:?}"),
                })
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if nats_urls.is_empty() {
        return Err(invalid_core_promote_result("nats_urls was empty"));
    }
    Ok(CorePromoteResult { nats_urls })
}

fn invalid_core_promote_result(message: &str) -> PloyzctlExecutionError {
    remote_machine_error(RemoteMachineExecutionError::InvalidCorePromoteResult {
        message: message.to_owned(),
    })
}

fn update_context_after_core_promote(
    config: &PloyzctlRuntimeConfig,
    result: &CorePromoteResult,
) -> Result<Option<PathBuf>, PloyzctlExecutionError> {
    let Some(path) = optional_cluster_context_path(config) else {
        return Ok(None);
    };
    let Some(mut context) = load_cluster_context(&path).map_err(|source| {
        remote_machine_error(RemoteMachineExecutionError::ClusterContext { source })
    })?
    else {
        return Ok(None);
    };
    context.nats_url = result.primary_nats_url().clone();
    save_cluster_context(&path, &context).map_err(|source| {
        remote_machine_error(RemoteMachineExecutionError::ClusterContext { source })
    })?;
    Ok(Some(path))
}

async fn resolve_core_replace_machine_id(
    target: &SshTarget,
    config: &PloyzctlRuntimeConfig,
    api: &ployz_nats::operation_api_client::OperationApiClient,
) -> Result<MachineId, PloyzctlExecutionError> {
    if let Some(machine_id) = read_remote_machine_id(target, config) {
        return Ok(machine_id);
    }
    if let Some(machine_id) = context_machine_id(target, config)? {
        return Ok(machine_id);
    }
    machine_id_from_roster_endpoint(target, api).await
}

fn read_remote_machine_id(target: &SshTarget, config: &PloyzctlRuntimeConfig) -> Option<MachineId> {
    let client = ssh_client(config, DEFAULT_SSH_COMMAND_TIMEOUT);
    let output = client
        .run(
            target,
            SshPhase::CoreReplace,
            "sudo cat /var/lib/ployz/join-material",
        )
        .ok()?;
    parse_machine_id_from_join_material(&output.stdout.text)
}

fn parse_machine_id_from_join_material(contents: &str) -> Option<MachineId> {
    let raw = contents
        .lines()
        .find_map(|line| line.strip_prefix("machine_id="))?;
    MachineId::try_new(raw.to_owned()).ok()
}

fn context_machine_id(
    target: &SshTarget,
    config: &PloyzctlRuntimeConfig,
) -> Result<Option<MachineId>, PloyzctlExecutionError> {
    let Some(path) = optional_cluster_context_path(config) else {
        return Ok(None);
    };
    let Some(context) = load_cluster_context(&path).map_err(|source| {
        remote_machine_error(RemoteMachineExecutionError::ClusterContext { source })
    })?
    else {
        return Ok(None);
    };
    Ok(context
        .machines
        .into_iter()
        .find(|machine| machine.ssh.as_ref() == Some(target))
        .map(|machine| machine.machine_id))
}

async fn machine_id_from_roster_endpoint(
    target: &SshTarget,
    api: &ployz_nats::operation_api_client::OperationApiClient,
) -> Result<MachineId, PloyzctlExecutionError> {
    let Ok(host) = target.host().parse::<IpAddr>() else {
        return Err(remote_machine_error(
            RemoteMachineExecutionError::CoreReplaceMachineUnknown {
                target: target.destination(),
            },
        ));
    };
    let machines = api
        .machine_list(&MachineListRequest {})
        .await
        .map_err(api_error)?
        .machines;
    let matches = machines
        .into_iter()
        .filter(|machine| machine.active.control_endpoints.contains(&host))
        .map(|machine| machine.active.machine_id)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [machine_id] => Ok(machine_id.clone()),
        [] => Err(remote_machine_error(
            RemoteMachineExecutionError::CoreReplaceMachineUnknown {
                target: target.destination(),
            },
        )),
        many => Err(remote_machine_error(
            RemoteMachineExecutionError::CoreReplaceMachineAmbiguous {
                target: target.destination(),
                machine_ids: many.to_vec(),
            },
        )),
    }
}

fn failure_message(message: String) -> FailureMessage {
    match FailureMessage::try_new(message) {
        Ok(message) => message,
        Err(_) => FailureMessage::try_new("core demotion failed").expect("valid failure message"),
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

    let json_line = extract_marker_json(
        stdout,
        FIRST_MACHINE_BOOTSTRAP_RESULT_BEGIN,
        FIRST_MACHINE_BOOTSTRAP_RESULT_END,
    )
    .map_err(|error| match error {
        MarkerBlockError::Missing => {
            remote_machine_error(RemoteMachineExecutionError::MissingFirstMachineBootstrapResult)
        }
        MarkerBlockError::Malformed(message) => invalid_first_machine_bootstrap_result(message),
    })?;
    let file: FirstMachineBootstrapResultFile =
        serde_json::from_str(json_line).map_err(|error| {
            remote_machine_error(
                RemoteMachineExecutionError::InvalidFirstMachineBootstrapResult {
                    message: error.to_string(),
                },
            )
        })?;
    first_machine_bootstrap_result_from_file(file)
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

fn local_release_error(error: LocalReleaseStageError) -> PloyzctlExecutionError {
    remote_machine_error(RemoteMachineExecutionError::LocalRelease {
        message: error.to_string(),
    })
}

/// `ployz machine init USER@HOST`: derive the remote identity, render
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
    let staged_release = match &command.local_release {
        Some(bundle) => Some((
            bundle
                .stage(&installer, &target)
                .map_err(local_release_error)?,
            bundle.version().to_owned(),
        )),
        None => None,
    };
    let (bootstrap_installer, release, release_manifest_url) = match staged_release {
        Some((staged, version)) => (
            BootstrapInstaller::RemoteScript(staged.installer_script),
            BootstrapRelease::Version(version),
            Some(staged.manifest_url),
        ),
        None => (
            command.installer(),
            command.release.clone(),
            command.release_manifest_url.clone(),
        ),
    };
    let install_command = FounderBootstrapCommand {
        installer: bootstrap_installer,
        release,
        release_manifest_url,
        machine_id: identity.machine_id.clone(),
        roles: command.roles,
        bootstrap_url: command.bootstrap_url.clone(),
        cluster_name: command.cluster_name.clone(),
        runtime_nats_url: runtime_nats_url_for_target(&target)?,
        machine_public_ip: command.public_ip,
        host_port_assurance: command.host_port_assurance,
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
        &FirstMachineActivateCommand {
            machine_id: identity.machine_id.clone(),
            roles: command.roles,
            automatic_hostname_configuration: command.automatic_hostname_configuration.clone(),
            ployz_dns_target: command.ployz_dns_target,
        },
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
    // Surface Host Runner's install stderr: when PLOYZ_RECOVERY_SECRET is unset, Host Runner
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

/// `ployz machine add USER@HOST`: derive the identity from the remote
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
    let config = with_cluster_context_from_disk(config.clone())?;
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
            host_port_assurance: command.host_port_assurance,
        })
        .await
        .map_err(api_error)?;
    let output = MachineAddOutput::from_accepted(accepted, join_seed);

    if let Some(bundle) = &command.local_release {
        bundle
            .validate_join_material(&output.join_bundle.material)
            .map_err(|message| {
                remote_machine_error(RemoteMachineExecutionError::LocalReleaseAfterMachineAdd {
                    operation_id: operation_id.clone(),
                    message,
                })
            })?;
    }
    let staged_release = command
        .local_release
        .as_ref()
        .map(|bundle| {
            bundle
                .stage(&installer, &target)
                .map_err(local_release_error)
        })
        .transpose()?;
    let add_installer = staged_release.map_or_else(
        || command.installer(),
        |staged| MachineAddInstaller::RemoteScript(staged.installer_script),
    );
    let install_command = output.install_command(&add_installer);
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
    LocalRelease {
        message: String,
    },
    LocalReleaseAfterMachineAdd {
        operation_id: OperationId,
        message: String,
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
    MissingRecoverySecret,
    CorePromoteOutputTruncated,
    MissingCorePromoteResult,
    InvalidCorePromoteResult {
        message: String,
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
    CoreReplaceMachineUnknown {
        target: String,
    },
    CoreReplaceMachineAmbiguous {
        target: String,
        machine_ids: Vec<MachineId>,
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
            Self::LocalRelease { message } => write!(formatter, "local release: {message}"),
            Self::LocalReleaseAfterMachineAdd {
                operation_id,
                message,
            } => write!(
                formatter,
                "machine add operation {} accepted, but local release validation failed: {message}",
                operation_id.as_str()
            ),
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
            Self::MissingRecoverySecret => write!(
                formatter,
                "set PLOYZ_RECOVERY_SECRET before running ployz core promote"
            ),
            Self::CorePromoteOutputTruncated => write!(
                formatter,
                "core promote output was truncated before the result could be collected"
            ),
            Self::MissingCorePromoteResult => write!(
                formatter,
                "core promote output did not contain a structured result"
            ),
            Self::InvalidCorePromoteResult { message } => {
                write!(formatter, "core promote result is invalid: {message}")
            }
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
            Self::CoreReplaceMachineUnknown { target } => write!(
                formatter,
                "could not determine the machine id for core demote target {target}; the target did not expose join material and no unique roster endpoint matched it"
            ),
            Self::CoreReplaceMachineAmbiguous {
                target,
                machine_ids,
            } => write!(
                formatter,
                "core demote target {target} matched multiple machines: {}",
                machine_ids
                    .iter()
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
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
