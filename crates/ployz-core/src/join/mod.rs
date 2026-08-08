//! Join-token, public-door, and machine-arrival contracts.

mod acceptance;
mod admission;
mod arrival;
mod substrate;
mod token;

pub use acceptance::*;
pub use admission::*;
pub use arrival::*;
pub use substrate::*;
pub use token::*;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JoinAcceptanceValidationError {
    #[error("accepted join response belongs to a different cluster")]
    ClusterMismatch,
    #[error("accepted join identity disagrees with the request")]
    AcceptedIdentityMismatch,
    #[error("accepted join name disagrees with the request")]
    AcceptedNameMismatch,
    #[error("accepted join transport disagrees with the request")]
    AcceptedTransportMismatch,
    #[error("accepted join response carries a different door fingerprint")]
    DoorFingerprintMismatch,
    #[error("accepted join response has no reachable seed machine")]
    SeedIsNotReachable,
    #[error("accepted join response has an invalid Corrosion gossip seed")]
    InvalidCorrosionSeed,
    #[error("accepted join response uses a different mesh provider")]
    ProviderMismatch,
    #[error("accepted machine address does not derive from its cluster and key")]
    AcceptedAddressMismatch,
    #[error("accepted machine subnet is outside the cluster prefix")]
    EndpointSubnetOutsideClusterPrefix,
    #[error("the joining machine cannot bootstrap Corrosion from itself")]
    SeedIsJoiningMachine,
    #[error("seed machine transport does not match the cluster provider")]
    SeedProviderMismatch,
    #[error("seed machine address does not derive from its cluster and key")]
    SeedAddressMismatch,
    #[error("seed machine subnet is outside the cluster prefix")]
    SeedSubnetOutsideClusterPrefix,
    #[error("Corrosion bootstrap address does not name the seed machine")]
    CorrosionSeedDoesNotMatchMachine,
    #[error("accepted machine substrate has no exact Corrosion version")]
    MissingCorrosionVersion,
    #[error("accepted machine substrate is missing install artifact {install_path:?}")]
    MissingInstallArtifact { install_path: String },
    #[error("accepted machine substrate repeats install artifact {install_path:?}")]
    DuplicateInstallArtifact { install_path: String },
}
