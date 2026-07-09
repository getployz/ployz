use std::process::ExitCode;

use crate::cli::{HostRunnerBootstrap, HostRunnerBootstrapMode};
use crate::executor::HostRunnerPlanTerminal;
use crate::report::HostRunnerTextRecorder;
use crate::steps::JoinToken;

use crate::cloud_bootstrap_runner;
use crate::env_config::local_core_target_from_env;
use crate::first_machine::{print_first_machine_bootstrap_result, run_first_machine_install};
use crate::join_client::{CloudJoinTokenConsumer, run_join_with_consumer};
use crate::runtime::failure_summary;

pub(crate) fn run_bootstrap_command(bootstrap: HostRunnerBootstrap) -> ExitCode {
    match bootstrap.mode {
        HostRunnerBootstrapMode::LocalGuidance => {
            eprintln!("Use local CLI setup from your workstation:");
            eprintln!("  ployz machine init USER@HOST");
            eprintln!("Or opt in to Cloud:");
            eprintln!("  sudo ployz host bootstrap cloud");
            ExitCode::FAILURE
        }
        HostRunnerBootstrapMode::Cloud { cloud_host } => {
            let host = choose_cloud_host(cloud_host);
            cloud_bootstrap_runner::run_interactive_cloud_bootstrap(host.as_str())
        }
        HostRunnerBootstrapMode::Core => run_local_core_bootstrap(),
        HostRunnerBootstrapMode::Join { join_token } => run_bootstrap_join(&join_token),
    }
}

fn choose_cloud_host(cloud_host: Option<crate::cli::CloudHost>) -> crate::cli::CloudHost {
    cloud_host.unwrap_or_default()
}

fn run_local_core_bootstrap() -> ExitCode {
    let target = match local_core_target_from_env() {
        Ok(target) => target,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let machine_id = target.machine_id.clone();
    let nats_material = target.nats_material.clone();
    let runtime_nats_url = target.machine_join_runtime_nats_url().clone();
    let stdout = std::io::stdout();
    let mut recorder = HostRunnerTextRecorder::new(stdout.lock());
    let execution = run_first_machine_install(target, &mut recorder);
    match execution.terminal {
        HostRunnerPlanTerminal::Completed => {
            drop(recorder);
            match print_first_machine_bootstrap_result(
                &machine_id,
                &runtime_nats_url,
                &nats_material,
            ) {
                Ok(()) => ExitCode::SUCCESS,
                Err(message) => {
                    eprintln!("{message}");
                    ExitCode::FAILURE
                }
            }
        }
        HostRunnerPlanTerminal::Failed(failure) => {
            eprintln!(
                "ployz host bootstrap core failed: {}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
    }
}

fn run_bootstrap_join(join_token: &JoinToken) -> ExitCode {
    let stdout = std::io::stdout();
    let mut recorder = HostRunnerTextRecorder::new(stdout.lock());
    let execution = run_join_with_consumer(join_token, CloudJoinTokenConsumer, &mut recorder);
    match execution.terminal {
        HostRunnerPlanTerminal::Completed => ExitCode::SUCCESS,
        HostRunnerPlanTerminal::Failed(failure) => {
            eprintln!(
                "ployz host bootstrap join failed: {}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
    }
}
