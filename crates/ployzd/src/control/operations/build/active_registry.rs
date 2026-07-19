use std::collections::BTreeMap;
use std::sync::Arc;

use ployz_core::build::{BuildExecutorAssignment, BuildExecutorEvidence, BuildTarget};
use ployz_core::ids::OperationId;
use ployz_core::operation::{BuildCleanupEvidence, CancellationReason, EventSequence};
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub(super) struct ActiveBuildRegistry {
    builds: Arc<Mutex<BTreeMap<OperationId, ActiveBuild>>>,
}

#[derive(Clone)]
enum ActiveBuild {
    Running {
        target: BuildTarget,
        started: Vec<BuildExecutorAssignment>,
        cancellation_reason: Option<CancellationReason>,
        watch_start_sequence: EventSequence,
    },
    Finalizing {
        target: BuildTarget,
        started: Vec<BuildExecutorAssignment>,
    },
}

pub(super) enum CancellationRequest {
    Accepted {
        executors: Vec<BuildExecutorAssignment>,
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
        target: BuildTarget,
    ) {
        self.builds.lock().await.insert(
            operation_id,
            ActiveBuild::Running {
                target,
                started: Vec::new(),
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
                target: _,
                started,
                cancellation_reason,
                watch_start_sequence,
            }) => {
                if cancellation_reason.is_none() {
                    *cancellation_reason = Some(reason);
                }
                CancellationRequest::Accepted {
                    executors: started.clone(),
                    watch_start_sequence: *watch_start_sequence,
                }
            }
            Some(ActiveBuild::Finalizing { .. }) => CancellationRequest::Finalizing,
            None => CancellationRequest::Missing,
        }
    }

    pub(super) async fn claim_executor_start(
        &self,
        operation_id: &OperationId,
        executor: &BuildExecutorAssignment,
    ) -> bool {
        let mut builds = self.builds.lock().await;
        let Some(build) = builds.get_mut(operation_id) else {
            return false;
        };
        match build {
            ActiveBuild::Running {
                target,
                started,
                cancellation_reason: None,
                watch_start_sequence: _,
            } if assignment_matches_target(target, executor) => {
                if !started.contains(executor) {
                    started.push(executor.clone());
                }
                true
            }
            ActiveBuild::Running {
                cancellation_reason: Some(_) | None,
                ..
            }
            | ActiveBuild::Finalizing { .. } => false,
        }
    }

    pub(super) async fn executor_start_is_authorized(
        &self,
        operation_id: &OperationId,
        executor: &BuildExecutorAssignment,
    ) -> bool {
        matches!(
            self.builds.lock().await.get(operation_id),
            Some(ActiveBuild::Running {
                target: _,
                started,
                cancellation_reason: None,
                watch_start_sequence: _,
            }) if started.contains(executor)
        )
    }

    pub(super) async fn release_executor_start_claim(
        &self,
        operation_id: &OperationId,
        executor: &BuildExecutorAssignment,
    ) {
        if let Some(ActiveBuild::Running { started, .. }) =
            self.builds.lock().await.get_mut(operation_id)
        {
            started.retain(|started| started != executor);
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
                target,
                started,
                cancellation_reason,
                watch_start_sequence: _,
            } => {
                let reason = cancellation_reason.clone();
                let target = target.clone();
                let started = started.clone();
                *build = ActiveBuild::Finalizing { target, started };
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
                target: _,
                cancellation_reason: Some(reason),
                ..
            } => RejectedAdmissionOutcome::Cancelled(reason.clone()),
            ActiveBuild::Running { .. } | ActiveBuild::Finalizing { .. } => {
                RejectedAdmissionOutcome::Interrupted
            }
        };
        let target = match build {
            ActiveBuild::Running { target, .. } | ActiveBuild::Finalizing { target, .. } => {
                target.clone()
            }
        };
        *build = ActiveBuild::Finalizing {
            target,
            started: Vec::new(),
        };
        outcome
    }

    pub(super) async fn cleanup_evidence(
        &self,
        operation_id: &OperationId,
        confirmed: Vec<BuildExecutorAssignment>,
    ) -> BuildCleanupEvidence {
        let (target, started) = self
            .builds
            .lock()
            .await
            .get(operation_id)
            .map(|build| match build {
                ActiveBuild::Running {
                    target, started, ..
                }
                | ActiveBuild::Finalizing { target, started } => (target.clone(), started.clone()),
            })
            .unwrap_or((BuildTarget::Cluster, Vec::new()));
        if started.is_empty() {
            return BuildCleanupEvidence::NotRequired;
        }
        let unconfirmed = started
            .iter()
            .filter(|executor| !confirmed.contains(executor))
            .cloned()
            .collect::<Vec<_>>();
        match target {
            BuildTarget::Cluster if unconfirmed.is_empty() => BuildCleanupEvidence::Completed {
                machine_ids: confirmed
                    .into_iter()
                    .filter_map(|executor| match executor {
                        BuildExecutorAssignment::Cluster { machine_id } => Some(machine_id),
                        BuildExecutorAssignment::External { .. } => None,
                    })
                    .collect(),
            },
            BuildTarget::Cluster => BuildCleanupEvidence::Unconfirmed {
                machine_ids: unconfirmed
                    .into_iter()
                    .filter_map(|executor| match executor {
                        BuildExecutorAssignment::Cluster { machine_id } => Some(machine_id),
                        BuildExecutorAssignment::External { .. } => None,
                    })
                    .collect(),
            },
            BuildTarget::External { .. } if unconfirmed.is_empty() => {
                BuildCleanupEvidence::ExternalCompleted {
                    executors: confirmed
                        .iter()
                        .map(BuildExecutorEvidence::from_assignment)
                        .collect(),
                }
            }
            BuildTarget::External { .. } => BuildCleanupEvidence::ExternalUnconfirmed {
                executors: unconfirmed
                    .iter()
                    .map(BuildExecutorEvidence::from_assignment)
                    .collect(),
            },
        }
    }
}

fn assignment_matches_target(target: &BuildTarget, assignment: &BuildExecutorAssignment) -> bool {
    match (target, assignment) {
        (BuildTarget::Cluster, BuildExecutorAssignment::Cluster { .. }) => true,
        (
            BuildTarget::External { pool_id },
            BuildExecutorAssignment::External {
                pool_id: assignment_pool,
                ..
            },
        ) => pool_id == assignment_pool,
        (BuildTarget::Cluster, BuildExecutorAssignment::External { .. })
        | (BuildTarget::External { .. }, BuildExecutorAssignment::Cluster { .. }) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::MachineId;

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
        registry
            .start(operation_id.clone(), sequence(), BuildTarget::Cluster)
            .await;
        let first = BuildExecutorAssignment::Cluster { machine_id: first };
        let second = BuildExecutorAssignment::Cluster { machine_id: second };
        assert!(registry.claim_executor_start(&operation_id, &first).await);
        assert!(registry.claim_executor_start(&operation_id, &second).await);

        let reason = CancellationReason::try_new("stop").expect("reason");
        let CancellationRequest::Accepted { executors, .. } =
            registry.request_cancellation(&operation_id, reason).await
        else {
            panic!("active cancellation")
        };
        assert_eq!(executors, vec![first, second.clone()]);
        assert!(!registry.claim_executor_start(&operation_id, &second).await);
    }

    #[tokio::test]
    async fn finalization_claims_cancellation_once_and_fences_late_cancel() {
        let registry = ActiveBuildRegistry::default();
        let operation_id = operation_id("build-1");
        registry
            .start(operation_id.clone(), sequence(), BuildTarget::Cluster)
            .await;
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
        registry
            .start(operation_id.clone(), sequence(), BuildTarget::Cluster)
            .await;
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
        registry
            .start(operation_id.clone(), sequence(), BuildTarget::Cluster)
            .await;
        let executor = BuildExecutorAssignment::Cluster {
            machine_id: machine_id.clone(),
        };
        assert!(
            registry
                .claim_executor_start(&operation_id, &executor)
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
                .cleanup_evidence(&operation_id, vec![executor])
                .await,
            BuildCleanupEvidence::Completed {
                machine_ids: vec![machine_id],
            }
        );
    }

    #[tokio::test]
    async fn external_cleanup_keeps_executor_and_seed_provenance() {
        let registry = ActiveBuildRegistry::default();
        let operation_id = operation_id("build-external");
        let pool_id = ployz_core::build::BuildPoolId::try_new("pool-a").expect("pool");
        let executor = BuildExecutorAssignment::External {
            pool_id: pool_id.clone(),
            executor_id: ployz_core::build::BuildExecutorId::try_new("executor-a")
                .expect("executor"),
            image_seed: machine_id("seed-a"),
        };
        registry
            .start(
                operation_id.clone(),
                sequence(),
                BuildTarget::External { pool_id },
            )
            .await;
        assert!(
            registry
                .claim_executor_start(&operation_id, &executor)
                .await
        );
        assert!(
            !registry
                .claim_executor_start(
                    &operation_id,
                    &BuildExecutorAssignment::Cluster {
                        machine_id: machine_id("wrong-origin"),
                    },
                )
                .await
        );
        let _ = registry.claim_finalization(&operation_id).await;

        assert_eq!(
            registry.cleanup_evidence(&operation_id, Vec::new()).await,
            BuildCleanupEvidence::ExternalUnconfirmed {
                executors: vec![BuildExecutorEvidence::from_assignment(&executor)],
            }
        );
        assert_eq!(
            registry
                .cleanup_evidence(&operation_id, vec![executor.clone()])
                .await,
            BuildCleanupEvidence::ExternalCompleted {
                executors: vec![BuildExecutorEvidence::from_assignment(&executor)],
            }
        );
    }
}
