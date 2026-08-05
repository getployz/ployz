//! Artifact targets installed by Host Runner.

use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::fsx::{FileMode, StagedFile, StagedFileError, write_durable_file};
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

/// The local, content-addressed executable store for `ployzd`.
///
/// The store owns only machine-local artifact bytes and the stable links that
/// supervisor units execute. Callers must supply a [`VerifiedArtifactFile`],
/// so neither `current` nor `previous` can point at bytes that have not passed
/// the expected SHA-256 check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzdArtifactStore {
    state: PathBuf,
}

impl PloyzdArtifactStore {
    pub fn new(state: PathBuf) -> Result<Self, ArtifactStoreError> {
        if !state.is_absolute() {
            return Err(ArtifactStoreError::RelativeStateDirectory { path: state });
        }
        Ok(Self { state })
    }

    #[must_use]
    pub fn state(&self) -> &Path {
        &self.state
    }

    #[must_use]
    pub fn artifacts_path(&self) -> PathBuf {
        self.state.join("artifacts")
    }

    #[must_use]
    pub fn current_path(&self) -> PathBuf {
        self.state.join("current")
    }

    #[must_use]
    pub fn previous_path(&self) -> PathBuf {
        self.state.join("previous")
    }

    #[must_use]
    pub fn pending_path(&self) -> PathBuf {
        self.state.join("upgrade-pending")
    }

    /// Stages a verified binary under `<state>/artifacts/<sha256>`.
    pub fn stage(
        &self,
        verified: &VerifiedArtifactFile,
    ) -> Result<ContentAddressedArtifactFile, ArtifactStoreError> {
        stage_verified_artifact_content_addressed(verified, &self.artifacts_path())
            .map_err(ArtifactStoreError::Stage)
    }

    /// Seeds `current` for a founding or joining machine without creating a
    /// rollback target. Repeating the same seed is safe; seeding over a
    /// different current binary is refused so bootstrap cannot masquerade as
    /// an upgrade.
    pub fn seed_current(
        &self,
        verified: &VerifiedArtifactFile,
    ) -> Result<ContentAddressedArtifactFile, ArtifactStoreError> {
        let staged = self.stage(verified)?;
        let current = self.current_path();
        match self.read_verified_link(&current)? {
            Some(current_digest) if current_digest == staged.digest => return Ok(staged),
            Some(current_digest) => {
                return Err(ArtifactStoreError::CurrentAlreadySeeded {
                    current: current_digest,
                    requested: staged.digest,
                });
            }
            None => {}
        }
        if self.path_exists(&self.previous_path())? {
            return Err(ArtifactStoreError::PreviousExistsWithoutCurrent {
                path: self.previous_path(),
            });
        }
        if self.path_exists(&self.pending_path())? {
            return Err(ArtifactStoreError::PendingUpgradeExists {
                path: self.pending_path(),
            });
        }

        self.replace_link(&current, &artifact_link_target(&staged.digest))?;
        Ok(staged)
    }

    /// Arms a keeper-first swap. The previous executable and durable pending
    /// marker are committed before `current` changes, so a failed new Keeper
    /// has an already-verified target for its systemd failure handler.
    pub fn arm_staged(
        &self,
        expected: &Sha256Digest,
    ) -> Result<ArmedPloyzdArtifact, ArtifactStoreError> {
        let staged_path = self.artifacts_path().join(expected.as_str());
        verify_artifact_file(&staged_path, expected).map_err(ArtifactStoreError::Verification)?;
        let current_link = self.read_current_verified_link()?.ok_or_else(|| {
            ArtifactStoreError::CurrentMissing {
                path: self.current_path(),
            }
        })?;
        let current = current_link.digest().clone();
        let pending = self.pending_upgrade()?;
        if let CurrentArtifactLink::RevertedToPrevious(current) = current_link
            && let Some(pending) = pending.as_ref()
        {
            return Err(ArtifactStoreError::InterruptedRollback {
                current,
                pending: pending.clone(),
            });
        }
        if current == *expected {
            if pending.as_ref() == Some(expected) {
                let previous =
                    self.read_verified_link(&self.previous_path())?
                        .ok_or_else(|| ArtifactStoreError::PreviousMissing {
                            path: self.previous_path(),
                        })?;
                return Ok(ArmedPloyzdArtifact {
                    previous,
                    current,
                    staged_path,
                });
            }
            return Err(ArtifactStoreError::TargetAlreadyCurrent { digest: current });
        }
        if pending.as_ref() == Some(expected) {
            let previous = self
                .read_verified_link(&self.previous_path())?
                .ok_or_else(|| ArtifactStoreError::PreviousMissing {
                    path: self.previous_path(),
                })?;
            if previous != current {
                return Err(ArtifactStoreError::PendingUpgradeExists {
                    path: self.pending_path(),
                });
            }
            self.replace_link(&self.current_path(), &artifact_link_target(expected))?;
            return Ok(ArmedPloyzdArtifact {
                previous,
                current: expected.clone(),
                staged_path,
            });
        }
        if pending.is_some() {
            return Err(ArtifactStoreError::PendingUpgradeExists {
                path: self.pending_path(),
            });
        }

        self.replace_link(&self.previous_path(), &artifact_link_target(&current))?;
        write_durable_file(
            &self.state,
            "upgrade-pending",
            FileMode::Plain,
            format!("{}\n", expected.as_str()).as_bytes(),
        )
        .map_err(|error| ArtifactStoreError::WritePending {
            path: self.pending_path(),
            message: error.to_string(),
        })?;
        self.replace_link(&self.current_path(), &artifact_link_target(expected))?;

        Ok(ArmedPloyzdArtifact {
            previous: current,
            current: expected.clone(),
            staged_path,
        })
    }

    /// Returns the staged digest whose first-converge confirmation is still
    /// pending. A malformed marker is refused rather than interpreted as an
    /// upgrade target.
    pub fn pending_upgrade(&self) -> Result<Option<Sha256Digest>, ArtifactStoreError> {
        let contents = match fs::read_to_string(self.pending_path()) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(ArtifactStoreError::ReadPending {
                    path: self.pending_path(),
                    message: error.to_string(),
                });
            }
        };
        Sha256Digest::try_new(contents.trim().to_owned())
            .map(Some)
            .map_err(|error| ArtifactStoreError::InvalidPending {
                path: self.pending_path(),
                message: error.to_string(),
            })
    }

    /// Marks the armed binary as healthy after Keeper reaches its first
    /// converge point. `previous` remains available for the next arm.
    pub fn confirm_armed(&self) -> Result<(), ArtifactStoreError> {
        self.remove_pending_marker()
    }

    /// Reverts an armed swap locally. This is the same link shape used by the
    /// systemd failure unit: `current` points at `previous`, and then the
    /// pending marker is removed.
    pub fn revert_armed(&self) -> Result<Sha256Digest, ArtifactStoreError> {
        if !self.path_exists(&self.pending_path())? {
            return Err(ArtifactStoreError::PendingUpgradeMissing {
                path: self.pending_path(),
            });
        }
        let previous = self
            .read_verified_link(&self.previous_path())?
            .ok_or_else(|| ArtifactStoreError::PreviousMissing {
                path: self.previous_path(),
            })?;
        self.replace_link(&self.current_path(), &self.previous_path())?;
        self.remove_pending_marker()?;
        Ok(previous)
    }

    fn read_verified_link(&self, link: &Path) -> Result<Option<Sha256Digest>, ArtifactStoreError> {
        let target = match fs::read_link(link) {
            Ok(target) => target,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
                return Err(ArtifactStoreError::LinkIsNotSymlink {
                    path: link.to_path_buf(),
                });
            }
            Err(error) => {
                return Err(ArtifactStoreError::ReadLink {
                    path: link.to_path_buf(),
                    message: error.to_string(),
                });
            }
        };
        let digest = if let Some(digest) = parse_artifact_link_target(&target) {
            digest
        } else if link == self.current_path() && target == self.previous_path() {
            return self.read_verified_link(&self.previous_path());
        } else {
            return Err(ArtifactStoreError::UnsafeLinkTarget {
                path: link.to_path_buf(),
                target,
            });
        };
        let artifact = self.artifacts_path().join(digest.as_str());
        verify_artifact_file(&artifact, &digest).map_err(ArtifactStoreError::Verification)?;
        Ok(Some(digest))
    }

    fn read_current_verified_link(
        &self,
    ) -> Result<Option<CurrentArtifactLink>, ArtifactStoreError> {
        let current_path = self.current_path();
        let target = match fs::read_link(&current_path) {
            Ok(target) => target,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
                return Err(ArtifactStoreError::LinkIsNotSymlink { path: current_path });
            }
            Err(error) => {
                return Err(ArtifactStoreError::ReadLink {
                    path: current_path,
                    message: error.to_string(),
                });
            }
        };
        if let Some(digest) = parse_artifact_link_target(&target) {
            let artifact = self.artifacts_path().join(digest.as_str());
            verify_artifact_file(&artifact, &digest).map_err(ArtifactStoreError::Verification)?;
            return Ok(Some(CurrentArtifactLink::DirectArtifact(digest)));
        }
        if target == self.previous_path() {
            let digest = self
                .read_verified_link(&self.previous_path())?
                .ok_or_else(|| ArtifactStoreError::PreviousMissing {
                    path: self.previous_path(),
                })?;
            return Ok(Some(CurrentArtifactLink::RevertedToPrevious(digest)));
        }
        Err(ArtifactStoreError::UnsafeLinkTarget {
            path: current_path,
            target,
        })
    }

    fn replace_link(&self, link: &Path, target: &Path) -> Result<(), ArtifactStoreError> {
        fs::create_dir_all(&self.state).map_err(|error| ArtifactStoreError::CreateState {
            path: self.state.clone(),
            message: error.to_string(),
        })?;
        let staged = staged_link_path(link)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &staged).map_err(|error| {
            ArtifactStoreError::CreateLink {
                link: staged.clone(),
                target: target.to_path_buf(),
                message: error.to_string(),
            }
        })?;
        #[cfg(not(unix))]
        {
            let _ = (link, target, staged);
            return Err(ArtifactStoreError::SymlinksUnsupported);
        }
        fs::rename(&staged, link).map_err(|error| ArtifactStoreError::PublishLink {
            staged: staged.clone(),
            link: link.to_path_buf(),
            message: error.to_string(),
        })?;
        sync_directory(&self.state).map_err(|error| ArtifactStoreError::SyncState {
            path: self.state.clone(),
            message: error.to_string(),
        })
    }

    fn remove_pending_marker(&self) -> Result<(), ArtifactStoreError> {
        match fs::remove_file(self.pending_path()) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(ArtifactStoreError::RemovePending {
                    path: self.pending_path(),
                    message: error.to_string(),
                });
            }
        }
        sync_directory(&self.state).map_err(|error| ArtifactStoreError::SyncState {
            path: self.state.clone(),
            message: error.to_string(),
        })
    }

    fn path_exists(&self, path: &Path) -> Result<bool, ArtifactStoreError> {
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(ArtifactStoreError::InspectPath {
                path: path.to_path_buf(),
                message: error.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmedPloyzdArtifact {
    pub previous: Sha256Digest,
    pub current: Sha256Digest,
    pub staged_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CurrentArtifactLink {
    DirectArtifact(Sha256Digest),
    RevertedToPrevious(Sha256Digest),
}

impl CurrentArtifactLink {
    fn digest(&self) -> &Sha256Digest {
        match self {
            Self::DirectArtifact(digest) | Self::RevertedToPrevious(digest) => digest,
        }
    }
}

fn artifact_link_target(digest: &Sha256Digest) -> PathBuf {
    PathBuf::from("artifacts").join(digest.as_str())
}

fn parse_artifact_link_target(target: &Path) -> Option<Sha256Digest> {
    let components = target.components().collect::<Vec<_>>();
    let [directory, digest] = components.as_slice() else {
        return None;
    };
    if directory.as_os_str() != "artifacts" {
        return None;
    }
    Sha256Digest::try_new(digest.as_os_str().to_str()?.to_owned()).ok()
}

fn staged_link_path(link: &Path) -> Result<PathBuf, ArtifactStoreError> {
    let file_name = link
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ArtifactStoreError::InvalidLinkPath {
            path: link.to_path_buf(),
        })?;
    let entropy = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ArtifactStoreError::ClockWentBackwards {
            message: error.to_string(),
        })?
        .as_nanos();
    Ok(link.with_file_name(format!(
        ".{file_name}.ployz-link-{}-{entropy}",
        std::process::id()
    )))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactStoreError {
    #[error("ployzd artifact state directory {} must be absolute", path.display())]
    RelativeStateDirectory { path: PathBuf },
    #[error("failed to stage verified ployzd artifact: {0}")]
    Stage(ArtifactInstallError),
    #[error("failed to verify artifact-store content: {0}")]
    Verification(ArtifactVerificationError),
    #[error("failed to create ployzd artifact state directory {}: {message}", path.display())]
    CreateState { path: PathBuf, message: String },
    #[error("failed to inspect artifact-store path {}: {message}", path.display())]
    InspectPath { path: PathBuf, message: String },
    #[error("artifact-store link {} is not a symbolic link", path.display())]
    LinkIsNotSymlink { path: PathBuf },
    #[error("failed to read artifact-store link {}: {message}", path.display())]
    ReadLink { path: PathBuf, message: String },
    #[error("artifact-store link {} has unsafe target {}", path.display(), target.display())]
    UnsafeLinkTarget { path: PathBuf, target: PathBuf },
    #[error("artifact-store link path {} has no UTF-8 file name", path.display())]
    InvalidLinkPath { path: PathBuf },
    #[error("system clock moved backwards while creating artifact-store link: {message}")]
    ClockWentBackwards { message: String },
    #[error("failed to create artifact-store link {} -> {}: {message}", link.display(), target.display())]
    CreateLink {
        link: PathBuf,
        target: PathBuf,
        message: String,
    },
    #[error("failed to publish staged artifact-store link {} to {}: {message}", staged.display(), link.display())]
    PublishLink {
        staged: PathBuf,
        link: PathBuf,
        message: String,
    },
    #[error("failed to sync artifact-store state directory {}: {message}", path.display())]
    SyncState { path: PathBuf, message: String },
    #[error("symlinks are unsupported on this host")]
    SymlinksUnsupported,
    #[error("ployzd current artifact is absent at {}", path.display())]
    CurrentMissing { path: PathBuf },
    #[error("ployzd previous artifact is absent at {}", path.display())]
    PreviousMissing { path: PathBuf },
    #[error("ployzd current artifact is already {}", digest.as_str())]
    TargetAlreadyCurrent { digest: Sha256Digest },
    #[error("ployzd current artifact is already {}, not requested {}", current.as_str(), requested.as_str())]
    CurrentAlreadySeeded {
        current: Sha256Digest,
        requested: Sha256Digest,
    },
    #[error("ployzd previous artifact exists without current at {}", path.display())]
    PreviousExistsWithoutCurrent { path: PathBuf },
    #[error("a ployzd upgrade is already pending at {}", path.display())]
    PendingUpgradeExists { path: PathBuf },
    #[error(
        "ployzd rollback to {} was interrupted while upgrade {} remained pending",
        current.as_str(),
        pending.as_str()
    )]
    InterruptedRollback {
        current: Sha256Digest,
        pending: Sha256Digest,
    },
    #[error("no ployzd upgrade is pending at {}", path.display())]
    PendingUpgradeMissing { path: PathBuf },
    #[error("failed to write durable pending upgrade marker {}: {message}", path.display())]
    WritePending { path: PathBuf, message: String },
    #[error("failed to remove pending upgrade marker {}: {message}", path.display())]
    RemovePending { path: PathBuf, message: String },
    #[error("failed to read pending upgrade marker {}: {message}", path.display())]
    ReadPending { path: PathBuf, message: String },
    #[error("pending upgrade marker {} is invalid: {message}", path.display())]
    InvalidPending { path: PathBuf, message: String },
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
