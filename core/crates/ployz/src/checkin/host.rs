//! What the check-in loop asks of the host it runs on: the `Host` trait, the system's answers
//! (`ployzd`, `systemctl`, the daemon's socket) and the mark a preparation run leaves.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use ployz_core::probe_document::ProbeDocument;

use super::probe::{Binder, Sockets};
use super::{Committed, EnrollInstruction, enroll, unit};
use crate::cluster::Call;

pub(super) const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) trait Host {
    async fn probe(&self) -> Result<ProbeDocument, String>;
    fn hostname(&self) -> String;
    /// Whether systemd says `ployz-prepare` runs.
    async fn prepare_running(&self) -> Result<bool, String>;
    /// Whether a preparation run marks ployzd's preparation `lock`.
    fn preparation_marked(&self, lock: &Path) -> io::Result<bool> {
        preparation_marked(lock)
    }
    /// The sockets a probe window opens.
    fn binder(&self) -> Arc<dyn Binder> {
        Arc::new(Sockets)
    }
    async fn start_prepare(&self) -> Result<(), String>;
    async fn disable_loop(&self) -> Result<(), String>;
    /// Why the ployzd this loop runs with should be replaced, when it is older than this CLI.
    fn stale_ployzd(&self) -> Option<String>;
    /// Run `server add` here as `instruction` says, telling `progress` each stage it starts.
    async fn enroll(
        &self,
        instruction: &EnrollInstruction,
        progress: &dyn Fn(crate::cloud_enroll::EnrollStage),
    ) -> Result<Committed, String>;
    async fn discard_zfs_build(&self) -> Result<(), String>;
    /// The name this Server holds in the cluster it already participates in.
    async fn participating(&self) -> Option<String>;
    /// The program holding UDP [`ployz_core::WIREGUARD_PORT`], which ployzd needs for WireGuard.
    fn wireguard_port_holder(&self) -> Option<String> {
        None
    }
}

pub(crate) struct System {
    pub(crate) ployzd: PathBuf,
    pub(crate) stale: Option<String>,
    pub(crate) cloud: String,
}

impl System {
    pub(crate) fn with(ployzd: PathBuf) -> Self {
        Self {
            ployzd,
            stale: None,
            cloud: String::new(),
        }
    }
}

impl System {
    /// Whether this ployzd answers the host probe in time, so the loop can run with it.
    pub(crate) async fn probes(ployzd: &Path) -> bool {
        tokio::time::timeout(PROBE_TIMEOUT, Self::with(ployzd.to_owned()).probe())
            .await
            .is_ok_and(|probed| probed.is_ok())
    }
}

impl Host for System {
    async fn probe(&self) -> Result<ProbeDocument, String> {
        let run = |error: io::Error| format!("run {}: {error}", self.ployzd.display());
        let child = tokio::process::Command::new(&self.ployzd)
            .args(["install", "--probe", "--json"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(run)?;
        let group = ProcessGroup(child.id());
        let output = child.wait_with_output().await.map_err(run)?;
        // Reaped, so the id may already belong to someone else's group.
        group.reaped();
        if !output.status.success() {
            return Err(format!(
                "ployzd install --probe exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("read the host probe: {error}"))
    }

    fn hostname(&self) -> String {
        fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|name| name.trim().to_owned())
            .unwrap_or_default()
    }

    async fn prepare_running(&self) -> Result<bool, String> {
        let output =
            systemctl_output(&["show", "--property=ActiveState", "--value", unit::PREPARE]).await?;
        Ok(matches!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "active" | "activating" | "deactivating" | "reloading"
        ))
    }

    async fn start_prepare(&self) -> Result<(), String> {
        systemctl(&["start", "--no-block", unit::PREPARE]).await
    }

    async fn disable_loop(&self) -> Result<(), String> {
        systemctl(&["disable", unit::CHECKIN]).await
    }

    fn stale_ployzd(&self) -> Option<String> {
        self.stale.clone()
    }

    async fn enroll(
        &self,
        instruction: &EnrollInstruction,
        progress: &dyn Fn(crate::cloud_enroll::EnrollStage),
    ) -> Result<Committed, String> {
        enroll::server_add(self, instruction, progress).await
    }

    async fn discard_zfs_build(&self) -> Result<(), String> {
        enroll::discard_zfs_build(&self.ployzd).await
    }

    fn wireguard_port_holder(&self) -> Option<String> {
        ployz_core::port_holder::udp_holder(Path::new("/proc"), ployz_core::WIREGUARD_PORT)
    }

    async fn participating(&self) -> Option<String> {
        let details = local_machine(PROBE_TIMEOUT).await?;
        (details.phase == ployz_core::LocalMachinePhase::Participating).then(|| {
            details
                .machine
                .map_or_else(|| self.hostname(), |machine| machine.name.to_string())
        })
    }
}

/// What this host's daemon says of its Machine over the local socket, or none when it doesn't
/// answer within `timeout`.
pub(crate) async fn local_machine(timeout: Duration) -> Option<ployz_core::MachineDetails> {
    use crate::context::{Connection, ConnectionSource, SelectedConnections};
    use ployz_core::{InspectRequest, op};

    let inspect = async {
        let connection = Connection::unix(ployz_core::MachineLayout::production().socket()).ok()?;
        let mut client = crate::connect::connect_selected_with(
            SelectedConnections {
                source: ConnectionSource::LocalSocket,
                connections: vec![connection],
            },
            std::sync::Arc::new(crate::connect::SystemConnector::default()),
        )
        .await
        .ok()?;
        client
            .call::<op::Inspect>(
                InspectRequest::default(),
                Call::read().waiting(crate::cluster::Wait::standard()),
            )
            .await
            .ok()
    };
    tokio::time::timeout(timeout, inspect).await.ok().flatten()
}

/// Kills the probe's whole process group, so a hung `docker` or `lsblk` under
/// `ployzd` goes with it.
struct ProcessGroup(Option<u32>);

impl ProcessGroup {
    fn reaped(mut self) {
        self.0 = None;
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(group) = self
            .0
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
    }
}

async fn systemctl_output(args: &[&str]) -> Result<std::process::Output, String> {
    let output = tokio::process::Command::new("systemctl")
        .args(args)
        .kill_on_drop(true)
        .output();
    tokio::time::timeout(SYSTEMCTL_TIMEOUT, output)
        .await
        .map_err(|_| {
            format!(
                "systemctl {} did not finish in {}s. The change may still apply",
                args.join(" "),
                SYSTEMCTL_TIMEOUT.as_secs()
            )
        })?
        .map_err(|error| format!("run systemctl: {error}"))
}

pub(crate) async fn systemctl(args: &[&str]) -> Result<(), String> {
    let output = systemctl_output(args).await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "systemctl {} exited with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Whether this process runs as root.
pub(crate) fn process_is_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// Whether a preparation run marks the lock at `lock`. ployzd's run puts a whole-file write lock
/// on the open file it flocks, an open file description lock, which the kernel keeps for as long
/// as any process holds that file open, the run's build children included, and which reports no
/// pid. A process's own record lock reports its pid, and a byte range isn't the mark. Only a lock
/// file owned by this user and writable by no one else can carry it, so no other user can fake
/// one, and only on a filesystem whose locks stay local: see [`keeps_locks_local`]. Querying
/// takes no lock, so it never turns a run away.
#[cfg(target_os = "linux")]
pub(super) fn preparation_marked(lock: &Path) -> io::Result<bool> {
    use nix::{libc, sys::statfs::fstatfs};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    // Nonblocking, so a lease on the file fails the open rather than holding it up.
    let file = match fs::File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(lock)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let owner = file.metadata()?;
    if owner.uid() != rustix::process::geteuid().as_raw() || owner.mode() & 0o022 != 0 {
        return Ok(false);
    }
    marked_on(&file, fstatfs(&file)?.filesystem_type())
}

/// Whether `file`, open on a filesystem of type `filesystem`, carries the mark. Off the allowlist
/// it can't tell.
#[cfg(target_os = "linux")]
pub(super) fn marked_on(file: &fs::File, filesystem: nix::sys::statfs::FsType) -> io::Result<bool> {
    use nix::{
        fcntl::{FcntlArg, fcntl},
        libc,
    };
    use std::os::fd::AsRawFd;
    if !keeps_locks_local(filesystem) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "it is on a filesystem of type {:#x}, whose flocks and record locks aren't known to stay local and apart",
                filesystem.0
            ),
        ));
    }
    let write = libc::F_WRLCK as libc::c_short;
    let mut held = libc::flock {
        l_type: write,
        l_whence: libc::SEEK_SET as libc::c_short,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    };
    fcntl(file.as_raw_fd(), FcntlArg::F_OFD_GETLK(&mut held))?;
    Ok(held.l_type == write && held.l_pid == -1 && held.l_start == 0 && held.l_len == 0)
}

/// Whether locks on `filesystem` are the kernel's own. None of these gives its files a `lock` or
/// `flock` operation, so the kernel keeps flocks and record locks on separate local lists, and a
/// query reads the local record locks alone (`vfs_test_lock` and `posix_test_lock` in
/// `fs/locks.c`). NFS gives both operations, sends a flock to the server as a record lock and
/// asks the server on a query, so a plain flock answers as the mark there.
#[cfg(target_os = "linux")]
pub(super) fn keeps_locks_local(filesystem: nix::sys::statfs::FsType) -> bool {
    use nix::sys::statfs::{
        BTRFS_SUPER_MAGIC, EXT4_SUPER_MAGIC, FsType, OVERLAYFS_SUPER_MAGIC, TMPFS_MAGIC,
    };
    // Linux's `XFS_SUPER_MAGIC`, from include/uapi/linux/magic.h. nix 0.29 doesn't define it on
    // musl, which the release binaries build for.
    const XFS_SUPER_MAGIC: FsType = FsType(0x5846_5342);
    // OpenZFS's `ZFS_SUPER_MAGIC`, from its include/sys/fs/zfs.h.
    const ZFS_SUPER_MAGIC: FsType = FsType(0x2fc1_2fc1);
    // ext2 and ext3 share ext4's magic.
    [
        EXT4_SUPER_MAGIC,
        XFS_SUPER_MAGIC,
        BTRFS_SUPER_MAGIC,
        TMPFS_MAGIC,
        OVERLAYFS_SUPER_MAGIC,
        ZFS_SUPER_MAGIC,
    ]
    .contains(&filesystem)
}

/// This build has no open file description lock to read.
#[cfg(not(target_os = "linux"))]
pub(super) fn preparation_marked(_lock: &Path) -> io::Result<bool> {
    Ok(false)
}
