//! Control-Plane Core recovery command execution.

use std::net::IpAddr;
use std::path::PathBuf;
use std::{error, fmt};

use serde::Deserialize;

use crate::core::command::{CorePromoteCommand, CoreReplaceCommand};
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::{
    CommandExit, ExecutionSupportError, PloyzctlExecutionOutput, api_error,
    generate_client_core_replace_id, operation_api_client, watch_operation_until_terminal,
    with_cluster_context_from_disk,
};
use crate::machine::operator_context::{
    ClusterContextError, default_cluster_context_path, load_cluster_context, save_cluster_context,
};
use crate::shell::shell_quote;
use crate::ssh::{
    DEFAULT_SSH_COMMAND_TIMEOUT, MarkerBlockError, SshClient, SshCommandError, SshPhase, SshTarget,
    extract_marker_json,
};
use ployz_core::ids::MachineId;
use ployz_core::install::MachineJoinRuntimeNatsUrl;
use ployz_core::nats_config::NatsCaCertificatePem;
use ployz_core::operation::{
    CoreReplaceFailure, EventSequence, FailureMessage, MAX_OPERATION_EVENT_REPLAY_LIMIT,
    OperationEventReplayLimit, OperationEventReplayRequest,
};
use ployz_nats::connect::NatsClientUrl;
use ployz_sdk_types::{
    CoreReplaceReportOutcome, CoreReplaceReportRequest, CoreReplaceRequest, MachineListRequest,
};

const CORE_PROMOTE_RESULT_BEGIN: &str = "ployz-core-promote-result begin";
const CORE_PROMOTE_RESULT_END: &str = "ployz-core-promote-result end";

pub(crate) async fn promote(
    command: CorePromoteCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let recovery_secret = std::env::var("PLOYZ_RECOVERY_SECRET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| core_error(CoreRuntimeError::MissingRecoverySecret))?;
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

pub(crate) async fn replace(
    command: CoreReplaceCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let config = with_cluster_context_from_disk(config.clone())?;
    let api = operation_api_client(&config).await?;
    let machine_id = resolve_core_replace_machine_id(&command.target, &config, &api).await?;
    let operation_id = generate_client_core_replace_id(&machine_id)
        .map_err(|error| {
            core_error(CoreRuntimeError::ClientGeneratedIds {
                message: error.to_string(),
            })
        })?
        .operation_id;
    let successor_nats_url = config
        .nats_url
        .as_deref()
        .and_then(|url| NatsClientUrl::try_new(url).ok())
        .ok_or(ExecutionSupportError::MissingNatsUrl)?;
    let successor_runtime_nats_url =
        MachineJoinRuntimeNatsUrl::try_new(successor_nats_url.as_str()).map_err(|error| {
            core_error(CoreRuntimeError::InvalidSuccessorRuntimeNatsUrl {
                host: successor_nats_url.as_str().to_owned(),
                message: error.to_string(),
            })
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
        return Err(core_error(CoreRuntimeError::CorePromoteOutputTruncated));
    }

    let json_line = extract_marker_json(stdout, CORE_PROMOTE_RESULT_BEGIN, CORE_PROMOTE_RESULT_END)
        .map_err(|error| match error {
            MarkerBlockError::Missing => core_error(CoreRuntimeError::MissingCorePromoteResult),
            MarkerBlockError::Malformed(message) => invalid_core_promote_result(message),
        })?;
    let file: CorePromoteResultFile = serde_json::from_str(json_line).map_err(|error| {
        core_error(CoreRuntimeError::InvalidCorePromoteResult {
            message: error.to_string(),
        })
    })?;
    core_promote_result_from_file(file)
}

fn core_promote_result_from_file(
    file: CorePromoteResultFile,
) -> Result<CorePromoteResult, PloyzctlExecutionError> {
    NatsCaCertificatePem::try_new(file.ca_pem.as_str()).map_err(|error| {
        core_error(CoreRuntimeError::InvalidCorePromoteResult {
            message: format!("invalid ca_pem: {error}"),
        })
    })?;
    let nats_urls = file
        .nats_urls
        .into_iter()
        .map(|url| {
            NatsClientUrl::try_new(url).map_err(|error| {
                core_error(CoreRuntimeError::InvalidCorePromoteResult {
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
    core_error(CoreRuntimeError::InvalidCorePromoteResult {
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
    let Some(mut context) = load_cluster_context(&path)
        .map_err(|source| core_error(CoreRuntimeError::ClusterContext { source }))?
    else {
        return Ok(None);
    };
    context.nats_url = result.primary_nats_url().clone();
    save_cluster_context(&path, &context)
        .map_err(|source| core_error(CoreRuntimeError::ClusterContext { source }))?;
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
    let Some(context) = load_cluster_context(&path)
        .map_err(|source| core_error(CoreRuntimeError::ClusterContext { source }))?
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
        return Err(core_error(CoreRuntimeError::CoreReplaceMachineUnknown {
            target: target.destination(),
        }));
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
        [] => Err(core_error(CoreRuntimeError::CoreReplaceMachineUnknown {
            target: target.destination(),
        })),
        many => Err(core_error(CoreRuntimeError::CoreReplaceMachineAmbiguous {
            target: target.destination(),
            machine_ids: many.to_vec(),
        })),
    }
}

fn optional_cluster_context_path(config: &PloyzctlRuntimeConfig) -> Option<PathBuf> {
    config
        .cluster_context_path
        .clone()
        .or_else(default_cluster_context_path)
}

fn ssh_client(config: &PloyzctlRuntimeConfig, timeout: std::time::Duration) -> SshClient {
    match &config.ssh_program {
        Some(program) => SshClient::with_program(program.clone(), timeout),
        None => SshClient::system(timeout),
    }
}

fn core_error(source: CoreRuntimeError) -> PloyzctlExecutionError {
    source.into()
}

fn ssh_error(source: Box<SshCommandError>) -> PloyzctlExecutionError {
    core_error(CoreRuntimeError::Ssh { source })
}

fn failure_message(message: String) -> FailureMessage {
    match FailureMessage::try_new(message) {
        Ok(message) => message,
        Err(_) => FailureMessage::try_new("core demotion failed").expect("valid failure message"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreRuntimeError {
    Ssh {
        source: Box<SshCommandError>,
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
    InvalidSuccessorRuntimeNatsUrl {
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
}

impl fmt::Display for CoreRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ssh { source } => write!(formatter, "{source}"),
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
            Self::InvalidSuccessorRuntimeNatsUrl { host, message } => write!(
                formatter,
                "cannot build founder bootstrap runtime NATS URL from host {host:?}: {message}"
            ),
            Self::ClientGeneratedIds { message } => write!(
                formatter,
                "could not generate client operation ids: {message}"
            ),
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
        }
    }
}

impl error::Error for CoreRuntimeError {}

impl From<CoreRuntimeError> for PloyzctlExecutionError {
    fn from(error: CoreRuntimeError) -> Self {
        Self::Core(error)
    }
}
