//! Which binary a process runs: the daemon's own, and the main process of a runtime unit.

use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use super::systemctl;

/// A file's device and inode, which change when a release swaps the binary
/// beneath a running process.
pub(crate) type Identity = (u64, u64);

/// `/proc/self/exe` reads `(deleted)` after a swap, so the path is captured at
/// start and stat'ed afterwards.
pub(crate) struct InstalledExe {
    path: PathBuf,
    identity: Option<Identity>,
}

impl InstalledExe {
    pub(crate) fn current() -> io::Result<Self> {
        Self::at(fs::read_link("/proc/self/exe")?)
    }

    pub(crate) fn at(path: PathBuf) -> io::Result<Self> {
        let identity = identity_of(&path)?;
        Ok(Self {
            path,
            identity: Some(identity),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn identity(&self) -> Option<Identity> {
        self.identity
    }

    pub(crate) fn changed(&self) -> Option<Identity> {
        let identity = identity_of(&self.path).ok()?;
        (self.identity != Some(identity)).then_some(identity)
    }

    pub(crate) fn adopt(&mut self, identity: Identity) {
        self.identity = Some(identity);
    }
}

pub(crate) fn identity_of(path: &Path) -> io::Result<Identity> {
    let metadata = fs::metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

/// What systemd says runs for a unit. Callers choose what `Unknown` means, and
/// they do not agree: a restart waits for proof of other software, an upgrade
/// gate waits for proof of the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnitProcess {
    /// `MainPID` is 0: nothing runs.
    NoProcess,
    Running(Identity),
    /// `systemctl` failed, `MainPID` was empty, or the process's binary could
    /// not be read.
    Unknown,
}

impl UnitProcess {
    pub(crate) async fn of(unit: &str) -> Self {
        let main_pid = systemctl(&["show", "-p", "MainPID", "--value", unit])
            .await
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
        Self::from_main_pid(main_pid.as_deref())
    }

    fn from_main_pid(main_pid: Option<&str>) -> Self {
        match main_pid {
            None | Some("") => Self::Unknown,
            Some("0") => Self::NoProcess,
            Some(pid) => identity_of(&PathBuf::from(format!("/proc/{pid}/exe")))
                .map_or(Self::Unknown, Self::Running),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_pid_names_what_runs() {
        let own = identity_of(Path::new("/proc/self/exe")).unwrap();
        let own_pid = std::process::id().to_string();
        for (main_pid, process) in [
            (Some(own_pid.as_str()), UnitProcess::Running(own)),
            (Some("0"), UnitProcess::NoProcess),
            (Some(""), UnitProcess::Unknown),
            (None, UnitProcess::Unknown),
            (Some("4294967295"), UnitProcess::Unknown),
            (Some("not-a-pid"), UnitProcess::Unknown),
        ] {
            assert_eq!(
                UnitProcess::from_main_pid(main_pid),
                process,
                "{main_pid:?}"
            );
        }
    }

    #[test]
    fn an_installed_exe_changes_only_when_its_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exe");
        fs::write(&path, b"one").unwrap();
        let mut exe = InstalledExe::at(path.clone()).unwrap();
        assert_eq!(exe.changed(), None);
        let first = exe.identity().unwrap();

        fs::write(dir.path().join("next"), b"two").unwrap();
        fs::rename(dir.path().join("next"), &path).unwrap();
        let swapped = exe.changed().unwrap();
        assert_ne!(swapped, first);

        exe.adopt(swapped);
        assert_eq!(exe.identity(), Some(swapped));
        assert_eq!(exe.changed(), None);
    }
}
