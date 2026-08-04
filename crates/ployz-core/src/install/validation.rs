//! Validation failures and shared validation policy for install contracts.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallContractError {
    #[error("artifact version is empty")]
    EmptyArtifactVersion,
    #[error("artifact source is empty")]
    EmptyArtifactSource,
    #[error("artifact source path {value} must be absolute")]
    RelativeArtifactSource { value: String },
    #[error("sha256 digest is empty")]
    EmptySha256Digest,
    #[error("sha256 digest {value:?} must be 64 hex characters")]
    InvalidSha256Digest { value: String },
    #[error("install path is empty")]
    EmptyInstallPath,
    #[error("install path {value} must be absolute")]
    RelativeInstallPath { value: String },
    #[error("install path {value} must include a parent")]
    MissingInstallParent { value: String },
    #[error("install path {value} must include a file name")]
    MissingInstallFileName { value: String },
}
