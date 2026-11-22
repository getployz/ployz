//! This computer's Device key file.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use ployz_core::DeviceKey;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum KeyError {
    #[error("couldn't read {path}")]
    Read { path: PathBuf, source: io::Error },
    #[error("couldn't write {path}")]
    Write { path: PathBuf, source: io::Error },
    #[error("{path} is readable by other users. Run: chmod 600 {path}")]
    Exposed { path: PathBuf },
    #[error("{path} isn't a device key. Delete it and run ployz device request")]
    Malformed { path: PathBuf },
}

impl From<KeyError> for crate::failure::Failure {
    fn from(error: KeyError) -> Self {
        Self::command(error)
    }
}

pub(crate) fn path(config: &Path) -> PathBuf {
    crate::config_dir::ConfigDir::new(config).device_key()
}

pub(crate) fn public(secret: &[u8; 32]) -> DeviceKey {
    DeviceKey::from_bytes(*iroh::SecretKey::from_bytes(secret).public().as_bytes())
}

pub(crate) fn load(path: &Path) -> Result<Option<[u8; 32]>, KeyError> {
    let read = |source| KeyError::Read {
        path: path.to_owned(),
        source,
    };
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(read(source)),
    };
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(KeyError::Exposed {
            path: path.to_owned(),
        });
    }
    let bytes = fs::read(path).map_err(read)?;
    bytes.try_into().map(Some).map_err(|_| KeyError::Malformed {
        path: path.to_owned(),
    })
}

pub(crate) fn generate() -> [u8; 32] {
    iroh::SecretKey::generate().to_bytes()
}

/// Write `secret` to `path`, readable only by its owner. False when a key is already
/// there, which is never replaced.
pub(crate) fn store(path: &Path, secret: &[u8; 32]) -> Result<bool, KeyError> {
    let write = |source| KeyError::Write {
        path: path.to_owned(),
        source,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(write)?;
    }
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(source) => return Err(write(source)),
    };
    file.write_all(secret).map_err(write)?;
    file.sync_all().map_err(write)?;
    Ok(true)
}
