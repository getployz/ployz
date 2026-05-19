use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use mvp_bus::{BusActorHandle, BusSession, FactKey, FactPayload, FactWriteOutcome, Subject};
use mvp_commands::{
    Command, CommandCompensationFuture, CommandContext, CommandName, CommandStepFuture, IntentId,
    PhaseTransition, PhasedCommand, run_phased,
};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_mesh::{removal_started_fact_key, removal_started_fact_payload, tombstone_fact_key};
use mvp_projection::{
    NodeRemovalStartedFact, NodeTombstonedFact, ProjectionFactPayload, ProjectionState,
};
use mvp_routing::{
    BusServingFactWriter, ProjectionCatchUp, ServingCommitId, ServingCommitPlan, ServingFactWriter,
};

use crate::facts::{
    MachineRemoveCleanupDoneFact, MachineRemoveDecisionFact, MachineRemoveId,
    machine_remove_cleanup_done_fact_key, machine_remove_cleanup_done_fact_payload,
    machine_remove_decision_fact_key, machine_remove_decision_fact_payload,
};
use crate::wire::{
    PrepareRemoveIntent, PrepareRemoveOutcome, PrepareRemoveReply, PrepareRemoveRequest,
    StopRemovedWorkloadsOutcome, StopRemovedWorkloadsReply, StopRemovedWorkloadsRequest, decode,
    encode,
};
use crate::{MachineRemoveError, MachineRemoveResult};
use serde::{Deserialize, Serialize};

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
pub enum MachineRemoveCommandInput {
    Start(Box<MachineRemoveRequest>),
    Resume(MachineRemoveId),
}

impl MachineRemoveCommandInput {
    #[must_use]
    pub fn remove_id(&self) -> MachineRemoveId {
        match self {
            Self::Start(request) => {
                MachineRemoveId::new(request.target_node_id.clone(), request.removal_epoch)
            }
            Self::Resume(remove_id) => remove_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenMachineFact {
    pub key: FactKey,
}

pub trait MachineFactWriter: Send + Sync {
    fn write_remove_decision<'a>(
        &'a self,
        fact: MachineRemoveDecisionFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>>;

    fn write_removal_started<'a>(
        &'a self,
        fact: NodeRemovalStartedFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>>;

    fn write_tombstone<'a>(
        &'a self,
        fact: NodeTombstonedFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>>;

    fn write_cleanup_done<'a>(
        &'a self,
        fact: MachineRemoveCleanupDoneFact,
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
    fn write_remove_decision<'a>(
        &'a self,
        fact: MachineRemoveDecisionFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = machine_remove_decision_fact_key(&fact.remove_id())?;
            let payload = machine_remove_decision_fact_payload(&fact)?;
            write_machine_fact(&self.bus, &self.session, key, payload).await
        })
    }

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

    fn write_cleanup_done<'a>(
        &'a self,
        fact: MachineRemoveCleanupDoneFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = machine_remove_cleanup_done_fact_key(&fact.remove_id())?;
            let payload = machine_remove_cleanup_done_fact_payload(&fact)?;
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

    pub async fn execute_phased(
        &self,
        cx: &CommandContext,
        input: MachineRemoveCommandInput,
        projection: Option<ProjectionCatchUp>,
    ) -> MachineRemoveResult<MachineRemoveCommandResult> {
        run_phased(
            cx,
            &MachineRemovePhasedCommand {
                owner: self,
                input,
                projection,
            },
        )
        .await
    }

    async fn probe_prepare_responder(
        &self,
        request: &MachineRemoveRequest,
    ) -> MachineRemoveResult<()> {
        let reply = self
            .request_prepare(
                &request.target_node_id,
                &request.reason,
                PrepareRemoveIntent::Probe,
            )
            .await?;
        match reply.outcome {
            PrepareRemoveOutcome::ResponderReady
            | PrepareRemoveOutcome::NoNewWorkAndDrained
            | PrepareRemoveOutcome::NotDrained { .. } => Ok(()),
        }
    }

    async fn prepare_remove_decision(
        &self,
        decision: &MachineRemoveDecisionFact,
    ) -> MachineRemoveResult<()> {
        let reply = self
            .request_prepare(
                &decision.target_node_id,
                &decision.reason,
                PrepareRemoveIntent::Drain,
            )
            .await?;
        require_drained(&decision.target_node_id, reply)
    }

    async fn request_prepare(
        &self,
        target_node_id: &NodeId,
        reason: &str,
        intent: PrepareRemoveIntent,
    ) -> MachineRemoveResult<PrepareRemoveReply> {
        let subject = prepare_remove_subject(target_node_id)?;
        let response = self
            .bus
            .request(
                &self.session,
                subject,
                encode(
                    &PrepareRemoveRequest {
                        target_node_id: target_node_id.clone(),
                        reason: reason.to_string(),
                        intent,
                    },
                    "prepare remove request",
                )?,
                self.timeouts.participant,
            )
            .await?;
        let reply: PrepareRemoveReply = decode(response.payload(), "prepare remove reply")?;
        if &reply.target_node_id != target_node_id {
            return Err(MachineRemoveError::ParticipantNodeMismatch {
                operation: "prepare_remove",
                expected_node_id: target_node_id.clone(),
                actual_node_id: reply.target_node_id,
            });
        }
        Ok(reply)
    }

    async fn stop_removed_workloads_decision(
        &self,
        decision: &MachineRemoveDecisionFact,
    ) -> MachineRemoveResult<()> {
        let subject = stop_removed_workloads_subject(&decision.target_node_id)?;
        let response = self
            .bus
            .request(
                &self.session,
                subject,
                encode(
                    &StopRemovedWorkloadsRequest {
                        target_node_id: decision.target_node_id.clone(),
                        reason: decision.reason.clone(),
                    },
                    "stop removed workloads request",
                )?,
                self.timeouts.participant,
            )
            .await?;
        let reply: StopRemovedWorkloadsReply =
            decode(response.payload(), "stop removed workloads reply")?;
        if reply.target_node_id != decision.target_node_id {
            return Err(MachineRemoveError::ParticipantNodeMismatch {
                operation: "stop_removed_workloads",
                expected_node_id: decision.target_node_id.clone(),
                actual_node_id: reply.target_node_id,
            });
        }
        match reply.outcome {
            StopRemovedWorkloadsOutcome::Stopped => Ok(()),
            StopRemovedWorkloadsOutcome::Failed { .. } => {
                Err(MachineRemoveError::Bus(mvp_bus::BusError::HandlerFailed {
                    subject: stop_removed_workloads_subject(&decision.target_node_id)?.to_string(),
                    failure: mvp_bus::HandlerFailure::Application,
                }))
            }
        }
    }
}

fn require_drained(target_node_id: &NodeId, reply: PrepareRemoveReply) -> MachineRemoveResult<()> {
    match reply.outcome {
        PrepareRemoveOutcome::NoNewWorkAndDrained => Ok(()),
        PrepareRemoveOutcome::ResponderReady | PrepareRemoveOutcome::NotDrained { .. } => {
            Err(MachineRemoveError::PrepareRemoveRejected {
                node_id: target_node_id.clone(),
                outcome: reply.outcome,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum MachineRemovePhase {
    Start,
    DecisionWritten(Box<MachineRemoveDecisionFact>),
    RemovalStartedWritten(Box<MachineRemoveDecisionFact>),
    Prepared(Box<MachineRemoveDecisionFact>),
    ServingCommitWritten(Box<MachineRemoveDecisionFact>),
    Stopped(Box<MachineRemoveDecisionFact>),
    Tombstoned(Box<MachineRemoveDecisionFact>),
    CleanupDone(Box<MachineRemoveDecisionFact>),
}

struct MachineRemovePhasedCommand<'a, W, S> {
    owner: &'a MachineRemoveCoordinator<W, S>,
    input: MachineRemoveCommandInput,
    projection: Option<ProjectionCatchUp>,
}

impl<W, S> Command for MachineRemovePhasedCommand<'_, W, S>
where
    W: MachineFactWriter,
    S: ServingFactWriter,
{
    type Output = MachineRemoveCommandResult;
    type Error = MachineRemoveError;

    fn name(&self) -> CommandName {
        CommandName::parse("machine-remove").expect("static command name validates")
    }

    fn intent_id(&self) -> IntentId {
        let remove_id = self.input.remove_id();
        IntentId::parse(format!(
            "{}-{}",
            remove_id.target_node_id().as_str(),
            remove_id.removal_epoch()
        ))
        .expect("node ids and epochs validate as command intents")
    }
}

impl<W, S> PhasedCommand for MachineRemovePhasedCommand<'_, W, S>
where
    W: MachineFactWriter,
    S: ServingFactWriter,
{
    type Phase = MachineRemovePhase;

    fn initial_phase(&self) -> Self::Phase {
        MachineRemovePhase::Start
    }

    fn step<'b>(
        &'b self,
        _cx: &'b CommandContext,
        phase: Self::Phase,
    ) -> CommandStepFuture<'b, Self::Phase, Self::Output, <Self as Command>::Error> {
        Box::pin(async move {
            match phase {
                MachineRemovePhase::Start => {
                    let MachineRemoveCommandInput::Start(request) = &self.input else {
                        return Err(MachineRemoveError::CommandPhaseMissing {
                            remove_id: self.input.remove_id(),
                        });
                    };
                    validate_preconditions(request)?;
                    self.owner.probe_prepare_responder(request).await?;
                    let decision = decision_fact_from_request(request);
                    self.owner
                        .fact_writer
                        .write_remove_decision(decision.clone())
                        .await?;
                    Ok(PhaseTransition::Continue(
                        MachineRemovePhase::DecisionWritten(Box::new(decision)),
                    ))
                }
                MachineRemovePhase::DecisionWritten(decision) => {
                    self.owner
                        .fact_writer
                        .write_removal_started(NodeRemovalStartedFact {
                            node_id: decision.target_node_id.clone(),
                            epoch: decision.removal_epoch,
                            reason: decision.reason.clone(),
                        })
                        .await?;
                    Ok(PhaseTransition::Continue(
                        MachineRemovePhase::RemovalStartedWritten(decision),
                    ))
                }
                MachineRemovePhase::RemovalStartedWritten(decision) => {
                    self.owner.prepare_remove_decision(&decision).await?;
                    Ok(PhaseTransition::Continue(MachineRemovePhase::Prepared(
                        decision,
                    )))
                }
                MachineRemovePhase::Prepared(decision) => {
                    self.owner
                        .serving_writer
                        .write_serving_commit(&decision.serving_commit)
                        .await?;
                    Ok(PhaseTransition::Continue(
                        MachineRemovePhase::ServingCommitWritten(decision),
                    ))
                }
                MachineRemovePhase::ServingCommitWritten(decision) => {
                    let pending = pending_from_decision((*decision).clone())?;
                    let Some(projection) = self.projection.clone() else {
                        let serving_commit_id = decision.serving_commit.serving_commit_id.clone();
                        return Ok(PhaseTransition::Done(pending.cleanup_pending(
                            RemoveCleanupPendingReason::ProjectionCatchUpMissing {
                                serving_commit_id,
                            },
                        )));
                    };
                    if projection.serving_commit_id() != &decision.serving_commit.serving_commit_id
                    {
                        let serving_commit_id = decision.serving_commit.serving_commit_id.clone();
                        return Ok(PhaseTransition::Done(pending.cleanup_pending(
                            RemoveCleanupPendingReason::ProjectionCatchUpMismatch {
                                serving_commit_id,
                            },
                        )));
                    }
                    match self.owner.stop_removed_workloads_decision(&decision).await {
                        Ok(()) => Ok(PhaseTransition::Continue(MachineRemovePhase::Stopped(
                            decision,
                        ))),
                        Err(error) => {
                            let node_id = decision.target_node_id.clone();
                            let cause = cleanup_failure_kind(&error);
                            Ok(PhaseTransition::Done(pending.cleanup_pending(
                                RemoveCleanupPendingReason::StopUnavailable { node_id, cause },
                            )))
                        }
                    }
                }
                MachineRemovePhase::Stopped(decision) => {
                    self.owner
                        .fact_writer
                        .write_tombstone(NodeTombstonedFact {
                            node_id: decision.target_node_id.clone(),
                            epoch: decision.tombstone_epoch,
                            reason: decision.reason.clone(),
                        })
                        .await?;
                    Ok(PhaseTransition::Continue(MachineRemovePhase::Tombstoned(
                        decision,
                    )))
                }
                MachineRemovePhase::Tombstoned(decision) => {
                    let tombstone_key =
                        tombstone_fact_key(&decision.target_node_id, decision.tombstone_epoch)?;
                    let cleanup_done = MachineRemoveCleanupDoneFact::new(&decision, tombstone_key)?;
                    self.owner
                        .fact_writer
                        .write_cleanup_done(cleanup_done)
                        .await?;
                    Ok(PhaseTransition::Continue(MachineRemovePhase::CleanupDone(
                        decision,
                    )))
                }
                MachineRemovePhase::CleanupDone(decision) => {
                    let pending = pending_from_decision(*decision)?;
                    let tombstone_key = tombstone_fact_key(
                        &pending.decision.target_node_id,
                        pending.decision.tombstone_epoch,
                    )?;
                    Ok(PhaseTransition::Done(pending.removed(tombstone_key)))
                }
            }
        })
    }

    fn compensate<'b>(
        &'b self,
        _cx: &'b CommandContext,
        _phase: Self::Phase,
    ) -> CommandCompensationFuture<'b, <Self as Command>::Error> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingMachineRemove {
    pub decision: MachineRemoveDecisionFact,
    pub removal_started_fact_key: FactKey,
}

impl PendingMachineRemove {
    fn cleanup_pending(self, reason: RemoveCleanupPendingReason) -> MachineRemoveCommandResult {
        MachineRemoveCommandResult {
            target_node_id: self.decision.target_node_id,
            outcome: MachineRemoveOutcome::CleanupPending,
            visible_nodes: self.decision.visible_nodes,
            serving_commit_id: Some(self.decision.serving_commit.serving_commit_id),
            removal_started_fact_key: Some(self.removal_started_fact_key),
            tombstone_fact_key: None,
            cleanup_status: RemoveCleanupStatus::Pending { reason },
        }
    }

    fn removed(self, tombstone_fact_key: FactKey) -> MachineRemoveCommandResult {
        MachineRemoveCommandResult {
            target_node_id: self.decision.target_node_id,
            outcome: MachineRemoveOutcome::Removed,
            visible_nodes: self.decision.visible_nodes,
            serving_commit_id: Some(self.decision.serving_commit.serving_commit_id),
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
    ProjectionCatchUpMissing {
        serving_commit_id: ServingCommitId,
    },
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

fn decision_fact_from_request(request: &MachineRemoveRequest) -> MachineRemoveDecisionFact {
    MachineRemoveDecisionFact::new(
        request.target_node_id.clone(),
        request.removal_epoch,
        request.tombstone_epoch,
        request.reason.clone(),
        request.visible_nodes.clone(),
        request.serving_commit.clone(),
    )
}

fn pending_remove_from_decision(
    decision: MachineRemoveDecisionFact,
    removal_started_fact_key: FactKey,
) -> PendingMachineRemove {
    PendingMachineRemove {
        decision,
        removal_started_fact_key,
    }
}

fn pending_from_decision(
    decision: MachineRemoveDecisionFact,
) -> MachineRemoveResult<PendingMachineRemove> {
    let removal_started_fact_key =
        removal_started_fact_key(&decision.target_node_id, decision.removal_epoch)?;
    Ok(pending_remove_from_decision(
        decision,
        removal_started_fact_key,
    ))
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
        | MachineRemoveError::CommandFactMissing { .. }
        | MachineRemoveError::CommandFactConflict { .. }
        | MachineRemoveError::CommandFactKindMismatch { .. }
        | MachineRemoveError::CommandFactMismatch { .. }
        | MachineRemoveError::CommandPhaseMissing { .. }
        | MachineRemoveError::UnauthorizedFactWrite { .. }
        | MachineRemoveError::PrincipalMismatch { .. }
        | MachineRemoveError::UntrustedAuthorKey { .. }
        | MachineRemoveError::AuthorKeyMismatch { .. }
        | MachineRemoveError::FactStore { .. }
        | MachineRemoveError::WirePayload { .. }
        | MachineRemoveError::Mesh(_)
        | MachineRemoveError::Routing(_)
        | MachineRemoveError::FactSource(_)
        | MachineRemoveError::Command(_)
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
mod tests;
