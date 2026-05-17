use std::collections::BTreeSet;
use std::fmt::{self, Display, Formatter};

use mvp_projection::{
    BackendEndpoint, DnsRecordFact, DnsRecordProjection, GatewayRouteProjection, ProjectionReport,
    RouteId, ServiceName,
};
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ServingCommitId(String);

impl ServingCommitId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ServingCommitId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RouteCommitId(String);

impl RouteCommitId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for RouteCommitId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GatewayCommitId(String);

impl GatewayCommitId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for GatewayCommitId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DnsCommitId(String);

impl DnsCommitId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for DnsCommitId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DeployNodeId(String);

impl DeployNodeId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for DeployNodeId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VisibleNode {
    node_id: DeployNodeId,
}

impl VisibleNode {
    #[must_use]
    pub fn new(node_id: DeployNodeId) -> Self {
        Self { node_id }
    }

    #[must_use]
    pub fn node_id(&self) -> &DeployNodeId {
        &self.node_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityReply {
    pub node_id: DeployNodeId,
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
    pub node_id: DeployNodeId,
    pub service: ServiceName,
    pub revision: RevisionId,
    pub capacity_requirement: InstanceCapacityRequirement,
}

impl InstancePlan {
    #[must_use]
    pub fn new(
        instance_id: InstanceId,
        node_id: DeployNodeId,
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
pub struct ServingCommitPlan {
    pub serving_commit_id: ServingCommitId,
    pub route_commit_id: RouteCommitId,
    pub gateway_commit_id: GatewayCommitId,
    pub dns_commit_id: DnsCommitId,
    pub route_id: RouteId,
    pub hostnames: Vec<String>,
    pub active_backends: Vec<BackendEndpoint>,
    pub old_backends_to_drain: Vec<BackendEndpoint>,
    pub dns_records: Vec<DnsRecordFact>,
    pub epoch: u64,
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
        node_id: DeployNodeId,
        cause: CleanupFailureKind,
    },
    StopUnavailable {
        node_id: DeployNodeId,
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
    pub visible_nodes: BTreeSet<VisibleNode>,
    pub serving_commit_id: Option<ServingCommitId>,
    pub drain_status: DrainStatus,
    pub cleanup_status: CleanupStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionCatchUp {
    serving_commit_id: ServingCommitId,
}

impl ProjectionCatchUp {
    pub fn from_report(
        commit: &ServingCommitPlan,
        report: &ProjectionReport,
    ) -> crate::DeployResult<Self> {
        let gateway = report
            .state
            .gateway
            .as_ref()
            .ok_or(crate::DeployError::ProjectionCatchUpMissing)?;
        let dns = report
            .state
            .dns
            .as_ref()
            .ok_or(crate::DeployError::ProjectionCatchUpMissing)?;
        if report.gateway_snapshot.is_none() || report.dns_snapshot.is_none() {
            return Err(crate::DeployError::ProjectionCatchUpMissing);
        }
        if gateway_snapshot_revision(report) != Some(expected_gateway_revision(commit).as_str())
            || dns_snapshot_revision(report) != Some(expected_dns_revision(commit).as_str())
            || gateway.gateway_commit_id != commit.gateway_commit_id.to_string()
            || gateway.route_commit_id != commit.route_commit_id.to_string()
            || gateway.routes != expected_gateway_routes(commit)
            || dns.dns_commit_id != commit.dns_commit_id.to_string()
            || dns.records != expected_dns_records(commit)
        {
            return Err(crate::DeployError::ProjectionCatchUpMismatch {
                serving_commit_id: commit.serving_commit_id.clone(),
            });
        }
        Ok(Self {
            serving_commit_id: commit.serving_commit_id.clone(),
        })
    }

    #[must_use]
    pub fn serving_commit_id(&self) -> &ServingCommitId {
        &self.serving_commit_id
    }
}

fn expected_gateway_revision(commit: &ServingCommitPlan) -> String {
    format!(
        "gateway:{}:{}",
        commit.gateway_commit_id, commit.route_commit_id
    )
}

fn expected_dns_revision(commit: &ServingCommitPlan) -> String {
    format!("dns:{}", commit.dns_commit_id)
}

fn gateway_snapshot_revision(report: &ProjectionReport) -> Option<&str> {
    report
        .gateway_snapshot
        .as_ref()
        .map(|snapshot| snapshot.revision.as_str())
}

fn dns_snapshot_revision(report: &ProjectionReport) -> Option<&str> {
    report
        .dns_snapshot
        .as_ref()
        .map(|snapshot| snapshot.revision.as_str())
}

fn expected_gateway_routes(commit: &ServingCommitPlan) -> Vec<GatewayRouteProjection> {
    let mut hostnames = commit.hostnames.clone();
    hostnames.sort();
    let mut backends = commit.active_backends.clone();
    backends.sort();
    let mut old_backends_to_drain = commit.old_backends_to_drain.clone();
    old_backends_to_drain.sort();
    vec![GatewayRouteProjection {
        route_id: commit.route_id.clone(),
        hostnames,
        backends,
        old_backends_to_drain,
    }]
}

fn expected_dns_records(commit: &ServingCommitPlan) -> Vec<DnsRecordProjection> {
    let mut records = commit
        .dns_records
        .clone()
        .into_iter()
        .map(DnsRecordProjection::from)
        .collect::<Vec<_>>();
    records.sort();
    records
}
