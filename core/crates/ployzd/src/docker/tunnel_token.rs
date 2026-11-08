//! The opened tunnel token on the Machine: one file root writes and only the Tunnel
//! Connector's user reads, bind-mounted read-only into the connector, which reads
//! it once at start. `/run` is memory, so a reboot clears it and the daemon writes
//! it again before the connector can start.

use std::{
    fs, io,
    os::unix::fs::{MetadataExt, PermissionsExt, chown},
    path::{Path, PathBuf},
};

use ployz_core::{
    Ciphertext, ContainerKind, MachineId, TUNNEL_TOKEN_FILE, TUNNEL_TOKEN_MACHINE_DIR,
    TUNNEL_USER_ID, tunnel_sealed_token,
};
use zeroize::Zeroizing;

use super::{ContainerRuntime, Door, Error};
use crate::filesystem::atomic_write;
use crate::sealed_env::{self, OpenError};

fn token_dir(root: &Path) -> PathBuf {
    root.join(TUNNEL_TOKEN_MACHINE_DIR.trim_start_matches('/'))
}

/// Write `token` where the connector's bind mount finds it.
pub(crate) fn write(root: &Path, token: &[u8]) -> io::Result<()> {
    let dir = token_dir(root);
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let path = dir.join(TUNNEL_TOKEN_FILE);
    atomic_write(&path, token, 0o400)?;
    if fs::metadata("/proc/self")?.uid() == 0 {
        chown(&path, Some(TUNNEL_USER_ID), Some(TUNNEL_USER_ID))?;
        chown(&dir, Some(TUNNEL_USER_ID), Some(TUNNEL_USER_ID))?;
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500))
}

/// Remove the token, once no connector is wanted here.
pub(crate) fn remove(root: &Path) -> io::Result<()> {
    let dir = token_dir(root);
    let opened = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .and_then(|()| fs::remove_dir_all(&dir));
    match opened {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Whether a token is on the Machine.
#[cfg(test)]
pub(crate) fn present(root: &Path) -> bool {
    token_dir(root).join(TUNNEL_TOKEN_FILE).is_file()
}

impl ContainerRuntime {
    pub(crate) fn write_tunnel_token(&self, token: &[u8]) -> io::Result<()> {
        write(&self.machine_root, token)
    }

    pub(crate) fn remove_tunnel_token(&self) -> io::Result<()> {
        remove(&self.machine_root)
    }

    /// Open `sealed` with this Machine's Sealing Key.
    ///
    /// # Errors
    ///
    /// Returns the [`OpenError`] when the key is missing or the token was sealed for
    /// another Cluster.
    pub(crate) fn open_tunnel_token(
        &self,
        sealed: &Ciphertext,
    ) -> Result<Zeroizing<Vec<u8>>, OpenError> {
        sealed_env::open_tunnel_token(sealed, self.sealing_key_dir())
    }

    /// Write the token of the newest Tunnel Connector here again, after a reboot
    /// cleared `/run`. A connector Docker restarted meanwhile reads it on its next
    /// try. Nothing to do when no connector is here.
    pub(crate) async fn restore_tunnel_token(&self, machine_id: &MachineId) -> Result<(), Error> {
        let mut newest = None;
        for (id, _) in self.docker.door_containers(Door::Tunnel).await? {
            let observation = match self.inspect_managed(&id, machine_id).await {
                Ok(observation) => observation,
                Err(Error::ContainerNotFound(_) | Error::SpecNotFound(_)) => continue,
                Err(error) => return Err(error),
            };
            if observation.kind != ContainerKind::ServiceContainer {
                continue;
            }
            let Some(sealed) = tunnel_sealed_token(&observation.resolved_spec.container.labels)
            else {
                continue;
            };
            if newest
                .as_ref()
                .is_none_or(|(created_at, _)| *created_at < observation.created_at_unix_nanos)
            {
                newest = Some((observation.created_at_unix_nanos, sealed));
            }
        }
        let Some((_, sealed)) = newest else {
            return Ok(());
        };
        let token = self.open_tunnel_token(&sealed)?;
        self.write_tunnel_token(&token).map_err(Error::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_is_written_read_only_and_removed_with_its_directory() {
        let root = std::env::temp_dir().join(format!("ployzd-tunnel-{}", MachineId::random()));
        write(&root, b"first").unwrap();
        let path = token_dir(&root).join(TUNNEL_TOKEN_FILE);
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o400);
        assert_eq!(
            fs::metadata(token_dir(&root)).unwrap().mode() & 0o777,
            0o500
        );
        write(&root, b"second").unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            b"second",
            "a later token replaces it"
        );
        remove(&root).unwrap();
        assert!(!present(&root));
        remove(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
