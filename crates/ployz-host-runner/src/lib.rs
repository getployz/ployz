#![forbid(unsafe_code)]

//! Machine-local Host Runner substrate bootstrap installer.
//!
//! The Host Runner owns local artifact installation, supervisor unit planning,
//! and join material storage. It does not own product truth.

pub mod cli;
mod cloud_client;
mod env_config;
pub mod execution;
pub mod lifecycle;
pub mod plan;
pub mod recovery;
mod release_manifest;
mod runtime;

use std::process::ExitCode;

use cli::HostRunnerCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Host Runner commands require Linux; this platform is {platform}")]
pub struct UnsupportedHostRunnerPlatform {
    platform: &'static str,
}

impl UnsupportedHostRunnerPlatform {
    #[must_use]
    pub const fn current() -> Self {
        Self {
            platform: std::env::consts::OS,
        }
    }
}

#[must_use]
pub fn run_host_runner_command(command: HostRunnerCommand) -> ExitCode {
    if std::env::consts::OS != "linux" {
        eprintln!("{}", UnsupportedHostRunnerPlatform::current());
        return ExitCode::FAILURE;
    }

    match command {
        HostRunnerCommand::Start(startup) => {
            lifecycle::machine_join::client::run_start_command(startup)
        }
        HostRunnerCommand::Bootstrap(bootstrap) => lifecycle::run_bootstrap_command(bootstrap),
        HostRunnerCommand::SubstrateUpdate(update) => {
            lifecycle::substrate_update::run_substrate_update_command(update)
        }
        HostRunnerCommand::FirstMachineInstall(target) => {
            lifecycle::founder_bootstrap::run_first_machine_install_command(*target)
        }
        HostRunnerCommand::CorePromote(promote) => recovery::run_core_promote_command(promote),
        HostRunnerCommand::CoreDemote(demote) => recovery::run_core_demote_command(demote),
    }
}
