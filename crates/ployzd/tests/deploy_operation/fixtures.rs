use ployz_core::dataplane::{
    WireGuardEbpfComponent, WireGuardEbpfPrepareError, WireGuardEbpfPrepareRequest,
};
use ployz_core::deploy::{
    DeployCleanupContainer, DeployRequest, DeployRoute, ImageReference, ReplicaCount,
};
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::{
    ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    NodeContainerObservationSnapshot,
};
use ployz_core::ops::{
    DeployCleanupFailure, DeployEvidence, DeployRunningStage, DeployTransition, FailureMessage,
    OperatorHint, RetainedArtifact, RouteHostname, RoutePort, RouteTarget,
};
use ployz_core::state::{
    ActiveRouteCommit, ActiveRouteCommitRequest, ActiveRouteState, ActiveServiceCommit,
    ActiveServiceCommitRequest, CoreStateRevision, ExpectedActiveRoute, ExpectedActiveService,
};
use ployzd::deploy_worker::{
    ActiveRouteCommitError, ActiveRouteCommitter, ActiveServiceCommitError, ActiveServiceCommitter,
    DeployExecutionCommand, DeployExecutionFacts, DeployHealthCheckError, DeployHealthChecker,
    DeployOperationRecordError, DeployOperationRecorder, NodeContainerRuntime,
    NodeContainerRuntimeError, NodeRemoveContainerRequest, NodeRunContainerOutcome,
    NodeRunContainerRequest, NodeRuntimeUnavailableReason, WireGuardEbpfPreparer,
    prepare_deploy_execution_command,
};
use ployzd::operation_lease::OperationLeaseRenewer;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
pub(super) struct RecordingOperations {
    pub(super) records: Vec<RecordedOperation>,
    fail_completed_transition_remaining: usize,
    pub(super) completed_transition_attempts: usize,
}

impl RecordingOperations {
    pub(super) const fn fail_completed_transition_times(times: usize) -> Self {
        Self {
            records: Vec::new(),
            fail_completed_transition_remaining: times,
            completed_transition_attempts: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecordedOperation {
    Transition(DeployTransition),
    PlanCreated {
        replica_count: usize,
    },
    HealthCheckStarted,
    ContainerStarted {
        node_id: NodeId,
        container_id: ContainerId,
    },
    CleanupFinished {
        removed: Vec<DeployCleanupContainer>,
        failed: Vec<DeployCleanupFailure>,
    },
}

impl DeployOperationRecorder for RecordingOperations {
    async fn record_deploy_transition(
        &mut self,
        recorded_operation_id: &OperationId,
        transition: DeployTransition,
    ) -> Result<(), DeployOperationRecordError> {
        assert_eq!(recorded_operation_id, &operation_id("op_123"));
        if self.fail_completed_transition_remaining > 0
            && transition == (DeployTransition::Completed)
        {
            self.fail_completed_transition_remaining -= 1;
            self.completed_transition_attempts += 1;
            return Err(DeployOperationRecordError::Synthetic {
                message: "completion record failed",
            });
        }
        self.records.push(RecordedOperation::Transition(transition));
        Ok(())
    }

    async fn record_deploy_evidence(
        &mut self,
        recorded_operation_id: &OperationId,
        evidence: DeployEvidence,
    ) -> Result<(), DeployOperationRecordError> {
        assert_eq!(recorded_operation_id, &operation_id("op_123"));
        match evidence {
            DeployEvidence::PlanCreated { plan } => {
                self.records.push(RecordedOperation::PlanCreated {
                    replica_count: plan.steps.len(),
                });
            }
            DeployEvidence::ContainerStarted {
                node_id,
                container_id,
            } => {
                self.records.push(RecordedOperation::ContainerStarted {
                    node_id,
                    container_id,
                });
            }
            DeployEvidence::HealthCheckStarted => {
                self.records.push(RecordedOperation::HealthCheckStarted);
            }
            DeployEvidence::CleanupFinished { removed, failed } => {
                self.records
                    .push(RecordedOperation::CleanupFinished { removed, failed });
            }
        }
        Ok(())
    }
}

pub(super) struct RecordingRuntime {
    pub(super) requests: Vec<NodeRunContainerRequest>,
    pub(super) removals: Vec<NodeRemoveContainerRequest>,
    containers: Vec<ContainerId>,
    fail_after_first: bool,
    reuse_existing: bool,
    fail_start: bool,
    fail_remove: bool,
}

#[derive(Default)]
pub(super) struct RecordingWireGuardEbpf {
    pub(super) requests: Vec<WireGuardEbpfPrepareRequest>,
    failure: Option<WireGuardEbpfPrepareError>,
}

impl RecordingWireGuardEbpf {
    pub(super) fn ready() -> Self {
        Self::default()
    }

    pub(super) fn wireguard_failed(node_id: &str) -> Self {
        Self {
            requests: Vec::new(),
            failure: Some(WireGuardEbpfPrepareError::Unavailable {
                node_id: self::node_id(node_id),
                component: WireGuardEbpfComponent::WireGuard,
                message: ployz_core::ops::FailureMessage::try_new("wireguard interface failed")
                    .expect("valid failure message"),
            }),
        }
    }
}

impl WireGuardEbpfPreparer for RecordingWireGuardEbpf {
    async fn prepare_wireguard_ebpf(
        &mut self,
        request: WireGuardEbpfPrepareRequest,
    ) -> Result<(), WireGuardEbpfPrepareError> {
        self.requests.push(request);
        match &self.failure {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}

pub(super) struct RecordingActiveState {
    pub(super) requests: Vec<ActiveServiceCommitRequest>,
    outcome: ActiveServiceCommit,
}

pub(super) struct RecordingRouteState {
    pub(super) requests: Vec<ActiveRouteCommitRequest>,
    outcome: ActiveRouteCommit,
}

impl RecordingRouteState {
    pub(super) fn stored() -> Self {
        Self {
            requests: Vec::new(),
            outcome: ActiveRouteCommit::Stored {
                revision: CoreStateRevision::new(1),
            },
        }
    }

    pub(super) fn stale_mismatch() -> Self {
        let route = route_target("api.example.com", 443);
        Self {
            requests: Vec::new(),
            outcome: ActiveRouteCommit::ActiveRouteChanged {
                expected_current: ExpectedActiveRoute::Absent,
                current: Some(ActiveRouteState {
                    target: route.clone(),
                    endpoint_port: route_port(8080),
                    service_id: service_id("svc_other"),
                    revision_id: revision_id("rev_other"),
                }),
                attempted: ActiveRouteState {
                    target: route,
                    endpoint_port: route_port(8080),
                    service_id: service_id("svc_api"),
                    revision_id: revision_id("rev_2"),
                },
            },
        }
    }
}

impl ActiveRouteCommitter for RecordingRouteState {
    async fn commit_active_route(
        &mut self,
        request: ActiveRouteCommitRequest,
    ) -> Result<ActiveRouteCommit, ActiveRouteCommitError> {
        self.requests.push(request);
        Ok(self.outcome.clone())
    }
}

impl RecordingActiveState {
    pub(super) fn stored() -> Self {
        Self {
            requests: Vec::new(),
            outcome: ActiveServiceCommit::Stored {
                revision: CoreStateRevision::new(1),
            },
        }
    }

    pub(super) fn stale_mismatch() -> Self {
        Self {
            requests: Vec::new(),
            outcome: ActiveServiceCommit::ActiveServiceChanged {
                expected_current: ExpectedActiveService::Revision(revision_id("rev_old")),
                current_revision: Some(revision_id("rev_other")),
                attempted_revision: revision_id("rev_2"),
            },
        }
    }
}

impl ActiveServiceCommitter for RecordingActiveState {
    async fn commit_active_service(
        &mut self,
        request: ActiveServiceCommitRequest,
    ) -> Result<ActiveServiceCommit, ActiveServiceCommitError> {
        self.requests.push(request);
        Ok(self.outcome.clone())
    }
}

pub(super) struct HangingActiveState;

impl ActiveServiceCommitter for HangingActiveState {
    async fn commit_active_service(
        &mut self,
        _request: ActiveServiceCommitRequest,
    ) -> Result<ActiveServiceCommit, ActiveServiceCommitError> {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok(ActiveServiceCommit::Stored {
            revision: CoreStateRevision::new(1),
        })
    }
}

#[derive(Default)]
pub(super) struct RecordingHealth {
    pub(super) checked: Vec<Vec<DeployContainerForAssert>>,
    failure: Option<DeployHealthCheckError>,
}

impl RecordingHealth {
    pub(super) fn healthy() -> Self {
        Self::default()
    }

    pub(super) fn unhealthy(node_id: &str, container_id: &str) -> Self {
        Self {
            checked: Vec::new(),
            failure: Some(DeployHealthCheckError::Unhealthy {
                node_id: self::node_id(node_id),
                container_id: self::container_id(container_id),
                message: ployz_core::ops::FailureMessage::try_new("probe failed")
                    .expect("valid failure message"),
                log_hint: ployz_core::ops::OperatorHint::try_new(format!(
                    "ployz logs {container_id}"
                ))
                .expect("valid log hint"),
            }),
        }
    }
}

impl DeployHealthChecker for RecordingHealth {
    async fn wait_healthy(
        &mut self,
        containers: &[ployzd::deploy_worker::DeployContainer],
    ) -> Result<(), DeployHealthCheckError> {
        self.checked.push(
            containers
                .iter()
                .map(|container| {
                    DeployContainerForAssert::from_container(
                        container.node_id.clone(),
                        container.container_id.clone(),
                        container.required_endpoint_port,
                    )
                })
                .collect(),
        );

        match &self.failure {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}

pub(super) struct HangingHealth;

impl DeployHealthChecker for HangingHealth {
    async fn wait_healthy(
        &mut self,
        _containers: &[ployzd::deploy_worker::DeployContainer],
    ) -> Result<(), DeployHealthCheckError> {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(super) struct RecordingLeaseRenewer {
    renewals: Arc<Mutex<Vec<OperationId>>>,
}

impl RecordingLeaseRenewer {
    pub(super) fn lost() -> Self {
        Self {
            renewals: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(super) fn renewals(&self) -> Vec<OperationId> {
        self.renewals
            .lock()
            .expect("renewals lock is not poisoned")
            .clone()
    }
}

impl OperationLeaseRenewer for RecordingLeaseRenewer {
    async fn renew_operation_lease(
        &mut self,
        operation_id: &OperationId,
    ) -> Result<
        Option<ployz_core::ops::OperationOwnerLease>,
        ployz_nats::operations::OperationStatusStoreError,
    > {
        self.renewals
            .lock()
            .expect("renewals lock is not poisoned")
            .push(operation_id.clone());
        Ok(None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeployContainerForAssert {
    node_id: NodeId,
    container_id: ContainerId,
    required_endpoint_port: Option<RoutePort>,
}

impl DeployContainerForAssert {
    pub(super) fn new(node_id: &str, container_id: &str) -> Self {
        Self::from_container(
            self::node_id(node_id),
            self::container_id(container_id),
            None,
        )
    }

    pub(super) fn routed(node_id: &str, container_id: &str) -> Self {
        Self::from_container(
            self::node_id(node_id),
            self::container_id(container_id),
            Some(route_port(8080)),
        )
    }

    const fn from_container(
        node_id: NodeId,
        container_id: ContainerId,
        required_endpoint_port: Option<RoutePort>,
    ) -> Self {
        Self {
            node_id,
            container_id,
            required_endpoint_port,
        }
    }
}

impl RecordingRuntime {
    pub(super) fn with_containers<const N: usize>(containers: [&str; N]) -> Self {
        Self {
            requests: Vec::new(),
            removals: Vec::new(),
            containers: containers.into_iter().map(container_id).rev().collect(),
            fail_after_first: false,
            reuse_existing: false,
            fail_start: false,
            fail_remove: false,
        }
    }

    pub(super) fn reusing_containers<const N: usize>(containers: [&str; N]) -> Self {
        Self {
            requests: Vec::new(),
            removals: Vec::new(),
            containers: containers.into_iter().map(container_id).rev().collect(),
            fail_after_first: false,
            reuse_existing: true,
            fail_start: false,
            fail_remove: false,
        }
    }

    pub(super) fn failing_after_first_container() -> Self {
        Self {
            requests: Vec::new(),
            removals: Vec::new(),
            containers: vec![container_id("ctr_1")],
            fail_after_first: true,
            reuse_existing: false,
            fail_start: false,
            fail_remove: false,
        }
    }

    pub(super) fn failing_start(container_id: &str) -> Self {
        Self {
            requests: Vec::new(),
            removals: Vec::new(),
            containers: vec![self::container_id(container_id)],
            fail_after_first: false,
            reuse_existing: false,
            fail_start: true,
            fail_remove: false,
        }
    }

    pub(super) fn with_remove_failure(mut self) -> Self {
        self.fail_remove = true;
        self
    }
}

impl NodeContainerRuntime for RecordingRuntime {
    async fn run_container(
        &mut self,
        request: NodeRunContainerRequest,
    ) -> Result<NodeRunContainerOutcome, NodeContainerRuntimeError> {
        self.requests.push(request.clone());
        if self.fail_after_first && self.requests.len() > 1 {
            return Err(NodeContainerRuntimeError::Unavailable {
                node_id: request.node_id,
                reason: NodeRuntimeUnavailableReason::RequestFailed {
                    message: "synthetic runtime failure".to_owned(),
                },
            });
        }

        let Some(container_id) = self.containers.pop() else {
            return Err(NodeContainerRuntimeError::Unavailable {
                node_id: request.node_id,
                reason: NodeRuntimeUnavailableReason::RequestFailed {
                    message: "synthetic missing container id".to_owned(),
                },
            });
        };

        if self.fail_start {
            return Err(NodeContainerRuntimeError::CreatedContainerStartFailed {
                node_id: request.node_id,
                container_id,
                message: runtime_failure_message("container start failed: exec format error"),
                inspect_hint: inspect_hint("ctr_created"),
            });
        }

        if self.reuse_existing {
            Ok(NodeRunContainerOutcome::ReusedRunning { container_id })
        } else {
            Ok(NodeRunContainerOutcome::Created { container_id })
        }
    }

    async fn remove_container(
        &mut self,
        request: NodeRemoveContainerRequest,
    ) -> Result<(), NodeContainerRuntimeError> {
        self.removals.push(request.clone());
        if self.fail_remove {
            return Err(NodeContainerRuntimeError::RemoveContainerFailed {
                node_id: request.node_id,
                container_id: request.container_id.clone(),
                message: runtime_failure_message("container remove failed: busy"),
                inspect_hint: OperatorHint::try_new(format!(
                    "ployz container inspect {}",
                    request.container_id.as_str()
                ))
                .expect("valid inspect hint"),
            });
        }

        Ok(())
    }
}

pub(super) fn deploy_command(replicas: u16) -> DeployExecutionCommand {
    prepared_deploy_command(
        replicas,
        vec![node_id("node_a"), node_id("node_b")],
        Vec::new(),
    )
}

pub(super) fn routed_deploy_command(replicas: u16) -> DeployExecutionCommand {
    prepare_deploy_execution_command(
        operation_id("op_123"),
        DeployRequest {
            service_id: service_id("svc_api"),
            target_revision: revision_id("rev_2"),
            image: image("registry.example/api:rev_2"),
            replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
            route: Some(DeployRoute {
                target: route_target("api.example.com", 443),
                endpoint_port: route_port(8080),
            }),
        },
        DeployExecutionFacts {
            active_service: None,
            active_route: None,
            eligible_nodes: vec![node_id("node_a"), node_id("node_b")],
            observed_nodes: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
    .expect("routed deploy command preparation succeeds")
}

pub(super) fn deploy_command_without_eligible_nodes(replicas: u16) -> DeployExecutionCommand {
    prepared_deploy_command(replicas, Vec::new(), Vec::new())
}

pub(super) fn deploy_command_with_existing_container(
    replicas: u16,
    node_id: &str,
    container_id: &str,
) -> DeployExecutionCommand {
    let snapshot = NodeContainerObservationSnapshot::try_new(
        self::node_id(node_id),
        [observed_service_container(node_id, container_id, "rev_2")],
    )
    .expect("valid node observation snapshot");
    prepared_deploy_command(
        replicas,
        vec![self::node_id("node_a"), self::node_id("node_b")],
        vec![snapshot],
    )
}

pub(super) fn deploy_command_replacing_old_container(
    replicas: u16,
    node_id: &str,
    container_id: &str,
) -> DeployExecutionCommand {
    let snapshot = NodeContainerObservationSnapshot::try_new(
        self::node_id(node_id),
        [observed_service_container(node_id, container_id, "rev_old")],
    )
    .expect("valid node observation snapshot");
    prepared_deploy_command(
        replicas,
        vec![self::node_id("node_a"), self::node_id("node_b")],
        vec![snapshot],
    )
}

fn prepared_deploy_command(
    replicas: u16,
    eligible_nodes: Vec<NodeId>,
    observed_nodes: Vec<NodeContainerObservationSnapshot>,
) -> DeployExecutionCommand {
    prepare_deploy_execution_command(
        operation_id("op_123"),
        DeployRequest {
            service_id: service_id("svc_api"),
            target_revision: revision_id("rev_2"),
            image: image("registry.example/api:rev_2"),
            replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
            route: None,
        },
        DeployExecutionFacts {
            active_service: None,
            active_route: None,
            eligible_nodes,
            observed_nodes,
            step_timeout: Duration::from_secs(5),
        },
    )
    .expect("deploy command preparation succeeds")
}

fn observed_service_container(
    node_id: &str,
    container_id: &str,
    revision_id: &str,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        node_id: self::node_id(node_id),
        container_id: self::container_id(container_id),
        service_id: service_id("svc_api"),
        revision_id: self::revision_id(revision_id),
        operation_id: operation_id("op_existing"),
        step_id: StepId::try_new(format!("existing_{container_id}")).expect("valid step id"),
        kind: ManagedContainerKind::Service,
        state: ContainerRuntimeState::running_unroutable(),
    }
}

pub(super) fn active_service_running() -> DeployRunningStage {
    DeployRunningStage::ActiveServiceCommit
}

pub(super) fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

pub(super) fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

pub(super) fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

pub(super) fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

pub(super) fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

pub(super) fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image")
}

pub(super) fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget {
        hostname: RouteHostname::try_new(hostname).expect("valid route hostname"),
        port: route_port(port),
    }
}

pub(super) fn route_port(port: u16) -> RoutePort {
    RoutePort::try_new(port).expect("valid route port")
}

pub(super) fn retained_container(node_id: &str, container_id: &str) -> RetainedArtifact {
    RetainedArtifact::StartedContainer {
        node_id: self::node_id(node_id),
        container_id: self::container_id(container_id),
        log_hint: OperatorHint::try_new(format!("ployz logs {container_id}"))
            .expect("valid log hint"),
    }
}

pub(super) fn cleanup_container(
    node_id: &str,
    container_id: &str,
    revision_id: &str,
) -> DeployCleanupContainer {
    DeployCleanupContainer {
        node_id: self::node_id(node_id),
        container_id: self::container_id(container_id),
        service_id: service_id("svc_api"),
        revision_id: self::revision_id(revision_id),
        operation_id: operation_id("op_existing"),
        step_id: StepId::try_new(format!("existing_{container_id}")).expect("valid step id"),
        kind: ManagedContainerKind::Service,
        endpoint_port: None,
    }
}

pub(super) fn retained_created_container(node_id: &str, container_id: &str) -> RetainedArtifact {
    RetainedArtifact::CreatedContainer {
        node_id: self::node_id(node_id),
        container_id: self::container_id(container_id),
        inspect_hint: inspect_hint(container_id),
    }
}

pub(super) fn inspect_hint(container_id: &str) -> OperatorHint {
    OperatorHint::try_new(format!("ployz container inspect {container_id}"))
        .expect("valid inspect hint")
}

fn runtime_failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("valid failure message")
}
