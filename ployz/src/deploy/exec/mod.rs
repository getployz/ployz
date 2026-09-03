use std::{future::Future, time::Duration};

use ployz_core::{
    ContainerCreated, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, CreateContainerRequest, DeployEvent, DockerVolumeId,
    ExecutionError, FailedOperation, HookFailure, InspectContainerRequest, MachineAction,
    MachineId, MachineTarget, MembershipObservation, OperationPhase, OperationRow, ProjectName,
    QualifiedService, RemoveContainerRequest, RemoveVolumeRequest, ResolvedServiceSpec, RpcError,
    RpcErrorCode, StartContainerRequest, StopContainerPurpose, StopContainerRequest, UpdateOrder,
    op,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::cluster::ContainerObservationCondition;
use crate::connect::{Client, TARGET_RPC_TIMEOUT, stop_rpc_timeout};

use super::{
    DeployOperation, DeployOutcome, ReplacementCompensation, ReplacementOperation, RestartAttempt,
    StopAttempt,
};

pub(super) use super::progress::Progress;

mod health;
use health::{monitor_container, wait_healthy};

const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub(super) const POLL_INTERVAL: Duration = Duration::from_secs(1);
const RESTART_WAIT: Duration = Duration::from_secs(60);
const RESTART_POLL: Duration = Duration::from_millis(250);

fn failure_outcome_from<E>(
    operations: &[DeployOperation],
    completed_count: usize,
    error: E,
) -> Option<DeployOutcome<E>> {
    let completed = operations.get(..completed_count)?;
    let (failed, unexecuted) = operations.get(completed_count..)?.split_first()?;
    Some(DeployOutcome::Failed {
        completed: completed.to_vec(),
        failed: FailedOperation::Operation {
            operation: failed.clone(),
            error,
        },
        unexecuted: unexecuted.to_vec(),
    })
}

fn replacement_health_failure_outcome_from<E>(
    operations: &[DeployOperation],
    completed_count: usize,
    error: E,
    compensation: ReplacementCompensation<E>,
) -> Option<DeployOutcome<E>> {
    let completed = operations.get(..completed_count)?;
    let (failed, unexecuted) = operations.get(completed_count..)?.split_first()?;
    let DeployOperation::ReplaceContainer(operation) = failed else {
        return None;
    };
    Some(DeployOutcome::Failed {
        completed: completed.to_vec(),
        failed: FailedOperation::ReplacementHealth {
            operation: operation.clone(),
            error,
            compensation,
        },
        unexecuted: unexecuted.to_vec(),
    })
}

pub(super) trait MachineOperations {
    async fn wait_for_container_observations(
        &self,
        container_ids: &[ContainerId],
        condition: ContainerObservationCondition,
        cancellation: &CancellationToken,
    ) -> Result<(), RpcError>;
    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError>;
    async fn create_container(
        &self,
        machine_id: &MachineId,
        kind: ContainerKind,
        project_name: &ProjectName,
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
    async fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError>;
}

impl MachineOperations for Client {
    async fn wait_for_container_observations(
        &self,
        container_ids: &[ContainerId],
        condition: ContainerObservationCondition,
        cancellation: &CancellationToken,
    ) -> Result<(), RpcError> {
        Client::wait_for_container_observations(self, container_ids, condition, cancellation).await
    }

    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        let mut client = self.clone();
        let machines = client.machines().await.map_err(RpcError::from)?;
        let listed = client
            .live_services_from(&machines)
            .await
            .map_err(RpcError::from)?
            .containers;
        if let Some(failure) = listed.failures.into_iter().next() {
            return Err(failure.error);
        }
        if let Some(machine_id) = listed.omissions.into_iter().find(|machine_id| {
            omission_requires_observation(
                machines
                    .iter()
                    .find(|machine| machine.machine.id == *machine_id)
                    .map(|machine| &machine.membership),
            )
        }) {
            return Err(RpcError {
                code: RpcErrorCode::Unavailable,
                message: format!("container observation omitted {machine_id}"),
                details: serde_json::Value::Null,
            });
        }
        Ok(listed
            .successes
            .into_iter()
            .flat_map(|success| success.value)
            .filter(|container| {
                container.kind == ContainerKind::ServiceContainer
                    && container.project_name == service.project
                    && container.service_name == service.name
            })
            .collect())
    }

    async fn create_container(
        &self,
        machine_id: &MachineId,
        kind: ContainerKind,
        project_name: &ProjectName,
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
                project_name: project_name.clone(),
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

    async fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError> {
        self.invoke::<op::RemoveVolume>(
            RemoveVolumeRequest {
                name: id.name.clone(),
                force: false,
            },
            &MachineTarget::from(&id.machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|_| ())
    }
}

fn omission_requires_observation(membership: Option<&MembershipObservation>) -> bool {
    membership.is_none_or(MembershipObservation::invites_rpc)
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
    async fn wait_for_container_observations(
        &self,
        container_ids: &[ContainerId],
        condition: ContainerObservationCondition,
        cancellation: &CancellationToken,
    ) -> Result<(), RpcError> {
        self.inner
            .wait_for_container_observations(container_ids, condition, cancellation)
            .await
    }

    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        wait_out_restart(self.cancellation, || self.inner.service_containers(service)).await
    }

    async fn create_container(
        &self,
        machine_id: &MachineId,
        kind: ContainerKind,
        project_name: &ProjectName,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerCreated, RpcError> {
        self.inner
            .create_container(machine_id, kind, project_name, spec)
            .await
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

    async fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError> {
        wait_out_restart(self.cancellation, || self.inner.remove_volume(id)).await
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

pub(super) async fn execute_operation_sequence<C: MachineOperations>(
    rows: Vec<OperationRow>,
    client: &C,
    cancellation: &CancellationToken,
    tx: Option<UnboundedSender<DeployEvent>>,
    project_name: &ProjectName,
) -> DeployOutcome<ExecutionError> {
    // TODO: there is deliberately no persisted "already run" guard at this boundary.
    let operations: Vec<DeployOperation> = rows.iter().map(|row| row.operation.clone()).collect();
    let client = RestartTolerant {
        inner: client,
        cancellation,
    };
    let mut progress = Progress::new(rows, tx);
    progress.emit();
    for (index, operation) in operations.iter().enumerate() {
        if cancellation.is_cancelled() {
            progress.fail(index, ExecutionError::Cancelled);
            let outcome = failure_outcome_from(&operations, index, ExecutionError::Cancelled)
                .expect("the failed operation belongs to this plan");
            progress.outcome(outcome.clone());
            return outcome;
        }
        match execute_operation(
            operation,
            index,
            &mut progress,
            &client,
            cancellation,
            project_name,
        )
        .await
        {
            Ok(()) => {
                progress.set_completed(index);
            }
            Err(OperationFailure::Ordinary(error)) => {
                progress.fail(index, error.clone());
                let outcome = failure_outcome_from(&operations, index, error)
                    .expect("the failed operation belongs to this plan");
                progress.outcome(outcome.clone());
                return outcome;
            }
            Err(OperationFailure::ReplacementHealth {
                error,
                compensation,
            }) => {
                progress.fail(index, error.clone());
                let outcome = replacement_health_failure_outcome_from(
                    &operations,
                    index,
                    error,
                    *compensation,
                )
                .expect("replacement health failure belongs to the replacement operation");
                progress.outcome(outcome.clone());
                return outcome;
            }
        }
    }
    let outcome = DeployOutcome::Success {
        completed: operations,
    };
    progress.outcome(outcome.clone());
    outcome
}

async fn execute_operation<C: MachineOperations>(
    operation: &DeployOperation,
    index: usize,
    progress: &mut Progress,
    client: &C,
    cancellation: &CancellationToken,
    project_name: &ProjectName,
) -> Result<(), OperationFailure> {
    progress.set_running(index, OperationPhase::Starting);
    match operation {
        DeployOperation::WaitHealthy { dependency, .. } => {
            wait_healthy(client, index, progress, dependency, cancellation)
                .await
                .map_err(Into::into)
        }
        DeployOperation::RunContainer {
            machine_id,
            spec,
            skip_health_monitor,
        } => run_container(
            client,
            index,
            progress,
            machine_id,
            project_name,
            spec,
            *skip_health_monitor,
            cancellation,
        )
        .await
        .map(|_| ())
        .map_err(Into::into),
        DeployOperation::StopContainer {
            machine_id,
            container_id,
            purpose,
        } => {
            progress.set_running(index, OperationPhase::StoppingContainer);
            ignore_not_found(client.stop_container(machine_id, container_id, None).await)
                .map_err(|error| machine_error(MachineAction::StopContainer, error))?;
            if *purpose == StopContainerPurpose::Lifecycle {
                client
                    .wait_for_container_observations(
                        &[*container_id],
                        ContainerObservationCondition::Dropped,
                        cancellation,
                    )
                    .await
                    .map_err(|error| machine_error(MachineAction::InspectContainer, error))?;
            }
            Ok(())
        }
        DeployOperation::RemoveContainer {
            machine_id,
            container_id,
        } => {
            progress.set_running(index, OperationPhase::StoppingContainer);
            ignore_not_found(client.stop_container(machine_id, container_id, None).await)
                .map_err(|error| machine_error(MachineAction::StopContainer, error))?;
            progress.set_running(index, OperationPhase::RemovingContainer);
            ignore_not_found(client.remove_container(machine_id, container_id).await)
                .map_err(|error| machine_error(MachineAction::RemoveContainer, error))?;
            client
                .wait_for_container_observations(
                    &[*container_id],
                    ContainerObservationCondition::Dropped,
                    cancellation,
                )
                .await
                .map_err(|error| machine_error(MachineAction::InspectContainer, error).into())
        }
        DeployOperation::ReplaceContainer(replacement) => {
            replace_container(
                client,
                index,
                progress,
                replacement,
                project_name,
                cancellation,
            )
            .await
        }
        DeployOperation::StopHook {
            machine_id,
            container_id,
        } => {
            progress.set_running(index, OperationPhase::StoppingContainer);
            ignore_not_found(client.stop_container(machine_id, container_id, None).await)
                .map_err(|error| machine_error(MachineAction::StopContainer, error).into())
        }
        DeployOperation::RunHook {
            machine_id,
            spec,
            old_hook_containers,
        } => run_hook(
            client,
            index,
            progress,
            machine_id,
            project_name,
            spec,
            old_hook_containers,
            cancellation,
        )
        .await
        .map_err(Into::into),
        DeployOperation::RemoveVolume { id } => {
            progress.set_running(index, OperationPhase::RemovingVolume);
            ignore_not_found(client.remove_volume(id).await)
                .map_err(|error| machine_error(MachineAction::RemoveVolume, error).into())
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "progress row plus Project label travel together through execute"
)]
async fn create_and_start<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &mut Progress,
    machine_id: &MachineId,
    kind: ContainerKind,
    project_name: &ProjectName,
    spec: &ResolvedServiceSpec,
    cancellation: &CancellationToken,
) -> Result<ContainerCreated, ExecutionError> {
    progress.set_running(index, OperationPhase::CreatingContainer);
    let created = match client
        .create_container(machine_id, kind, project_name, spec)
        .await
    {
        Ok(created) => created,
        Err(_) if cancellation.is_cancelled() => return Err(ExecutionError::Cancelled),
        Err(error) => {
            return Err(machine_error(MachineAction::CreateContainer, error));
        }
    };
    if cancellation.is_cancelled() {
        let _ = client
            .remove_container(machine_id, &created.container_id)
            .await;
        return Err(ExecutionError::Cancelled);
    }
    progress.set_display_name(index, created.display_name.clone());
    progress.set_running(index, OperationPhase::StartingContainer);
    if let Err(error) = client
        .start_container(machine_id, &created.container_id)
        .await
    {
        let _ = client
            .remove_container(machine_id, &created.container_id)
            .await;
        return Err(machine_error(MachineAction::StartContainer, error));
    }
    Ok(created)
}

#[expect(
    clippy::too_many_arguments,
    reason = "progress row plus Project label travel together through execute"
)]
async fn run_container<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &mut Progress,
    machine_id: &MachineId,
    project_name: &ProjectName,
    spec: &ResolvedServiceSpec,
    skip_health_monitor: bool,
    cancellation: &CancellationToken,
) -> Result<ContainerId, ExecutionError> {
    let created = create_and_start(
        client,
        index,
        progress,
        machine_id,
        ContainerKind::ServiceContainer,
        project_name,
        spec,
        cancellation,
    )
    .await?;
    if !skip_health_monitor {
        monitor_container(
            client,
            index,
            progress,
            machine_id,
            &created.container_id,
            spec,
            cancellation,
        )
        .await?;
    }
    client
        .wait_for_container_observations(
            &[created.container_id],
            ContainerObservationCondition::Serving,
            cancellation,
        )
        .await
        .map_err(|error| machine_error(MachineAction::InspectContainer, error))?;
    Ok(created.container_id)
}

async fn replace_container<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &mut Progress,
    operation: &ReplacementOperation,
    project_name: &ProjectName,
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
            progress.set_running(index, OperationPhase::StoppingContainer);
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

    let created = create_and_start(
        client,
        index,
        progress,
        &operation.machine_id,
        ContainerKind::ServiceContainer,
        project_name,
        &operation.spec,
        cancellation,
    )
    .await?;
    let container_id = created.container_id;

    if !operation.skip_health_monitor
        && let Err(error) = monitor_container(
            client,
            index,
            progress,
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
        progress.set_running(index, OperationPhase::Compensating);
        let stop_new_container = StopAttempt::from(
            ignore_not_found(
                client
                    .stop_container(
                        &operation.machine_id,
                        &container_id,
                        Some(stop_grace_period(&operation.spec).unwrap_or(0)),
                    )
                    .await,
            )
            .map_err(|error| machine_error(MachineAction::StopContainer, error)),
        );
        let compensation = if stop_first {
            ReplacementCompensation::StopFirst {
                stop_new_container,
                restart_old_container: if restart_old_on_failure {
                    RestartAttempt::attempted(
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

    client
        .wait_for_container_observations(
            &[container_id],
            ContainerObservationCondition::Serving,
            cancellation,
        )
        .await
        .map_err(|error| machine_error(MachineAction::InspectContainer, error))?;

    if !stop_first {
        progress.set_running(index, OperationPhase::StoppingContainer);
        ignore_not_found(
            client
                .stop_container(&operation.machine_id, &operation.old_container_id, None)
                .await,
        )
        .map_err(|error| machine_error(MachineAction::StopContainer, error))?;
    }
    progress.set_running(index, OperationPhase::RemovingContainer);
    ignore_not_found(
        client
            .remove_container(&operation.machine_id, &operation.old_container_id)
            .await,
    )
    .map_err(|error| machine_error(MachineAction::RemoveContainer, error))?;
    client
        .wait_for_container_observations(
            &[operation.old_container_id],
            ContainerObservationCondition::Dropped,
            cancellation,
        )
        .await
        .map_err(|error| machine_error(MachineAction::InspectContainer, error).into())
}

#[expect(
    clippy::too_many_arguments,
    reason = "progress row plus Project label travel together through execute"
)]
async fn run_hook<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &mut Progress,
    machine_id: &MachineId,
    project_name: &ProjectName,
    spec: &ResolvedServiceSpec,
    old_hook_containers: &[(MachineId, ContainerId)],
    cancellation: &CancellationToken,
) -> Result<(), ExecutionError> {
    progress.set_running(index, OperationPhase::RemovingContainer);
    for (old_machine_id, old_container_id) in old_hook_containers {
        ignore_not_found(
            client
                .remove_container(old_machine_id, old_container_id)
                .await,
        )
        .map_err(|error| machine_error(MachineAction::RemoveContainer, error))?;
    }

    let created = create_and_start(
        client,
        index,
        progress,
        machine_id,
        ContainerKind::PreDeployHook,
        project_name,
        spec,
        cancellation,
    )
    .await?;
    let container_id = created.container_id;

    let timeout = spec
        .pre_deploy
        .as_ref()
        .and_then(|hook| hook.timeout_millis)
        .map_or(DEFAULT_HOOK_TIMEOUT, Duration::from_millis);
    let started = Instant::now();
    let deadline = started + timeout;
    let deadline_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    loop {
        progress.set_running(
            index,
            OperationPhase::WaitingForHook {
                container_id,
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                deadline_ms,
            },
        );
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
                    failure: HookFailure::Exit { code },
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

pub(super) async fn inspect<C: MachineOperations>(
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

fn stop_grace_period(spec: &ResolvedServiceSpec) -> Option<i32> {
    spec.container
        .stop_timeout_secs
        .map(|secs| i32::try_from(secs).unwrap_or(i32::MAX))
}

#[cfg(test)]
#[path = "../exec_tests.rs"]
mod tests;
