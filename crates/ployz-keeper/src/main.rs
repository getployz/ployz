use std::process::ExitCode;

use ployz_keeper::cli::{KeeperCommand, load_command};

mod bootstrap;
mod cloud_bootstrap_runner;
mod core_demote_command;
mod core_promote;
mod env_config;
mod first_machine;
mod join_client;
mod runtime;
mod substrate_update;

fn main() -> ExitCode {
    match load_command(std::env::args_os().skip(1)) {
        Ok(KeeperCommand::Start(startup)) => join_client::run_start_command(startup),
        Ok(KeeperCommand::Bootstrap(bootstrap)) => bootstrap::run_bootstrap_command(bootstrap),
        Ok(KeeperCommand::SubstrateUpdate(update)) => {
            substrate_update::run_substrate_update_command(update)
        }
        Ok(KeeperCommand::FirstMachineInstall(target)) => {
            first_machine::run_first_machine_install_command(*target)
        }
        Ok(KeeperCommand::CorePromote(promote)) => core_promote::run_core_promote_command(promote),
        Ok(KeeperCommand::CoreDemote(demote)) => {
            core_demote_command::run_core_demote_command(demote)
        }
        Err(error) if error.is_help_requested() => {
            print!("{error}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
