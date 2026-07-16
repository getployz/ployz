//! Installation contracts grouped by product responsibility.

use std::time::Duration;

pub const MACHINE_SUBSTRATE_UPDATE_LEAK_BACKSTOP: Duration = Duration::from_secs(30 * 60);
pub const MACHINE_SUBSTRATE_UPDATE_TERMINATION_GRACE: Duration = Duration::from_secs(30);

pub mod artifacts;
pub mod join;
pub mod nats;
pub mod paths;
pub mod roles;
pub mod validation;

pub use artifacts::{
    FirstMachineInstallArtifacts, InstallArtifactSource, InstallArtifactSpec,
    InstallArtifactVersion, InstallSha256Digest, MachineJoinArtifactBundleSpec,
    NatsServerInstallSpec,
};
pub use join::{
    DEFAULT_MACHINE_BOOTSTRAP_URL, MachineBootstrapUrl, MachineJoinBundle, MachineJoinClusterName,
    MachineJoinMaterial, MachineJoinSecretDelivery, MachineJoinTemplate, WrappedCaKey,
    WrappedCoreSeeds,
};
pub use nats::{MachineJoinRuntimeNatsUrl, MachineJoinTrustedNats, NatsMachineMaterialPaths};
pub use paths::{AbsoluteInstallPath, INTENT_MIRROR_FILE_NAME};
pub use roles::{FirstMachineInstallSpec, HostPortAssurance};
pub use validation::{InstallContractError, is_valid_host_syntax};
