//! Owned deploy execution started by the control service.

use crate::certificate::{CertificateManager, GatewayCertificateTarget};
use crate::control::intent::ingress_intent::{IngressProjectionStore, PloyzDnsTargetStore};
use crate::control::intent::namespace_intent::NamespaceIntentStore;
use crate::control::intent::service::NatsIntentReader;
use crate::control::operation_evidence::{
    AcceptedDeploySubmission, OperationStatusStoreError, OperationStatusWrite,
    RecordDeployTransitionError, RecordOperationEventError,
};
use crate::control::operations::deploy::{
    CertificateProvisioner, DeployContainer, DeployExecutionError, DeployExecutionInput,
    DeployExecutionOutcome, DeployExecutionPorts, DeployFactLoadError, DeployHealthCheckError,
    DeployHealthChecker, DeployPhasePromotion, MachineContainerRuntime, NamespaceCommitError,
    NamespaceStateCommitter, execute_deploy_operation, load_deploy_execution_facts_from_nats,
};
use crate::control::role_client::machine::{
    MachineContainerInspectError, NatsMachineContainerRuntime, NatsMachineFactsReader,
};
use crate::control::sequencer::{AcceptedDeployExecution, OperationControllers};
use crate::roles::machine::protocol::{
    MachineContainerInspectRpcOk, MachineContainerInspectRpcRequest,
};
use crate::tasks::TaskSpawner;
use ployz_core::certificate::ActiveCertState;
use ployz_core::deploy::{
    AutoHostnameRouteBindingError, DeployRouteBindingValidationError, VolumeDeclaredDeployRequest,
};
use ployz_core::ingress::CertificateOwner;
use ployz_core::machine::runtime::{ContainerHealth, ContainerRuntimeState};
use ployz_core::operation::{
    CertificateProvisionFailure, DeployOperationFailure, DeployOperationState, DeployTransition,
    FailureMessage, OperationStatus, OperatorHint, RouteHostname, StatusProjectionError,
};
use ployz_nats::subjects::INTENT_CHANGED;
use std::collections::BTreeSet;
use std::time::Duration;

const DEPLOY_HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// A container without a Docker healthcheck must stay running this long
/// before deploy completion commits it: a fast-exiting process can be
/// sampled alive once, and one running observation is not survival.
const DEPLOY_RUNNING_CONFIRMATION_WINDOW: Duration = Duration::from_millis(1_500);
const DEPLOY_PREVIEW_GATHER_TIMEOUT: Duration = Duration::from_secs(8);

impl CertificateProvisioner for CertificateManager {
    async fn ensure(
        &mut self,
        owner_operation_id: &ployz_core::ids::OperationId,
        owner: CertificateOwner,
        hostname: &RouteHostname,
        targets: &[GatewayCertificateTarget],
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        CertificateManager::ensure(self, owner_operation_id, owner, hostname, targets).await
    }

    async fn ensure_ployz_wildcard(
        &mut self,
        owner_operation_id: &ployz_core::ids::OperationId,
        targets: &[GatewayCertificateTarget],
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        CertificateManager::ensure_ployz_wildcard(self, owner_operation_id, targets).await
    }
}

pub async fn run_deploy_operation<N, H, C>(
    accepted_execution: AcceptedDeployExecution,
    stores: DeployOperationStores,
    ports: DeployOperationPorts<'_, N, H, C>,
    step_timeout: Duration,
) -> Result<DeployExecutionOutcome, DeployOperationRunError>
where
    N: MachineContainerRuntime + super::MachineImageRemovalRuntime,
    H: DeployHealthChecker,
    C: CertificateProvisioner,
{
    let AcceptedDeployExecution {
        submission: accepted,
        registry_credentials,
        ingress_fence_owner: _,
    } = accepted_execution;
    if !claim_deploy_execution(&stores.controllers, &accepted.operation_id).await? {
        return Err(DeployOperationRunError::AlreadyStarted);
    }
    let DeployOperationPorts {
        facts_reader,
        intent_reader,
        machine_runtime,
        health_checker,
        certificate_provisioner,
    } = ports;
    let normalized_request = match normalize_stored_deploy_target(accepted.target.clone()) {
        Ok(request) => request,
        Err(source) => {
            let failure_record_error = record_operation_failure(
                &stores.controllers,
                &accepted,
                fact_load_failure(&accepted.target, &source),
            )
            .await
            .err();
            return Err(DeployOperationRunError::LoadFacts {
                source,
                failure_record_error,
            });
        }
    };

    let facts = match load_deploy_execution_facts_from_nats(
        &normalized_request,
        intent_reader,
        facts_reader,
        &stores.ployz_dns_target,
        &stores.ingress_projection,
        step_timeout,
    )
    .await
    {
        Ok(facts) => facts,
        Err(source) => {
            let failure_record_error = record_operation_failure(
                &stores.controllers,
                &accepted,
                fact_load_failure(&accepted.target, &source),
            )
            .await
            .err();
            return Err(DeployOperationRunError::LoadFacts {
                source,
                failure_record_error,
            });
        }
    };
    let reusable_interrupted_operation_ids = match reusable_interrupted_deploy_operation_ids(
        stores.controllers.repository(),
        normalized_request.namespace_id(),
        &facts.observed_machines,
    )
    .await
    {
        Ok(operation_ids) => operation_ids,
        Err(source) => {
            let failure_record_error = record_operation_failure(
                &stores.controllers,
                &accepted,
                DeployOperationFailure::PlanningFailed {
                    service_id: normalized_request.status_service_id(),
                    namespace_revision_id: normalized_request.namespace_revision_id(),
                    message: FailureMessage::try_new(source.to_string())
                        .expect("rendered recovery evidence failure is non-empty"),
                },
            )
            .await
            .err();
            return Err(DeployOperationRunError::RecoveryEvidence {
                source,
                failure_record_error,
            });
        }
    };
    let DeployOperationStores {
        intent_change_client,
        namespace_intent,
        ployz_dns_target: _,
        ingress_projection: _,
        controllers,
    } = stores;
    let input = DeployExecutionInput::new(
        accepted.operation_id.clone(),
        normalized_request,
        facts,
        registry_credentials,
        reusable_interrupted_operation_ids,
    );
    let mut recorder = controllers;
    let mut namespace_state = NamespaceIntentCommitter::new(intent_change_client, namespace_intent);
    execute_deploy_operation(
        input,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime,
            health_checker,
            certificate_provisioner,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .map_err(DeployOperationRunError::Execute)
}

fn normalize_stored_deploy_target(
    request: ployz_core::deploy::DeployRequest,
) -> Result<VolumeDeclaredDeployRequest, DeployFactLoadError> {
    VolumeDeclaredDeployRequest::try_new(request).map_err(|error| {
        DeployFactLoadError::InvalidStoredTarget {
            message: error.to_string(),
        }
    })
}

async fn record_operation_failure(
    controllers: &OperationControllers,
    accepted: &AcceptedDeploySubmission,
    failure: DeployOperationFailure,
) -> Result<(), RecordDeployTransitionError> {
    controllers
        .repository()
        .record_deploy_transition(&accepted.operation_id, DeployTransition::Failed { failure })
        .await
        .map(|_| ())
}

fn fact_load_failure(
    request: &ployz_core::deploy::DeployRequest,
    source: &DeployFactLoadError,
) -> DeployOperationFailure {
    match source {
        DeployFactLoadError::InvalidRouteBindings {
            failure:
                DeployRouteBindingValidationError::Automatic(
                    AutoHostnameRouteBindingError::HostnameCollision {
                        hostname,
                        route_binding_id,
                    },
                ),
        } => DeployOperationFailure::AutomaticHostnameCollision {
            hostname: hostname.clone(),
            route_binding_id: route_binding_id.clone(),
        },
        DeployFactLoadError::InvalidStoredTarget { .. }
        | DeployFactLoadError::IntentRead { .. }
        | DeployFactLoadError::InvalidRouteBindings { .. }
        | DeployFactLoadError::IngressState { .. }
        | DeployFactLoadError::IngressUnavailable { .. } => {
            DeployOperationFailure::PlanningFailed {
                service_id: request.status_service_id(),
                namespace_revision_id: request.namespace_revision_id(),
                message: FailureMessage::try_new(source.to_string())
                    .expect("rendered fact load failure message is non-empty"),
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeployOperationStores {
    pub intent_change_client: async_nats::Client,
    pub namespace_intent: NamespaceIntentStore,
    pub ployz_dns_target: PloyzDnsTargetStore,
    pub ingress_projection: IngressProjectionStore,
    pub controllers: OperationControllers,
}

pub struct DeployOperationPorts<'a, N, H, C> {
    pub facts_reader: &'a NatsMachineFactsReader,
    pub intent_reader: &'a NatsIntentReader,
    pub machine_runtime: &'a mut N,
    pub health_checker: &'a mut H,
    pub certificate_provisioner: &'a mut C,
}

struct NamespaceIntentCommitter {
    intent_change_client: async_nats::Client,
    namespace_intent: NamespaceIntentStore,
}

impl NamespaceIntentCommitter {
    fn new(
        intent_change_client: async_nats::Client,
        namespace_intent: NamespaceIntentStore,
    ) -> Self {
        Self {
            intent_change_client,
            namespace_intent,
        }
    }

    async fn publish_intent_changed(&self) {
        let _ = self
            .intent_change_client
            .publish(INTENT_CHANGED, Vec::new().into())
            .await;
    }
}

impl NamespaceStateCommitter for NamespaceIntentCommitter {
    async fn commit_deploy_phase(
        &mut self,
        promotion: DeployPhasePromotion,
    ) -> Result<(), NamespaceCommitError> {
        let DeployPhasePromotion {
            scope,
            route_bindings,
            route_binding_removals,
            first_serving_target_entry,
            remaining_serving_target_entries,
        } = promotion;
        let serving_target_entries = std::iter::once(first_serving_target_entry)
            .chain(remaining_serving_target_entries)
            .collect();
        self.namespace_intent
            .commit_deploy_phase(
                route_bindings,
                route_binding_removals,
                serving_target_entries,
            )
            .await
            .map_err(|error| NamespaceCommitError::ServingTarget {
                scope,
                message: error.to_string(),
            })?;
        self.publish_intent_changed().await;
        Ok(())
    }

    async fn remove_route_binding(
        &mut self,
        target: ployz_core::operation::RouteTarget,
    ) -> Result<(), NamespaceCommitError> {
        self.namespace_intent
            .remove_route_binding(&target)
            .await
            .map_err(|error| NamespaceCommitError::Route {
                target,
                message: error.to_string(),
            })?;
        self.publish_intent_changed().await;
        Ok(())
    }

    async fn remove_serving_target_entry(
        &mut self,
        entry: ployz_core::intent::ServingTargetEntry,
    ) -> Result<(), NamespaceCommitError> {
        let scope = ployz_core::operation::ControlPlaneCommitScope::ServiceEntry {
            service_id: entry.service_id.clone(),
            namespace_revision_entry_id: entry.namespace_revision_entry_id.clone(),
        };
        self.namespace_intent
            .remove_serving_target_entry(&entry)
            .await
            .map_err(|error| NamespaceCommitError::ServingTarget {
                scope,
                message: error.to_string(),
            })?;
        self.publish_intent_changed().await;
        Ok(())
    }

    async fn replace_volume_pin(
        &mut self,
        state: ployz_core::intent::VolumePinState,
    ) -> Result<(), NamespaceCommitError> {
        self.namespace_intent
            .replace_volume_pin(state.clone())
            .await
            .map_err(|error| NamespaceCommitError::VolumePin {
                state,
                message: error.to_string(),
            })?;
        self.publish_intent_changed().await;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DeployOperationRunError {
    #[error("deploy operation was already started")]
    AlreadyStarted,
    #[error("deploy start could not be recorded: {0:?}")]
    ClaimStart(RecordDeployTransitionError),
    #[error(
        "interrupted deploy evidence could not be read: {source:?}; failure record: {failure_record_error:?}"
    )]
    RecoveryEvidence {
        source: OperationStatusStoreError,
        failure_record_error: Option<RecordDeployTransitionError>,
    },
    #[error("deploy facts could not be loaded: {source}; failure record: {failure_record_error:?}")]
    LoadFacts {
        source: DeployFactLoadError,
        failure_record_error: Option<RecordDeployTransitionError>,
    },
    #[error("deploy execution failed: {0}")]
    Execute(DeployExecutionError),
}

#[cfg(test)]
mod fact_load_failure_tests {
    use super::*;
    use ployz_core::deploy::{AutoHostnameRouteBindingError, DeployRouteBindingValidationError};
    use ployz_core::ids::RouteBindingId;
    use ployz_test_support::fixtures::deploy_target;
    use ployz_test_support::ids::{route_hostname, service_id};

    #[test]
    fn fact_load_failure_preserves_automatic_hostname_collision_identity() {
        let request = deploy_target("svc_api");
        let failure = fact_load_failure(
            &request,
            &DeployFactLoadError::InvalidRouteBindings {
                failure: DeployRouteBindingValidationError::Automatic(
                    AutoHostnameRouteBindingError::HostnameCollision {
                        hostname: route_hostname("api.apps.example.com"),
                        route_binding_id: RouteBindingId::try_new("route_existing")
                            .expect("valid route binding id"),
                    },
                ),
            },
        );

        assert_eq!(
            failure,
            DeployOperationFailure::AutomaticHostnameCollision {
                hostname: route_hostname("api.apps.example.com"),
                route_binding_id: RouteBindingId::try_new("route_existing")
                    .expect("valid route binding id"),
            }
        );
    }

    #[test]
    fn non_collision_invalid_route_failure_keeps_planning_failed_contract() {
        let request = deploy_target("svc_api");
        let failure = fact_load_failure(
            &request,
            &DeployFactLoadError::InvalidRouteBindings {
                failure: DeployRouteBindingValidationError::DuplicateServiceId {
                    service_id: service_id("svc_api"),
                },
            },
        );

        let DeployOperationFailure::PlanningFailed { message, .. } = failure else {
            panic!("non-collision route error must keep planning_failed")
        };
        assert_eq!(
            message.as_str(),
            "invalid route bindings: service svc_api is declared more than once"
        );
    }
}

async fn reusable_interrupted_deploy_operation_ids(
    repository: &crate::control::operation_evidence::OperationRepository,
    namespace_id: &ployz_core::ids::NamespaceId,
    observed_machines: &[ployz_core::machine::runtime::MachineContainerObservationSnapshot],
) -> Result<BTreeSet<ployz_core::ids::OperationId>, OperationStatusStoreError> {
    let candidate_ids = observed_machines
        .iter()
        .flat_map(ployz_core::machine::runtime::MachineContainerObservationSnapshot::containers)
        .filter(|container| container.identity.namespace_id == *namespace_id)
        .map(|container| container.identity.operation_id.clone())
        .collect::<BTreeSet<_>>();
    let statuses = repository.operation_statuses_for(candidate_ids).await?;
    let mut reusable = BTreeSet::new();
    for status in statuses {
        match status {
            OperationStatus::Deploy {
                id,
                namespace_id: status_namespace_id,
                state: DeployOperationState::Interrupted { .. },
                ..
            } if status_namespace_id == *namespace_id => {
                reusable.insert(id);
            }
            OperationStatus::Build { .. }
            | OperationStatus::Deploy { .. }
            | OperationStatus::VolumeCreate { .. }
            | OperationStatus::Cert { .. }
            | OperationStatus::MachineAdd { .. }
            | OperationStatus::MachineUpdate { .. }
            | OperationStatus::MachineStoragePrepare { .. }
            | OperationStatus::MachineLifecycle { .. }
            | OperationStatus::CoreReplace { .. }
            | OperationStatus::CredentialGrant { .. }
            | OperationStatus::NetworkRepair { .. }
            | OperationStatus::ServiceRestart { .. }
            | OperationStatus::ManagedDnsReconcile { .. }
            | OperationStatus::IngressConfigure { .. }
            | OperationStatus::NamespaceRemove { .. }
            | OperationStatus::VolumeRemove { .. } => {}
        }
    }
    Ok(reusable)
}

#[derive(Debug, Clone)]
pub struct DeployOperationDriver {
    stores: DeployOperationStores,
    certificate_manager: CertificateManager,
    step_timeout: Duration,
    task_registry: TaskSpawner,
}

impl DeployOperationDriver {
    #[must_use]
    pub fn new(
        stores: DeployOperationStores,
        certificate_manager: CertificateManager,
        step_timeout: Duration,
        task_registry: TaskSpawner,
    ) -> Self {
        Self {
            stores,
            certificate_manager,
            step_timeout,
            task_registry,
        }
    }

    pub async fn start(&self, accepted: AcceptedDeployExecution) {
        if !accepted.submission.should_start_execution {
            return;
        }

        let operation_id = accepted.submission.operation_id.clone();
        let runtime = self.clone();
        super::super::finish_rejected_task_admission(
            &self.stores.controllers,
            &operation_id,
            self.task_registry.spawn(|| async move {
                let _outcome = runtime.run(accepted).await;
            }),
        )
        .await;
    }

    pub async fn preview(
        &self,
        request: ployz_sdk_types::DeployPreviewRequest,
    ) -> Result<ployz_sdk_types::DeployPreview, ployz_sdk_types::DeployPreviewError> {
        let client = self.stores.intent_change_client.clone();
        let gather_timeout = self.step_timeout.min(DEPLOY_PREVIEW_GATHER_TIMEOUT);
        let facts_reader =
            NatsMachineFactsReader::new(client.clone()).with_request_timeout(gather_timeout);
        let intent_reader = NatsIntentReader::new(client).with_request_timeout(gather_timeout);
        super::preview_deploy_from_nats(
            request,
            &intent_reader,
            &facts_reader,
            &self.stores.ployz_dns_target,
            &self.stores.ingress_projection,
            gather_timeout,
        )
        .await
    }

    pub async fn run(
        self,
        accepted: AcceptedDeployExecution,
    ) -> Result<DeployExecutionOutcome, DeployOperationRunError> {
        let client = self.stores.intent_change_client.clone();
        let mut machine_runtime = NatsMachineContainerRuntime::new(client.clone())
            .with_request_timeout(self.step_timeout);
        let facts_reader =
            NatsMachineFactsReader::new(client.clone()).with_request_timeout(self.step_timeout);
        let health_runtime = NatsMachineContainerRuntime::new(client.clone())
            .with_request_timeout(self.step_timeout);
        let mut health_checker =
            LiveContainerHealthChecker::new(health_runtime, DEPLOY_HEALTH_POLL_INTERVAL);
        let intent_reader = NatsIntentReader::new(client).with_request_timeout(self.step_timeout);
        let mut certificate_manager = self.certificate_manager.clone();

        let namespace_id = accepted.submission.target.namespace_id.clone();
        let operation_id = accepted.submission.operation_id.clone();
        let ingress_fence_owner = accepted.ingress_fence_owner.clone();
        let controllers = self.stores.controllers.clone();
        let result = run_deploy_operation(
            accepted,
            self.stores,
            DeployOperationPorts {
                facts_reader: &facts_reader,
                intent_reader: &intent_reader,
                machine_runtime: &mut machine_runtime,
                health_checker: &mut health_checker,
                certificate_provisioner: &mut certificate_manager,
            },
            self.step_timeout,
        )
        .await;
        if !matches!(&result, Err(DeployOperationRunError::AlreadyStarted)) {
            controllers
                .release_namespace(&namespace_id, &operation_id)
                .await;
            if let Some(owner) = ingress_fence_owner {
                controllers.release_ingress(&owner).await;
            }
        }
        result
    }
}

async fn claim_deploy_execution(
    controllers: &OperationControllers,
    operation_id: &ployz_core::ids::OperationId,
) -> Result<bool, DeployOperationRunError> {
    match controllers
        .repository()
        .record_deploy_transition(operation_id, DeployTransition::Planning)
        .await
    {
        Ok(OperationStatusWrite::Stored) => Ok(true),
        Ok(OperationStatusWrite::AlreadySatisfied { .. }) => Ok(false),
        Err(RecordOperationEventError::ProjectStatus(
            StatusProjectionError::InvalidTransition { .. }
            | StatusProjectionError::TerminalState { .. },
        )) => Ok(false),
        Err(error) => Err(DeployOperationRunError::ClaimStart(error)),
    }
}

pub struct LiveContainerHealthChecker {
    runtime: NatsMachineContainerRuntime,
    poll_interval: Duration,
}

impl LiveContainerHealthChecker {
    #[must_use]
    pub fn new(runtime: NatsMachineContainerRuntime, poll_interval: Duration) -> Self {
        Self {
            runtime,
            poll_interval,
        }
    }
}

impl DeployHealthChecker for LiveContainerHealthChecker {
    async fn wait_healthy(
        &mut self,
        containers: &[DeployContainer],
    ) -> Result<(), DeployHealthCheckError> {
        loop {
            let mut all_confirmed = true;
            for container in containers {
                let MachineContainerInspectRpcOk {
                    observation,
                    observed_at_unix_ms,
                    ..
                } = self
                    .runtime
                    .inspect_container(
                        &container.machine_id,
                        MachineContainerInspectRpcRequest {
                            container_id: container.container_id.clone(),
                        },
                    )
                    .await
                    .map_err(|error| unhealthy_container(container, health_read_error(error)))?;
                let readiness = match &observation {
                    Some(observation) => observed_container_readiness(
                        observation,
                        container.requires_docker_healthcheck,
                        observed_at_unix_ms,
                    ),
                    None => ObservedContainerReadiness::Pending,
                };
                match readiness {
                    ObservedContainerReadiness::Confirmed => {}
                    ObservedContainerReadiness::Pending => all_confirmed = false,
                    ObservedContainerReadiness::Failed(message) => {
                        return Err(unhealthy_container(container, message));
                    }
                }
            }

            if all_confirmed {
                return Ok(());
            }

            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

fn observed_container_readiness(
    observation: &ployz_core::machine::runtime::ManagedContainerObservation,
    requires_docker_healthcheck: bool,
    observed_at_unix_ms: u64,
) -> ObservedContainerReadiness {
    match &observation.state {
        ContainerRuntimeState::Running {
            health,
            started_at_unix_ms,
            ..
        } => match health {
            ContainerHealth::Starting => ObservedContainerReadiness::Pending,
            ContainerHealth::Healthy => ObservedContainerReadiness::Confirmed,
            ContainerHealth::Unhealthy => ObservedContainerReadiness::Failed("unhealthy"),
            ContainerHealth::None if requires_docker_healthcheck => {
                ObservedContainerReadiness::Pending
            }
            ContainerHealth::None => match started_at_unix_ms {
                Some(started_at_unix_ms)
                    if observed_at_unix_ms.saturating_sub(*started_at_unix_ms)
                        >= DEPLOY_RUNNING_CONFIRMATION_WINDOW.as_millis() as u64 =>
                {
                    ObservedContainerReadiness::Confirmed
                }
                Some(_) | None => ObservedContainerReadiness::Pending,
            },
        },
        ContainerRuntimeState::Exited => ObservedContainerReadiness::Failed("container exited"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservedContainerReadiness {
    Confirmed,
    Pending,
    Failed(&'static str),
}

fn unhealthy_container(
    container: &DeployContainer,
    message: impl Into<String>,
) -> DeployHealthCheckError {
    let message = FailureMessage::try_new(message).expect("health failure message is non-empty");
    let log_hint = OperatorHint::try_new(format!("ployz logs {}", container.container_id.as_str()))
        .expect("generated log hint is non-empty");
    DeployHealthCheckError::Unhealthy {
        machine_id: container.machine_id.clone(),
        container_id: container.container_id.clone(),
        message,
        log_hint,
    }
}

fn health_read_error(error: MachineContainerInspectError) -> String {
    match error {
        MachineContainerInspectError::InspectFailed { message, .. } => {
            format!("container status could not be read: {}", message.as_str())
        }
        MachineContainerInspectError::Unavailable { reason, .. } => {
            format!(
                "container status unavailable: {}",
                reason.failure_message().as_str()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::{
        ContainerId, MachineId, NamespaceRevisionEntryId, OperationId, ServiceId, StepId,
    };
    use ployz_core::machine::runtime::{
        ContainerHealth, ContainerRuntimeState, ManagedContainerIdentity, ManagedContainerKind,
        ManagedContainerObservation,
    };

    #[test]
    fn invalid_persisted_volume_contract_has_a_dedicated_failure() {
        let mut runtime = ployz_core::deploy::ContainerRuntimeSpec::image_defaults();
        runtime.volume_mounts = vec![ployz_core::deploy::ServiceVolumeMount {
            volume_name: ployz_core::deploy::VolumeName::try_new("data").expect("volume name"),
            target: ployz_core::deploy::ContainerMountPath::try_new("/data").expect("mount path"),
        }];
        let request = ployz_core::deploy::DeployRequest {
            namespace_id: namespace_id("default"),
            origin: None,
            volumes: std::collections::BTreeMap::new(),
            services: vec![ployz_core::deploy::DeployServiceSpec {
                keep: None,
                service_id: service_id("svc_api"),
                image: ployz_core::deploy::ImageReference::try_new("nginx:latest").expect("image"),
                image_source: ployz_core::deploy::ImageSource::Registry,
                replicas: ployz_core::deploy::ReplicaCount::try_new(1).expect("replicas"),
                runtime,
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            }],
        };

        assert!(matches!(
            normalize_stored_deploy_target(request),
            Err(DeployFactLoadError::InvalidStoredTarget { .. })
        ));
    }

    #[test]
    fn running_confirmation_uses_observed_container_start_time() {
        let mut observation = observation(
            "machine_a",
            "ctr_1",
            ContainerRuntimeState::running_unroutable(),
        );
        observation.state = ContainerRuntimeState::Running {
            ip: None,
            health: ContainerHealth::None,
            started_at_unix_ms: Some(1_000),
        };

        assert_eq!(
            observed_container_readiness(&observation, false, 2_500),
            ObservedContainerReadiness::Confirmed
        );
    }

    #[test]
    fn docker_health_confirms_immediately() {
        assert_eq!(
            observed_container_readiness(
                &observation(
                    "machine_a",
                    "ctr_1",
                    ContainerRuntimeState::Running {
                        ip: None,
                        health: ContainerHealth::Healthy,
                        started_at_unix_ms: None,
                    },
                ),
                false,
                0,
            ),
            ObservedContainerReadiness::Confirmed,
        );
    }

    #[test]
    fn health_waits_when_running_start_time_is_missing() {
        assert_eq!(
            observed_container_readiness(
                &observation(
                    "machine_a",
                    "ctr_1",
                    ContainerRuntimeState::running_unroutable()
                ),
                false,
                0,
            ),
            ObservedContainerReadiness::Pending
        );
    }

    #[test]
    fn health_waits_for_missing_configured_docker_health() {
        assert_eq!(
            observed_container_readiness(
                &observation(
                    "machine_a",
                    "ctr_1",
                    ContainerRuntimeState::running_unroutable()
                ),
                true,
                0,
            ),
            ObservedContainerReadiness::Pending
        );
    }

    #[test]
    fn health_confirms_docker_healthy_regardless_of_requirement() {
        for requires_docker_healthcheck in [false, true] {
            assert_eq!(
                observed_container_readiness(
                    &observation(
                        "machine_a",
                        "ctr_1",
                        ContainerRuntimeState::Running {
                            ip: None,
                            health: ContainerHealth::Healthy,
                            started_at_unix_ms: None,
                        },
                    ),
                    requires_docker_healthcheck,
                    0,
                ),
                ObservedContainerReadiness::Confirmed
            );
        }
    }

    #[test]
    fn health_fails_exited_container() {
        assert_eq!(
            observed_container_readiness(
                &observation("machine_a", "ctr_1", ContainerRuntimeState::Exited),
                false,
                0,
            ),
            ObservedContainerReadiness::Failed("container exited")
        );
    }

    #[test]
    fn health_waits_for_starting_and_fails_unhealthy() {
        assert_eq!(
            observed_container_readiness(
                &observation(
                    "machine_a",
                    "ctr_1",
                    ContainerRuntimeState::Running {
                        ip: None,
                        health: ContainerHealth::Starting,
                        started_at_unix_ms: None,
                    }
                ),
                true,
                0,
            ),
            ObservedContainerReadiness::Pending
        );
        assert_eq!(
            observed_container_readiness(
                &observation(
                    "machine_a",
                    "ctr_1",
                    ContainerRuntimeState::Running {
                        ip: None,
                        health: ContainerHealth::Unhealthy,
                        started_at_unix_ms: None,
                    }
                ),
                true,
                0,
            ),
            ObservedContainerReadiness::Failed("unhealthy")
        );
    }

    fn observation(
        machine_id_value: &str,
        container_id_value: &str,
        state: ContainerRuntimeState,
    ) -> ManagedContainerObservation {
        ManagedContainerObservation {
            machine_id: machine_id(machine_id_value),
            container_id: container_id(container_id_value),
            identity: ManagedContainerIdentity {
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                namespace_revision_entry_id: namespace_revision_entry_id("entry_observed"),
                operation_id: operation_id("op_123"),
                step_id: step_id("run_1"),
                kind: ManagedContainerKind::Service,
            },
            state,
            health_status: None,
            resolved_image_identity: None,
            created_at_unix_seconds: None,
        }
    }

    fn namespace_id(value: &str) -> ployz_core::ids::NamespaceId {
        ployz_core::ids::NamespaceId::try_new(value).expect("valid namespace id")
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn container_id(value: &str) -> ContainerId {
        ContainerId::try_new(value).expect("valid container id")
    }

    fn service_id(value: &str) -> ServiceId {
        ServiceId::try_new(value).expect("valid service id")
    }

    fn namespace_revision_entry_id(value: &str) -> NamespaceRevisionEntryId {
        NamespaceRevisionEntryId::try_new(value).expect("valid namespace revision entry id")
    }

    fn operation_id(value: &str) -> OperationId {
        OperationId::try_new(value).expect("valid operation id")
    }

    fn step_id(value: &str) -> StepId {
        StepId::try_new(value).expect("valid step id")
    }
}
