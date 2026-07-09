use std::process::ExitCode;

use crate::cli::HostRunnerCoreDemote;
use crate::command::SystemHostRunnerCommandRunner;
use crate::core_demote::{CoreDemoteTarget, demote_local_core};

pub(crate) fn run_core_demote_command(demote: HostRunnerCoreDemote) -> ExitCode {
    let mut runner = SystemHostRunnerCommandRunner::default();
    let target = CoreDemoteTarget::new(demote.successor_nats_url);
    match demote_local_core(&target, &mut runner) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ployz host core-demote failed: {error}");
            ExitCode::FAILURE
        }
    }
}
