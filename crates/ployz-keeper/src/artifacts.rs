//! Artifact targets installed by keeper.

use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::File;
use std::io::Read;
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

    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        match self {
            Self::Keeper(target) => &target.digest,
            Self::Ployzd(target) => &target.digest,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifactFile {
    pub path: PathBuf,
    pub digest: Sha256Digest,
}

pub fn verify_artifact_file(
    path: impl AsRef<Path>,
    expected: &Sha256Digest,
) -> Result<VerifiedArtifactFile, ArtifactVerificationError> {
    let path = path.as_ref();
    let actual = sha256_file(path)?;
    if actual != *expected {
        return Err(ArtifactVerificationError::DigestMismatch {
            path: path.to_path_buf(),
            expected: expected.clone(),
            actual,
        });
    }

    Ok(VerifiedArtifactFile {
        path: path.to_path_buf(),
        digest: actual,
    })
}

fn sha256_file(path: &Path) -> Result<Sha256Digest, ArtifactVerificationError> {
    let mut file = File::open(path).map_err(|error| ArtifactVerificationError::ReadFailed {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];

    loop {
        let bytes_read =
            file.read(&mut buffer)
                .map_err(|error| ArtifactVerificationError::ReadFailed {
                    path: path.to_path_buf(),
                    message: error.to_string(),
                })?;
        if bytes_read == 0 {
            break;
        }
        let (chunk, _) = buffer.split_at(bytes_read);
        hasher.update(chunk);
    }

    let digest = hasher.finalize();
    Ok(Sha256Digest(format!("{digest:x}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactVerificationError {
    ReadFailed {
        path: PathBuf,
        message: String,
    },
    DigestMismatch {
        path: PathBuf,
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
}

impl fmt::Display for ArtifactVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadFailed { path, message } => {
                write!(
                    formatter,
                    "failed to read artifact {}: {message}",
                    path.display()
                )
            }
            Self::DigestMismatch {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "artifact {} sha256 mismatch: expected {}, got {}",
                path.display(),
                expected.as_str(),
                actual.as_str()
            ),
        }
    }
}

impl std::error::Error for ArtifactVerificationError {}

impl fmt::Display for ArtifactTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyVersion => formatter.write_str("artifact version is empty"),
            Self::EmptySource => formatter.write_str("artifact source is empty"),
            Self::EmptyDigest => formatter.write_str("artifact sha256 digest is empty"),
            Self::InvalidSha256Digest { value } => {
                write!(
                    formatter,
                    "artifact sha256 digest {value:?} must be 64 hex characters"
                )
            }
            Self::EmptyInstallPath => formatter.write_str("artifact install path is empty"),
            Self::RelativeInstallPath { value } => {
                write!(
                    formatter,
                    "artifact install path {} must be absolute",
                    value.display()
                )
            }
        }
    }
}

impl std::error::Error for ArtifactTargetError {}
