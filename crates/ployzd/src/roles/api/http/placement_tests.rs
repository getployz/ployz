use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use ployz_core::corrosion::{
    CorrosionDeployFailure, CorrosionDeployOutcome, CorrosionDeployServiceFailure,
    CorrosionDeployServiceResult, CorrosionDeployServiceResultKind, CorrosionDeployTargets,
    CorrosionDeployTransition, CorrosionDocumentVersion, CorrosionTimestamp, MachineLoadBand,
    OperationDocument, Principal, V2ManagedContainerIdentity,
};
use ployz_core::deploy::{ImageReference, RegistryCredential, VolumeName};
use ployz_core::ids::{
    ClusterId, ContainerId, MachineRowId, NamespaceRowId, OperationRowId, PeerId, ServiceRowId,
};
use ployz_core::machine::runtime::ContainerHealth;
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::{DeployExecuteOutcome, DeployExecuteRequest, DeployVerb, PlacementBidRequest};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::watch;

use super::super::deploy_runtime::DeployRuntime;
use super::{BidIdentity, collect_bid, execute_verb};
use crate::roles::api::http::deploy_stores::DeployOperationRows;
use crate::roles::api::http::operation_store::{
    ConditionalOperationWrite, HeartbeatWrite, ObservedOperation, OperationStoreError,
};
use crate::roles::api::runner::{
    CreateV2ManagedContainer, ExistingManagedContainerState, ExistingV2ManagedContainer,
    MachineContainerStopOutcome,
};
use crate::roles::system_observation::SystemObservation;

fn cluster_id() -> ClusterId {
    ClusterId::try_new("01J00000000000000000000010").expect("cluster id")
}

fn namespace_id() -> NamespaceRowId {
    NamespaceRowId::try_new("01J00000000000000000000011").expect("namespace id")
}

fn driver_machine_id() -> MachineRowId {
    MachineRowId::try_new("01J00000000000000000000012").expect("machine id")
}

fn other_machine_id() -> MachineRowId {
    MachineRowId::try_new("01J00000000000000000000013").expect("machine id")
}

fn service_id() -> ServiceRowId {
    ServiceRowId::try_new("01J00000000000000000000014").expect("service id")
}

fn other_service_id() -> ServiceRowId {
    ServiceRowId::try_new("01J00000000000000000000015").expect("service id")
}

fn operation_id() -> OperationRowId {
    OperationRowId::try_new("01J00000000000000000000016").expect("operation id")
}

fn initiator() -> Principal {
    Principal::Peer {
        peer_id: PeerId::try_new("01J00000000000000000000017").expect("peer id"),
    }
}

fn machine_caller(machine_id: MachineRowId) -> Principal {
    Principal::Machine { machine_id }
}

fn recent_timestamp() -> CorrosionTimestamp {
    let value = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("format timestamp");
    CorrosionTimestamp::try_new(value).expect("timestamp")
}

fn deploy_claim(
    driver: MachineRowId,
    target: ServiceRowId,
    created_at: CorrosionTimestamp,
) -> OperationDocument {
    OperationDocument::deploy_created(
        CorrosionDocumentVersion::V1,
        cluster_id(),
        driver,
        initiator(),
        namespace_id(),
        CorrosionDeployTargets::try_new(vec![target]).expect("targets"),
        created_at,
    )
}

fn terminal_claim() -> OperationDocument {
    deploy_claim(driver_machine_id(), service_id(), recent_timestamp())
        .transition_deploy(CorrosionDeployTransition::Terminal {
            completed_at: recent_timestamp(),
            outcome: CorrosionDeployOutcome::Failed {
                results: vec![CorrosionDeployServiceResult {
                    service_id: service_id(),
                    result: CorrosionDeployServiceResultKind::Failed {
                        failure: CorrosionDeployServiceFailure::ImagePullFailed {
                            message: "pull failed".to_owned(),
                        },
                    },
                }],
                failure: CorrosionDeployFailure::ServiceFailed {
                    service_id: service_id(),
                    failure: CorrosionDeployServiceFailure::ImagePullFailed {
                        message: "pull failed".to_owned(),
                    },
                },
            },
        })
        .expect("terminal transition")
}

fn container(
    container_id: &str,
    service: ServiceRowId,
    operation: OperationRowId,
    running: bool,
    volumes: &[&str],
) -> ExistingV2ManagedContainer {
    ExistingV2ManagedContainer {
        container_id: ContainerId::try_new(container_id).expect("container id"),
        identity: V2ManagedContainerIdentity {
            namespace_id: namespace_id(),
            service_id: service,
            operation_id: operation,
        },
        state: if running {
            ExistingManagedContainerState::Running {
                ip: Some(IpAddr::V4(Ipv4Addr::new(10, 210, 20, 9))),
                health: ContainerHealth::None,
                started_at_unix_ms: None,
            }
        } else {
            ExistingManagedContainerState::StartableStopped
        },
        health_status: None,
        resolved_image_identity: None,
        created_at_unix_seconds: None,
        named_volume_names: volumes
            .iter()
            .map(|name| VolumeName::try_new(*name).expect("volume name"))
            .collect(),
    }
}

struct FakeRunner {
    containers: Result<Vec<ExistingV2ManagedContainer>, String>,
    held: BTreeSet<VolumeName>,
    started: Mutex<Vec<ContainerId>>,
    stopped: Mutex<Vec<ContainerId>>,
}

impl FakeRunner {
    fn with_containers(containers: Vec<ExistingV2ManagedContainer>) -> Self {
        Self {
            containers: Ok(containers),
            held: BTreeSet::new(),
            started: Mutex::new(Vec::new()),
            stopped: Mutex::new(Vec::new()),
        }
    }

    fn failing(diagnostic: &str) -> Self {
        Self {
            containers: Err(diagnostic.to_owned()),
            held: BTreeSet::new(),
            started: Mutex::new(Vec::new()),
            stopped: Mutex::new(Vec::new()),
        }
    }

    fn holding(mut self, volumes: &[&str]) -> Self {
        self.held = volumes
            .iter()
            .map(|name| VolumeName::try_new(*name).expect("volume name"))
            .collect();
        self
    }
}

#[async_trait]
impl DeployRuntime for FakeRunner {
    async fn bridge_ready(&self) -> bool {
        unreachable!("bridge_ready is not exercised by these tests")
    }

    async fn resolve_image(&self, _image: &ImageReference) -> Result<ImageReference, String> {
        unreachable!("resolve_image is not exercised by these tests")
    }

    async fn managed_containers(&self) -> Result<Vec<ExistingV2ManagedContainer>, String> {
        self.containers.clone()
    }

    async fn held_volumes(
        &self,
        _namespace_id: &NamespaceRowId,
        volumes: &BTreeSet<VolumeName>,
    ) -> Result<BTreeSet<VolumeName>, String> {
        Ok(self.held.intersection(volumes).cloned().collect())
    }

    async fn pull_image(
        &self,
        _image: &ImageReference,
        _credential: Option<&RegistryCredential>,
        _shutdown: watch::Receiver<bool>,
    ) -> Result<(), String> {
        unreachable!("pull_image is not exercised by these tests")
    }

    async fn create_container_command(
        &self,
        _command: CreateV2ManagedContainer,
    ) -> Result<ContainerId, String> {
        unreachable!("create_container_command is not exercised by these tests")
    }

    async fn start_container(&self, container_id: &ContainerId) -> Result<(), String> {
        self.started
            .lock()
            .expect("started lock")
            .push(container_id.clone());
        Ok(())
    }

    async fn stop_container(
        &self,
        container_id: &ContainerId,
        _expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<MachineContainerStopOutcome, String> {
        self.stopped
            .lock()
            .expect("stopped lock")
            .push(container_id.clone());
        Ok(MachineContainerStopOutcome::StoppedRunning)
    }

    async fn remove_container(
        &self,
        _container_id: &ContainerId,
        _expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<(), String> {
        unreachable!("remove_container is not exercised by these tests")
    }

    async fn health_gate(
        &self,
        _container_id: &ContainerId,
        _identity: &V2ManagedContainerIdentity,
    ) -> Result<Ipv4Addr, String> {
        unreachable!("health_gate is not exercised by these tests")
    }

    async fn container_ip(
        &self,
        _container_id: &ContainerId,
        _identity: &V2ManagedContainerIdentity,
    ) -> Result<Ipv4Addr, String> {
        unreachable!("container_ip is not exercised by these tests")
    }
}

struct FakeRows {
    candidates: Vec<(OperationRowId, OperationDocument)>,
    operations: HashMap<OperationRowId, OperationDocument>,
    candidate_reads: AtomicUsize,
}

impl FakeRows {
    fn empty() -> Self {
        Self {
            candidates: Vec::new(),
            operations: HashMap::new(),
            candidate_reads: AtomicUsize::new(0),
        }
    }

    fn with_candidate(operation: OperationRowId, document: OperationDocument) -> Self {
        let mut rows = Self::empty();
        rows.operations.insert(operation.clone(), document.clone());
        rows.candidates.push((operation, document));
        rows
    }

    fn with_operation_only(operation: OperationRowId, document: OperationDocument) -> Self {
        let mut rows = Self::empty();
        rows.operations.insert(operation, document);
        rows
    }
}

#[async_trait]
impl DeployOperationRows for FakeRows {
    async fn insert_created(
        &self,
        _operation_id: &OperationRowId,
        _operation: &OperationDocument,
    ) -> Result<(), OperationStoreError> {
        unreachable!("insert_created is not exercised by these tests")
    }

    async fn operation(
        &self,
        operation_id: &OperationRowId,
    ) -> Result<Option<ObservedOperation>, OperationStoreError> {
        Ok(self
            .operations
            .get(operation_id)
            .map(|document| ObservedOperation {
                id: operation_id.clone(),
                exact_document: serde_json::to_string(document).expect("encode operation"),
                document: document.clone(),
            }))
    }

    async fn transition_deploy(
        &self,
        _observed: ObservedOperation,
        _transition: CorrosionDeployTransition,
    ) -> Result<ConditionalOperationWrite, OperationStoreError> {
        unreachable!("transition_deploy is not exercised by these tests")
    }

    async fn replace_terminal(
        &self,
        _observed: &ObservedOperation,
        _terminal: &OperationDocument,
    ) -> Result<ConditionalOperationWrite, OperationStoreError> {
        unreachable!("replace_terminal is not exercised by these tests")
    }

    async fn refresh_heartbeat(
        &self,
        _observed: &ObservedOperation,
        _now: CorrosionTimestamp,
    ) -> Result<HeartbeatWrite, OperationStoreError> {
        unreachable!("refresh_heartbeat is not exercised by these tests")
    }

    async fn deploy_claim_candidates(
        &self,
        service_id: &ServiceRowId,
    ) -> Result<Vec<(OperationRowId, OperationDocument)>, OperationStoreError> {
        self.candidate_reads.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .candidates
            .iter()
            .filter(|(_, document)| {
                document
                    .deploy_targets()
                    .is_some_and(|targets| targets.as_slice().contains(service_id))
            })
            .cloned()
            .collect())
    }

    async fn deploy_takeover_candidates(
        &self,
        _operation_id: &OperationRowId,
        _service_id: &ServiceRowId,
    ) -> Result<Vec<(OperationRowId, OperationDocument)>, OperationStoreError> {
        unreachable!("deploy_takeover_candidates is not exercised by these tests")
    }
}

fn bid_request(volumes: &[&str]) -> PlacementBidRequest {
    PlacementBidRequest {
        namespace_id: namespace_id(),
        service_id: service_id(),
        volumes: volumes
            .iter()
            .map(|name| VolumeName::try_new(*name).expect("volume name"))
            .collect(),
        active_deploy: None,
    }
}

fn bid_identity() -> BidIdentity {
    BidIdentity {
        machine_id: driver_machine_id(),
        machine_name: MachineName::try_new("alpha").expect("machine name"),
        lifecycle: MachineLifecycle::Active,
    }
}

fn observation() -> SystemObservation {
    SystemObservation {
        free_disk_bytes: 11,
        free_memory_bytes: 22,
        load: MachineLoadBand::Normal,
    }
}

fn execute_request(verb: DeployVerb) -> DeployExecuteRequest {
    DeployExecuteRequest {
        operation_id: operation_id(),
        namespace_id: namespace_id(),
        service_id: service_id(),
        verb,
    }
}

fn no_shutdown() -> watch::Receiver<bool> {
    let (_sender, receiver) = watch::channel(false);
    receiver
}

#[tokio::test]
async fn bid_reflects_live_containers_and_held_volumes() {
    let runner = FakeRunner::with_containers(vec![
        container(
            "aaaaaaaaaaaa",
            service_id(),
            operation_id(),
            true,
            &["data"],
        ),
        container(
            "bbbbbbbbbbbb",
            other_service_id(),
            operation_id(),
            false,
            &[],
        ),
    ])
    .holding(&["data"]);

    let bid = collect_bid(
        &runner,
        bid_identity(),
        observation(),
        &bid_request(&["data", "logs"]),
    )
    .await
    .expect("bid");

    assert_eq!(bid.machine_id, driver_machine_id());
    assert_eq!(bid.machine_name.as_str(), "alpha");
    assert_eq!(bid.architecture, std::env::consts::ARCH);
    assert_eq!(bid.lifecycle, MachineLifecycle::Active);
    assert_eq!(bid.free_disk_bytes, 11);
    assert_eq!(bid.free_memory_bytes, 22);
    assert_eq!(bid.load, MachineLoadBand::Normal);
    assert_eq!(bid.total_container_count, 2);
    // The observation scope is the namespace: a sibling service id in the
    // same namespace (a failed first attempt's dead id) is still reported.
    let [observed, sibling] = bid.service_containers.as_slice() else {
        panic!("both namespace containers are observed");
    };
    assert_eq!(observed.container_id.as_str(), "aaaaaaaaaaaa");
    assert_eq!(observed.service_id, service_id());
    assert_eq!(observed.deploy, operation_id());
    assert_eq!(
        observed.named_volumes,
        BTreeSet::from([VolumeName::try_new("data").expect("volume name")])
    );
    assert_eq!(sibling.container_id.as_str(), "bbbbbbbbbbbb");
    assert_eq!(sibling.service_id, other_service_id());
    assert_eq!(
        bid.volumes_held,
        BTreeSet::from([VolumeName::try_new("data").expect("volume name")])
    );
}

#[tokio::test]
async fn bid_collection_surfaces_docker_failure_as_typed_error() {
    let runner = FakeRunner::failing("docker daemon unreachable");

    let error = collect_bid(&runner, bid_identity(), observation(), &bid_request(&[]))
        .await
        .expect_err("docker failure declines the bid");

    assert!(error.contains("docker daemon unreachable"));
}

#[tokio::test]
async fn execute_refuses_a_caller_that_is_not_the_driver() {
    let rows = FakeRows::with_candidate(
        operation_id(),
        deploy_claim(driver_machine_id(), service_id(), recent_timestamp()),
    );
    let runner = FakeRunner::with_containers(Vec::new());

    let outcome = execute_verb(
        &rows,
        &runner,
        &machine_caller(other_machine_id()),
        execute_request(DeployVerb::ListServiceContainers),
        no_shutdown(),
    )
    .await;

    assert_eq!(
        outcome,
        DeployExecuteOutcome::CallerNotDriver {
            driver: driver_machine_id()
        }
    );
}

#[tokio::test]
async fn execute_refuses_a_non_machine_caller() {
    let rows = FakeRows::with_candidate(
        operation_id(),
        deploy_claim(driver_machine_id(), service_id(), recent_timestamp()),
    );
    let runner = FakeRunner::with_containers(Vec::new());

    let outcome = execute_verb(
        &rows,
        &runner,
        &initiator(),
        execute_request(DeployVerb::ListServiceContainers),
        no_shutdown(),
    )
    .await;

    assert_eq!(
        outcome,
        DeployExecuteOutcome::CallerNotDriver {
            driver: driver_machine_id()
        }
    );
}

#[tokio::test]
async fn execute_refuses_a_terminal_claim() {
    let rows = FakeRows::with_operation_only(operation_id(), terminal_claim());
    let runner = FakeRunner::with_containers(Vec::new());

    let outcome = execute_verb(
        &rows,
        &runner,
        &machine_caller(driver_machine_id()),
        execute_request(DeployVerb::ListServiceContainers),
        no_shutdown(),
    )
    .await;

    assert_eq!(outcome, DeployExecuteOutcome::ClaimTerminal);
    assert_eq!(rows.candidate_reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn execute_refuses_an_operation_for_another_service() {
    let rows = FakeRows::with_candidate(
        operation_id(),
        deploy_claim(driver_machine_id(), other_service_id(), recent_timestamp()),
    );
    let runner = FakeRunner::with_containers(Vec::new());

    let outcome = execute_verb(
        &rows,
        &runner,
        &machine_caller(driver_machine_id()),
        execute_request(DeployVerb::ListServiceContainers),
        no_shutdown(),
    )
    .await;

    assert_eq!(outcome, DeployExecuteOutcome::ServiceMismatch);
}

#[tokio::test]
async fn execute_refuses_a_namespace_that_does_not_match_the_claim() {
    let rows = FakeRows::with_candidate(
        operation_id(),
        deploy_claim(driver_machine_id(), service_id(), recent_timestamp()),
    );
    let runner = FakeRunner::with_containers(Vec::new());
    let mut request = execute_request(DeployVerb::ListServiceContainers);
    request.namespace_id =
        NamespaceRowId::try_new("01J00000000000000000000019").expect("namespace id");

    let outcome = execute_verb(
        &rows,
        &runner,
        &machine_caller(driver_machine_id()),
        request,
        no_shutdown(),
    )
    .await;

    assert_eq!(outcome, DeployExecuteOutcome::ServiceMismatch);
}

#[tokio::test(start_paused = true)]
async fn execute_answers_claim_not_yet_visible_after_a_bounded_wait() {
    let rows = FakeRows::empty();
    let runner = FakeRunner::with_containers(Vec::new());

    let outcome = execute_verb(
        &rows,
        &runner,
        &machine_caller(driver_machine_id()),
        execute_request(DeployVerb::ListServiceContainers),
        no_shutdown(),
    )
    .await;

    assert_eq!(outcome, DeployExecuteOutcome::ClaimNotYetVisible);
    assert!(
        rows.candidate_reads.load(Ordering::SeqCst) > 1,
        "convergence lag is re-queried, never judged from one read"
    );
}

#[tokio::test(start_paused = true)]
async fn execute_treats_a_stale_heartbeat_as_convergence_lag() {
    let rows = FakeRows::with_candidate(
        operation_id(),
        deploy_claim(
            driver_machine_id(),
            service_id(),
            CorrosionTimestamp::try_new("2020-01-01T00:00:00Z").expect("timestamp"),
        ),
    );
    let runner = FakeRunner::with_containers(Vec::new());

    let outcome = execute_verb(
        &rows,
        &runner,
        &machine_caller(driver_machine_id()),
        execute_request(DeployVerb::ListServiceContainers),
        no_shutdown(),
    )
    .await;

    assert_eq!(outcome, DeployExecuteOutcome::ClaimNotYetVisible);
}

#[tokio::test]
async fn execute_dispatches_an_authorized_list_verb() {
    let rows = FakeRows::with_candidate(
        operation_id(),
        deploy_claim(driver_machine_id(), service_id(), recent_timestamp()),
    );
    let runner = FakeRunner::with_containers(vec![
        container("aaaaaaaaaaaa", service_id(), operation_id(), true, &[]),
        container(
            "bbbbbbbbbbbb",
            other_service_id(),
            operation_id(),
            true,
            &[],
        ),
    ]);

    let outcome = execute_verb(
        &rows,
        &runner,
        &machine_caller(driver_machine_id()),
        execute_request(DeployVerb::ListServiceContainers),
        no_shutdown(),
    )
    .await;

    let DeployExecuteOutcome::ServiceContainers { containers } = outcome else {
        panic!("authorized list verb answers service containers");
    };
    let [observed, sibling] = containers.as_slice() else {
        panic!("every container in the claim's namespace is listed");
    };
    assert_eq!(observed.container_id.as_str(), "aaaaaaaaaaaa");
    assert_eq!(sibling.container_id.as_str(), "bbbbbbbbbbbb");
    assert_eq!(sibling.service_id, other_service_id());
}

#[tokio::test]
async fn execute_dispatches_an_authorized_start_verb_for_a_service_scoped_container() {
    let rows = FakeRows::with_candidate(
        operation_id(),
        deploy_claim(driver_machine_id(), service_id(), recent_timestamp()),
    );
    let runner = FakeRunner::with_containers(vec![container(
        "aaaaaaaaaaaa",
        service_id(),
        operation_id(),
        false,
        &[],
    )]);

    let outcome = execute_verb(
        &rows,
        &runner,
        &machine_caller(driver_machine_id()),
        execute_request(DeployVerb::StartContainer {
            container_id: ContainerId::try_new("aaaaaaaaaaaa").expect("container id"),
        }),
        no_shutdown(),
    )
    .await;

    assert_eq!(outcome, DeployExecuteOutcome::ContainerStarted);
    assert_eq!(
        runner.started.lock().expect("started lock").as_slice(),
        &[ContainerId::try_new("aaaaaaaaaaaa").expect("container id")]
    );
}

#[tokio::test]
async fn start_refuses_a_container_outside_the_verbs_service_scope() {
    let rows = FakeRows::with_candidate(
        operation_id(),
        deploy_claim(driver_machine_id(), service_id(), recent_timestamp()),
    );
    let runner = FakeRunner::with_containers(vec![container(
        "cccccccccccc",
        other_service_id(),
        operation_id(),
        false,
        &[],
    )]);

    let outcome = execute_verb(
        &rows,
        &runner,
        &machine_caller(driver_machine_id()),
        execute_request(DeployVerb::StartContainer {
            container_id: ContainerId::try_new("cccccccccccc").expect("container id"),
        }),
        no_shutdown(),
    )
    .await;

    let DeployExecuteOutcome::Failed { diagnostic } = outcome else {
        panic!("a foreign container is a typed verb failure");
    };
    assert!(diagnostic.contains("does not belong"));
    assert!(runner.started.lock().expect("started lock").is_empty());
}

#[tokio::test]
async fn container_verbs_never_touch_another_services_containers() {
    let rows = FakeRows::with_candidate(
        operation_id(),
        deploy_claim(driver_machine_id(), service_id(), recent_timestamp()),
    );
    let runner = FakeRunner::with_containers(vec![container(
        "cccccccccccc",
        other_service_id(),
        operation_id(),
        true,
        &[],
    )]);

    let outcome = execute_verb(
        &rows,
        &runner,
        &machine_caller(driver_machine_id()),
        execute_request(DeployVerb::StopContainer {
            container_id: ContainerId::try_new("cccccccccccc").expect("container id"),
            expected: V2ManagedContainerIdentity {
                namespace_id: namespace_id(),
                service_id: service_id(),
                operation_id: operation_id(),
            },
        }),
        no_shutdown(),
    )
    .await;

    assert_eq!(
        outcome,
        DeployExecuteOutcome::ContainerIdentityMismatch {
            actual: V2ManagedContainerIdentity {
                namespace_id: namespace_id(),
                service_id: other_service_id(),
                operation_id: operation_id(),
            },
        }
    );
    assert!(runner.stopped.lock().expect("stopped lock").is_empty());
}

#[tokio::test]
async fn stop_refuses_a_container_a_newer_deploy_already_replaced() {
    let newer_deploy = OperationRowId::try_new("01J00000000000000000000018").expect("operation id");
    let rows = FakeRows::with_candidate(
        operation_id(),
        deploy_claim(driver_machine_id(), service_id(), recent_timestamp()),
    );
    let runner = FakeRunner::with_containers(vec![container(
        "cccccccccccc",
        service_id(),
        newer_deploy.clone(),
        true,
        &[],
    )]);

    let outcome = execute_verb(
        &rows,
        &runner,
        &machine_caller(driver_machine_id()),
        execute_request(DeployVerb::StopContainer {
            container_id: ContainerId::try_new("cccccccccccc").expect("container id"),
            expected: V2ManagedContainerIdentity {
                namespace_id: namespace_id(),
                service_id: service_id(),
                operation_id: operation_id(),
            },
        }),
        no_shutdown(),
    )
    .await;

    assert_eq!(
        outcome,
        DeployExecuteOutcome::ContainerIdentityMismatch {
            actual: V2ManagedContainerIdentity {
                namespace_id: namespace_id(),
                service_id: service_id(),
                operation_id: newer_deploy,
            },
        }
    );
    assert!(runner.stopped.lock().expect("stopped lock").is_empty());
}

#[tokio::test]
async fn stop_treats_an_absent_container_as_already_settled() {
    let rows = FakeRows::with_candidate(
        operation_id(),
        deploy_claim(driver_machine_id(), service_id(), recent_timestamp()),
    );
    let runner = FakeRunner::with_containers(Vec::new());

    let outcome = execute_verb(
        &rows,
        &runner,
        &machine_caller(driver_machine_id()),
        execute_request(DeployVerb::StopContainer {
            container_id: ContainerId::try_new("cccccccccccc").expect("container id"),
            expected: V2ManagedContainerIdentity {
                namespace_id: namespace_id(),
                service_id: service_id(),
                operation_id: operation_id(),
            },
        }),
        no_shutdown(),
    )
    .await;

    assert_eq!(outcome, DeployExecuteOutcome::ContainerStopped);
    assert!(runner.stopped.lock().expect("stopped lock").is_empty());
}
