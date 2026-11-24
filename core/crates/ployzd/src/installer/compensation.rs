//! Install an Upgrade's release over the running one, with Replacement Compensation: put the
//! previous release back when the new one does not stay ready.

use std::{
    fs,
    io::{self, ErrorKind},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use ployz_core::{MachineRelease, MachineUpgradeStage, MachineVersion, error_chain::inline};
use thiserror::Error;

use super::{Error as InstallError, InstallMode, InstallPaths, InstallRequest, ReleaseSource};
use crate::{filesystem::atomic_write, mutation, units::Unit};

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
    let previous = super::release::installed_release(paths)
        .await
        .map_err(UpgradeFailure::NotRestored)?;
    // A unit that crashes on the new release is no longer running, so `try-restart` would leave
    // it stopped after the restore.
    let active_plugins: Vec<&str> = Unit::HELPERS
        .map(Unit::name)
        .into_iter()
        .filter(|unit| super::systemctl("check unit", ["is-active", "--quiet", unit]).is_ok())
        .collect();
    let units = UnitSnapshot::take(&paths.systemd_dir, paths.layout.data_dir())
        .map_err(UpgradeFailure::NotRestored)?;
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
        match restore(paths, &previous, &active_plugins, &units).await {
            Ok(()) => UpgradeFailure::Restored { error, previous },
            Err(restore) => UpgradeFailure::RestoreFailed { error, restore },
        },
    )
}

/// Hold a ready daemon for [`super::Pacing::soak`]: a crash restarts it under another main PID.
async fn soak(paths: &InstallPaths, target: &MachineVersion) -> Result<(), InstallError> {
    let started = super::host::daemon_main_pid()?;
    tokio::time::sleep(paths.pacing.soak).await;
    let running = super::host::daemon_main_pid()?;
    if running != started {
        return Err(super::refuse(
            "soak daemon",
            format!(
                "{} restarted (main PID {started} became {running})",
                Unit::Ployz.name()
            ),
        ));
    }
    super::host::verify_daemon_contract(&paths.socket(), target).await
}

/// The unit files `install-units` writes, and the fence file a downgrade removes, as an attempt
/// found them, each with its mode: `None` for a file that was not there. Compensation owns these
/// files, because the release that failed rewrote them.
struct UnitSnapshot(Vec<(PathBuf, Option<Vec<u8>>, u32)>);

impl UnitSnapshot {
    const WRITTEN: [Unit; 5] = [
        Unit::Ployz,
        Unit::PloyzSocket,
        Unit::VolumePluginSocket,
        Unit::VolumePluginService,
        Unit::Fence,
    ];

    fn take(systemd_dir: &Path, data_dir: &Path) -> Result<Self, InstallError> {
        Self::WRITTEN
            .into_iter()
            .map(|unit| (systemd_dir.join(unit.name()), 0o644))
            .chain([
                (super::host::docker_fence_drop_in(systemd_dir), 0o644),
                (data_dir.join(crate::network::FENCE_FILE), 0o600),
            ])
            .map(|(path, mode)| {
                let content = read_unit(&path).map_err(|source| InstallError::Io {
                    stage: "read systemd unit before the upgrade",
                    source,
                })?;
                Ok((path, content, mode))
            })
            .collect::<Result<_, InstallError>>()
            .map(Self)
    }

    /// Put back every file the attempt changed or added, trying each one even after another
    /// failed. Returns whether any file may have changed, so a `daemon-reload` runs only when
    /// systemd's view of a unit may have gone stale, and the first failure, if any.
    fn restore(&self) -> (bool, Result<(), InstallError>) {
        let mut changed = false;
        let mut failure = None;
        for (path, before, mode) in &self.0 {
            let current = match read_unit(path) {
                Ok(current) => current,
                Err(source) => {
                    failure.get_or_insert(InstallError::Io {
                        stage: "read systemd unit to restore",
                        source,
                    });
                    continue;
                }
            };
            if &current == before {
                continue;
            }
            changed = true;
            if let Err(source) = match before {
                Some(content) => atomic_write(path, content, *mode),
                None => fs::remove_file(path),
            } {
                failure.get_or_insert(InstallError::Io {
                    stage: "restore systemd unit",
                    source,
                });
            }
        }
        (changed, failure.map_or(Ok(()), Err))
    }
}

fn read_unit(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Put the retained previous daemon and its unit files back into service, restart `plugins`, and
/// prove it ready.
///
/// `ployz-uninstall` is the one file of the failed release left in place: it removes every unit
/// [`Unit`] names, and the previous release's units are a subset of the newer release's, so the
/// newer command removes at least what the restored release installed.
async fn restore(
    paths: &InstallPaths,
    previous: &MachineVersion,
    plugins: &[&str],
    units: &UnitSnapshot,
) -> Result<(), InstallError> {
    super::release::restore_previous(paths)?;
    // A unit file that would not go back, or a reload that failed, must not leave the restored
    // daemon stopped: recovery still runs, and the failure is reported once the daemon is back.
    let (changed, units_restored) = units.restore();
    let reloaded = if changed {
        super::systemctl("reload systemd units", ["daemon-reload"]).map(drop)
    } else {
        Ok(())
    };
    let units_restored = units_restored.and(reloaded);
    // A crash loop exhausts a unit's start limit, which would refuse the restart. `reset-failed`
    // fails on a unit that is not loaded, so it only names units known to be.
    super::systemctl(
        "clear daemon failure",
        ["reset-failed", Unit::PloyzSocket.name(), Unit::Ployz.name()],
    )?;
    for unit in plugins {
        super::systemctl("clear unit failure", ["reset-failed", unit])?;
    }
    super::systemctl(
        "restart daemon",
        ["restart", Unit::PloyzSocket.name(), Unit::Ployz.name()],
    )?;
    for unit in plugins {
        super::systemctl("restart unit", ["restart", unit])?;
    }
    super::host::verify_running_daemon(paths, previous).await?;
    super::host::remove_unknown_fence(paths.layout.data_dir())?;
    units_restored
}

/// Which file `path` names, so a later rename over it shows.
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_snapshot_covers_every_unit_file_install_units_writes() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        super::super::host::write_units(
            &root.path().join("bin"),
            &root.path().join("run"),
            root.path(),
        )
        .unwrap();
        let mut written = Vec::new();
        let mut directories = vec![root.path().to_path_buf()];
        while let Some(directory) = directories.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    directories.push(path);
                } else {
                    written.push(path);
                }
            }
        }
        written.push(data.path().join(crate::network::FENCE_FILE));
        written.sort();
        let snapshot = UnitSnapshot::take(root.path(), data.path()).unwrap();
        let mut snapshot: Vec<_> = snapshot.0.into_iter().map(|(path, ..)| path).collect();
        snapshot.sort();
        assert_eq!(written, snapshot);
    }

    /// A downgrade takes the fence's boot files away once the older binaries are in. When it
    /// fails after that, the release put back keeps its fence at the next boot.
    #[test]
    fn a_failed_downgrade_puts_the_fence_boot_files_back() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        super::super::host::write_units(
            &root.path().join("bin"),
            &root.path().join("run"),
            root.path(),
        )
        .unwrap();
        let fence = data.path().join(crate::network::FENCE_FILE);
        fs::write(&fence, "rules").unwrap();
        let snapshot = UnitSnapshot::take(root.path(), data.path()).unwrap();

        super::super::host::remove_fence_files(root.path(), data.path()).unwrap();
        let (_, result) = snapshot.restore();
        result.unwrap();
        assert_eq!(fs::read_to_string(&fence).unwrap(), "rules");
        assert!(root.path().join(Unit::Fence.name()).is_file());
        assert!(super::super::host::docker_fence_drop_in(root.path()).is_file());
    }

    #[test]
    fn restoring_puts_back_changed_files_and_removes_added_ones_only() {
        let root = tempfile::tempdir().unwrap();
        let kept = root.path().join(Unit::Ployz.name());
        let changed = root.path().join(Unit::PloyzSocket.name());
        let added = root.path().join(Unit::VolumePluginSocket.name());
        fs::write(&kept, "kept").unwrap();
        fs::write(&changed, "old").unwrap();
        let snapshot = UnitSnapshot::take(root.path(), root.path()).unwrap();

        let (any, result) = snapshot.restore();
        assert!(!any, "nothing changed yet");
        result.unwrap();
        fs::write(&changed, "new").unwrap();
        fs::write(&added, "added").unwrap();
        let (any, result) = snapshot.restore();
        assert!(any);
        result.unwrap();
        assert_eq!(fs::read_to_string(&kept).unwrap(), "kept");
        assert_eq!(fs::read_to_string(&changed).unwrap(), "old");
        assert!(!added.exists());
        let (any, result) = snapshot.restore();
        assert!(!any, "a restored set is stable");
        result.unwrap();
    }

    #[test]
    fn a_unit_that_will_not_go_back_does_not_stop_the_rest_and_is_reported() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join(Unit::Ployz.name());
        let stuck = root.path().join(Unit::PloyzSocket.name());
        let last = root.path().join(Unit::VolumePluginSocket.name());
        fs::write(&first, "old").unwrap();
        fs::write(&last, "old").unwrap();
        let snapshot = UnitSnapshot::take(root.path(), root.path()).unwrap();

        fs::write(&first, "new").unwrap();
        fs::create_dir(&stuck).unwrap();
        fs::write(&last, "new").unwrap();
        let (any, result) = snapshot.restore();
        assert!(any, "systemd must be told to reload what did go back");
        assert!(matches!(result, Err(InstallError::Io { .. })));
        assert_eq!(fs::read_to_string(&first).unwrap(), "old");
        assert_eq!(fs::read_to_string(&last).unwrap(), "old");
    }
}
