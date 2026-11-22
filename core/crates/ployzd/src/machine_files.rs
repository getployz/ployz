//! The secret files the daemon keeps on the Machine for the Containers that mount them.
//!
//! The Private Access router's opened auth key and the Tunnel Connector's opened token
//! are each one file root writes into a read-only directory the Container bind-mounts.
//! `/run` is memory, so a reboot clears them, and Docker starts those Containers
//! before the daemon can write them again, so the daemon starts them itself once the
//! file is back (`LocalMachine::restore_private_access`, `LocalMachine::restore_doors`).
//!
//! The router's node state is on disk, and its presence is the durable word that this
//! Machine wants a router: `off` renames it away before it touches the router, so an
//! `off` that fails or is cut short leaves nothing a reboot would start. A router can
//! outlive the state it mounts, when `off` forgets the state and then cannot stop the
//! router. So the state carries a random ID, written when the state is made and
//! stamped on each router created to mount it, and a router is the one wanted only
//! while its ID is the one on disk.

use std::{
    fs, io,
    os::unix::fs::{MetadataExt, PermissionsExt, chown},
    path::{Path, PathBuf},
};

use ployz_core::private_fs::{Existing, create_private_dir};
use ployz_core::{
    PRIVATE_ACCESS_KEY_FILE, PRIVATE_ACCESS_KEY_MACHINE_DIR, PRIVATE_ACCESS_STATE_MACHINE_DIR,
    PrivateAccessStateId, TUNNEL_TOKEN_FILE, TUNNEL_TOKEN_MACHINE_DIR, TUNNEL_USER_ID,
};

use crate::filesystem::atomic_write;

const STATE_ID_FILE: &str = "ployz-state-id";

/// Present while a connector the Tunnel Door closing could not remove may still be in
/// Docker. It is on disk, since the connector and its sealed token label outlive a
/// reboot, and `/run` does not.
const TUNNEL_CLOSED_MARKER: &str = "var/lib/ployz/tunnel-door-closed";

/// The Machine's secret files, under `root`, which is `/` outside tests.
#[derive(Clone, Debug)]
pub(crate) struct MachineFiles {
    root: PathBuf,
}

impl MachineFiles {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn under(&self, machine_path: &str) -> PathBuf {
        self.root.join(machine_path.trim_start_matches('/'))
    }

    fn key_dir(&self) -> PathBuf {
        self.under(PRIVATE_ACCESS_KEY_MACHINE_DIR)
    }

    fn state_dir(&self) -> PathBuf {
        self.under(PRIVATE_ACCESS_STATE_MACHINE_DIR)
    }

    fn forgotten_dir(&self) -> PathBuf {
        self.state_dir().with_extension("forgotten")
    }

    fn token_dir(&self) -> PathBuf {
        self.under(TUNNEL_TOKEN_MACHINE_DIR)
    }

    fn closed_marker(&self) -> PathBuf {
        self.root.join(TUNNEL_CLOSED_MARKER)
    }

    /// Write `key` where the router's bind mount finds it.
    pub(crate) fn write_private_access_key(&self, key: &[u8]) -> io::Result<()> {
        write_read_only(&self.key_dir(), PRIVATE_ACCESS_KEY_FILE, key, None)
    }

    /// Remove the key, once no router is wanted here.
    pub(crate) fn remove_private_access_key(&self) -> io::Result<()> {
        remove_read_only(&self.key_dir())
    }

    /// Make the node state directory the router's bind mount finds, for root alone, and
    /// return its ID. The router runs as root, so the daemon's own ownership is the
    /// router's. What the router already wrote there stays, so the node it registered
    /// comes back. A directory with no ID gets a new one, which no router carries yet.
    pub(crate) fn keep_private_access_state(&self) -> io::Result<PrivateAccessStateId> {
        let dir = self.state_dir();
        private_dir(&dir)?;
        let path = dir.join(STATE_ID_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => {
                if let Ok(id) = PrivateAccessStateId::parse(text.trim()) {
                    return Ok(id);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let id = PrivateAccessStateId::random();
        atomic_write(&path, id.as_str().as_bytes(), 0o600)?;
        Ok(id)
    }

    /// Forget the node, once no router is wanted here, so a later `on` registers a new
    /// one. One rename moves the state out of the router's path and is the moment the
    /// Machine stops wanting a router, so a daemon killed while the rest is deleted
    /// leaves no half-deleted state that a reboot would read as `on`.
    pub(crate) fn forget_private_access_state(&self) -> io::Result<()> {
        let dir = self.state_dir();
        let forgotten = self.forgotten_dir();
        remove_dir_if_present(&forgotten)?;
        match fs::rename(&dir, &forgotten) {
            Ok(()) => sync_parent(&dir)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        remove_dir_if_present(&forgotten)
    }

    /// Whether this Machine wants a router: `on` keeps the node state and `off` forgets it.
    pub(crate) fn private_access_state_kept(&self) -> bool {
        self.state_dir().is_dir()
    }

    /// Write `token` where the connector's bind mount finds it, for the connector's
    /// user alone.
    pub(crate) fn write_tunnel_token(&self, token: &[u8]) -> io::Result<()> {
        write_read_only(
            &self.token_dir(),
            TUNNEL_TOKEN_FILE,
            token,
            Some(TUNNEL_USER_ID),
        )
    }

    /// Remove the token, once no connector is wanted here.
    pub(crate) fn remove_tunnel_token(&self) -> io::Result<()> {
        remove_read_only(&self.token_dir())
    }

    /// Record that the Tunnel Door is closed, before the connector is removed.
    pub(crate) fn mark_tunnel_door_closed(&self) -> io::Result<()> {
        let path = self.closed_marker();
        fs::create_dir_all(path.parent().expect("the marker has a parent"))?;
        atomic_write(&path, b"", 0o600)
    }

    /// Forget the marker, once the Tunnel Door is open or no closed connector remains.
    pub(crate) fn clear_tunnel_door_closed(&self) -> io::Result<()> {
        let path = self.closed_marker();
        match fs::remove_file(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
            Ok(()) => sync_parent(&path),
        }
    }

    /// Whether the Tunnel Door was closed and its connector may remain.
    pub(crate) fn tunnel_door_closed(&self) -> bool {
        self.closed_marker().exists()
    }
}

/// `dir` as a directory only root enters: made as `create_dir_all` makes it, so a file
/// in the way fails as it always did, then set to exactly `0700`.
fn private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    create_private_dir(dir, Existing::Tighten)?;
    Ok(())
}

/// Write `contents` as `dir/file`, readable only by its owner, in a directory its owner
/// can read but not change. When the daemon runs as root, `owner` takes both.
fn write_read_only(dir: &Path, file: &str, contents: &[u8], owner: Option<u32>) -> io::Result<()> {
    private_dir(dir)?;
    let path = dir.join(file);
    atomic_write(&path, contents, 0o400)?;
    if let Some(owner) = owner
        && fs::metadata("/proc/self")?.uid() == 0
    {
        chown(&path, Some(owner), Some(owner))?;
        chown(dir, Some(owner), Some(owner))?;
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o500))
}

fn remove_read_only(dir: &Path) -> io::Result<()> {
    let opened = fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .and_then(|()| fs::remove_dir_all(dir));
    match opened {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn remove_dir_if_present(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn sync_parent(path: &Path) -> io::Result<()> {
    fs::File::open(path.parent().expect("a Machine file has a parent"))?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(root: &Path) -> MachineFiles {
        MachineFiles::new(root.to_owned())
    }

    #[test]
    fn a_file_where_a_parent_directory_belongs_fails_as_not_a_directory() {
        let root = tempfile::tempdir().unwrap();
        let files = files(root.path());
        let state = files.state_dir();
        fs::create_dir_all(state.parent().unwrap().parent().unwrap()).unwrap();
        fs::write(state.parent().unwrap(), b"in the way").unwrap();
        assert_eq!(
            files.keep_private_access_state().unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        let key = files.key_dir();
        fs::create_dir_all(key.parent().unwrap().parent().unwrap()).unwrap();
        fs::write(key.parent().unwrap(), b"in the way").unwrap();
        assert_eq!(
            files.write_private_access_key(b"key").unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
    }

    #[test]
    fn each_secret_is_written_read_only_and_removed_with_its_directory() {
        let root = tempfile::tempdir().unwrap();
        let files = files(root.path());
        type Write = fn(&MachineFiles, &[u8]) -> io::Result<()>;
        type Remove = fn(&MachineFiles) -> io::Result<()>;
        let secrets: [(PathBuf, Write, Remove); 2] = [
            (
                files.key_dir().join(PRIVATE_ACCESS_KEY_FILE),
                MachineFiles::write_private_access_key,
                MachineFiles::remove_private_access_key,
            ),
            (
                files.token_dir().join(TUNNEL_TOKEN_FILE),
                MachineFiles::write_tunnel_token,
                MachineFiles::remove_tunnel_token,
            ),
        ];
        for (path, write, remove) in secrets {
            write(&files, b"first").unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"first");
            assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o400);
            let dir = path.parent().unwrap();
            assert_eq!(fs::metadata(dir).unwrap().mode() & 0o777, 0o500);
            write(&files, b"second").unwrap();
            assert_eq!(
                fs::read(&path).unwrap(),
                b"second",
                "a later one replaces it"
            );
            remove(&files).unwrap();
            assert!(!dir.exists());
            remove(&files).unwrap();
        }
    }

    // Catches the state directory losing what the router wrote when `on` runs again,
    // which would register a new node, and surviving `off`, which would keep one.
    #[test]
    fn the_node_state_is_kept_for_root_across_on_and_forgotten_by_off() {
        let root = tempfile::tempdir().unwrap();
        let files = files(root.path());
        assert!(!files.private_access_state_kept());
        files.keep_private_access_state().unwrap();
        assert!(files.private_access_state_kept());
        assert_eq!(
            fs::metadata(files.state_dir()).unwrap().mode() & 0o777,
            0o700
        );
        fs::write(files.state_dir().join("tailscaled.state"), b"node").unwrap();
        let id = files.keep_private_access_state().unwrap();
        assert_eq!(
            fs::read(files.state_dir().join("tailscaled.state")).unwrap(),
            b"node",
            "keeping the state again keeps the node"
        );
        assert_eq!(files.keep_private_access_state().unwrap(), id, "and its ID");
        files.forget_private_access_state().unwrap();
        assert!(!files.private_access_state_kept());
        files.forget_private_access_state().unwrap();
        assert_ne!(
            files.keep_private_access_state().unwrap(),
            id,
            "state made again after a forget is new state"
        );
    }

    // Catches a forget cut short partway through deleting the node, which left a
    // half-deleted directory a reboot read as a Machine that wants a router.
    #[test]
    fn a_forget_cut_short_after_its_rename_still_reads_as_off() {
        let root = tempfile::tempdir().unwrap();
        let files = files(root.path());
        files.keep_private_access_state().unwrap();
        fs::write(files.state_dir().join("tailscaled.state"), b"node").unwrap();
        fs::rename(files.state_dir(), files.forgotten_dir()).unwrap();
        assert!(!files.private_access_state_kept());
        files.forget_private_access_state().unwrap();
        assert!(
            !files.forgotten_dir().exists(),
            "the next forget finishes the delete"
        );
        files.keep_private_access_state().unwrap();
        assert!(
            !files.state_dir().join("tailscaled.state").exists(),
            "a later on starts a fresh node"
        );
    }
}
