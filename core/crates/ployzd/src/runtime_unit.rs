//! Units the daemon writes under `/run/systemd/system` on every start and keeps
//! on its own binary, so a release's processes never depend on what an older
//! installer left in `/etc`.

use std::{fs, io, path::Path, process::Stdio, time::Duration};

use tokio::process::Command;

use crate::{filesystem::atomic_write, units::Unit};

pub(crate) mod process_identity;

use process_identity::{Identity, InstalledExe, UnitProcess};

pub(crate) const UNIT_DIR: &str = "/run/systemd/system";
const SYSTEMCTL_BOUND: Duration = Duration::from_secs(5);

/// Whether the daemon writes its runtime units and drives them with `systemctl`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitSupervision {
    /// The daemon runs as `ployz.service` and owns `ployz-dns.service` and
    /// `ployz-observe.service`.
    Systemd,
    /// Something else runs `ployzd dns` and `ployzd observe`; the daemon only
    /// publishes the DNS spec.
    External,
}

impl UnitSupervision {
    /// Systemd only when this process runs as `ployz.service`. `NOTIFY_SOCKET`
    /// and `INVOCATION_ID` are inherited by anything a unit starts, tests
    /// included, so the cgroup decides.
    #[must_use]
    pub fn detect() -> Self {
        let cgroup = fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
        if runs_as_host_unit(&cgroup) {
            Self::Systemd
        } else {
            Self::External
        }
    }
}

fn runs_as_host_unit(cgroup: &str) -> bool {
    cgroup
        .lines()
        .filter_map(|line| line.rsplit('/').next())
        .any(|unit| unit.trim_end() == Unit::Ployz.name())
}

/// Write `text` to `file` and reload systemd when its loaded copy of the unit
/// is missing or older than the file. Answers whether it reloaded, so the
/// caller can restart a process still running the old definition.
///
/// The file is the record: a reload that fails or is cancelled leaves systemd
/// stale, and the next call reloads again.
///
/// # Errors
///
/// Returns an error when the unit cannot be written or systemd does not
/// reload.
pub(crate) async fn install(file: &Path, text: &str) -> io::Result<bool> {
    if fs::read_to_string(file).ok().as_deref() != Some(text) {
        atomic_write(file, text.as_bytes(), 0o644)?;
    }
    let unit = file
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other(format!("{} names no unit", file.display())))?;
    if !loaded_copy_is_stale(unit).await {
        return Ok(false);
    }
    systemctl(&["daemon-reload"])
        .await
        .ok_or_else(|| io::Error::other(format!("systemd did not reload {unit}")))?;
    Ok(true)
}

async fn loaded_copy_is_stale(unit: &str) -> bool {
    let Some(output) =
        systemctl(&["show", "-p", "LoadState", "-p", "NeedDaemonReload", unit]).await
    else {
        return true;
    };
    String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        line == "NeedDaemonReload=yes"
            || line
                .strip_prefix("LoadState=")
                .is_some_and(|state| state != "loaded")
    })
}

/// Whether `unit`'s main process runs a binary other than `own`, as it does
/// after the binary was swapped beneath it.
///
/// Only proof of other software counts: a failed `systemctl`, no process and an
/// unreadable process all leave the unit as it is.
pub(crate) async fn runs_other_software(unit: &str, own: &InstalledExe) -> bool {
    other_software_runs(UnitProcess::of(unit).await, own.identity())
}

fn other_software_runs(process: UnitProcess, ours: Option<Identity>) -> bool {
    match process {
        UnitProcess::Running(theirs) => restart_needed(Some(theirs), ours),
        UnitProcess::NoProcess | UnitProcess::Unknown => false,
    }
}

pub(crate) fn restart_needed(theirs: Option<Identity>, ours: Option<Identity>) -> bool {
    matches!((theirs, ours), (Some(theirs), Some(ours)) if theirs != ours)
}

pub(crate) async fn systemctl(args: &[&str]) -> Option<std::process::Output> {
    let run = Command::new("systemctl")
        .args(args)
        .env_remove("NOTIFY_SOCKET")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(SYSTEMCTL_BOUND, run).await {
        Ok(Ok(output)) if output.status.success() => Some(output),
        Ok(Ok(output)) => {
            eprintln!(
                "systemctl {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
            None
        }
        Ok(Err(error)) => {
            eprintln!("systemctl {} could not run: {error}", args.join(" "));
            None
        }
        Err(_) => {
            eprintln!(
                "systemctl {} did not finish within {SYSTEMCTL_BOUND:?}",
                args.join(" ")
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_needed_only_for_other_software() {
        assert!(restart_needed(Some((1, 2)), Some((1, 3))));
        assert!(!restart_needed(Some((1, 2)), Some((1, 2))));
        assert!(!restart_needed(None, Some((1, 2))));
        assert!(!restart_needed(Some((1, 2)), None));
    }

    #[test]
    fn only_proof_of_other_software_restarts_a_unit() {
        for (process, ours, restarts) in [
            (UnitProcess::Running((1, 2)), Some((1, 3)), true),
            (UnitProcess::Running((1, 2)), Some((1, 2)), false),
            (UnitProcess::Running((1, 2)), None, false),
            (UnitProcess::NoProcess, Some((1, 2)), false),
            (UnitProcess::Unknown, Some((1, 2)), false),
        ] {
            assert_eq!(
                other_software_runs(process, ours),
                restarts,
                "{process:?} {ours:?}"
            );
        }
    }

    #[test]
    fn systemd_supervision_means_running_as_the_host_unit() {
        assert!(runs_as_host_unit("0::/system.slice/ployz.service\n"));
        assert!(!runs_as_host_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/t3code.service\n"
        ));
        assert!(!runs_as_host_unit("0::/system.slice/ployz-dns.service\n"));
        assert!(!runs_as_host_unit(""));
    }
}
