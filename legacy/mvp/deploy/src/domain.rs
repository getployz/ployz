use std::fmt::{self, Display, Formatter};

use mvp_identity::{NodeId, VisibleNodes};
use mvp_projection::ServiceName;
use mvp_routing::{ServingCommitId, ServingCommitPlan};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DeployId(String);

impl DeployId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for DeployId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PhaseId(u32);

impl PhaseId {
    #[must_use]
    pub fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InstanceId(String);

impl InstanceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for InstanceId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RevisionId(String);

impl RevisionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityReply {
    pub node_id: NodeId,
    pub memory_free_bytes: u64,
    pub can_run_database: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceCapacityRequirement {
    General,
    Database,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapacityRejectionReason {
    NoFreeMemory,
    DatabaseUnsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstancePlan {
    pub instance_id: InstanceId,
    pub node_id: NodeId,
    pub service: ServiceName,
    pub revision: RevisionId,
    pub capacity_requirement: InstanceCapacityRequirement,
}

impl InstancePlan {
    #[must_use]
    pub fn new(
        instance_id: InstanceId,
        node_id: NodeId,
        service: ServiceName,
        revision: RevisionId,
        capacity_requirement: InstanceCapacityRequirement,
    ) -> Self {
        Self {
            instance_id,
            node_id,
            service,
            revision,
            capacity_requirement,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhaseReversibility {
    Reversible,
    Irreversible,
}

impl PhaseReversibility {
    #[must_use]
    pub fn is_irreversible(self) -> bool {
        matches!(self, Self::Irreversible)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServingPublication {
    NoChange,
    Commit,
}

impl ServingPublication {
    #[must_use]
    pub fn commits_serving(self) -> bool {
        matches!(self, Self::Commit)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhasePolicy {
    pub reversibility: PhaseReversibility,
    pub serving: ServingPublication,
}

impl PhasePolicy {
    #[must_use]
    pub fn new(reversibility: PhaseReversibility, serving: ServingPublication) -> Self {
        Self {
            reversibility,
            serving,
        }
    }

    #[must_use]
    pub fn reversible() -> Self {
        Self::new(PhaseReversibility::Reversible, ServingPublication::NoChange)
    }

    #[must_use]
    pub fn irreversible() -> Self {
        Self::new(
            PhaseReversibility::Irreversible,
            ServingPublication::NoChange,
        )
    }

    #[must_use]
    pub fn serving(reversibility: PhaseReversibility) -> Self {
        Self::new(reversibility, ServingPublication::Commit)
    }

    #[must_use]
    pub fn is_irreversible(self) -> bool {
        self.reversibility.is_irreversible()
    }

    #[must_use]
    pub fn commits_serving(self) -> bool {
        self.serving.commits_serving()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhasePlan {
    pub phase_id: PhaseId,
    pub instances: Vec<InstancePlan>,
    pub policy: PhasePolicy,
}

impl PhasePlan {
    #[must_use]
    pub fn new(phase_id: PhaseId, instances: Vec<InstancePlan>, policy: PhasePolicy) -> Self {
        Self {
            phase_id,
            instances,
            policy,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployManifest {
    pub deploy_id: DeployId,
    pub phases: Vec<PhasePlan>,
    pub serving_commit: ServingCommitPlan,
}

impl DeployManifest {
    #[must_use]
    pub fn new(
        deploy_id: DeployId,
        phases: Vec<PhasePlan>,
        serving_commit: ServingCommitPlan,
    ) -> Self {
        Self {
            deploy_id,
            phases,
            serving_commit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DrainStatus {
    NotStarted,
    Started,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CleanupStatus {
    NotNeeded,
    Done,
    Pending { reason: CleanupPendingReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CleanupPendingReason {
    DrainUnavailable {
        node_id: NodeId,
        cause: CleanupFailureKind,
    },
    StopUnavailable {
        node_id: NodeId,
        cause: CleanupFailureKind,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CleanupFailureKind {
    NoResponders,
    Timeout,
    HandlerFailed,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateCleanupState {
    Planned,
    PrepareAttempted,
    Prepared,
    Started,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateCleanupTarget {
    pub instance_id: InstanceId,
    pub service: ServiceName,
    pub revision: RevisionId,
    pub state: CandidateCleanupState,
}

impl CandidateCleanupTarget {
    #[must_use]
    pub fn new(
        instance_id: InstanceId,
        service: ServiceName,
        revision: RevisionId,
        state: CandidateCleanupState,
    ) -> Self {
        Self {
            instance_id,
            service,
            revision,
            state,
        }
    }

    #[must_use]
    pub fn from_instance(instance: &InstancePlan, state: CandidateCleanupState) -> Self {
        Self::new(
            instance.instance_id.clone(),
            instance.service.clone(),
            instance.revision.clone(),
            state,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCandidateCleanup {
    pub node_id: NodeId,
    pub candidates: Vec<CandidateCleanupTarget>,
}

impl NodeCandidateCleanup {
    #[must_use]
    pub fn new(node_id: NodeId, candidates: Vec<CandidateCleanupTarget>) -> Self {
        Self {
            node_id,
            candidates,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateCleanupFailure {
    pub node_id: NodeId,
    pub candidates: Vec<CandidateCleanupTarget>,
    pub cause: CleanupFailureKind,
}

impl CandidateCleanupFailure {
    #[must_use]
    pub fn new(
        node_id: NodeId,
        candidates: Vec<CandidateCleanupTarget>,
        cause: CleanupFailureKind,
    ) -> Self {
        Self {
            node_id,
            candidates,
            cause,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateCleanupStatus {
    NotNeeded,
    Done,
    Pending {
        failures: Vec<CandidateCleanupFailure>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreCommitCleanupReport {
    pub deploy_id: DeployId,
    pub visible_nodes: VisibleNodes,
    pub attempted: Vec<NodeCandidateCleanup>,
    pub status: CandidateCleanupStatus,
}

impl PreCommitCleanupReport {
    #[must_use]
    pub fn new(
        deploy_id: DeployId,
        visible_nodes: VisibleNodes,
        attempted: Vec<NodeCandidateCleanup>,
        status: CandidateCleanupStatus,
    ) -> Self {
        Self {
            deploy_id,
            visible_nodes,
            attempted,
            status,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeployOutcome {
    DeployDone,
    FailedBeforeCommit,
    DeployBlockedAfterIrreversiblePhase,
    CleanupPending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployCommandResult {
    pub deploy_id: DeployId,
    pub outcome: DeployOutcome,
    pub visible_nodes: VisibleNodes,
    pub serving_commit_id: Option<ServingCommitId>,
    pub drain_status: DrainStatus,
    pub cleanup_status: CleanupStatus,
}
