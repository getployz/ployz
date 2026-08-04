//! Installation contracts grouped by product responsibility.

pub mod artifacts;
pub mod paths;
pub mod roles;
pub mod validation;

pub use artifacts::{
    ExactPloyzVersion, ExactPloyzVersionError, InstallArtifactSource, InstallArtifactSpec,
    InstallArtifactVersion, InstallSha256Digest, ReleasePlatformFailure,
};
pub use paths::AbsoluteInstallPath;
pub use roles::HostPortAssurance;
pub use validation::InstallContractError;
