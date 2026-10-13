//! Units the daemon writes under `/run/systemd/system` on every start and keeps
//! on its own binary, so a release's processes never depend on what an older
//! installer left in `/etc`.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use tokio::process::Command;

use crate::{
    dns::{InstalledExe, identity_of},
    filesystem::atomic_write,
};

pub(crate) const UNIT_DIR: &str = "/run/systemd/system";
const SYSTEMCTL_BOUND: Duration = Duration::from_secs(5);
const HOST_UNIT_NAME: &str = "ployz.service";

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
        .any(|unit| unit.trim_end() == HOST_UNIT_NAME)
}

/// Write `text` to `file` and reload systemd, only when the file differs.
/// Answers whether it did, so the caller can restart a unit still running the
/// old definition.
///
/// # Errors
///
/// Returns an error when the unit cannot be written or systemd does not
/// reload. A failed reload removes the file so the next start retries it.
pub(crate) async fn install(file: &Path, text: &str) -> io::Result<bool> {
    if fs::read_to_string(file).ok().as_deref() == Some(text) {
        return Ok(false);
    }
    atomic_write(file, text.as_bytes(), 0o644)?;
    if systemctl(&["daemon-reload"]).await.is_none() {
        let _ = fs::remove_file(file);
        return Err(io::Error::other(format!(
            "systemd did not reload after writing {}",
            file.display()
        )));
    }
    Ok(true)
}

/// Whether `unit`'s main process runs a binary other than `own`, as it does
/// after the binary was swapped beneath it.
pub(crate) async fn runs_other_software(unit: &str, own: &InstalledExe) -> bool {
    let Some(output) = systemctl(&["show", "-p", "MainPID", "--value", unit]).await else {
        return false;
    };
    let pid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if pid.is_empty() || pid == "0" {
        return false;
    }
    let theirs = identity_of(&PathBuf::from(format!("/proc/{pid}/exe"))).ok();
    restart_needed(theirs, own.identity())
}

pub(crate) fn restart_needed(theirs: Option<(u64, u64)>, ours: Option<(u64, u64)>) -> bool {
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
    fn systemd_supervision_means_running_as_the_host_unit() {
        assert!(runs_as_host_unit("0::/system.slice/ployz.service\n"));
        assert!(!runs_as_host_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/t3code.service\n"
        ));
        assert!(!runs_as_host_unit("0::/system.slice/ployz-dns.service\n"));
        assert!(!runs_as_host_unit(""));
    }
}
