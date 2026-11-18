//! Taking a cut root out of the namespace while something outside Docker still reads it.

use std::{
    fs,
    path::{Path, PathBuf},
};

use super::{Dataset, Result, VolumeStorage, checked_command};

impl VolumeStorage {
    /// Unmounts the root a cut is about to park in its slot.
    ///
    /// The cut has already made the root read-only and snapshotted it at the target, and
    /// the daemon stopped every container using it, so whatever still holds the mount
    /// (a backup walking `/var/lib/ployz-volumes`, a shell in the directory) cannot change
    /// what the target pulls. `zfs unmount` answers "busy" for that reader and the move
    /// would wait out the worker's retry backoff with the Service stopped, so a busy mount
    /// is detached lazily instead: the reader keeps its open files, new lookups no longer
    /// reach the dataset, and `zfs rename` into the slot proceeds. Any other failure is
    /// returned untouched.
    pub(super) async fn unmount_cut_root(&self, root: &Dataset) -> Result<()> {
        let error = match self.zfs(&["unmount", &root.name]).await {
            Ok(_) => return Ok(()),
            Err(error) if error.to_string().contains("busy") => error,
            Err(error) => return Err(error),
        };
        let mountpoint = PathBuf::from(&root.mountpoint);
        let holders =
            tokio::task::spawn_blocking(move || open_holders(Path::new("/proc"), &mountpoint))
                .await
                .unwrap_or_default();
        tracing::warn!(
            dataset = %root.name,
            %error,
            holders = %holders.join(", "),
            "cut root is busy; detaching its mount lazily"
        );
        checked_command(&self.umount, &["-l", &root.mountpoint])
            .await
            .map(drop)
            .map_err(|detach| format!("{error}; then {detach}").into())
    }
}

/// `pid (comm)` for each process under `proc_root` with a working directory or open file
/// inside `mountpoint`. Best effort: a process that exits mid-scan, or one this user cannot
/// inspect, is skipped.
fn open_holders(proc_root: &Path, mountpoint: &Path) -> Vec<String> {
    let Ok(processes) = fs::read_dir(proc_root) else {
        return Vec::new();
    };
    let mut holders = Vec::new();
    for process in processes.flatten() {
        let Some(pid) = process
            .file_name()
            .to_str()
            .filter(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
            .map(str::to_owned)
        else {
            continue;
        };
        let dir = process.path();
        let cwd = fs::read_link(dir.join("cwd")).ok();
        let descriptors = fs::read_dir(dir.join("fd"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|descriptor| fs::read_link(descriptor.path()).ok());
        if cwd
            .into_iter()
            .chain(descriptors)
            .any(|path| path.starts_with(mountpoint))
        {
            let comm = fs::read_to_string(dir.join("comm")).unwrap_or_default();
            holders.push(format!("{pid} ({})", comm.trim()));
        }
    }
    holders
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn open_holders_names_processes_inside_the_mountpoint_only() {
        let proc = std::env::temp_dir().join(format!("ployzd-holders-{}", std::process::id()));
        let _ = fs::remove_dir_all(&proc);
        for (pid, comm, cwd, file) in [
            ("41", "tar", "/", "/var/lib/ployz-volumes/db/PG_VERSION"),
            ("42", "bash", "/var/lib/ployz-volumes/db", "/dev/null"),
            ("43", "sleep", "/", "/var/lib/ployz-volumes/dbx/other"),
        ] {
            let dir = proc.join(pid);
            fs::create_dir_all(dir.join("fd")).unwrap();
            fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
            symlink(cwd, dir.join("cwd")).unwrap();
            symlink(file, dir.join("fd/3")).unwrap();
        }
        fs::write(proc.join("meminfo"), "").unwrap();

        let mut holders = open_holders(&proc, Path::new("/var/lib/ployz-volumes/db"));
        holders.sort();
        let _ = fs::remove_dir_all(&proc);

        assert_eq!(holders, ["41 (tar)", "42 (bash)"]);
    }
}
