use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures_util::{StreamExt, TryStreamExt, stream};
use mvp_bus::{BusActorHandle, BusError, BusSession, RequestManyPolicy, RequestTarget, Subject};
use mvp_identity::{NodeId, VisibleNodes};

use crate::facts::{BusDeployFactWriter, DeployDecisionFact, DeployFactWriter};
use crate::serving_commit::{BusServingFactWriter, ServingFactWriter};
use crate::wire::{
    CapacityRequest, DrainInstanceRequest, InstanceCommandReply, InstanceCommandRequest,
    InstanceStartOutcome, StopInstanceRequest, decode, decode_capacity_reply, encode,
};
use crate::{
    CapacityRejectionReason, CapacityReply, CleanupFailureKind, CleanupPendingReason,
    CleanupStatus, DeployCommandResult, DeployError, DeployManifest, DeployResult,
    DeployStateMachine, InstanceCapacityRequirement, InstancePlan, ProjectionCatchUp,
};

const CAPACITY_PROBE_CONCURRENCY: usize = 32;

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

        for phase in &manifest.phases {
            state.mark_preparing(phase.phase_id)?;
            for instance in &phase.instances {
                self.prepare_instance(&manifest, instance)
                    .await
                    .map_err(|error| classify_pre_commit_error(&mut state, error))?;
                let reply = self
                    .start_instance(&manifest, instance)
                    .await
                    .map_err(|error| classify_pre_commit_error(&mut state, error))?;
                classify_start_reply(&mut state, instance, reply)?;
            }
            state.mark_ready(phase.phase_id)?;
            state.commit_phase(phase.phase_id, phase.policy)?;
            if phase.policy.commits_serving() {
                self.serving_writer
                    .write_serving_commit(&manifest.serving_commit)
                    .await
                    .map_err(|error| classify_pre_commit_error(&mut state, error))?;
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

        state.finish_cleanup()
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
