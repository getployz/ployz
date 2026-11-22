//! Runs the host programs the volume plugin and the send server drive: `zfs`, `zpool`,
//! `fallocate` and `stat`.

use std::{io, path::Path, process::Output};

use tokio::process::Command;

/// `program` with `args`, retried while exec reports ETXTBSY.
pub(crate) async fn run(program: &Path, args: &[&str]) -> io::Result<Output> {
    let mut attempt = 0;
    loop {
        match Command::new(program).args(args).output().await {
            // ETXTBSY means exec never started; only that launch error is safe to retry.
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy && attempt < 4 => {
                tokio::time::sleep(std::time::Duration::from_millis(10 << attempt)).await;
                attempt += 1;
            }
            output => return output,
        }
    }
}

/// What an exit 0 proves.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Exit {
    /// The exit status alone decides.
    Status,
    /// A ZFS listing an absence is judged from. ZFS exits 0 when it stopped iterating on
    /// an error and says so only on stderr, so any stderr fails it.
    Listing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Ran {
    Succeeded,
    Failed,
    /// Exit 0 with stderr, under [`Exit::Listing`].
    StoppedEarly,
}

impl Exit {
    pub(crate) fn judge(self, output: &Output) -> Ran {
        if !output.status.success() {
            Ran::Failed
        } else if matches!(self, Self::Listing) && !output.stderr.trim_ascii().is_empty() {
            Ran::StoppedEarly
        } else {
            Ran::Succeeded
        }
    }
}
