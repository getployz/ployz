//! Read-only review of a captured candidate against fresh, observer-relative evidence.

use ployz_core::{
    ContainerId, ContainerKind, DescribeContractRequest, MachineFailure, MachineId, ProjectName,
    PruneRefusal, QualifiedService, RpcErrorCode, ServiceAttempt, ServiceName, SettingChange,
    compare_specs_detailed, op,
};
use serde::Serialize;
use thiserror::Error;

use crate::{
    compose::{CapturedCompose, ComposeSource},
    connect::{Client, ConnectError},
};

/// Failures obtaining review evidence or lowering captured attachment identities.
#[derive(Debug, Error)]
pub enum ChangesError {
    #[error(transparent)]
    Observe(#[from] ConnectError),
    #[error(transparent)]
    Volumes(#[from] ployz_core::ServiceVolumeGraphError),
    #[error(transparent)]
    Ingress(#[from] crate::dns::ExpandIngressError),
}

/// Candidate comparison against one observer-relative snapshot, including missing evidence.
#[derive(Clone, Debug, Serialize)]
pub struct ChangesReview {
    pub candidate_id: String,
    pub project_name: ProjectName,
    pub selection: Vec<ServiceAttempt>,
    pub source: ComposeSource,
    pub observer_machine_id: MachineId,
    pub observed_at: String,
    pub compared_settings: Vec<String>,
    pub services: Vec<ServiceChanges>,
    /// Observed Services absent from the complete captured target.
    pub would_remove: Vec<QualifiedService>,
    /// When present, these Services are preserved by deployment.
    pub prune_refusal: Option<PruneRefusal>,
    pub failures: Vec<MachineFailure<RpcErrorCode>>,
    pub omissions: Vec<MachineId>,
}

/// Per-container evidence for one selected Service and Machines where it was absent.
#[derive(Clone, Debug, Serialize)]
pub struct ServiceChanges {
    pub name: ServiceName,
    pub command: Vec<String>,
    pub observations: Vec<ServiceComparisonObservation>,
    /// Successful Machine queries with no observed Container for this Service.
    pub missing_on: Vec<MachineId>,
}

/// Shared setting differences and redacted environment evidence for one Container.
#[derive(Clone, Debug, Serialize)]
pub struct ServiceComparisonObservation {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub command: Vec<String>,
    pub changes: Vec<SettingChange>,
    pub environment: Vec<ployz_core::config::EnvironmentReview>,
    pub environment_failure: Option<RpcErrorCode>,
}

impl Client {
    /// Observe now and compare only the captured input. Does not prepare a Deploy.
    ///
    /// # Errors
    /// Fails if the Entry Machine cannot supply its identity or Machine view, or
    /// captured mount or ingress configuration cannot be lowered for this project.
    /// Target failures and omissions are retained in the returned review.
    pub async fn changes(
        &mut self,
        candidate: &CapturedCompose,
    ) -> Result<ChangesReview, ChangesError> {
        let observer_machine_id = self
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await?
            .machine_id;
        let intent = candidate.intent();
        let domain = if intent.target.iter().any(|spec| {
            spec.ports
                .iter()
                .any(|port| matches!(port, ployz_core::PortPublication::Ingress { .. }))
        }) {
            self.domain_if_reserved().await?
        } else {
            None
        };
        let mut target = intent.target.clone();
        for spec in &mut target {
            spec.mount_graph = spec
                .mount_graph
                .clone()
                .scope_to_project(&intent.project_name)?;
            crate::dns::expand_ingress_ports(spec, &intent.project_name, domain.as_deref())?;
        }
        let machines = self.machines().await?;
        let snapshot = self.deploy_snapshot(machines).await?;
        let would_remove =
            crate::deploy::obsolete_services(intent, &snapshot.services_in(&intent.project_name));
        let prune_refusal = intent.prune_refusal(snapshot.is_observer_complete());
        let applied = intent.applied_names();
        let mut services = Vec::new();
        for spec in target.iter().filter(|spec| applied.contains(&spec.name)) {
            let mut observations = Vec::new();
            let mut missing_on = Vec::new();
            for machine in snapshot.machines.iter().filter(|machine| {
                !snapshot.container_omissions.contains(&machine.machine.id)
                    && !snapshot
                        .container_failures
                        .iter()
                        .any(|failure| failure.machine_id == machine.machine.id)
            }) {
                let before = observations.len();
                for container in snapshot
                    .containers
                    .iter()
                    .filter(|container| container.machine_id == machine.machine.id)
                {
                    if container.kind == ContainerKind::ServiceContainer
                        && container.project_name == intent.project_name
                        && container.resolved_spec.name == spec.name
                    {
                        let inspected = self
                            .call::<op::InspectContainer>(
                                ployz_core::InspectContainerRequest {
                                    container_id: container.container_id,
                                },
                                Some(&ployz_core::MachineTarget::from(&container.machine_id)),
                            )
                            .await;
                        let (inspected, environment_failure) = match inspected {
                            Ok(details) => (Some(details), None),
                            Err(error) => (None, Some(crate::connect::rpc_error(error).code)),
                        };
                        observations.push(ServiceComparisonObservation {
                            machine_id: container.machine_id,
                            container_id: container.container_id,
                            command: container.resolved_spec.container.command.clone(),
                            environment: ployz_core::config::review_environment(
                                &spec.container.environment,
                                &container.resolved_spec.container.environment,
                                inspected
                                    .as_ref()
                                    .and_then(|details| details.environment.as_ref()),
                            ),
                            changes: compare_specs_detailed(&container.resolved_spec, spec).changes,
                            environment_failure,
                        });
                    }
                }
                if observations.len() == before {
                    missing_on.push(machine.machine.id);
                }
            }
            observations.sort_by_key(|row| (row.machine_id, row.container_id));
            missing_on.sort();
            services.push(ServiceChanges {
                name: spec.name.clone(),
                command: spec.container.command.clone(),
                observations,
                missing_on,
            });
        }
        let mut failures: Vec<_> = snapshot
            .container_failures
            .into_iter()
            .map(|failure| MachineFailure {
                machine_id: failure.machine_id,
                error: failure.error.code,
            })
            .collect();
        failures.sort_by_key(|failure| failure.machine_id);
        let mut omissions = snapshot.container_omissions;
        omissions.sort();
        Ok(ChangesReview {
            candidate_id: candidate.id().into(),
            project_name: intent.project_name.clone(),
            selection: intent.options.selected.clone(),
            source: candidate.source().clone(),
            observer_machine_id,
            observed_at: chrono::Utc::now().to_rfc3339(),
            compared_settings: ployz_core::COMPARED_SERVICE_SETTINGS
                .iter()
                .map(|setting| (*setting).into())
                .collect(),
            services,
            would_remove,
            prune_refusal,
            failures,
            omissions,
        })
    }
}
