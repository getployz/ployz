//! Install an Upgrade's release over the running one, with Replacement Compensation: put the
//! previous release back when the new one does not stay ready.

use std::{fs, os::unix::fs::MetadataExt, path::Path, time::Duration};

use ployz_core::{MachineRelease, MachineUpgradeStage, MachineVersion};
use thiserror::Error;

use super::{Error as InstallError, InstallMode, InstallPaths, InstallRequest, ReleaseSource};
use crate::mutation;

/// How long a ready daemon must keep running before an Upgrade succeeds.
#[cfg(not(test))]
const SOAK: Duration = Duration::from_secs(30);
#[cfg(test)]
const SOAK: Duration = Duration::from_millis(100);

/// Units besides the daemon that an Upgrade restarts and compensation must bring back.
const PLUGIN_UNITS: [&str; 2] = ["ployz-volume-plugin.socket", "ployz-volume-plugin.service"];

/// Why an Upgrade failed, and whether the previous release went back into service. Its text is
/// the attempt's recorded failure.
#[derive(Debug, Error)]
pub enum UpgradeFailure {
    /// Nothing this attempt swapped needed putting back.
    #[error("{0}")]
    NotRestored(#[source] InstallError),
    /// The previous release is running again.
    #[error("{error}; restored {previous}")]
    Restored {
        #[source]
        error: InstallError,
        previous: MachineVersion,
    },
    /// Putting the previous release back failed too.
    #[error("{error}; restore failed: {restore}")]
    RestoreFailed {
        #[source]
        error: InstallError,
        restore: InstallError,
    },
}

/// Install `target` over the running release. A failure after the swap compensates: it puts the
/// previous release back and restarts the units that were active before the attempt.
pub(super) async fn install_or_compensate(
    source: &ReleaseSource,
    paths: &InstallPaths,
    guard: &mutation::InstallationGuard,
    target: &MachineVersion,
    progress: impl FnMut(MachineUpgradeStage) -> Result<(), InstallError>,
) -> Result<(), UpgradeFailure> {
    let daemon = paths.daemon();
    let before = file_identity(&daemon);
    let previous = super::release::installed_release(&daemon)
        .await
        .map_err(UpgradeFailure::NotRestored)?;
    // A unit that crashes on the new release is no longer running, so `try-restart` would leave
    // it stopped after the restore.
    let active_plugins: Vec<&str> = PLUGIN_UNITS
        .into_iter()
        .filter(|unit| super::systemctl("check unit", ["is-active", "--quiet", unit]).is_ok())
        .collect();
    let request = InstallRequest {
        release: MachineRelease::Exact(target.clone()),
        mode: InstallMode::SoftwareOnly,
    };
    let Err(error) = async {
        super::install_locked(source, request, paths, guard, progress).await?;
        soak(paths, target).await
    }
    .await
    else {
        return Ok(());
    };
    // Only a swap this attempt made is undone, and only within one release line, where the
    // older daemon still reads everything the newer one wrote.
    let Some(previous) = previous
        .filter(|previous| previous.major() == target.major() && file_identity(&daemon) != before)
    else {
        return Err(UpgradeFailure::NotRestored(error));
    };
    Err(match restore(paths, &previous, &active_plugins).await {
        Ok(()) => UpgradeFailure::Restored { error, previous },
        Err(restore) => UpgradeFailure::RestoreFailed { error, restore },
    })
}

/// Hold a ready daemon for [`SOAK`]: a crash restarts it under another main PID.
async fn soak(paths: &InstallPaths, target: &MachineVersion) -> Result<(), InstallError> {
    let started = super::host::daemon_main_pid()?;
    tokio::time::sleep(SOAK).await;
    let running = super::host::daemon_main_pid()?;
    if running != started {
        return Err(super::refuse(
            "soak daemon",
            format!("ployz.service restarted (main PID {started} became {running})"),
        ));
    }
    super::host::verify_daemon_contract(&paths.run_dir.join("ployz.sock"), target).await
}

/// Put the retained previous daemon back into service, restart `plugins`, and prove it ready.
async fn restore(
    paths: &InstallPaths,
    previous: &MachineVersion,
    plugins: &[&str],
) -> Result<(), InstallError> {
    super::release::restore_previous(paths)?;
    // A crash loop exhausts a unit's start limit, which would refuse the restart. `reset-failed`
    // fails on a unit that is not loaded, so it only names units known to be.
    super::systemctl(
        "clear daemon failure",
        ["reset-failed", "ployz.socket", "ployz.service"],
    )?;
    for unit in plugins {
        super::systemctl("clear volume plugin failure", ["reset-failed", unit])?;
    }
    super::systemctl(
        "restart daemon",
        ["restart", "ployz.socket", "ployz.service"],
    )?;
    for unit in plugins {
        super::systemctl("restart volume plugin", ["restart", unit])?;
    }
    super::host::verify_running_daemon(paths, previous).await
}

/// Which file `path` names, so a later rename over it shows.
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}
