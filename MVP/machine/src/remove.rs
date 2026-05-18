use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use mvp_bus::{BusActorHandle, BusSession, FactKey, FactPayload, FactWriteOutcome, Subject};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_mesh::{removal_started_fact_key, removal_started_fact_payload, tombstone_fact_key};
use mvp_projection::{
    NodeRemovalStartedFact, NodeTombstonedFact, ProjectionFactPayload, ProjectionState,
};
use mvp_routing::{
    BusServingFactWriter, ProjectionCatchUp, ServingCommitId, ServingCommitPlan, ServingFactWriter,
};

use crate::wire::{
    PrepareRemoveIntent, PrepareRemoveOutcome, PrepareRemoveReply, PrepareRemoveRequest,
    StopRemovedWorkloadsOutcome, StopRemovedWorkloadsReply, StopRemovedWorkloadsRequest, decode,
    encode,
};
use crate::{MachineRemoveError, MachineRemoveResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineRemoveTimeouts {
    pub participant: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRemoveRequest {
    pub target_node_id: NodeId,
    pub removal_epoch: u64,
    pub tombstone_epoch: u64,
    pub reason: String,
    pub visible_nodes: VisibleNodes,
    pub current_projection: ProjectionState,
    pub serving_commit: ServingCommitPlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenMachineFact {
    pub key: FactKey,
}

pub trait MachineFactWriter: Send + Sync {
    fn write_removal_started<'a>(
        &'a self,
        fact: NodeRemovalStartedFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>>;

    fn write_tombstone<'a>(
        &'a self,
        fact: NodeTombstonedFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>>;
}

#[derive(Clone)]
pub struct BusMachineFactWriter {
    bus: BusActorHandle,
    session: BusSession,
}

impl BusMachineFactWriter {
    #[must_use]
    pub fn new(bus: BusActorHandle, session: BusSession) -> Self {
        Self { bus, session }
    }
}

impl MachineFactWriter for BusMachineFactWriter {
    fn write_removal_started<'a>(
        &'a self,
        fact: NodeRemovalStartedFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = removal_started_fact_key(&fact.node_id, fact.epoch)?;
            let payload = removal_started_fact_payload(&fact.node_id, fact.epoch, fact.reason)?;
            write_machine_fact(&self.bus, &self.session, key, payload).await
        })
    }

    fn write_tombstone<'a>(
        &'a self,
        fact: NodeTombstonedFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = tombstone_fact_key(&fact.node_id, fact.epoch)?;
            let payload = ProjectionFactPayload::NodeTombstoned(fact)
                .to_fact_bytes()
                .map_err(|source| MachineRemoveError::WirePayload {
                    context: "serialize tombstone fact",
                    source,
                })?;
            write_machine_fact(&self.bus, &self.session, key, payload).await
        })
    }
}

async fn write_machine_fact(
    bus: &BusActorHandle,
    session: &BusSession,
    key: FactKey,
    payload: impl Into<FactPayload>,
) -> MachineRemoveResult<WrittenMachineFact> {
    match bus
        .write_fact_payload(session, key.clone(), payload.into())
        .await?
    {
        FactWriteOutcome::Inserted(_) | FactWriteOutcome::AlreadyPresent(_) => {
            Ok(WrittenMachineFact { key })
        }
        FactWriteOutcome::Conflict(_) => Err(MachineRemoveError::FactConflict { key }),
    }
}

pub struct MachineRemoveCoordinator<W = BusMachineFactWriter, S = BusServingFactWriter> {
    bus: BusActorHandle,
    session: BusSession,
    fact_writer: W,
    serving_writer: S,
    timeouts: MachineRemoveTimeouts,
}

impl MachineRemoveCoordinator<BusMachineFactWriter, BusServingFactWriter> {
    #[must_use]
    pub fn new(bus: BusActorHandle, session: BusSession, timeouts: MachineRemoveTimeouts) -> Self {
        let fact_writer = BusMachineFactWriter::new(bus.clone(), session.clone());
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

impl<W, S> MachineRemoveCoordinator<W, S>
where
    W: MachineFactWriter,
    S: ServingFactWriter,
{
    #[must_use]
    pub fn with_fact_writers(
        bus: BusActorHandle,
        session: BusSession,
        fact_writer: W,
        serving_writer: S,
        timeouts: MachineRemoveTimeouts,
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
        request: MachineRemoveRequest,
    ) -> MachineRemoveResult<PendingMachineRemove> {
        validate_preconditions(&request)?;
        self.probe_prepare_responder(&request).await?;
        let removal_started = self
            .fact_writer
            .write_removal_started(NodeRemovalStartedFact {
                node_id: request.target_node_id.clone(),
                epoch: request.removal_epoch,
                reason: request.reason.clone(),
            })
            .await?;
        self.prepare_remove(&request).await?;
        self.serving_writer
            .write_serving_commit(&request.serving_commit)
            .await?;
        Ok(PendingMachineRemove {
            target_node_id: request.target_node_id,
            reason: request.reason,
            tombstone_epoch: request.tombstone_epoch,
            visible_nodes: request.visible_nodes,
            serving_commit: request.serving_commit,
            removal_started_fact_key: removal_started.key,
        })
    }

    pub async fn finish_cleanup(
        &self,
        pending: PendingMachineRemove,
        projection: ProjectionCatchUp,
    ) -> MachineRemoveResult<MachineRemoveCommandResult> {
        if projection.serving_commit_id() != &pending.serving_commit.serving_commit_id {
            let serving_commit_id = pending.serving_commit.serving_commit_id.clone();
            return Ok(pending.cleanup_pending(
                RemoveCleanupPendingReason::ProjectionCatchUpMismatch { serving_commit_id },
            ));
        }
        match self.stop_removed_workloads(&pending).await {
            Ok(()) => {}
            Err(error) => {
                let node_id = pending.target_node_id.clone();
                let cause = cleanup_failure_kind(&error);
                return Ok(
                    pending.cleanup_pending(RemoveCleanupPendingReason::StopUnavailable {
                        node_id,
                        cause,
                    }),
                );
            }
        }
        let tombstone = self
            .fact_writer
            .write_tombstone(NodeTombstonedFact {
                node_id: pending.target_node_id.clone(),
                epoch: pending.tombstone_epoch,
                reason: pending.reason.clone(),
            })
            .await?;
        Ok(pending.removed(tombstone.key))
    }

    async fn probe_prepare_responder(
        &self,
        request: &MachineRemoveRequest,
    ) -> MachineRemoveResult<()> {
        let reply = self
            .request_prepare(request, PrepareRemoveIntent::Probe)
            .await?;
        match reply.outcome {
            PrepareRemoveOutcome::ResponderReady
            | PrepareRemoveOutcome::NoNewWorkAndDrained
            | PrepareRemoveOutcome::NotDrained { .. } => Ok(()),
        }
    }

    async fn prepare_remove(&self, request: &MachineRemoveRequest) -> MachineRemoveResult<()> {
        let reply = self
            .request_prepare(request, PrepareRemoveIntent::Drain)
            .await?;
        match reply.outcome {
            PrepareRemoveOutcome::NoNewWorkAndDrained => Ok(()),
            PrepareRemoveOutcome::ResponderReady | PrepareRemoveOutcome::NotDrained { .. } => {
                Err(MachineRemoveError::PrepareRemoveRejected {
                    node_id: request.target_node_id.clone(),
                    outcome: reply.outcome,
                })
            }
        }
    }

    async fn request_prepare(
        &self,
        request: &MachineRemoveRequest,
        intent: PrepareRemoveIntent,
    ) -> MachineRemoveResult<PrepareRemoveReply> {
        let subject = prepare_remove_subject(&request.target_node_id)?;
        let response = self
            .bus
            .request(
                &self.session,
                subject,
                encode(
                    &PrepareRemoveRequest {
                        target_node_id: request.target_node_id.clone(),
                        reason: request.reason.clone(),
                        intent,
                    },
                    "prepare remove request",
                )?,
                self.timeouts.participant,
            )
            .await?;
        let reply: PrepareRemoveReply = decode(response.payload(), "prepare remove reply")?;
        if reply.target_node_id != request.target_node_id {
            return Err(MachineRemoveError::ParticipantNodeMismatch {
                operation: "prepare_remove",
                expected_node_id: request.target_node_id.clone(),
                actual_node_id: reply.target_node_id,
            });
        }
        Ok(reply)
    }

    async fn stop_removed_workloads(
        &self,
        pending: &PendingMachineRemove,
    ) -> MachineRemoveResult<()> {
        let subject = stop_removed_workloads_subject(&pending.target_node_id)?;
        let response = self
            .bus
            .request(
                &self.session,
                subject,
                encode(
                    &StopRemovedWorkloadsRequest {
                        target_node_id: pending.target_node_id.clone(),
                        reason: pending.reason.clone(),
                    },
                    "stop removed workloads request",
                )?,
                self.timeouts.participant,
            )
            .await?;
        let reply: StopRemovedWorkloadsReply =
            decode(response.payload(), "stop removed workloads reply")?;
        if reply.target_node_id != pending.target_node_id {
            return Err(MachineRemoveError::ParticipantNodeMismatch {
                operation: "stop_removed_workloads",
                expected_node_id: pending.target_node_id.clone(),
                actual_node_id: reply.target_node_id,
            });
        }
        match reply.outcome {
            StopRemovedWorkloadsOutcome::Stopped => Ok(()),
            StopRemovedWorkloadsOutcome::Failed { .. } => {
                Err(MachineRemoveError::Bus(mvp_bus::BusError::HandlerFailed {
                    subject: stop_removed_workloads_subject(&pending.target_node_id)?.to_string(),
                    failure: mvp_bus::HandlerFailure::Application,
                }))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMachineRemove {
    pub target_node_id: NodeId,
    pub reason: String,
    pub tombstone_epoch: u64,
    pub visible_nodes: VisibleNodes,
    pub serving_commit: ServingCommitPlan,
    pub removal_started_fact_key: FactKey,
}

impl PendingMachineRemove {
    fn cleanup_pending(self, reason: RemoveCleanupPendingReason) -> MachineRemoveCommandResult {
        MachineRemoveCommandResult {
            target_node_id: self.target_node_id,
            outcome: MachineRemoveOutcome::CleanupPending,
            visible_nodes: self.visible_nodes,
            serving_commit_id: Some(self.serving_commit.serving_commit_id),
            removal_started_fact_key: Some(self.removal_started_fact_key),
            tombstone_fact_key: None,
            cleanup_status: RemoveCleanupStatus::Pending { reason },
        }
    }

    fn removed(self, tombstone_fact_key: FactKey) -> MachineRemoveCommandResult {
        MachineRemoveCommandResult {
            target_node_id: self.target_node_id,
            outcome: MachineRemoveOutcome::Removed,
            visible_nodes: self.visible_nodes,
            serving_commit_id: Some(self.serving_commit.serving_commit_id),
            removal_started_fact_key: Some(self.removal_started_fact_key),
            tombstone_fact_key: Some(tombstone_fact_key),
            cleanup_status: RemoveCleanupStatus::Done,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRemoveCommandResult {
    pub target_node_id: NodeId,
    pub outcome: MachineRemoveOutcome,
    pub visible_nodes: VisibleNodes,
    pub serving_commit_id: Option<ServingCommitId>,
    pub removal_started_fact_key: Option<FactKey>,
    pub tombstone_fact_key: Option<FactKey>,
    pub cleanup_status: RemoveCleanupStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineRemoveOutcome {
    Removed,
    CleanupPending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveCleanupStatus {
    Done,
    Pending { reason: RemoveCleanupPendingReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveCleanupPendingReason {
    ProjectionCatchUpMismatch {
        serving_commit_id: ServingCommitId,
    },
    StopUnavailable {
        node_id: NodeId,
        cause: CleanupFailureKind,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupFailureKind {
    NoResponders,
    Timeout,
    HandlerFailed,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineRemoveValidationError {
    TargetStillActive,
    TargetMissingFromDrainSet,
}

fn validate_preconditions(request: &MachineRemoveRequest) -> MachineRemoveResult<()> {
    let target = &request.target_node_id;
    if let Some(epoch) = request.current_projection.tombstoned_nodes.get(target) {
        return Err(MachineRemoveError::TargetTombstoned {
            node_id: target.clone(),
            epoch: *epoch,
        });
    }
    if let Some(removing) = request.current_projection.removing_nodes.get(target) {
        return Err(MachineRemoveError::TargetAlreadyRemoving {
            node_id: target.clone(),
            epoch: removing.epoch,
            reason: removing.reason.clone(),
        });
    }
    if !request.current_projection.nodes.contains_key(target) {
        return Err(MachineRemoveError::TargetMissing {
            node_id: target.clone(),
        });
    }
    if request
        .serving_commit
        .active_backends
        .iter()
        .any(|backend| &backend.node_id == target)
    {
        return Err(MachineRemoveError::InvalidServingCommit {
            serving_commit_id: request.serving_commit.serving_commit_id.clone(),
            node_id: target.clone(),
            reason: MachineRemoveValidationError::TargetStillActive,
        });
    }
    if !request
        .serving_commit
        .old_backends_to_drain
        .iter()
        .any(|backend| &backend.node_id == target)
    {
        return Err(MachineRemoveError::InvalidServingCommit {
            serving_commit_id: request.serving_commit.serving_commit_id.clone(),
            node_id: target.clone(),
            reason: MachineRemoveValidationError::TargetMissingFromDrainSet,
        });
    }
    Ok(())
}

pub fn prepare_remove_subject(node_id: &NodeId) -> MachineRemoveResult<Subject> {
    participant_subject(node_id, "prepare_remove")
}

pub fn stop_removed_workloads_subject(node_id: &NodeId) -> MachineRemoveResult<Subject> {
    participant_subject(node_id, "stop_removed_workloads")
}

fn participant_subject(node_id: &NodeId, suffix: &str) -> MachineRemoveResult<Subject> {
    Ok(Subject::parse(format!(
        "node.{}.rpc.{suffix}",
        node_id.as_str()
    ))?)
}

fn cleanup_failure_kind(error: &MachineRemoveError) -> CleanupFailureKind {
    match error {
        MachineRemoveError::Bus(mvp_bus::BusError::NoResponders { .. }) => {
            CleanupFailureKind::NoResponders
        }
        MachineRemoveError::Bus(mvp_bus::BusError::Timeout { .. }) => CleanupFailureKind::Timeout,
        MachineRemoveError::Bus(mvp_bus::BusError::HandlerFailed { .. }) => {
            CleanupFailureKind::HandlerFailed
        }
        MachineRemoveError::TargetMissing { .. }
        | MachineRemoveError::TargetTombstoned { .. }
        | MachineRemoveError::TargetAlreadyRemoving { .. }
        | MachineRemoveError::InvalidServingCommit { .. }
        | MachineRemoveError::PrepareRemoveRejected { .. }
        | MachineRemoveError::ParticipantNodeMismatch { .. }
        | MachineRemoveError::FactConflict { .. }
        | MachineRemoveError::WirePayload { .. }
        | MachineRemoveError::Mesh(_)
        | MachineRemoveError::Routing(_)
        | MachineRemoveError::SubjectParse(_)
        | MachineRemoveError::FactKeyParse(_)
        | MachineRemoveError::Bus(mvp_bus::BusError::SubjectParse(_))
        | MachineRemoveError::Bus(mvp_bus::BusError::FactKeyParse(_))
        | MachineRemoveError::Bus(mvp_bus::BusError::UnauthorizedPublish { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::UnauthorizedRequestTarget { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::UnauthorizedSubscribe { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::UnauthorizedQueue { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::UnauthorizedResponse { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::UnauthorizedDrain { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::UnauthorizedFactWrite { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::UnauthorizedFactRead { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::BridgeRuleInvalid { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::BridgeUnavailable { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::BridgeRequestManyUnsupported { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::Draining)
        | MachineRemoveError::Bus(mvp_bus::BusError::IncompleteResponses { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::NoReplyPermit { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::ResponseClosed { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::DeliveryRuntimeStopped)
        | MachineRemoveError::Bus(mvp_bus::BusError::DuplicateResponse { .. })
        | MachineRemoveError::Bus(mvp_bus::BusError::ActorUnavailable { .. }) => {
            CleanupFailureKind::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use mvp_bus::{
        BusActorHandle, BusSession, FactContentHash, Grant, IslandId, PrincipalId, SubjectPattern,
    };
    use mvp_identity::NodeId;
    use mvp_projection::{
        BackendEndpoint, DnsRecordFact, GatewayProjection, GatewayRouteProjection, NodeProjection,
        ProjectionReport, RemovingNodeProjection, RouteId, SnapshotWriteReport,
    };
    use mvp_routing::{
        DnsCommitId, GatewayCommitId, RouteCommitId, RoutingError, RoutingResult,
        ServingFactWriter, WrittenServingFact, serving_commit_fact_key,
        serving_commit_fact_payload,
    };

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum RecordedEvent {
        RemovalStarted(NodeId),
        Tombstone(NodeId),
        Probe,
        PrepareDrain,
        ServingCommit(ServingCommitId),
        Stop,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RecordingServingOutcome {
        Succeed,
        Conflict,
    }

    #[derive(Clone, Default)]
    struct RecordingFactWriter {
        events: Arc<Mutex<Vec<RecordedEvent>>>,
    }

    impl RecordingFactWriter {
        fn events(&self) -> Vec<RecordedEvent> {
            self.events.lock().expect("event log").clone()
        }
    }

    impl MachineFactWriter for RecordingFactWriter {
        fn write_removal_started<'a>(
            &'a self,
            fact: NodeRemovalStartedFact,
        ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>>
        {
            Box::pin(async move {
                self.events
                    .lock()
                    .expect("event log")
                    .push(RecordedEvent::RemovalStarted(fact.node_id.clone()));
                Ok(WrittenMachineFact {
                    key: removal_started_fact_key(&fact.node_id, fact.epoch)?,
                })
            })
        }

        fn write_tombstone<'a>(
            &'a self,
            fact: NodeTombstonedFact,
        ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>>
        {
            Box::pin(async move {
                self.events
                    .lock()
                    .expect("event log")
                    .push(RecordedEvent::Tombstone(fact.node_id.clone()));
                Ok(WrittenMachineFact {
                    key: tombstone_fact_key(&fact.node_id, fact.epoch)?,
                })
            })
        }
    }

    #[derive(Clone)]
    struct RecordingServingFactWriter {
        events: Arc<Mutex<Vec<RecordedEvent>>>,
        outcome: RecordingServingOutcome,
    }

    impl RecordingServingFactWriter {
        fn succeeds(events: Arc<Mutex<Vec<RecordedEvent>>>) -> Self {
            Self {
                events,
                outcome: RecordingServingOutcome::Succeed,
            }
        }

        fn conflicts(events: Arc<Mutex<Vec<RecordedEvent>>>) -> Self {
            Self {
                events,
                outcome: RecordingServingOutcome::Conflict,
            }
        }
    }

    impl ServingFactWriter for RecordingServingFactWriter {
        fn write_serving_commit<'a>(
            &'a self,
            commit: &'a ServingCommitPlan,
        ) -> Pin<Box<dyn Future<Output = RoutingResult<WrittenServingFact>> + Send + 'a>> {
            Box::pin(async move {
                let key = serving_commit_fact_key(&commit.serving_commit_id)?;
                match self.outcome {
                    RecordingServingOutcome::Succeed => {
                        self.events
                            .lock()
                            .expect("event log")
                            .push(RecordedEvent::ServingCommit(
                                commit.serving_commit_id.clone(),
                            ));
                        let payload = serving_commit_fact_payload(commit)?;
                        Ok(WrittenServingFact::inserted(
                            key,
                            FactContentHash::for_payload(&payload),
                        ))
                    }
                    RecordingServingOutcome::Conflict => {
                        Err(RoutingError::ServingFactConflict { key })
                    }
                }
            })
        }
    }

    #[tokio::test]
    async fn missing_target_fails_before_fact_write() {
        let (bus, session) = test_bus();
        let writer = RecordingFactWriter::default();
        let coordinator = coordinator(bus, session, writer.clone());
        let mut request = remove_request();
        request.current_projection.nodes.clear();

        let error = coordinator
            .execute_until_serving_commit(request)
            .await
            .expect_err("target missing");

        assert!(matches!(error, MachineRemoveError::TargetMissing { .. }));
        assert!(writer.events().is_empty());
    }

    #[tokio::test]
    async fn tombstoned_target_fails_before_fact_write() {
        let (bus, session) = test_bus();
        let writer = RecordingFactWriter::default();
        let coordinator = coordinator(bus, session, writer.clone());
        let mut request = remove_request();
        request
            .current_projection
            .tombstoned_nodes
            .insert(NodeId::new("node-old"), 9);

        let error = coordinator
            .execute_until_serving_commit(request)
            .await
            .expect_err("target tombstoned");

        assert!(matches!(error, MachineRemoveError::TargetTombstoned { .. }));
        assert!(writer.events().is_empty());
    }

    #[tokio::test]
    async fn already_removing_target_fails_before_fact_write() {
        let (bus, session) = test_bus();
        let writer = RecordingFactWriter::default();
        let coordinator = coordinator(bus, session, writer.clone());
        let mut request = remove_request();
        request.current_projection.removing_nodes.insert(
            NodeId::new("node-old"),
            RemovingNodeProjection {
                node_id: NodeId::new("node-old"),
                epoch: 2,
                reason: "existing-remove".to_string(),
            },
        );

        let error = coordinator
            .execute_until_serving_commit(request)
            .await
            .expect_err("target already removing");

        assert!(matches!(
            error,
            MachineRemoveError::TargetAlreadyRemoving { .. }
        ));
        assert!(writer.events().is_empty());
    }

    #[tokio::test]
    async fn target_still_active_in_serving_commit_fails_before_fact_write() {
        let (bus, session) = test_bus();
        let writer = RecordingFactWriter::default();
        let coordinator = coordinator(bus, session, writer.clone());
        let mut request = remove_request();
        request
            .serving_commit
            .active_backends
            .push(BackendEndpoint {
                node_id: NodeId::new("node-old"),
                address: "fd00::1:8080".to_string(),
            });

        let error = coordinator
            .execute_until_serving_commit(request)
            .await
            .expect_err("target still active");

        assert!(matches!(
            error,
            MachineRemoveError::InvalidServingCommit {
                reason: MachineRemoveValidationError::TargetStillActive,
                ..
            }
        ));
        assert!(writer.events().is_empty());
    }

    #[tokio::test]
    async fn target_missing_from_drain_set_fails_before_fact_write() {
        let (bus, session) = test_bus();
        let writer = RecordingFactWriter::default();
        let coordinator = coordinator(bus, session, writer.clone());
        let mut request = remove_request();
        request.serving_commit.old_backends_to_drain.clear();

        let error = coordinator
            .execute_until_serving_commit(request)
            .await
            .expect_err("target missing from drain set");

        assert!(matches!(
            error,
            MachineRemoveError::InvalidServingCommit {
                reason: MachineRemoveValidationError::TargetMissingFromDrainSet,
                ..
            }
        ));
        assert!(writer.events().is_empty());
    }

    #[tokio::test]
    async fn no_prepare_responder_fails_before_fact_write() {
        let (bus, session) = test_bus();
        let writer = RecordingFactWriter::default();
        let coordinator = coordinator(bus, session, writer.clone());

        let error = coordinator
            .execute_until_serving_commit(remove_request())
            .await
            .expect_err("no responder");

        assert!(matches!(
            error,
            MachineRemoveError::Bus(mvp_bus::BusError::NoResponders { .. })
        ));
        assert!(writer.events().is_empty());
    }

    #[tokio::test]
    async fn prepare_rejection_fails_before_serving_commit_or_tombstone() {
        let (bus, session) = test_bus();
        let writer = RecordingFactWriter::default();
        register_prepare(
            &bus,
            &session,
            writer.events.clone(),
            PrepareRemoveOutcome::NotDrained {
                reason: "busy".to_string(),
            },
        )
        .await;
        let coordinator = coordinator(bus, session, writer.clone());

        let error = coordinator
            .execute_until_serving_commit(remove_request())
            .await
            .expect_err("prepare rejected");

        assert!(matches!(
            error,
            MachineRemoveError::PrepareRemoveRejected { .. }
        ));
        assert_eq!(
            writer.events(),
            vec![
                RecordedEvent::Probe,
                RecordedEvent::RemovalStarted(NodeId::new("node-old")),
                RecordedEvent::PrepareDrain
            ]
        );
    }

    #[tokio::test]
    async fn serving_commit_failure_leaves_only_removal_started_intent() {
        let (bus, session) = test_bus();
        let writer = RecordingFactWriter::default();
        register_prepare(
            &bus,
            &session,
            writer.events.clone(),
            PrepareRemoveOutcome::NoNewWorkAndDrained,
        )
        .await;
        let request = remove_request();
        let serving_writer = RecordingServingFactWriter::conflicts(writer.events.clone());
        let coordinator = coordinator_with_serving(bus, session, writer.clone(), serving_writer);

        let error = coordinator
            .execute_until_serving_commit(request)
            .await
            .expect_err("serving commit conflict");

        assert!(matches!(
            error,
            MachineRemoveError::Routing(mvp_routing::RoutingError::ServingFactConflict { .. })
        ));
        assert_eq!(
            writer.events(),
            vec![
                RecordedEvent::Probe,
                RecordedEvent::RemovalStarted(NodeId::new("node-old")),
                RecordedEvent::PrepareDrain
            ]
        );
    }

    #[tokio::test]
    async fn projection_mismatch_returns_cleanup_pending_without_tombstone() {
        let (bus, session) = test_bus();
        let writer = RecordingFactWriter::default();
        register_prepare(
            &bus,
            &session,
            writer.events.clone(),
            PrepareRemoveOutcome::NoNewWorkAndDrained,
        )
        .await;
        register_stop(&bus, &session, writer.events.clone()).await;
        let coordinator = coordinator(bus, session, writer.clone());
        let pending = coordinator
            .execute_until_serving_commit(remove_request())
            .await
            .expect("pending remove");
        let mismatch_commit = alternate_commit();
        let mismatch = ProjectionCatchUp::from_report(
            &mismatch_commit,
            &projection_report_for(&mismatch_commit),
        )
        .expect("mismatched catchup object");

        let result = coordinator
            .finish_cleanup(pending, mismatch)
            .await
            .expect("cleanup pending result");

        assert_eq!(result.outcome, MachineRemoveOutcome::CleanupPending);
        assert!(result.tombstone_fact_key.is_none());
        assert_eq!(
            writer.events(),
            vec![
                RecordedEvent::Probe,
                RecordedEvent::RemovalStarted(NodeId::new("node-old")),
                RecordedEvent::PrepareDrain,
                RecordedEvent::ServingCommit(ServingCommitId::new("serving-remove-1")),
            ]
        );
    }

    #[tokio::test]
    async fn stop_failure_returns_cleanup_pending_without_tombstone() {
        let (bus, session) = test_bus();
        let writer = RecordingFactWriter::default();
        register_prepare(
            &bus,
            &session,
            writer.events.clone(),
            PrepareRemoveOutcome::NoNewWorkAndDrained,
        )
        .await;
        let coordinator = coordinator(bus, session, writer.clone());
        let request = remove_request();
        let catch_up = ProjectionCatchUp::from_report(
            &request.serving_commit,
            &projection_report_for(&request.serving_commit),
        )
        .expect("catchup");
        let pending = coordinator
            .execute_until_serving_commit(request)
            .await
            .expect("pending remove");

        let result = coordinator
            .finish_cleanup(pending, catch_up)
            .await
            .expect("cleanup pending");

        assert_eq!(result.outcome, MachineRemoveOutcome::CleanupPending);
        assert!(matches!(
            result.cleanup_status,
            RemoveCleanupStatus::Pending {
                reason: RemoveCleanupPendingReason::StopUnavailable {
                    cause: CleanupFailureKind::NoResponders,
                    ..
                }
            }
        ));
        assert!(result.tombstone_fact_key.is_none());
    }

    #[tokio::test]
    async fn stop_failure_reply_returns_cleanup_pending_without_tombstone() {
        let (bus, session) = test_bus();
        let writer = RecordingFactWriter::default();
        register_prepare(
            &bus,
            &session,
            writer.events.clone(),
            PrepareRemoveOutcome::NoNewWorkAndDrained,
        )
        .await;
        register_stop_with_outcome(
            &bus,
            &session,
            writer.events.clone(),
            StopRemovedWorkloadsOutcome::Failed {
                reason: "still draining".to_string(),
            },
        )
        .await;
        let coordinator = coordinator(bus, session, writer.clone());
        let request = remove_request();
        let catch_up = ProjectionCatchUp::from_report(
            &request.serving_commit,
            &projection_report_for(&request.serving_commit),
        )
        .expect("catchup");
        let pending = coordinator
            .execute_until_serving_commit(request)
            .await
            .expect("pending remove");

        let result = coordinator
            .finish_cleanup(pending, catch_up)
            .await
            .expect("cleanup pending");

        assert_eq!(result.outcome, MachineRemoveOutcome::CleanupPending);
        assert!(matches!(
            result.cleanup_status,
            RemoveCleanupStatus::Pending {
                reason: RemoveCleanupPendingReason::StopUnavailable {
                    cause: CleanupFailureKind::HandlerFailed,
                    ..
                }
            }
        ));
        assert!(result.tombstone_fact_key.is_none());
        assert_eq!(
            writer.events(),
            vec![
                RecordedEvent::Probe,
                RecordedEvent::RemovalStarted(NodeId::new("node-old")),
                RecordedEvent::PrepareDrain,
                RecordedEvent::ServingCommit(ServingCommitId::new("serving-remove-1")),
                RecordedEvent::Stop,
            ]
        );
    }

    #[tokio::test]
    async fn successful_remove_writes_intent_serving_stop_tombstone_in_order() {
        let (bus, session) = test_bus();
        let writer = RecordingFactWriter::default();
        register_prepare(
            &bus,
            &session,
            writer.events.clone(),
            PrepareRemoveOutcome::NoNewWorkAndDrained,
        )
        .await;
        register_stop(&bus, &session, writer.events.clone()).await;
        let coordinator = coordinator(bus, session, writer.clone());
        let request = remove_request();
        let catch_up = ProjectionCatchUp::from_report(
            &request.serving_commit,
            &projection_report_for(&request.serving_commit),
        )
        .expect("catchup");

        let pending = coordinator
            .execute_until_serving_commit(request)
            .await
            .expect("pending remove");
        let result = coordinator
            .finish_cleanup(pending, catch_up)
            .await
            .expect("removed");

        assert_eq!(result.outcome, MachineRemoveOutcome::Removed);
        assert_eq!(result.visible_nodes.len(), 2);
        assert_eq!(
            result.tombstone_fact_key.expect("tombstone").as_str(),
            "/facts/node/node-old/tombstoned/3"
        );
        assert_eq!(
            writer.events(),
            vec![
                RecordedEvent::Probe,
                RecordedEvent::RemovalStarted(NodeId::new("node-old")),
                RecordedEvent::PrepareDrain,
                RecordedEvent::ServingCommit(ServingCommitId::new("serving-remove-1")),
                RecordedEvent::Stop,
                RecordedEvent::Tombstone(NodeId::new("node-old")),
            ]
        );
    }

    fn coordinator(
        bus: BusActorHandle,
        session: BusSession,
        writer: RecordingFactWriter,
    ) -> MachineRemoveCoordinator<RecordingFactWriter, RecordingServingFactWriter> {
        let serving_writer = RecordingServingFactWriter::succeeds(writer.events.clone());
        coordinator_with_serving(bus, session, writer, serving_writer)
    }

    fn coordinator_with_serving(
        bus: BusActorHandle,
        session: BusSession,
        writer: RecordingFactWriter,
        serving_writer: RecordingServingFactWriter,
    ) -> MachineRemoveCoordinator<RecordingFactWriter, RecordingServingFactWriter> {
        MachineRemoveCoordinator::with_fact_writers(
            bus,
            session,
            writer,
            serving_writer,
            MachineRemoveTimeouts {
                participant: Duration::from_secs(1),
            },
        )
    }

    fn test_bus() -> (BusActorHandle, BusSession) {
        let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
        let session = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("operator"),
            Grant::allow_all(),
        );
        (bus, session)
    }

    async fn register_prepare(
        bus: &BusActorHandle,
        session: &BusSession,
        events: Arc<Mutex<Vec<RecordedEvent>>>,
        drain_outcome: PrepareRemoveOutcome,
    ) {
        bus.subscribe(
            session,
            SubjectPattern::parse("node.node-old.rpc.prepare_remove").expect("pattern"),
            move |ctx| {
                let request: PrepareRemoveRequest =
                    serde_json::from_slice(ctx.message.payload().as_bytes()).map_err(|_| {
                        mvp_bus::BusError::HandlerFailed {
                            subject: ctx.message.subject().to_string(),
                            failure: mvp_bus::HandlerFailure::Application,
                        }
                    })?;
                let outcome = match request.intent {
                    PrepareRemoveIntent::Probe => {
                        events.lock().expect("event log").push(RecordedEvent::Probe);
                        PrepareRemoveOutcome::ResponderReady
                    }
                    PrepareRemoveIntent::Drain => {
                        events
                            .lock()
                            .expect("event log")
                            .push(RecordedEvent::PrepareDrain);
                        drain_outcome.clone()
                    }
                };
                ctx.reply(
                    serde_json::to_vec(&PrepareRemoveReply {
                        target_node_id: request.target_node_id,
                        outcome,
                    })
                    .map_err(|_| mvp_bus::BusError::HandlerFailed {
                        subject: ctx.message.subject().to_string(),
                        failure: mvp_bus::HandlerFailure::Application,
                    })?,
                )
            },
        )
        .await
        .expect("register prepare");
    }

    async fn register_stop(
        bus: &BusActorHandle,
        session: &BusSession,
        events: Arc<Mutex<Vec<RecordedEvent>>>,
    ) {
        register_stop_with_outcome(bus, session, events, StopRemovedWorkloadsOutcome::Stopped)
            .await;
    }

    async fn register_stop_with_outcome(
        bus: &BusActorHandle,
        session: &BusSession,
        events: Arc<Mutex<Vec<RecordedEvent>>>,
        outcome: StopRemovedWorkloadsOutcome,
    ) {
        bus.subscribe(
            session,
            SubjectPattern::parse("node.node-old.rpc.stop_removed_workloads").expect("pattern"),
            move |ctx| {
                let request: StopRemovedWorkloadsRequest =
                    serde_json::from_slice(ctx.message.payload().as_bytes()).map_err(|_| {
                        mvp_bus::BusError::HandlerFailed {
                            subject: ctx.message.subject().to_string(),
                            failure: mvp_bus::HandlerFailure::Application,
                        }
                    })?;
                events.lock().expect("event log").push(RecordedEvent::Stop);
                ctx.reply(
                    serde_json::to_vec(&StopRemovedWorkloadsReply {
                        target_node_id: request.target_node_id,
                        outcome: outcome.clone(),
                    })
                    .map_err(|_| mvp_bus::BusError::HandlerFailed {
                        subject: ctx.message.subject().to_string(),
                        failure: mvp_bus::HandlerFailure::Application,
                    })?,
                )
            },
        )
        .await
        .expect("register stop");
    }

    fn remove_request() -> MachineRemoveRequest {
        MachineRemoveRequest {
            target_node_id: NodeId::new("node-old"),
            removal_epoch: 2,
            tombstone_epoch: 3,
            reason: "graceful-remove".to_string(),
            visible_nodes: VisibleNodes::new([NodeId::new("node-old"), NodeId::new("node-new")]),
            current_projection: projection_state(),
            serving_commit: serving_commit(),
        }
    }

    fn projection_state() -> ProjectionState {
        let mut state = ProjectionState::for_island(IslandId::new("prod"));
        for node in ["node-old", "node-new"] {
            state.nodes.insert(
                NodeId::new(node),
                NodeProjection {
                    node_id: NodeId::new(node),
                    epoch: 1,
                    overlay_ip: format!("fd00::{node}"),
                    iroh_endpoint_id: format!("iroh-{node}"),
                    wg_public_key: format!("wg-{node}"),
                },
            );
        }
        state
    }

    fn serving_commit() -> ServingCommitPlan {
        ServingCommitPlan {
            serving_commit_id: ServingCommitId::new("serving-remove-1"),
            route_commit_id: RouteCommitId::new("route-remove-1"),
            gateway_commit_id: GatewayCommitId::new("gateway-remove-1"),
            dns_commit_id: DnsCommitId::new("dns-remove-1"),
            route_id: RouteId::new("web"),
            hostnames: vec!["web.example.test".to_string()],
            active_backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-new"),
                address: "fd00::2:8080".to_string(),
            }],
            old_backends_to_drain: vec![BackendEndpoint {
                node_id: NodeId::new("node-old"),
                address: "fd00::1:8080".to_string(),
            }],
            dns_records: vec![DnsRecordFact {
                name: "web.example.test".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::2".to_string(),
                ttl_seconds: 30,
            }],
            epoch: 1,
        }
    }

    fn alternate_commit() -> ServingCommitPlan {
        let mut commit = serving_commit();
        commit.serving_commit_id = ServingCommitId::new("serving-other");
        commit.route_commit_id = RouteCommitId::new("route-other");
        commit.gateway_commit_id = GatewayCommitId::new("gateway-other");
        commit.dns_commit_id = DnsCommitId::new("dns-other");
        commit
    }

    fn projection_report_for(commit: &ServingCommitPlan) -> ProjectionReport {
        let mut state = ProjectionState::for_island(IslandId::new("prod"));
        state.gateway = Some(GatewayProjection {
            gateway_commit_id: commit.gateway_commit_id.to_string(),
            route_commit_id: commit.route_commit_id.to_string(),
            routes: vec![GatewayRouteProjection {
                route_id: commit.route_id.clone(),
                hostnames: commit.hostnames.clone(),
                backends: commit.active_backends.clone(),
                old_backends_to_drain: commit.old_backends_to_drain.clone(),
            }],
        });
        state.dns = Some(mvp_projection::DnsProjection {
            dns_commit_id: commit.dns_commit_id.to_string(),
            records: commit
                .dns_records
                .clone()
                .into_iter()
                .map(mvp_projection::DnsRecordProjection::from)
                .collect(),
        });
        ProjectionReport {
            state,
            sqlite_path: std::path::PathBuf::from("unused.sqlite"),
            gateway_snapshot: Some(SnapshotWriteReport {
                path: std::path::PathBuf::from("gateway.snapshot"),
                bytes_written: 1,
                revision: format!(
                    "gateway:{}:{}",
                    commit.gateway_commit_id, commit.route_commit_id
                ),
            }),
            dns_snapshot: Some(SnapshotWriteReport {
                path: std::path::PathBuf::from("dns.snapshot"),
                bytes_written: 1,
                revision: format!("dns:{}", commit.dns_commit_id),
            }),
            duration: Duration::from_millis(1),
        }
    }
}
