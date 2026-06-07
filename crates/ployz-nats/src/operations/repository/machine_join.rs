use ployz_core::ids::{NodeId, OperationId};
use ployz_core::machine::{
    IssuedJoinToken, JoinTokenRedeemedAt, MachineAddFailure, MachineAddOperationState,
    MachineAddOperationStateName, MachineName, RawJoinToken, redeem_pending_join_token,
};
use ployz_core::ops::{EventSequence, OperationStatus};
use ployz_core::roles::FirstNodeGateway;

use super::{
    AsyncNatsOperationRepository, OperationStatusReadError, OperationStatusStoreError,
    RecordMachineAddEventError,
};

impl AsyncNatsOperationRepository {
    pub async fn redeem_machine_join_token(
        &self,
        token: &RawJoinToken,
        joined_at: JoinTokenRedeemedAt,
    ) -> Result<MachineJoinRedemption, RedeemMachineJoinTokenError> {
        let fingerprint = token
            .fingerprint()
            .map_err(|_| RedeemMachineJoinTokenError::InvalidJoinToken)?;
        let Some(submission) = self
            .status_store
            .machine_add_submission_for_join_token(&fingerprint)
            .await
            .map_err(RedeemMachineJoinTokenError::StoreStatus)?
        else {
            return Err(RedeemMachineJoinTokenError::UnknownJoinToken);
        };
        if submission.raw_join_token != *token || !submission.join_token.matches(&fingerprint) {
            return Err(RedeemMachineJoinTokenError::JoinTokenMismatch {
                operation_id: submission.operation_id,
            });
        }

        let Some(status) = self
            .operation_status(&submission.operation_id)
            .await
            .map_err(RedeemMachineJoinTokenError::LoadStatus)?
        else {
            return Err(RedeemMachineJoinTokenError::MissingOperation {
                operation_id: submission.operation_id,
            });
        };

        let OperationStatus::MachineAdd {
            id,
            node_id,
            name,
            gateway,
            state,
            last_event_sequence,
        } = status
        else {
            return Err(RedeemMachineJoinTokenError::WrongOperationKind {
                operation_id: submission.operation_id,
            });
        };

        match state {
            MachineAddOperationState::Pending { join_token } => {
                self.redeem_pending_machine_join(
                    RedeemPendingMachineJoin {
                        operation_id: id,
                        node_id,
                        join_token,
                    },
                    token,
                    joined_at,
                )
                .await
            }
            MachineAddOperationState::Joining { joined_at } => {
                Ok(MachineJoinRedemption::AlreadyJoined(RedeemedMachineJoin {
                    operation_id: id,
                    node_id,
                    name,
                    gateway,
                    joined_at,
                    last_event_sequence,
                }))
            }
            MachineAddOperationState::Completed => {
                Err(RedeemMachineJoinTokenError::OperationNotPending {
                    operation_id: id,
                    current: MachineAddOperationStateName::Completed,
                })
            }
            MachineAddOperationState::Failed { .. } => {
                Err(RedeemMachineJoinTokenError::OperationNotPending {
                    operation_id: id,
                    current: MachineAddOperationStateName::Failed,
                })
            }
            MachineAddOperationState::Cancelled { .. } => {
                Err(RedeemMachineJoinTokenError::OperationNotPending {
                    operation_id: id,
                    current: MachineAddOperationStateName::Cancelled,
                })
            }
        }
    }

    async fn redeem_pending_machine_join(
        &self,
        pending: RedeemPendingMachineJoin,
        token: &RawJoinToken,
        joined_at: JoinTokenRedeemedAt,
    ) -> Result<MachineJoinRedemption, RedeemMachineJoinTokenError> {
        let fingerprint = token
            .fingerprint()
            .map_err(|_| RedeemMachineJoinTokenError::InvalidJoinToken)?;
        match redeem_pending_join_token(&pending.join_token, &fingerprint, joined_at) {
            Ok(joined_at) => {
                self.record_machine_add_joined(&pending.operation_id, &pending.node_id, joined_at)
                    .await
                    .map_err(RedeemMachineJoinTokenError::RecordMachineAddEvent)?;
                let joined = self
                    .redeemed_machine_join(&pending.operation_id)
                    .await?
                    .ok_or_else(|| RedeemMachineJoinTokenError::MissingOperation {
                        operation_id: pending.operation_id.clone(),
                    })?;
                Ok(MachineJoinRedemption::Joined(joined))
            }
            Err(failure) => {
                self.record_machine_add_failed(
                    &pending.operation_id,
                    &pending.node_id,
                    failure.clone(),
                )
                .await
                .map_err(RedeemMachineJoinTokenError::RecordMachineAddEvent)?;
                Err(RedeemMachineJoinTokenError::JoinRejected {
                    operation_id: pending.operation_id,
                    failure,
                })
            }
        }
    }

    async fn redeemed_machine_join(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<RedeemedMachineJoin>, RedeemMachineJoinTokenError> {
        let Some(status) = self
            .operation_status(operation_id)
            .await
            .map_err(RedeemMachineJoinTokenError::LoadStatus)?
        else {
            return Ok(None);
        };
        let OperationStatus::MachineAdd {
            id,
            node_id,
            name,
            gateway,
            state,
            last_event_sequence,
        } = status
        else {
            return Err(RedeemMachineJoinTokenError::WrongOperationKind {
                operation_id: operation_id.clone(),
            });
        };
        let MachineAddOperationState::Joining { joined_at } = state else {
            return Err(RedeemMachineJoinTokenError::OperationNotPending {
                operation_id: id,
                current: state.name(),
            });
        };

        Ok(Some(RedeemedMachineJoin {
            operation_id: id,
            node_id,
            name,
            gateway,
            joined_at,
            last_event_sequence,
        }))
    }
}

struct RedeemPendingMachineJoin {
    operation_id: OperationId,
    node_id: NodeId,
    join_token: IssuedJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineJoinRedemption {
    Joined(RedeemedMachineJoin),
    AlreadyJoined(RedeemedMachineJoin),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemedMachineJoin {
    pub operation_id: OperationId,
    pub node_id: NodeId,
    pub name: MachineName,
    pub gateway: FirstNodeGateway,
    pub joined_at: JoinTokenRedeemedAt,
    pub last_event_sequence: EventSequence,
}

#[derive(Debug)]
pub enum RedeemMachineJoinTokenError {
    InvalidJoinToken,
    UnknownJoinToken,
    LoadStatus(OperationStatusReadError),
    StoreStatus(OperationStatusStoreError),
    RecordMachineAddEvent(RecordMachineAddEventError),
    MissingOperation {
        operation_id: OperationId,
    },
    WrongOperationKind {
        operation_id: OperationId,
    },
    JoinTokenMismatch {
        operation_id: OperationId,
    },
    OperationNotPending {
        operation_id: OperationId,
        current: MachineAddOperationStateName,
    },
    JoinRejected {
        operation_id: OperationId,
        failure: MachineAddFailure,
    },
}
