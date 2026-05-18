use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures_util::{StreamExt, TryStreamExt, stream};
use mvp_bus::{
    BusActorHandle, BusError, BusSession, IslandId, RequestManyPolicy, RequestTarget, Subject,
};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_projection::FactSource;
use mvp_routing::{RoutingError, read_exact_serving_commit};

use crate::facts::{
    BusDeployFactWriter, DeployCleanupDoneFact, DeployDecisionCandidate, DeployDecisionFact,
    DeployFactWriter, read_deploy_cleanup_done, read_deploy_decision,
};
use crate::serving_commit::{BusServingFactWriter, ServingFactWriter};
use crate::wire::{
    CapacityRequest, CleanupDeployCandidatesRequest, DrainInstanceRequest, InstanceCommandReply,
    InstanceCommandRequest, InstanceStartOutcome, StopInstanceRequest, decode,
    decode_capacity_reply, encode,
};
use crate::{
    CandidateCleanupFailure, CandidateCleanupState, CandidateCleanupStatus, CandidateCleanupTarget,
    CapacityRejectionReason, CapacityReply, CleanupFailureKind, CleanupPendingReason,
    CleanupStatus, DeployCommandResult, DeployError, DeployId, DeployManifest, DeployResult,
    DeployStateMachine, InstanceCapacityRequirement, InstanceId, InstancePlan,
    NodeCandidateCleanup, PreCommitCleanupReport, ProjectionCatchUp,
};

const CAPACITY_PROBE_CONCURRENCY: usize = 32;
const CANDIDATE_CLEANUP_CONCURRENCY: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeployTimeouts {
    pub capacity: Duration,
    pub participant: Duration,
}

pub struct DeployCoordinator<W = BusDeployFactWriter, S = BusServingFactWriter> {
    bus: BusActorHandle,
    session: BusSession,
    fact_writer: W,
    serving_writer: S,
    timeouts: DeployTimeouts,
}

#[derive(Debug)]
pub struct PendingCleanup {
    manifest: DeployManifest,
    state: DeployStateMachine,
}

#[derive(Debug)]
pub struct ProjectedPendingCleanup {
    pending: PendingCleanup,
}

#[derive(Debug)]
pub struct RecoveredPendingCleanup {
    pending: PendingCleanup,
    superseded_decisions: Vec<DeployDecisionCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreCommitIncompleteRecovery {
    pub manifest: DeployManifest,
    pub visible_nodes: VisibleNodes,
    pub superseded_decisions: Vec<DeployDecisionCandidate>,
}

#[derive(Debug)]
pub enum DeployRecovery {
    Pending(Box<RecoveredPendingCleanup>),
    PreCommitIncomplete(Box<PreCommitIncompleteRecovery>),
    CleanupDone(DeployCommandResult),
}

impl PendingCleanup {
    pub fn after_projection(
        self,
        projection: ProjectionCatchUp,
    ) -> DeployResult<ProjectedPendingCleanup> {
        if projection.serving_commit_id() != &self.manifest.serving_commit.serving_commit_id {
            return Err(DeployError::ProjectionCatchUpMismatch {
                serving_commit_id: self.manifest.serving_commit.serving_commit_id.clone(),
            });
        }
        Ok(ProjectedPendingCleanup { pending: self })
    }
}

impl RecoveredPendingCleanup {
    pub fn after_projection(
        self,
        projection: ProjectionCatchUp,
    ) -> DeployResult<ProjectedPendingCleanup> {
        self.pending.after_projection(projection)
    }

    #[must_use]
    pub fn manifest(&self) -> &DeployManifest {
        &self.pending.manifest
    }

    #[must_use]
    pub fn superseded_decisions(&self) -> &[DeployDecisionCandidate] {
        &self.superseded_decisions
    }
}

#[derive(Debug, Default)]
struct CandidateCleanupTracker {
    by_node: BTreeMap<NodeId, BTreeMap<InstanceId, CandidateCleanupTarget>>,
}

impl CandidateCleanupTracker {
    fn from_manifest_planned(manifest: &DeployManifest) -> Self {
        let mut tracker = Self::default();
        tracker.track_manifest_planned(manifest);
        tracker
    }

    fn from_recovery_planned(recovery: &PreCommitIncompleteRecovery) -> Self {
        let mut tracker = Self::from_manifest_planned(&recovery.manifest);
        for decision in &recovery.superseded_decisions {
            tracker.track_manifest_planned(&decision.fact.manifest);
        }
        tracker
    }

    fn track_manifest_planned(&mut self, manifest: &DeployManifest) {
        for phase in &manifest.phases {
            for instance in &phase.instances {
                self.track(instance, CandidateCleanupState::Planned);
            }
        }
    }

    fn track(&mut self, instance: &InstancePlan, state: CandidateCleanupState) {
        self.by_node
            .entry(instance.node_id.clone())
            .or_default()
            .insert(
                instance.instance_id.clone(),
                CandidateCleanupTarget::from_instance(instance, state),
            );
    }

    fn discard_prepare_attempt_without_dispatch(&mut self, instance: &InstancePlan) {
        let Some(instances) = self.by_node.get_mut(&instance.node_id) else {
            return;
        };
        let remove = instances
            .get(&instance.instance_id)
            .is_some_and(|target| target.state == CandidateCleanupState::PrepareAttempted);
        if remove {
            instances.remove(&instance.instance_id);
        }
        if instances.is_empty() {
            self.by_node.remove(&instance.node_id);
        }
    }

    fn node_targets(&self) -> Vec<NodeCandidateCleanup> {
        self.by_node
            .iter()
            .map(|(node_id, instances)| {
                NodeCandidateCleanup::new(node_id.clone(), instances.values().cloned().collect())
            })
            .collect()
    }
}

impl DeployCoordinator<BusDeployFactWriter, BusServingFactWriter> {
    #[must_use]
    pub fn new(bus: BusActorHandle, session: BusSession, timeouts: DeployTimeouts) -> Self {
        let fact_writer = BusDeployFactWriter::new(bus.clone(), session.clone());
        let serving_writer = BusServingFactWriter::new(bus.clone(), session.clone());
        Self {
            bus,
            session,
            fact_writer,
            serving_writer,
            timeouts,
        }
    }
}

impl<W, S> DeployCoordinator<W, S>
where
    W: DeployFactWriter,
    S: ServingFactWriter,
{
    #[must_use]
    pub fn with_fact_writers(
        bus: BusActorHandle,
        session: BusSession,
        fact_writer: W,
        serving_writer: S,
        timeouts: DeployTimeouts,
    ) -> Self {
        Self {
            bus,
            session,
            fact_writer,
            serving_writer,
            timeouts,
        }
    }

    pub async fn execute_until_serving_commit(
        &self,
        manifest: DeployManifest,
    ) -> DeployResult<PendingCleanup> {
        validate_manifest(&manifest)?;
        let mut state = DeployStateMachine::new(
            manifest.deploy_id.clone(),
            manifest.phases.iter().map(|phase| phase.phase_id),
        );
        let capacities = self.inspect_capacity(&manifest).await?;
        validate_planned_capacity(&manifest, &capacities)?;
        let visible_nodes = VisibleNodes::new(capacities.keys().cloned());
        self.fact_writer
            .write_decision(DeployDecisionFact::new(
                manifest.clone(),
                visible_nodes.clone(),
            ))
            .await?;
        state.record_visible_nodes(visible_nodes);
        let mut candidates = CandidateCleanupTracker::default();

        for phase in &manifest.phases {
            state.mark_preparing(phase.phase_id)?;
            for instance in &phase.instances {
                candidates.track(instance, CandidateCleanupState::PrepareAttempted);
                if let Err(error) = self.prepare_instance(&manifest, instance).await {
                    if prepare_failed_before_dispatch(&error) {
                        candidates.discard_prepare_attempt_without_dispatch(instance);
                    }
                    return Err(self
                        .classify_or_cleanup_pre_commit_failure(
                            &mut state,
                            &manifest,
                            &candidates,
                            error,
                        )
                        .await);
                }
                candidates.track(instance, CandidateCleanupState::Prepared);
                let reply = match self.start_instance(&manifest, instance).await {
                    Ok(reply) => reply,
                    Err(error) => {
                        return Err(self
                            .classify_or_cleanup_pre_commit_failure(
                                &mut state,
                                &manifest,
                                &candidates,
                                error,
                            )
                            .await);
                    }
                };
                candidates.track(instance, CandidateCleanupState::Started);
                if let Err(error) = classify_start_reply(&mut state, instance, reply) {
                    return Err(self
                        .classify_or_cleanup_pre_commit_failure(
                            &mut state,
                            &manifest,
                            &candidates,
                            error,
                        )
                        .await);
                }
            }
            state.mark_ready(phase.phase_id)?;
            state.commit_phase(phase.phase_id, phase.policy)?;
            if phase.policy.commits_serving() {
                if let Err(error) = self
                    .serving_writer
                    .write_serving_commit(&manifest.serving_commit)
                    .await
                {
                    return Err(self
                        .classify_or_cleanup_pre_commit_failure(
                            &mut state,
                            &manifest,
                            &candidates,
                            error,
                        )
                        .await);
                }
                state.commit_serving(manifest.serving_commit.serving_commit_id.clone())?;
                return Ok(PendingCleanup { manifest, state });
            }
        }

        Err(DeployError::ServingCommitPhaseRequired)
    }

    pub async fn finish_cleanup(
        &self,
        mut projected: ProjectedPendingCleanup,
    ) -> DeployResult<DeployCommandResult> {
        let pending = &mut projected.pending;
        let manifest = &pending.manifest;
        let state = &mut pending.state;

        if let Some(result) = self
            .cleanup_old_backends(state, manifest, CleanupParticipantOp::Drain)
            .await?
        {
            return Ok(result);
        }
        if let Some(result) = self
            .cleanup_old_backends(state, manifest, CleanupParticipantOp::Stop)
            .await?
        {
            return Ok(result);
        }

        let cleanup_done = DeployCleanupDoneFact::new(manifest);
        let result = state.finish_cleanup()?;
        self.fact_writer.write_cleanup_done(cleanup_done).await?;
        Ok(result)
    }

    pub fn recover_pending_cleanup(
        &self,
        source: &dyn FactSource,
        island: &IslandId,
        fact_session: &BusSession,
        deploy_id: &crate::DeployId,
    ) -> DeployResult<DeployRecovery> {
        let selection = read_deploy_decision(source, island, fact_session, deploy_id)?;
        let decision = selection.winner.fact;
        match read_exact_serving_commit(
            source,
            island,
            fact_session,
            &decision.manifest.serving_commit,
        ) {
            Ok(_serving) => {}
            Err(RoutingError::ServingFactMissing { .. }) => {
                return Ok(DeployRecovery::PreCommitIncomplete(Box::new(
                    PreCommitIncompleteRecovery {
                        manifest: decision.manifest,
                        visible_nodes: decision.visible_nodes,
                        superseded_decisions: selection.superseded,
                    },
                )));
            }
            Err(error) => return Err(error.into()),
        }

        if let Some(cleanup_done) =
            read_deploy_cleanup_done(source, island, fact_session, deploy_id)?
        {
            validate_cleanup_done_fact(&decision.manifest, &cleanup_done)?;
            let mut state = recovered_cleanup_state(&decision)?;
            return Ok(DeployRecovery::CleanupDone(state.finish_cleanup()?));
        }

        Ok(DeployRecovery::Pending(Box::new(RecoveredPendingCleanup {
            pending: PendingCleanup {
                state: recovered_cleanup_state(&decision)?,
                manifest: decision.manifest,
            },
            superseded_decisions: selection.superseded,
        })))
    }

    pub async fn cleanup_pre_commit_incomplete(
        &self,
        recovery: &PreCommitIncompleteRecovery,
    ) -> PreCommitCleanupReport {
        let candidates = CandidateCleanupTracker::from_recovery_planned(recovery);
        self.cleanup_candidate_targets(
            &recovery.manifest.deploy_id,
            recovery.visible_nodes.clone(),
            candidates.node_targets(),
        )
        .await
    }

    async fn cleanup_old_backends(
        &self,
        state: &mut DeployStateMachine,
        manifest: &DeployManifest,
        op: CleanupParticipantOp,
    ) -> DeployResult<Option<DeployCommandResult>> {
        for backend in &manifest.serving_commit.old_backends_to_drain {
            let subject = match cleanup_subject(backend.node_id.as_str(), op) {
                Ok(subject) => subject,
                Err(error) => {
                    return state
                        .cleanup_pending(CleanupStatus::Pending {
                            reason: op.cleanup_pending_reason(&backend.node_id, &error),
                        })
                        .map(Some);
                }
            };
            let request = match op {
                CleanupParticipantOp::Drain => match encode(
                    &DrainInstanceRequest {
                        deploy_id: manifest.deploy_id.clone(),
                        cleanup_target: backend.clone(),
                    },
                    "drain instance request",
                ) {
                    Ok(request) => request,
                    Err(error) => {
                        return state
                            .cleanup_pending(CleanupStatus::Pending {
                                reason: op.cleanup_pending_reason(&backend.node_id, &error),
                            })
                            .map(Some);
                    }
                },
                CleanupParticipantOp::Stop => match encode(
                    &StopInstanceRequest {
                        deploy_id: manifest.deploy_id.clone(),
                        cleanup_target: backend.clone(),
                    },
                    "stop instance request",
                ) {
                    Ok(request) => request,
                    Err(error) => {
                        return state
                            .cleanup_pending(CleanupStatus::Pending {
                                reason: op.cleanup_pending_reason(&backend.node_id, &error),
                            })
                            .map(Some);
                    }
                },
            };
            match self.request_encoded_unit(subject, request).await {
                Ok(()) => {
                    if matches!(op, CleanupParticipantOp::Drain) {
                        state.mark_drain_started();
                    }
                }
                Err(error) => {
                    return state
                        .cleanup_pending(CleanupStatus::Pending {
                            reason: op.cleanup_pending_reason(&backend.node_id, &error),
                        })
                        .map(Some);
                }
            }
        }
        Ok(None)
    }

    async fn classify_or_cleanup_pre_commit_failure(
        &self,
        state: &mut DeployStateMachine,
        manifest: &DeployManifest,
        candidates: &CandidateCleanupTracker,
        error: DeployError,
    ) -> DeployError {
        if state.has_irreversible_commit() || state.has_serving_commit() {
            let _ = state.block_after_irreversible();
            return DeployError::BlockedAfterIrreversiblePhase;
        }
        let cleanup = self
            .cleanup_candidate_targets(
                &manifest.deploy_id,
                state.visible_nodes().clone(),
                candidates.node_targets(),
            )
            .await;
        if cleanup.status == CandidateCleanupStatus::NotNeeded {
            error
        } else {
            DeployError::PreCommitFailed {
                source: Box::new(error),
                cleanup,
            }
        }
    }

    async fn cleanup_candidate_targets(
        &self,
        deploy_id: &DeployId,
        visible_nodes: VisibleNodes,
        attempted: Vec<NodeCandidateCleanup>,
    ) -> PreCommitCleanupReport {
        if attempted.is_empty() {
            return PreCommitCleanupReport::new(
                deploy_id.clone(),
                visible_nodes,
                attempted,
                CandidateCleanupStatus::NotNeeded,
            );
        }

        let failures = stream::iter(attempted.iter())
            .map(|target| async move {
                self.cleanup_node_candidates(deploy_id, target)
                    .await
                    .err()
                    .map(|error| {
                        CandidateCleanupFailure::new(
                            target.node_id.clone(),
                            target.candidates.clone(),
                            cleanup_failure_kind(&error),
                        )
                    })
            })
            .buffered(CANDIDATE_CLEANUP_CONCURRENCY)
            .filter_map(|failure| async move { failure })
            .collect::<Vec<_>>()
            .await;
        let status = if failures.is_empty() {
            CandidateCleanupStatus::Done
        } else {
            CandidateCleanupStatus::Pending { failures }
        };
        PreCommitCleanupReport::new(deploy_id.clone(), visible_nodes, attempted, status)
    }

    async fn cleanup_node_candidates(
        &self,
        deploy_id: &DeployId,
        target: &NodeCandidateCleanup,
    ) -> DeployResult<()> {
        let subject = candidate_cleanup_subject(target.node_id.as_str())?;
        let request = CleanupDeployCandidatesRequest {
            deploy_id: deploy_id.clone(),
            candidates: target.candidates.clone(),
        };
        self.request_unit(subject, &request).await
    }

    async fn inspect_capacity(
        &self,
        manifest: &DeployManifest,
    ) -> DeployResult<BTreeMap<NodeId, CapacityReply>> {
        let request = CapacityRequest {
            deploy_id: manifest.deploy_id.clone(),
        };
        let payload = encode(&request, "capacity request")?;
        stream::iter(planned_node_ids(manifest))
            .map(|node_id| {
                let payload = payload.clone();
                async move { self.probe_node_capacity(node_id, payload).await }
            })
            .buffer_unordered(CAPACITY_PROBE_CONCURRENCY)
            .try_filter_map(|reply| async move {
                Ok(reply.map(|capacity| (capacity.node_id.clone(), capacity)))
            })
            .try_collect()
            .await
    }

    async fn probe_node_capacity(
        &self,
        node_id: NodeId,
        payload: Vec<u8>,
    ) -> DeployResult<Option<CapacityReply>> {
        let subject = Subject::parse(format!("node.{}.capacity", node_id.as_str()))?;
        let replies = match self
            .bus
            .request_many(
                &self.session,
                RequestTarget::Subject(subject.clone()),
                subject,
                payload,
                RequestManyPolicy::new(1, self.timeouts.capacity),
            )
            .await
        {
            Ok(replies) => replies,
            Err(BusError::NoResponders { .. }) => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        let Some(reply) = replies.first() else {
            return Ok(None);
        };
        let capacity = decode_capacity_reply(reply.payload())?;
        if capacity.node_id != node_id {
            return Err(DeployError::CapacityReplyNodeMismatch {
                subject_node_id: node_id,
                payload_node_id: capacity.node_id,
            });
        }
        Ok(Some(capacity))
    }

    async fn prepare_instance(
        &self,
        manifest: &DeployManifest,
        instance: &InstancePlan,
    ) -> DeployResult<()> {
        let subject = instance_subject(instance.node_id.as_str(), InstanceParticipantOp::Prepare)?;
        let request = instance_command_request(manifest, instance);
        self.request_unit(subject, &request).await
    }

    async fn start_instance(
        &self,
        manifest: &DeployManifest,
        instance: &InstancePlan,
    ) -> DeployResult<InstanceCommandReply> {
        let subject = instance_subject(instance.node_id.as_str(), InstanceParticipantOp::Start)?;
        let request = instance_command_request(manifest, instance);
        let response = self
            .bus
            .request(
                &self.session,
                subject,
                encode(&request, "start instance request")?,
                self.timeouts.participant,
            )
            .await?;
        decode(response.payload(), "start instance reply")
    }

    async fn request_unit<T: serde::Serialize>(
        &self,
        subject: Subject,
        request: &T,
    ) -> DeployResult<()> {
        self.request_encoded_unit(subject, encode(request, "participant request")?)
            .await
    }

    async fn request_encoded_unit(&self, subject: Subject, request: Vec<u8>) -> DeployResult<()> {
        self.bus
            .request(&self.session, subject, request, self.timeouts.participant)
            .await
            .map(|_| ())
            .map_err(DeployError::from)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstanceParticipantOp {
    Prepare,
    Start,
}

impl InstanceParticipantOp {
    fn subject_suffix(self) -> &'static str {
        match self {
            Self::Prepare => "prepare_instance",
            Self::Start => "start_instance",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupParticipantOp {
    Drain,
    Stop,
}

impl CleanupParticipantOp {
    fn subject_suffix(self) -> &'static str {
        match self {
            Self::Drain => "drain_instance",
            Self::Stop => "stop_instance",
        }
    }

    fn cleanup_pending_reason(self, node_id: &NodeId, error: &DeployError) -> CleanupPendingReason {
        let cause = cleanup_failure_kind(error);
        match self {
            Self::Drain => CleanupPendingReason::DrainUnavailable {
                node_id: node_id.clone(),
                cause,
            },
            Self::Stop => CleanupPendingReason::StopUnavailable {
                node_id: node_id.clone(),
                cause,
            },
        }
    }
}

fn instance_subject(node_id: &str, op: InstanceParticipantOp) -> DeployResult<Subject> {
    participant_subject(node_id, op.subject_suffix())
}

fn cleanup_subject(node_id: &str, op: CleanupParticipantOp) -> DeployResult<Subject> {
    participant_subject(node_id, op.subject_suffix())
}

fn candidate_cleanup_subject(node_id: &str) -> DeployResult<Subject> {
    participant_subject(node_id, "cleanup_deploy_candidates")
}

fn participant_subject(node_id: &str, suffix: &str) -> DeployResult<Subject> {
    Ok(Subject::parse(format!("node.{node_id}.rpc.{suffix}"))?)
}

fn instance_command_request(
    manifest: &DeployManifest,
    instance: &InstancePlan,
) -> InstanceCommandRequest {
    InstanceCommandRequest {
        deploy_id: manifest.deploy_id.clone(),
        instance_id: instance.instance_id.clone(),
        service: instance.service.clone(),
        revision: instance.revision.clone(),
    }
}

fn recovered_cleanup_state(decision: &DeployDecisionFact) -> DeployResult<DeployStateMachine> {
    DeployStateMachine::recover_pending_cleanup(
        decision.deploy_id.clone(),
        decision.manifest.phases.iter().map(|phase| phase.phase_id),
        decision.visible_nodes.clone(),
        decision.expected_serving_commit_id.clone(),
    )
}

fn validate_cleanup_done_fact(
    manifest: &DeployManifest,
    cleanup_done: &DeployCleanupDoneFact,
) -> DeployResult<()> {
    if cleanup_done.deploy_id == manifest.deploy_id
        && cleanup_done.serving_commit_id == manifest.serving_commit.serving_commit_id
        && cleanup_done.cleanup_targets == manifest.serving_commit.old_backends_to_drain
        && cleanup_done.serving_epoch == manifest.serving_commit.epoch
    {
        Ok(())
    } else {
        Err(DeployError::DeployFactMismatch {
            deploy_id: manifest.deploy_id.clone(),
        })
    }
}

fn validate_manifest(manifest: &DeployManifest) -> DeployResult<()> {
    if manifest.phases.is_empty() {
        return Err(DeployError::EmptyManifest {
            deploy_id: manifest.deploy_id.clone(),
        });
    }
    let Some((serving_index, _)) = manifest
        .phases
        .iter()
        .enumerate()
        .find(|(_, phase)| phase.policy.commits_serving())
    else {
        return Err(DeployError::ServingCommitPhaseRequired);
    };
    if serving_index + 1 != manifest.phases.len() {
        return Err(DeployError::ServingCommitPhaseRequired);
    }
    Ok(())
}

fn validate_planned_capacity(
    manifest: &DeployManifest,
    capacities: &BTreeMap<NodeId, CapacityReply>,
) -> DeployResult<()> {
    for phase in &manifest.phases {
        for instance in &phase.instances {
            let Some(capacity) = capacities.get(&instance.node_id) else {
                return Err(DeployError::PlannedNodeNotVisible {
                    node_id: instance.node_id.clone(),
                });
            };
            validate_instance_capacity(instance, capacity)?;
        }
    }
    Ok(())
}

fn validate_instance_capacity(
    instance: &InstancePlan,
    capacity: &CapacityReply,
) -> DeployResult<()> {
    if capacity.memory_free_bytes == 0 {
        return Err(DeployError::InsufficientCapacity {
            node_id: instance.node_id.clone(),
            reason: CapacityRejectionReason::NoFreeMemory,
        });
    }
    if instance.capacity_requirement == InstanceCapacityRequirement::Database
        && !capacity.can_run_database
    {
        return Err(DeployError::InsufficientCapacity {
            node_id: instance.node_id.clone(),
            reason: CapacityRejectionReason::DatabaseUnsupported,
        });
    }
    Ok(())
}

fn planned_node_ids(manifest: &DeployManifest) -> BTreeSet<NodeId> {
    manifest
        .phases
        .iter()
        .flat_map(|phase| {
            phase
                .instances
                .iter()
                .map(|instance| instance.node_id.clone())
        })
        .collect()
}

fn classify_start_reply(
    state: &mut DeployStateMachine,
    instance: &InstancePlan,
    reply: InstanceCommandReply,
) -> DeployResult<()> {
    match reply.outcome {
        InstanceStartOutcome::Ready => Ok(()),
        InstanceStartOutcome::NotReady { reason } => Err(classify_pre_commit_error(
            state,
            DeployError::InstanceNotReady {
                instance_id: instance.instance_id.clone(),
                node_id: instance.node_id.clone(),
                reason,
            },
        )),
    }
}

fn classify_pre_commit_error(state: &mut DeployStateMachine, error: DeployError) -> DeployError {
    if state.has_irreversible_commit() || state.has_serving_commit() {
        let _ = state.block_after_irreversible();
        return DeployError::BlockedAfterIrreversiblePhase;
    }
    error
}

fn prepare_failed_before_dispatch(error: &DeployError) -> bool {
    matches!(error, DeployError::Bus(BusError::NoResponders { .. }))
}

fn cleanup_failure_kind(error: &DeployError) -> CleanupFailureKind {
    match error {
        DeployError::Bus(BusError::NoResponders { .. }) => CleanupFailureKind::NoResponders,
        DeployError::Bus(BusError::Timeout { .. }) => CleanupFailureKind::Timeout,
        DeployError::Bus(BusError::HandlerFailed { .. }) => CleanupFailureKind::HandlerFailed,
        DeployError::EmptyManifest { .. }
        | DeployError::PhaseNotReady { .. }
        | DeployError::ServingCommitAlreadyExists
        | DeployError::ServingCommitRequired
        | DeployError::ProjectionCatchUpMissing
        | DeployError::ProjectionCatchUpMismatch { .. }
        | DeployError::BlockedAfterIrreversiblePhase
        | DeployError::PreCommitFailed { .. }
        | DeployError::DeployStillRunning
        | DeployError::ServingCommitPhaseRequired
        | DeployError::ServingFactConflict { .. }
        | DeployError::ServingFactMissing { .. }
        | DeployError::ServingFactKindMismatch { .. }
        | DeployError::ServingFactMismatch { .. }
        | DeployError::DeployFactConflict { .. }
        | DeployError::DeployFactMissing { .. }
        | DeployError::DeployFactKindMismatch { .. }
        | DeployError::DeployFactMismatch { .. }
        | DeployError::PlannedNodeNotVisible { .. }
        | DeployError::InsufficientCapacity { .. }
        | DeployError::CapacityReplyNodeMismatch { .. }
        | DeployError::InstanceNotReady { .. }
        | DeployError::WirePayload { .. }
        | DeployError::FactSource(_)
        | DeployError::SubjectParse(_)
        | DeployError::FactKeyParse(_)
        | DeployError::Bus(BusError::SubjectParse(_))
        | DeployError::Bus(BusError::FactKeyParse(_))
        | DeployError::Bus(BusError::UnauthorizedPublish { .. })
        | DeployError::Bus(BusError::UnauthorizedRequestTarget { .. })
        | DeployError::Bus(BusError::UnauthorizedSubscribe { .. })
        | DeployError::Bus(BusError::UnauthorizedQueue { .. })
        | DeployError::Bus(BusError::UnauthorizedResponse { .. })
        | DeployError::Bus(BusError::UnauthorizedDrain { .. })
        | DeployError::Bus(BusError::UnauthorizedFactWrite { .. })
        | DeployError::Bus(BusError::UnauthorizedFactRead { .. })
        | DeployError::Bus(BusError::BridgeRuleInvalid { .. })
        | DeployError::Bus(BusError::BridgeUnavailable { .. })
        | DeployError::Bus(BusError::BridgeRequestManyUnsupported { .. })
        | DeployError::Bus(BusError::Draining)
        | DeployError::Bus(BusError::IncompleteResponses { .. })
        | DeployError::Bus(BusError::NoReplyPermit { .. })
        | DeployError::Bus(BusError::ResponseClosed { .. })
        | DeployError::Bus(BusError::DeliveryRuntimeStopped)
        | DeployError::Bus(BusError::DuplicateResponse { .. })
        | DeployError::Bus(BusError::ActorUnavailable { .. }) => CleanupFailureKind::Other,
    }
}
