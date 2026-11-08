//! The Private Access router as Docker shows it, and the opened auth key on the
//! Machine: one file root writes and the router reads once at start. `/run` is memory,
//! so a reboot clears it and the daemon writes it again before the router can restart.

use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use bollard::query_parameters::LogsOptionsBuilder;
use futures_util::StreamExt;
use ployz_core::{
    Ciphertext, ContainerId, ContainerKind, ContainerObservation, MachineId,
    PRIVATE_ACCESS_KEY_FILE, PRIVATE_ACCESS_KEY_MACHINE_DIR, QualifiedService,
    private_access_sealed_key,
};
use zeroize::Zeroizing;

use super::{ContainerRuntime, Error, LABEL_MANAGED, LABEL_NAMESPACE, LABEL_SERVICE_NAME};
use crate::filesystem::atomic_write;
use crate::sealed_env::{self, OpenError};

/// How much of the router's output the daemon reads to say why it stopped.
const OUTPUT_TAIL_LINES: &str = "40";

fn key_dir(root: &Path) -> PathBuf {
    root.join(PRIVATE_ACCESS_KEY_MACHINE_DIR.trim_start_matches('/'))
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
                LABEL_MANAGED.to_owned(),
                format!("{LABEL_NAMESPACE}={}", identity.namespace),
                format!("{LABEL_SERVICE_NAME}={}", identity.name),
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
        routers.sort_by_key(|router| (router.created_at_unix_nanos, router.container_id));
        Ok(routers)
    }

    /// Write the auth key of the newest router here again, after a reboot cleared
    /// `/run`. A router Docker restarted meanwhile reads it on its next try. Nothing
    /// to do when no router is here.
    pub(crate) async fn restore_private_access_key(
        &self,
        machine_id: &MachineId,
    ) -> Result<(), Error> {
        let sealed = self
            .routers(machine_id)
            .await?
            .iter()
            .rev()
            .find_map(|router| private_access_sealed_key(&router.resolved_spec.container.labels));
        let Some(sealed) = sealed else {
            return Ok(());
        };
        let key = self.open_private_access_key(&sealed)?;
        self.write_private_access_key(&key).map_err(Error::Io)
    }

    /// The last lines a router wrote, for saying why it stopped. Empty when Docker
    /// cannot say.
    pub(crate) async fn router_output(&self, id: &ContainerId) -> String {
        let options = LogsOptionsBuilder::default()
            .stdout(true)
            .stderr(true)
            .tail(OUTPUT_TAIL_LINES)
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
}
