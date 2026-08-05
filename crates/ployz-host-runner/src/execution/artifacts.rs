//! Artifact targets installed by Host Runner.

use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::fsx::{FileMode, StagedFile, StagedFileError};
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion,
    InstallContractError, InstallSha256Digest,
};
pub use ployz_core::join::JoinArtifactKind as ArtifactKind;

pub type ArtifactVersion = InstallArtifactVersion;
pub type ArtifactSource = InstallArtifactSource;
pub type Sha256Digest = InstallSha256Digest;
pub type ArtifactTargetError = InstallContractError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactSourceView<'a> {
    LocalPath(&'a Path),
    RemoteUrl(&'a str),
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
        let install_path = AbsoluteInstallPath::try_new(install_path.display().to_string())?;
        Ok(Self {
            kind,
            version,
            source,
            digest,
            install_path: PathBuf::from(install_path.as_str()),
        })
    }

    #[must_use]
    pub fn install_path(&self) -> &Path {
        &self.install_path
    }

    #[must_use]
    pub fn source_view(&self) -> ArtifactSourceView<'_> {
        let source = self.source.as_str();
        if source.starts_with("https://") || source.starts_with("http://") {
            ArtifactSourceView::RemoteUrl(source)
        } else {
            ArtifactSourceView::LocalPath(Path::new(source))
        }
    }

    #[must_use]
    pub fn install_spec(&self) -> InstallArtifactSpec {
        InstallArtifactSpec {
            version: self.version.clone(),
            source: self.source.clone(),
            sha256: self.digest.clone(),
            install_path: AbsoluteInstallPath::try_new(self.install_path.display().to_string())
                .expect("validated artifact target install path stays valid"),
        }
    }
}

/// Converts one wire-level install artifact spec into a validated Host Runner
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

/// Acquires a remote artifact into `<store>/<sha256>` without exposing
/// unverified bytes at the content-addressed path.
pub fn acquire_remote_artifact_content_addressed<E: fmt::Display>(
    source: &str,
    expected: &Sha256Digest,
    store: &Path,
    download: impl FnOnce(&Path) -> Result<(), E>,
) -> Result<VerifiedArtifactFile, ArtifactInstallError> {
    if !store.is_absolute() {
        return Err(ArtifactInstallError::RelativeContentStore {
            path: store.to_path_buf(),
        });
    }
    fs::create_dir_all(store).map_err(|error| ArtifactInstallError::CreateParentFailed {
        path: store.to_path_buf(),
        message: error.to_string(),
    })?;
    let cached = store.join(expected.as_str());
    if cached
        .try_exists()
        .map_err(|error| ArtifactInstallError::InspectCachedFailed {
            path: cached.clone(),
            message: error.to_string(),
        })?
    {
        match verify_artifact_file(&cached, expected) {
            Ok(verified) => return Ok(verified),
            Err(ArtifactVerificationError::DigestMismatch { .. }) => {}
            Err(error @ ArtifactVerificationError::ReadFailed { .. }) => {
                return Err(ArtifactInstallError::VerificationFailed(error));
            }
        }
    }

    let staged = create_empty_staged_artifact(store, expected.as_str(), "ployz-download")?;
    download(staged.path()).map_err(|error| ArtifactInstallError::DownloadFailed {
        source_url: source.to_owned(),
        staged_path: staged.path().to_path_buf(),
        message: error.to_string(),
    })?;
    let verified = verify_artifact_file(staged.path(), expected)
        .map_err(ArtifactInstallError::VerificationFailed)?;
    let published = stage_verified_artifact_content_addressed(&verified, store)?;
    Ok(VerifiedArtifactFile {
        path: published.staged_path,
        digest: published.digest,
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
    Ok(Sha256Digest::try_new(format!("{digest:x}")).expect("sha256 hasher yields valid hex"))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactVerificationError {
    #[error("failed to read artifact {}: {message}", path.display())]
    ReadFailed { path: PathBuf, message: String },
    #[error(
        "artifact {} sha256 mismatch: expected {}, got {}",
        path.display(),
        expected.as_str(),
        actual.as_str()
    )]
    DigestMismatch {
        path: PathBuf,
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledArtifactFile {
    pub source_path: PathBuf,
    pub install_path: PathBuf,
    pub digest: Sha256Digest,
    pub durability: ArtifactInstallDurability,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentAddressedArtifactFile {
    pub source_path: PathBuf,
    pub staged_path: PathBuf,
    pub digest: Sha256Digest,
    pub durability: ArtifactInstallDurability,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactInstallDurability {
    Confirmed,
    Unconfirmed { message: String },
}

/// Stages a verified artifact at `<store>/<sha256>`. Existing valid content is
/// reused; corrupted content is replaced through an atomic sibling rename.
pub fn stage_verified_artifact_content_addressed(
    verified: &VerifiedArtifactFile,
    store: &Path,
) -> Result<ContentAddressedArtifactFile, ArtifactInstallError> {
    if !store.is_absolute() {
        return Err(ArtifactInstallError::RelativeContentStore {
            path: store.to_path_buf(),
        });
    }
    let staged_path = store.join(verified.digest.as_str());
    if staged_path.exists() {
        match verify_artifact_file(&staged_path, &verified.digest) {
            Ok(existing) => {
                make_executable(&staged_path).map_err(|error| {
                    ArtifactInstallError::SetExecutableFailed {
                        staged_path: staged_path.clone(),
                        message: error.to_string(),
                    }
                })?;
                sync_staged_file(&staged_path)?;
                return Ok(ContentAddressedArtifactFile {
                    source_path: verified.path.clone(),
                    staged_path,
                    digest: existing.digest,
                    durability: ArtifactInstallDurability::Confirmed,
                });
            }
            Err(ArtifactVerificationError::DigestMismatch { .. }) => {}
            Err(error) => return Err(ArtifactInstallError::VerificationFailed(error)),
        }
    }

    let (digest, durability) = copy_verified_artifact_to(verified, &verified.digest, &staged_path)?;

    Ok(ContentAddressedArtifactFile {
        source_path: verified.path.clone(),
        staged_path,
        digest,
        durability,
    })
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
    let (digest, durability) = copy_verified_artifact_to(verified, &target.digest, install_path)?;

    Ok(InstalledArtifactFile {
        source_path: verified.path.clone(),
        install_path: install_path.to_path_buf(),
        digest,
        durability,
    })
}

fn copy_verified_artifact_to(
    verified: &VerifiedArtifactFile,
    expected: &Sha256Digest,
    install_path: &Path,
) -> Result<(Sha256Digest, ArtifactInstallDurability), ArtifactInstallError> {
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
    let staged = match verify_artifact_file(staged_artifact.path(), expected) {
        Ok(staged) => staged,
        Err(error) => {
            return Err(ArtifactInstallError::VerificationFailed(error));
        }
    };
    let durability = commit_staged_artifact(&mut staged_artifact, install_path)?;
    Ok((staged.digest, durability))
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
    let mut staged = create_empty_staged_artifact(parent, &file_name, "ployz-install")?;
    let staged_path = staged.path().to_path_buf();
    copy_file_to_writer(source_path, &staged_path, staged.file())?;
    Ok(staged)
}

fn create_empty_staged_artifact(
    parent: &Path,
    file_name: &str,
    purpose: &str,
) -> Result<StagedFile, ArtifactInstallError> {
    StagedFile::create(parent, file_name, purpose, FileMode::Plain).map_err(|error| match error {
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
    })
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactInstallError {
    #[error("artifact verification failed before install: {0}")]
    VerificationFailed(ArtifactVerificationError),
    #[error("content-addressed artifact store {} must be absolute", path.display())]
    RelativeContentStore { path: PathBuf },
    #[error("failed to inspect cached artifact {}: {message}", path.display())]
    InspectCachedFailed { path: PathBuf, message: String },
    #[error(
        "artifact download from {source_url} to {} failed: {message}",
        staged_path.display()
    )]
    DownloadFailed {
        source_url: String,
        staged_path: PathBuf,
        message: String,
    },
    #[error(
        "verified artifact digest {} does not match install target {} for {}",
        verified.as_str(),
        expected.as_str(),
        install_path.display()
    )]
    VerifiedDigestMismatch {
        install_path: PathBuf,
        expected: Sha256Digest,
        verified: Sha256Digest,
    },
    #[error("failed to create artifact install directory {}: {message}", path.display())]
    CreateParentFailed { path: PathBuf, message: String },
    #[error("failed to create staged artifact name: {message}")]
    ClockWentBackwards { message: String },
    #[error("failed to create staged artifact {}: {message}", staged_path.display())]
    CreateStagedFailed {
        staged_path: PathBuf,
        message: String,
    },
    #[error("failed to create a unique staged artifact in {}", parent.display())]
    CreateStagedExhausted { parent: PathBuf },
    #[error("failed to read artifact {} for install: {message}", source_path.display())]
    ReadFailed {
        source_path: PathBuf,
        message: String,
    },
    #[error(
        "failed to copy artifact {} to staged file: {message}",
        source_path.display()
    )]
    CopyFailed {
        source_path: PathBuf,
        message: String,
    },
    #[error("failed to sync staged artifact {}: {message}", staged_path.display())]
    SyncStagedFailed {
        staged_path: PathBuf,
        message: String,
    },
    #[error(
        "failed to mark staged artifact {} executable: {message}",
        staged_path.display()
    )]
    SetExecutableFailed {
        staged_path: PathBuf,
        message: String,
    },
    #[error(
        "failed to commit staged artifact {} to {}: {message}",
        staged_path.display(),
        install_path.display()
    )]
    CommitFailed {
        staged_path: PathBuf,
        install_path: PathBuf,
        message: String,
    },
}
