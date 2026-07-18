//! Deploy operation execution over explicit ports.

mod completion;
pub mod driver;
mod facts;
mod failure;
mod images;
mod machine_client;
mod phase;
mod placement;
mod ports;
mod preparation;
mod preview;
mod step;
mod types;

use ployz_core::deploy::{
    ContainerRestartPolicy, DeployCleanupContainer, DeployPlan, DeployPlanStep,
    DeployPlanningContext, ImageSource, ReplicaSlot, plan_namespace_deploy,
};
use ployz_core::ids::{OperationId, StepId};
use ployz_core::machine::runtime::ManagedContainerKind;
use ployz_core::operation::{
    ControlPlaneCommitScope, DeployCleanupFailure, DeployEvidence, DeployImageCleanup,
    DeployPhaseNumber, DeployPhaseOutcome, DeployRunningStage, DeployServiceResult,
    DeployTransition, FailureMessage, OperatorHint, RetainedArtifact,
};

pub use crate::control::role_client::machine::MachineVolumeEnsureError;
#[cfg(test)]
pub use crate::roles::machine::MachineRuntimeUnavailableReason;
use completion::{deploy_completion_outcome, plan_has_global_deferrals};
pub use facts::{
    DeployFactLoadError, load_deploy_execution_facts_from_nats, validate_deploy_route_admission,
};
pub use failure::{
    DeployExecutionError, DeployHealthCheckError, MachineContainerRuntimeError,
    PreStartHookRuntimeError,
};
use failure::{DeployExecutionFailure, fail_deploy};
#[cfg(test)]
pub(crate) use images::execute_cleanup_actions;
use images::{
    dataplane_membership, machine_image_pull, resolve_registry_images, validate_pushed_platforms,
};
use phase::{CoarsePhaseProgress, DeployRun};
pub use ports::{
    CertificateProvisioner, DeployHealthChecker, DeployOperationRecorder, DeployPhasePromotion,
    MachineContainerRuntime, MachineImageRemovalRuntime, NamespaceCommitError,
    NamespaceStateCommitter,
};
pub use preparation::{AutomaticHostnameMode, DeployExecutionFacts, DeployExecutionInput};
#[cfg(test)]
pub use preparation::{namespace_cleanup_candidates, prepare_deploy_execution_command};
pub(crate) use preview::DeployPreviewStores;
pub use preview::preview_deploy_from_nats;
pub use step::{DeployExecutionStep, DeployFailureRecordError, DeployOperationRecordError};
use step::{deploy_step_id, with_step_timeout};

use crate::roles::machine::protocol::{
    MachineContainerRemoveRpcRequest, MachineContainerRunHookRpcRequest,
    MachineContainerRunRpcRequest, MachineContainerStopRpcRequest,
};
use ployz_core::machine::runtime::ManagedContainerIdentity;
pub use types::{
    DeployCleanupResult, DeployContainer, DeployExecutionCommand, DeployExecutionOutcome,
    DeployExecutionPorts, DeployServiceExecutionCommand, DeployTerminalEvent,
    RunContainerDisposition,
};

pub async fn execute_deploy_operation<R, N, H, C, S>(
    input: DeployExecutionInput,
    ports: DeployExecutionPorts<'_, R, N, H, C, S>,
) -> Result<DeployExecutionOutcome, DeployExecutionError>
where
    R: DeployOperationRecorder,
    N: MachineContainerRuntime + MachineImageRemovalRuntime,
    H: DeployHealthChecker,
    C: CertificateProvisioner,
    S: NamespaceStateCommitter,
{
    let DeployExecutionInput {
        operation_id,
        request,
        facts,
        registry_credentials,
        reusable_interrupted_operation_ids,
    } = input;
    let provisional_command = preparation::prepare_deploy_execution_command_with_credentials(
        operation_id.clone(),
        request.clone(),
        facts.clone(),
        &registry_credentials,
        &reusable_interrupted_operation_ids,
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
    let mut request = request;
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
        &reusable_interrupted_operation_ids,
    );
    let mut ports = ports;
    match execute_deploy_after_planning(&command, &mut ports).await {
        Ok(outcome) => Ok(outcome),
        Err(mut failure) => {
            let (cleanup, cleanup_artifacts) = cleanup_failed_phase_containers(
                &command,
                &mut *ports.machine_runtime,
                failure.failed_phase_cleanup_targets(),
            )
            .await;
            if !cleanup.is_empty()
                && let Some(error) = record_failure_evidence(
                    &command,
                    &mut *ports.recorder,
                    cleanup_evidence(&cleanup, Vec::new()),
                )
                .await
            {
                failure.add_evidence_record_error(error);
            }
            failure.add_retained_artifacts(cleanup_artifacts);
            let phase_failure_evidence =
                if let phase::DeployFailurePhase::During(phase) = failure.phase() {
                    let mut services = phase
                        .service_ids
                        .iter()
                        .map(|service_id| {
                            if phase.failed_service_id.as_ref() == Some(service_id) {
                                DeployServiceResult::Failed {
                                    service_id: service_id.clone(),
                                    failure: failure.operation_failure().clone(),
                                }
                            } else if let Some(result) = phase
                                .completed_services
                                .iter()
                                .find(|result| result.service_id() == service_id)
                            {
                                result.clone()
                            } else {
                                DeployServiceResult::Skipped {
                                    service_id: service_id.clone(),
                                }
                            }
                        })
                        .collect::<Vec<_>>();
                    services.extend(
                        phase
                            .skipped_service_ids
                            .iter()
                            .cloned()
                            .map(|service_id| DeployServiceResult::Skipped { service_id }),
                    );
                    Some(DeployEvidence::PhaseFinished {
                        phase: phase.phase,
                        outcome: DeployPhaseOutcome::Failed,
                        services,
                    })
                } else {
                    None
                };
            if let Some(evidence) = phase_failure_evidence
                && let Some(error) =
                    record_failure_evidence(&command, &mut *ports.recorder, evidence).await
            {
                failure.add_evidence_record_error(error);
            }
            fail_deploy(command, &mut *ports.recorder, failure).await
        }
    }
}

async fn execute_deploy_after_planning<R, N, H, C, S>(
    command: &DeployExecutionCommand,
    ports: &mut DeployExecutionPorts<'_, R, N, H, C, S>,
) -> Result<DeployExecutionOutcome, DeployExecutionFailure>
where
    R: DeployOperationRecorder,
    N: MachineContainerRuntime + MachineImageRemovalRuntime,
    H: DeployHealthChecker,
    C: CertificateProvisioner,
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
    validate_pushed_platforms(command, &plan).map_err(|source| run.fail(*source))?;
    let dataplane_membership = dataplane_membership(command, &plan);
    if command
        .services()
        .iter()
        .any(|service| matches!(service.service.image_source, ImageSource::PushedToSeed(_)))
    {
        record_running_stage(
            command,
            &mut *ports.recorder,
            DeployRunningStage::EnsuringImages,
        )
        .await
        .map_err(|source| run.fail(source))?;
    }
    if !plan.volume_pin_commits.is_empty() {
        commit_volume_pins(command, &plan, &mut *ports.namespace_state)
            .await
            .map_err(|source| run.fail(source))?;
    }
    if !plan.volume_ensures.is_empty() {
        record_running_stage(
            command,
            &mut *ports.recorder,
            DeployRunningStage::EnsuringVolumes,
        )
        .await
        .map_err(|source| run.fail(source))?;
        ensure_volumes(command, &plan, &mut *ports.machine_runtime)
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

    for (phase_index, phase) in plan.phases.iter().enumerate() {
        let Some(first_phase_service) = phase.services.first() else {
            return Err(run.fail(DeployExecutionError::PlanInconsistent {
                service_id: command.request.status_service_id(),
            }));
        };
        let phase_number = u16::try_from(phase_index + 1)
            .ok()
            .and_then(|phase| DeployPhaseNumber::try_new(phase).ok())
            .ok_or_else(|| {
                run.fail(DeployExecutionError::PlanInconsistent {
                    service_id: first_phase_service.service_id.clone(),
                })
            })?;
        let phase_service_ids = phase
            .services
            .iter()
            .map(|service| service.service_id.clone())
            .collect::<Vec<_>>();
        let skipped_service_ids = plan
            .phases
            .iter()
            .skip(phase_index + 1)
            .flat_map(|phase| &phase.services)
            .map(|service| service.service_id.clone())
            .collect();
        run.start_phase(phase_number, phase_service_ids.clone(), skipped_service_ids);
        record_evidence(
            command,
            &mut *ports.recorder,
            DeployEvidence::PhaseStarted {
                phase: phase_number,
                service_ids: phase_service_ids,
            },
        )
        .await
        .map_err(|source| run.fail(source))?;
        let coarse_progress = if phase_index == 0 {
            CoarsePhaseProgress::FirstPhase
        } else {
            CoarsePhaseProgress::LaterPhase
        };

        phase::start_services(
            command,
            phase,
            &dataplane_membership,
            &mut containers,
            &mut run,
            &mut *ports.recorder,
            &mut *ports.machine_runtime,
        )
        .await?;

        phase::gate_health(
            command,
            coarse_progress,
            &run,
            &mut *ports.recorder,
            &mut *ports.health_checker,
        )
        .await
        .map_err(|source| run.fail(source))?;

        phase::promote(
            command,
            phase,
            phase_number,
            coarse_progress,
            &mut run,
            phase::PromotionPorts {
                recorder: &mut *ports.recorder,
                certificate_provisioner: &mut *ports.certificate_provisioner,
                namespace_state: &mut *ports.namespace_state,
            },
        )
        .await?;
    }

    if plan.phases.is_empty() {
        record_empty_deploy_stages(command, &mut *ports.recorder)
            .await
            .map_err(|source| run.fail(source))?;
    }

    remove_undeclared_route_bindings(command, &mut *ports.namespace_state)
        .await
        .map_err(|source| run.fail(source))?;
    unpublish_omitted_serving_target_entries(command, &mut *ports.namespace_state)
        .await
        .map_err(|source| run.fail(source))?;
    if !plan.cleanup_actions.is_empty() {
        let _ = record_running_stage(
            command,
            &mut *ports.recorder,
            DeployRunningStage::RemovingSupersededContainers,
        )
        .await;
    }
    let (cleanup, image_cleanup) = images::execute_cleanup_actions(
        &command.operation_id,
        command.step_timeout(),
        &mut *ports.machine_runtime,
        &plan.cleanup_actions,
    )
    .await;
    let terminal_event = record_terminal_state(
        command,
        &mut *ports.recorder,
        &cleanup,
        image_cleanup.clone(),
        plan_has_global_deferrals(&plan),
    )
    .await;

    let outcome = DeployExecutionOutcome {
        namespace_id: plan.namespace_id,
        namespace_revision_id: plan.namespace_revision_id,
        containers,
        cleanup,
        image_cleanup,
        terminal_event,
    };

    Ok(outcome)
}

async fn record_empty_deploy_stages<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    record_running_stage(command, recorder, DeployRunningStage::WaitingForHealth).await?;
    if !command.route_binding_removals().is_empty() {
        record_running_stage(command, recorder, DeployRunningStage::RouteCutover).await?;
    }
    record_running_stage(command, recorder, DeployRunningStage::ServingTargetCommit).await
}

pub(crate) fn deploy_plan(
    command: &DeployExecutionCommand,
) -> Result<DeployPlan, DeployExecutionError> {
    let target = ployz_core::deploy::DeployPlanningTarget::try_from_deploy(&command.request)
        .map_err(|error| DeployExecutionError::InternalInvariant {
            message: error.to_string(),
        })?;
    plan_namespace_deploy(
        &target,
        command
            .services()
            .iter()
            .map(|service| service.planning_input().clone())
            .collect(),
        command.namespace_cleanup_candidates().to_vec(),
        DeployPlanningContext {
            storage_testimony: &command.storage_testimony,
        },
    )
    .map(|plan| plan.with_revision(command.request.namespace_revision_id()))
    .map_err(DeployExecutionError::from)
}

fn service_result(service: &ployz_core::deploy::DeployServicePlan) -> DeployServiceResult {
    if service.pre_start.is_none()
        && service
            .steps
            .iter()
            .all(|step| matches!(step, DeployPlanStep::UseExistingContainer { .. }))
    {
        DeployServiceResult::Unchanged {
            service_id: service.service_id.clone(),
        }
    } else {
        DeployServiceResult::Completed {
            service_id: service.service_id.clone(),
        }
    }
}

async fn run_pre_start_hook<N>(
    command: &DeployExecutionCommand,
    service: &DeployServiceExecutionCommand,
    step: &ployz_core::deploy::PreStartHookStep,
    dataplane_members: &[ployz_core::network::DataplaneMember],
    machine_runtime: &mut N,
) -> Result<(), DeployExecutionError>
where
    N: MachineContainerRuntime,
{
    let Some(pre_start) = &service.service.pre_start else {
        return Err(DeployExecutionError::PlanInconsistent {
            service_id: service.service.service_id.clone(),
        });
    };
    let step_id = StepId::try_new("pre_start").map_err(DeployExecutionError::StepId)?;
    let mut runtime = service.service.runtime.clone();
    runtime.command = Some(pre_start.command.clone());
    runtime.healthcheck = None;
    runtime.restart_policy = ContainerRestartPolicy::No;
    let identity = ManagedContainerIdentity {
        namespace_id: command.request.namespace_id.clone(),
        service_id: service.service.service_id.clone(),
        namespace_revision_entry_id: service
            .service
            .namespace_revision_entry_id(&command.request.namespace_id),
        operation_id: command.operation_id.clone(),
        step_id,
        kind: ManagedContainerKind::Predeploy,
    };
    let request = MachineContainerRunHookRpcRequest {
        pull: machine_image_pull(
            &command.request.namespace_id,
            service,
            &step.machine_id,
            command
                .target_platform(&step.machine_id)
                .map_err(|error| error.into_execution_error())?,
            dataplane_members,
        )?,
        runtime,
        provisioned_volumes: provisioned_volume_names(service),
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

async fn cleanup_failed_phase_containers<N>(
    command: &DeployExecutionCommand,
    machine_runtime: &mut N,
    containers: &[DeployContainer],
) -> (Vec<DeployCleanupResult>, Vec<RetainedArtifact>)
where
    N: MachineContainerRuntime,
{
    let mut cleanup = Vec::new();
    let mut retained = Vec::new();
    for container in containers {
        let target = DeployCleanupContainer {
            machine_id: container.machine_id.clone(),
            container_id: container.container_id.clone(),
            identity: retained_container_identity(command, container),
        };
        let stop = machine_runtime.stop_container(
            &container.machine_id,
            MachineContainerStopRpcRequest {
                operation_id: command.operation_id.clone(),
                container_id: container.container_id.clone(),
                expected_identity: target.identity.clone(),
            },
        );
        let stop_error = match tokio::time::timeout(command.step_timeout(), stop).await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(cleanup_failure_message(error)),
            Err(_) => Some(
                FailureMessage::try_new(format!(
                    "failed-phase container stop timed out after {} seconds",
                    command.step_timeout().as_secs()
                ))
                .expect("generated failed-phase stop failure message is non-empty"),
            ),
        };
        if let Some(message) = stop_error {
            retained.push(container_stop_failed_artifact(container, message.clone()));
            cleanup.push(DeployCleanupResult::Failed { target, message });
            continue;
        }
        let remove = machine_runtime.remove_container(
            &container.machine_id,
            MachineContainerRemoveRpcRequest {
                operation_id: command.operation_id.clone(),
                container_id: container.container_id.clone(),
                expected_identity: target.identity.clone(),
            },
        );
        match tokio::time::timeout(command.step_timeout(), remove).await {
            Ok(Ok(())) => cleanup.push(DeployCleanupResult::Removed(target)),
            Ok(Err(error)) => {
                let message = cleanup_failure_message(error);
                retained.push(container_stop_failed_artifact(container, message.clone()));
                cleanup.push(DeployCleanupResult::Failed { target, message });
            }
            Err(_) => {
                let message = FailureMessage::try_new(format!(
                    "failed-phase container removal timed out after {} seconds",
                    command.step_timeout().as_secs()
                ))
                .expect("generated failed-phase removal failure message is non-empty");
                retained.push(container_stop_failed_artifact(container, message.clone()));
                cleanup.push(DeployCleanupResult::Failed { target, message });
            }
        }
    }
    (cleanup, retained)
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

/// Whether deploy health gating must wait for Docker to report a health
/// status for this service's containers. Only healthchecks that make Docker
/// run a probe qualify; disabled or image-inherited healthchecks never
/// guarantee a health report, so waiting on them would hang until timeout.
fn requires_docker_healthcheck(service: &DeployServiceExecutionCommand) -> bool {
    service
        .service
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

fn cleanup_evidence(
    cleanup: &[DeployCleanupResult],
    images: Vec<DeployImageCleanup>,
) -> DeployEvidence {
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

    DeployEvidence::CleanupFinished {
        removed,
        failed,
        images,
    }
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
    images: Vec<DeployImageCleanup>,
    global_deferrals: bool,
) -> DeployTerminalEvent
where
    R: DeployOperationRecorder,
{
    let image_warning = images.iter().any(|image| {
        matches!(
            image,
            DeployImageCleanup::MissingIdentity { .. } | DeployImageCleanup::Failed { .. }
        )
    });
    let outcome = deploy_completion_outcome(cleanup, &images, global_deferrals);
    if !cleanup.is_empty() || !images.is_empty() {
        let record_cleanup =
            record_evidence(command, recorder, cleanup_evidence(cleanup, images)).await;
        if (DeployCleanupResult::has_failure(cleanup) || image_warning) && record_cleanup.is_err() {
            return DeployTerminalEvent::Missing;
        }
    }

    match record_stage(command, recorder, DeployTransition::Completed { outcome }).await {
        Ok(()) => DeployTerminalEvent::Recorded,
        Err(_) => DeployTerminalEvent::Missing,
    }
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

async fn ensure_volumes<N>(
    command: &DeployExecutionCommand,
    plan: &DeployPlan,
    machine_runtime: &mut N,
) -> Result<(), DeployExecutionError>
where
    N: MachineContainerRuntime,
{
    for volume in &plan.volume_ensures {
        with_step_timeout(
            command,
            DeployExecutionStep::EnsureVolume {
                machine_id: volume.machine_id().clone(),
                volume_name: volume.volume_name().clone(),
            },
            machine_runtime.ensure_volume(volume.machine_id(), volume),
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
    for binding in command.route_binding_removals().iter().filter(|binding| {
        !command
            .services()
            .iter()
            .any(|service| service.service.service_id == binding.service_id)
    }) {
        with_step_timeout(
            command,
            DeployExecutionStep::RemoveRoute {
                route: binding.target.clone(),
            },
            namespace_state.remove_route_binding(binding.target.clone()),
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

async fn record_failure_evidence<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    evidence: DeployEvidence,
) -> Option<DeployFailureRecordError>
where
    R: DeployOperationRecorder,
{
    match tokio::time::timeout(
        command.step_timeout(),
        recorder.record_deploy_evidence(&command.operation_id, evidence),
    )
    .await
    {
        Ok(Ok(())) => None,
        Ok(Err(source)) => Some(DeployFailureRecordError::Record(source)),
        Err(_) => Some(DeployFailureRecordError::TimedOut {
            timeout: command.step_timeout(),
        }),
    }
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
    dataplane_members: &[ployz_core::network::DataplaneMember],
) -> Result<(DeployContainer, RunContainerDisposition), DeployExecutionError>
where
    N: MachineContainerRuntime,
{
    let step_id = deploy_step_id(slot, machine_id).map_err(DeployExecutionError::StepId)?;
    let requires_docker_healthcheck = requires_docker_healthcheck(service);
    let request = MachineContainerRunRpcRequest {
        pull: machine_image_pull(
            &command.request.namespace_id,
            service,
            machine_id,
            command
                .target_platform(machine_id)
                .map_err(|error| error.into_execution_error())?,
            dataplane_members,
        )?,
        runtime: service.service.runtime.clone(),
        provisioned_volumes: provisioned_volume_names(service),
        container: ManagedContainerIdentity {
            namespace_id: command.request.namespace_id.clone(),
            service_id: service.service.service_id.clone(),
            namespace_revision_entry_id: service
                .service
                .namespace_revision_entry_id(&command.request.namespace_id),
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
                    service_id: service.service.service_id.clone(),
                    namespace_revision_entry_id: service
                        .service
                        .namespace_revision_entry_id(&command.request.namespace_id),
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

fn provisioned_volume_names(
    service: &DeployServiceExecutionCommand,
) -> Vec<ployz_core::deploy::VolumeName> {
    let mut names = service
        .planning_input
        .volume_pins
        .iter()
        .filter_map(|pin| match pin.kind() {
            ployz_core::intent::VolumeKind::Provisioned { .. } => Some(pin.volume_name().clone()),
            ployz_core::intent::VolumeKind::Plain => None,
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}
