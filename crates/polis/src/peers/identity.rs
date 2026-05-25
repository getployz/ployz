use std::{fs, io::Write, path::Path};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use iroh::{EndpointAddr, SecretKey};

use crate::IrohEndpointId;

use super::{PeerError, PeerProbeResult};

#[derive(Debug, Clone)]
pub struct PeerIdentity {
    secret_key: SecretKey,
}

impl PeerIdentity {
    #[must_use]
    pub fn generate() -> Self {
        Self {
            secret_key: SecretKey::generate(),
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> PeerProbeResult<Self> {
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| PeerError::MalformedIdentity)?;

        Ok(Self {
            secret_key: SecretKey::from_bytes(&bytes),
        })
    }

    #[must_use]
    pub fn endpoint_id(&self) -> IrohEndpointId {
        IrohEndpointId::parse(self.secret_key.public().to_string())
            .expect("iroh endpoint ids are non-empty")
    }

    #[must_use]
    pub fn endpoint_addr(&self) -> EndpointAddr {
        EndpointAddr::new(self.secret_key.public())
    }

    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        self.secret_key.to_bytes()
    }

    pub(super) fn secret_key(&self) -> SecretKey {
        self.secret_key.clone()
    }
}

pub fn load_or_create_identity(path: &Path) -> PeerProbeResult<PeerIdentity> {
    match fs::read(path) {
        Ok(bytes) => parse_existing_identity(path, &bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let identity = PeerIdentity::generate();
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| PeerError::IdentityIo { source })?;
            }
            match write_identity_file(path, &identity.to_bytes()) {
                Ok(()) => Ok(identity),
                Err(PeerError::IdentityIo { source })
                    if source.kind() == std::io::ErrorKind::AlreadyExists =>
                {
                    let bytes =
                        fs::read(path).map_err(|source| PeerError::IdentityIo { source })?;
                    parse_existing_identity(path, &bytes)
                }
                Err(error) => Err(error),
            }
        }
        Err(source) => Err(PeerError::IdentityIo { source }),
    }
}

fn parse_existing_identity(path: &Path, bytes: &[u8]) -> PeerProbeResult<PeerIdentity> {
    restrict_identity_permissions(path)?;
    PeerIdentity::from_bytes(bytes)
}

fn write_identity_file(path: &Path, bytes: &[u8; 32]) -> PeerProbeResult<()> {
    #[cfg(unix)]
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| PeerError::IdentityIo { source })?;

    #[cfg(not(unix))]
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| PeerError::IdentityIo { source })?;

    file.write_all(bytes)
        .map_err(|source| PeerError::IdentityIo { source })?;
    Ok(())
}

fn restrict_identity_permissions(path: &Path) -> PeerProbeResult<()> {
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions)
            .map_err(|source| PeerError::IdentityIo { source })?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}
