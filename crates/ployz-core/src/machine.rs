//! Machine identity and machine-local runtime contracts.

pub mod lifecycle;
pub mod roles;
pub mod runtime;
pub mod storage;
pub mod testimony;

pub use lifecycle::{MachineLifecycle, MachineUsabilityReason, placement_rejection};
pub use roles::{GatewayRole, InstallRolePolicy};
pub use runtime::*;
pub use storage::*;
pub use testimony::{
    GatewayHttpFailure, GatewayProcessAttempt, GatewayProcessHealth, GatewayServingStatus,
    GatewayStatusObservation, GatewayStatusPublishFailure, GatewayWatchFailure,
};

use serde::{Deserialize, Serialize};

use crate::ids::{SubjectToken, SubjectTokenError};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"MachineName\">"))]
#[serde(transparent)]
pub struct MachineName(SubjectToken);

impl MachineName {
    pub fn try_new(value: impl Into<String>) -> Result<Self, SubjectTokenError> {
        Ok(Self(SubjectToken::try_new(value)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
