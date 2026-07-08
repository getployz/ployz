use std::process::ExitCode;

use ployz_keeper::cli::KeeperCoreDemote;
use ployz_keeper::command::SystemKeeperCommandRunner;
use ployz_keeper::core_demote::{CoreDemoteTarget, demote_local_core};

pub(crate) fn run_core_demote_command(demote: KeeperCoreDemote) -> ExitCode {
    let mut runner = SystemKeeperCommandRunner::default();
    let target = CoreDemoteTarget::new(demote.successor_nats_url);
    match demote_local_core(&target, &mut runner) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ployz-keeper core-demote failed: {error}");
            ExitCode::FAILURE
        }
    }
}
