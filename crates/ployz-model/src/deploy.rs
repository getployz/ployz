use super::*;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, JsonSchema,
)]
pub enum InstancePhase {
    #[display("pending")]
    #[strum(serialize = "pending")]
    Pending,
    #[display("starting")]
    #[strum(serialize = "starting")]
    Starting,
    #[display("ready")]
    #[strum(serialize = "ready")]
    Ready,
    #[display("failed")]
    #[strum(serialize = "failed")]
    Failed,
    #[display("draining")]
    #[strum(serialize = "draining")]
    Draining,
    #[display("removed")]
    #[strum(serialize = "removed")]
    Removed,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, JsonSchema,
)]
pub enum DrainState {
    #[display("none")]
    #[strum(serialize = "none")]
    None,
    #[display("requested")]
    #[strum(serialize = "requested")]
    Requested,
    #[display("complete")]
    #[strum(serialize = "complete")]
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct InstanceStatusRecord {
    pub instance_id: InstanceId,
    pub namespace: Namespace,
    pub service: String,
    pub slot_id: SlotId,
    pub machine_id: MachineId,
    pub revision_hash: String,
    pub deploy_id: DeployId,
    pub docker_container_id: String,
    pub overlay_ip: Option<Ipv4Addr>,
    pub backend_ports: BTreeMap<String, u16>,
    pub phase: InstancePhase,
    pub ready: bool,
    pub drain_state: DrainState,
    pub error: Option<String>,
    pub started_at: u64,
    pub updated_at: u64,
}

impl<'de> Deserialize<'de> for InstanceStatusRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawInstanceStatusRecord {
            instance_id: InstanceId,
            namespace: Namespace,
            service: String,
            slot_id: SlotId,
            machine_id: MachineId,
            revision_hash: String,
            deploy_id: DeployId,
            docker_container_id: String,
            overlay_ip: Option<Ipv4Addr>,
            backend_ports: BTreeMap<String, u16>,
            phase: InstancePhase,
            ready: bool,
            drain_state: DrainState,
            error: Option<String>,
            started_at: u64,
            updated_at: u64,
        }

        let raw = RawInstanceStatusRecord::deserialize(deserializer)?;
        match raw.phase {
            InstancePhase::Ready if !raw.ready => {
                return Err(de::Error::custom(format!(
                    "ready instance '{}' must set ready=true",
                    raw.instance_id
                )));
            }
            InstancePhase::Failed if raw.error.is_none() => {
                return Err(de::Error::custom(format!(
                    "failed instance '{}' must carry an error",
                    raw.instance_id
                )));
            }
            InstancePhase::Draining
                if raw.ready || raw.drain_state == DrainState::None || raw.error.is_some() =>
            {
                return Err(de::Error::custom(format!(
                    "draining instance '{}' must be not-ready, have drain evidence, and no error",
                    raw.instance_id
                )));
            }
            InstancePhase::Pending
            | InstancePhase::Starting
            | InstancePhase::Ready
            | InstancePhase::Removed
                if raw.error.is_some() =>
            {
                return Err(de::Error::custom(format!(
                    "{} instance '{}' cannot carry failure error",
                    raw.phase, raw.instance_id
                )));
            }
            _ => {}
        }

        Ok(Self {
            instance_id: raw.instance_id,
            namespace: raw.namespace,
            service: raw.service,
            slot_id: raw.slot_id,
            machine_id: raw.machine_id,
            revision_hash: raw.revision_hash,
            deploy_id: raw.deploy_id,
            docker_container_id: raw.docker_container_id,
            overlay_ip: raw.overlay_ip,
            backend_ports: raw.backend_ports,
            phase: raw.phase,
            ready: raw.ready,
            drain_state: raw.drain_state,
            error: raw.error,
            started_at: raw.started_at,
            updated_at: raw.updated_at,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstanceStatusTransition {
    pub goal: InstanceStatusGoal,
    pub evidence: InstanceStatusEvidence,
    pub at_unix_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "goal", rename_all = "snake_case")]
pub enum InstanceStatusGoal {
    MarkDraining,
    MarkFailed { error: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstanceStatusEvidence {
    DeployCleanup { deploy_id: DeployId },
    RuntimeStart { deploy_id: DeployId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceStatusTransitionOutcome {
    Applied,
    AlreadyInState,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct InstanceStatusTransitionError {
    code: &'static str,
    message: String,
}

impl InstanceStatusTransitionError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl InstanceStatusRecord {
    pub fn apply_status_transition(
        &mut self,
        transition: InstanceStatusTransition,
    ) -> Result<InstanceStatusTransitionOutcome, InstanceStatusTransitionError> {
        let InstanceStatusTransition {
            goal,
            evidence: _,
            at_unix_secs,
        } = transition;
        match goal {
            InstanceStatusGoal::MarkDraining => {
                if self.phase == InstancePhase::Draining
                    && !self.ready
                    && self.drain_state == DrainState::Requested
                {
                    return Ok(InstanceStatusTransitionOutcome::AlreadyInState);
                }
                self.phase = InstancePhase::Draining;
                self.ready = false;
                self.drain_state = DrainState::Requested;
                self.error = None;
            }
            InstanceStatusGoal::MarkFailed { error } => {
                if self.phase == InstancePhase::Failed
                    && !self.ready
                    && self.error.as_deref() == Some(error.as_str())
                {
                    return Ok(InstanceStatusTransitionOutcome::AlreadyInState);
                }
                self.phase = InstancePhase::Failed;
                self.ready = false;
                self.drain_state = DrainState::None;
                self.error = Some(error);
            }
        }
        self.updated_at = at_unix_secs;
        Ok(InstanceStatusTransitionOutcome::Applied)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
pub enum DeployState {
    #[display("planning")]
    #[strum(serialize = "planning")]
    Planning,
    #[display("applying")]
    #[strum(serialize = "applying")]
    Applying,
    #[display("committed")]
    #[strum(serialize = "committed")]
    Committed,
    #[display("checkpoint_committed")]
    #[strum(serialize = "checkpoint_committed")]
    CheckpointCommitted,
    #[display("cleanup_pending")]
    #[strum(serialize = "cleanup_pending")]
    CleanupPending,
    #[display("failed_after_checkpoint")]
    #[strum(serialize = "failed_after_checkpoint")]
    FailedAfterCheckpoint,
    #[display("failed")]
    #[strum(serialize = "failed")]
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeployStateTransition {
    pub goal: DeployStateGoal,
    pub evidence: DeployTransitionEvidence,
    pub at_unix_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "goal", rename_all = "snake_case")]
pub enum DeployStateGoal {
    Commit { summary_json: String },
    MarkCleanupPending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeployTransitionEvidence {
    DeployExecutor { coordinator_machine_id: MachineId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployTransitionOutcome {
    Applied,
    AlreadyInState,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct DeployTransitionError {
    code: &'static str,
    message: String,
}

impl DeployTransitionError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "INVALID_TRANSITION",
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployRecordState {
    Planning {
        summary_json: String,
    },
    Applying {
        summary_json: String,
    },
    Committed {
        committed_at: u64,
        finished_at: u64,
        summary_json: String,
    },
    CheckpointCommitted {
        summary_json: String,
    },
    CleanupPending {
        committed_at: u64,
        finished_at: u64,
        summary_json: String,
    },
    FailedAfterCheckpoint {
        finished_at: u64,
        summary_json: String,
    },
    Failed {
        finished_at: u64,
        summary_json: String,
    },
}

impl DeployRecordState {
    #[must_use]
    pub fn state(&self) -> DeployState {
        match self {
            Self::Planning { .. } => DeployState::Planning,
            Self::Applying { .. } => DeployState::Applying,
            Self::Committed { .. } => DeployState::Committed,
            Self::CheckpointCommitted { .. } => DeployState::CheckpointCommitted,
            Self::CleanupPending { .. } => DeployState::CleanupPending,
            Self::FailedAfterCheckpoint { .. } => DeployState::FailedAfterCheckpoint,
            Self::Failed { .. } => DeployState::Failed,
        }
    }

    #[must_use]
    pub fn committed_at(&self) -> Option<u64> {
        match self {
            Self::Committed { committed_at, .. } | Self::CleanupPending { committed_at, .. } => {
                Some(*committed_at)
            }
            Self::Planning { .. }
            | Self::Applying { .. }
            | Self::CheckpointCommitted { .. }
            | Self::FailedAfterCheckpoint { .. }
            | Self::Failed { .. } => None,
        }
    }

    #[must_use]
    pub fn finished_at(&self) -> Option<u64> {
        match self {
            Self::Committed { finished_at, .. }
            | Self::CleanupPending { finished_at, .. }
            | Self::FailedAfterCheckpoint { finished_at, .. }
            | Self::Failed { finished_at, .. } => Some(*finished_at),
            Self::Planning { .. } | Self::Applying { .. } | Self::CheckpointCommitted { .. } => {
                None
            }
        }
    }

    #[must_use]
    pub fn summary_json(&self) -> &str {
        match self {
            Self::Planning { summary_json }
            | Self::Applying { summary_json }
            | Self::Committed { summary_json, .. }
            | Self::CheckpointCommitted { summary_json }
            | Self::CleanupPending { summary_json, .. }
            | Self::FailedAfterCheckpoint { summary_json, .. }
            | Self::Failed { summary_json, .. } => summary_json,
        }
    }

    pub fn set_summary_json(&mut self, next_summary_json: String) {
        match self {
            Self::Planning { summary_json }
            | Self::Applying { summary_json }
            | Self::Committed { summary_json, .. }
            | Self::CheckpointCommitted { summary_json }
            | Self::CleanupPending { summary_json, .. }
            | Self::FailedAfterCheckpoint { summary_json, .. }
            | Self::Failed { summary_json, .. } => *summary_json = next_summary_json,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRecord {
    pub deploy_id: DeployId,
    pub namespace: Namespace,
    pub coordinator_machine_id: MachineId,
    pub manifest_hash: String,
    pub started_at: u64,
    pub state: DeployRecordState,
}

impl DeployRecord {
    #[must_use]
    pub fn state(&self) -> DeployState {
        self.state.state()
    }

    #[must_use]
    pub fn committed_at(&self) -> Option<u64> {
        self.state.committed_at()
    }

    #[must_use]
    pub fn finished_at(&self) -> Option<u64> {
        self.state.finished_at()
    }

    #[must_use]
    pub fn summary_json(&self) -> &str {
        self.state.summary_json()
    }

    pub fn set_summary_json(&mut self, summary_json: String) {
        self.state.set_summary_json(summary_json);
    }

    pub fn mark_checkpoint_committed(&mut self, summary_json: String) {
        self.state = DeployRecordState::CheckpointCommitted { summary_json };
    }

    pub fn mark_committed(&mut self, committed_at: u64, finished_at: u64, summary_json: String) {
        self.state = DeployRecordState::Committed {
            committed_at,
            finished_at,
            summary_json,
        };
    }

    pub fn mark_cleanup_pending(&mut self, finished_at: u64) -> Result<(), DeployTransitionError> {
        let DeployRecordState::Committed {
            committed_at,
            summary_json,
            ..
        } = self.state.clone()
        else {
            return Err(DeployTransitionError::invalid(format!(
                "deploy '{}' must be committed before cleanup pending; current state is {}",
                self.deploy_id,
                self.state()
            )));
        };
        self.state = DeployRecordState::CleanupPending {
            committed_at,
            finished_at,
            summary_json,
        };
        Ok(())
    }

    pub fn mark_failed(&mut self, finished_at: u64, summary_json: String) {
        self.state = DeployRecordState::Failed {
            finished_at,
            summary_json,
        };
    }

    pub fn mark_failed_after_checkpoint(&mut self, finished_at: u64, summary_json: String) {
        self.state = DeployRecordState::FailedAfterCheckpoint {
            finished_at,
            summary_json,
        };
    }

    pub fn apply_state_transition(
        &mut self,
        transition: DeployStateTransition,
    ) -> Result<DeployTransitionOutcome, DeployTransitionError> {
        let DeployStateTransition {
            goal,
            evidence: _,
            at_unix_secs,
        } = transition;
        match goal {
            DeployStateGoal::Commit { summary_json } => {
                if self.state() == DeployState::Committed {
                    return Ok(DeployTransitionOutcome::AlreadyInState);
                }
                if self.state() != DeployState::Applying {
                    return Err(DeployTransitionError::invalid(format!(
                        "deploy '{}' must be applying before commit; current state is {}",
                        self.deploy_id,
                        self.state()
                    )));
                }
                self.mark_committed(at_unix_secs, at_unix_secs, summary_json);
            }
            DeployStateGoal::MarkCleanupPending => {
                if self.state() == DeployState::CleanupPending {
                    return Ok(DeployTransitionOutcome::AlreadyInState);
                }
                self.mark_cleanup_pending(at_unix_secs)?;
            }
        }
        Ok(DeployTransitionOutcome::Applied)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeRecord {
    pub namespace: Namespace,
    pub volume_name: String,
    pub scope: VolumeScope,
    pub machine_id: MachineId,
    pub quota: String,
    pub mode: String,
    pub owner: String,
    pub attached_services: Vec<String>,
    pub created_at: u64,
    pub created_by_deploy_id: DeployId,
    pub last_modified_at: u64,
    pub last_modified_by_deploy_id: DeployId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployChangeKind {
    Create,
    Replace,
    Remove,
    Unchanged,
}

validated_string_id!(pub struct DeployPhaseId("deploy phase id"););

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeployPhaseCommitPolicy {
    EndOfDeploy,
    Checkpoint,
    NoStoreCommit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeployPhaseRollbackPolicy {
    Reversible,
    ForwardOnly,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeployPhaseAdvancePolicy {
    Immediate,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeployPhaseWork {
    Service {
        service: String,
        action: DeployChangeKind,
    },
    Volume {
        volume: String,
        action: DeployChangeKind,
    },
    VolumeMove {
        volume: String,
        from_machine: MachineId,
        to_machine: MachineId,
        attached_services: Vec<String>,
    },
    VolumeClone {
        volume: String,
        source_namespace: Namespace,
        source_volume: String,
        source_machine: MachineId,
        target_machine: MachineId,
        data_policy: VolumeCloneDataPolicy,
        consistency: VolumeCloneConsistency,
        attached_services: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPhasePlan {
    pub phase_id: DeployPhaseId,
    pub name: String,
    pub order: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<DeployPhaseId>,
    pub participants: Vec<MachineId>,
    pub work: Vec<DeployPhaseWork>,
    pub commit_policy: DeployPhaseCommitPolicy,
    pub rollback_policy: DeployPhaseRollbackPolicy,
    pub advance_policy: DeployPhaseAdvancePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployPhaseState {
    Pending,
    Running,
    Succeeded {
        completed_at: u64,
    },
    Failed {
        completed_at: u64,
        failure: DeployPhaseFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPhaseFailure {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeployPhaseSuccess {
    EndOfDeploy {
        completed_at: u64,
        commit_deploy_id: DeployId,
    },
    Checkpoint {
        completed_at: u64,
        commit_deploy_id: DeployId,
    },
    NoStoreCommit {
        completed_at: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeployPhaseRecordState {
    Pending {
        commit_policy: DeployPhaseCommitPolicy,
    },
    Running {
        commit_policy: DeployPhaseCommitPolicy,
    },
    Succeeded {
        success: DeployPhaseSuccess,
    },
    Failed {
        commit_policy: DeployPhaseCommitPolicy,
        completed_at: u64,
        failure: DeployPhaseFailure,
    },
}

impl DeployPhaseRecordState {
    #[must_use]
    pub fn pending(commit_policy: DeployPhaseCommitPolicy) -> Self {
        Self::Pending { commit_policy }
    }

    #[must_use]
    pub fn running(commit_policy: DeployPhaseCommitPolicy) -> Self {
        Self::Running { commit_policy }
    }

    pub fn succeeded(
        commit_policy: DeployPhaseCommitPolicy,
        completed_at: u64,
        commit_deploy_id: Option<DeployId>,
    ) -> Result<Self, String> {
        let success = match commit_policy {
            DeployPhaseCommitPolicy::EndOfDeploy => DeployPhaseSuccess::EndOfDeploy {
                completed_at,
                commit_deploy_id: commit_deploy_id.ok_or_else(|| {
                    "end-of-deploy phase success requires commit deploy id".to_string()
                })?,
            },
            DeployPhaseCommitPolicy::Checkpoint => DeployPhaseSuccess::Checkpoint {
                completed_at,
                commit_deploy_id: commit_deploy_id.ok_or_else(|| {
                    "checkpoint phase success requires commit deploy id".to_string()
                })?,
            },
            DeployPhaseCommitPolicy::NoStoreCommit => {
                if commit_deploy_id.is_some() {
                    return Err("no-store phase success cannot carry commit deploy id".into());
                }
                DeployPhaseSuccess::NoStoreCommit { completed_at }
            }
        };
        Ok(Self::Succeeded { success })
    }

    #[must_use]
    pub fn failed(
        commit_policy: DeployPhaseCommitPolicy,
        completed_at: u64,
        failure: DeployPhaseFailure,
    ) -> Self {
        Self::Failed {
            commit_policy,
            completed_at,
            failure,
        }
    }

    #[must_use]
    pub fn lifecycle(&self) -> DeployPhaseState {
        match self {
            Self::Pending { .. } => DeployPhaseState::Pending,
            Self::Running { .. } => DeployPhaseState::Running,
            Self::Succeeded { success } => DeployPhaseState::Succeeded {
                completed_at: success.completed_at(),
            },
            Self::Failed {
                completed_at,
                failure,
                ..
            } => DeployPhaseState::Failed {
                completed_at: *completed_at,
                failure: failure.clone(),
            },
        }
    }

    #[must_use]
    pub fn commit_policy(&self) -> DeployPhaseCommitPolicy {
        match self {
            Self::Pending { commit_policy }
            | Self::Running { commit_policy }
            | Self::Failed { commit_policy, .. } => *commit_policy,
            Self::Succeeded { success } => success.commit_policy(),
        }
    }

    #[must_use]
    pub fn commit_deploy_id(&self) -> Option<DeployId> {
        match self {
            Self::Succeeded { success } => success.commit_deploy_id(),
            Self::Pending { .. } | Self::Running { .. } | Self::Failed { .. } => None,
        }
    }
}

impl DeployPhaseSuccess {
    #[must_use]
    pub fn completed_at(&self) -> u64 {
        match self {
            Self::EndOfDeploy { completed_at, .. }
            | Self::Checkpoint { completed_at, .. }
            | Self::NoStoreCommit { completed_at } => *completed_at,
        }
    }

    #[must_use]
    pub fn commit_policy(&self) -> DeployPhaseCommitPolicy {
        match self {
            Self::EndOfDeploy { .. } => DeployPhaseCommitPolicy::EndOfDeploy,
            Self::Checkpoint { .. } => DeployPhaseCommitPolicy::Checkpoint,
            Self::NoStoreCommit { .. } => DeployPhaseCommitPolicy::NoStoreCommit,
        }
    }

    #[must_use]
    pub fn commit_deploy_id(&self) -> Option<DeployId> {
        match self {
            Self::EndOfDeploy {
                commit_deploy_id, ..
            }
            | Self::Checkpoint {
                commit_deploy_id, ..
            } => Some(commit_deploy_id.clone()),
            Self::NoStoreCommit { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPhaseRecord {
    pub namespace: Namespace,
    pub deploy_id: DeployId,
    pub phase_id: DeployPhaseId,
    pub name: String,
    pub order: u32,
    pub after: Vec<DeployPhaseId>,
    pub participants: Vec<MachineId>,
    pub work: Vec<DeployPhaseWork>,
    pub state: DeployPhaseRecordState,
    pub rollback_policy: DeployPhaseRollbackPolicy,
    pub advance_policy: DeployPhaseAdvancePolicy,
    pub started_at: u64,
}

impl DeployPhaseRecord {
    #[must_use]
    pub fn lifecycle_state(&self) -> DeployPhaseState {
        self.state.lifecycle()
    }

    #[must_use]
    pub fn commit_policy(&self) -> DeployPhaseCommitPolicy {
        self.state.commit_policy()
    }

    #[must_use]
    pub fn commit_deploy_id(&self) -> Option<DeployId> {
        self.state.commit_deploy_id()
    }

    pub fn mark_succeeded(
        &mut self,
        commit_deploy_id: Option<DeployId>,
        completed_at: u64,
    ) -> Result<(), String> {
        self.state = DeployPhaseRecordState::succeeded(
            self.commit_policy(),
            completed_at,
            commit_deploy_id,
        )?;
        Ok(())
    }

    pub fn mark_failed(&mut self, completed_at: u64, failure: DeployPhaseFailure) {
        self.state = DeployPhaseRecordState::failed(self.commit_policy(), completed_at, failure);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotPlan {
    pub slot_id: SlotId,
    pub machine_id: MachineId,
    pub current_instance_id: Option<InstanceId>,
    pub next_instance_id: Option<InstanceId>,
    pub current_revision_hash: Option<String>,
    pub next_revision_hash: Option<String>,
    pub action: DeployChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServicePlan {
    pub service: String,
    pub current_revision_hash: Option<String>,
    pub next_revision_hash: Option<String>,
    pub slots: Vec<SlotPlan>,
    pub action: DeployChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceBranchSourcePlan {
    pub service: String,
    pub source_namespace: Namespace,
    pub source_service: String,
    pub source_revision_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceSourceMode {
    Fresh,
    Branch {
        source_namespace: Namespace,
        source_service: String,
        source_revision_hash: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceSourcePlan {
    pub service: String,
    pub mode: ServiceSourceMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployBaselineComponent {
    Manifest,
    Participants,
    Phases,
    Services,
    ServiceSources,
    Volumes,
    VolumeMoves,
    VolumeClones,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPreviewBaselineComponents {
    pub manifest: String,
    pub participants: String,
    pub phases: String,
    pub services: String,
    pub service_sources: String,
    pub volumes: String,
    pub volume_moves: String,
    pub volume_clones: String,
}

impl DeployPreviewBaselineComponents {
    #[must_use]
    pub fn ordered(&self) -> [(DeployBaselineComponent, &str); 8] {
        [
            (DeployBaselineComponent::Manifest, &self.manifest),
            (DeployBaselineComponent::Participants, &self.participants),
            (DeployBaselineComponent::Phases, &self.phases),
            (DeployBaselineComponent::Services, &self.services),
            (
                DeployBaselineComponent::ServiceSources,
                &self.service_sources,
            ),
            (DeployBaselineComponent::Volumes, &self.volumes),
            (DeployBaselineComponent::VolumeMoves, &self.volume_moves),
            (DeployBaselineComponent::VolumeClones, &self.volume_clones),
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeployPreviewBaseline {
    pub fingerprint: String,
    pub components: DeployPreviewBaselineComponents,
}

impl<'de> Deserialize<'de> for DeployPreviewBaseline {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawDeployPreviewBaseline {
            fingerprint: String,
            components: DeployPreviewBaselineComponents,
        }

        let raw = RawDeployPreviewBaseline::deserialize(deserializer)?;
        let baseline = Self::new(raw.components);
        if baseline.fingerprint != raw.fingerprint {
            return Err(de::Error::custom(format!(
                "deploy preview baseline fingerprint '{}' does not match canonical fingerprint '{}'",
                raw.fingerprint, baseline.fingerprint
            )));
        }
        Ok(baseline)
    }
}

impl DeployPreviewBaseline {
    #[must_use]
    pub fn new(components: DeployPreviewBaselineComponents) -> Self {
        let mut input = String::new();
        for (_, component_fingerprint) in components.ordered() {
            append_fingerprint_segment(&mut input, component_fingerprint);
        }
        Self {
            fingerprint: stable_hash_hex(input.as_bytes()),
            components,
        }
    }

    #[must_use]
    pub fn changed_components(&self, actual: &Self) -> Vec<DeployBaselineComponent> {
        let mut changed = Vec::new();
        for ((component, expected), (_, actual)) in self
            .components
            .ordered()
            .into_iter()
            .zip(actual.components.ordered())
        {
            if expected != actual {
                changed.push(component);
            }
        }
        changed
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fingerprint.is_empty()
    }

    #[must_use]
    pub fn is_canonical(&self) -> bool {
        self.fingerprint == Self::new(self.components.clone()).fingerprint
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployBaselineDiff {
    pub expected: DeployPreviewBaseline,
    pub actual: DeployPreviewBaseline,
}

impl DeployBaselineDiff {
    #[must_use]
    pub fn new(expected: DeployPreviewBaseline, actual: DeployPreviewBaseline) -> Self {
        Self { expected, actual }
    }

    #[must_use]
    pub fn changed_components(&self) -> Vec<DeployBaselineComponent> {
        self.expected.changed_components(&self.actual)
    }
}

impl std::fmt::Display for DeployBaselineDiff {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "deploy baseline changed: expected fingerprint '{}', got '{}'",
            self.expected.fingerprint, self.actual.fingerprint
        )
    }
}

#[must_use]
pub fn service_source_fingerprint(sources: &[ServiceSourcePlan]) -> String {
    if sources.is_empty() {
        return String::new();
    }

    let mut sources = sources.to_vec();
    sources.sort_by(|left, right| left.service.cmp(&right.service));

    let mut input = String::new();
    for source in sources {
        append_fingerprint_segment(&mut input, &source.service);
        match source.mode {
            ServiceSourceMode::Fresh => {
                input.push_str("fresh:");
            }
            ServiceSourceMode::Branch {
                source_namespace,
                source_service,
                source_revision_hash,
            } => {
                input.push_str("branch:");
                append_fingerprint_segment(&mut input, source_namespace.as_ref());
                append_fingerprint_segment(&mut input, &source_service);
                append_fingerprint_segment(&mut input, &source_revision_hash);
            }
        }
    }

    stable_hash_hex(input.as_bytes())
}

fn append_fingerprint_segment(input: &mut String, value: &str) {
    input.push_str(&value.len().to_string());
    input.push(':');
    input.push_str(value);
    input.push(';');
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeMovePlan {
    pub volume: String,
    pub from_machine: MachineId,
    pub to_machine: MachineId,
    pub attached_services: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeClonePlan {
    pub volume: String,
    pub source_namespace: Namespace,
    pub source_volume: String,
    pub source_machine: MachineId,
    pub target_machine: MachineId,
    pub data_policy: VolumeCloneDataPolicy,
    pub consistency: VolumeCloneConsistency,
    pub attached_services: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeClonePreflightAction {
    DrainAndRemoveBeforeCloneReplacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeClonePreflightScope {
    UncommittedNamespaceInstances,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeClonePreflightPlan {
    pub phase_id: DeployPhaseId,
    pub volumes: Vec<String>,
    pub action: VolumeClonePreflightAction,
    pub scope: VolumeClonePreflightScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployImageAvailabilityStatus {
    Present,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployImageAvailabilityPlan {
    pub service: String,
    pub slot_id: SlotId,
    pub machine_id: MachineId,
    pub image: String,
    pub digest: ImageDigest,
    pub status: DeployImageAvailabilityStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPreview {
    pub namespace: Namespace,
    pub manifest_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<DeployPreviewBaseline>,
    pub participants: Vec<MachineId>,
    pub phases: Vec<DeployPhasePlan>,
    pub services: Vec<ServicePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_sources: Vec<ServiceSourcePlan>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub service_source_fingerprint: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_branch_sources: Vec<ServiceBranchSourcePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_moves: Vec<VolumeMovePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_clones: Vec<VolumeClonePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_clone_preflights: Vec<VolumeClonePreflightPlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_availability: Vec<DeployImageAvailabilityPlan>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparedDeployState {
    Prepared,
    Applied,
    Expired,
    Superseded,
}

impl std::fmt::Display for PreparedDeployState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Prepared => "prepared",
            Self::Applied => "applied",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedDeployRecord {
    pub prepared_deploy_id: DeployId,
    pub namespace: Namespace,
    pub manifest_hash: String,
    pub manifest_json: String,
    pub preview: DeployPreview,
    pub baseline: DeployPreviewBaseline,
    pub coordinator_machine_id: MachineId,
    pub state: PreparedDeployState,
    pub created_at: u64,
    pub expires_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchEnvironmentState {
    Prepared,
    Applying,
    Active,
    Failed,
}

impl std::fmt::Display for BranchEnvironmentState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Prepared => "prepared",
            Self::Applying => "applying",
            Self::Active => "active",
            Self::Failed => "failed",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchEnvironmentResourceMode {
    Fresh,
    Branch,
}

impl std::fmt::Display for BranchEnvironmentResourceMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Fresh => "fresh",
            Self::Branch => "branch",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchEnvironmentResourceOverride {
    pub name: String,
    pub mode: BranchEnvironmentResourceMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchEnvironmentFailure {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_id: Option<DeployId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchEnvironmentRecord {
    pub source_namespace: Namespace,
    pub target_namespace: Namespace,
    pub state: BranchEnvironmentState,
    pub default_service_mode: BranchEnvironmentResourceMode,
    pub default_volume_mode: BranchEnvironmentResourceMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<BranchEnvironmentResourceOverride>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<BranchEnvironmentResourceOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_deploy_id: Option<DeployId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_deploy_id: Option<DeployId>,
    pub manifest_hash: String,
    pub baseline: DeployPreviewBaseline,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_branch_sources: Vec<ServiceBranchSourcePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_clones: Vec<VolumeClonePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_availability: Vec<DeployImageAvailabilityPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<BranchEnvironmentFailure>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchEnvironmentRecordValidationError {
    PreparedMissingPreparedDeploy,
    PreparedHasAppliedDeploy,
    PreparedHasFailure,
    ApplyingMissingPreparedDeploy,
    ApplyingHasAppliedDeploy,
    ApplyingHasFailure,
    ActiveMissingPreparedDeploy,
    ActiveMissingAppliedDeploy,
    ActiveHasFailure,
    FailedMissingPreparedDeploy,
    FailedMissingFailure,
}

impl std::fmt::Display for BranchEnvironmentRecordValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::PreparedMissingPreparedDeploy => {
                "prepared branch environment requires a prepared deploy id"
            }
            Self::PreparedHasAppliedDeploy => {
                "prepared branch environment cannot have an applied deploy id"
            }
            Self::PreparedHasFailure => "prepared branch environment cannot have a failure",
            Self::ApplyingMissingPreparedDeploy => {
                "applying branch environment requires a prepared deploy id"
            }
            Self::ApplyingHasAppliedDeploy => {
                "applying branch environment cannot have an applied deploy id"
            }
            Self::ApplyingHasFailure => "applying branch environment cannot have a failure",
            Self::ActiveMissingPreparedDeploy => {
                "active branch environment requires a prepared deploy id"
            }
            Self::ActiveMissingAppliedDeploy => {
                "active branch environment requires an applied deploy id"
            }
            Self::ActiveHasFailure => "active branch environment cannot have a failure",
            Self::FailedMissingPreparedDeploy => {
                "failed branch environment requires a prepared deploy id"
            }
            Self::FailedMissingFailure => "failed branch environment requires a failure",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for BranchEnvironmentRecordValidationError {}

impl BranchEnvironmentRecord {
    pub fn validate(&self) -> std::result::Result<(), BranchEnvironmentRecordValidationError> {
        match self.state {
            BranchEnvironmentState::Prepared => {
                if self.prepared_deploy_id.is_none() {
                    return Err(
                        BranchEnvironmentRecordValidationError::PreparedMissingPreparedDeploy,
                    );
                }
                if self.applied_deploy_id.is_some() {
                    return Err(BranchEnvironmentRecordValidationError::PreparedHasAppliedDeploy);
                }
                if self.failure.is_some() {
                    return Err(BranchEnvironmentRecordValidationError::PreparedHasFailure);
                }
            }
            BranchEnvironmentState::Applying => {
                if self.prepared_deploy_id.is_none() {
                    return Err(
                        BranchEnvironmentRecordValidationError::ApplyingMissingPreparedDeploy,
                    );
                }
                if self.applied_deploy_id.is_some() {
                    return Err(BranchEnvironmentRecordValidationError::ApplyingHasAppliedDeploy);
                }
                if self.failure.is_some() {
                    return Err(BranchEnvironmentRecordValidationError::ApplyingHasFailure);
                }
            }
            BranchEnvironmentState::Active => {
                if self.prepared_deploy_id.is_none() {
                    return Err(
                        BranchEnvironmentRecordValidationError::ActiveMissingPreparedDeploy,
                    );
                }
                if self.applied_deploy_id.is_none() {
                    return Err(BranchEnvironmentRecordValidationError::ActiveMissingAppliedDeploy);
                }
                if self.failure.is_some() {
                    return Err(BranchEnvironmentRecordValidationError::ActiveHasFailure);
                }
            }
            BranchEnvironmentState::Failed => {
                if self.prepared_deploy_id.is_none() {
                    return Err(
                        BranchEnvironmentRecordValidationError::FailedMissingPreparedDeploy,
                    );
                }
                if self.failure.is_none() {
                    return Err(BranchEnvironmentRecordValidationError::FailedMissingFailure);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployEvent {
    pub step: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployApplyResult {
    pub deploy_id: DeployId,
    pub preview: DeployPreview,
    pub state: DeployState,
    pub events: Vec<DeployEvent>,
}
