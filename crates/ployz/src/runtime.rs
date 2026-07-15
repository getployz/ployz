//! Runtime execution for parsed CLI commands.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::commands::PloyzctlCommand;
use crate::confirmation::{confirm_namespace_remove, confirm_volume_remove};
use crate::execution_support::{
    api_error, current_unix_seconds, operation_api_client, render_api_call,
};
use crate::machine::operator_context::ClusterContext;
use crate::machine::runtime::remote::{execute_core_promote_remote, execute_core_replace_remote};
use tokio::time::sleep as async_sleep;

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
        PloyzctlCommand::ComposeCheck(command) => {
            Ok(crate::deploy::runtime::compose::check(command))
        }
        PloyzctlCommand::Deploy(command) => {
            crate::deploy::runtime::follow::execute_deploy(command, config).await
        }
        PloyzctlCommand::DeployHistory(command) => {
            crate::deploy::runtime::history::inspect(command, config)
        }
        PloyzctlCommand::DeployRollback(command) => {
            crate::deploy::runtime::rollback::execute(command, config).await
        }
        PloyzctlCommand::InternalInit(command) => {
            crate::machine::runtime::founder_init(*command, config)
        }
        PloyzctlCommand::InitFirstMachineActivate(command) => {
            crate::machine::runtime::activate_founder(command, config).await
        }
        PloyzctlCommand::InitJoinTemplate(command) => {
            Ok(crate::machine::runtime::render_join_template(command))
        }
        PloyzctlCommand::IngressConfigure(command) => {
            let api = operation_api_client(config).await?;
            let accepted = api
                .ingress_configure(&command.into_request())
                .await
                .map_err(api_error)?;
            crate::operation::runtime::watch_accepted(&api, accepted.operation_id, config).await
        }
        PloyzctlCommand::MachineInit(command) => {
            crate::machine::runtime::remote::execute_machine_init(command, config).await
        }
        PloyzctlCommand::MachineAddRemote(command) => {
            crate::machine::runtime::remote::execute_machine_add_remote(command, config).await
        }
        PloyzctlCommand::MachineAdd(command) => crate::machine::runtime::add(command, config).await,
        PloyzctlCommand::MachineUpdate(command) => {
            crate::machine::runtime::update(command, config).await
        }
        PloyzctlCommand::MachineLifecycle(command) => {
            crate::machine::runtime::lifecycle(command, config).await
        }
        PloyzctlCommand::MachineList(command) => {
            crate::machine::runtime::list(command, config).await
        }
        PloyzctlCommand::MachineInspect(command) => {
            crate::machine::runtime::inspect(command, config).await
        }
        PloyzctlCommand::NetworkStatus(command) => {
            crate::network::runtime::status(command, config).await
        }
        PloyzctlCommand::NetworkResolve(command) => {
            crate::network::runtime::resolve(command, config).await
        }
        PloyzctlCommand::NetworkRepair(command) => {
            crate::network::runtime::repair(command, config).await
        }
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
                    crate::machine::command::AcceptedOperationOutput::from_accepted(accepted)
                        .render(),
                ));
            }
            crate::operation::runtime::watch_accepted(&api, accepted.operation_id, config).await
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
                    crate::machine::command::AcceptedOperationOutput::from_accepted(accepted)
                        .render(),
                ));
            }
            crate::operation::runtime::watch_accepted(&api, accepted.operation_id, config).await
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
                    crate::machine::command::AcceptedOperationOutput::from_accepted(accepted)
                        .render(),
                ));
            }
            crate::operation::runtime::watch_accepted(&api, accepted.operation_id, config).await
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
            crate::operation::runtime::status(command, config).await
        }
        PloyzctlCommand::OpsList(command) => crate::operation::runtime::list(command, config).await,
        PloyzctlCommand::OpsWatch(command) => {
            crate::operation::runtime::watch(command, config).await
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
