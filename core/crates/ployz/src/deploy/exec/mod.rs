use std::{collections::BTreeMap, future::Future, time::Duration};

use ployz_core::{
    ContainerCreated, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, CreateContainerRequest, DeployEvent, DockerVolumeId,
    ExecutionError, FailedOperation, HookFailure, InspectContainerRequest, MachineAction,
    MachineId, MachineTarget, MembershipObservation, Namespace, OperationPhase, PullImageRequest,
    PullPolicy, QualifiedService, RemoveContainerRequest, RemoveVolumeRequest, ResolvedServiceSpec,
    RpcError, RpcErrorCode, StartContainerRequest, StopContainerPurpose, StopContainerRequest,
    UpdateOrder, op,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::cluster::ContainerObservationCondition;
use crate::connect::{Client, TARGET_RPC_TIMEOUT, stop_rpc_timeout};
use ployz_core::EnvironmentValues;

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

fn replacement_failure_outcome_from<E>(
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
        failed: FailedOperation::Replacement {
            operation: operation.clone(),
            error,
            compensation,
        },
        unexecuted: unexecuted.to_vec(),
    })
}

/// One Deploy run's identity. Every create the run makes is keyed off it, so a
/// later Deploy or Retry never receives a Container this run created.
#[derive(Clone, Copy, Debug)]
pub(super) struct DeployRun(uuid::Uuid);

impl DeployRun {
    pub(super) fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    /// The key of the create `operation` makes. An Operation creates at most one
    /// Container, so the key is unique within the run.
    pub(super) fn creation_key(self, operation: usize) -> CreationKey {
        CreationKey(format!("deploy:{}:{operation}", self.0))
    }
}

/// Retry identity of one create. The daemon answers a repeated key with the
/// Container it already made, so a create retried after a lost reply never
/// mints a second one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CreationKey(String);

pub(super) trait MachineOperations {
    /// Make the image `spec` runs present on the Machine as its pull policy asks.
    async fn pull_image(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), RpcError>;
    async fn prepare_volumes(
        &self,
        machine_id: &MachineId,
        specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<ployz_core::PreparedVolumes, RpcError>;
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
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        key: &CreationKey,
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
    async fn pull_image(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), RpcError> {
        let image = &spec.container.image;
        let policy = spec.container.pull_policy;
        crate::image::ensure_cluster_image(self, machine_id, image, policy).await?;
        match self
            .invoke::<op::PullImage>(
                PullImageRequest {
                    image: image.clone(),
                    pull_policy: policy,
                    registry_auth: self.registry_auth.get(&spec.name).cloned(),
                },
                &MachineTarget::from(machine_id),
                None,
            )
            .await
        {
            Ok(_) => Ok(()),
            // A daemon from before PullImage still pulls while it creates.
            Err(RpcError {
                code: RpcErrorCode::Unsupported,
                ..
            }) => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn prepare_volumes(
        &self,
        machine_id: &MachineId,
        specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<ployz_core::PreparedVolumes, RpcError> {
        self.invoke::<op::PrepareVolumes>(
            ployz_core::PrepareVolumesRequest {
                specs: specs.to_vec(),
            },
            &MachineTarget::from(machine_id),
            Some(Duration::from_secs(120)),
        )
        .await
    }
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
            .live_services_from(&machines, EnvironmentValues::Redacted)
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
                    && container.namespace == service.namespace
                    && container.resolved_spec.name == service.name
            })
            .collect())
    }

    async fn create_container(
        &self,
        machine_id: &MachineId,
        kind: ContainerKind,
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        key: &CreationKey,
    ) -> Result<ContainerCreated, RpcError> {
        self.invoke::<op::CreateContainer>(
            CreateContainerRequest {
                deployment_id: self.deployment_id.clone(),
                creation_key: Some(key.0.clone()),
                kind,
                namespace: namespace.clone(),
                resolved_spec: spec.clone(),
                registry_auth: self.registry_auth.get(&spec.name).cloned(),
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
        crate::ingress::stop_container(
            self,
            machine_id,
            StopContainerRequest {
                container_id: *container_id,
                signal: None,
                grace_period_seconds,
            },
            stop_rpc_timeout(grace_period_seconds, 1),
        )
        .await
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
    Replacement {
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

// Wait out a target daemon restart on the same Machine. Every create carries
// its run's creation key, so a retried create finds the Container its lost
// reply made. Do not walk to another connection; that is a different
// command-entry problem.
struct RestartTolerant<'a, C> {
    inner: &'a C,
    cancellation: &'a CancellationToken,
}

impl<C: MachineOperations> MachineOperations for RestartTolerant<'_, C> {
    async fn pull_image(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), RpcError> {
        wait_out_restart(self.cancellation, || {
            self.inner.pull_image(machine_id, spec)
        })
        .await
    }

    async fn prepare_volumes(
        &self,
        machine_id: &MachineId,
        specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<ployz_core::PreparedVolumes, RpcError> {
        self.inner.prepare_volumes(machine_id, specs).await
    }
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
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        key: &CreationKey,
    ) -> Result<ContainerCreated, RpcError> {
        wait_out_restart(self.cancellation, || {
            self.inner
                .create_container(machine_id, kind, namespace, spec, key)
        })
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

struct ImagePull<'a> {
    spec: &'a ResolvedServiceSpec,
    first_operation: usize,
}

fn pull_rank(policy: PullPolicy) -> u8 {
    match policy {
        PullPolicy::Never => 0,
        PullPolicy::Missing => 1,
        PullPolicy::Always => 2,
    }
}

// Each image once per Machine, under the strictest policy any operation asks
// for, remembering the earliest operation that needs it.
// One Service's registry credentials pull an image every Service shares, on
// purpose: under `Missing` the first create pulled it and the rest found it.
fn image_pulls(operations: &[DeployOperation]) -> BTreeMap<MachineId, Vec<ImagePull<'_>>> {
    let mut pulls: BTreeMap<MachineId, Vec<ImagePull<'_>>> = BTreeMap::new();
    for (first_operation, operation) in operations.iter().enumerate() {
        let Some(spec) = operation.spec() else {
            continue;
        };
        let machine = pulls.entry(operation.machine_id()).or_default();
        match machine
            .iter_mut()
            .find(|pull| pull.spec.container.image == spec.container.image)
        {
            Some(pull) => {
                if pull_rank(spec.container.pull_policy)
                    > pull_rank(pull.spec.container.pull_policy)
                {
                    pull.spec = spec;
                }
            }
            None => machine.push(ImagePull {
                spec,
                first_operation,
            }),
        }
    }
    pulls
}

// Machines pull in parallel; a failure names the earliest operation it blocks.
async fn pull_images<C: MachineOperations>(
    operations: &[DeployOperation],
    client: &C,
) -> Result<(), (usize, RpcError)> {
    let pulls = image_pulls(operations);
    let results =
        futures_util::future::join_all(pulls.iter().map(|(machine_id, pulls)| async move {
            for pull in pulls {
                client
                    .pull_image(machine_id, pull.spec)
                    .await
                    .map_err(|error| (pull.first_operation, error))?;
            }
            Ok(())
        }))
        .await;
    results
        .into_iter()
        .filter_map(Result::err)
        .min_by_key(|(first_operation, _)| *first_operation)
        .map_or(Ok(()), Err)
}

pub(super) async fn execute_operation_sequence<C: MachineOperations>(
    plan: &super::DeployPlan,
    client: &C,
    cancellation: &CancellationToken,
    tx: Option<UnboundedSender<DeployEvent>>,
) -> DeployOutcome<ExecutionError> {
    // TODO: there is deliberately no persisted "already run" guard at this boundary.
    let operations = plan.operations();
    let namespace = &plan.namespace;
    let mut progress = Progress::new(plan.pending_rows(), tx);
    progress.emit();
    let client = RestartTolerant {
        inner: client,
        cancellation,
    };
    let run = DeployRun::new();
    if let Err((index, error)) = pull_images(operations, &client).await {
        let error = machine_error(MachineAction::PullImage, error);
        progress.fail(index, error.clone());
        let mut unexecuted = operations.to_vec();
        let operation = unexecuted.remove(index);
        let outcome = DeployOutcome::Failed {
            completed: Vec::new(),
            failed: FailedOperation::Operation { operation, error },
            unexecuted,
        };
        progress.outcome(outcome.clone());
        return outcome;
    }
    for (index, operation) in operations.iter().enumerate() {
        if cancellation.is_cancelled() {
            progress.fail(index, ExecutionError::Cancelled);
            let outcome = failure_outcome_from(operations, index, ExecutionError::Cancelled)
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
            namespace,
            run,
        )
        .await
        {
            Ok(()) => {
                progress.set_completed(index);
            }
            Err(OperationFailure::Ordinary(error)) => {
                progress.fail(index, error.clone());
                let outcome = failure_outcome_from(operations, index, error)
                    .expect("the failed operation belongs to this plan");
                progress.outcome(outcome.clone());
                return outcome;
            }
            Err(OperationFailure::Replacement {
                error,
                compensation,
            }) => {
                progress.fail(index, error.clone());
                let outcome =
                    replacement_failure_outcome_from(operations, index, error, *compensation)
                        .expect("replacement failure belongs to the replacement operation");
                progress.outcome(outcome.clone());
                return outcome;
            }
        }
    }
    let outcome = DeployOutcome::Success {
        completed: operations.to_vec(),
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
    namespace: &Namespace,
    run: DeployRun,
) -> Result<(), OperationFailure> {
    progress.set_running(index, OperationPhase::Starting);
    match operation {
        DeployOperation::PrepareVolumes { machine_id, specs } => client
            .prepare_volumes(machine_id, specs)
            .await
            .map(|_| ())
            .map_err(|mut error| {
                if !error.details.is_object() {
                    error.details = serde_json::json!({});
                }
                error
                    .details
                    .as_object_mut()
                    .expect("details normalized to object")
                    .insert("machine_id".into(), serde_json::json!(machine_id));
                machine_error(MachineAction::PrepareVolumes, error).into()
            }),
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
            namespace,
            spec,
            *skip_health_monitor,
            cancellation,
            run,
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
                namespace,
                cancellation,
                run,
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
            namespace,
            spec,
            old_hook_containers,
            cancellation,
            run,
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

/// Placement convergence's one move: start `spec` on `to`, wait until it serves, then
/// remove `old` from `from`. A new Container that never serves is removed again, so
/// `old` keeps serving and the Container count holds. No hooks run.
pub(super) async fn move_container(
    client: &Client,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    to: &MachineId,
    (from, old): (&MachineId, &ContainerId),
    cancellation: &CancellationToken,
) -> Result<ContainerId, ExecutionError> {
    let client = RestartTolerant {
        inner: client,
        cancellation,
    };
    let mut progress = Progress::new(Vec::new(), None);
    let run = DeployRun::new();
    let created = create_and_start(
        &client,
        0,
        &mut progress,
        to,
        ContainerKind::ServiceContainer,
        namespace,
        spec,
        run,
        cancellation,
    )
    .await?;
    let new = created.container_id;
    if let Err(failure) = serve(
        &client,
        0,
        &mut progress,
        to,
        &new,
        spec,
        false,
        cancellation,
    )
    .await
    {
        let _ = client.stop_container(to, &new, None).await;
        let _ = client.remove_container(to, &new).await;
        return Err(failure.into());
    }
    let removal = DeployOperation::RemoveContainer {
        machine_id: *from,
        container_id: *old,
    };
    execute_operation(
        &removal,
        0,
        &mut progress,
        &client,
        cancellation,
        namespace,
        run,
    )
    .await
    .map_err(|failure| match failure {
        OperationFailure::Ordinary(error) | OperationFailure::Replacement { error, .. } => error,
    })?;
    Ok(new)
}

#[expect(
    clippy::too_many_arguments,
    reason = "progress row plus Namespace label travel together through execute"
)]
async fn create_and_start<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &mut Progress,
    machine_id: &MachineId,
    kind: ContainerKind,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    run: DeployRun,
    cancellation: &CancellationToken,
) -> Result<ContainerCreated, ExecutionError> {
    progress.set_running(index, OperationPhase::CreatingContainer);
    let key = run.creation_key(index);
    let created = match client
        .create_container(machine_id, kind, namespace, spec, &key)
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
    reason = "progress row plus Namespace label travel together through execute"
)]
async fn run_container<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &mut Progress,
    machine_id: &MachineId,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    skip_health_monitor: bool,
    cancellation: &CancellationToken,
    run: DeployRun,
) -> Result<ContainerId, ExecutionError> {
    let created = create_and_start(
        client,
        index,
        progress,
        machine_id,
        ContainerKind::ServiceContainer,
        namespace,
        spec,
        run,
        cancellation,
    )
    .await?;
    serve(
        client,
        index,
        progress,
        machine_id,
        &created.container_id,
        spec,
        skip_health_monitor,
        cancellation,
    )
    .await
    .map_err(ExecutionError::from)?;
    Ok(created.container_id)
}

/// Why a started Service Container did not serve.
enum ServeFailure {
    /// It failed its health check, or the serving barrier ended without
    /// proof it serves: a timeout or any error other than a cancel.
    Unproven(ExecutionError),
    /// The Deploy was cancelled, or a health check could not ask the Machine.
    Interrupted(ExecutionError),
}

impl From<ServeFailure> for ExecutionError {
    fn from(failure: ServeFailure) -> Self {
        match failure {
            ServeFailure::Unproven(error) | ServeFailure::Interrupted(error) => error,
        }
    }
}

// A new Service Container succeeds once it serves, not merely once it is healthy.
#[expect(
    clippy::too_many_arguments,
    reason = "progress row plus health spec travel together through execute"
)]
async fn serve<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &mut Progress,
    machine_id: &MachineId,
    container_id: &ContainerId,
    spec: &ResolvedServiceSpec,
    skip_health_monitor: bool,
    cancellation: &CancellationToken,
) -> Result<(), ServeFailure> {
    if !skip_health_monitor {
        monitor_container(
            client,
            index,
            progress,
            machine_id,
            container_id,
            spec,
            cancellation,
        )
        .await
        .map_err(|error| match error {
            ExecutionError::Health { .. } => ServeFailure::Unproven(error),
            ExecutionError::Machine { .. }
            | ExecutionError::DependencyHealth { .. }
            | ExecutionError::Hook { .. }
            | ExecutionError::Cancelled => ServeFailure::Interrupted(error),
        })?;
    }
    wait_serving(client, *container_id, cancellation)
        .await
        .map_err(|error| {
            if cancellation.is_cancelled() {
                ServeFailure::Interrupted(error)
            } else {
                ServeFailure::Unproven(error)
            }
        })
}

async fn wait_serving<C: MachineOperations>(
    client: &C,
    container_id: ContainerId,
    cancellation: &CancellationToken,
) -> Result<(), ExecutionError> {
    client
        .wait_for_container_observations(
            &[container_id],
            ContainerObservationCondition::Serving,
            cancellation,
        )
        .await
        .map_err(|error| machine_error(MachineAction::InspectContainer, error))
}

async fn replace_container<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &mut Progress,
    operation: &ReplacementOperation,
    namespace: &Namespace,
    cancellation: &CancellationToken,
    run: DeployRun,
) -> Result<(), OperationFailure> {
    let stop_first = operation.spec.update.order == UpdateOrder::StopFirst;
    let old_stopped = if stop_first {
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

    let started = match create_and_start(
        client,
        index,
        progress,
        &operation.machine_id,
        ContainerKind::ServiceContainer,
        namespace,
        &operation.spec,
        run,
        cancellation,
    )
    .await
    {
        Ok(created) => serve(
            client,
            index,
            progress,
            &operation.machine_id,
            &created.container_id,
            &operation.spec,
            operation.skip_health_monitor,
            cancellation,
        )
        .await
        .map_err(|failure| match failure {
            ServeFailure::Unproven(error) => (Candidate::Unproven(created.container_id), error),
            ServeFailure::Interrupted(error) => (Candidate::Started(created.container_id), error),
        }),
        Err(error) => Err((Candidate::None, error)),
    };
    if let Err((candidate, error)) = started {
        return Err(compensate(
            client,
            index,
            progress,
            operation,
            old_stopped,
            candidate,
            error,
        )
        .await);
    }

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

/// How far a replacement's new Container got before the replacement failed.
#[derive(Clone, Copy)]
enum Candidate {
    /// Create or start failed; no new Container runs.
    None,
    /// Started, then the Deploy was cancelled or a health check could not
    /// ask the Machine.
    Started(ContainerId),
    /// Started, then failed its health check or the serving barrier ended
    /// without proof it serves, including on a barrier RPC error.
    Unproven(ContainerId),
}

// A failure after the old container stopped restores it. With the old one
// untouched, only an unproven candidate is compensated: it is stopped, not
// removed, so its logs stay readable.
async fn compensate<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &mut Progress,
    operation: &ReplacementOperation,
    old_stopped: bool,
    candidate: Candidate,
    error: ExecutionError,
) -> OperationFailure {
    let compensation = match (old_stopped, candidate) {
        (false, Candidate::None | Candidate::Started(_)) => return error.into(),
        (false, Candidate::Unproven(container_id)) => {
            progress.set_running(index, OperationPhase::Compensating);
            ReplacementCompensation::OldUntouched {
                stop_new_container: stop_candidate(client, operation, &container_id).await,
            }
        }
        (true, candidate) => {
            progress.set_running(index, OperationPhase::Compensating);
            let stop_new_container = match candidate {
                Candidate::None => None,
                Candidate::Started(container_id) | Candidate::Unproven(container_id) => {
                    Some(stop_candidate(client, operation, &container_id).await)
                }
            };
            ReplacementCompensation::OldStopped {
                stop_new_container,
                restart_old_container: restore_old_container(client, operation).await,
            }
        }
    };
    OperationFailure::Replacement {
        error,
        compensation: Box::new(compensation),
    }
}

async fn stop_candidate<C: MachineOperations>(
    client: &C,
    operation: &ReplacementOperation,
    container_id: &ContainerId,
) -> StopAttempt<ExecutionError> {
    StopAttempt::from(
        ignore_not_found(
            client
                .stop_container(
                    &operation.machine_id,
                    container_id,
                    Some(stop_grace_period(&operation.spec).unwrap_or(0)),
                )
                .await,
        )
        .map_err(|error| machine_error(MachineAction::StopContainer, error)),
    )
}

// Cancelling the Deploy does not abandon the container it stopped. The wait stays
// bounded because `wait_for_container_observations` gives up after its own
// `BARRIER_TIMEOUT` (cluster/container_observations.rs).
async fn restore_old_container<C: MachineOperations>(
    client: &C,
    operation: &ReplacementOperation,
) -> RestartAttempt<ExecutionError> {
    let restored = async {
        client
            .start_container(&operation.machine_id, &operation.old_container_id)
            .await
            .map_err(|error| machine_error(MachineAction::StartContainer, error))?;
        wait_serving(
            client,
            operation.old_container_id,
            &CancellationToken::new(),
        )
        .await
    }
    .await;
    RestartAttempt::from(restored)
}

#[expect(
    clippy::too_many_arguments,
    reason = "progress row plus Namespace label travel together through execute"
)]
async fn run_hook<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &mut Progress,
    machine_id: &MachineId,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    old_hook_containers: &[(MachineId, ContainerId)],
    cancellation: &CancellationToken,
    run: DeployRun,
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
        namespace,
        spec,
        run,
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

#[cfg(test)]
#[path = "../exec_cluster_tests.rs"]
mod cluster_tests;
