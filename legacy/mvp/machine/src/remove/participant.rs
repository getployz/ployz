use mvp_bus::{BusActorHandle, BusSession, Subject};
use mvp_identity::NodeId;

use crate::facts::MachineRemoveDecisionFact;
use crate::wire::{
    PrepareRemoveIntent, PrepareRemoveOutcome, PrepareRemoveReply, PrepareRemoveRequest,
    StopRemovedWorkloadsOutcome, StopRemovedWorkloadsReply, StopRemovedWorkloadsRequest, decode,
    encode,
};
use crate::{MachineRemoveError, MachineRemoveResult};

use super::MachineRemoveTimeouts;

pub(super) struct MachineRemoveParticipantClient {
    bus: BusActorHandle,
    session: BusSession,
    timeouts: MachineRemoveTimeouts,
}

impl MachineRemoveParticipantClient {
    pub(super) fn new(
        bus: BusActorHandle,
        session: BusSession,
        timeouts: MachineRemoveTimeouts,
    ) -> Self {
        Self {
            bus,
            session,
            timeouts,
        }
    }

    pub(super) async fn probe_prepare_responder(
        &self,
        request: &super::MachineRemoveRequest,
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

    pub(super) async fn prepare_remove_decision(
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

    pub(super) async fn stop_removed_workloads_decision(
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
