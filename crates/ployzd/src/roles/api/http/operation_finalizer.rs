use async_trait::async_trait;
use ployz_core::corrosion::{
    CorrosionDeployFailure, CorrosionPromotionFailure, CorrosionPromotionRowObservation,
};
use ployz_core::ids::ServiceRowId;

use super::operation_evidence::PreparedPromotion;

/// Exact readback of the two rows in one atomic prepared promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PromotionRowsObservation {
    pub(super) service_row: CorrosionPromotionRowObservation,
    pub(super) container_row: CorrosionPromotionRowObservation,
}

impl PromotionRowsObservation {
    pub(super) const EXACT: Self = Self {
        service_row: CorrosionPromotionRowObservation::Exact,
        container_row: CorrosionPromotionRowObservation::Exact,
    };
    pub(super) const ABSENT: Self = Self {
        service_row: CorrosionPromotionRowObservation::Absent,
        container_row: CorrosionPromotionRowObservation::Absent,
    };

    fn invariant_failure(self, service_id: ServiceRowId) -> CorrosionDeployFailure {
        CorrosionDeployFailure::Promotion {
            service_id,
            failure: CorrosionPromotionFailure::InvariantViolation {
                service_row: self.service_row,
                container_row: self.container_row,
            },
        }
    }
}

/// Whether a prepared promotion request has a definite outcome yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromotionRequestDisposition {
    Accepted,
    Rejected,
    Uncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UncertaintyDeadline {
    Open,
    Reached,
}

/// Service-name adjudication after both intended rows are durably visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PromotionClaimOutcome {
    Won,
    Lost { winner: ServiceRowId },
}

/// The narrow storage seam implemented by Packet C's exact service/container row adapter.
#[async_trait]
pub(super) trait PreparedPromotionStore: Send + Sync {
    async fn converge_rows(
        &self,
        prepared: &PreparedPromotion,
    ) -> Result<(PromotionRequestDisposition, PromotionRowsObservation), PromotionFinalizerStoreError>;

    async fn adjudicate_service_claim(
        &self,
        prepared: &PreparedPromotion,
    ) -> Result<PromotionClaimOutcome, PromotionFinalizerStoreError>;

    async fn delete_exact_losing_rows(
        &self,
        prepared: &PreparedPromotion,
    ) -> Result<PromotionRowsObservation, PromotionFinalizerStoreError>;
}

/// Durable states of the one legal prepared-promotion finalizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PromotionFinalizerState {
    PromotionPrepared {
        prepared: PreparedPromotion,
    },
    RowsCommitted {
        prepared: PreparedPromotion,
    },
    ClaimWon {
        prepared: PreparedPromotion,
    },
    ClaimLost {
        prepared: PreparedPromotion,
        winner: ServiceRowId,
    },
}

/// The next durable action or terminal classification. The caller persists every `Append*`
/// marker before feeding its corresponding state back into this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PromotionFinalizerDecision {
    RetryRows {
        state: PromotionFinalizerState,
    },
    AppendRowsCommitted {
        state: PromotionFinalizerState,
    },
    AppendClaimWon {
        state: PromotionFinalizerState,
    },
    AppendClaimLost {
        state: PromotionFinalizerState,
        winner: ServiceRowId,
    },
    RetryLoserCleanup {
        state: PromotionFinalizerState,
    },
    Succeeded {
        prepared: PreparedPromotion,
    },
    Failed {
        prepared: PreparedPromotion,
        failure: CorrosionDeployFailure,
    },
}

impl PromotionFinalizerState {
    pub(super) fn observe_prepared_rows(
        self,
        disposition: PromotionRequestDisposition,
        rows: PromotionRowsObservation,
        uncertainty_deadline: UncertaintyDeadline,
    ) -> PromotionFinalizerDecision {
        let Self::PromotionPrepared { prepared } = self else {
            let service_id = self.service_id();
            let prepared = self.into_prepared();
            return PromotionFinalizerDecision::Failed {
                prepared,
                failure: CorrosionDeployFailure::Promotion {
                    service_id,
                    failure: CorrosionPromotionFailure::InvariantViolation {
                        service_row: rows.service_row,
                        container_row: rows.container_row,
                    },
                },
            };
        };
        if rows == PromotionRowsObservation::EXACT {
            return PromotionFinalizerDecision::AppendRowsCommitted {
                state: Self::RowsCommitted { prepared },
            };
        }
        if rows != PromotionRowsObservation::ABSENT {
            let failure = rows.invariant_failure(prepared.service_id.clone());
            return PromotionFinalizerDecision::Failed { prepared, failure };
        }
        match disposition {
            PromotionRequestDisposition::Rejected | PromotionRequestDisposition::Accepted => {
                let failure = CorrosionDeployFailure::Promotion {
                    service_id: prepared.service_id.clone(),
                    failure: CorrosionPromotionFailure::Rejected,
                };
                PromotionFinalizerDecision::Failed { prepared, failure }
            }
            PromotionRequestDisposition::Uncertain
                if uncertainty_deadline == UncertaintyDeadline::Reached =>
            {
                let failure = CorrosionDeployFailure::Promotion {
                    service_id: prepared.service_id.clone(),
                    failure: CorrosionPromotionFailure::OutcomeUncertain,
                };
                PromotionFinalizerDecision::Failed { prepared, failure }
            }
            PromotionRequestDisposition::Uncertain => PromotionFinalizerDecision::RetryRows {
                state: Self::PromotionPrepared { prepared },
            },
        }
    }

    pub(super) fn observe_claim(
        self,
        outcome: PromotionClaimOutcome,
    ) -> PromotionFinalizerDecision {
        let Self::RowsCommitted { prepared } = self else {
            let service_id = self.service_id();
            let prepared = self.into_prepared();
            return PromotionFinalizerDecision::Failed {
                prepared,
                failure: CorrosionDeployFailure::Promotion {
                    service_id,
                    failure: CorrosionPromotionFailure::InvariantViolation {
                        service_row: CorrosionPromotionRowObservation::Exact,
                        container_row: CorrosionPromotionRowObservation::Exact,
                    },
                },
            };
        };
        match outcome {
            PromotionClaimOutcome::Won => PromotionFinalizerDecision::AppendClaimWon {
                state: Self::ClaimWon { prepared },
            },
            PromotionClaimOutcome::Lost { winner } => PromotionFinalizerDecision::AppendClaimLost {
                state: Self::ClaimLost {
                    prepared,
                    winner: winner.clone(),
                },
                winner,
            },
        }
    }

    pub(super) fn finish_claim_won(self) -> PromotionFinalizerDecision {
        let Self::ClaimWon { prepared } = self else {
            let service_id = self.service_id();
            let prepared = self.into_prepared();
            return PromotionFinalizerDecision::Failed {
                prepared,
                failure: CorrosionDeployFailure::Promotion {
                    service_id,
                    failure: CorrosionPromotionFailure::InvariantViolation {
                        service_row: CorrosionPromotionRowObservation::Exact,
                        container_row: CorrosionPromotionRowObservation::Exact,
                    },
                },
            };
        };
        PromotionFinalizerDecision::Succeeded { prepared }
    }

    pub(super) fn observe_loser_cleanup(
        self,
        rows: PromotionRowsObservation,
    ) -> PromotionFinalizerDecision {
        let Self::ClaimLost { prepared, winner } = self else {
            let service_id = self.service_id();
            let prepared = self.into_prepared();
            return PromotionFinalizerDecision::Failed {
                prepared,
                failure: CorrosionDeployFailure::Promotion {
                    service_id,
                    failure: CorrosionPromotionFailure::InvariantViolation {
                        service_row: rows.service_row,
                        container_row: rows.container_row,
                    },
                },
            };
        };
        if rows == PromotionRowsObservation::ABSENT {
            let failure = CorrosionDeployFailure::Superseded {
                service_id: prepared.service_id.clone(),
                winner,
            };
            return PromotionFinalizerDecision::Failed { prepared, failure };
        }
        if rows == PromotionRowsObservation::EXACT {
            return PromotionFinalizerDecision::RetryLoserCleanup {
                state: Self::ClaimLost { prepared, winner },
            };
        }
        let failure = rows.invariant_failure(prepared.service_id.clone());
        PromotionFinalizerDecision::Failed { prepared, failure }
    }

    fn service_id(&self) -> ServiceRowId {
        self.prepared().service_id.clone()
    }

    fn prepared(&self) -> &PreparedPromotion {
        match self {
            Self::PromotionPrepared { prepared }
            | Self::RowsCommitted { prepared }
            | Self::ClaimWon { prepared }
            | Self::ClaimLost { prepared, .. } => prepared,
        }
    }

    fn into_prepared(self) -> PreparedPromotion {
        match self {
            Self::PromotionPrepared { prepared }
            | Self::RowsCommitted { prepared }
            | Self::ClaimWon { prepared }
            | Self::ClaimLost { prepared, .. } => prepared,
        }
    }
}

/// The next step after one redeploy flip attempt, judged by exact readback.
///
/// The incumbent CAS is the hard gate: an exact pair is the committed flip, a
/// service row that no longer matches either document belongs to a rival
/// deploy, and only transport uncertainty earns a retry before the deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RedeployRowsDecision {
    Committed,
    Retry,
    SupersededByCasMiss,
    Failure { failure: CorrosionPromotionFailure },
}

pub(super) fn classify_redeploy_rows(
    disposition: PromotionRequestDisposition,
    rows: PromotionRowsObservation,
    uncertainty_deadline: UncertaintyDeadline,
) -> RedeployRowsDecision {
    if rows == PromotionRowsObservation::EXACT {
        return RedeployRowsDecision::Committed;
    }
    match rows.service_row {
        CorrosionPromotionRowObservation::Exact => match uncertainty_deadline {
            // The flip landed but the container row has not converged; the
            // conditional insert heals it on the next attempt.
            UncertaintyDeadline::Open => RedeployRowsDecision::Retry,
            UncertaintyDeadline::Reached => RedeployRowsDecision::Failure {
                failure: CorrosionPromotionFailure::InvariantViolation {
                    service_row: rows.service_row,
                    container_row: rows.container_row,
                },
            },
        },
        CorrosionPromotionRowObservation::Absent | CorrosionPromotionRowObservation::Mismatch => {
            match disposition {
                PromotionRequestDisposition::Accepted | PromotionRequestDisposition::Rejected => {
                    RedeployRowsDecision::SupersededByCasMiss
                }
                PromotionRequestDisposition::Uncertain => match uncertainty_deadline {
                    UncertaintyDeadline::Open => RedeployRowsDecision::Retry,
                    UncertaintyDeadline::Reached => RedeployRowsDecision::Failure {
                        failure: CorrosionPromotionFailure::OutcomeUncertain,
                    },
                },
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum PromotionFinalizerStoreError {
    #[error("prepared promotion storage transport failed: {0}")]
    Transport(String),
    #[error("prepared promotion storage response was invalid: {0}")]
    Protocol(String),
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::CorrosionPromotionFailure;

    use super::{
        PromotionClaimOutcome, PromotionFinalizerDecision, PromotionFinalizerState,
        PromotionRequestDisposition, PromotionRowsObservation, RedeployRowsDecision,
        UncertaintyDeadline, classify_redeploy_rows,
    };
    use crate::roles::api::http::operation_evidence::prepared_promotion_fixture;

    #[test]
    fn exact_pair_advances_to_durable_rows_committed() {
        let decision = PromotionFinalizerState::PromotionPrepared {
            prepared: prepared_promotion_fixture(),
        }
        .observe_prepared_rows(
            PromotionRequestDisposition::Accepted,
            PromotionRowsObservation::EXACT,
            UncertaintyDeadline::Open,
        );
        assert!(matches!(
            decision,
            PromotionFinalizerDecision::AppendRowsCommitted { .. }
        ));
    }

    #[test]
    fn partial_pair_is_a_typed_invariant_failure() {
        let decision = PromotionFinalizerState::PromotionPrepared {
            prepared: prepared_promotion_fixture(),
        }
        .observe_prepared_rows(
            PromotionRequestDisposition::Accepted,
            PromotionRowsObservation {
                service_row: ployz_core::corrosion::CorrosionPromotionRowObservation::Exact,
                container_row: ployz_core::corrosion::CorrosionPromotionRowObservation::Absent,
            },
            UncertaintyDeadline::Open,
        );
        let PromotionFinalizerDecision::Failed { failure, .. } = decision else {
            panic!("partial pair must fail");
        };
        assert!(matches!(
            failure,
            ployz_core::corrosion::CorrosionDeployFailure::Promotion {
                failure: CorrosionPromotionFailure::InvariantViolation { .. },
                ..
            }
        ));
    }

    #[test]
    fn ambiguous_absence_retries_until_deadline_then_stays_typed() {
        let prepared = prepared_promotion_fixture();
        let decision = PromotionFinalizerState::PromotionPrepared {
            prepared: prepared.clone(),
        }
        .observe_prepared_rows(
            PromotionRequestDisposition::Uncertain,
            PromotionRowsObservation::ABSENT,
            UncertaintyDeadline::Open,
        );
        let PromotionFinalizerDecision::RetryRows { state } = decision else {
            panic!("uncertainty before deadline must retry");
        };
        let decision = state.observe_prepared_rows(
            PromotionRequestDisposition::Uncertain,
            PromotionRowsObservation::ABSENT,
            UncertaintyDeadline::Reached,
        );
        assert!(matches!(
            decision,
            PromotionFinalizerDecision::Failed {
                failure: ployz_core::corrosion::CorrosionDeployFailure::Promotion {
                    failure: CorrosionPromotionFailure::OutcomeUncertain,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn redeploy_flip_commits_only_on_the_exact_pair() {
        use ployz_core::corrosion::CorrosionPromotionRowObservation;

        assert_eq!(
            classify_redeploy_rows(
                PromotionRequestDisposition::Accepted,
                PromotionRowsObservation::EXACT,
                UncertaintyDeadline::Open,
            ),
            RedeployRowsDecision::Committed
        );
        // A CAS miss with a definite answer is a supersession, not a retry.
        assert_eq!(
            classify_redeploy_rows(
                PromotionRequestDisposition::Rejected,
                PromotionRowsObservation {
                    service_row: CorrosionPromotionRowObservation::Mismatch,
                    container_row: CorrosionPromotionRowObservation::Absent,
                },
                UncertaintyDeadline::Open,
            ),
            RedeployRowsDecision::SupersededByCasMiss
        );
        // Transport uncertainty retries until the deadline, then stays typed.
        assert_eq!(
            classify_redeploy_rows(
                PromotionRequestDisposition::Uncertain,
                PromotionRowsObservation {
                    service_row: CorrosionPromotionRowObservation::Mismatch,
                    container_row: CorrosionPromotionRowObservation::Absent,
                },
                UncertaintyDeadline::Open,
            ),
            RedeployRowsDecision::Retry
        );
        assert!(matches!(
            classify_redeploy_rows(
                PromotionRequestDisposition::Uncertain,
                PromotionRowsObservation {
                    service_row: CorrosionPromotionRowObservation::Mismatch,
                    container_row: CorrosionPromotionRowObservation::Absent,
                },
                UncertaintyDeadline::Reached,
            ),
            RedeployRowsDecision::Failure {
                failure: CorrosionPromotionFailure::OutcomeUncertain
            }
        ));
        // A flipped service row with a missing container row heals by retry.
        assert_eq!(
            classify_redeploy_rows(
                PromotionRequestDisposition::Accepted,
                PromotionRowsObservation {
                    service_row: CorrosionPromotionRowObservation::Exact,
                    container_row: CorrosionPromotionRowObservation::Absent,
                },
                UncertaintyDeadline::Open,
            ),
            RedeployRowsDecision::Retry
        );
    }

    #[test]
    fn claim_win_is_durable_before_success() {
        let decision = PromotionFinalizerState::RowsCommitted {
            prepared: prepared_promotion_fixture(),
        }
        .observe_claim(PromotionClaimOutcome::Won);
        let PromotionFinalizerDecision::AppendClaimWon { state } = decision else {
            panic!("claim decision must be persisted");
        };
        assert!(matches!(
            state.finish_claim_won(),
            PromotionFinalizerDecision::Succeeded { .. }
        ));
    }

    #[test]
    fn claim_loss_requires_exact_absence_before_superseded() {
        let winner =
            ployz_core::ids::ServiceRowId::try_new("01J00000000000000000000019").expect("winner");
        let decision = PromotionFinalizerState::RowsCommitted {
            prepared: prepared_promotion_fixture(),
        }
        .observe_claim(PromotionClaimOutcome::Lost {
            winner: winner.clone(),
        });
        let PromotionFinalizerDecision::AppendClaimLost { state, .. } = decision else {
            panic!("claim loss must be persisted");
        };
        assert!(matches!(
            state.observe_loser_cleanup(PromotionRowsObservation::EXACT),
            PromotionFinalizerDecision::RetryLoserCleanup { .. }
        ));

        let state = PromotionFinalizerState::ClaimLost {
            prepared: prepared_promotion_fixture(),
            winner,
        };
        assert!(matches!(
            state.observe_loser_cleanup(PromotionRowsObservation::ABSENT),
            PromotionFinalizerDecision::Failed {
                failure: ployz_core::corrosion::CorrosionDeployFailure::Superseded { .. },
                ..
            }
        ));
    }
}
