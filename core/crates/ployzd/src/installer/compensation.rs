//! Install an Upgrade's release over the running one, with Replacement Compensation: put the
//! previous release back when the new one does not stay ready.

use std::{fs, os::unix::fs::MetadataExt, path::Path, time::Duration};

use ployz_core::{MachineRelease, MachineUpgradeStage, MachineVersion, error_chain::inline};
use thiserror::Error;

use super::{Error as InstallError, InstallMode, InstallPaths, InstallRequest, ReleaseSource};
use crate::mutation;

/// How long a ready daemon must keep running before an Upgrade succeeds.
#[cfg(not(test))]
const SOAK: Duration = Duration::from_secs(30);
#[cfg(test)]
const SOAK: Duration = Duration::from_millis(100);

const OBSERVER_UNIT: &str = "ployz-observe.service";

/// Units besides the daemon that an Upgrade restarts and compensation must bring back.
const HELPER_UNITS: [&str; 3] = [
    "ployz-volume-plugin.socket",
    "ployz-volume-plugin.service",
    OBSERVER_UNIT,
];

/// Why an Upgrade failed, and whether the previous release went back into service. Its text is
/// the attempt's recorded failure.
#[derive(Debug, Error)]
pub enum UpgradeFailure {
    /// Nothing this attempt swapped needed putting back.
    #[error(transparent)]
    NotRestored(InstallError),
    /// The previous release is running again.
    #[error("{}; restored {previous}", inline(.error))]
    Restored {
        error: InstallError,
        previous: MachineVersion,
    },
    /// Putting the previous release back failed too.
    #[error("{}; restore failed: {}", inline(.error), inline(.restore))]
    RestoreFailed {
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
    let active_plugins: Vec<&str> = HELPER_UNITS
        .into_iter()
        .filter(|unit| super::systemctl("check unit", ["is-active", "--quiet", unit]).is_ok())
        .collect();
    let observer_state = super::systemctl(
        "check observer enablement",
        ["show", "--property=UnitFileState", "--value", OBSERVER_UNIT],
    )
    .map_err(UpgradeFailure::NotRestored)?;
    let observer_enabled = match observer_state.stdout.trim_ascii() {
        b"enabled" | b"enabled-runtime" => true,
        b"disabled" | b"" => false,
        state => {
            return Err(UpgradeFailure::NotRestored(super::refuse(
                "check observer enablement",
                format!(
                    "unknown unit file state {:?}",
                    String::from_utf8_lossy(state)
                ),
            )));
        }
    };
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
    Err(
        match restore(paths, &previous, &active_plugins, observer_enabled).await {
            Ok(()) => UpgradeFailure::Restored { error, previous },
            Err(restore) => UpgradeFailure::RestoreFailed { error, restore },
        },
    )
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
    observer_enabled: bool,
) -> Result<(), InstallError> {
    let observer = super::systemctl(
        "check observer unit",
        ["show", "--property=LoadState", "--value", OBSERVER_UNIT],
    )?;
    if observer.stdout.trim_ascii() != b"not-found" {
        if !plugins.contains(&OBSERVER_UNIT) {
            super::systemctl("stop new observer", ["stop", OBSERVER_UNIT])?;
        }
        if !observer_enabled {
            super::systemctl("restore observer enablement", ["disable", OBSERVER_UNIT])?;
        }
    }
    super::release::restore_previous(paths)?;
    // A crash loop exhausts a unit's start limit, which would refuse the restart. `reset-failed`
    // fails on a unit that is not loaded, so it only names units known to be.
    super::systemctl(
        "clear daemon failure",
        ["reset-failed", "ployz.socket", "ployz.service"],
    )?;
    for unit in plugins {
        super::systemctl("clear unit failure", ["reset-failed", unit])?;
    }
    super::systemctl(
        "restart daemon",
        ["restart", "ployz.socket", "ployz.service"],
    )?;
    for unit in plugins {
        super::systemctl("restart unit", ["restart", unit])?;
    }
    super::host::verify_running_daemon(paths, previous).await
}

/// Which file `path` names, so a later rename over it shows.
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}
