//! Joiner Bootstrap delivery over an existing machine-add operation.

use std::process::ExitCode;

use crate::plan::{HostRunnerPlanTerminal, HostRunnerTextRecorder, JoinToken};
use crate::runtime::failure_summary;

use super::machine_join::client::{CloudJoinTokenConsumer, run_join_with_consumer};

pub(crate) fn run(join_token: &JoinToken) -> ExitCode {
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
