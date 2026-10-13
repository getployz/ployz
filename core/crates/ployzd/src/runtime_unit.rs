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

/// Write `text` to `file` and reload systemd, only when the file differs.
///
/// # Errors
///
/// Returns an error when the unit cannot be written.
pub(crate) async fn install(file: &Path, text: &str) -> io::Result<()> {
    if fs::read_to_string(file).ok().as_deref() != Some(text) {
        atomic_write(file, text.as_bytes(), 0o644)?;
        systemctl(&["daemon-reload"]).await;
    }
    Ok(())
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
}
