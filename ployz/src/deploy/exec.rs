use std::time::Duration;

use ployz_core::{
    ContainerCreated, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, CreateVolumeRequest, HealthObservation, MachineId,
    ResolvedServiceSpec, RpcError, RpcErrorCode, ServiceVolume, UpdateOrder, VolumeSource,
};
use thiserror::Error;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::connect::Client;

use super::{
    DeployOperation, DeployOutcome, DeployPlan, ReplacementCompensation, ReplacementOperation,
    RestartAttempt,
};

const DEFAULT_HEALTH_MONITOR: Duration = Duration::from_secs(5);
const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MachineAction {
    CreateVolume,
    CreateContainer,
    StartContainer,
    InspectContainer,
    StopContainer,
    RemoveContainer,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HealthFailure {
    Cancelled,
    TimedOut,
    Runtime(ContainerRuntimeObservation),
}

#[derive(Clone, Debug, PartialEq)]
pub enum HookFailure {
    Cancelled { stop_error: Option<RpcError> },
    TimedOut { stop_error: Option<RpcError> },
    Exit(i64),
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum ExecutionError {
    #[error("{action:?} failed: {}", error.message)]
    Machine {
        action: MachineAction,
        error: RpcError,
    },
    #[error("container {container_id} failed health monitoring: {failure:?}")]
    Health {
        container_id: ContainerId,
        failure: HealthFailure,
    },
    #[error("hook container {container_id} failed: {failure:?}")]
    Hook {
        container_id: ContainerId,
        failure: HookFailure,
    },
}

/// Executes this finite plan once. Reusing the same value starts a fresh attempt from operation 0.
// TODO(UT-087): there is deliberately no persisted "already run" guard at this boundary.
pub async fn execute_plan(
    plan: &DeployPlan,
    client: &Client,
    cancellation: &CancellationToken,
) -> DeployOutcome<ExecutionError> {
    execute_with(plan, client, cancellation).await
}

trait MachineOperations {
    async fn create_volume(
        &self,
        machine_id: &MachineId,
        volume: &ServiceVolume,
    ) -> Result<(), RpcError>;
    async fn create_container(
        &self,
        machine_id: &MachineId,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerCreated, RpcError>;
    async fn start_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError>;
    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerObservation, RpcError>;
    async fn stop_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        grace_period_seconds: Option<i32>,
    ) -> Result<(), RpcError>;
    async fn remove_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError>;
}

impl MachineOperations for Client {
    async fn create_volume(
        &self,
        machine_id: &MachineId,
        volume: &ServiceVolume,
    ) -> Result<(), RpcError> {
        let VolumeSource::Named {
            name,
            driver,
            labels,
            ..
        } = &volume.source
        else {
            return Err(RpcError {
                code: RpcErrorCode::InvalidArgument,
                message: "volume creation requires a named Docker Volume".into(),
                details: Default::default(),
            });
        };
        crate::service::create_volume_on_machine(
            self,
            machine_id,
            CreateVolumeRequest {
                name: name.clone(),
                driver: driver
                    .as_ref()
                    .map_or_else(|| "local".into(), |driver| driver.name.clone()),
                options: driver
                    .as_ref()
                    .map_or_else(Default::default, |driver| driver.options.clone()),
                labels: labels.clone(),
            },
        )
        .await
    }

    async fn create_container(
        &self,
        machine_id: &MachineId,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerCreated, RpcError> {
        Client::create_container(self, machine_id.clone(), kind, spec.clone()).await
    }

    async fn start_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError> {
        Client::change_container(
            self,
            machine_id.clone(),
            container_id.clone(),
            ployz_core::ContainerAction::Start,
            None,
            None,
        )
        .await
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerObservation, RpcError> {
        Client::inspect_container(self, machine_id.clone(), container_id.clone()).await
    }

    async fn stop_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        grace_period_seconds: Option<i32>,
    ) -> Result<(), RpcError> {
        Client::change_container(
            self,
            machine_id.clone(),
            container_id.clone(),
            ployz_core::ContainerAction::Stop,
            None,
            grace_period_seconds,
        )
        .await
    }

    async fn remove_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError> {
        Client::remove_container(self, machine_id.clone(), container_id.clone()).await
    }
}

enum OperationFailure {
    Ordinary(ExecutionError),
    ReplacementHealth {
        error: ExecutionError,
        compensation: Box<ReplacementCompensation<ExecutionError>>,
    },
}

#[derive(Clone, Copy)]
enum HookInterruption {
    Cancelled,
    TimedOut,
}

enum HealthPoll {
    Complete,
    PendingUntil(Instant),
    Failed(HealthFailure),
}

impl From<ExecutionError> for OperationFailure {
    fn from(error: ExecutionError) -> Self {
        Self::Ordinary(error)
    }
}

async fn execute_with<C: MachineOperations>(
    plan: &DeployPlan,
    client: &C,
    cancellation: &CancellationToken,
) -> DeployOutcome<ExecutionError> {
    let operations = plan.flattened_operations();
    for (index, operation) in operations.iter().enumerate() {
        match execute_operation(operation, client, cancellation).await {
            Ok(()) => {}
            Err(OperationFailure::Ordinary(error)) => {
                return plan
                    .failure_outcome(index, error)
                    .expect("the failed flattened operation belongs to this plan");
            }
            Err(OperationFailure::ReplacementHealth {
                error,
                compensation,
            }) => {
                return plan
                    .replacement_health_failure_outcome(index, error, *compensation)
                    .expect("replacement health failure belongs to the replacement operation");
            }
        }
    }
    plan.success_outcome()
}

async fn execute_operation<C: MachineOperations>(
    operation: &DeployOperation,
    client: &C,
    cancellation: &CancellationToken,
) -> Result<(), OperationFailure> {
    match operation {
        DeployOperation::CreateVolume { machine_id, volume } => client
            .create_volume(machine_id, volume)
            .await
            .map_err(|error| machine_error(MachineAction::CreateVolume, error).into()),
        DeployOperation::RunContainer {
            machine_id,
            spec,
            skip_health_monitor,
        } => run_container(client, machine_id, spec, *skip_health_monitor, cancellation)
            .await
            .map(|_| ())
            .map_err(Into::into),
        DeployOperation::StopContainer {
            machine_id,
            container_id,
        } => client
            .stop_container(machine_id, container_id, None)
            .await
            .map_err(|error| machine_error(MachineAction::StopContainer, error).into()),
        DeployOperation::RemoveContainer {
            machine_id,
            container_id,
        } => {
            ignore_not_found(client.stop_container(machine_id, container_id, None).await)
                .map_err(|error| machine_error(MachineAction::StopContainer, error))?;
            client
                .remove_container(machine_id, container_id)
                .await
                .map_err(|error| machine_error(MachineAction::RemoveContainer, error).into())
        }
        DeployOperation::ReplaceContainer(replacement) => {
            replace_container(client, replacement, cancellation).await
        }
        DeployOperation::StopHook {
            machine_id,
            container_id,
        } => ignore_not_found(client.stop_container(machine_id, container_id, None).await)
            .map_err(|error| machine_error(MachineAction::StopContainer, error).into()),
        DeployOperation::RunHook {
            machine_id,
            spec,
            old_hook_containers,
        } => run_hook(client, machine_id, spec, old_hook_containers, cancellation)
            .await
            .map_err(Into::into),
        DeployOperation::Sequence { .. } => {
            unreachable!("nested sequences are flattened before execution")
        }
    }
}

async fn create_and_start<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    kind: ContainerKind,
    spec: &ResolvedServiceSpec,
) -> Result<ContainerId, ExecutionError> {
    let created = client
        .create_container(machine_id, kind, spec)
        .await
        .map_err(|error| machine_error(MachineAction::CreateContainer, error))?;
    client
        .start_container(machine_id, &created.container_id)
        .await
        .map_err(|error| machine_error(MachineAction::StartContainer, error))?;
    Ok(created.container_id)
}

async fn run_container<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    spec: &ResolvedServiceSpec,
    skip_health_monitor: bool,
    cancellation: &CancellationToken,
) -> Result<ContainerId, ExecutionError> {
    let container_id =
        create_and_start(client, machine_id, ContainerKind::ServiceContainer, spec).await?;
    if !skip_health_monitor {
        monitor_container(client, machine_id, &container_id, spec, cancellation).await?;
    }
    Ok(container_id)
}

async fn replace_container<C: MachineOperations>(
    client: &C,
    operation: &ReplacementOperation,
    cancellation: &CancellationToken,
) -> Result<(), OperationFailure> {
    let stop_first = operation.spec.update.order == UpdateOrder::StopFirst;
    let old_was_active = if stop_first {
        let old = match client
            .inspect_container(&operation.machine_id, &operation.old_container_id)
            .await
        {
            Ok(old) => Some(old),
            Err(error) if error.code == RpcErrorCode::NotFound => None,
            Err(error) => {
                return Err(machine_error(MachineAction::InspectContainer, error).into());
            }
        };
        let active = old.is_some_and(|old| super::is_active_runtime(&old.runtime));
        if active {
            client
                .stop_container(
                    &operation.machine_id,
                    &operation.old_container_id,
                    stop_grace_period(&operation.spec),
                )
                .await
                .map_err(|error| machine_error(MachineAction::StopContainer, error))?;
        }
        active
    } else {
        false
    };

    let container_id = create_and_start(
        client,
        &operation.machine_id,
        ContainerKind::ServiceContainer,
        &operation.spec,
    )
    .await?;

    if !operation.skip_health_monitor
        && let Err(error) = monitor_container(
            client,
            &operation.machine_id,
            &container_id,
            &operation.spec,
            cancellation,
        )
        .await
    {
        let stop_new_container = client
            .stop_container(
                &operation.machine_id,
                &container_id,
                stop_grace_period(&operation.spec),
            )
            .await
            .map_err(|error| machine_error(MachineAction::StopContainer, error));
        let compensation = if stop_first {
            ReplacementCompensation::StopFirst {
                stop_new_container,
                restart_old_container: if old_was_active {
                    RestartAttempt::Attempted(
                        client
                            .start_container(&operation.machine_id, &operation.old_container_id)
                            .await
                            .map_err(|error| machine_error(MachineAction::StartContainer, error)),
                    )
                } else {
                    RestartAttempt::NotAttempted
                },
            }
        } else {
            ReplacementCompensation::StartFirst { stop_new_container }
        };
        return Err(OperationFailure::ReplacementHealth {
            error,
            compensation: Box::new(compensation),
        });
    }

    if !stop_first {
        // TODO(UT-074): Caddy learns about the new container asynchronously, so stopping the old
        // single replica here can still cause a brief interruption.
        ignore_not_found(
            client
                .stop_container(
                    &operation.machine_id,
                    &operation.old_container_id,
                    stop_grace_period(&operation.spec),
                )
                .await,
        )
        .map_err(|error| machine_error(MachineAction::StopContainer, error))?;
    }
    ignore_not_found(
        client
            .remove_container(&operation.machine_id, &operation.old_container_id)
            .await,
    )
    .map_err(|error| machine_error(MachineAction::RemoveContainer, error).into())
}

async fn monitor_container<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
    spec: &ResolvedServiceSpec,
    cancellation: &CancellationToken,
) -> Result<(), ExecutionError> {
    let monitor = spec
        .update
        .monitor_millis
        .map_or(DEFAULT_HEALTH_MONITOR, Duration::from_millis);
    let has_healthcheck = spec
        .container
        .healthcheck
        .as_ref()
        .is_some_and(|healthcheck| {
            !healthcheck.disabled
                && !healthcheck.test.is_empty()
                && healthcheck.test.first().is_none_or(|first| first != "NONE")
        });

    if !has_healthcheck {
        if !monitor.is_zero() {
            tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(health_error(container_id, HealthFailure::Cancelled));
                }
                () = tokio::time::sleep(monitor) => {}
            }
        }
        let observed = inspect(client, machine_id, container_id).await?;
        return if matches!(
            observed.runtime,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::NotConfigured | HealthObservation::Healthy,
            }
        ) {
            Ok(())
        } else {
            Err(health_error(
                container_id,
                HealthFailure::Runtime(observed.runtime),
            ))
        };
    }

    let healthcheck = spec
        .container
        .healthcheck
        .as_ref()
        .expect("health check presence was established");
    let started = Instant::now();
    let monitor_deadline = started + monitor;
    let health_deadline = started + healthcheck_timeout(healthcheck);
    loop {
        if cancellation.is_cancelled() {
            return Err(health_error(container_id, HealthFailure::Cancelled));
        }
        let observed = inspect(client, machine_id, container_id).await?;
        let now = Instant::now();
        let wake_deadline =
            match classify_health(&observed.runtime, now, monitor_deadline, health_deadline) {
                HealthPoll::Complete => return Ok(()),
                HealthPoll::PendingUntil(deadline) => deadline,
                HealthPoll::Failed(failure) => return Err(health_error(container_id, failure)),
            };
        let wake = std::cmp::min(now + POLL_INTERVAL, wake_deadline);
        tokio::select! {
            () = cancellation.cancelled() => {
                return Err(health_error(container_id, HealthFailure::Cancelled));
            }
            () = tokio::time::sleep_until(wake) => {}
        }
    }
}

fn classify_health(
    runtime: &ContainerRuntimeObservation,
    now: Instant,
    monitor_deadline: Instant,
    health_deadline: Instant,
) -> HealthPoll {
    match runtime {
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        } => HealthPoll::Complete,
        ContainerRuntimeObservation::Running {
            health:
                HealthObservation::Starting
                | HealthObservation::NotConfigured
                | HealthObservation::Unrecognized(_),
        } => {
            if now >= health_deadline {
                HealthPoll::Failed(HealthFailure::TimedOut)
            } else {
                HealthPoll::PendingUntil(health_deadline)
            }
        }
        ContainerRuntimeObservation::Created
        | ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy,
        }
        | ContainerRuntimeObservation::Paused
        | ContainerRuntimeObservation::Restarting
        | ContainerRuntimeObservation::Exited { .. }
        | ContainerRuntimeObservation::Removing
        | ContainerRuntimeObservation::Dead
        | ContainerRuntimeObservation::Unknown { .. } => {
            if now >= monitor_deadline {
                HealthPoll::Failed(HealthFailure::Runtime(runtime.clone()))
            } else {
                HealthPoll::PendingUntil(monitor_deadline)
            }
        }
    }
}

async fn run_hook<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    spec: &ResolvedServiceSpec,
    old_hook_containers: &[(MachineId, ContainerId)],
    cancellation: &CancellationToken,
) -> Result<(), ExecutionError> {
    for (old_machine_id, old_container_id) in old_hook_containers {
        ignore_not_found(
            client
                .remove_container(old_machine_id, old_container_id)
                .await,
        )
        .map_err(|error| machine_error(MachineAction::RemoveContainer, error))?;
    }

    let container_id =
        create_and_start(client, machine_id, ContainerKind::PreDeployHook, spec).await?;

    let timeout = spec
        .pre_deploy
        .as_ref()
        .and_then(|hook| hook.timeout_millis)
        .map_or(DEFAULT_HOOK_TIMEOUT, Duration::from_millis);
    let deadline = Instant::now() + timeout;
    loop {
        let observed = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(interrupt_hook(
                    client,
                    machine_id,
                    &container_id,
                    HookInterruption::Cancelled,
                ).await);
            }
            () = tokio::time::sleep_until(deadline) => {
                return Err(interrupt_hook(
                    client,
                    machine_id,
                    &container_id,
                    HookInterruption::TimedOut,
                ).await);
            }
            observed = inspect(client, machine_id, &container_id) => observed?,
        };
        match observed.runtime {
            ContainerRuntimeObservation::Exited { code: 0 } => return Ok(()),
            ContainerRuntimeObservation::Exited { code } => {
                return Err(ExecutionError::Hook {
                    container_id,
                    failure: HookFailure::Exit(code),
                });
            }
            ContainerRuntimeObservation::Created
            | ContainerRuntimeObservation::Running { .. }
            | ContainerRuntimeObservation::Paused
            | ContainerRuntimeObservation::Restarting
            | ContainerRuntimeObservation::Removing
            | ContainerRuntimeObservation::Dead
            | ContainerRuntimeObservation::Unknown { .. } => {}
        }
        let wake = std::cmp::min(Instant::now() + POLL_INTERVAL, deadline);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(interrupt_hook(
                    client,
                    machine_id,
                    &container_id,
                    HookInterruption::Cancelled,
                ).await);
            }
            () = tokio::time::sleep_until(deadline) => {
                return Err(interrupt_hook(
                    client,
                    machine_id,
                    &container_id,
                    HookInterruption::TimedOut,
                ).await);
            }
            () = tokio::time::sleep_until(wake) => {}
        }
    }
}

async fn interrupt_hook<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
    interruption: HookInterruption,
) -> ExecutionError {
    let stop_error = client
        .stop_container(machine_id, container_id, Some(0))
        .await
        .err();
    let failure = match interruption {
        HookInterruption::Cancelled => HookFailure::Cancelled { stop_error },
        HookInterruption::TimedOut => HookFailure::TimedOut { stop_error },
    };
    ExecutionError::Hook {
        container_id: container_id.clone(),
        failure,
    }
}

async fn inspect<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
) -> Result<ContainerObservation, ExecutionError> {
    client
        .inspect_container(machine_id, container_id)
        .await
        .map_err(|error| machine_error(MachineAction::InspectContainer, error))
}

fn machine_error(action: MachineAction, error: RpcError) -> ExecutionError {
    ExecutionError::Machine { action, error }
}

fn ignore_not_found(result: Result<(), RpcError>) -> Result<(), RpcError> {
    match result {
        Err(error) if error.code == RpcErrorCode::NotFound => Ok(()),
        result => result,
    }
}

fn health_error(container_id: &ContainerId, failure: HealthFailure) -> ExecutionError {
    ExecutionError::Health {
        container_id: container_id.clone(),
        failure,
    }
}

fn stop_grace_period(spec: &ResolvedServiceSpec) -> Option<i32> {
    spec.container
        .stop_grace_period_millis
        .map(|millis| i32::try_from(millis.div_ceil(1_000)).unwrap_or(i32::MAX))
}

fn healthcheck_timeout(healthcheck: &ployz_core::HealthcheckSpec) -> Duration {
    let interval = healthcheck.interval_millis.unwrap_or(30_000);
    let timeout = healthcheck.timeout_millis.unwrap_or(30_000);
    let retries = u64::from(healthcheck.retries.unwrap_or(3));
    Duration::from_millis(
        healthcheck
            .start_period_millis
            .unwrap_or_default()
            .saturating_add(interval.saturating_add(timeout).saturating_mul(retries))
            .saturating_add(5_000),
    )
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
