use crate::model::{MachineId, PrivateKey, PublicKey};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, IdentityError>;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("reading identity from {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing identity JSON")]
    Parse(#[source] serde_json::Error),
    #[error("creating directory {path}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("serializing identity")]
    Serialize(#[source] serde_json::Error),
    #[error("writing identity to {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Persistent machine identity: who this node is, independent of any network.
#[derive(Debug, Serialize, Deserialize)]
pub struct Identity {
    pub machine_id: MachineId,
    pub private_key: PrivateKey,
    pub public_key: PublicKey,
}

impl Identity {
    pub fn generate(machine_id: MachineId, key_bytes: [u8; 32]) -> Self {
        let private_key = PrivateKey(key_bytes);
        // Prototype key derivation (real impl uses x25519).
        let mut pub_bytes = [0u8; 32];
        for (i, &b) in key_bytes.iter().enumerate() {
            pub_bytes[31 - i] = b ^ (i as u8);
        }
        let public_key = PublicKey(pub_bytes);
        Self {
            machine_id,
            private_key,
            public_key,
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read_to_string(path).map_err(|source| IdentityError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&data).map_err(IdentityError::Parse)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| IdentityError::CreateDirectory {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let data = serde_json::to_string_pretty(self).map_err(IdentityError::Serialize)?;
        std::fs::write(path, data).map_err(|source| IdentityError::Write {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Load from path, or generate and save a new identity.
    pub fn load_or_generate(path: &Path) -> Result<Self> {
        if path.exists() {
            return Self::load(path);
        }

        let hostname = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "node".into());
        let machine_id = MachineId(hostname);

        let mut key_bytes = [0u8; 32];
        rand::fill(&mut key_bytes);

        let identity = Self::generate(machine_id, key_bytes);
        identity.save(path)?;
        Ok(identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_save_load() {
        let dir = std::env::temp_dir().join("ployz-test-identity");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("identity.json");

        let id = Identity::generate(MachineId("test-1".into()), [0x42; 32]);
        id.save(&path).expect("save identity");

        let loaded = Identity::load(&path).expect("load identity");
        assert_eq!(loaded.machine_id, id.machine_id);
        assert_eq!(loaded.public_key, id.public_key);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_errors() {
        assert!(Identity::load(Path::new("/tmp/ployz-nonexistent-identity.json")).is_err());
    }
}
