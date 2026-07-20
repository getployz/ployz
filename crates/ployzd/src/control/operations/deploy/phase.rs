use ployz_core::deploy::{
    DeployCleanupContainer, DeployPhasePlan, DeployPlanStep, DeployPlanStepRef, DeployServiceWork,
    ExistingReplicaCreationGate, ImageSource,
};
use ployz_core::ids::{ContainerId, MachineId, ServiceId, StepId};
use ployz_core::machine::runtime::{ManagedContainerIdentity, ManagedContainerKind};
use ployz_core::operation::{
    ControlPlaneCommitScope, DeployEvidence, DeployPhaseNumber, DeployPhaseOutcome,
    DeployRunningStage, DeployServiceResult,
};

use super::failure::DeployExecutionFailure;
use super::images::ensure_images;
use super::volume_handoff::{self, VolumeHandoffRollbackState};
use super::{
    CertificateProvisioner, DeployContainer, DeployExecutionCommand, DeployExecutionError,
    DeployExecutionStep, DeployHealthCheckError, DeployHealthChecker, DeployOperationRecorder,
    DeployPhasePromotion, DeployServiceExecutionCommand, MachineContainerRuntime,
    MachineContainerRuntimeError, NamespaceStateCommitter, PreStartHookRuntimeError,
    RunContainerDisposition, deploy_step_id, record_evidence, record_running_stage,
    requires_docker_healthcheck, run_deploy_step, run_pre_start_hook, service_result,
    with_step_timeout,
};

#[derive(Debug, Clone)]
pub(super) enum DeployFailurePhase {
    OutsidePhase,
    During(DeployFailedPhase),
}

#[derive(Debug, Clone)]
pub(super) struct DeployFailedPhase {
    pub phase: DeployPhaseNumber,
    pub service_ids: Vec<ServiceId>,
    pub completed_services: Vec<DeployServiceResult>,
    pub skipped_service_ids: Vec<ServiceId>,
    pub failed_service_id: Option<ServiceId>,
}

pub(super) struct DeployRun<'a> {
    command: &'a DeployExecutionCommand,
    phase: DeployRunPhase,
    promoted_phases: u16,
}

enum DeployRunPhase {
    OutsidePhase,
    During(ActiveDeployPhase),
}

struct ActiveDeployPhase {
    phase_created_containers: Vec<DeployContainer>,
    health_check_containers: Vec<DeployContainer>,
    phase: DeployPhaseNumber,
    phase_service_ids: Vec<ServiceId>,
    completed_services: Vec<DeployServiceResult>,
    skipped_service_ids: Vec<ServiceId>,
    volume_handoff: VolumeHandoffRollbackState,
}

#[derive(Clone, Copy)]
pub(super) enum CoarsePhaseProgress {
    FirstPhase,
    LaterPhase,
}

pub(super) struct PromotionPorts<'a, R, C, S> {
    pub recorder: &'a mut R,
    pub certificate_provisioner: &'a mut C,
    pub namespace_state: &'a mut S,
}

pub(super) struct ServiceStartPorts<'a, R, N> {
    pub containers: &'a mut Vec<DeployContainer>,
    pub recorder: &'a mut R,
    pub machine_runtime: &'a mut N,
}

impl CoarsePhaseProgress {
    async fn record<R>(
        self,
        command: &DeployExecutionCommand,
        recorder: &mut R,
        stage: DeployRunningStage,
    ) -> Result<(), DeployExecutionError>
    where
        R: DeployOperationRecorder,
    {
        match self {
            Self::FirstPhase => record_running_stage(command, recorder, stage).await,
            Self::LaterPhase => Ok(()),
        }
    }
}

impl<'a> DeployRun<'a> {
    pub(super) fn new(command: &'a DeployExecutionCommand) -> Self {
        Self {
            command,
            phase: DeployRunPhase::OutsidePhase,
            promoted_phases: 0,
        }
    }

    pub(super) fn start_phase(
        &mut self,
        phase: DeployPhaseNumber,
        phase_service_ids: Vec<ServiceId>,
        skipped_service_ids: Vec<ServiceId>,
    ) {
        self.phase = DeployRunPhase::During(ActiveDeployPhase {
            phase_created_containers: Vec::new(),
            health_check_containers: Vec::new(),
            phase,
            phase_service_ids,
            completed_services: Vec::new(),
            skipped_service_ids,
            volume_handoff: VolumeHandoffRollbackState::default(),
        });
    }

    fn container_started(
        &mut self,
        started: DeployContainer,
        disposition: RunContainerDisposition,
        volume_handoff: bool,
    ) {
        if volume_handoff {
            let target = DeployCleanupContainer {
                machine_id: started.machine_id.clone(),
                container_id: started.container_id.clone(),
                identity: super::retained_container_identity(self.command, &started),
            };
            let DeployRunPhase::During(phase) = &mut self.phase else {
                unreachable!("container work requires an active deploy phase");
            };
            phase
                .volume_handoff
                .consumer_started(&started.service_id, target);
        }
        match disposition {
            RunContainerDisposition::Created => {
                let DeployRunPhase::During(phase) = &mut self.phase else {
                    unreachable!("container work requires an active deploy phase");
                };
                phase.health_check_containers.push(started.clone());
                phase.phase_created_containers.push(started);
            }
            RunContainerDisposition::Reused => {}
        }
    }

    fn volume_handoff_mut(&mut self) -> &mut VolumeHandoffRollbackState {
        let DeployRunPhase::During(phase) = &mut self.phase else {
            unreachable!("volume handoff requires an active deploy phase");
        };
        &mut phase.volume_handoff
    }

    fn existing_container(
        &mut self,
        container: DeployContainer,
        creation_gate: ExistingReplicaCreationGate,
    ) {
        if creation_gate == ExistingReplicaCreationGate::RequiredAfterInterruption {
            let DeployRunPhase::During(phase) = &mut self.phase else {
                unreachable!("container work requires an active deploy phase");
            };
            phase.health_check_containers.push(container);
        }
    }

    fn service_completed(&mut self, result: DeployServiceResult) {
        let DeployRunPhase::During(phase) = &mut self.phase else {
            unreachable!("service work requires an active deploy phase");
        };
        phase.completed_services.push(result);
    }

    fn completed_services(&self) -> &[DeployServiceResult] {
        match &self.phase {
            DeployRunPhase::OutsidePhase => &[],
            DeployRunPhase::During(phase) => &phase.completed_services,
        }
    }

    fn health_check_containers(&self) -> &[DeployContainer] {
        match &self.phase {
            DeployRunPhase::OutsidePhase => &[],
            DeployRunPhase::During(phase) => &phase.health_check_containers,
        }
    }

    fn phase_promoted(&mut self) {
        self.promoted_phases += 1;
        self.phase = DeployRunPhase::OutsidePhase;
    }

    fn failed_container(&self, source: &DeployExecutionError) -> Option<&DeployContainer> {
        let DeployExecutionError::WaitHealthy(DeployHealthCheckError::Unhealthy {
            container_id,
            ..
        }) = source
        else {
            return None;
        };
        let DeployRunPhase::During(phase) = &self.phase else {
            return None;
        };
        phase
            .health_check_containers
            .iter()
            .find(|container| container.container_id == *container_id)
    }

    pub(super) fn fail(&self, source: DeployExecutionError) -> DeployExecutionFailure {
        self.fail_with_service(source, None)
    }

    fn fail_service(
        &self,
        source: DeployExecutionError,
        failed_service_id: ServiceId,
    ) -> DeployExecutionFailure {
        self.fail_with_service(source, Some(failed_service_id))
    }

    fn fail_with_service(
        &self,
        source: DeployExecutionError,
        failed_service_id: Option<ServiceId>,
    ) -> DeployExecutionFailure {
        let failed = self
            .failed_container(&source)
            .cloned()
            .into_iter()
            .collect::<Vec<_>>();
        let cleanup = match &self.phase {
            DeployRunPhase::OutsidePhase => Vec::new(),
            DeployRunPhase::During(phase) => phase
                .phase_created_containers
                .iter()
                .filter(|container| !failed.contains(container))
                .cloned()
                .collect(),
        };
        let failed_service_id = failed
            .first()
            .map(|container| container.service_id.clone())
            .or(failed_service_id);
        let failure =
            DeployExecutionFailure::with_stop_targets(self.command, source, &failed, &cleanup);
        self.with_phase_context(failure, failed_service_id)
    }

    fn fail_run_container(
        &self,
        service: &DeployServiceExecutionCommand,
        source: DeployExecutionError,
    ) -> DeployExecutionFailure {
        let cleanup = match &self.phase {
            DeployRunPhase::OutsidePhase => &[][..],
            DeployRunPhase::During(phase) => phase.phase_created_containers.as_slice(),
        };
        let failure = DeployExecutionFailure::with_stop_targets(self.command, source, &[], cleanup);
        self.with_phase_context(failure, Some(service.service.service_id.clone()))
    }

    fn with_phase_context(
        &self,
        failure: DeployExecutionFailure,
        failed_service_id: Option<ServiceId>,
    ) -> DeployExecutionFailure {
        let DeployRunPhase::During(phase) = &self.phase else {
            return failure.with_promoted_phases(self.promoted_phases);
        };
        failure
            .with_volume_handoff_rollback(phase.volume_handoff.clone())
            .with_phase(
                DeployFailedPhase {
                    phase: phase.phase,
                    service_ids: phase.phase_service_ids.clone(),
                    completed_services: phase.completed_services.clone(),
                    skipped_service_ids: phase.skipped_service_ids.clone(),
                    failed_service_id,
                },
                self.promoted_phases,
            )
    }
}

fn consumer_target(
    identity: ManagedContainerIdentity,
    machine_id: MachineId,
    container_id: ContainerId,
) -> DeployCleanupContainer {
    DeployCleanupContainer {
        machine_id,
        container_id,
        identity,
    }
}

fn consumer_identity(
    command: &DeployExecutionCommand,
    service: &DeployServiceExecutionCommand,
    step_id: StepId,
    kind: ManagedContainerKind,
) -> ManagedContainerIdentity {
    ManagedContainerIdentity {
        namespace_id: command.request.namespace_id.clone(),
        service_id: service.service.service_id.clone(),
        namespace_revision_entry_id: service.namespace_revision_entry_id(
            &command.request.namespace_id,
            &command.environment_revision_key,
        ),
        operation_id: command.operation_id().clone(),
        step_id,
        kind,
    }
}

fn capture_run_failure_consumers(
    service: &DeployServiceExecutionCommand,
    run: &mut DeployRun<'_>,
    expected_identity: &ManagedContainerIdentity,
    source: &DeployExecutionError,
) {
    if let DeployExecutionError::RunContainer(
        MachineContainerRuntimeError::OperationStepAmbiguous {
            machine_id,
            operation_id: _,
            step_id: _,
            container_ids,
        },
    ) = source
        && !container_ids.is_empty()
    {
        let targets = container_ids.iter().map(|container_id| {
            consumer_target(
                expected_identity.clone(),
                machine_id.clone(),
                container_id.clone(),
            )
        });
        run.volume_handoff_mut().ambiguous_consumers(
            &service.service.service_id,
            expected_identity,
            targets,
        );
        return;
    }
    if !matches!(
        source,
        DeployExecutionError::RunContainer(MachineContainerRuntimeError::Unavailable { .. })
            | DeployExecutionError::RunContainer(
                MachineContainerRuntimeError::OperationStepAmbiguous { .. },
            )
            | DeployExecutionError::StepTimedOut {
                step: DeployExecutionStep::RunContainer { .. },
                ..
            }
    ) {
        run.volume_handoff_mut()
            .consumer_did_not_start(&service.service.service_id, expected_identity);
    }
}

fn capture_hook_failure_consumers(
    service: &DeployServiceExecutionCommand,
    run: &mut DeployRun<'_>,
    expected_identity: &ManagedContainerIdentity,
    source: &DeployExecutionError,
) {
    if let DeployExecutionError::PreStartHook(PreStartHookRuntimeError::OperationStepAmbiguous {
        machine_id,
        container_ids,
        ..
    }) = source
        && !container_ids.is_empty()
    {
        let targets = container_ids.iter().map(|container_id| {
            consumer_target(
                expected_identity.clone(),
                machine_id.clone(),
                container_id.clone(),
            )
        });
        run.volume_handoff_mut().ambiguous_consumers(
            &service.service.service_id,
            expected_identity,
            targets,
        );
        return;
    }
    if let DeployExecutionError::PreStartHook(
        PreStartHookRuntimeError::WaitFailed {
            machine_id,
            container_id,
            ..
        }
        | PreStartHookRuntimeError::TimedOut {
            machine_id,
            container_id,
            ..
        },
    ) = source
    {
        run.volume_handoff_mut().ambiguous_consumers(
            &service.service.service_id,
            expected_identity,
            [consumer_target(
                expected_identity.clone(),
                machine_id.clone(),
                container_id.clone(),
            )],
        );
        return;
    }
    if !matches!(
        source,
        DeployExecutionError::PreStartHook(PreStartHookRuntimeError::Unavailable { .. })
            | DeployExecutionError::PreStartHook(
                PreStartHookRuntimeError::OperationStepAmbiguous { .. }
            )
            | DeployExecutionError::StepTimedOut {
                step: DeployExecutionStep::RunPreStartHook { .. },
                ..
            }
    ) {
        run.volume_handoff_mut()
            .consumer_did_not_start(&service.service.service_id, expected_identity);
    }
}

pub(super) async fn start_services<R, N>(
    command: &DeployExecutionCommand,
    phase: &DeployPhasePlan,
    dataplane_members: &[ployz_core::network::DataplaneMember],
    services_with_cleanup: &std::collections::BTreeSet<ServiceId>,
    run: &mut DeployRun<'_>,
    ports: ServiceStartPorts<'_, R, N>,
) -> Result<(), DeployExecutionFailure>
where
    R: DeployOperationRecorder,
    N: MachineContainerRuntime,
{
    if phase.services.iter().any(|planned| {
        command.services().iter().any(|service| {
            service.service.service_id == planned.service_id
                && matches!(service.service.image_source, ImageSource::PushedToSeed(_))
        })
    }) {
        ensure_images(
            command,
            &phase.services,
            ports.recorder,
            ports.machine_runtime,
        )
        .await
        .map_err(|source| run.fail(source))?;
    }

    for service_plan in &phase.services {
        let Some(service) = command
            .services()
            .iter()
            .find(|service| service.service.service_id == service_plan.service_id)
        else {
            return Err(run.fail(DeployExecutionError::PlanInconsistent {
                service_id: service_plan.service_id.clone(),
            }));
        };
        let is_volume_handoff = match &service_plan.work {
            DeployServiceWork::Ordinary { steps: _ } => false,
            DeployServiceWork::VolumeHandoff {
                replacement,
                remaining_steps: _,
                participants,
            } => {
                let result = volume_handoff::stop_volume_owners(
                    command,
                    &service.service.service_id,
                    &replacement.machine_id,
                    participants,
                    run.volume_handoff_mut(),
                    ports.recorder,
                    ports.machine_runtime,
                )
                .await;
                result.map_err(|source| {
                    run.fail_service(source, service.service.service_id.clone())
                })?;
                true
            }
        };
        if let Some(pre_start) = &service_plan.pre_start {
            let expected_identity = if is_volume_handoff {
                let step_id = StepId::try_new("pre_start").map_err(|source| {
                    run.fail_service(
                        DeployExecutionError::StepId(source),
                        service.service.service_id.clone(),
                    )
                })?;
                let identity =
                    consumer_identity(command, service, step_id, ManagedContainerKind::Predeploy);
                run.volume_handoff_mut().begin_consumer_start(
                    &service.service.service_id,
                    pre_start.machine_id.clone(),
                    identity.clone(),
                );
                Some(identity)
            } else {
                None
            };
            let result = run_pre_start_hook(
                command,
                service,
                pre_start,
                dataplane_members,
                ports.machine_runtime,
            )
            .await;
            if let Err(source) = result {
                if let Some(expected_identity) = &expected_identity {
                    capture_hook_failure_consumers(service, run, expected_identity, &source);
                }
                return Err(run.fail_service(source, service.service.service_id.clone()));
            }
            if let Some(expected_identity) = &expected_identity {
                run.volume_handoff_mut()
                    .consumer_did_not_start(&service.service.service_id, expected_identity);
            }
        }
        for step in service_plan.work.steps() {
            let (machine_id, slot) = match step {
                DeployPlanStepRef::Step(DeployPlanStep::UseExistingContainer {
                    machine_id,
                    container_id,
                    slot,
                }) => {
                    let Some(existing) =
                        service
                            .planning_input
                            .existing_replicas
                            .iter()
                            .find(|existing| {
                                existing.machine_id == *machine_id
                                    && existing.container_id == *container_id
                            })
                    else {
                        return Err(run.fail_service(
                            DeployExecutionError::PlanInconsistent {
                                service_id: service.service.service_id.clone(),
                            },
                            service.service.service_id.clone(),
                        ));
                    };
                    let container = DeployContainer {
                        service_id: service.service.service_id.clone(),
                        namespace_revision_entry_id: service.namespace_revision_entry_id(
                            &command.request.namespace_id,
                            &command.environment_revision_key,
                        ),
                        machine_id: machine_id.clone(),
                        container_id: container_id.clone(),
                        step_id: deploy_step_id(*slot, machine_id).map_err(|source| {
                            run.fail_service(
                                DeployExecutionError::StepId(source),
                                service.service.service_id.clone(),
                            )
                        })?,
                        requires_docker_healthcheck: existing.creation_gate
                            == ExistingReplicaCreationGate::RequiredAfterInterruption
                            && requires_docker_healthcheck(service),
                    };
                    ports.containers.push(container.clone());
                    run.existing_container(container, existing.creation_gate);
                    continue;
                }
                DeployPlanStepRef::RunContainer(replacement) => {
                    (&replacement.machine_id, replacement.slot)
                }
                DeployPlanStepRef::Step(DeployPlanStep::RunContainer { machine_id, slot }) => {
                    (machine_id, *slot)
                }
            };
            let expected_identity = if is_volume_handoff {
                let step_id = deploy_step_id(slot, machine_id).map_err(|source| {
                    run.fail_service(
                        DeployExecutionError::StepId(source),
                        service.service.service_id.clone(),
                    )
                })?;
                let identity =
                    consumer_identity(command, service, step_id, ManagedContainerKind::Service);
                run.volume_handoff_mut().begin_consumer_start(
                    &service.service.service_id,
                    machine_id.clone(),
                    identity.clone(),
                );
                Some(identity)
            } else {
                None
            };
            let run_result = with_step_timeout(
                command,
                DeployExecutionStep::RunContainer {
                    machine_id: machine_id.clone(),
                },
                run_deploy_step(
                    ports.machine_runtime,
                    command,
                    service,
                    machine_id,
                    slot,
                    dataplane_members,
                ),
            )
            .await;
            let (started, disposition) = match run_result {
                Ok(started) => started,
                Err(source) => {
                    if let Some(expected_identity) = &expected_identity {
                        capture_run_failure_consumers(service, run, expected_identity, &source);
                    }
                    return Err(run.fail_run_container(service, source));
                }
            };
            ports.containers.push(started.clone());
            run.container_started(started.clone(), disposition, is_volume_handoff);
            record_evidence(
                command,
                ports.recorder,
                DeployEvidence::ContainerStarted {
                    machine_id: started.machine_id.clone(),
                    container_id: started.container_id.clone(),
                },
            )
            .await
            .map_err(|source| run.fail_service(source, service.service.service_id.clone()))?;
        }
        run.service_completed(service_result(
            service,
            service_plan,
            services_with_cleanup.contains(&service_plan.service_id),
        ));
    }

    Ok(())
}

pub(super) async fn gate_health<R, H>(
    command: &DeployExecutionCommand,
    coarse_progress: CoarsePhaseProgress,
    run: &DeployRun<'_>,
    recorder: &mut R,
    health_checker: &mut H,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
    H: DeployHealthChecker,
{
    coarse_progress
        .record(command, recorder, DeployRunningStage::WaitingForHealth)
        .await?;
    if run.health_check_containers().is_empty() {
        return Ok(());
    }
    record_evidence(command, recorder, DeployEvidence::HealthCheckStarted).await?;
    with_step_timeout(
        command,
        DeployExecutionStep::WaitHealthy,
        health_checker.wait_healthy(run.health_check_containers()),
    )
    .await
}

pub(super) async fn promote<R, C, S>(
    command: &DeployExecutionCommand,
    phase: &DeployPhasePlan,
    phase_number: DeployPhaseNumber,
    coarse_progress: CoarsePhaseProgress,
    run: &mut DeployRun<'_>,
    ports: PromotionPorts<'_, R, C, S>,
) -> Result<(), DeployExecutionFailure>
where
    R: DeployOperationRecorder,
    C: CertificateProvisioner,
    S: NamespaceStateCommitter,
{
    let phase_services = command
        .services()
        .iter()
        .filter(|service| {
            phase
                .services
                .iter()
                .any(|planned| planned.service_id == service.service.service_id)
        })
        .collect::<Vec<_>>();
    let ployz_automatic_route = command
        .ployz_automatic_hostnames()
        .then_some(())
        .and_then(|()| {
            phase_services
                .iter()
                .flat_map(|service| service.route_binding_states())
                .find(|binding| {
                    binding.origin == ployz_core::ingress::RouteBindingOrigin::Automatic
                })
        });
    if !command.exact_certificate_routes().is_empty() || ployz_automatic_route.is_some() {
        coarse_progress
            .record(
                command,
                ports.recorder,
                DeployRunningStage::EnsuringCertificates,
            )
            .await
            .map_err(|source| run.fail(source))?;
    }
    if let Some(binding) = ployz_automatic_route {
        let hostname = &binding.target.hostname;
        with_step_timeout(
            command,
            DeployExecutionStep::EnsureCertificate {
                hostname: hostname.clone(),
            },
            async {
                ports
                    .certificate_provisioner
                    .ensure_ployz_wildcard(
                        command.operation_id(),
                        command.ployz_gateway_certificate_targets(),
                    )
                    .await
                    .map(|_| ())
                    .map_err(|failure| DeployExecutionError::ProvisionCertificate {
                        hostname: hostname.clone(),
                        failure: Box::new(failure),
                    })
            },
        )
        .await
        .map_err(|source| run.fail(source))?;
    }
    for binding in command.exact_certificate_routes().iter().filter(|binding| {
        phase_services.iter().any(|service| {
            service
                .route_binding_states()
                .iter()
                .any(|route| route.id == binding.id)
        })
    }) {
        let hostname = &binding.target.hostname;
        with_step_timeout(
            command,
            DeployExecutionStep::EnsureCertificate {
                hostname: hostname.clone(),
            },
            async {
                ports
                    .certificate_provisioner
                    .ensure(
                        command.operation_id(),
                        ployz_core::ingress::CertificateOwner::RouteBinding {
                            route_binding_id: binding.id.clone(),
                        },
                        hostname,
                        command.gateway_certificate_targets(),
                    )
                    .await
                    .map(|_| ())
                    .map_err(|failure| DeployExecutionError::ProvisionCertificate {
                        hostname: hostname.clone(),
                        failure: Box::new(failure),
                    })
            },
        )
        .await
        .map_err(|source| run.fail(source))?;
    }

    let route_bindings: Vec<_> = phase_services
        .iter()
        .flat_map(|service| service.route_binding_states().iter().cloned())
        .collect();
    let route_binding_removals: Vec<_> = command
        .route_binding_removals()
        .iter()
        .filter(|binding| {
            phase_services
                .iter()
                .any(|service| service.service.service_id == binding.service_id)
        })
        .map(|binding| binding.target.clone())
        .collect();
    let phase_changes_routes = !route_bindings.is_empty() || !route_binding_removals.is_empty();
    let [first_service, remaining_services @ ..] = phase_services.as_slice() else {
        return Err(run.fail(DeployExecutionError::PlanInconsistent {
            service_id: command.request.status_service_id(),
        }));
    };
    let scope = ControlPlaneCommitScope::DeployPhase {
        namespace_revision_id: command.namespace_revision_id(),
        phase: phase_number,
    };
    let promotion = DeployPhasePromotion {
        scope: scope.clone(),
        route_bindings,
        route_binding_removals,
        first_serving_target_entry: first_service.serving_target_entry_state(
            &command.request.namespace_id,
            &command.environment_revision_key,
        ),
        remaining_serving_target_entries: remaining_services
            .iter()
            .map(|service| {
                service.serving_target_entry_state(
                    &command.request.namespace_id,
                    &command.environment_revision_key,
                )
            })
            .collect(),
    };
    if phase_changes_routes {
        record_running_stage(command, ports.recorder, DeployRunningStage::RouteCutover)
            .await
            .map_err(|source| run.fail(source))?;
        record_running_stage(
            command,
            ports.recorder,
            DeployRunningStage::ServingTargetCommit,
        )
        .await
        .map_err(|source| run.fail(source))?;
    } else {
        coarse_progress
            .record(
                command,
                ports.recorder,
                DeployRunningStage::ServingTargetCommit,
            )
            .await
            .map_err(|source| run.fail(source))?;
    }
    ports
        .namespace_state
        .commit_deploy_phase(promotion)
        .await
        .map_err(DeployExecutionError::from)
        .map_err(|source| run.fail(source))?;
    let completed_services = run.completed_services().to_vec();
    run.phase_promoted();

    record_evidence(
        command,
        ports.recorder,
        DeployEvidence::PhaseFinished {
            phase: phase_number,
            outcome: DeployPhaseOutcome::Promoted,
            services: completed_services,
        },
    )
    .await
    .map_err(|source| run.fail(source))
}
