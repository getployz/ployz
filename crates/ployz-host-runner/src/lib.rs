#![forbid(unsafe_code)]

//! Machine-local Host Runner substrate bootstrap installer.
//!
//! The Host Runner owns local artifact installation, supervisor unit planning,
//! and join material storage. It does not own product truth.

pub mod cli;
mod cloud_client;
mod env_config;
mod execution;
mod lifecycle;
mod plan;
mod recovery;
mod release_manifest;
mod runtime;

#[cfg(test)]
mod tests;

use std::process::ExitCode;

use cli::HostRunnerCommand;
use execution::{
    HostRunnerCommandRunner, PoolSelection, SystemHostRunnerCommandRunner, destroy_dataset,
    ensure_dataset, gather_dataset_facts, gather_pool_capacity, observe_storage_capability,
    prepare_storage_for_operation,
};

const HOST_RUNNER_STATE_DIRECTORY: &str = "/var/lib/ployz";
const DOCKER_SYSTEMD_DROP_IN_DIRECTORY: &str = "/etc/systemd/system/docker.service.d";

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
        HostRunnerCommand::StoragePrepare(prepare) => run_storage_effect(|| {
            let mut runner = SystemHostRunnerCommandRunner::default();
            let os_release = runner
                .read_os_release()
                .map_err(|error| error.to_string())?;
            let profile =
                execution::detect_host_platform(&os_release).map_err(|error| error.to_string())?;
            let selection = prepare
                .pool
                .map_or(PoolSelection::Automatic, PoolSelection::Explicit);
            prepare_storage_for_operation(
                &mut runner,
                &profile,
                &prepare.operation_id,
                &selection,
                std::path::Path::new(HOST_RUNNER_STATE_DIRECTORY),
                std::path::Path::new(DOCKER_SYSTEMD_DROP_IN_DIRECTORY),
            )
            .map_err(|error| error.to_string())
        }),
        HostRunnerCommand::StorageCapability => run_storage_effect(|| {
            observe_storage_capability(
                &mut SystemHostRunnerCommandRunner::default(),
                std::path::Path::new(HOST_RUNNER_STATE_DIRECTORY),
                std::path::Path::new("/sys/module/zfs"),
            )
        }),
        HostRunnerCommand::StoragePoolFacts => run_storage_effect(|| {
            gather_pool_capacity(
                &mut SystemHostRunnerCommandRunner::default(),
                std::path::Path::new(HOST_RUNNER_STATE_DIRECTORY),
            )
        }),
        HostRunnerCommand::StorageDatasetEnsure(effect) => run_typed_storage_effect(|| {
            ensure_dataset(
                &mut SystemHostRunnerCommandRunner::default(),
                std::path::Path::new(HOST_RUNNER_STATE_DIRECTORY),
                &effect.dataset,
                effect.quota,
            )
            .map(|()| serde_json::Value::Null)
        }),
        HostRunnerCommand::StorageDatasetFacts(dataset) => run_storage_effect(|| {
            gather_dataset_facts(
                &mut SystemHostRunnerCommandRunner::default(),
                std::path::Path::new(HOST_RUNNER_STATE_DIRECTORY),
                &dataset,
            )
        }),
        HostRunnerCommand::StorageDatasetDestroy(dataset) => run_typed_storage_effect(|| {
            destroy_dataset(
                &mut SystemHostRunnerCommandRunner::default(),
                std::path::Path::new(HOST_RUNNER_STATE_DIRECTORY),
                &dataset,
            )
            .map(|()| serde_json::Value::Null)
        }),
    }
}

fn run_typed_storage_effect<T: serde::Serialize>(
    effect: impl FnOnce() -> Result<T, ployz_core::storage::StorageEffectFailure>,
) -> ExitCode {
    match effect() {
        Ok(value) => match serde_json::to_string(&value) {
            Ok(value) => {
                println!("{value}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("failed to encode storage result: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            match serde_json::to_string(&error) {
                Ok(error) => eprintln!("{error}"),
                Err(encode_error) => eprintln!("failed to encode storage failure: {encode_error}"),
            }
            ExitCode::FAILURE
        }
    }
}

fn run_storage_effect<T: serde::Serialize, E: std::fmt::Display>(
    effect: impl FnOnce() -> Result<T, E>,
) -> ExitCode {
    match effect() {
        Ok(value) => match serde_json::to_string(&value) {
            Ok(value) => {
                println!("{value}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("failed to encode storage result: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("storage effect failed: {error}");
            ExitCode::FAILURE
        }
    }
}
