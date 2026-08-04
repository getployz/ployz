//! Machine role policy.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum GatewayRole {
    Install,
    Skip,
}

/// Which optional roles an installed machine runs next to its required
/// processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct InstallRolePolicy {
    pub gateway: GatewayRole,
}

impl InstallRolePolicy {
    #[must_use]
    pub const fn install_all() -> Self {
        Self {
            gateway: GatewayRole::Install,
        }
    }

    #[must_use]
    pub const fn without_gateway(mut self) -> Self {
        self.gateway = GatewayRole::Skip;
        self
    }
}
