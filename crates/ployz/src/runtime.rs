//! Runtime execution for parsed CLI commands.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::commands::PloyzctlCommand;
use crate::commands::init::FirstMachineInitMode;
use crate::config::ClusterContext;
use crate::confirmation::{confirm_namespace_remove, confirm_volume_remove};
use crate::execution_support::{
    activate_first_machine, api_error, current_unix_seconds, nats_connect_config,
    operation_api_client, operation_api_client_with_connect, operation_replay_request,
    read_join_seed, render_api_call, replay_operation_events, watch_accepted_operation,
    watch_operation_until_terminal, with_cluster_context_from_disk,
};
use crate::host_runner_install::run_host_runner_first_machine_install;
use crate::remote_machine_runtime::{
    execute_core_promote_remote, execute_core_replace_remote, execute_machine_add_remote,
    execute_machine_init,
};
use ployz_sdk_types::OpsStatusRequest;
use tokio::time::sleep as async_sleep;

mod compose;
mod deploy_follow;
mod deploy_history;
mod deploy_rollback;
mod network;

pub use crate::execution_support::{
    CommandExit, DEFAULT_NATS_CONNECT_TIMEOUT, DEFAULT_OPS_WATCH_POLL_INTERVAL,
    DEFAULT_OPS_WATCH_TIMEOUT, PLOYZ_JOIN_NKEY_SEED_FILE_ENV, PLOYZ_NATS_CA_FILE_ENV,
    PLOYZ_NATS_NKEY_SEED_FILE_ENV, PLOYZ_NATS_URL_ENV, PloyzctlExecutionError,
    PloyzctlExecutionOutput,
};

/// Stand-in for the system `ssh` (test/automation seam for the remote
/// machine bootstrap commands).
pub const PLOYZ_SSH_PROGRAM_ENV: &str = "PLOYZ_SSH_PROGRAM";
pub const DEFAULT_HOST_RUNNER_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
/// Budget for the remote installer phase (artifact downloads plus the Host Runner
/// install); much longer than the per-command probe timeout.
pub const DEFAULT_SSH_INSTALL_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PloyzctlRuntimeConfig {
    pub nats_url: Option<String>,
    pub nats_ca_file: Option<PathBuf>,
    pub nats_seed_file: Option<PathBuf>,
    pub join_seed_file: Option<PathBuf>,
    pub nats_connect_timeout: Option<Duration>,
    pub host_runner_install_timeout: Option<Duration>,
    pub ops_watch_timeout: Option<Duration>,
    pub ops_watch_poll_interval: Option<Duration>,
    /// Program to run instead of the system `ssh` (test seam).
    pub ssh_program: Option<PathBuf>,
    /// Per-command budget for the remote installer phase.
    pub ssh_install_timeout: Option<Duration>,
    /// Where `machine init` records the local cluster context (test seam;
    /// defaults to the user config directory).
    pub cluster_context_path: Option<PathBuf>,
    /// Deploy-history root override for embedded runtimes and tests.
    pub deploy_history_root: Option<PathBuf>,
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
            host_runner_install_timeout: None,
            ops_watch_timeout: None,
            ops_watch_poll_interval: None,
            ssh_program: std::env::var(PLOYZ_SSH_PROGRAM_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from),
            ssh_install_timeout: None,
            cluster_context_path: None,
            deploy_history_root: None,
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

    #[must_use]
    pub fn nats_connect_timeout(&self) -> Duration {
        self.nats_connect_timeout
            .unwrap_or(DEFAULT_NATS_CONNECT_TIMEOUT)
    }

    #[must_use]
    pub fn host_runner_install_timeout(&self) -> Duration {
        self.host_runner_install_timeout
            .unwrap_or(DEFAULT_HOST_RUNNER_INSTALL_TIMEOUT)
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
        PloyzctlCommand::Login => {
            Err(PloyzctlExecutionError::CloudUnconfigured { command: "login" })
        }
        PloyzctlCommand::Telemetry(_) => Err(PloyzctlExecutionError::LocalCommand),
        PloyzctlCommand::CorePromote(command) => execute_core_promote_remote(command, config).await,
        PloyzctlCommand::CoreReplace(command) => execute_core_replace_remote(command, config).await,
        PloyzctlCommand::ComposeCheck(command) => Ok(compose::check(command)),
        PloyzctlCommand::Deploy(command) => deploy_follow::execute_deploy(command, config).await,
        PloyzctlCommand::DeployHistory(command) => deploy_history::inspect(command, config),
        PloyzctlCommand::DeployRollback(command) => deploy_rollback::execute(command, config).await,
        PloyzctlCommand::InternalInit(command) => match &command.mode {
            FirstMachineInitMode::RunHostRunnerInstall {
                host_runner_install,
                host_runner_binary,
            } => {
                let output = run_host_runner_first_machine_install(
                    host_runner_binary,
                    host_runner_install,
                    config.host_runner_install_timeout(),
                )
                .map_err(|source| {
                    PloyzctlExecutionError::HostRunnerFirstMachineInstall { source }
                })?;
                Ok(PloyzctlExecutionOutput {
                    stdout: output.stdout,
                    stderr: output.stderr,
                    exit: CommandExit::Success,
                })
            }
            FirstMachineInitMode::Summary { .. }
            | FirstMachineInitMode::EmitHostRunnerInstall(_) => {
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
        PloyzctlCommand::IngressConfigure(command) => {
            let api = operation_api_client(config).await?;
            let accepted = api
                .ingress_configure(&command.into_request())
                .await
                .map_err(api_error)?;
            watch_accepted_operation(&api, accepted.operation_id, config).await
        }
        PloyzctlCommand::MachineInit(command) => execute_machine_init(command, config).await,
        PloyzctlCommand::MachineAddRemote(command) => {
            execute_machine_add_remote(command, config).await
        }
        PloyzctlCommand::MachineAdd(command) => {
            let config = with_cluster_context_from_disk(config.clone())?;
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
        PloyzctlCommand::MachineUpdate(command) => {
            let detach = command.detach;
            let api = operation_api_client(config).await?;
            let accepted = api
                .machine_update(&command.into_request())
                .await
                .map_err(api_error)?;
            if detach {
                return Ok(PloyzctlExecutionOutput::stdout(
                    crate::commands::machine::AcceptedOperationOutput::from_accepted(accepted)
                        .render(),
                ));
            }
            watch_accepted_operation(&api, accepted.operation_id, config).await
        }
        PloyzctlCommand::MachineLifecycle(command) => {
            let detach = command.detach;
            let target = command.target;
            let api = operation_api_client(config).await?;
            let request = command.into_request();
            let accepted = match target {
                ployz_core::state::MachineLifecycle::Draining => api.machine_drain(&request).await,
                ployz_core::state::MachineLifecycle::Active => api.machine_resume(&request).await,
            }
            .map_err(api_error)?;
            if detach {
                return Ok(PloyzctlExecutionOutput::stdout(
                    crate::commands::machine::AcceptedOperationOutput::from_accepted(accepted)
                        .render(),
                ));
            }
            watch_accepted_operation(&api, accepted.operation_id, config).await
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
        PloyzctlCommand::NetworkStatus(command) => network::status(command, config).await,
        PloyzctlCommand::NetworkResolve(command) => network::resolve(command, config).await,
        PloyzctlCommand::NetworkRepair(command) => network::repair(command, config).await,
        PloyzctlCommand::ServiceList(command) => {
            render_api_call(
                config,
                async |api| api.service_list(&command.into_request()).await,
                |result| crate::commands::service::ServiceListOutput::from_result(result).render(),
            )
            .await
        }
        PloyzctlCommand::VolumeList(command) => {
            render_api_call(
                config,
                async |api| api.volume_list(&command.into_request()).await,
                |result| crate::commands::volume::VolumeListOutput::from_result(result).render(),
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
        PloyzctlCommand::ServiceRestart(command) => {
            let detach = command.detach;
            let api = operation_api_client(config).await?;
            let accepted = api
                .service_restart(&command.into_request())
                .await
                .map_err(api_error)?;
            if detach {
                return Ok(PloyzctlExecutionOutput::stdout(
                    crate::commands::machine::AcceptedOperationOutput::from_accepted(accepted)
                        .render(),
                ));
            }
            watch_accepted_operation(&api, accepted.operation_id, config).await
        }
        PloyzctlCommand::NamespaceRemove(command) => {
            let detach = command.detach;
            if !command.force {
                confirm_namespace_remove(&command.namespace_id)?;
            }
            let api = operation_api_client(config).await?;
            let accepted = api
                .namespace_remove(&command.into_request())
                .await
                .map_err(api_error)?;
            if detach {
                return Ok(PloyzctlExecutionOutput::stdout(
                    crate::commands::machine::AcceptedOperationOutput::from_accepted(accepted)
                        .render(),
                ));
            }
            watch_accepted_operation(&api, accepted.operation_id, config).await
        }
        PloyzctlCommand::VolumeRemove(command) => {
            let detach = command.detach;
            if !command.force {
                confirm_volume_remove(&command.namespace_id, &command.volume_name)?;
            }
            let api = operation_api_client(config).await?;
            let accepted = api
                .volume_remove(&command.into_request())
                .await
                .map_err(api_error)?;
            if detach {
                return Ok(PloyzctlExecutionOutput::stdout(
                    crate::commands::machine::AcceptedOperationOutput::from_accepted(accepted)
                        .render(),
                ));
            }
            watch_accepted_operation(&api, accepted.operation_id, config).await
        }
        PloyzctlCommand::LogsTail(command) => {
            if command.follow {
                follow_logs(command, config).await
            } else {
                render_api_call(
                    config,
                    async |api| api.logs_tail(&command.into_request()).await,
                    |result| crate::commands::logs::LogsTailOutput::new(result).render(),
                )
                .await
            }
        }
        PloyzctlCommand::OpsStatus(command) => {
            let api = operation_api_client(config).await?;
            let operation_id = command.operation_id;
            let snapshot = api
                .ops_status(&OpsStatusRequest {
                    operation_id: operation_id.clone(),
                })
                .await
                .map_err(api_error)?;
            let events =
                replay_operation_events(&api, operation_replay_request(operation_id)).await?;
            Ok(PloyzctlExecutionOutput::stdout(
                crate::commands::ops::StatusOutput::new(snapshot, events).render(),
            ))
        }
        PloyzctlCommand::OpsList(command) => {
            let active_only = command.active_only;
            let api = operation_api_client(config).await?;
            let result = api
                .ops_list(&command.into_request())
                .await
                .map_err(api_error)?;
            let output = crate::commands::ops::ListOutput::from_result(result);
            Ok(PloyzctlExecutionOutput::stdout(output.render())
                .with_stderr(output.render_more_hint(active_only)))
        }
        PloyzctlCommand::OpsWatch(command) => {
            let api = operation_api_client(config).await?;
            let output = command.output;
            let request = command.into_request();
            let (events, outcome) = watch_operation_until_terminal(
                &api,
                request,
                config.ops_watch_timeout(),
                config.ops_watch_poll_interval(),
            )
            .await?;

            Ok(PloyzctlExecutionOutput::stdout(
                crate::commands::ops::WatchOutput { events, output }.render(),
            )
            .with_operation_outcome(outcome))
        }
    }
}

async fn follow_logs(
    command: crate::commands::logs::LogsTailCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let api = operation_api_client(config).await?;
    let started_at = Instant::now();
    let mut output = String::new();
    let mut request = command.clone().into_request();
    loop {
        let next_since = current_unix_seconds();
        let result = api.logs_tail(&request).await.map_err(api_error)?;
        output.push_str(&crate::commands::logs::LogsTailOutput::new(result).render());
        if started_at.elapsed() >= config.ops_watch_timeout() {
            return Ok(PloyzctlExecutionOutput::stdout(output));
        }
        request = command.request_after(next_since);
        async_sleep(config.ops_watch_poll_interval()).await;
    }
}
