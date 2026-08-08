//! Small value types shared by otherwise independent product domains.

use serde::{Deserialize, Serialize};

mod routes;
mod text;

pub use routes::{RouteHostname, RouteHostnameError, RoutePort, RoutePortError, RouteTarget};
pub use text::{CancellationReason, FailureMessage, NonEmptyTextError};

/// A typed certificate issuance failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "class", rename_all = "snake_case", deny_unknown_fields)]
pub enum CertificateProvisionFailure {
    DnsPreflight {
        message: FailureMessage,
    },
    ChallengePublish {
        message: FailureMessage,
    },
    ChallengeReadiness {
        missing_machine_ids: Vec<crate::ids::MachineId>,
    },
    AcmeValidation {
        message: FailureMessage,
    },
    GatewayArtifactPush {
        machine_id: crate::ids::MachineId,
        message: FailureMessage,
    },
    ActiveCertCommit {
        attempted_active_cert: crate::certificate::ActiveCertState,
        message: FailureMessage,
    },
}
