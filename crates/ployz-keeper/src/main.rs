use std::process::ExitCode;

use ployz_core::ops::FailureMessage;
use ployz_keeper::cli::{KeeperCliError, KeeperCommand, load_command};
use ployz_keeper::executor::{KeeperPlanFailure, KeeperPlanTerminal, execute_keeper_plan};
use ployz_keeper::join_executor::{
    KeeperJoinRedeemer, KeeperJoinTokenConsumer, execute_keeper_join,
};
use ployz_keeper::local::{KeeperLocalConfig, KeeperLocalEffects, SystemKeeperCommandRunner};
use ployz_keeper::report::KeeperTextRecorder;
use ployz_keeper::steps::{
    FirstNodeInstallTarget, JoinToken, KeeperJoinTarget, first_node_install_plan,
};

fn main() -> ExitCode {
    match load_command(std::env::args_os().skip(1)) {
        Ok(KeeperCommand::Start(startup)) => {
            if let Some(join) = &startup.join {
                let stdout = std::io::stdout();
                let mut recorder = KeeperTextRecorder::new(stdout.lock());
                let execution = run_join(&join.token, join.file.clone(), &mut recorder);
                match execution.terminal {
                    KeeperPlanTerminal::Completed => ExitCode::SUCCESS,
                    KeeperPlanTerminal::Failed(failure) => {
                        eprintln!("ployz-keeper join failed: {}", failure_summary(&failure));
                        ExitCode::FAILURE
                    }
                }
            } else {
                println!("ployz-keeper started");
                ExitCode::SUCCESS
            }
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

fn run_join(
    token: &JoinToken,
    join_token_file: std::path::PathBuf,
    recorder: &mut impl ployz_keeper::executor::KeeperStepRecorder,
) -> ployz_keeper::executor::KeeperPlanExecution {
    let mut redeemer = SystemJoinRedeemer;
    let mut token_consumer = StartupJoinTokenConsumer { join_token_file };
    let mut effects = KeeperLocalEffects::new(
        KeeperLocalConfig {
            systemd_dir: "/etc/systemd/system".into(),
            state_dir: "/var/lib/ployz".into(),
        },
        SystemKeeperCommandRunner::default(),
    );
    execute_keeper_join(
        token,
        &mut redeemer,
        &mut token_consumer,
        &mut effects,
        recorder,
    )
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

struct SystemJoinRedeemer;

impl KeeperJoinRedeemer for SystemJoinRedeemer {
    fn redeem_join_token(
        &mut self,
        _token: &JoinToken,
    ) -> Result<KeeperJoinTarget, FailureMessage> {
        Err(
            FailureMessage::try_new("join token redemption is not wired to NATS yet")
                .expect("static failure message is non-empty"),
        )
    }
}

struct StartupJoinTokenConsumer {
    join_token_file: std::path::PathBuf,
}

impl KeeperJoinTokenConsumer for StartupJoinTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage> {
        ployz_keeper::join::remove_join_token_file(&self.join_token_file).map_err(|error| {
            FailureMessage::try_new(error.to_string()).expect("join token file error is non-empty")
        })
    }
}
