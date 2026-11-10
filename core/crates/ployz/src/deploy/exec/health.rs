use ployz_core::{
    ContainerObservation, CopyRotation, DependencyHealthFailure, DeploymentLogId, ExecutionError,
    HEALTH_POLL_INTERVAL, HEALTHCHECK_RUNTIME_GRACE, HealthExpectation, HealthPoll, OperationPhase,
    QualifiedService, Unconfigured, classify_health, health_deadline_for, monitor_deadline,
    observed_health,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{MachineOperations, Progress};

pub(super) async fn wait_healthy<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    dependency: &QualifiedService,
    run: &DeploymentLogId,
    cancellation: &CancellationToken,
) -> Result<(), ExecutionError> {
    let started = Instant::now();
    let mut reported = std::collections::BTreeSet::new();
    loop {
        if cancellation.is_cancelled() {
            return Err(dependency_health_error(
                dependency,
                DependencyHealthFailure::Cancelled,
            ));
        }
        // A Machine whose silence is a listing gap is left out, as a Deploy's gen choice
        // leaves it; an answered error or any other silence still fails the check.
        let listed = client.listed_copies(dependency).await.map_err(|error| {
            dependency_health_error(dependency, DependencyHealthFailure::Observation { error })
        })?;
        for silent in listed.unanswered {
            if reported.insert(silent.failure.machine_id) {
                silent.warn(progress);
            }
        }
        let mut containers = listed.copies;
        containers.retain(|container| {
            container.kind == ployz_core::ContainerKind::ServiceContainer
                && container.namespace == dependency.namespace
                && container.resolved_spec.name == dependency.name
        });
        // This run's copies are the ones it is bringing up: Pending or In.
        // Without any, the copies already In serve the dependent; a copy
        // Held or Out by another run is leaving and must not fail it.
        let of_this_run = |container: &ContainerObservation| {
            container.deploy_label().as_ref() == Some(run)
                && matches!(
                    container.rotation(),
                    CopyRotation::Pending | CopyRotation::In
                )
        };
        if containers.iter().any(of_this_run) {
            containers.retain(of_this_run);
        } else {
            containers.retain(|container| *container.rotation() == CopyRotation::In);
        }
        if containers.is_empty() {
            return Err(dependency_health_error(
                dependency,
                DependencyHealthFailure::NoContainers,
            ));
        }

        let elapsed = started.elapsed();
        let mut pending = None;
        for observed in &containers {
            let healthcheck = observed.resolved_spec.container.healthcheck.as_ref();
            let health_deadline =
                health_deadline_for(healthcheck, observed, Unconfigured::UseDeadline);
            let deadline = match classify_health(
                &observed.runtime,
                None,
                elapsed,
                monitor_deadline(healthcheck, health_deadline, HEALTHCHECK_RUNTIME_GRACE),
                health_deadline,
                HealthExpectation::Healthy,
            ) {
                HealthPoll::Complete => continue,
                HealthPoll::PendingUntil(deadline) => deadline,
                HealthPoll::Failed(failure) => {
                    return Err(dependency_health_error(
                        dependency,
                        DependencyHealthFailure::Container {
                            container_id: observed.container_id,
                            failure,
                        },
                    ));
                }
            };
            let health = observed_health(&observed.runtime);
            if pending
                .as_ref()
                .is_none_or(|(_, _, pending_deadline)| deadline < *pending_deadline)
            {
                pending = Some((observed.container_id, health, deadline));
            }
        }
        let Some((container_id, health, deadline)) = pending else {
            return Ok(());
        };
        progress.set_running(
            index,
            OperationPhase::WaitingForHealth {
                container_id,
                health,
                elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                deadline_ms: u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
            },
        );
        tokio::select! {
            () = cancellation.cancelled() => {
                return Err(dependency_health_error(
                    dependency,
                    DependencyHealthFailure::Cancelled,
                ));
            }
            () = tokio::time::sleep_until(started + std::cmp::min(elapsed + HEALTH_POLL_INTERVAL, deadline)) => {}
        }
    }
}

fn dependency_health_error(
    dependency: &QualifiedService,
    failure: DependencyHealthFailure,
) -> ExecutionError {
    ExecutionError::DependencyHealth {
        dependency: dependency.clone(),
        failure,
    }
}
