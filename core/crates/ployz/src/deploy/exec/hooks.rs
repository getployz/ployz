use super::machine_ops::MachineOperations;
use super::start::{Attempt, FailedStart, Row, SlotRun, start};
use super::{inspect, remove};
use ployz_core::{
    AnswerWhen, ContainerId, ContainerLocator, ContainerRuntimeObservation, ContainerWant,
    DeploymentLogId, Drain, ExecutionError, Gen, HEALTH_POLL_INTERVAL, HookFailure, MachineId,
    OperationPhase, ResolvedServiceSpec, SlotKey, StopOptions,
};
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy)]
pub(super) enum HookInterruption {
    Cancelled,
    TimedOut,
}

pub(super) async fn run_hook<C: MachineOperations>(
    client: &C,
    row: Row<'_>,
    (machine_id, slot): (&MachineId, SlotKey),
    (spec, run): (&ResolvedServiceSpec, &DeploymentLogId),
    old_hook_containers: &[(MachineId, ContainerId)],
    cancellation: &CancellationToken,
) -> Result<(), ExecutionError> {
    row.running(OperationPhase::RemovingContainer);
    for (old_machine_id, old_container_id) in old_hook_containers {
        remove(client, old_machine_id, old_container_id, Drain::NONE).await?;
    }

    let attempt = Attempt {
        client,
        row: Some(row),
        cancellation,
    };
    let run = SlotRun {
        machine_id,
        slot,
        spec,
        deploy: Some(run),
        r#gen: Gen::FIRST,
        answer: AnswerWhen::Started,
    };
    let container_id = start(&attempt, run, FailedStart::Discard).await?;

    let timeout = spec
        .pre_deploy
        .as_ref()
        .and_then(|hook| hook.timeout_millis)
        .map_or(DEFAULT_HOOK_TIMEOUT, Duration::from_millis);
    let started = Instant::now();
    let deadline = started + timeout;
    let deadline_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    loop {
        row.running(OperationPhase::WaitingForHook {
            container_id,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            deadline_ms,
        });
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
            observed = inspect(client, machine_id, &container_id) => observed?.container,
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
        let wake = std::cmp::min(Instant::now() + HEALTH_POLL_INTERVAL, deadline);
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

pub(super) async fn interrupt_hook<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
    interruption: HookInterruption,
) -> ExecutionError {
    let stop_error = client
        .ensure_container(
            machine_id,
            ContainerWant::Stopped {
                container: ContainerLocator::Id(*container_id),
                drain: Drain::NONE,
                stop: StopOptions {
                    signal: None,
                    grace_period_seconds: Some(0),
                },
            },
        )
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
