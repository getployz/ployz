//! Thin dispatch for parsed CLI commands.

use std::path::PathBuf;
use std::time::Duration;

use crate::commands::PloyzctlCommand;
use crate::machine::operator_context::ClusterContext;

pub use crate::execution_error::PloyzctlExecutionError;
pub use crate::execution_support::{
    CommandExit, DEFAULT_NATS_CONNECT_TIMEOUT, DEFAULT_OPS_WATCH_POLL_INTERVAL,
    DEFAULT_OPS_WATCH_TIMEOUT, PLOYZ_JOIN_NKEY_SEED_FILE_ENV, PLOYZ_NATS_CA_FILE_ENV,
    PLOYZ_NATS_NKEY_SEED_FILE_ENV, PLOYZ_NATS_URL_ENV, PloyzctlExecutionOutput,
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
        PloyzctlCommand::BuildSubmit(command) => {
            crate::build::runtime::submit(command, config).await
        }
        PloyzctlCommand::BuildCancel(command) => {
            crate::build::runtime::cancel(command, config).await
        }
        PloyzctlCommand::CorePromote(command) => {
            crate::core::runtime::promote(command, config).await
        }
        PloyzctlCommand::CoreReplace(command) => {
            crate::core::runtime::replace(command, config).await
        }
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
            Ok(crate::machine::runtime::render_join_template(*command))
        }
        PloyzctlCommand::IngressConfigure(command) => {
            crate::ingress::runtime::configure(command, config).await
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
        PloyzctlCommand::MachineStoragePrepare(command) => {
            crate::machine::runtime::storage_prepare(command, config).await
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
            crate::service::runtime::list(command, config).await
        }
        PloyzctlCommand::VolumeList(command) => crate::volume::runtime::list(command, config).await,
        PloyzctlCommand::VolumeCreate(command) => {
            crate::volume::runtime::create(command, config).await
        }
        PloyzctlCommand::ServiceInspect(command) => {
            crate::service::runtime::inspect(command, config).await
        }
        PloyzctlCommand::ServiceRestart(command) => {
            crate::service::runtime::restart(command, config).await
        }
        PloyzctlCommand::NamespaceRemove(command) => {
            crate::namespace::runtime::remove(command, config).await
        }
        PloyzctlCommand::VolumeRemove(command) => {
            crate::volume::runtime::remove(command, config).await
        }
        PloyzctlCommand::LogsTail(command) => crate::logs::runtime::tail(command, config).await,
        PloyzctlCommand::OpsStatus(command) => {
            crate::operation::runtime::status(command, config).await
        }
        PloyzctlCommand::OpsList(command) => crate::operation::runtime::list(command, config).await,
        PloyzctlCommand::OpsWatch(command) => {
            crate::operation::runtime::watch(command, config).await
        }
    }
}
