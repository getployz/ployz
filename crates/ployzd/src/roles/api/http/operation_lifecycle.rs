use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ployz_core::corrosion::{
    CorrosionDeployState, CorrosionEvidenceLossReason, CorrosionOperation, CorrosionOperationState,
    OperationDocument,
};
use ployz_core::ids::MachineRowId;
use tokio::sync::{Mutex, watch};
use tokio::task::JoinSet;

use super::operation_evidence::DurablePromotionProgress;

/// Evidence state observed after identity validation and torn-tail repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StartupEvidence {
    Usable {
        terminal: Option<Box<OperationDocument>>,
        promotion: Option<Box<DurablePromotionProgress>>,
    },
    Unusable {
        reason: CorrosionEvidenceLossReason,
    },
}

/// The only legal startup disposition for one operation row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StartupRecoveryAction {
    IgnoreRemoteOwner,
    LeaveTerminal,
    TerminalDetailUnavailable,
    ReconcileDurableTerminal {
        operation: Box<OperationDocument>,
    },
    ResumePreparedPromotion {
        progress: Box<DurablePromotionProgress>,
    },
    TerminalizeInterrupted,
    QuarantineAndTerminalizeEvidenceLoss {
        reason: CorrosionEvidenceLossReason,
    },
}

/// Classifies recovery without granting startup authority over another machine's operation row.
pub(super) fn classify_startup_recovery(
    local_machine_id: &MachineRowId,
    row: &OperationDocument,
    evidence: StartupEvidence,
) -> StartupRecoveryAction {
    if &row.machine_id != local_machine_id {
        return StartupRecoveryAction::IgnoreRemoteOwner;
    }
    if operation_is_terminal(row) {
        return match evidence {
            StartupEvidence::Usable {
                terminal: Some(terminal),
                ..
            } if *terminal == *row => StartupRecoveryAction::LeaveTerminal,
            StartupEvidence::Usable { .. } => StartupRecoveryAction::TerminalDetailUnavailable,
            StartupEvidence::Unusable { .. } => StartupRecoveryAction::TerminalDetailUnavailable,
        };
    }
    match evidence {
        StartupEvidence::Unusable { reason } => {
            StartupRecoveryAction::QuarantineAndTerminalizeEvidenceLoss { reason }
        }
        StartupEvidence::Usable {
            terminal: Some(operation),
            ..
        } => StartupRecoveryAction::ReconcileDurableTerminal { operation },
        StartupEvidence::Usable {
            terminal: None,
            promotion: Some(progress),
        } => StartupRecoveryAction::ResumePreparedPromotion { progress },
        StartupEvidence::Usable {
            terminal: None,
            promotion: None,
        } => StartupRecoveryAction::TerminalizeInterrupted,
    }
}

pub(super) fn operation_is_terminal(document: &OperationDocument) -> bool {
    match document.operation() {
        CorrosionOperation::Deploy { state, .. } => {
            matches!(state, CorrosionDeployState::Terminal { .. })
        }
        CorrosionOperation::Build { state, .. }
        | CorrosionOperation::MachineAdd { state, .. }
        | CorrosionOperation::MachineRemove { state, .. }
        | CorrosionOperation::Recovery { state, .. } => matches!(
            state,
            CorrosionOperationState::Succeeded { .. } | CorrosionOperationState::Failed { .. }
        ),
    }
}

/// A bounded set of operation tasks with an explicit cooperative shutdown signal.
#[derive(Debug)]
pub(super) struct OperationTaskRegistry {
    accepting: Arc<AtomicBool>,
    shutdown: watch::Sender<bool>,
    tasks: Mutex<JoinSet<()>>,
}

impl OperationTaskRegistry {
    #[must_use]
    pub(super) fn new() -> Self {
        let (shutdown, _) = watch::channel(false);
        Self {
            accepting: Arc::new(AtomicBool::new(true)),
            shutdown,
            tasks: Mutex::new(JoinSet::new()),
        }
    }

    /// Starts work only while admission remains open. The task must interpret shutdown according
    /// to its durable state: ordinary work terminalizes Interrupted; prepared promotion invokes
    /// the shared finalizer before returning.
    pub(super) async fn spawn<Task, TaskFuture>(&self, task: Task) -> bool
    where
        Task: FnOnce(watch::Receiver<bool>) -> TaskFuture + Send + 'static,
        TaskFuture: Future<Output = ()> + Send + 'static,
    {
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }
        let shutdown = self.shutdown.subscribe();
        let mut tasks = self.tasks.lock().await;
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }
        tasks.spawn(task(shutdown));
        true
    }

    /// Stops admission, requests cooperative durable shutdown, then aborts only after the bound.
    pub(super) async fn shutdown(&self, timeout: Duration) -> OperationShutdownOutcome {
        self.accepting.store(false, Ordering::Release);
        self.shutdown.send_replace(true);
        let drain = async {
            let mut tasks = self.tasks.lock().await;
            while tasks.join_next().await.is_some() {}
        };
        if tokio::time::timeout(timeout, drain).await.is_ok() {
            return OperationShutdownOutcome::Drained;
        }
        let mut tasks = self.tasks.lock().await;
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        OperationShutdownOutcome::AbortedAfterDeadline
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OperationShutdownOutcome {
    Drained,
    AbortedAfterDeadline,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ployz_core::corrosion::{
        CorrosionBasicOperation, CorrosionBasicTransition, CorrosionDocumentVersion,
        CorrosionTimestamp, OperationDocument, OperationInitiator,
    };
    use ployz_core::ids::{ClusterId, MachineRowId, PeerId, ServiceRowId};

    use super::{
        OperationShutdownOutcome, OperationTaskRegistry, StartupEvidence, StartupRecoveryAction,
        classify_startup_recovery,
    };
    use crate::roles::api::http::operation_evidence::{
        DurablePromotionProgress, prepared_promotion_fixture,
    };

    fn machine(value: &str) -> MachineRowId {
        MachineRowId::try_new(value).expect("machine")
    }

    fn created(owner: MachineRowId) -> OperationDocument {
        OperationDocument::basic_created(
            CorrosionDocumentVersion::V1,
            ClusterId::try_new("01J00000000000000000000020").expect("cluster"),
            owner,
            OperationInitiator::Peer {
                peer_id: PeerId::try_new("01J00000000000000000000021").expect("peer"),
            },
            CorrosionBasicOperation::Build {
                service_id: ServiceRowId::try_new("01J00000000000000000000022").expect("service"),
            },
            CorrosionTimestamp::try_new("2026-08-05T10:00:00Z").expect("timestamp"),
        )
    }

    fn terminal(owner: MachineRowId, completed_at: &str) -> OperationDocument {
        created(owner)
            .transition_basic(CorrosionBasicTransition::Succeeded {
                completed_at: CorrosionTimestamp::try_new(completed_at).expect("completed at"),
            })
            .expect("terminal")
    }

    #[test]
    fn startup_never_mutates_a_remote_owned_row() {
        let local = machine("01J00000000000000000000023");
        let remote = created(machine("01J00000000000000000000024"));
        let action = classify_startup_recovery(
            &local,
            &remote,
            StartupEvidence::Unusable {
                reason: ployz_core::corrosion::CorrosionEvidenceLossReason::MissingFile,
            },
        );
        assert_eq!(action, StartupRecoveryAction::IgnoreRemoteOwner);
    }

    #[test]
    fn local_nonterminal_missing_evidence_is_typed_for_quarantine_terminalization() {
        let local = machine("01J00000000000000000000023");
        let action = classify_startup_recovery(
            &local,
            &created(local.clone()),
            StartupEvidence::Unusable {
                reason: ployz_core::corrosion::CorrosionEvidenceLossReason::MissingFile,
            },
        );
        assert!(matches!(
            action,
            StartupRecoveryAction::QuarantineAndTerminalizeEvidenceLoss { .. }
        ));
    }

    #[test]
    fn local_prepared_promotion_resumes_the_shared_finalizer() {
        let local = machine("01J00000000000000000000023");
        let action = classify_startup_recovery(
            &local,
            &created(local.clone()),
            StartupEvidence::Usable {
                terminal: None,
                promotion: Some(Box::new(DurablePromotionProgress::RowsCommitted {
                    prepared: prepared_promotion_fixture(),
                })),
            },
        );
        let StartupRecoveryAction::ResumePreparedPromotion { progress } = action else {
            panic!("prepared promotion did not resume");
        };
        assert!(matches!(
            *progress,
            DurablePromotionProgress::RowsCommitted { .. }
        ));
    }

    #[test]
    fn terminal_row_requires_exact_matching_durable_terminal_detail() {
        let local = machine("01J00000000000000000000023");
        let row = terminal(local.clone(), "2026-08-05T10:00:02Z");
        assert_eq!(
            classify_startup_recovery(
                &local,
                &row,
                StartupEvidence::Usable {
                    terminal: Some(Box::new(row.clone())),
                    promotion: None,
                },
            ),
            StartupRecoveryAction::LeaveTerminal
        );
        assert_eq!(
            classify_startup_recovery(
                &local,
                &row,
                StartupEvidence::Usable {
                    terminal: Some(Box::new(terminal(local.clone(), "2026-08-05T10:00:03Z",))),
                    promotion: None,
                },
            ),
            StartupRecoveryAction::TerminalDetailUnavailable
        );
    }

    #[tokio::test]
    async fn shutdown_is_bounded_and_closes_admission() {
        let registry = OperationTaskRegistry::new();
        assert!(
            registry
                .spawn(|mut shutdown| async move {
                    let _ = shutdown.changed().await;
                })
                .await
        );
        assert_eq!(
            registry.shutdown(Duration::from_secs(1)).await,
            OperationShutdownOutcome::Drained
        );
        assert!(!registry.spawn(|_| async {}).await);
    }
}
