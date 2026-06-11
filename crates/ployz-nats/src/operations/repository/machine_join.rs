use ployz_core::ids::{NodeId, OperationId};
use ployz_core::install::{MachineJoinBundle, MachineJoinSecretDelivery};
use ployz_core::machine::{
    IssuedJoinToken, JoinTokenRedeemedAt, MachineAddFailure, MachineAddOperationState,
    MachineAddOperationStateName, MachineName, RawJoinToken, redeem_pending_join_token,
};
use ployz_core::ops::{EventSequence, OperationStatus};
use ployz_core::roles::FirstNodeGateway;

use super::{
    AsyncNatsOperationRepository, OperationStatusReadError, OperationStatusStoreError,
    OperationStatusWrite, RecordMachineAddEventError,
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
            .status_store
            .get(&submission.operation_id)
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
                        join_bundle: submission.join_bundle,
                        idempotency_key: submission.idempotency_key,
                        join_token,
                    },
                    token,
                    joined_at,
                )
                .await
            }
            MachineAddOperationState::Joining { joined_at } => {
                let secret_delivery = self
                    .load_machine_add_secret_delivery(&submission.idempotency_key, &id)
                    .await?;
                Ok(MachineJoinRedemption::AlreadyJoined(RedeemedMachineJoin {
                    operation_id: id,
                    node_id,
                    name,
                    gateway,
                    join_bundle: submission.join_bundle,
                    secret_delivery,
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

    /// The minted per-machine secret, or the typed not-ready error: redeem
    /// before the mint worker's `material-ready` must not hand out a join
    /// without credentials, and must not transition the operation.
    async fn load_machine_add_secret_delivery(
        &self,
        idempotency_key: &ployz_core::ops::OperationIdempotencyKey,
        operation_id: &OperationId,
    ) -> Result<MachineJoinSecretDelivery, RedeemMachineJoinTokenError> {
        Ok(self
            .status_store
            .machine_add_secret_delivery(idempotency_key)
            .await
            .map_err(RedeemMachineJoinTokenError::StoreStatus)?
            .ok_or_else(|| RedeemMachineJoinTokenError::MissingSecretDelivery {
                operation_id: operation_id.clone(),
            })?
            .secret_delivery)
    }

    pub async fn record_machine_join_completed(
        &self,
        token: &RawJoinToken,
    ) -> Result<RecordedMachineJoinReport, RecordMachineJoinReportError> {
        let target = self.machine_join_report_target(token).await?;
        let status_write = self
            .record_machine_add_completed(&target.operation_id, &target.node_id)
            .await
            .map_err(RecordMachineJoinReportError::RecordMachineAddEvent)?;
        self.status_store
            .delete_machine_add_secret_delivery(&target.idempotency_key)
            .await
            .map_err(RecordMachineJoinReportError::StoreStatus)?;
        Ok(RecordedMachineJoinReport {
            operation_id: target.operation_id,
            node_id: target.node_id,
            status_write,
        })
    }

    pub async fn record_machine_join_failed(
        &self,
        token: &RawJoinToken,
        failure: MachineAddFailure,
    ) -> Result<RecordedMachineJoinReport, RecordMachineJoinReportError> {
        let target = self.machine_join_report_target(token).await?;
        let status_write = self
            .record_machine_add_failed(&target.operation_id, &target.node_id, failure)
            .await
            .map_err(RecordMachineJoinReportError::RecordMachineAddEvent)?;
        self.status_store
            .delete_machine_add_secret_delivery(&target.idempotency_key)
            .await
            .map_err(RecordMachineJoinReportError::StoreStatus)?;
        Ok(RecordedMachineJoinReport {
            operation_id: target.operation_id,
            node_id: target.node_id,
            status_write,
        })
    }

    async fn machine_join_report_target(
        &self,
        token: &RawJoinToken,
    ) -> Result<MachineJoinReportTarget, RecordMachineJoinReportError> {
        let fingerprint = token
            .fingerprint()
            .map_err(|_| RecordMachineJoinReportError::InvalidJoinToken)?;
        let Some(submission) = self
            .status_store
            .machine_add_submission_for_join_token(&fingerprint)
            .await
            .map_err(RecordMachineJoinReportError::StoreStatus)?
        else {
            return Err(RecordMachineJoinReportError::UnknownJoinToken);
        };

        if submission.raw_join_token != *token || !submission.join_token.matches(&fingerprint) {
            return Err(RecordMachineJoinReportError::JoinTokenMismatch {
                operation_id: submission.operation_id,
            });
        }

        Ok(MachineJoinReportTarget {
            operation_id: submission.operation_id,
            idempotency_key: submission.idempotency_key,
            node_id: submission.node_id,
        })
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
                // Material gating happens after token validation but
                // before any state change: a not-ready redeem leaves the
                // operation Pending so the keeper can retry.
                let secret_delivery = self
                    .load_machine_add_secret_delivery(
                        &pending.idempotency_key,
                        &pending.operation_id,
                    )
                    .await?;
                self.record_machine_add_joined(&pending.operation_id, &pending.node_id, joined_at)
                    .await
                    .map_err(RedeemMachineJoinTokenError::RecordMachineAddEvent)?;
                let joined = self
                    .redeemed_machine_join(
                        &pending.operation_id,
                        pending.join_bundle,
                        secret_delivery,
                    )
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
        join_bundle: MachineJoinBundle,
        secret_delivery: MachineJoinSecretDelivery,
    ) -> Result<Option<RedeemedMachineJoin>, RedeemMachineJoinTokenError> {
        let Some(status) = self
            .status_store
            .get(operation_id)
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
            join_bundle,
            secret_delivery,
            joined_at,
            last_event_sequence,
        }))
    }
}

struct RedeemPendingMachineJoin {
    operation_id: OperationId,
    node_id: NodeId,
    join_token: IssuedJoinToken,
    join_bundle: MachineJoinBundle,
    idempotency_key: ployz_core::ops::OperationIdempotencyKey,
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
    pub join_bundle: MachineJoinBundle,
    pub secret_delivery: MachineJoinSecretDelivery,
    pub joined_at: JoinTokenRedeemedAt,
    pub last_event_sequence: EventSequence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedMachineJoinReport {
    pub operation_id: OperationId,
    pub node_id: NodeId,
    pub status_write: OperationStatusWrite,
}

struct MachineJoinReportTarget {
    operation_id: OperationId,
    idempotency_key: ployz_core::ops::OperationIdempotencyKey,
    node_id: NodeId,
}

#[derive(Debug)]
pub enum RedeemMachineJoinTokenError {
    Clock {
        message: String,
    },
    InvalidJoinToken,
    UnknownJoinToken,
    LoadStatus(OperationStatusReadError),
    StoreStatus(OperationStatusStoreError),
    RecordMachineAddEvent(RecordMachineAddEventError),
    MissingOperation {
        operation_id: OperationId,
    },
    MissingSecretDelivery {
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

#[derive(Debug)]
pub enum RecordMachineJoinReportError {
    InvalidJoinToken,
    UnknownJoinToken,
    StoreStatus(OperationStatusStoreError),
    RecordMachineAddEvent(RecordMachineAddEventError),
    JoinTokenMismatch { operation_id: OperationId },
}
