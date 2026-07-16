use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::{BuildCleanupEvidence, CancellationReason, EventSequence};
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub(super) struct ActiveBuildRegistry {
    builds: Arc<Mutex<BTreeMap<OperationId, ActiveBuild>>>,
}

#[derive(Clone)]
enum ActiveBuild {
    Running {
        started: BTreeSet<MachineId>,
        cancellation_reason: Option<CancellationReason>,
        watch_start_sequence: EventSequence,
    },
    Finalizing {
        started: BTreeSet<MachineId>,
    },
}

pub(super) enum CancellationRequest {
    Accepted {
        machines: BTreeSet<MachineId>,
        watch_start_sequence: EventSequence,
    },
    Finalizing,
    Missing,
}

pub(super) enum RejectedAdmissionOutcome {
    Cancelled(CancellationReason),
    Interrupted,
}

impl ActiveBuildRegistry {
    pub(super) async fn start(
        &self,
        operation_id: OperationId,
        watch_start_sequence: EventSequence,
    ) {
        self.builds.lock().await.insert(
            operation_id,
            ActiveBuild::Running {
                started: BTreeSet::new(),
                cancellation_reason: None,
                watch_start_sequence,
            },
        );
    }

    pub(super) async fn remove(&self, operation_id: &OperationId) {
        self.builds.lock().await.remove(operation_id);
    }

    pub(super) async fn request_cancellation(
        &self,
        operation_id: &OperationId,
        reason: CancellationReason,
    ) -> CancellationRequest {
        let mut builds = self.builds.lock().await;
        match builds.get_mut(operation_id) {
            Some(ActiveBuild::Running {
                started,
                cancellation_reason,
                watch_start_sequence,
            }) => {
                if cancellation_reason.is_none() {
                    *cancellation_reason = Some(reason);
                }
                CancellationRequest::Accepted {
                    machines: started.clone(),
                    watch_start_sequence: *watch_start_sequence,
                }
            }
            Some(ActiveBuild::Finalizing { .. }) => CancellationRequest::Finalizing,
            None => CancellationRequest::Missing,
        }
    }

    pub(super) async fn claim_machine_start(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
    ) -> bool {
        let mut builds = self.builds.lock().await;
        let Some(build) = builds.get_mut(operation_id) else {
            return false;
        };
        match build {
            ActiveBuild::Running {
                started,
                cancellation_reason: None,
                watch_start_sequence: _,
            } => {
                started.insert(machine_id.clone());
                true
            }
            ActiveBuild::Running {
                cancellation_reason: Some(_),
                ..
            }
            | ActiveBuild::Finalizing { .. } => false,
        }
    }

    pub(super) async fn machine_start_is_authorized(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
    ) -> bool {
        matches!(
            self.builds.lock().await.get(operation_id),
            Some(ActiveBuild::Running {
                started,
                cancellation_reason: None,
                watch_start_sequence: _,
            }) if started.contains(machine_id)
        )
    }

    pub(super) async fn release_machine_start_claim(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
    ) {
        if let Some(ActiveBuild::Running { started, .. }) =
            self.builds.lock().await.get_mut(operation_id)
        {
            started.remove(machine_id);
        }
    }

    pub(super) async fn claim_finalization(
        &self,
        operation_id: &OperationId,
    ) -> Option<CancellationReason> {
        let mut builds = self.builds.lock().await;
        let build = builds.get_mut(operation_id)?;
        match build {
            ActiveBuild::Running {
                started,
                cancellation_reason,
                watch_start_sequence: _,
            } => {
                let reason = cancellation_reason.clone();
                let started = started.clone();
                *build = ActiveBuild::Finalizing { started };
                reason
            }
            ActiveBuild::Finalizing { .. } => None,
        }
    }

    pub(super) async fn claim_rejected_admission(
        &self,
        operation_id: &OperationId,
    ) -> RejectedAdmissionOutcome {
        let mut builds = self.builds.lock().await;
        let Some(build) = builds.get_mut(operation_id) else {
            return RejectedAdmissionOutcome::Interrupted;
        };
        let outcome = match build {
            ActiveBuild::Running {
                cancellation_reason: Some(reason),
                ..
            } => RejectedAdmissionOutcome::Cancelled(reason.clone()),
            ActiveBuild::Running { .. } | ActiveBuild::Finalizing { .. } => {
                RejectedAdmissionOutcome::Interrupted
            }
        };
        *build = ActiveBuild::Finalizing {
            started: BTreeSet::new(),
        };
        outcome
    }

    pub(super) async fn cleanup_evidence(
        &self,
        operation_id: &OperationId,
        confirmed: Vec<MachineId>,
    ) -> BuildCleanupEvidence {
        let started = self
            .builds
            .lock()
            .await
            .get(operation_id)
            .map(|build| match build {
                ActiveBuild::Running { started, .. } | ActiveBuild::Finalizing { started } => {
                    started.clone()
                }
            })
            .unwrap_or_default();
        if started.is_empty() {
            return BuildCleanupEvidence::NotRequired;
        }
        let confirmed: BTreeSet<_> = confirmed.into_iter().collect();
        let unconfirmed = started.difference(&confirmed).cloned().collect::<Vec<_>>();
        if unconfirmed.is_empty() {
            BuildCleanupEvidence::Completed {
                machine_ids: confirmed.into_iter().collect(),
            }
        } else {
            BuildCleanupEvidence::Unconfirmed {
                machine_ids: unconfirmed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation_id(value: &str) -> OperationId {
        OperationId::try_new(value).expect("operation id")
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("machine id")
    }

    fn sequence() -> EventSequence {
        EventSequence::try_new(1).expect("sequence")
    }

    #[tokio::test]
    async fn cancellation_fences_machine_start_and_keeps_every_claimed_machine() {
        let registry = ActiveBuildRegistry::default();
        let operation_id = operation_id("build-1");
        let first = machine_id("machine-a");
        let second = machine_id("machine-b");
        registry.start(operation_id.clone(), sequence()).await;
        assert!(registry.claim_machine_start(&operation_id, &first).await);
        assert!(registry.claim_machine_start(&operation_id, &second).await);

        let reason = CancellationReason::try_new("stop").expect("reason");
        let CancellationRequest::Accepted { machines, .. } =
            registry.request_cancellation(&operation_id, reason).await
        else {
            panic!("active cancellation")
        };
        assert_eq!(machines, BTreeSet::from([first, second.clone()]));
        assert!(!registry.claim_machine_start(&operation_id, &second).await);
    }

    #[tokio::test]
    async fn finalization_claims_cancellation_once_and_fences_late_cancel() {
        let registry = ActiveBuildRegistry::default();
        let operation_id = operation_id("build-1");
        registry.start(operation_id.clone(), sequence()).await;
        let reason = CancellationReason::try_new("stop").expect("reason");
        assert!(matches!(
            registry
                .request_cancellation(&operation_id, reason.clone())
                .await,
            CancellationRequest::Accepted { .. }
        ));
        assert_eq!(
            registry.claim_finalization(&operation_id).await,
            Some(reason)
        );
        assert!(matches!(
            registry
                .request_cancellation(
                    &operation_id,
                    CancellationReason::try_new("late").expect("reason")
                )
                .await,
            CancellationRequest::Finalizing
        ));
        assert_eq!(registry.claim_finalization(&operation_id).await, None);
    }

    #[tokio::test]
    async fn accepted_cancellation_wins_rejected_admission() {
        let registry = ActiveBuildRegistry::default();
        let operation_id = operation_id("build-1");
        registry.start(operation_id.clone(), sequence()).await;
        let reason = CancellationReason::try_new("stop").expect("reason");
        let _ = registry
            .request_cancellation(&operation_id, reason.clone())
            .await;

        assert!(matches!(
            registry.claim_rejected_admission(&operation_id).await,
            RejectedAdmissionOutcome::Cancelled(actual) if actual == reason
        ));
    }

    #[tokio::test]
    async fn cleanup_is_unconfirmed_until_every_claimed_machine_confirms() {
        let registry = ActiveBuildRegistry::default();
        let operation_id = operation_id("build-1");
        let machine_id = machine_id("machine-a");
        registry.start(operation_id.clone(), sequence()).await;
        assert!(
            registry
                .claim_machine_start(&operation_id, &machine_id)
                .await
        );
        let _ = registry.claim_finalization(&operation_id).await;

        assert_eq!(
            registry.cleanup_evidence(&operation_id, Vec::new()).await,
            BuildCleanupEvidence::Unconfirmed {
                machine_ids: vec![machine_id.clone()],
            }
        );
        assert_eq!(
            registry
                .cleanup_evidence(&operation_id, vec![machine_id.clone()])
                .await,
            BuildCleanupEvidence::Completed {
                machine_ids: vec![machine_id],
            }
        );
    }
}
