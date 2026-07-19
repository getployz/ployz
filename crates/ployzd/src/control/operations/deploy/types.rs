use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlanningInput, DeployPlanningTarget, DeployRequest,
    DeployRouteBindingAddition, DeployServiceSpec, RegistryCredential,
};
#[cfg(test)]
use ployz_core::deploy::{
    DeployPlanningPlacementInput, ExistingServiceReplica, ObservedCleanupCandidate,
};
use ployz_core::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, NamespaceRevisionId,
    OperationId, ServiceId, StepId,
};
use ployz_core::image::OciPlatform;
use ployz_core::intent::RouteBindingState;
use ployz_core::intent::ServingTargetEntry;
use ployz_core::network::DataplaneMember;
use ployz_core::operation::{
    DeployCompletionOutcome, DeployImageCleanup, FailureMessage, OperatorHint, RetainedArtifact,
};

use std::collections::BTreeMap;
use std::time::Duration;

use crate::certificate::GatewayCertificateTarget;
use crate::control::role_client::machine::MachineClockTestimony;

const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeployPreviewPlanningCommand {
    pub(super) target: DeployPlanningTarget,
    pub(super) planning_inputs: Vec<DeployPlanningInput>,
    pub(super) route_binding_additions: Vec<DeployRouteBindingAddition>,
    pub(super) route_binding_removals: Vec<RouteBindingState>,
    pub(super) serving_target_commits: Vec<ServingTargetEntry>,
    pub(super) serving_target_removals: Vec<ServingTargetEntry>,
    pub(super) namespace_cleanup_candidates: Vec<DeployCleanupContainer>,
    pub(super) storage_testimony:
        BTreeMap<MachineId, Option<ployz_core::machine::StorageCapability>>,
    pub(super) machine_platforms: BTreeMap<MachineId, OciPlatform>,
    pub(super) seed_clock_testimony: BTreeMap<MachineId, MachineClockTestimony>,
    pub(super) unusable_machines: Vec<ployz_core::operation::UnusableMachine>,
    pub(super) unusable_machines_by_service:
        BTreeMap<ServiceId, Vec<ployz_core::operation::UnusableMachine>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionCommand {
    pub(super) operation_id: OperationId,
    pub(super) request: DeployRequest,
    pub(super) services: Vec<DeployServiceExecutionCommand>,
    pub(super) route_binding_removals: Vec<RouteBindingState>,
    pub(super) serving_target_removals: Vec<ServingTargetEntry>,
    pub(super) namespace_cleanup_candidates: Vec<DeployCleanupContainer>,
    pub(super) storage_testimony:
        BTreeMap<MachineId, Option<ployz_core::machine::StorageCapability>>,
    pub(super) machine_platforms: BTreeMap<MachineId, OciPlatform>,
    pub(super) seed_clock_testimony: BTreeMap<MachineId, MachineClockTestimony>,
    pub(super) dataplane_members: Vec<DataplaneMember>,
    pub(super) exact_certificate_routes: Vec<RouteBindingState>,
    pub(super) ployz_automatic_hostnames: bool,
    pub(super) gateway_certificate_targets: Vec<GatewayCertificateTarget>,
    pub(super) ployz_gateway_certificate_targets: Vec<GatewayCertificateTarget>,
    pub(super) unusable_machines: Vec<ployz_core::operation::UnusableMachine>,
    pub(super) step_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployServiceExecutionCommand {
    pub(super) service: DeployServiceSpec,
    pub(super) registry_credential: Option<RegistryCredential>,
    pub(super) route_commits: Vec<RouteBindingState>,
    pub(super) planning_input: DeployPlanningInput,
    pub(super) serving_intent: ServingIntentDisposition,
    pub(super) unusable_machines: Vec<ployz_core::operation::UnusableMachine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ServingIntentDisposition {
    Unchanged,
    Changed,
}

impl DeployExecutionCommand {
    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }

    #[must_use]
    pub fn namespace_revision_id(&self) -> NamespaceRevisionId {
        self.request
            .namespace_revision_id_for_operation(&self.operation_id)
    }

    #[must_use]
    pub fn services(&self) -> &[DeployServiceExecutionCommand] {
        &self.services
    }

    #[must_use]
    pub fn service(&self, service_id: &ServiceId) -> Option<&DeployServiceExecutionCommand> {
        self.services
            .iter()
            .find(|service| &service.service.service_id == service_id)
    }

    #[must_use]
    pub fn unusable_machines_for_service(
        &self,
        service_id: &ServiceId,
    ) -> Option<Vec<ployz_core::operation::UnusableMachine>> {
        let service = self.service(service_id)?;
        Some(merged_unusable_machines(
            &self.unusable_machines,
            service.unusable_machines.clone(),
        ))
    }

    #[must_use]
    pub fn namespace_cleanup_candidates(&self) -> &[DeployCleanupContainer] {
        &self.namespace_cleanup_candidates
    }

    #[must_use]
    pub fn route_binding_removals(&self) -> &[RouteBindingState] {
        &self.route_binding_removals
    }

    #[must_use]
    pub fn serving_target_removals(&self) -> &[ServingTargetEntry] {
        &self.serving_target_removals
    }

    #[must_use]
    #[cfg(test)]
    pub fn unusable_machines(&self) -> &[ployz_core::operation::UnusableMachine] {
        &self.unusable_machines
    }

    pub(super) fn target_platform(
        &self,
        machine_id: &MachineId,
    ) -> Result<&OciPlatform, MissingTargetPlatform> {
        let Some(platform) = self.machine_platforms.get(machine_id) else {
            return Err(MissingTargetPlatform {
                machine_id: machine_id.clone(),
            });
        };
        Ok(platform)
    }

    #[must_use]
    #[cfg(test)]
    pub fn dataplane_machines(&self) -> Vec<MachineId> {
        self.dataplane_members
            .iter()
            .map(|member| member.machine_id.clone())
            .collect()
    }

    #[must_use]
    pub fn exact_certificate_routes(&self) -> &[RouteBindingState] {
        &self.exact_certificate_routes
    }

    #[must_use]
    pub const fn ployz_automatic_hostnames(&self) -> bool {
        self.ployz_automatic_hostnames
    }

    #[must_use]
    pub fn gateway_certificate_targets(&self) -> &[GatewayCertificateTarget] {
        &self.gateway_certificate_targets
    }

    #[must_use]
    pub fn ployz_gateway_certificate_targets(&self) -> &[GatewayCertificateTarget] {
        &self.ployz_gateway_certificate_targets
    }

    #[must_use]
    pub fn step_timeout(&self) -> Duration {
        if self.step_timeout.is_zero() {
            DEFAULT_STEP_TIMEOUT
        } else {
            self.step_timeout
        }
    }
}

pub(super) fn merged_unusable_machines(
    shared: &[ployz_core::operation::UnusableMachine],
    service: Vec<ployz_core::operation::UnusableMachine>,
) -> Vec<ployz_core::operation::UnusableMachine> {
    let mut merged = shared.to_vec();
    for unusable in service {
        if !merged.contains(&unusable) {
            merged.push(unusable);
        }
    }
    merged.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
    merged
}

pub(super) struct MissingTargetPlatform {
    machine_id: MachineId,
}

impl MissingTargetPlatform {
    pub(super) fn into_execution_error(self) -> super::DeployExecutionError {
        super::DeployExecutionError::InternalInvariant {
            message: format!(
                "placed target machine {} has no answered platform facts",
                self.machine_id.as_str()
            ),
        }
    }
}

impl DeployServiceExecutionCommand {
    #[must_use]
    #[cfg(test)]
    pub fn unusable_machines(&self) -> &[ployz_core::operation::UnusableMachine] {
        &self.unusable_machines
    }

    #[must_use]
    pub fn registry_credential(&self) -> Option<&RegistryCredential> {
        self.registry_credential.as_ref()
    }

    #[must_use]
    #[cfg(test)]
    pub fn existing_replicas(&self) -> &[ExistingServiceReplica] {
        &self.planning_input.existing_replicas
    }

    #[must_use]
    #[cfg(test)]
    pub fn cleanup_candidates(&self) -> &[ObservedCleanupCandidate] {
        &self.planning_input.cleanup_candidates
    }

    #[must_use]
    #[cfg(test)]
    pub fn eligible_machines(&self) -> Vec<MachineId> {
        match &self.planning_input.placement {
            DeployPlanningPlacementInput::Replicated { eligible_machines } => {
                eligible_machines.clone()
            }
            DeployPlanningPlacementInput::Global(input) => input.selected_machines(),
        }
    }

    #[must_use]
    pub(super) fn planning_input(&self) -> &DeployPlanningInput {
        &self.planning_input
    }

    #[must_use]
    pub fn serving_target_entry_state(
        &self,
        namespace_id: &NamespaceId,
        operation_id: &OperationId,
    ) -> ServingTargetEntry {
        serving_target_entry(
            namespace_id,
            &self.service.service_id,
            self.namespace_revision_entry_id(namespace_id, operation_id),
            &self.service.image,
            self.service.mode,
            &self.service.runtime,
        )
    }

    #[must_use]
    pub(super) fn namespace_revision_entry_id(
        &self,
        namespace_id: &NamespaceId,
        operation_id: &OperationId,
    ) -> NamespaceRevisionEntryId {
        self.service
            .namespace_revision_entry_id_for_operation(namespace_id, operation_id)
    }

    #[must_use]
    pub fn route_binding_states(&self) -> &[RouteBindingState] {
        &self.route_commits
    }
}

pub(super) fn serving_target_entry(
    namespace_id: &NamespaceId,
    service_id: &ServiceId,
    namespace_revision_entry_id: NamespaceRevisionEntryId,
    image: &ployz_core::deploy::ImageReference,
    mode: ployz_core::deploy::ServiceMode,
    runtime: &ployz_core::deploy::ContainerRuntimeSpec,
) -> ServingTargetEntry {
    let mut volume_names = runtime
        .volume_mounts
        .iter()
        .map(|mount| mount.volume_name.clone())
        .collect::<Vec<_>>();
    volume_names.sort();
    volume_names.dedup();
    ServingTargetEntry {
        namespace_id: namespace_id.clone(),
        service_id: service_id.clone(),
        namespace_revision_entry_id,
        image: image.clone(),
        mode,
        volume_names,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionOutcome {
    pub namespace_id: ployz_core::ids::NamespaceId,
    pub namespace_revision_id: NamespaceRevisionId,
    pub containers: Vec<DeployContainer>,
    pub cleanup: Vec<DeployCleanupResult>,
    pub image_cleanup: Vec<DeployImageCleanup>,
    pub completion_outcome: DeployCompletionOutcome,
    pub terminal_event: DeployTerminalEvent,
}

impl DeployExecutionOutcome {
    #[must_use]
    #[cfg(test)]
    pub fn completion_outcome(&self) -> DeployCompletionOutcome {
        self.completion_outcome
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployCleanupResult {
    Removed(DeployCleanupContainer),
    Failed {
        target: DeployCleanupContainer,
        message: FailureMessage,
    },
}

impl DeployCleanupResult {
    pub(super) fn completion_outcome(
        cleanup: &[Self],
        images: &[DeployImageCleanup],
    ) -> DeployCompletionOutcome {
        if Self::has_failure(cleanup)
            || images.iter().any(|image| {
                matches!(
                    image,
                    DeployImageCleanup::MissingIdentity { .. } | DeployImageCleanup::Failed { .. }
                )
            })
        {
            DeployCompletionOutcome::CompletedWithWarnings
        } else {
            DeployCompletionOutcome::Completed
        }
    }

    pub(super) fn has_failure(cleanup: &[Self]) -> bool {
        cleanup
            .iter()
            .any(|result| matches!(result, Self::Failed { .. }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployTerminalEvent {
    Recorded,
    Missing,
}

/// How a deploy run step produced its running container. Freshly created
/// containers are health-gated before the deploy completes; reused or
/// restarted existing containers already proved themselves and are never
/// re-gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunContainerDisposition {
    Created,
    Reused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployContainer {
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub step_id: StepId,
    pub requires_docker_healthcheck: bool,
}

impl DeployContainer {
    pub(super) fn retained_artifact(&self) -> RetainedArtifact {
        RetainedArtifact::StartedContainer {
            machine_id: self.machine_id.clone(),
            container_id: self.container_id.clone(),
            log_hint: OperatorHint::try_new(format!("ployz logs {}", self.container_id.as_str()))
                .expect("generated log hint is non-empty"),
        }
    }
}

pub struct DeployExecutionPorts<'a, R, N, H, C, S> {
    pub recorder: &'a mut R,
    pub machine_runtime: &'a mut N,
    pub health_checker: &'a mut H,
    pub certificate_provisioner: &'a mut C,
    pub namespace_state: &'a mut S,
}
