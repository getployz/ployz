//! Transport-neutral contracts for bounded build executors.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{BuildAdapter, BuildSource, BuildSourceEvidence, VerifiedBuildSource};
use crate::deploy::PlatformImage;
use crate::ids::{BuildExecutorId, BuildPoolId, MachineId, OperationId};
use crate::image::OciPlatform;
use crate::machine::MachineUsabilityReason;
use crate::operation::{
    BuildLogChunk, BuildPlatformFailure, BuildToolchainEvidence, FailureMessage,
};

/// Durable identity of one external Build Executor within one Build Pool.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorIdentity {
    pub pool_id: BuildPoolId,
    pub executor_id: BuildExecutorId,
}

/// One point-of-use readiness answer from an external Build Executor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorReadinessAnswer<T> {
    pub identity: BuildExecutorIdentity,
    pub readiness: T,
}

/// Strict empty request for point-of-use external Build Executor readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorReadinessRequest {}

/// Native execution capability reported at the point of build admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorReadiness {
    pub native_platform: OciPlatform,
    pub capability: BuildExecutorCapability,
}

/// The complete adapter set one external executor can accept now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum BuildExecutorCapability {
    DockerfileAndRailpack,
    DockerfileOnly,
    RuntimeUnavailable,
}

/// Point-of-use Build Target capability testimony for the operator UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BuildTargetCapabilities {
    pub cluster: ClusterBuildTargetCapabilities,
    pub external_pools: Vec<ExternalBuildPoolCapabilities>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ClusterBuildTargetCapabilities {
    pub machines: Vec<ClusterBuildMachineCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "observation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClusterBuildMachineCapability {
    Answered {
        machine_id: MachineId,
        native_platform: OciPlatform,
        capability: BuildExecutorCapability,
    },
    Unavailable {
        machine_id: MachineId,
        reason: MachineUsabilityReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ExternalBuildPoolCapabilities {
    pub pool_id: BuildPoolId,
    pub executors: Vec<ExternalBuildExecutorCapability>,
    pub reachable_image_seeds: Vec<MachineId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "observation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExternalBuildExecutorCapability {
    Answered {
        identity: BuildExecutorIdentity,
        readiness: BuildExecutorReadiness,
    },
    Silent {
        identity: BuildExecutorIdentity,
    },
}

impl BuildExecutorCapability {
    #[must_use]
    pub const fn supports(self, adapter: &BuildAdapter) -> bool {
        match (self, adapter) {
            (Self::DockerfileAndRailpack, BuildAdapter::Dockerfile { .. })
            | (Self::DockerfileAndRailpack, BuildAdapter::Railpack { .. })
            | (Self::DockerfileOnly, BuildAdapter::Dockerfile { .. }) => true,
            (Self::DockerfileOnly, BuildAdapter::Railpack { .. })
            | (Self::RuntimeUnavailable, BuildAdapter::Dockerfile { .. })
            | (Self::RuntimeUnavailable, BuildAdapter::Railpack { .. }) => false,
        }
    }
}

/// Adapter identity used in typed admission failures without repeating adapter input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum BuildAdapterKind {
    Dockerfile,
    Railpack,
}

impl From<&BuildAdapter> for BuildAdapterKind {
    fn from(adapter: &BuildAdapter) -> Self {
        match adapter {
            BuildAdapter::Dockerfile { .. } => Self::Dockerfile,
            BuildAdapter::Railpack { .. } => Self::Railpack,
        }
    }
}

/// Readiness testimony for every executor in the enrolled known set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildExecutorReadinessTestimony<T> {
    Answered {
        identity: BuildExecutorIdentity,
        readiness: T,
    },
    Silent {
        identity: BuildExecutorIdentity,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildExecutorReadinessReconcileError {
    #[error("Build Executor known set contains duplicate identity {identity:?}")]
    DuplicateKnownIdentity { identity: BuildExecutorIdentity },
    #[error("un-enrolled Build Executor answered as {identity:?}")]
    UnknownIdentity { identity: BuildExecutorIdentity },
    #[error("Build Executor answered more than once as {identity:?}")]
    DuplicateResponse { identity: BuildExecutorIdentity },
}

/// Reconciles readiness answers against the known executor set.
pub fn reconcile_build_executor_readiness<T>(
    known: &[BuildExecutorIdentity],
    answers: impl IntoIterator<Item = BuildExecutorReadinessAnswer<T>>,
) -> Result<Vec<BuildExecutorReadinessTestimony<T>>, BuildExecutorReadinessReconcileError> {
    let mut testimony = BTreeMap::new();
    for identity in known {
        if testimony.insert(identity.clone(), None).is_some() {
            return Err(
                BuildExecutorReadinessReconcileError::DuplicateKnownIdentity {
                    identity: identity.clone(),
                },
            );
        }
    }

    for BuildExecutorReadinessAnswer {
        identity,
        readiness,
    } in answers
    {
        let Some(existing) = testimony.get_mut(&identity) else {
            return Err(BuildExecutorReadinessReconcileError::UnknownIdentity { identity });
        };
        if existing.replace(readiness).is_some() {
            return Err(BuildExecutorReadinessReconcileError::DuplicateResponse { identity });
        }
    }

    Ok(testimony
        .into_iter()
        .map(|(identity, readiness)| match readiness {
            Some(readiness) => BuildExecutorReadinessTestimony::Answered {
                identity,
                readiness,
            },
            None => BuildExecutorReadinessTestimony::Silent { identity },
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildTarget {
    Cluster,
    External { pool_id: BuildPoolId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "executor", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorAssignment {
    Cluster {
        machine_id: MachineId,
    },
    External {
        pool_id: BuildPoolId,
        executor_id: BuildExecutorId,
        image_seed: MachineId,
    },
}

impl BuildExecutorAssignment {
    #[must_use]
    pub fn origin(&self) -> BuildExecutorOrigin {
        match self {
            Self::Cluster { machine_id } => BuildExecutorOrigin::Cluster {
                machine_id: machine_id.clone(),
            },
            Self::External {
                pool_id,
                executor_id,
                image_seed: _,
            } => BuildExecutorOrigin::External {
                pool_id: pool_id.clone(),
                executor_id: executor_id.clone(),
            },
        }
    }

    #[must_use]
    pub const fn image_seed(&self) -> &MachineId {
        match self {
            Self::Cluster { machine_id } => machine_id,
            Self::External { image_seed, .. } => image_seed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalBuildExecutorCandidate {
    pub pool_id: BuildPoolId,
    pub executor_id: BuildExecutorId,
    pub platform: OciPlatform,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BuildPlatformExecutorAssignment {
    pub platform: OciPlatform,
    pub executor: BuildExecutorAssignment,
}

/// Canonical per-platform executor provenance for one build operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Array<BuildPlatformExecutorAssignment>"))]
#[serde(
    try_from = "Vec<BuildPlatformExecutorAssignment>",
    into = "Vec<BuildPlatformExecutorAssignment>"
)]
pub struct BuildExecutorAssignments(Vec<BuildPlatformExecutorAssignment>);

impl BuildExecutorAssignments {
    #[must_use]
    pub const fn empty() -> Self {
        Self(Vec::new())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &BuildPlatformExecutorAssignment> {
        self.0.iter()
    }
}

impl TryFrom<Vec<BuildPlatformExecutorAssignment>> for BuildExecutorAssignments {
    type Error = BuildExecutorAssignmentsError;

    fn try_from(assignments: Vec<BuildPlatformExecutorAssignment>) -> Result<Self, Self::Error> {
        let Some(first) = assignments.first() else {
            return Ok(Self::empty());
        };
        for (index, assignment) in assignments.iter().enumerate() {
            if assignments
                .iter()
                .take(index)
                .any(|placed| placed.platform == assignment.platform)
            {
                return Err(BuildExecutorAssignmentsError::DuplicatePlatform);
            }
            if !same_target(&first.executor, &assignment.executor) {
                return Err(BuildExecutorAssignmentsError::HeterogeneousTarget);
            }
        }
        Ok(Self(assignments))
    }
}

impl From<BuildExecutorAssignments> for Vec<BuildPlatformExecutorAssignment> {
    fn from(assignments: BuildExecutorAssignments) -> Self {
        assignments.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BuildExecutorAssignmentsError {
    #[error("build executor assignments contain a duplicate platform")]
    DuplicatePlatform,
    #[error("build executor assignments mix targets or external pools")]
    HeterogeneousTarget,
}

fn same_target(left: &BuildExecutorAssignment, right: &BuildExecutorAssignment) -> bool {
    match (left, right) {
        (BuildExecutorAssignment::Cluster { .. }, BuildExecutorAssignment::Cluster { .. }) => true,
        (
            BuildExecutorAssignment::External { pool_id: left, .. },
            BuildExecutorAssignment::External { pool_id: right, .. },
        ) => left == right,
        (BuildExecutorAssignment::Cluster { .. }, BuildExecutorAssignment::External { .. })
        | (BuildExecutorAssignment::External { .. }, BuildExecutorAssignment::Cluster { .. }) => {
            false
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalBuildPlacementError {
    NoCapableExecutor {
        pool_id: BuildPoolId,
        platform: OciPlatform,
    },
    NoReachableImageSeed {
        pool_id: BuildPoolId,
    },
}

/// Selects only from the capability inventory and reachable Machine seeds the
/// caller supplied. An unsolicited responder has no path into either set.
pub fn place_external_build_platforms(
    pool_id: &BuildPoolId,
    platforms: &super::BuildPlatforms,
    candidates: &[ExternalBuildExecutorCandidate],
    reachable_image_seeds: &BTreeSet<MachineId>,
) -> Result<Vec<BuildPlatformExecutorAssignment>, ExternalBuildPlacementError> {
    let mut assignments = Vec::new();
    for platform in platforms.iter() {
        let executor = candidates
            .iter()
            .filter(|candidate| candidate.pool_id == *pool_id && candidate.platform == *platform)
            .min_by(|left, right| left.executor_id.cmp(&right.executor_id))
            .ok_or_else(|| ExternalBuildPlacementError::NoCapableExecutor {
                pool_id: pool_id.clone(),
                platform: platform.clone(),
            })?;
        let image_seed = reachable_image_seeds
            .iter()
            .next()
            .cloned()
            .ok_or_else(|| ExternalBuildPlacementError::NoReachableImageSeed {
                pool_id: pool_id.clone(),
            })?;
        assignments.push(BuildPlatformExecutorAssignment {
            platform: platform.clone(),
            executor: BuildExecutorAssignment::External {
                pool_id: pool_id.clone(),
                executor_id: executor.executor_id.clone(),
                image_seed,
            },
        });
    }
    Ok(assignments)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "origin", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorOrigin {
    Cluster {
        machine_id: MachineId,
    },
    External {
        pool_id: BuildPoolId,
        executor_id: BuildExecutorId,
    },
}

/// Validated executor identity and image-seed provenance for build evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    try_from = "BuildExecutorEvidenceWire",
    into = "BuildExecutorEvidenceWire"
)]
pub struct BuildExecutorEvidence(BuildExecutorAssignment);

#[cfg(feature = "ts")]
impl ts_rs::TS for BuildExecutorEvidence {
    type WithoutGenerics = Self;
    type OptionInnerType = Self;

    fn name(_cfg: &ts_rs::Config) -> String {
        "BuildExecutorEvidence".to_owned()
    }

    fn decl(cfg: &ts_rs::Config) -> String {
        format!(
            "type BuildExecutorEvidence = {};",
            BuildExecutorEvidenceWire::inline(cfg)
        )
    }

    fn decl_concrete(cfg: &ts_rs::Config) -> String {
        Self::decl(cfg)
    }

    fn inline(cfg: &ts_rs::Config) -> String {
        BuildExecutorEvidenceWire::inline(cfg)
    }

    fn inline_flattened(cfg: &ts_rs::Config) -> String {
        BuildExecutorEvidenceWire::inline_flattened(cfg)
    }

    fn visit_dependencies(visitor: &mut impl ts_rs::TypeVisitor)
    where
        Self: 'static,
    {
        BuildExecutorEvidenceWire::visit_dependencies(visitor);
    }
}

impl BuildExecutorEvidence {
    #[must_use]
    pub fn from_assignment(assignment: &BuildExecutorAssignment) -> Self {
        Self(assignment.clone())
    }

    #[must_use]
    pub const fn machine_id(&self) -> &MachineId {
        self.0.image_seed()
    }

    #[must_use]
    pub const fn assignment(&self) -> &BuildExecutorAssignment {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildExecutorEvidenceError {
    #[error("cluster build evidence seed does not match its executor machine")]
    ClusterSeedMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
struct BuildExecutorEvidenceWire {
    machine_id: MachineId,
    executor_origin: BuildExecutorOrigin,
}

impl TryFrom<BuildExecutorEvidenceWire> for BuildExecutorEvidence {
    type Error = BuildExecutorEvidenceError;

    fn try_from(value: BuildExecutorEvidenceWire) -> Result<Self, Self::Error> {
        let assignment = match value.executor_origin {
            BuildExecutorOrigin::Cluster { machine_id } if machine_id == value.machine_id => {
                BuildExecutorAssignment::Cluster { machine_id }
            }
            BuildExecutorOrigin::Cluster { .. } => {
                return Err(BuildExecutorEvidenceError::ClusterSeedMismatch);
            }
            BuildExecutorOrigin::External {
                pool_id,
                executor_id,
            } => BuildExecutorAssignment::External {
                pool_id,
                executor_id,
                image_seed: value.machine_id,
            },
        };
        Ok(Self(assignment))
    }
}

impl From<BuildExecutorEvidence> for BuildExecutorEvidenceWire {
    fn from(value: BuildExecutorEvidence) -> Self {
        Self {
            machine_id: value.0.image_seed().clone(),
            executor_origin: value.0.origin(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorStartRequest {
    pub operation_id: OperationId,
    pub assignment: BuildExecutorAssignment,
    pub source: BuildSource,
    pub adapter: BuildAdapter,
    pub platform: OciPlatform,
    pub timeout_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct BuildRequestCommitment(String);

impl BuildRequestCommitment {
    fn from_start_request(request: &BuildExecutorStartRequest) -> Self {
        #[derive(Serialize)]
        struct Commitment<'a> {
            version: u8,
            operation_id: &'a OperationId,
            assignment: &'a BuildExecutorAssignment,
            source: BuildSourceEvidence,
            adapter: &'a BuildAdapter,
            platform: &'a OciPlatform,
            timeout_millis: u64,
        }

        let canonical = serde_json::to_vec(&Commitment {
            version: 1,
            operation_id: &request.operation_id,
            assignment: &request.assignment,
            source: request.source.evidence(),
            adapter: &request.adapter,
            platform: &request.platform,
            timeout_millis: request.timeout_millis,
        })
        .expect("validated build request serializes for commitment");
        Self(format!("{:x}", Sha256::digest(canonical)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BuildRequestCommitment {
    type Error = BuildRequestCommitmentError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(BuildRequestCommitmentError::Invalid);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

impl From<BuildRequestCommitment> for String {
    fn from(value: BuildRequestCommitment) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BuildRequestCommitmentError {
    #[error("build request commitment must be exactly 64 hexadecimal characters")]
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorAcceptance {
    pub operation_id: OperationId,
    pub assignment: BuildExecutorAssignment,
    pub platform: OciPlatform,
    pub request_commitment: BuildRequestCommitment,
}

impl BuildExecutorAcceptance {
    #[must_use]
    pub fn from_start_request(request: &BuildExecutorStartRequest) -> Self {
        Self {
            operation_id: request.operation_id.clone(),
            assignment: request.assignment.clone(),
            platform: request.platform.clone(),
            request_commitment: BuildRequestCommitment::from_start_request(request),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuildExecutorStartOk {
    pub acceptance: BuildExecutorAcceptance,
    #[serde(flatten)]
    pub completion: BuildExecutorCompletion,
}

impl<'de> Deserialize<'de> for BuildExecutorStartOk {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            acceptance: BuildExecutorAcceptance,
            cleanup: BuildExecutorSuccessCleanupEvidence,
            image: PlatformImage,
            verified_source: VerifiedBuildSource,
            toolchain: BuildToolchainEvidence,
            #[serde(flatten)]
            log_summary: BuildLogSummary,
        }

        let Wire {
            acceptance,
            cleanup,
            image,
            verified_source,
            toolchain,
            log_summary,
        } = Wire::deserialize(deserializer)?;
        Ok(Self {
            acceptance,
            completion: BuildExecutorCompletion {
                cleanup,
                image,
                verified_source,
                toolchain,
                log_summary,
            },
        })
    }
}

/// Positive proof required on every successful build-executor response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorSuccessCleanupEvidence {
    pub outcome: BuildExecutorSuccessCleanupOutcome,
}

impl BuildExecutorSuccessCleanupEvidence {
    #[must_use]
    pub const fn confirmed() -> Self {
        Self {
            outcome: BuildExecutorSuccessCleanupOutcome::Confirmed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildExecutorSuccessCleanupOutcome {
    Confirmed,
}

/// Terminal response from one exact external Build Executor start request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorStartResponse {
    Ok(Box<BuildExecutorStartOk>),
    DomainError {
        error: BuildExecutorStartDomainError,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildLogSummary {
    pub final_log_sequence: u64,
    pub omitted_log_bytes: u64,
}

impl BuildLogSummary {
    #[must_use]
    pub const fn new(final_log_sequence: u64, omitted_log_bytes: u64) -> Self {
        Self {
            final_log_sequence,
            omitted_log_bytes,
        }
    }

    #[must_use]
    pub const fn none() -> Self {
        Self::new(0, 0)
    }

    /// Returns true only when activity advanced without either monotonic
    /// component regressing.
    #[must_use]
    pub const fn strictly_advances(&self, previous: &Self) -> bool {
        self.final_log_sequence >= previous.final_log_sequence
            && self.omitted_log_bytes >= previous.omitted_log_bytes
            && (self.final_log_sequence > previous.final_log_sequence
                || self.omitted_log_bytes > previous.omitted_log_bytes)
    }
}

/// Exact accepted build identity queried from one machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorStatusRequest {
    pub acceptance: BuildExecutorAcceptance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorStatus {
    pub acceptance: BuildExecutorAcceptance,
    pub state: BuildExecutorState,
}

/// Current or terminal state of one accepted machine build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorState {
    Running {
        #[serde(flatten)]
        log_summary: BuildLogSummary,
    },
    Completed {
        result: Box<BuildExecutorCompletion>,
    },
    Failed {
        failure: BuildExecutorStatusFailure,
        cleanup: BuildExecutorCleanupOutcome,
        #[serde(flatten)]
        log_summary: BuildLogSummary,
    },
    Cancelled {
        cleanup: BuildExecutorCleanupOutcome,
        #[serde(flatten)]
        log_summary: BuildLogSummary,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorCompletion {
    pub cleanup: BuildExecutorSuccessCleanupEvidence,
    pub image: PlatformImage,
    pub verified_source: VerifiedBuildSource,
    pub toolchain: BuildToolchainEvidence,
    #[serde(flatten)]
    pub log_summary: BuildLogSummary,
}

impl BuildExecutorStatus {
    #[must_use]
    pub fn from_start_ok(result: BuildExecutorStartOk) -> Self {
        let BuildExecutorStartOk {
            acceptance,
            completion,
        } = result;
        Self {
            acceptance,
            state: BuildExecutorState::Completed {
                result: Box::new(completion),
            },
        }
    }

    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        !matches!(self.state, BuildExecutorState::Running { .. })
    }

    #[must_use]
    pub const fn log_summary(&self) -> BuildLogSummary {
        match &self.state {
            BuildExecutorState::Running { log_summary }
            | BuildExecutorState::Failed { log_summary, .. }
            | BuildExecutorState::Cancelled { log_summary, .. } => *log_summary,
            BuildExecutorState::Completed { result } => result.log_summary,
        }
    }
}

/// Typed reason an accepted machine build terminated unsuccessfully.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorStatusFailure {
    PlatformFailed { failure: BuildPlatformFailure },
    Stalled { message: FailureMessage },
}

/// Failure to obtain status evidence for one exact accepted build identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorStatusDomainError {
    NotFound {
        acceptance: BuildExecutorAcceptance,
    },
    EvidenceUnavailable {
        acceptance: BuildExecutorAcceptance,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorStartDomainError {
    OperationIdentityMismatch {
        expected: OperationId,
        actual: OperationId,
    },
    AssignmentMismatch {
        expected: Box<BuildExecutorAssignment>,
        actual: Box<BuildExecutorAssignment>,
    },
    ExecutorIdentityMismatch {
        expected: BuildExecutorIdentity,
        actual: Box<BuildExecutorAssignment>,
    },
    RuntimeUnavailable,
    RuntimeStopped,
    PlatformMismatch {
        expected: OciPlatform,
        actual: OciPlatform,
    },
    ToolchainUnavailable {
        adapter: BuildAdapterKind,
    },
    ImageSeedUnavailable {
        image_seed: MachineId,
    },
    InvalidTimeout {
        timeout_millis: u64,
    },
    AlreadyRunning,
    PlatformFailed {
        acceptance: Box<BuildExecutorAcceptance>,
        failure: BuildPlatformFailure,
        #[serde(flatten)]
        log_summary: BuildLogSummary,
    },
    Cancelled {
        acceptance: Box<BuildExecutorAcceptance>,
        cleanup: BuildExecutorCleanupOutcome,
        #[serde(flatten)]
        log_summary: BuildLogSummary,
    },
    TimedOut {
        acceptance: Box<BuildExecutorAcceptance>,
        message: FailureMessage,
        cleanup: BuildExecutorCleanupOutcome,
        #[serde(flatten)]
        log_summary: BuildLogSummary,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildExecutorCleanupOutcome {
    Confirmed,
    Unconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorCancelRequest {
    pub operation_id: OperationId,
    pub assignment: BuildExecutorAssignment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorCancelOk {
    pub assignment: BuildExecutorAssignment,
    pub outcome: BuildExecutorCancelOutcome,
}

/// Response from one exact external Build Executor cancellation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorCancelResponse {
    Ok(BuildExecutorCancelOk),
    DomainError {
        error: BuildExecutorCancelDomainError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorCancelOutcome {
    Requested,
    NotRunning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorCancelDomainError {
    AssignmentMismatch {
        expected: Box<BuildExecutorAssignment>,
        actual: BuildExecutorAssignment,
    },
    CancelFailed {
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorLogFrame {
    pub operation_id: OperationId,
    pub assignment: BuildExecutorAssignment,
    pub platform: OciPlatform,
    pub sequence: u64,
    pub chunk: BuildLogChunk,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::GitSource;

    #[test]
    fn external_executor_transport_envelopes_are_strict() {
        assert_eq!(
            serde_json::from_value::<BuildExecutorReadinessRequest>(serde_json::json!({}))
                .expect("empty readiness request"),
            BuildExecutorReadinessRequest {},
        );
        assert!(
            serde_json::from_value::<BuildExecutorReadinessRequest>(serde_json::json!({
                "unexpected": true
            }))
            .is_err()
        );

        let start = BuildExecutorStartResponse::DomainError {
            error: BuildExecutorStartDomainError::RuntimeUnavailable,
        };
        let start_json = serde_json::to_value(&start).expect("start response");
        assert_eq!(
            serde_json::from_value::<BuildExecutorStartResponse>(start_json.clone())
                .expect("strict start response"),
            start,
        );
        let mut invalid_start = start_json;
        invalid_start
            .as_object_mut()
            .expect("object")
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<BuildExecutorStartResponse>(invalid_start).is_err());

        let cancel = BuildExecutorCancelResponse::DomainError {
            error: BuildExecutorCancelDomainError::CancelFailed {
                message: FailureMessage::try_new("cancel failed").expect("message"),
            },
        };
        let cancel_json = serde_json::to_value(&cancel).expect("cancel response");
        assert_eq!(
            serde_json::from_value::<BuildExecutorCancelResponse>(cancel_json.clone())
                .expect("strict cancel response"),
            cancel,
        );
        let mut invalid_cancel = cancel_json;
        invalid_cancel
            .as_object_mut()
            .expect("object")
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<BuildExecutorCancelResponse>(invalid_cancel).is_err());
    }

    #[test]
    fn external_success_requires_confirmed_cleanup_evidence() {
        let request = start_request();
        let digest =
            crate::image::OciDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("digest");
        let success = BuildExecutorStartResponse::Ok(Box::new(BuildExecutorStartOk {
            acceptance: BuildExecutorAcceptance::from_start_request(&request),
            completion: BuildExecutorCompletion {
                cleanup: BuildExecutorSuccessCleanupEvidence::confirmed(),
                image: PlatformImage {
                    seed: request.assignment.image_seed().clone(),
                    manifest_digest: digest.clone(),
                    image_id: digest.clone(),
                    availability_expires_at: crate::deploy::ImageAvailabilityExpiresAt::try_new(
                        4_102_444_800,
                    )
                    .expect("expiry"),
                },
                verified_source: VerifiedBuildSource::from_source(&request.source),
                toolchain: BuildToolchainEvidence {
                    buildkit_image: digest,
                    adapter: crate::operation::BuildAdapterToolchainEvidence::Dockerfile,
                },
                log_summary: BuildLogSummary::new(8, 13),
            },
        }));
        let encoded = serde_json::to_value(&success).expect("success response");
        assert_eq!(
            encoded.get("cleanup"),
            Some(&serde_json::json!({"outcome": "confirmed"}))
        );
        assert_eq!(
            serde_json::from_value::<BuildExecutorStartResponse>(encoded.clone())
                .expect("strict success response"),
            success
        );

        let mut unconfirmed = encoded.clone();
        unconfirmed
            .get_mut("cleanup")
            .and_then(serde_json::Value::as_object_mut)
            .expect("cleanup object")
            .insert("outcome".to_owned(), serde_json::json!("unconfirmed"));
        assert!(serde_json::from_value::<BuildExecutorStartResponse>(unconfirmed).is_err());

        let mut unknown = encoded;
        unknown
            .get_mut("cleanup")
            .and_then(serde_json::Value::as_object_mut)
            .expect("cleanup object")
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<BuildExecutorStartResponse>(unknown).is_err());
    }

    #[test]
    fn readiness_reconcile_is_known_set_driven_and_preserves_silence() {
        let first = executor_identity("pool-a", "executor-a");
        let second = executor_identity("pool-a", "executor-b");
        let testimony = reconcile_build_executor_readiness(
            &[second.clone(), first.clone()],
            [BuildExecutorReadinessAnswer {
                identity: first.clone(),
                readiness: "ready",
            }],
        )
        .expect("known response");

        assert_eq!(
            testimony,
            vec![
                BuildExecutorReadinessTestimony::Answered {
                    identity: first,
                    readiness: "ready",
                },
                BuildExecutorReadinessTestimony::Silent { identity: second },
            ]
        );
    }

    #[test]
    fn readiness_reconcile_rejects_unknown_and_duplicate_answers() {
        let known = executor_identity("pool-a", "executor-a");
        let answer = |identity| BuildExecutorReadinessAnswer {
            identity,
            readiness: (),
        };

        for unknown in [
            executor_identity("pool-a", "unknown"),
            executor_identity("pool-b", "executor-a"),
        ] {
            assert!(matches!(
                reconcile_build_executor_readiness(std::slice::from_ref(&known), [answer(unknown)]),
                Err(BuildExecutorReadinessReconcileError::UnknownIdentity { .. })
            ));
        }
        assert!(matches!(
            reconcile_build_executor_readiness(
                std::slice::from_ref(&known),
                [answer(known.clone()), answer(known.clone())],
            ),
            Err(BuildExecutorReadinessReconcileError::DuplicateResponse { .. })
        ));
    }

    #[test]
    fn readiness_reconcile_rejects_duplicate_known_identity() {
        let identity = executor_identity("pool-a", "executor-a");
        assert!(matches!(
            reconcile_build_executor_readiness(
                &[identity.clone(), identity],
                std::iter::empty::<BuildExecutorReadinessAnswer<()>>(),
            ),
            Err(BuildExecutorReadinessReconcileError::DuplicateKnownIdentity { .. })
        ));
    }

    fn executor_identity(pool: &str, executor: &str) -> BuildExecutorIdentity {
        BuildExecutorIdentity {
            pool_id: BuildPoolId::try_new(pool).expect("pool id"),
            executor_id: BuildExecutorId::try_new(executor).expect("executor id"),
        }
    }

    fn cluster_assignment() -> BuildExecutorAssignment {
        BuildExecutorAssignment::Cluster {
            machine_id: MachineId::try_new("machine-a").expect("machine id"),
        }
    }

    fn platform(architecture: &str) -> OciPlatform {
        OciPlatform::try_new("linux", architecture).expect("platform")
    }

    fn placed(
        architecture: &str,
        executor: BuildExecutorAssignment,
    ) -> BuildPlatformExecutorAssignment {
        BuildPlatformExecutorAssignment {
            platform: platform(architecture),
            executor,
        }
    }

    fn start_request() -> BuildExecutorStartRequest {
        let source = GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "redacted-test-value",
            None::<String>,
        )
        .expect("source");
        BuildExecutorStartRequest {
            operation_id: OperationId::try_new("build-1").expect("operation id"),
            assignment: cluster_assignment(),
            source: source.into(),
            adapter: BuildAdapter::Dockerfile {
                dockerfile: super::super::BuildContextPath::try_new("Dockerfile")
                    .expect("dockerfile path"),
                target: None,
            },
            platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
            timeout_millis: 1_000,
        }
    }

    fn completed_result() -> BuildExecutorStartOk {
        let request = start_request();
        let digest =
            crate::image::OciDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("digest");
        BuildExecutorStartOk {
            acceptance: BuildExecutorAcceptance::from_start_request(&request),
            completion: BuildExecutorCompletion {
                cleanup: BuildExecutorSuccessCleanupEvidence::confirmed(),
                image: PlatformImage {
                    seed: request.assignment.image_seed().clone(),
                    manifest_digest: digest.clone(),
                    image_id: digest.clone(),
                    availability_expires_at: crate::deploy::ImageAvailabilityExpiresAt::try_new(
                        4_102_444_800,
                    )
                    .expect("expiry"),
                },
                verified_source: VerifiedBuildSource::from_source(&request.source),
                toolchain: BuildToolchainEvidence {
                    buildkit_image: digest,
                    adapter: crate::operation::BuildAdapterToolchainEvidence::Dockerfile,
                },
                log_summary: BuildLogSummary::new(8, 13),
            },
        }
    }

    #[test]
    fn readiness_answer_is_exact_and_capabilities_are_adapter_specific() {
        let answer = BuildExecutorReadinessAnswer {
            identity: executor_identity("pool-a", "executor-a"),
            readiness: BuildExecutorReadiness {
                native_platform: platform("amd64"),
                capability: BuildExecutorCapability::DockerfileOnly,
            },
        };

        assert_eq!(
            serde_json::to_value(answer).expect("readiness answer"),
            serde_json::json!({
                "identity": {"pool_id": "pool-a", "executor_id": "executor-a"},
                "readiness": {
                    "native_platform": {"os": "linux", "architecture": "amd64"},
                    "capability": "dockerfile_only",
                },
            })
        );
    }

    #[test]
    fn readiness_capability_distinguishes_runtime_and_railpack_availability() {
        let dockerfile = BuildAdapter::Dockerfile {
            dockerfile: super::super::BuildContextPath::try_new("Dockerfile")
                .expect("dockerfile path"),
            target: None,
        };
        let railpack = BuildAdapter::Railpack {
            cache_scope: super::super::BuildCacheScope::try_new("cache").expect("cache scope"),
        };

        assert!(BuildExecutorCapability::DockerfileOnly.supports(&dockerfile));
        assert!(!BuildExecutorCapability::DockerfileOnly.supports(&railpack));
        assert!(!BuildExecutorCapability::RuntimeUnavailable.supports(&dockerfile));
        assert!(BuildExecutorCapability::DockerfileAndRailpack.supports(&railpack));
    }

    #[test]
    fn external_pre_acceptance_errors_preserve_typed_provenance() {
        let error = BuildExecutorStartDomainError::ExecutorIdentityMismatch {
            expected: executor_identity("pool-a", "executor-a"),
            actual: Box::new(BuildExecutorAssignment::External {
                pool_id: BuildPoolId::try_new("pool-a").expect("pool"),
                executor_id: BuildExecutorId::try_new("executor-b").expect("executor"),
                image_seed: MachineId::try_new("seed-a").expect("seed"),
            }),
        };

        assert_eq!(
            serde_json::to_value(error).expect("start error"),
            serde_json::json!({
                "error": "executor_identity_mismatch",
                "expected": {"pool_id": "pool-a", "executor_id": "executor-a"},
                "actual": {
                    "executor": "external",
                    "pool_id": "pool-a",
                    "executor_id": "executor-b",
                    "image_seed": "seed-a",
                },
            })
        );
    }

    #[test]
    fn build_executor_ids_are_validated_stable_key_tokens() {
        assert!(BuildPoolId::try_new("pool-a").is_ok());
        assert!(BuildExecutorId::try_new("executor.a").is_err());
    }

    #[test]
    fn executor_origin_rejects_unknown_fields() {
        let origin = serde_json::json!({
            "origin": "cluster",
            "machine_id": "machine-a",
            "credential": "must-not-fit",
        });
        assert!(serde_json::from_value::<BuildExecutorOrigin>(origin).is_err());
    }

    #[test]
    fn executor_evidence_keeps_the_flat_checked_wire_shape() {
        let evidence = BuildExecutorEvidence::from_assignment(&cluster_assignment());
        let encoded = serde_json::to_value(evidence).expect("encode evidence");
        assert_eq!(
            encoded,
            serde_json::json!({
                "machine_id": "machine-a",
                "executor_origin": {"origin": "cluster", "machine_id": "machine-a"},
            })
        );

        let mut unknown = encoded;
        unknown
            .as_object_mut()
            .expect("evidence object")
            .insert("credential".into(), serde_json::json!("must-not-fit"));
        assert!(serde_json::from_value::<BuildExecutorEvidence>(unknown).is_err());
    }

    #[test]
    fn executor_assignments_reject_duplicate_platforms() {
        let encoded = serde_json::to_value([
            placed("amd64", cluster_assignment()),
            placed("amd64", cluster_assignment()),
        ])
        .expect("encode assignments");
        assert!(serde_json::from_value::<BuildExecutorAssignments>(encoded).is_err());
    }

    #[test]
    fn executor_assignments_use_a_transparent_array() {
        let assignments = BuildExecutorAssignments::try_from(vec![
            placed("amd64", cluster_assignment()),
            placed(
                "arm64",
                BuildExecutorAssignment::Cluster {
                    machine_id: MachineId::try_new("machine-b").expect("machine"),
                },
            ),
        ])
        .expect("assignments");
        let encoded = serde_json::to_value(&assignments).expect("encode assignments");
        assert!(encoded.is_array());
        assert_eq!(
            serde_json::from_value::<BuildExecutorAssignments>(encoded)
                .expect("decode assignments"),
            assignments
        );
    }

    #[test]
    fn executor_assignments_reject_mixed_targets_and_external_pools() {
        let external = |pool: &str, executor: &str| BuildExecutorAssignment::External {
            pool_id: BuildPoolId::try_new(pool).expect("pool"),
            executor_id: BuildExecutorId::try_new(executor).expect("executor"),
            image_seed: MachineId::try_new("seed-a").expect("seed"),
        };
        for assignments in [
            vec![
                placed("amd64", cluster_assignment()),
                placed("arm64", external("pool-a", "executor-a")),
            ],
            vec![
                placed("amd64", external("pool-a", "executor-a")),
                placed("arm64", external("pool-b", "executor-b")),
            ],
        ] {
            let encoded = serde_json::to_value(assignments).expect("encode assignments");
            assert!(serde_json::from_value::<BuildExecutorAssignments>(encoded).is_err());
        }
    }

    #[test]
    fn acceptance_is_an_exact_start_request_projection() {
        let request = start_request();
        let expected = BuildExecutorAcceptance::from_start_request(&request);

        assert_eq!(expected.operation_id, request.operation_id);
        assert_eq!(expected.assignment, request.assignment);
        assert_eq!(expected.platform, request.platform);
    }

    #[test]
    fn acceptance_commitment_excludes_credentials_but_covers_source_evidence() {
        let request = start_request();
        let mut rotated = start_request();
        rotated.source = GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "another-user",
            "another-secret",
            None::<String>,
        )
        .expect("rotated source")
        .into();
        assert_eq!(
            BuildExecutorAcceptance::from_start_request(&request).request_commitment,
            BuildExecutorAcceptance::from_start_request(&rotated).request_commitment
        );

        rotated.source = GitSource::try_new(
            "https://example.test/other.git",
            "0123456789abcdef0123456789abcdef01234567",
            "another-user",
            "another-secret",
            None::<String>,
        )
        .expect("different source")
        .into();
        assert_ne!(
            BuildExecutorAcceptance::from_start_request(&request).request_commitment,
            BuildExecutorAcceptance::from_start_request(&rotated).request_commitment
        );
    }

    #[test]
    fn log_summary_advances_only_component_wise_without_regression() {
        let previous = BuildLogSummary::new(8, 13);

        for newer in [
            BuildLogSummary::new(9, 13),
            BuildLogSummary::new(8, 14),
            BuildLogSummary::new(9, 14),
        ] {
            assert!(newer.strictly_advances(&previous));
        }
        for not_newer in [
            BuildLogSummary::new(8, 13),
            BuildLogSummary::new(7, 14),
            BuildLogSummary::new(9, 12),
            BuildLogSummary::new(7, 12),
        ] {
            assert!(!not_newer.strictly_advances(&previous));
        }
    }

    #[test]
    fn status_request_and_states_are_strict_roundtrips() {
        let acceptance = BuildExecutorAcceptance::from_start_request(&start_request());
        let request = BuildExecutorStatusRequest {
            acceptance: acceptance.clone(),
        };
        assert_strict_roundtrip(request);

        let states = [
            BuildExecutorStatus {
                acceptance: acceptance.clone(),
                state: BuildExecutorState::Running {
                    log_summary: BuildLogSummary::new(3, 5),
                },
            },
            BuildExecutorStatus::from_start_ok(completed_result()),
            BuildExecutorStatus {
                acceptance: acceptance.clone(),
                state: BuildExecutorState::Failed {
                    failure: BuildExecutorStatusFailure::PlatformFailed {
                        failure: BuildPlatformFailure::MachineUnavailable {
                            message: FailureMessage::try_new("machine restarted").expect("message"),
                        },
                    },
                    cleanup: BuildExecutorCleanupOutcome::Confirmed,
                    log_summary: BuildLogSummary::new(4, 7),
                },
            },
            BuildExecutorStatus {
                acceptance: acceptance.clone(),
                state: BuildExecutorState::Failed {
                    failure: BuildExecutorStatusFailure::Stalled {
                        message: FailureMessage::try_new("no verified progress").expect("message"),
                    },
                    cleanup: BuildExecutorCleanupOutcome::Unconfirmed,
                    log_summary: BuildLogSummary::new(5, 11),
                },
            },
            BuildExecutorStatus {
                acceptance,
                state: BuildExecutorState::Cancelled {
                    cleanup: BuildExecutorCleanupOutcome::Confirmed,
                    log_summary: BuildLogSummary::new(6, 13),
                },
            },
        ];

        for state in states {
            assert_strict_roundtrip(state);
        }
    }

    #[test]
    fn status_failure_and_evidence_errors_reject_unknown_fields() {
        let acceptance = BuildExecutorAcceptance::from_start_request(&start_request());
        for error in [
            BuildExecutorStatusDomainError::NotFound {
                acceptance: acceptance.clone(),
            },
            BuildExecutorStatusDomainError::EvidenceUnavailable {
                acceptance,
                message: FailureMessage::try_new("status record unreadable").expect("message"),
            },
        ] {
            assert_strict_roundtrip(error);
        }

        let invalid_failure = serde_json::json!({
            "status": "failed",
            "acceptance": {
                "operation_id": "build-1",
                "assignment": {"executor": "cluster", "machine_id": "machine-a"},
                "platform": {"os": "linux", "architecture": "amd64"},
            },
            "failure": {
                "kind": "stalled",
                "message": "no verified progress",
                "unexpected": true,
            },
            "cleanup": "confirmed",
            "final_log_sequence": 3,
            "omitted_log_bytes": 5,
        });
        assert!(serde_json::from_value::<BuildExecutorStatus>(invalid_failure).is_err());
    }

    fn assert_strict_roundtrip<T>(value: T)
    where
        T: std::fmt::Debug + PartialEq + Serialize + for<'de> Deserialize<'de>,
    {
        let encoded = serde_json::to_value(&value).expect("encode strict value");
        assert_eq!(
            serde_json::from_value::<T>(encoded.clone()).expect("decode strict value"),
            value
        );
        let mut unknown = encoded;
        unknown
            .as_object_mut()
            .expect("strict value object")
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<T>(unknown).is_err());
    }

    #[test]
    fn start_request_rejects_unknown_fields() {
        let mut encoded = serde_json::to_value(start_request()).expect("encode request");
        encoded
            .as_object_mut()
            .expect("request object")
            .insert("credential".to_owned(), serde_json::json!("must-not-fit"));

        assert!(serde_json::from_value::<BuildExecutorStartRequest>(encoded).is_err());
    }

    #[test]
    fn log_frame_rejects_unknown_or_oversized_payloads() {
        let frame = serde_json::json!({
            "operation_id": "build-1",
            "assignment": {"executor": "cluster", "machine_id": "machine-a"},
            "platform": {"os": "linux", "architecture": "amd64"},
            "sequence": 1,
            "chunk": "hello",
            "credential": "must-not-fit"
        });
        assert!(serde_json::from_value::<BuildExecutorLogFrame>(frame).is_err());

        let frame = serde_json::json!({
            "operation_id": "build-1",
            "assignment": {"executor": "cluster", "machine_id": "machine-a"},
            "platform": {"os": "linux", "architecture": "amd64"},
            "sequence": 1,
            "chunk": "x".repeat(crate::operation::MAX_BUILD_LOG_CHUNK_BYTES + 1)
        });
        assert!(serde_json::from_value::<BuildExecutorLogFrame>(frame).is_err());
    }
}
