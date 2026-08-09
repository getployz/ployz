//! Coarse Corrosion summaries for cluster operations.
//!
//! The node-local workflow runtime owns execution detail. Corrosion exposes
//! only enough state for an operator to see whether a command was accepted,
//! finished, and what to do after failure.

use serde::{Deserialize, Serialize};

use super::document::{
    CorrosionDocumentVersion, CorrosionServiceName, CorrosionTimestamp, OperationDocument,
};
use super::principal::OperationInitiator;
use crate::ids::{ClusterId, MachineRowId, NamespaceRowId, ServiceRowId};
use crate::placement::PlacementRefusal;

/// The only two snapshots visible for a deploy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CorrosionDeployState {
    Created,
    Terminal {
        completed_at: CorrosionTimestamp,
        outcome: CorrosionDeployOutcome,
    },
}

/// A deploy's final public result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorrosionDeployOutcome {
    Completed {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<CorrosionDeployWarning>,
    },
    Failed {
        failure: CorrosionDeployFailure,
    },
    Interrupted,
}

impl CorrosionDeployOutcome {
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }
}

/// Useful terminal caveats that do not make a deploy fail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorrosionDeployWarning {
    HealthGateSkipped,
    CleanupIncomplete { machines: Vec<MachineRowId> },
}

/// Coarse, redaction-safe deploy failure classes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorrosionDeployFailure {
    DifferentService {
        incumbent_name: CorrosionServiceName,
    },
    MultipleServices,
    RoutesWithoutService,
    ReplicasOnGlobalService,
    UnknownPinnedMachine {
        machine_name: crate::machine::MachineName,
    },
    Placement {
        refusal: PlacementRefusal,
    },
    PrepareFailed {
        machine_id: MachineRowId,
    },
    PrepareRefused {
        machine_id: MachineRowId,
    },
    PreparedReplicaMismatch {
        machine_id: MachineRowId,
    },
    ResolvedImageMismatch,
    RuntimeRealityUnavailable,
}

impl OperationDocument {
    #[must_use]
    pub fn deploy_created(
        v: CorrosionDocumentVersion,
        cluster_id: ClusterId,
        machine_id: MachineRowId,
        initiator: OperationInitiator,
        namespace_id: NamespaceRowId,
        service_id: ServiceRowId,
        created_at: CorrosionTimestamp,
    ) -> Self {
        Self {
            v,
            cluster_id,
            machine_id,
            initiator,
            namespace_id,
            service_id,
            created_at,
            state: CorrosionDeployState::Created,
        }
    }

    #[must_use]
    pub fn deploy_state(&self) -> &CorrosionDeployState {
        &self.state
    }

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self.deploy_state(), CorrosionDeployState::Terminal { .. })
    }

    #[must_use]
    pub fn into_terminal(
        self,
        completed_at: CorrosionTimestamp,
        outcome: CorrosionDeployOutcome,
    ) -> Self {
        let Self {
            v,
            cluster_id,
            machine_id,
            initiator,
            namespace_id,
            service_id,
            created_at,
            ..
        } = self;
        Self {
            v,
            cluster_id,
            machine_id,
            initiator,
            namespace_id,
            service_id,
            created_at,
            state: CorrosionDeployState::Terminal {
                completed_at,
                outcome,
            },
        }
    }
}
