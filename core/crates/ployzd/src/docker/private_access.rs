//! The Private Access router as Docker shows it, and what it keeps on the Machine: the
//! opened auth key, one file root writes and the router reads when its node first
//! joins, and the node state the router writes. `/run` is memory, so a reboot clears
//! the key, and Docker starts the router before the daemon can write it again, so the
//! daemon starts the router itself once the key is back
//! (`LocalMachine::restore_private_access`). The node state is on disk, and its
//! presence is the durable word that this Machine wants a router: `off` renames it
//! away before it touches the router, so an `off` that fails or is cut short leaves
//! nothing a reboot would start.
//!
//! A router can outlive the state it mounts, when `off` forgets the state and then
//! cannot stop the router. So the state carries a random ID, written when the state
//! is made and stamped on each router created to mount it, and a router is the one
//! wanted only while its ID is the one on disk.

use ployz_core::labels;
use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use bollard::query_parameters::LogsOptionsBuilder;
use futures_util::StreamExt;
use ployz_core::{
    Ciphertext, ContainerId, ContainerKind, ContainerObservation, MachineId,
    PRIVATE_ACCESS_KEY_FILE, PRIVATE_ACCESS_KEY_MACHINE_DIR, PRIVATE_ACCESS_STATE_MACHINE_DIR,
    PrivateAccessStateId, QualifiedService,
};
use zeroize::Zeroizing;

use super::{ContainerRuntime, Error};
use crate::filesystem::atomic_write;
use crate::sealed_env::{self, OpenError};

fn key_dir(root: &Path) -> PathBuf {
    root.join(PRIVATE_ACCESS_KEY_MACHINE_DIR.trim_start_matches('/'))
}

fn state_dir(root: &Path) -> PathBuf {
    root.join(PRIVATE_ACCESS_STATE_MACHINE_DIR.trim_start_matches('/'))
}

/// The file in the node state directory that holds the state's ID.
const STATE_ID_FILE: &str = "ployz-state-id";

/// Make the node state directory the router's bind mount finds, for root alone, and
/// return its ID. The router runs as root, so the daemon's own ownership is the
/// router's. What the router already wrote there stays, so the node it registered
/// comes back. A directory with no ID gets a new one, which no router carries yet.
pub(crate) fn keep_state(root: &Path) -> io::Result<PrivateAccessStateId> {
    let dir = state_dir(root);
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
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

fn forgotten_dir(root: &Path) -> PathBuf {
    state_dir(root).with_extension("forgotten")
}

fn remove_dir_if_present(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Forget the node, once no router is wanted here, so a later `on` registers a new
/// one. One rename moves the state out of the router's path and is the moment the
/// Machine stops wanting a router, so a daemon killed while the rest is deleted leaves
/// no half-deleted state that a reboot would read as `on`.
pub(crate) fn forget_state(root: &Path) -> io::Result<()> {
    let dir = state_dir(root);
    let forgotten = forgotten_dir(root);
    remove_dir_if_present(&forgotten)?;
    match fs::rename(&dir, &forgotten) {
        Ok(()) => {
            fs::File::open(dir.parent().expect("the state directory has a parent"))?.sync_all()?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    remove_dir_if_present(&forgotten)
}

/// Whether this Machine wants a router: `on` keeps the node state and `off` forgets it.
pub(crate) fn state_kept(root: &Path) -> bool {
    state_dir(root).is_dir()
}

/// The tailnet the router's node joined, as its own `tailscaled.state` names it: the
/// current profile's tailnet domain, else its MagicDNS name. `None` when there is no
/// state, it names no profile, or it can't be read.
pub(crate) fn tailnet(root: &Path) -> Option<String> {
    use base64::Engine as _;
    let state: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&fs::read(state_dir(root).join("tailscaled.state")).ok()?).ok()?;
    let decoded = |key: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(state.get(key)?.as_str()?)
            .ok()
    };
    let current = String::from_utf8(decoded("_current-profile")?).ok()?;
    let profiles: serde_json::Value = serde_json::from_slice(&decoded("_profiles")?).ok()?;
    let network = profiles.get(current.trim())?.get("NetworkProfile")?;
    ["DomainName", "MagicDNSName"]
        .into_iter()
        .find_map(|field| network.get(field)?.as_str().filter(|name| !name.is_empty()))
        .map(str::to_owned)
}

/// Write `key` where the router's bind mount finds it.
pub(crate) fn write(root: &Path, key: &[u8]) -> io::Result<()> {
    let dir = key_dir(root);
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    atomic_write(&dir.join(PRIVATE_ACCESS_KEY_FILE), key, 0o400)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500))
}

/// Remove the key, once no router is wanted here.
pub(crate) fn remove(root: &Path) -> io::Result<()> {
    let dir = key_dir(root);
    let opened = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .and_then(|()| fs::remove_dir_all(&dir));
    match opened {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Whether a key is on the Machine.
#[cfg(test)]
pub(crate) fn present(root: &Path) -> bool {
    key_dir(root).join(PRIVATE_ACCESS_KEY_FILE).is_file()
}

impl super::LocalDocker {
    async fn router_containers(&self) -> Result<Vec<ContainerId>, Error> {
        let identity = QualifiedService::system_tailscale();
        Ok(self
            .labelled(vec![
                labels::MANAGED.to_owned(),
                format!("{}={}", labels::NAMESPACE, identity.namespace),
                format!("{}={}", labels::SERVICE_NAME, identity.name),
            ])
            .await?
            .into_iter()
            .map(|(id, _)| id)
            .collect())
    }
}

impl ContainerRuntime {
    pub(crate) fn write_private_access_key(&self, key: &[u8]) -> io::Result<()> {
        write(&self.machine_root, key)
    }

    pub(crate) fn remove_private_access_key(&self) -> io::Result<()> {
        remove(&self.machine_root)
    }

    pub(crate) fn keep_private_access_state(&self) -> io::Result<PrivateAccessStateId> {
        keep_state(&self.machine_root)
    }

    pub(crate) fn forget_private_access_state(&self) -> io::Result<()> {
        forget_state(&self.machine_root)
    }

    pub(crate) fn private_access_state_kept(&self) -> bool {
        state_kept(&self.machine_root)
    }

    pub(crate) fn private_access_tailnet(&self) -> Option<String> {
        tailnet(&self.machine_root)
    }

    /// Open `sealed` with this Machine's Sealing Key.
    ///
    /// # Errors
    ///
    /// Returns the [`OpenError`] when the key is missing or the auth key was sealed
    /// for another Cluster or purpose.
    pub(crate) fn open_private_access_key(
        &self,
        sealed: &Ciphertext,
    ) -> Result<Zeroizing<Vec<u8>>, OpenError> {
        sealed_env::open_private_access_key(sealed, self.sealing_key_dir())
    }

    /// Every Private Access router here that Docker still has and whose spec is
    /// stored, newest last.
    pub(crate) async fn routers(
        &self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Error> {
        let mut routers = Vec::new();
        for id in self.docker.router_containers().await? {
            let observation = match self.inspect_managed(&id, machine_id).await {
                Ok(observation) => observation,
                Err(Error::ContainerNotFound(_)) => continue,
                Err(Error::SpecNotFound(id)) => {
                    tracing::warn!(container = %id, "a Private Access router has no stored spec, so the route ignores it");
                    continue;
                }
                Err(error) => return Err(error),
            };
            if observation.kind == ContainerKind::ServiceContainer {
                routers.push(observation);
            }
        }
        routers.sort_by_key(ContainerObservation::recency_key);
        Ok(routers)
    }

    /// What a router wrote since `since`, in Unix seconds, for saying why it stopped.
    /// A router refused its key restarts and repeats its start, so a fixed tail would
    /// lose the refusal. Empty when Docker cannot say.
    pub(crate) async fn router_output(&self, id: &ContainerId, since: i32) -> String {
        let options = LogsOptionsBuilder::default()
            .stdout(true)
            .stderr(true)
            .since(since)
            .build();
        let mut lines = self.docker.client.logs(id.as_str(), Some(options));
        let mut output = String::new();
        while let Some(Ok(chunk)) = lines.next().await {
            output.push_str(&String::from_utf8_lossy(&chunk.into_bytes()));
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn the_tailnet_is_read_from_the_router_s_own_state() {
        use base64::Engine as _;
        let root = tempfile::tempdir().unwrap();
        assert_eq!(tailnet(root.path()), None);
        keep_state(root.path()).unwrap();
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let profiles = serde_json::json!({
            "a1b2": {"Name": "ada@example.com", "NetworkProfile": {"MagicDNSName": "tail1234.ts.net", "DomainName": "example.com"}}
        });
        let state = serde_json::json!({
            "_current-profile": encode(b"a1b2"),
            "_profiles": encode(profiles.to_string().as_bytes()),
        });
        std::fs::write(
            state_dir(root.path()).join("tailscaled.state"),
            state.to_string(),
        )
        .unwrap();
        assert_eq!(tailnet(root.path()).as_deref(), Some("example.com"));
    }

    #[test]
    fn the_key_is_written_read_only_and_removed_with_its_directory() {
        let root = std::env::temp_dir().join(format!("ployzd-tailscale-{}", MachineId::random()));
        write(&root, b"first").unwrap();
        let path = key_dir(&root).join(PRIVATE_ACCESS_KEY_FILE);
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o400);
        assert_eq!(fs::metadata(key_dir(&root)).unwrap().mode() & 0o777, 0o500);
        write(&root, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        remove(&root).unwrap();
        assert!(!present(&root));
        remove(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    // Catches the state directory losing what the router wrote when `on` runs again,
    // which would register a new node, and surviving `off`, which would keep one.
    #[test]
    fn the_node_state_is_kept_for_root_across_on_and_forgotten_by_off() {
        let root = std::env::temp_dir().join(format!("ployzd-ts-state-{}", MachineId::random()));
        assert!(!state_kept(&root));
        keep_state(&root).unwrap();
        assert!(state_kept(&root));
        assert_eq!(
            fs::metadata(state_dir(&root)).unwrap().mode() & 0o777,
            0o700
        );
        fs::write(state_dir(&root).join("tailscaled.state"), b"node").unwrap();
        let id = keep_state(&root).unwrap();
        assert_eq!(
            fs::read(state_dir(&root).join("tailscaled.state")).unwrap(),
            b"node",
            "keeping the state again keeps the node"
        );
        assert_eq!(keep_state(&root).unwrap(), id, "and its ID");
        forget_state(&root).unwrap();
        assert!(!state_kept(&root));
        forget_state(&root).unwrap();
        assert_ne!(
            keep_state(&root).unwrap(),
            id,
            "state made again after a forget is new state"
        );
        fs::remove_dir_all(root).unwrap();
    }

    // Catches a forget cut short partway through deleting the node, which left a
    // half-deleted directory a reboot read as a Machine that wants a router.
    #[test]
    fn a_forget_cut_short_after_its_rename_still_reads_as_off() {
        let root = std::env::temp_dir().join(format!("ployzd-ts-cut-{}", MachineId::random()));
        keep_state(&root).unwrap();
        fs::write(state_dir(&root).join("tailscaled.state"), b"node").unwrap();
        fs::rename(state_dir(&root), forgotten_dir(&root)).unwrap();
        assert!(!state_kept(&root));
        forget_state(&root).unwrap();
        assert!(
            !forgotten_dir(&root).exists(),
            "the next forget finishes the delete"
        );
        keep_state(&root).unwrap();
        assert!(
            !state_dir(&root).join("tailscaled.state").exists(),
            "a later on starts a fresh node"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
