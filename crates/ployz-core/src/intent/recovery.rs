//! Recovery evidence carried beside durable intent without becoming intent.

use serde::{Deserialize, Serialize};

use crate::ids::MachineId;
use crate::install::HostPortAssurance;
use crate::machine::roles::InstallRolePolicy;
use crate::machine::{IssuedJoinToken, MachineName};

/// Epoch-stamped non-secret pending machine-add recovery hints mirrored for
/// core promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingMachineJoinRecoverySnapshot {
    pub epoch: ControlPlaneEpoch,
    pub pending: Vec<PendingMachineJoinRecovery>,
}

/// Non-secret pending machine-add recovery hint mirrored for core promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingMachineJoinRecovery {
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    #[serde(default = "HostPortAssurance::keeper")]
    pub host_port_assurance: HostPortAssurance,
    pub endpoint_subnet: crate::network::MachineEndpointSubnet,
    pub join_token: IssuedJoinToken,
}

/// Monotonic control-plane generation advertised with intent. It fences a
/// promoted core from the core it succeeds; NATS carries the value but does
/// not define it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ControlPlaneEpoch(u64);

impl ControlPlaneEpoch {
    /// The epoch a core mints before it has ever been promoted.
    #[must_use]
    pub const fn initial() -> Self {
        Self(1)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next generation, minted by a promotion to fence the core it succeeds.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}
