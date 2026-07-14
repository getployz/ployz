use ployz_core::deploy::{DeployPhasePlan, DeployPlanStep, ImageSource};
use ployz_core::ids::ServiceId;
use ployz_core::ops::{
    ControlPlaneCommitScope, DeployEvidence, DeployPhaseNumber, DeployPhaseOutcome,
    DeployRunningStage, DeployServiceResult,
};

use super::failure::DeployExecutionFailure;
use super::images::ensure_images;
use super::{
    CertificateProvisioner, DeployContainer, DeployExecutionCommand, DeployExecutionError,
    DeployExecutionStep, DeployHealthCheckError, DeployHealthChecker, DeployOperationRecorder,
    DeployPhasePromotion, DeployServiceExecutionCommand, MachineContainerRuntime,
    NamespaceStateCommitter, RunContainerDisposition, deploy_step_id, record_evidence,
    record_running_stage, run_deploy_step, run_pre_start_hook, service_result, with_step_timeout,
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
        });
    }

    fn container_started(
        &mut self,
        started: DeployContainer,
        disposition: RunContainerDisposition,
    ) {
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

    fn service_completed(&mut self, result: DeployServiceResult) {
        let DeployRunPhase::During(phase) = &mut self.phase else {
            unreachable!("service work requires an active deploy phase");
        };
        phase.completed_services.push(result);
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
            .phase_created_containers
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
        self.with_phase_context(failure, Some(service.request.service_id.clone()))
    }

    fn with_phase_context(
        &self,
        failure: DeployExecutionFailure,
        failed_service_id: Option<ServiceId>,
    ) -> DeployExecutionFailure {
        let DeployRunPhase::During(phase) = &self.phase else {
            return failure.with_promoted_phases(self.promoted_phases);
        };
        failure.with_phase(
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

pub(super) async fn start_services<R, N>(
    command: &DeployExecutionCommand,
    phase: &DeployPhasePlan,
    dataplane_members: &[ployz_core::dataplane::DataplaneMember],
    containers: &mut Vec<DeployContainer>,
    run: &mut DeployRun<'_>,
    recorder: &mut R,
    machine_runtime: &mut N,
) -> Result<(), DeployExecutionFailure>
where
    R: DeployOperationRecorder,
    N: MachineContainerRuntime,
{
    if phase.services.iter().any(|planned| {
        command.services().iter().any(|service| {
            service.request.service_id == planned.service_id
                && matches!(
                    service.request.image_source,
                    ImageSource::PushedToSeed { .. }
                )
        })
    }) {
        ensure_images(command, &phase.services, recorder, machine_runtime)
            .await
            .map_err(|source| run.fail(source))?;
    }

    for service_plan in &phase.services {
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
                dataplane_members,
                machine_runtime,
            )
            .await
            .map_err(|source| run.fail_service(source, service.request.service_id.clone()))?;
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
                    step_id: deploy_step_id(*slot).map_err(|source| {
                        run.fail_service(
                            DeployExecutionError::StepId(source),
                            service.request.service_id.clone(),
                        )
                    })?,
                    requires_docker_healthcheck: false,
                }),
                DeployPlanStep::RunContainer { machine_id, slot } => {
                    let run_result = with_step_timeout(
                        command,
                        DeployExecutionStep::RunContainer {
                            machine_id: machine_id.clone(),
                        },
                        run_deploy_step(
                            machine_runtime,
                            command,
                            service,
                            machine_id,
                            *slot,
                            dataplane_members,
                        ),
                    )
                    .await;
                    let (started, disposition) = match run_result {
                        Ok(started) => started,
                        Err(source) => return Err(run.fail_run_container(service, source)),
                    };
                    containers.push(started.clone());
                    run.container_started(started.clone(), disposition);
                    record_evidence(
                        command,
                        recorder,
                        DeployEvidence::ContainerStarted {
                            machine_id: started.machine_id.clone(),
                            container_id: started.container_id.clone(),
                        },
                    )
                    .await
                    .map_err(|source| {
                        run.fail_service(source, service.request.service_id.clone())
                    })?;
                }
            }
        }
        run.service_completed(service_result(service_plan));
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
                .any(|planned| planned.service_id == service.request.service_id)
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
                    .ensure_ployz_wildcard(command.ployz_gateway_certificate_targets())
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

    let route_bindings = phase_services
        .iter()
        .flat_map(|service| service.route_binding_states().iter().cloned())
        .collect();
    let route_binding_removals = command
        .route_binding_removals()
        .iter()
        .filter(|binding| {
            phase_services
                .iter()
                .any(|service| service.request.service_id == binding.service_id)
        })
        .map(|binding| binding.target.clone())
        .collect();
    if command
        .services()
        .iter()
        .any(|service| !service.route_binding_states().is_empty())
    {
        coarse_progress
            .record(command, ports.recorder, DeployRunningStage::RouteCutover)
            .await
            .map_err(|source| run.fail(source))?;
    }
    coarse_progress
        .record(
            command,
            ports.recorder,
            DeployRunningStage::ServingTargetCommit,
        )
        .await
        .map_err(|source| run.fail(source))?;
    let [first_service, remaining_services @ ..] = phase_services.as_slice() else {
        return Err(run.fail(DeployExecutionError::PlanInconsistent {
            service_id: command.request.status_service_id(),
        }));
    };
    let scope = ControlPlaneCommitScope::DeployPhase {
        namespace_revision_id: command.request.namespace_revision_id(),
        phase: phase_number,
    };
    let promotion = DeployPhasePromotion {
        scope: scope.clone(),
        route_bindings,
        route_binding_removals,
        first_serving_target_entry: first_service.serving_target_entry_state(),
        remaining_serving_target_entries: remaining_services
            .iter()
            .map(|service| service.serving_target_entry_state())
            .collect(),
    };
    with_step_timeout(
        command,
        DeployExecutionStep::CommitServingTarget { scope },
        ports.namespace_state.commit_deploy_phase(promotion),
    )
    .await
    .map_err(|source| run.fail(source))?;
    run.phase_promoted();

    record_evidence(
        command,
        ports.recorder,
        DeployEvidence::PhaseFinished {
            phase: phase_number,
            outcome: DeployPhaseOutcome::Promoted,
            services: phase.services.iter().map(service_result).collect(),
        },
    )
    .await
    .map_err(|source| run.fail(source))
}
