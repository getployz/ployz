//! Artifact targets installed by keeper.

use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use ployz_core::install::InstallArtifactSpec;

use crate::fsx::{FileMode, StagedFile, StagedFileError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    EbpfBytecode,
    EbpfCtl,
    NatsServer,
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
pub struct ArtifactSource(ArtifactSourceKind);

#[derive(Debug, Clone, PartialEq, Eq)]
enum ArtifactSourceKind {
    LocalPath(PathBuf),
    RemoteUrl(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactSourceView<'a> {
    LocalPath(&'a Path),
    RemoteUrl(&'a str),
}

impl ArtifactSource {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ArtifactTargetError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ArtifactTargetError::EmptySource);
        }
        if value.starts_with("https://") || value.starts_with("http://") {
            return Ok(Self(ArtifactSourceKind::RemoteUrl(value)));
        }

        Self::from_local_path(PathBuf::from(value))
    }

    pub fn from_local_path(path: PathBuf) -> Result<Self, ArtifactTargetError> {
        if path.as_os_str().is_empty() {
            return Err(ArtifactTargetError::EmptySource);
        }
        if !path.is_absolute() {
            return Err(ArtifactTargetError::RelativeSourcePath { value: path });
        }
        Ok(Self(ArtifactSourceKind::LocalPath(path)))
    }

    #[must_use]
    pub fn view(&self) -> ArtifactSourceView<'_> {
        match &self.0 {
            ArtifactSourceKind::LocalPath(path) => ArtifactSourceView::LocalPath(path),
            ArtifactSourceKind::RemoteUrl(url) => ArtifactSourceView::RemoteUrl(url),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactTarget {
    pub kind: ArtifactKind,
    pub version: ArtifactVersion,
    pub source: ArtifactSource,
    pub digest: Sha256Digest,
    install_path: PathBuf,
}

impl ArtifactTarget {
    pub fn new(
        kind: ArtifactKind,
        version: ArtifactVersion,
        source: ArtifactSource,
        digest: Sha256Digest,
        install_path: PathBuf,
    ) -> Result<Self, ArtifactTargetError> {
        Ok(Self {
            kind,
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

/// Converts one wire-level install artifact spec into a validated keeper
/// install target of the given kind.
pub fn artifact_target(
    kind: ArtifactKind,
    spec: &InstallArtifactSpec,
) -> Result<ArtifactTarget, ArtifactTargetError> {
    ArtifactTarget::new(
        kind,
        ArtifactVersion::try_new(spec.version.as_str())?,
        ArtifactSource::try_new(spec.source.as_str())?,
        Sha256Digest::try_new(spec.sha256.as_str())?,
        PathBuf::from(spec.install_path.as_str()),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataplaneArtifactTargets {
    pub ebpf_bytecode: ArtifactTarget,
    pub ebpf_ctl: ArtifactTarget,
}

impl DataplaneArtifactTargets {
    #[must_use]
    pub const fn new(ebpf_bytecode: ArtifactTarget, ebpf_ctl: ArtifactTarget) -> Self {
        Self {
            ebpf_bytecode,
            ebpf_ctl,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactTargetError {
    EmptyVersion,
    EmptySource,
    RelativeSourcePath { value: PathBuf },
    EmptyDigest,
    InvalidSha256Digest { value: String },
    EmptyInstallPath,
    RelativeInstallPath { value: PathBuf },
    MissingInstallParent { value: PathBuf },
    MissingInstallFileName { value: PathBuf },
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
    if install_path.parent().is_none() {
        return Err(ArtifactTargetError::MissingInstallParent {
            value: install_path,
        });
    }
    if install_path.file_name().is_none() || has_trailing_separator(&install_path) {
        return Err(ArtifactTargetError::MissingInstallFileName {
            value: install_path,
        });
    }
    Ok(install_path)
}

#[cfg(unix)]
fn has_trailing_separator(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().last() == Some(&b'/')
}

#[cfg(not(unix))]
fn has_trailing_separator(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|value| value.ends_with(std::path::MAIN_SEPARATOR))
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledArtifactFile {
    pub source_path: PathBuf,
    pub install_path: PathBuf,
    pub digest: Sha256Digest,
    pub durability: ArtifactInstallDurability,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactInstallDurability {
    Confirmed,
    Unconfirmed { message: String },
}

pub fn install_verified_artifact(
    verified: &VerifiedArtifactFile,
    target: &ArtifactTarget,
) -> Result<InstalledArtifactFile, ArtifactInstallError> {
    if verified.digest != target.digest {
        return Err(ArtifactInstallError::VerifiedDigestMismatch {
            install_path: target.install_path().to_path_buf(),
            expected: target.digest.clone(),
            verified: verified.digest.clone(),
        });
    }
    let install_path = target.install_path();
    let parent = install_path
        .parent()
        .expect("artifact install path is validated with a parent");
    let file_name = install_path
        .file_name()
        .expect("artifact install path is validated with a file name");

    fs::create_dir_all(parent).map_err(|error| ArtifactInstallError::CreateParentFailed {
        path: parent.to_path_buf(),
        message: error.to_string(),
    })?;
    let mut staged_artifact = create_staged_artifact(&verified.path, parent, file_name)?;
    let staged = match verify_artifact_file(staged_artifact.path(), &target.digest) {
        Ok(staged) => staged,
        Err(error) => {
            return Err(ArtifactInstallError::VerificationFailed(error));
        }
    };
    let durability = commit_staged_artifact(&mut staged_artifact, install_path)?;

    Ok(InstalledArtifactFile {
        source_path: verified.path.clone(),
        install_path: install_path.to_path_buf(),
        digest: staged.digest,
        durability,
    })
}

#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn make_executable(path: &Path) -> std::io::Result<()> {
    let _metadata = fs::metadata(path)?;
    Ok(())
}

fn create_staged_artifact(
    source_path: &Path,
    parent: &Path,
    file_name: &std::ffi::OsStr,
) -> Result<StagedFile, ArtifactInstallError> {
    let file_name = file_name.to_string_lossy();
    let mut staged = StagedFile::create(parent, &file_name, "ployz-install", FileMode::Plain)
        .map_err(|error| match error {
            StagedFileError::ClockWentBackwards { message } => {
                ArtifactInstallError::ClockWentBackwards { message }
            }
            StagedFileError::CreateFailed {
                staged_path,
                message,
            } => ArtifactInstallError::CreateStagedFailed {
                staged_path,
                message,
            },
            StagedFileError::Exhausted { directory } => {
                ArtifactInstallError::CreateStagedExhausted { parent: directory }
            }
        })?;
    let staged_path = staged.path().to_path_buf();
    copy_file_to_writer(source_path, &staged_path, staged.file())?;
    Ok(staged)
}

fn copy_file_to_writer(
    source_path: &Path,
    staged_path: &Path,
    staged_file: &mut File,
) -> Result<(), ArtifactInstallError> {
    let mut source = File::open(source_path).map_err(|error| ArtifactInstallError::ReadFailed {
        source_path: source_path.to_path_buf(),
        message: error.to_string(),
    })?;
    std::io::copy(&mut source, staged_file).map_err(|error| ArtifactInstallError::CopyFailed {
        source_path: source_path.to_path_buf(),
        message: error.to_string(),
    })?;
    staged_file
        .sync_all()
        .map_err(|error| ArtifactInstallError::SyncStagedFailed {
            staged_path: staged_path.to_path_buf(),
            message: error.to_string(),
        })
}

fn commit_staged_artifact(
    staged: &mut StagedFile,
    install_path: &Path,
) -> Result<ArtifactInstallDurability, ArtifactInstallError> {
    make_executable(staged.path()).map_err(|error| ArtifactInstallError::SetExecutableFailed {
        staged_path: staged.path().to_path_buf(),
        message: error.to_string(),
    })?;
    sync_staged_file(staged.path())?;
    staged
        .commit_to(install_path)
        .map_err(|error| ArtifactInstallError::CommitFailed {
            staged_path: staged.path().to_path_buf(),
            install_path: install_path.to_path_buf(),
            message: error.to_string(),
        })?;
    if let Err(error) = sync_parent_directory(install_path) {
        return Ok(ArtifactInstallDurability::Unconfirmed {
            message: error.to_string(),
        });
    }
    Ok(ArtifactInstallDurability::Confirmed)
}

fn sync_staged_file(staged_path: &Path) -> Result<(), ArtifactInstallError> {
    let file = File::open(staged_path).map_err(|error| ArtifactInstallError::SyncStagedFailed {
        staged_path: staged_path.to_path_buf(),
        message: error.to_string(),
    })?;
    file.sync_all()
        .map_err(|error| ArtifactInstallError::SyncStagedFailed {
            staged_path: staged_path.to_path_buf(),
            message: error.to_string(),
        })
}

#[cfg(unix)]
fn sync_parent_directory(install_path: &Path) -> std::io::Result<()> {
    let parent = install_path
        .parent()
        .expect("artifact install path is validated with a parent");
    let directory = File::open(parent)?;
    directory.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_install_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactInstallError {
    VerificationFailed(ArtifactVerificationError),
    VerifiedDigestMismatch {
        install_path: PathBuf,
        expected: Sha256Digest,
        verified: Sha256Digest,
    },
    CreateParentFailed {
        path: PathBuf,
        message: String,
    },
    ClockWentBackwards {
        message: String,
    },
    CreateStagedFailed {
        staged_path: PathBuf,
        message: String,
    },
    CreateStagedExhausted {
        parent: PathBuf,
    },
    ReadFailed {
        source_path: PathBuf,
        message: String,
    },
    CopyFailed {
        source_path: PathBuf,
        message: String,
    },
    SyncStagedFailed {
        staged_path: PathBuf,
        message: String,
    },
    SetExecutableFailed {
        staged_path: PathBuf,
        message: String,
    },
    CommitFailed {
        staged_path: PathBuf,
        install_path: PathBuf,
        message: String,
    },
}

impl fmt::Display for ArtifactInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VerificationFailed(error) => {
                write!(
                    formatter,
                    "artifact verification failed before install: {error}"
                )
            }
            Self::VerifiedDigestMismatch {
                install_path,
                expected,
                verified,
            } => write!(
                formatter,
                "verified artifact digest {} does not match install target {} for {}",
                verified.as_str(),
                expected.as_str(),
                install_path.display()
            ),
            Self::CreateParentFailed { path, message } => {
                write!(
                    formatter,
                    "failed to create artifact install directory {}: {message}",
                    path.display()
                )
            }
            Self::ClockWentBackwards { message } => {
                write!(
                    formatter,
                    "failed to create staged artifact name: {message}"
                )
            }
            Self::CreateStagedFailed {
                staged_path,
                message,
            } => write!(
                formatter,
                "failed to create staged artifact {}: {message}",
                staged_path.display()
            ),
            Self::CreateStagedExhausted { parent } => write!(
                formatter,
                "failed to create a unique staged artifact in {}",
                parent.display()
            ),
            Self::ReadFailed {
                source_path,
                message,
            } => write!(
                formatter,
                "failed to read artifact {} for install: {message}",
                source_path.display()
            ),
            Self::CopyFailed {
                source_path,
                message,
            } => write!(
                formatter,
                "failed to copy artifact {} to staged file: {message}",
                source_path.display()
            ),
            Self::SyncStagedFailed {
                staged_path,
                message,
            } => write!(
                formatter,
                "failed to sync staged artifact {}: {message}",
                staged_path.display()
            ),
            Self::SetExecutableFailed {
                staged_path,
                message,
            } => write!(
                formatter,
                "failed to mark staged artifact {} executable: {message}",
                staged_path.display()
            ),
            Self::CommitFailed {
                staged_path,
                install_path,
                message,
            } => write!(
                formatter,
                "failed to commit staged artifact {} to {}: {message}",
                staged_path.display(),
                install_path.display()
            ),
        }
    }
}

impl std::error::Error for ArtifactInstallError {}

impl fmt::Display for ArtifactTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyVersion => formatter.write_str("artifact version is empty"),
            Self::EmptySource => formatter.write_str("artifact source is empty"),
            Self::RelativeSourcePath { value } => {
                write!(
                    formatter,
                    "artifact source path {} must be absolute",
                    value.display()
                )
            }
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
            Self::MissingInstallParent { value } => {
                write!(
                    formatter,
                    "artifact install path {} must have a parent directory",
                    value.display()
                )
            }
            Self::MissingInstallFileName { value } => {
                write!(
                    formatter,
                    "artifact install path {} must have a file name",
                    value.display()
                )
            }
        }
    }
}

impl std::error::Error for ArtifactTargetError {}
