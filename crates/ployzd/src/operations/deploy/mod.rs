//! Deploy operation execution over explicit ports.

pub mod driver;
mod facts;
mod failure;
mod images;
mod ports;
mod preparation;
mod types;

use ployz_core::deploy::{
    ContainerRestartPolicy, DeployPlan, DeployPlanStep, DeployPlanningInput, ImageSource,
    ReplicaSlot, plan_namespace_deploy,
};
use ployz_core::ids::{OperationId, StepId, SubjectTokenError};
use ployz_core::machine_runtime::ManagedContainerKind;
use ployz_core::ops::{
    ControlPlaneCommitScope, DeployCleanupFailure, DeployEvidence, DeployRunningStage,
    DeployTransition, FailureMessage, OperatorHint, RetainedArtifact,
};

pub use facts::{
    DeployFactLoadError, DeployMachineCandidates, ManagedCertificateWaitPolicy,
    load_deploy_execution_facts_from_nats,
    load_deploy_execution_facts_from_nats_with_managed_certificate,
};
pub use failure::{
    DeployExecutionError, DeployExecutionStep, DeployFailureRecordError, DeployHealthCheckError,
    DeployOperationRecordError, MachineContainerRuntimeError, MachineRuntimeUnavailableReason,
    PreStartHookRuntimeError,
};
use failure::{DeployExecutionFailure, fail_deploy, with_step_timeout};
use images::{
    dataplane_prepare_request, ensure_images, machine_image_pull, resolve_registry_images,
};
pub use ports::{
    DataplanePreparer, DeployHealthChecker, DeployOperationRecorder, MachineContainerRuntime,
    NamespaceCommitError, NamespaceStateCommitter,
};
pub use preparation::{
    DeployExecutionFacts, DeployExecutionInput, namespace_cleanup_candidates,
    prepare_deploy_execution_command,
};

use crate::roles::machine::protocol::{
    MachineContainerRemoveRpcRequest, MachineContainerRunHookRpcRequest,
    MachineContainerRunRpcRequest, MachineContainerStopRpcRequest,
    MachineEnsureEndpointNetworkRpcRequest,
};
use ployz_core::machine_runtime::ManagedContainerIdentity;
pub use types::{
    DeployCleanupResult, DeployContainer, DeployExecutionCommand, DeployExecutionOutcome,
    DeployExecutionPorts, DeployServiceExecutionCommand, DeployTerminalEvent,
    RunContainerDisposition,
};

pub async fn execute_deploy_operation<R, D, N, H, S>(
    input: DeployExecutionInput,
    ports: DeployExecutionPorts<'_, R, D, N, H, S>,
) -> Result<DeployExecutionOutcome, DeployExecutionError>
where
    R: DeployOperationRecorder,
    D: DataplanePreparer,
    N: MachineContainerRuntime,
    H: DeployHealthChecker,
    S: NamespaceStateCommitter,
{
    let DeployExecutionInput {
        operation_id,
        mut request,
        facts,
        registry_credentials,
    } = input;
    let provisional_command = preparation::prepare_deploy_execution_command_with_credentials(
        operation_id.clone(),
        request.clone(),
        facts.clone(),
        &registry_credentials,
    );
    if let Err(source) = record_stage(
        &provisional_command,
        &mut *ports.recorder,
        DeployTransition::Planning,
    )
    .await
    {
        return fail_deploy(
            provisional_command.clone(),
            &mut *ports.recorder,
            DeployExecutionFailure::new(&provisional_command, source, &[]),
        )
        .await;
    }
    if let Err(source) = resolve_registry_images(
        &provisional_command,
        &mut request,
        &mut *ports.recorder,
        &mut *ports.machine_runtime,
    )
    .await
    {
        return fail_deploy(
            provisional_command.clone(),
            &mut *ports.recorder,
            DeployExecutionFailure::new(&provisional_command, source, &[]),
        )
        .await;
    }
    let command = preparation::prepare_deploy_execution_command_with_credentials(
        operation_id,
        request,
        facts,
        &registry_credentials,
    );
    let mut ports = ports;
    match execute_deploy_after_planning(&command, &mut ports).await {
        Ok(outcome) => Ok(outcome),
        Err(mut failure) => {
            let stop_artifacts = stop_retained_containers(
                &command,
                &mut *ports.machine_runtime,
                failure.retained_stop_targets(),
            )
            .await;
            failure.add_retained_artifacts(stop_artifacts);
            fail_deploy(command, &mut *ports.recorder, failure).await
        }
    }
}

/// In-flight deploy execution state: the command plus the containers started
/// so far, which become retained failure evidence when a stage fails.
struct DeployRun<'a> {
    command: &'a DeployExecutionCommand,
    started_containers: Vec<DeployContainer>,
    health_check_containers: Vec<DeployContainer>,
}

impl<'a> DeployRun<'a> {
    fn new(command: &'a DeployExecutionCommand) -> Self {
        Self {
            command,
            started_containers: Vec::new(),
            health_check_containers: Vec::new(),
        }
    }

    fn container_started(
        &mut self,
        started: DeployContainer,
        disposition: RunContainerDisposition,
    ) {
        match disposition {
            RunContainerDisposition::Created => {
                self.health_check_containers.push(started.clone());
            }
            RunContainerDisposition::Reused => {}
        }
        self.started_containers.push(started);
    }

    fn fail(&self, source: DeployExecutionError) -> DeployExecutionFailure {
        DeployExecutionFailure::new(self.command, source, &self.started_containers)
    }

    fn fail_run_container(
        &self,
        service: &DeployServiceExecutionCommand,
        slot: ReplicaSlot,
        source: DeployExecutionError,
    ) -> DeployExecutionFailure {
        let retained =
            retained_containers_after_run_error(service, &self.started_containers, slot, &source);
        DeployExecutionFailure::with_stop_targets(
            self.command,
            source,
            &self.started_containers,
            &retained,
        )
    }
}

async fn execute_deploy_after_planning<R, D, N, H, S>(
    command: &DeployExecutionCommand,
    ports: &mut DeployExecutionPorts<'_, R, D, N, H, S>,
) -> Result<DeployExecutionOutcome, DeployExecutionFailure>
where
    R: DeployOperationRecorder,
    D: DataplanePreparer,
    N: MachineContainerRuntime,
    H: DeployHealthChecker,
    S: NamespaceStateCommitter,
{
    let mut containers = Vec::new();
    let mut run = DeployRun::new(command);
    let plan = deploy_plan(command).map_err(|source| run.fail(source))?;
    record_evidence(
        command,
        &mut *ports.recorder,
        DeployEvidence::PlanCreated { plan: plan.clone() },
    )
    .await
    .map_err(|source| run.fail(source))?;
    let dataplane_request = dataplane_prepare_request(command, &plan);
    if !plan.volume_pin_commits.is_empty() {
        commit_volume_pins(command, &plan, &mut *ports.namespace_state)
            .await
            .map_err(|source| run.fail(source))?;
    }
    if !plan.services.is_empty() {
        record_running_stage(
            command,
            &mut *ports.recorder,
            DeployRunningStage::PreparingDataplane,
        )
        .await
        .map_err(|source| run.fail(source))?;
        ensure_endpoint_networks(command, &dataplane_request, &mut *ports.machine_runtime)
            .await
            .map_err(|source| run.fail(source))?;
        let dataplane =
            prepare_dataplane(command, dataplane_request.clone(), &mut *ports.dataplane)
                .await
                .map_err(|source| run.fail(source))?;
        record_evidence(
            command,
            &mut *ports.recorder,
            DeployEvidence::DataplanePrepared { report: dataplane },
        )
        .await
        .map_err(|source| run.fail(source))?;
    }
    if command.services().iter().any(|service| {
        matches!(
            &service.request.image_source,
            ImageSource::PushedToSeed { .. }
        )
    }) {
        record_running_stage(
            command,
            &mut *ports.recorder,
            DeployRunningStage::EnsuringImages,
        )
        .await
        .map_err(|source| run.fail(source))?;
        ensure_images(
            command,
            &plan,
            &mut *ports.recorder,
            &mut *ports.machine_runtime,
        )
        .await
        .map_err(|source| run.fail(source))?;
    }
    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployRunningStage::StartingContainers,
    )
    .await
    .map_err(|source| run.fail(source))?;

    for service_plan in &plan.services {
        let Some(service) = command
            .services()
            .iter()
            .find(|service| service.request.service_id == service_plan.service_id)
        else {
            return Err(run.fail(DeployExecutionError::PlanInconsistent {
                service_id: service_plan.service_id.clone(),
            }));
        };
        if let Some(pre_start) = &service_plan.pre_start {
            run_pre_start_hook(
                command,
                service,
                pre_start,
                &dataplane_request.membership,
                &mut *ports.machine_runtime,
            )
            .await
            .map_err(|source| run.fail(source))?;
        }
        for step in &service_plan.steps {
            match step {
                DeployPlanStep::UseExistingContainer {
                    machine_id,
                    container_id,
                    slot,
                } => containers.push(DeployContainer {
                    service_id: service.request.service_id.clone(),
                    namespace_revision_entry_id: service
                        .request
                        .namespace_revision_entry_id
                        .clone(),
                    machine_id: machine_id.clone(),
                    container_id: container_id.clone(),
                    step_id: deploy_step_id(*slot)
                        .map_err(|source| run.fail(DeployExecutionError::StepId(source)))?,
                    requires_docker_healthcheck: false,
                }),
                DeployPlanStep::RunContainer { machine_id, slot } => {
                    let run_result = with_step_timeout(
                        command,
                        DeployExecutionStep::RunContainer {
                            machine_id: machine_id.clone(),
                        },
                        run_deploy_step(
                            &mut *ports.machine_runtime,
                            command,
                            service,
                            machine_id,
                            *slot,
                            &dataplane_request.membership,
                        ),
                    )
                    .await;
                    let (started, disposition) = match run_result {
                        Ok(started) => started,
                        Err(source) => return Err(run.fail_run_container(service, *slot, source)),
                    };
                    containers.push(started.clone());
                    run.container_started(started.clone(), disposition);
                    record_evidence(
                        command,
                        &mut *ports.recorder,
                        DeployEvidence::ContainerStarted {
                            machine_id: started.machine_id.clone(),
                            container_id: started.container_id.clone(),
                        },
                    )
                    .await
                    .map_err(|source| run.fail(source))?;
                }
            }
        }
    }

    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployRunningStage::WaitingForHealth,
    )
    .await
    .map_err(|source| run.fail(source))?;

    record_evidence(
        command,
        &mut *ports.recorder,
        DeployEvidence::HealthCheckStarted,
    )
    .await
    .map_err(|source| run.fail(source))?;

    if !run.health_check_containers.is_empty() {
        let health_result = with_step_timeout(
            command,
            DeployExecutionStep::WaitHealthy,
            (*ports.health_checker).wait_healthy(&run.health_check_containers),
        )
        .await;
        if let Err(source) = health_result {
            return Err(run.fail(source));
        }
    }

    if !command.route_binding_removals().is_empty()
        || command
            .services()
            .iter()
            .any(|service| !service.route_binding_states().is_empty())
    {
        record_running_stage(
            command,
            &mut *ports.recorder,
            DeployRunningStage::RouteCutover,
        )
        .await
        .map_err(|source| run.fail(source))?;

        for service in command.services() {
            cutover_route(command, service, &mut *ports.namespace_state)
                .await
                .map_err(|source| run.fail(source))?;
        }
        remove_undeclared_route_bindings(command, &mut *ports.namespace_state)
            .await
            .map_err(|source| run.fail(source))?;
    }
    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployRunningStage::ServingTargetCommit,
    )
    .await
    .map_err(|source| run.fail(source))?;

    for service in command.services() {
        commit_serving_target_entry(command, service, &mut *ports.namespace_state)
            .await
            .map_err(|source| run.fail(source))?;
    }
    unpublish_omitted_serving_target_entries(command, &mut *ports.namespace_state)
        .await
        .map_err(|source| run.fail(source))?;
    if !plan.cleanup_containers.is_empty() {
        let _ = record_running_stage(
            command,
            &mut *ports.recorder,
            DeployRunningStage::RemovingSupersededContainers,
        )
        .await;
    }
    let cleanup = cleanup_superseded_containers(command, &mut *ports.machine_runtime, &plan).await;
    let terminal_event = record_terminal_state(command, &mut *ports.recorder, &cleanup).await;

    let outcome = DeployExecutionOutcome {
        namespace_id: plan.namespace_id,
        namespace_revision_id: plan.namespace_revision_id,
        containers,
        cleanup,
        terminal_event,
    };

    Ok(outcome)
}

pub(super) fn deploy_plan(
    command: &DeployExecutionCommand,
) -> Result<DeployPlan, DeployExecutionError> {
    plan_namespace_deploy(
        command.request.namespace_id.clone(),
        command.request.namespace_revision_id(),
        command
            .services()
            .iter()
            .map(|service| DeployPlanningInput {
                request: service.request.clone(),
                eligible_machines: service.eligible_machines.clone(),
                existing_replicas: service.existing_replicas.clone(),
                cleanup_candidates: service.cleanup_candidates.clone(),
                volume_pins: service.volume_pins.clone(),
            })
            .collect(),
        command.namespace_cleanup_candidates().to_vec(),
    )
    .map_err(DeployExecutionError::from)
}

async fn run_pre_start_hook<N>(
    command: &DeployExecutionCommand,
    service: &DeployServiceExecutionCommand,
    step: &ployz_core::deploy::PreStartHookStep,
    dataplane_members: &[ployz_core::dataplane::DataplaneMember],
    machine_runtime: &mut N,
) -> Result<(), DeployExecutionError>
where
    N: MachineContainerRuntime,
{
    let Some(pre_start) = &service.request.pre_start else {
        return Err(DeployExecutionError::PlanInconsistent {
            service_id: service.request.service_id.clone(),
        });
    };
    let step_id = StepId::try_new("pre_start").map_err(DeployExecutionError::StepId)?;
    let mut runtime = service.request.runtime.clone();
    runtime.command = Some(pre_start.command.clone());
    runtime.healthcheck = None;
    runtime.restart_policy = ContainerRestartPolicy::No;
    let identity = ManagedContainerIdentity {
        namespace_id: service.request.namespace_id.clone(),
        service_id: service.request.service_id.clone(),
        namespace_revision_entry_id: service.request.namespace_revision_entry_id.clone(),
        operation_id: command.operation_id.clone(),
        step_id,
        kind: ManagedContainerKind::Predeploy,
    };
    let request = MachineContainerRunHookRpcRequest {
        pull: machine_image_pull(service, dataplane_members)?,
        runtime,
        container: identity.clone(),
        timeout_millis: hook_execution_timeout(command).as_millis() as u64,
    };
    let outcome = with_step_timeout(
        command,
        DeployExecutionStep::RunPreStartHook {
            machine_id: step.machine_id.clone(),
        },
        async {
            machine_runtime
                .run_pre_start_hook(&step.machine_id, request)
                .await
                .map_err(DeployExecutionError::PreStartHook)
        },
    )
    .await?;
    if outcome.exit_code != 0 {
        return Err(DeployExecutionError::PreStartHookExited {
            machine_id: step.machine_id.clone(),
            container_id: outcome.container_id,
            exit_code: outcome.exit_code,
            message: FailureMessage::try_new(format!(
                "pre-start hook exited with code {}",
                outcome.exit_code
            ))
            .expect("generated hook failure message is non-empty"),
        });
    }
    machine_runtime
        .remove_pre_start_hook(
            &step.machine_id,
            MachineContainerRemoveRpcRequest {
                operation_id: command.operation_id.clone(),
                container_id: outcome.container_id,
                expected_identity: identity,
            },
        )
        .await
        .map_err(DeployExecutionError::PreStartHook)?;
    Ok(())
}

fn hook_execution_timeout(command: &DeployExecutionCommand) -> std::time::Duration {
    let millis = command.step_timeout().as_millis();
    let bounded = millis.saturating_mul(9).div_euclid(10).max(1);
    let bounded = u64::try_from(bounded).unwrap_or(u64::MAX);
    std::time::Duration::from_millis(bounded)
}

async fn cleanup_superseded_containers<N>(
    command: &DeployExecutionCommand,
    machine_runtime: &mut N,
    plan: &DeployPlan,
) -> Vec<DeployCleanupResult>
where
    N: MachineContainerRuntime,
{
    if plan.cleanup_containers.is_empty() {
        return Vec::new();
    }

    let mut cleanup = Vec::new();
    for target in &plan.cleanup_containers {
        let result = machine_runtime.remove_container(
            &target.machine_id,
            MachineContainerRemoveRpcRequest {
                operation_id: command.operation_id.clone(),
                container_id: target.container_id.clone(),
                expected_identity: target.identity.clone(),
            },
        );
        let result = tokio::time::timeout(command.step_timeout(), result).await;
        match result {
            Ok(Ok(())) => cleanup.push(DeployCleanupResult::Removed(target.clone())),
            Ok(Err(error)) => cleanup.push(DeployCleanupResult::Failed {
                target: target.clone(),
                message: cleanup_failure_message(error),
            }),
            Err(_) => cleanup.push(DeployCleanupResult::Failed {
                target: target.clone(),
                message: FailureMessage::try_new(format!(
                    "container cleanup timed out after {} seconds",
                    command.step_timeout().as_secs()
                ))
                .expect("generated cleanup failure message is non-empty"),
            }),
        }
    }

    cleanup
}

async fn stop_retained_containers<N>(
    command: &DeployExecutionCommand,
    machine_runtime: &mut N,
    containers: &[DeployContainer],
) -> Vec<RetainedArtifact>
where
    N: MachineContainerRuntime,
{
    let mut stop_failures = Vec::new();
    for container in containers {
        let stop = machine_runtime.stop_container(
            &container.machine_id,
            MachineContainerStopRpcRequest {
                operation_id: command.operation_id.clone(),
                container_id: container.container_id.clone(),
                expected_identity: retained_container_identity(command, container),
            },
        );
        match tokio::time::timeout(command.step_timeout(), stop).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                stop_failures.push(container_stop_failed_artifact(
                    container,
                    cleanup_failure_message(error),
                ));
            }
            Err(_) => {
                stop_failures.push(container_stop_failed_artifact(
                    container,
                    FailureMessage::try_new(format!(
                        "retained container stop timed out after {} seconds",
                        command.step_timeout().as_secs()
                    ))
                    .expect("generated retained stop failure message is non-empty"),
                ));
            }
        }
    }
    stop_failures
}

fn container_stop_failed_artifact(
    container: &DeployContainer,
    message: FailureMessage,
) -> RetainedArtifact {
    RetainedArtifact::ContainerStopFailed {
        machine_id: container.machine_id.clone(),
        container_id: container.container_id.clone(),
        message,
        inspect_hint: inspect_hint(&container.container_id),
    }
}

fn retained_containers_after_run_error(
    service: &DeployServiceExecutionCommand,
    started_containers: &[DeployContainer],
    slot: ReplicaSlot,
    source: &DeployExecutionError,
) -> Vec<DeployContainer> {
    let mut retained = started_containers.to_vec();
    if let Some(container) = retained_created_container(service, slot, source)
        && !retained.contains(&container)
    {
        retained.push(container);
    }
    retained
}

fn retained_created_container(
    service: &DeployServiceExecutionCommand,
    slot: ReplicaSlot,
    source: &DeployExecutionError,
) -> Option<DeployContainer> {
    let DeployExecutionError::RunContainer(
        MachineContainerRuntimeError::CreatedContainerStartFailed {
            machine_id,
            container_id,
            ..
        },
    ) = source
    else {
        return None;
    };
    let step_id = deploy_step_id(slot).ok()?;
    Some(DeployContainer {
        service_id: service.request.service_id.clone(),
        namespace_revision_entry_id: service.request.namespace_revision_entry_id.clone(),
        machine_id: machine_id.clone(),
        container_id: container_id.clone(),
        step_id,
        requires_docker_healthcheck: requires_docker_healthcheck(service),
    })
}

/// Whether deploy health gating must wait for Docker to report a health
/// status for this service's containers. Only healthchecks that make Docker
/// run a probe qualify; disabled or image-inherited healthchecks never
/// guarantee a health report, so waiting on them would hang until timeout.
fn requires_docker_healthcheck(service: &DeployServiceExecutionCommand) -> bool {
    service
        .request
        .runtime
        .healthcheck
        .as_ref()
        .is_some_and(ployz_core::deploy::ContainerHealthcheck::reports_docker_health)
}

fn retained_container_identity(
    command: &DeployExecutionCommand,
    container: &DeployContainer,
) -> ManagedContainerIdentity {
    ManagedContainerIdentity {
        namespace_id: command.request.namespace_id.clone(),
        service_id: container.service_id.clone(),
        namespace_revision_entry_id: container.namespace_revision_entry_id.clone(),
        operation_id: command.operation_id.clone(),
        step_id: container.step_id.clone(),
        kind: ManagedContainerKind::Service,
    }
}

fn inspect_hint(container_id: &ployz_core::ids::ContainerId) -> OperatorHint {
    OperatorHint::try_new(format!("ployz container inspect {}", container_id.as_str()))
        .expect("generated inspect hint is non-empty")
}

fn cleanup_evidence(cleanup: &[DeployCleanupResult]) -> DeployEvidence {
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for result in cleanup {
        match result {
            DeployCleanupResult::Removed(target) => removed.push(target.clone()),
            DeployCleanupResult::Failed { target, message } => {
                failed.push(DeployCleanupFailure {
                    target: target.clone(),
                    message: message.clone(),
                });
            }
        }
    }

    DeployEvidence::CleanupFinished { removed, failed }
}

fn cleanup_failure_message(error: MachineContainerRuntimeError) -> FailureMessage {
    match error {
        MachineContainerRuntimeError::ImagePullFailed { message, .. } => message,
        MachineContainerRuntimeError::Unavailable { reason, .. } => reason.failure_message(),
        MachineContainerRuntimeError::OperationStepAmbiguous { .. } => {
            FailureMessage::try_new("cleanup found multiple operation-step containers")
                .expect("generated cleanup failure message is non-empty")
        }
        MachineContainerRuntimeError::CreatedContainerStartFailed { message, .. }
        | MachineContainerRuntimeError::ExistingContainerStartFailed { message, .. }
        | MachineContainerRuntimeError::OperationStepContainerNotStartable { message, .. }
        | MachineContainerRuntimeError::StopContainerFailed { message, .. }
        | MachineContainerRuntimeError::RestartContainerFailed { message, .. }
        | MachineContainerRuntimeError::RemoveContainerFailed { message, .. } => message,
    }
}

async fn record_terminal_state<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    cleanup: &[DeployCleanupResult],
) -> DeployTerminalEvent
where
    R: DeployOperationRecorder,
{
    if !cleanup.is_empty() {
        let record_cleanup = record_evidence(command, recorder, cleanup_evidence(cleanup)).await;
        if DeployCleanupResult::has_failure(cleanup) && record_cleanup.is_err() {
            return DeployTerminalEvent::Missing;
        }
    }

    let outcome = DeployCleanupResult::completion_outcome(cleanup);
    match record_stage(command, recorder, DeployTransition::Completed { outcome }).await {
        Ok(()) => DeployTerminalEvent::Recorded,
        Err(_) => DeployTerminalEvent::Missing,
    }
}

async fn commit_serving_target_entry<S>(
    command: &DeployExecutionCommand,
    service: &DeployServiceExecutionCommand,
    namespace_state: &mut S,
) -> Result<(), DeployExecutionError>
where
    S: NamespaceStateCommitter,
{
    with_step_timeout(
        command,
        DeployExecutionStep::CommitServingTarget,
        namespace_state.replace_serving_target_entry(service.serving_target_entry_state()),
    )
    .await
}

async fn commit_volume_pins<S>(
    command: &DeployExecutionCommand,
    plan: &DeployPlan,
    namespace_state: &mut S,
) -> Result<(), DeployExecutionError>
where
    S: NamespaceStateCommitter,
{
    for state in &plan.volume_pin_commits {
        with_step_timeout(
            command,
            DeployExecutionStep::CommitVolumePins,
            namespace_state.replace_volume_pin(state.clone()),
        )
        .await?;
    }

    Ok(())
}

pub(super) async fn cutover_route<S>(
    command: &DeployExecutionCommand,
    service: &DeployServiceExecutionCommand,
    namespace_state: &mut S,
) -> Result<(), DeployExecutionError>
where
    S: NamespaceStateCommitter,
{
    for state in service.route_binding_states() {
        with_step_timeout(
            command,
            DeployExecutionStep::CommitRoute {
                route: state.target.clone(),
            },
            namespace_state.replace_route_binding(state.clone()),
        )
        .await?;
    }

    Ok(())
}

/// Detach every stored binding whose target no service in the manifest
/// declares, including bindings owned by omitted services.
async fn remove_undeclared_route_bindings<S>(
    command: &DeployExecutionCommand,
    namespace_state: &mut S,
) -> Result<(), DeployExecutionError>
where
    S: NamespaceStateCommitter,
{
    for target in command.route_binding_removals() {
        with_step_timeout(
            command,
            DeployExecutionStep::RemoveRoute {
                route: target.clone(),
            },
            namespace_state.remove_route_binding(target.clone()),
        )
        .await?;
    }

    Ok(())
}

/// Unpublish serving target entries for services the manifest omits, so an
/// omitted service cannot stay serveable in stored state.
async fn unpublish_omitted_serving_target_entries<S>(
    command: &DeployExecutionCommand,
    namespace_state: &mut S,
) -> Result<(), DeployExecutionError>
where
    S: NamespaceStateCommitter,
{
    for entry in command.serving_target_removals() {
        with_step_timeout(
            command,
            DeployExecutionStep::RemoveServingTarget {
                scope: ControlPlaneCommitScope::ServiceEntry {
                    service_id: entry.service_id.clone(),
                    namespace_revision_entry_id: entry.namespace_revision_entry_id.clone(),
                },
            },
            namespace_state.remove_serving_target_entry(entry.clone()),
        )
        .await?;
    }

    Ok(())
}

async fn prepare_dataplane<D>(
    command: &DeployExecutionCommand,
    request: ployz_core::dataplane::DataplanePrepareRequest,
    dataplane: &mut D,
) -> Result<ployz_core::dataplane::PloyzNativeMeshPrepareReport, DeployExecutionError>
where
    D: DataplanePreparer,
{
    with_step_timeout(
        command,
        DeployExecutionStep::PrepareDataplane {
            machines: request.machines(),
        },
        dataplane.prepare_dataplane(request),
    )
    .await
}

async fn ensure_endpoint_networks<N>(
    command: &DeployExecutionCommand,
    request: &ployz_core::dataplane::DataplanePrepareRequest,
    machine_runtime: &mut N,
) -> Result<(), DeployExecutionError>
where
    N: MachineContainerRuntime,
{
    for member in &request.membership {
        let machine_id = &member.machine_id;
        let network_request = MachineEnsureEndpointNetworkRpcRequest {
            operation_id: command.operation_id.clone(),
        };
        with_step_timeout(
            command,
            DeployExecutionStep::EnsureEndpointNetwork {
                machine_id: machine_id.clone(),
            },
            async {
                machine_runtime
                    .ensure_endpoint_network(machine_id, network_request)
                    .await
                    .map_err(DeployExecutionError::RunContainer)
            },
        )
        .await?;
    }
    Ok(())
}

async fn record_evidence<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    evidence: DeployEvidence,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    with_step_timeout(command, DeployExecutionStep::RecordOperationEvent, async {
        recorder
            .record_deploy_evidence(&command.operation_id, evidence)
            .await
            .map_err(DeployExecutionError::RecordEvidence)
    })
    .await
}

async fn record_stage<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    transition: DeployTransition,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    with_step_timeout(
        command,
        DeployExecutionStep::RecordOperationEvent,
        record(recorder, &command.operation_id, transition),
    )
    .await
}

async fn record_running_stage<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    stage: DeployRunningStage,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    with_step_timeout(
        command,
        DeployExecutionStep::RecordOperationEvent,
        record(
            recorder,
            &command.operation_id,
            DeployTransition::Running { stage },
        ),
    )
    .await
}

async fn record<R>(
    recorder: &mut R,
    operation_id: &OperationId,
    transition: DeployTransition,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    recorder
        .record_deploy_transition(operation_id, transition)
        .await
        .map_err(DeployExecutionError::RecordTransition)
}

async fn run_deploy_step<N>(
    machine_runtime: &mut N,
    command: &DeployExecutionCommand,
    service: &DeployServiceExecutionCommand,
    machine_id: &ployz_core::ids::MachineId,
    slot: ReplicaSlot,
    dataplane_members: &[ployz_core::dataplane::DataplaneMember],
) -> Result<(DeployContainer, RunContainerDisposition), DeployExecutionError>
where
    N: MachineContainerRuntime,
{
    let step_id = deploy_step_id(slot).map_err(DeployExecutionError::StepId)?;
    let requires_docker_healthcheck = requires_docker_healthcheck(service);
    let request = MachineContainerRunRpcRequest {
        pull: machine_image_pull(service, dataplane_members)?,
        runtime: service.request.runtime.clone(),
        container: ManagedContainerIdentity {
            namespace_id: service.request.namespace_id.clone(),
            service_id: service.request.service_id.clone(),
            namespace_revision_entry_id: service.request.namespace_revision_entry_id.clone(),
            operation_id: command.operation_id.clone(),
            step_id: step_id.clone(),
            kind: ManagedContainerKind::Service,
        },
    };

    machine_runtime
        .run_container(machine_id, request)
        .await
        .map(|outcome| {
            let disposition = match outcome {
                crate::roles::machine::protocol::MachineRunContainerOutcome::Created { .. } => {
                    RunContainerDisposition::Created
                }
                crate::roles::machine::protocol::MachineRunContainerOutcome::ReusedRunning {
                    ..
                }
                | crate::roles::machine::protocol::MachineRunContainerOutcome::StartedExisting {
                    ..
                } => RunContainerDisposition::Reused,
            };
            (
                DeployContainer {
                    service_id: service.request.service_id.clone(),
                    namespace_revision_entry_id: service
                        .request
                        .namespace_revision_entry_id
                        .clone(),
                    machine_id: machine_id.clone(),
                    container_id: outcome.container_id().clone(),
                    step_id,
                    requires_docker_healthcheck,
                },
                disposition,
            )
        })
        .map_err(DeployExecutionError::RunContainer)
}

fn deploy_step_id(slot: ReplicaSlot) -> Result<StepId, SubjectTokenError> {
    StepId::try_new(format!("run_{}", slot.get()))
}
