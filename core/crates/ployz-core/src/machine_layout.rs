//! Where a Machine keeps its files, so the daemon, the installer and the CLI agree on every path.
//!
//! Installed Machines and running units embed these paths, so they never change.

use std::path::{Path, PathBuf};

/// The directory under the Machine's data directory where host preparation keeps its state.
pub const PREPARE_DIR_NAME: &str = "prepare";

const SOCKET_NAME: &str = "ployz.sock";
const OBSERVE_SOCKET_NAME: &str = "observe.sock";
const FAULT_FILE_NAME: &str = "fault";
const MACHINE_POOL_FILE_NAME: &str = "ployz-machine-pool";
const DEVICE_POOL_MARKER_NAME: &str = "ployz-machine-pool.device";
const CHECKIN_DIR_NAME: &str = "ployz-checkin";
const FLEET_TOKEN_NAME: &str = "fleet-token";
const PREPARE_STATE_NAME: &str = "state.json";
const PREPARE_LOCK_NAME: &str = "lock";
const PREPARE_ZFS_DIR_NAME: &str = "zfs";

/// The directories a Machine's files live under, and the files each holds.
///
/// A Machine keeps its sockets in the run directory, its state in the data directory, and the
/// Fleet Token in the config directory. The Machine Pool file, its device ownership marker and the
/// check-in directory sit beside the data directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineLayout {
    run_dir: PathBuf,
    data_dir: PathBuf,
    config_dir: PathBuf,
}

impl MachineLayout {
    /// A Machine installed by `ployzd install`.
    #[must_use]
    pub fn production() -> Self {
        Self::new("/run/ployz", "/var/lib/ployz", "/etc/ployz")
    }

    /// The production Machine's config directory with the run and data directories spelled as given.
    ///
    /// An operator can type a root the installer accepts as the standard one while spelling it with
    /// a repeated or trailing separator. The units and `install -d` arguments built from it carry
    /// that spelling, so it is kept as typed.
    #[must_use]
    pub fn production_in(run_dir: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) -> Self {
        Self::new(run_dir, data_dir, Self::production().config_dir)
    }

    /// A layout under other directories, such as a temporary directory in a test.
    #[must_use]
    pub fn new(
        run_dir: impl Into<PathBuf>,
        data_dir: impl Into<PathBuf>,
        config_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            run_dir: run_dir.into(),
            data_dir: data_dir.into(),
            config_dir: config_dir.into(),
        }
    }

    #[must_use]
    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The Machine API socket the CLI connects to and the daemon serves.
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        Self::socket_in(&self.run_dir)
    }

    /// The run directory a socket path lives in. A socket with no parent is the production Machine's.
    #[must_use]
    pub fn run_dir_of(socket: &Path) -> PathBuf {
        socket
            .parent()
            .map_or_else(|| Self::production().run_dir, Path::to_path_buf)
    }

    /// The Machine API socket inside a run directory a caller was handed.
    #[must_use]
    pub fn socket_in(run_dir: &Path) -> PathBuf {
        run_dir.join(SOCKET_NAME)
    }

    /// The Observe Store's socket.
    #[must_use]
    pub fn observe_socket(&self) -> PathBuf {
        Self::observe_socket_in(&self.run_dir)
    }

    /// The Observe Store's socket inside a run directory a caller was handed.
    #[must_use]
    pub fn observe_socket_in(run_dir: &Path) -> PathBuf {
        run_dir.join(OBSERVE_SOCKET_NAME)
    }

    /// The file a fault-injecting daemon reads its armed fault from.
    #[must_use]
    pub fn fault_file(&self) -> PathBuf {
        self.run_dir.join(FAULT_FILE_NAME)
    }

    /// The file a root-backed Machine Pool lives in.
    #[must_use]
    pub fn machine_pool_file(&self) -> PathBuf {
        self.beside_data_dir(MACHINE_POOL_FILE_NAME)
    }

    /// Survives a missing device so recovery cannot replace its Machine Pool with a root-backed one.
    #[must_use]
    pub fn device_pool_marker(&self) -> PathBuf {
        self.beside_data_dir(DEVICE_POOL_MARKER_NAME)
    }

    /// The check-in loop's key, status and lock. It stays out of the data directory, which ployzd
    /// refuses to claim when it holds foreign files.
    #[must_use]
    pub fn checkin_dir(&self) -> PathBuf {
        self.beside_data_dir(CHECKIN_DIR_NAME)
    }

    /// The Fleet Token a host checks in with.
    #[must_use]
    pub fn fleet_token(&self) -> PathBuf {
        self.config_dir.join(FLEET_TOKEN_NAME)
    }

    /// Where host preparation keeps its state. It survives a Machine reset.
    #[must_use]
    pub fn prepare_dir(&self) -> PathBuf {
        self.data_dir.join(PREPARE_DIR_NAME)
    }

    /// The record of each preparation step, written by ployzd and read by the check-in loop.
    #[must_use]
    pub fn prepare_state(&self) -> PathBuf {
        self.prepare_dir().join(PREPARE_STATE_NAME)
    }

    /// The lock ployzd holds while it prepares this host.
    #[must_use]
    pub fn prepare_lock(&self) -> PathBuf {
        self.prepare_dir().join(PREPARE_LOCK_NAME)
    }

    /// Where the ZFS step builds its module.
    #[must_use]
    pub fn prepare_zfs_dir(&self) -> PathBuf {
        self.prepare_dir().join(PREPARE_ZFS_DIR_NAME)
    }

    fn beside_data_dir(&self, name: &str) -> PathBuf {
        self.data_dir.with_file_name(name)
    }
}

#[cfg(test)]
#[path = "machine_layout_tests.rs"]
mod tests;
