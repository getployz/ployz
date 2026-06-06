//! Artifact targets installed by keeper.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Keeper,
    Ployzd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactVersion(String);

impl ArtifactVersion {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ArtifactTargetError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ArtifactTargetError::EmptyVersion);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ArtifactTargetError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ArtifactTargetError::EmptyDigest);
        }
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ArtifactTargetError::InvalidSha256Digest { value });
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSource(String);

impl ArtifactSource {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ArtifactTargetError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ArtifactTargetError::EmptySource);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperArtifactTarget {
    pub version: ArtifactVersion,
    pub source: ArtifactSource,
    pub digest: Sha256Digest,
    install_path: PathBuf,
}

impl KeeperArtifactTarget {
    pub fn new(
        version: ArtifactVersion,
        source: ArtifactSource,
        digest: Sha256Digest,
        install_path: PathBuf,
    ) -> Result<Self, ArtifactTargetError> {
        Ok(Self {
            version,
            source,
            digest,
            install_path: validate_install_path(install_path)?,
        })
    }

    #[must_use]
    pub fn install_path(&self) -> &Path {
        &self.install_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzdArtifactTarget {
    pub version: ArtifactVersion,
    pub source: ArtifactSource,
    pub digest: Sha256Digest,
    install_path: PathBuf,
}

impl PloyzdArtifactTarget {
    pub fn new(
        version: ArtifactVersion,
        source: ArtifactSource,
        digest: Sha256Digest,
        install_path: PathBuf,
    ) -> Result<Self, ArtifactTargetError> {
        Ok(Self {
            version,
            source,
            digest,
            install_path: validate_install_path(install_path)?,
        })
    }

    #[must_use]
    pub fn install_path(&self) -> &Path {
        &self.install_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactTarget {
    Keeper(KeeperArtifactTarget),
    Ployzd(PloyzdArtifactTarget),
}

impl ArtifactTarget {
    #[must_use]
    pub const fn kind(&self) -> ArtifactKind {
        match self {
            Self::Keeper(_) => ArtifactKind::Keeper,
            Self::Ployzd(_) => ArtifactKind::Ployzd,
        }
    }
}

impl From<KeeperArtifactTarget> for ArtifactTarget {
    fn from(value: KeeperArtifactTarget) -> Self {
        Self::Keeper(value)
    }
}

impl From<PloyzdArtifactTarget> for ArtifactTarget {
    fn from(value: PloyzdArtifactTarget) -> Self {
        Self::Ployzd(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactTargetError {
    EmptyVersion,
    EmptySource,
    EmptyDigest,
    InvalidSha256Digest { value: String },
    EmptyInstallPath,
    RelativeInstallPath { value: PathBuf },
}

fn validate_install_path(install_path: PathBuf) -> Result<PathBuf, ArtifactTargetError> {
    if install_path.as_os_str().is_empty() {
        return Err(ArtifactTargetError::EmptyInstallPath);
    }
    if !install_path.is_absolute() {
        return Err(ArtifactTargetError::RelativeInstallPath {
            value: install_path,
        });
    }
    Ok(install_path)
}
