//! Wait for a dependency's Containers to be healthy before its dependents start.

use ployz_core::{
    ContainerRuntimeObservation, DEFAULT_HEALTH_MONITOR, DependencyHealthFailure, ExecutionError,
    HEALTH_POLL_INTERVAL, HealthExpectation, HealthObservation, HealthPoll, HealthcheckSpec,
    OperationPhase, QualifiedService, classify_health, health_deadline_for, healthcheck_deadline,
    observed_health,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{MachineOperations, Progress};

pub(super) async fn wait_healthy<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &mut Progress,
    dependency: &QualifiedService,
    cancellation: &CancellationToken,
) -> Result<(), ExecutionError> {
    let started = Instant::now();
    loop {
        if cancellation.is_cancelled() {
            return Err(dependency_health_error(
                dependency,
                DependencyHealthFailure::Cancelled,
            ));
        }
        let mut containers = client
            .service_containers(dependency)
            .await
            .map_err(|error| {
                dependency_health_error(dependency, DependencyHealthFailure::Observation { error })
            })?;
        containers.retain(|container| {
            container.kind == ployz_core::ContainerKind::ServiceContainer
                && container.namespace == dependency.namespace
                && container.resolved_spec.name == dependency.name
        });
        if containers.is_empty() {
            return Err(dependency_health_error(
                dependency,
                DependencyHealthFailure::NoContainers,
            ));
        }

        let elapsed = started.elapsed();
        let mut pending = None;
        for observed in &containers {
            let health_deadline = health_deadline_for(
                observed.resolved_spec.container.healthcheck.as_ref(),
                observed,
            )
            .or_else(|| {
                matches!(
                    &observed.runtime,
                    ContainerRuntimeObservation::Running {
                        health: HealthObservation::Starting
                            | HealthObservation::NotConfigured
                            | HealthObservation::Unrecognized(_),
                    }
                )
                .then(|| healthcheck_deadline(None))
            });
            let deadline = match classify_health(
                &observed.runtime,
                None,
                elapsed,
                if matches!(
                    observed.resolved_spec.container.healthcheck,
                    Some(HealthcheckSpec::Http(_))
                ) {
                    health_deadline.unwrap_or(DEFAULT_HEALTH_MONITOR)
                } else {
                    DEFAULT_HEALTH_MONITOR
                },
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
