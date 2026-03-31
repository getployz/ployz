use ployz_types::model::{Identity, MachineId};
use serde_json;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, IdentityConfigError>;

#[derive(Debug, Error)]
pub enum IdentityConfigError {
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

pub fn load_identity(path: &Path) -> Result<Identity> {
    let data = std::fs::read_to_string(path).map_err(|source| IdentityConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&data).map_err(IdentityConfigError::Parse)
}

pub fn save_identity(identity: &Identity, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| IdentityConfigError::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let data = serde_json::to_string_pretty(identity).map_err(IdentityConfigError::Serialize)?;
    std::fs::write(path, data).map_err(|source| IdentityConfigError::Write {
        path: path.to_path_buf(),
        source,
    })
}

pub fn load_or_generate_identity(path: &Path) -> Result<Identity> {
    if path.exists() {
        return load_identity(path);
    }

    let hostname = hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "node".into());
    let machine_id = MachineId(hostname);

    let mut key_bytes = [0u8; 32];
    rand::fill(&mut key_bytes);

    let identity = Identity::generate(machine_id, key_bytes);
    save_identity(&identity, path)?;
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_save_load() {
        let dir = std::env::temp_dir().join("ployz-test-identity");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("identity.json");

        let identity = Identity::generate(MachineId("test-1".into()), [0x42; 32]);
        save_identity(&identity, &path).expect("save identity");

        let loaded = load_identity(&path).expect("load identity");
        assert_eq!(loaded.machine_id, identity.machine_id);
        assert_eq!(loaded.public_key, identity.public_key);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
