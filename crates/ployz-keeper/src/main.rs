use std::process::ExitCode;

use ployz_keeper::cli::{KeeperCliError, KeeperCommand, load_command};
use ployz_keeper::executor::{KeeperPlanFailure, KeeperPlanTerminal, execute_keeper_plan};
use ployz_keeper::local::{KeeperLocalConfig, KeeperLocalEffects, SystemKeeperCommandRunner};
use ployz_keeper::report::KeeperTextRecorder;
use ployz_keeper::steps::{FirstNodeInstallTarget, first_node_install_plan};

fn main() -> ExitCode {
    match load_command(std::env::args_os().skip(1)) {
        Ok(KeeperCommand::Start(startup)) => {
            if startup.join_token.is_some() {
                println!("ployz-keeper started with bootstrap join material");
            } else {
                println!("ployz-keeper started");
            }
            ExitCode::SUCCESS
        }
        Ok(KeeperCommand::FirstNodeInstall(target)) => {
            let stdout = std::io::stdout();
            let mut recorder = KeeperTextRecorder::new(stdout.lock());
            let execution = run_first_node_install(target, &mut recorder);
            match execution.terminal {
                KeeperPlanTerminal::Completed => ExitCode::SUCCESS,
                KeeperPlanTerminal::Failed(failure) => {
                    eprintln!(
                        "ployz-keeper first-node-install failed: {}",
                        failure_summary(&failure)
                    );
                    ExitCode::FAILURE
                }
            }
        }
        Err(KeeperCliError::HelpRequested) => {
            println!("{}", KeeperCliError::HelpRequested);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run_first_node_install(
    target: FirstNodeInstallTarget,
    recorder: &mut impl ployz_keeper::executor::KeeperStepRecorder,
) -> ployz_keeper::executor::KeeperPlanExecution {
    let plan = first_node_install_plan(target);
    let mut effects = KeeperLocalEffects::new(
        KeeperLocalConfig {
            systemd_dir: "/etc/systemd/system".into(),
            state_dir: "/var/lib/ployz".into(),
        },
        SystemKeeperCommandRunner::default(),
    );
    execute_keeper_plan(&plan, &mut effects, recorder)
}

fn failure_summary(failure: &KeeperPlanFailure) -> &str {
    match failure {
        KeeperPlanFailure::Step(step) => step.message.as_str(),
        KeeperPlanFailure::Record(record) => record.message.as_str(),
    }
}
