use std::{future::Future, time::Duration};

use ployz_core::{
    ConfiguredHealthcheck, ContainerCreated, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, CreateContainerRequest, CreateVolumeRequest, ExecutionError,
    HealthFailure, HealthObservation, HealthcheckSpec, HookFailure, InspectContainerRequest,
    MachineAction, MachineId, MachineTarget, RemoveContainerRequest, ResolvedServiceSpec, RpcError,
    RpcErrorCode, ServiceVolume, StartContainerRequest, StopContainerRequest, UpdateOrder,
    VolumeSource, op,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::connect::{Client, TARGET_RPC_TIMEOUT, stop_rpc_timeout};

use super::{
    DeployOperation, DeployOutcome, DeployPlan, ReplacementCompensation, ReplacementOperation,
    RestartAttempt,
};

const DEFAULT_HEALTH_MONITOR: Duration = Duration::from_secs(5);
const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const RESTART_WAIT: Duration = Duration::from_secs(60);
const RESTART_POLL: Duration = Duration::from_millis(250);

/// Executes this finite plan once. Reusing the same value starts a fresh attempt from operation 0.
// TODO(UT-087): there is deliberately no persisted "already run" guard at this boundary.
pub async fn execute_plan(
    plan: &DeployPlan,
    client: &Client,
    cancellation: &CancellationToken,
) -> DeployOutcome<ExecutionError> {
    execute_with(plan, client, cancellation).await
}

pub(crate) async fn execute_operations(
    operations: &[DeployOperation],
    client: &Client,
    cancellation: &CancellationToken,
) -> DeployOutcome<ExecutionError> {
    execute_operation_sequence(operations, client, cancellation).await
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
        crate::image::ensure_cluster_image(
            self,
            machine_id,
            &spec.container.image,
            spec.container.pull_policy,
        )
        .await?;
        self.invoke::<op::CreateContainer>(
            CreateContainerRequest {
                kind,
                resolved_spec: spec.clone(),
            },
            &MachineTarget::from(machine_id),
            None,
        )
        .await
    }

    async fn start_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError> {
        self.invoke::<op::StartContainer>(
            StartContainerRequest {
                container_id: *container_id,
            },
            &MachineTarget::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|_| ())
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerObservation, RpcError> {
        self.invoke::<op::InspectContainer>(
            InspectContainerRequest {
                container_id: *container_id,
            },
            &MachineTarget::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|details| details.container)
    }

    async fn stop_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        grace_period_seconds: Option<i32>,
    ) -> Result<(), RpcError> {
        self.invoke::<op::StopContainer>(
            StopContainerRequest {
                container_id: *container_id,
                signal: None,
                grace_period_seconds,
            },
            &MachineTarget::from(machine_id),
            stop_rpc_timeout(grace_period_seconds),
        )
        .await
        .map(|_| ())
    }

    async fn remove_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError> {
        self.invoke::<op::RemoveContainer>(
            RemoveContainerRequest {
                container_id: *container_id,
                remove_volumes: true,
                force: false,
            },
            &MachineTarget::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|_| ())
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

// Wait out a target daemon restart on the same Machine. Create stays
// one-shot — a dropped create response must not mint a second container.
// Do not walk to another connection; that is a different command-entry problem.
struct RestartTolerant<'a, C> {
    inner: &'a C,
    cancellation: &'a CancellationToken,
}

impl<C: MachineOperations> MachineOperations for RestartTolerant<'_, C> {
    async fn create_volume(
        &self,
        machine_id: &MachineId,
        volume: &ServiceVolume,
    ) -> Result<(), RpcError> {
        self.inner.create_volume(machine_id, volume).await
    }

    async fn create_container(
        &self,
        machine_id: &MachineId,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerCreated, RpcError> {
        self.inner.create_container(machine_id, kind, spec).await
    }

    async fn start_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError> {
        wait_out_restart(self.cancellation, || {
            self.inner.start_container(machine_id, container_id)
        })
        .await
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerObservation, RpcError> {
        wait_out_restart(self.cancellation, || {
            self.inner.inspect_container(machine_id, container_id)
        })
        .await
    }

    async fn stop_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        grace_period_seconds: Option<i32>,
    ) -> Result<(), RpcError> {
        wait_out_restart(self.cancellation, || {
            self.inner
                .stop_container(machine_id, container_id, grace_period_seconds)
        })
        .await
    }

    async fn remove_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError> {
        wait_out_restart(self.cancellation, || {
            self.inner.remove_container(machine_id, container_id)
        })
        .await
    }
}

fn is_unavailable(error: &RpcError) -> bool {
    error.code == RpcErrorCode::Unavailable
}

// Start that loses its transport leaves the container Created; retrying
// start is idempotent and finishes it once the daemon is back.
async fn wait_out_restart<T, Fut>(
    cancellation: &CancellationToken,
    mut op: impl FnMut() -> Fut,
) -> Result<T, RpcError>
where
    Fut: Future<Output = Result<T, RpcError>>,
{
    let deadline = Instant::now() + RESTART_WAIT;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(error)
                if is_unavailable(&error)
                    && Instant::now() < deadline
                    && !cancellation.is_cancelled() =>
            {
                tokio::select! {
                    () = cancellation.cancelled() => return Err(error),
                    () = tokio::time::sleep(RESTART_POLL) => {}
                }
            }
            Err(error) => return Err(error),
        }
    }
}

async fn execute_with<C: MachineOperations>(
    plan: &DeployPlan,
    client: &C,
    cancellation: &CancellationToken,
) -> DeployOutcome<ExecutionError> {
    execute_operation_sequence(&plan.operations, client, cancellation).await
}

async fn execute_operation_sequence<C: MachineOperations>(
    operations: impl IntoIterator<Item = &DeployOperation>,
    client: &C,
    cancellation: &CancellationToken,
) -> DeployOutcome<ExecutionError> {
    let operations: Vec<DeployOperation> = operations.into_iter().cloned().collect();
    let client = RestartTolerant {
        inner: client,
        cancellation,
    };
    for (index, operation) in operations.iter().enumerate() {
        match execute_operation(operation, &client, cancellation).await {
            Ok(()) => {}
            Err(OperationFailure::Ordinary(error)) => {
                return DeployPlan::failure_outcome_from(&operations, index, error)
                    .expect("the failed operation belongs to this plan");
            }
            Err(OperationFailure::ReplacementHealth {
                error,
                compensation,
            }) => {
                return DeployPlan::replacement_health_failure_outcome_from(
                    &operations,
                    index,
                    error,
                    *compensation,
                )
                .expect("replacement health failure belongs to the replacement operation");
            }
        }
    }
    DeployOutcome::Success {
        completed: operations,
    }
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
        } => ignore_not_found(client.stop_container(machine_id, container_id, None).await)
            .map_err(|error| machine_error(MachineAction::StopContainer, error).into()),
        DeployOperation::RemoveContainer {
            machine_id,
            container_id,
        } => {
            ignore_not_found(client.stop_container(machine_id, container_id, None).await)
                .map_err(|error| machine_error(MachineAction::StopContainer, error))?;
            ignore_not_found(client.remove_container(machine_id, container_id).await)
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
    let restart_old_on_failure = if stop_first {
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
            match client
                .stop_container(&operation.machine_id, &operation.old_container_id, None)
                .await
            {
                Ok(()) => true,
                Err(error) if error.code == RpcErrorCode::NotFound => false,
                Err(error) => {
                    return Err(machine_error(MachineAction::StopContainer, error).into());
                }
            }
        } else {
            false
        }
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
        if !matches!(&error, ExecutionError::Health { .. }) {
            return Err(error.into());
        }
        let stop_new_container = ignore_not_found(
            client
                .stop_container(
                    &operation.machine_id,
                    &container_id,
                    Some(stop_grace_period(&operation.spec).unwrap_or(0)),
                )
                .await,
        )
        .map_err(|error| machine_error(MachineAction::StopContainer, error));
        let compensation = if stop_first {
            ReplacementCompensation::StopFirst {
                stop_new_container,
                restart_old_container: if restart_old_on_failure {
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
                .stop_container(&operation.machine_id, &operation.old_container_id, None)
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
    // TODO(UT-032): health monitoring remains silent until its final outcome.
    let monitor = spec
        .update
        .monitor_millis
        .map_or_else(default_health_monitor, Duration::from_millis);
    let started = Instant::now();

    if !matches!(
        spec.container.healthcheck,
        Some(HealthcheckSpec::Configured(_))
    ) && !monitor.is_zero()
    {
        tokio::select! {
            () = cancellation.cancelled() => {
                return Err(health_error(container_id, HealthFailure::Cancelled));
            }
            () = tokio::time::sleep(monitor) => {}
        }
    }

    let monitor_deadline = started + monitor;
    loop {
        if cancellation.is_cancelled() {
            return Err(health_error(container_id, HealthFailure::Cancelled));
        }
        let observed = inspect(client, machine_id, container_id).await?;
        let now = Instant::now();
        let health_deadline =
            health_deadline_for(spec.container.healthcheck.as_ref(), &observed, started);
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
    health_deadline: Option<Instant>,
) -> HealthPoll {
    match runtime {
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        } => HealthPoll::Complete,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::NotConfigured,
        } if health_deadline.is_none() => HealthPoll::Complete,
        ContainerRuntimeObservation::Exited { code: 0 } => HealthPoll::Complete,
        ContainerRuntimeObservation::Running {
            health:
                HealthObservation::Starting
                | HealthObservation::NotConfigured
                | HealthObservation::Unrecognized(_),
        } => {
            let Some(health_deadline) = health_deadline else {
                return HealthPoll::Failed(HealthFailure::Runtime(runtime.clone()));
            };
            if now >= health_deadline {
                HealthPoll::Failed(HealthFailure::TimedOut)
            } else {
                HealthPoll::PendingUntil(health_deadline)
            }
        }
        // The process already died. Waiting would let a later healthy snapshot
        // during the next start window succeed the deploy.
        ContainerRuntimeObservation::Restarting => {
            HealthPoll::Failed(HealthFailure::Runtime(runtime.clone()))
        }
        ContainerRuntimeObservation::Created
        | ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy,
        }
        | ContainerRuntimeObservation::Paused
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
        container_id: *container_id,
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
        container_id: *container_id,
        failure,
    }
}

fn stop_grace_period(spec: &ResolvedServiceSpec) -> Option<i32> {
    spec.container
        .stop_timeout_secs
        .map(|secs| i32::try_from(secs).unwrap_or(i32::MAX))
}

fn health_deadline_for(
    spec: Option<&HealthcheckSpec>,
    observed: &ContainerObservation,
    started: Instant,
) -> Option<Instant> {
    match spec {
        Some(HealthcheckSpec::Configured(configured)) => {
            Some(started + healthcheck_timeout(Some(configured)))
        }
        Some(HealthcheckSpec::Disabled) => None,
        None => observed
            .effective_healthcheck
            .as_ref()
            .and_then(HealthcheckSpec::as_configured)
            .map(|check| started + healthcheck_timeout(Some(check)))
            .or_else(|| {
                matches!(
                    &observed.runtime,
                    ContainerRuntimeObservation::Running {
                        health: HealthObservation::Starting | HealthObservation::Unrecognized(_),
                    }
                )
                .then(|| started + healthcheck_timeout(None))
            }),
    }
}

fn default_health_monitor() -> Duration {
    std::env::var(crate::cli::env::HEALTH_MONITOR_PERIOD)
        .ok()
        .and_then(|value| parse_monitor_period(&value))
        .unwrap_or(DEFAULT_HEALTH_MONITOR)
}

fn parse_monitor_period(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value == "0" {
        return Some(Duration::ZERO);
    }
    crate::compose::duration_millis(Some(value))
        .ok()
        .flatten()
        .map(Duration::from_millis)
}

fn healthcheck_timeout(healthcheck: Option<&ConfiguredHealthcheck>) -> Duration {
    let interval = healthcheck
        .and_then(|check| check.interval_millis)
        .unwrap_or(30_000);
    let timeout = healthcheck
        .and_then(|check| check.timeout_millis)
        .unwrap_or(30_000);
    let retries = u64::from(healthcheck.and_then(|check| check.retries).unwrap_or(3));
    Duration::from_millis(
        healthcheck
            .and_then(|check| check.start_period_millis)
            .unwrap_or_default()
            .saturating_add(interval.saturating_add(timeout).saturating_mul(retries))
            .saturating_add(5_000),
    )
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
