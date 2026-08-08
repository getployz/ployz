use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use ployz_core::corrosion::{
    ContainerDocument, CorrosionAutomaticRouteFailure, CorrosionDeployFailure,
    CorrosionDeployOutcome, CorrosionDeployServiceResultKind, CorrosionDeployState,
    CorrosionDeployWarning, CorrosionNamespaceName, CorrosionServiceName, CorrosionTimestamp,
    MachineLoadBand, MachineTransport, NamespaceDocument, OperationDocument, Principal,
    ServiceDocument, ServiceReplicaCount, V2ManagedContainerIdentity,
};
use ployz_core::deploy::{ContainerRuntimeSpec, EnvName, EnvValue, ImageReference, VolumeName};
use ployz_core::ids::{
    ClusterId, ContainerId, MachineRowId, NamespaceRowId, OperationRowId, PeerId, ServiceRowId,
};
use ployz_core::machine::runtime::ContainerHealth;
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::network::MachineEndpointSubnet;
use ployz_core::placement::PlacementRefusal;
use ployz_core::{
    DeployExecuteOutcome, DeployExecuteRequest, DeployRefusal, DeployVerb, HealthGatePolicy,
    OperationEvidence, PlacementBid, PlacementBidRequest, RequestedPins, RequestedPlacement,
    ServiceContainerObservation,
};
use tempfile::TempDir;
use tokio::sync::{Mutex, watch};

use super::{
    DEPLOY_DRAIN_WAIT, DNS_TTL_SECONDS, DeployClock, DeployDriver, DeployDriverSeams, observed_with,
};
use crate::roles::api::http::deploy_dispatch::DeployVerbClient;
use crate::roles::api::http::deploy_runtime::DeployRuntime;
use crate::roles::api::http::deploy_stores::{DeployOperationRows, RedeployStore};
use crate::roles::api::http::deploy_task::DeployHeartbeat;
use crate::roles::api::http::operation_evidence::{
    OperationEvidenceDirectory, PreparedPromotion, PreparedRedeployIntent,
};
use crate::roles::api::http::operation_finalizer::{
    PreparedPromotionStore, PromotionClaimOutcome, PromotionFinalizerStoreError,
    PromotionRequestDisposition, PromotionRowsObservation,
};
use crate::roles::api::http::operation_store::{
    ConditionalOperationWrite, HeartbeatWrite, ObservedOperation, OperationStoreError,
};
use crate::roles::api::http::placement_gather::{PlacementMesh, PlacementPeer};
use crate::roles::api::http::promotion_store::{
    ContainerRowCleanup, DeployAdmission, ObservedContainer, ObservedService, ResolvedNamespace,
};
use crate::roles::api::runner::{
    ExistingManagedContainerState, ExistingV2ManagedContainer, MachineContainerStopOutcome,
};

#[derive(Clone, Copy)]
enum Admission {
    Missing,
    Ambiguous,
    DifferentService,
    MultipleServices,
    RoutesWithoutServices,
    First,
    Redeploy,
}

struct FakeStore {
    admission: Admission,
    prepared: Mutex<Option<PreparedPromotion>>,
    fail_adjudication: AtomicBool,
    adjudication_attempts: AtomicUsize,
    claim_lost: AtomicBool,
    cleanup_attempts: AtomicUsize,
    redeploy_prepared: Mutex<Option<PreparedRedeployIntent>>,
    converge_calls: AtomicUsize,
    rows: Mutex<Vec<ObservedContainer>>,
    deleted: Mutex<Vec<Vec<ContainerId>>>,
}

#[async_trait]
impl RedeployStore for FakeStore {
    async fn resolve_deploy_admission(
        &self,
        _namespace_name: &CorrosionNamespaceName,
        _service_name: &CorrosionServiceName,
    ) -> Result<DeployAdmission, PromotionFinalizerStoreError> {
        Ok(match self.admission {
            Admission::Missing => DeployAdmission::NamespaceMissing,
            Admission::Ambiguous => DeployAdmission::NamespaceAmbiguous {
                namespace_ids: vec![namespace_id()],
            },
            Admission::DifferentService => DeployAdmission::DifferentService {
                namespace_id: namespace_id(),
                incumbent_name: CorrosionServiceName::try_new("web").expect("name"),
            },
            Admission::MultipleServices => DeployAdmission::MultipleServices {
                namespace_id: namespace_id(),
                service_ids: vec![service_id()],
            },
            Admission::RoutesWithoutServices => DeployAdmission::RoutesWithoutServices {
                namespace_id: namespace_id(),
            },
            Admission::First => DeployAdmission::FirstDeploy {
                namespace: resolved_namespace(),
            },
            Admission::Redeploy => DeployAdmission::Redeploy {
                namespace: resolved_namespace(),
                incumbent: Box::new(incumbent_service(machine_id())),
            },
        })
    }

    async fn converge_redeploy_rows(
        &self,
        prepared: &PreparedRedeployIntent,
    ) -> Result<(PromotionRequestDisposition, PromotionRowsObservation), PromotionFinalizerStoreError>
    {
        self.converge_calls.fetch_add(1, Ordering::SeqCst);
        *self.redeploy_prepared.lock().await = Some(prepared.clone());
        Ok((
            PromotionRequestDisposition::Accepted,
            PromotionRowsObservation::EXACT,
        ))
    }

    async fn service_containers(
        &self,
        _service_id: &ServiceRowId,
    ) -> Result<Vec<ObservedContainer>, PromotionFinalizerStoreError> {
        Ok(self.rows.lock().await.clone())
    }

    async fn delete_exact_container_rows(
        &self,
        rows: &[ObservedContainer],
    ) -> Result<Vec<(ContainerId, ContainerRowCleanup)>, PromotionFinalizerStoreError> {
        self.deleted
            .lock()
            .await
            .push(rows.iter().map(|row| row.id.clone()).collect());
        Ok(rows
            .iter()
            .map(|row| (row.id.clone(), ContainerRowCleanup::Removed))
            .collect())
    }
}

#[async_trait]
impl PreparedPromotionStore for FakeStore {
    async fn converge_rows(
        &self,
        prepared: &PreparedPromotion,
    ) -> Result<(PromotionRequestDisposition, PromotionRowsObservation), PromotionFinalizerStoreError>
    {
        *self.prepared.lock().await = Some(prepared.clone());
        Ok((
            PromotionRequestDisposition::Accepted,
            PromotionRowsObservation::EXACT,
        ))
    }

    async fn adjudicate_service_claim(
        &self,
        _prepared: &PreparedPromotion,
    ) -> Result<PromotionClaimOutcome, PromotionFinalizerStoreError> {
        self.adjudication_attempts.fetch_add(1, Ordering::SeqCst);
        if self.fail_adjudication.load(Ordering::SeqCst) {
            return Err(PromotionFinalizerStoreError::Transport(
                "test outage".to_owned(),
            ));
        }
        if self.claim_lost.load(Ordering::SeqCst) {
            return Ok(PromotionClaimOutcome::Lost {
                winner: ServiceRowId::try_new("01J00000000000000000000019").expect("winner"),
            });
        }
        Ok(PromotionClaimOutcome::Won)
    }

    async fn delete_exact_losing_rows(
        &self,
        _prepared: &PreparedPromotion,
    ) -> Result<PromotionRowsObservation, PromotionFinalizerStoreError> {
        self.cleanup_attempts.fetch_add(1, Ordering::SeqCst);
        Ok(PromotionRowsObservation::EXACT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowWrite {
    Created,
    Transition,
    Heartbeat,
}

struct FakeOperations {
    operation: Mutex<Option<OperationDocument>>,
    writes: Mutex<Vec<RowWrite>>,
    claim_candidates: Mutex<Vec<(OperationRowId, OperationDocument)>>,
    takeover_candidates: Mutex<Vec<(OperationRowId, OperationDocument)>>,
    takeover_visible_after: AtomicUsize,
    takeover_calls: AtomicUsize,
    stale_heartbeat: AtomicBool,
}

impl FakeOperations {
    fn new() -> Self {
        Self {
            operation: Mutex::new(None),
            writes: Mutex::new(Vec::new()),
            claim_candidates: Mutex::new(Vec::new()),
            takeover_candidates: Mutex::new(Vec::new()),
            takeover_visible_after: AtomicUsize::new(0),
            takeover_calls: AtomicUsize::new(0),
            stale_heartbeat: AtomicBool::new(false),
        }
    }

    async fn terminal_state(&self) -> CorrosionDeployState {
        let operation = self.operation.lock().await.clone().expect("operation");
        operation.deploy_state().expect("deploy state").clone()
    }
}

#[async_trait]
impl DeployOperationRows for FakeOperations {
    async fn insert_created(
        &self,
        _operation_id: &OperationRowId,
        operation: &OperationDocument,
    ) -> Result<(), OperationStoreError> {
        *self.operation.lock().await = Some(operation.clone());
        self.writes.lock().await.push(RowWrite::Created);
        Ok(())
    }

    async fn operation(
        &self,
        operation_id: &OperationRowId,
    ) -> Result<Option<ObservedOperation>, OperationStoreError> {
        let Some(document) = self.operation.lock().await.clone() else {
            return Ok(None);
        };
        Ok(Some(
            observed_with(operation_id, document).expect("observed row"),
        ))
    }

    async fn transition_deploy(
        &self,
        _observed: ObservedOperation,
        transition: ployz_core::corrosion::CorrosionDeployTransition,
    ) -> Result<ConditionalOperationWrite, OperationStoreError> {
        let mut operation = self.operation.lock().await;
        let current = operation.take().expect("operation row present");
        *operation = Some(
            current
                .transition_deploy(transition)
                .map_err(OperationStoreError::Transition)?,
        );
        self.writes.lock().await.push(RowWrite::Transition);
        Ok(ConditionalOperationWrite::Written)
    }

    async fn replace_terminal(
        &self,
        _observed: &ObservedOperation,
        terminal: &OperationDocument,
    ) -> Result<ConditionalOperationWrite, OperationStoreError> {
        *self.operation.lock().await = Some(terminal.clone());
        self.writes.lock().await.push(RowWrite::Transition);
        Ok(ConditionalOperationWrite::Written)
    }

    async fn refresh_heartbeat(
        &self,
        observed: &ObservedOperation,
        now: CorrosionTimestamp,
    ) -> Result<HeartbeatWrite, OperationStoreError> {
        if self.stale_heartbeat.load(Ordering::SeqCst) {
            return Ok(HeartbeatWrite::Stale);
        }
        let refreshed = observed
            .document
            .clone()
            .refresh_heartbeat(now)
            .map_err(OperationStoreError::Transition)?;
        *self.operation.lock().await = Some(refreshed.clone());
        self.writes.lock().await.push(RowWrite::Heartbeat);
        Ok(HeartbeatWrite::Written(Box::new(
            observed_with(&observed.id, refreshed).expect("observed row"),
        )))
    }

    async fn deploy_claim_candidates(
        &self,
        _service_id: &ServiceRowId,
    ) -> Result<Vec<(OperationRowId, OperationDocument)>, OperationStoreError> {
        Ok(self.claim_candidates.lock().await.clone())
    }

    async fn deploy_takeover_candidates(
        &self,
        _operation_id: &OperationRowId,
        _service_id: &ServiceRowId,
    ) -> Result<Vec<(OperationRowId, OperationDocument)>, OperationStoreError> {
        let calls = self.takeover_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if calls <= self.takeover_visible_after.load(Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        Ok(self.takeover_candidates.lock().await.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeCall {
    Create,
    Start(String),
    Stop(String),
    Remove(String),
}

struct FakeRuntime {
    bridge_ready: AtomicBool,
    bridge_reads: AtomicUsize,
    containers: Mutex<Vec<ExistingV2ManagedContainer>>,
    calls: Mutex<Vec<RuntimeCall>>,
    health_failure: AtomicBool,
}

impl FakeRuntime {
    async fn call_log(&self) -> Vec<RuntimeCall> {
        self.calls.lock().await.clone()
    }

    async fn created(&self) -> usize {
        self.calls
            .lock()
            .await
            .iter()
            .filter(|call| matches!(call, RuntimeCall::Create))
            .count()
    }
}

#[async_trait]
impl DeployRuntime for FakeRuntime {
    async fn bridge_ready(&self) -> bool {
        self.bridge_reads.fetch_add(1, Ordering::SeqCst);
        self.bridge_ready.load(Ordering::SeqCst)
    }

    async fn resolve_image(&self, image: &ImageReference) -> Result<ImageReference, String> {
        image
            .with_digest(
                &ployz_core::image::OciDigest::try_new(format!("sha256:{}", "c".repeat(64)))
                    .expect("digest"),
            )
            .map_err(|error| error.to_string())
    }

    async fn pull_image(
        &self,
        _image: &ImageReference,
        _credential: Option<&ployz_core::deploy::RegistryCredential>,
        _shutdown: watch::Receiver<bool>,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn create_container_command(
        &self,
        command: crate::roles::api::runner::CreateV2ManagedContainer,
    ) -> Result<ContainerId, String> {
        self.calls.lock().await.push(RuntimeCall::Create);
        let container_id =
            ContainerId::try_new("new-container").map_err(|error| error.to_string())?;
        self.containers
            .lock()
            .await
            .push(ExistingV2ManagedContainer {
                container_id: container_id.clone(),
                identity: command.identity,
                state: ExistingManagedContainerState::StartableStopped,
                health_status: None,
                resolved_image_identity: None,
                created_at_unix_seconds: None,
                named_volume_names: BTreeSet::new(),
            });
        Ok(container_id)
    }

    async fn start_container(&self, container_id: &ContainerId) -> Result<(), String> {
        self.calls
            .lock()
            .await
            .push(RuntimeCall::Start(container_id.as_str().to_owned()));
        Ok(())
    }

    async fn health_gate(
        &self,
        _container_id: &ContainerId,
        _identity: &V2ManagedContainerIdentity,
    ) -> Result<Ipv4Addr, String> {
        if self.health_failure.load(Ordering::SeqCst) {
            return Err("unhealthy".to_owned());
        }
        Ok(Ipv4Addr::new(10, 210, 20, 2))
    }

    async fn container_ip(
        &self,
        _container_id: &ContainerId,
        _identity: &V2ManagedContainerIdentity,
    ) -> Result<Ipv4Addr, String> {
        Ok(Ipv4Addr::new(10, 210, 20, 2))
    }

    async fn managed_containers(&self) -> Result<Vec<ExistingV2ManagedContainer>, String> {
        Ok(self.containers.lock().await.clone())
    }

    async fn held_volumes(
        &self,
        _namespace_id: &ployz_core::ids::NamespaceRowId,
        _volumes: &BTreeSet<VolumeName>,
    ) -> Result<BTreeSet<VolumeName>, String> {
        Ok(BTreeSet::new())
    }

    async fn stop_container(
        &self,
        container_id: &ContainerId,
        _expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<MachineContainerStopOutcome, String> {
        self.calls
            .lock()
            .await
            .push(RuntimeCall::Stop(container_id.as_str().to_owned()));
        Ok(MachineContainerStopOutcome::StoppedRunning)
    }

    async fn remove_container(
        &self,
        container_id: &ContainerId,
        _expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<(), String> {
        self.calls
            .lock()
            .await
            .push(RuntimeCall::Remove(container_id.as_str().to_owned()));
        Ok(())
    }
}

/// The gather mesh: the local machine bids from `FakeRuntime`'s containers,
/// remote peers answer scripted bids keyed by their Tailscale address.
struct FakeMesh {
    peers: Mutex<Vec<PlacementPeer>>,
    runtime: Arc<FakeRuntime>,
    local_free_disk: AtomicU64,
    local_volumes_held: Mutex<BTreeSet<VolumeName>>,
    remote_bids: Mutex<BTreeMap<Ipv4Addr, PlacementBid>>,
}

impl FakeMesh {
    fn new(runtime: Arc<FakeRuntime>) -> Self {
        Self {
            peers: Mutex::new(vec![local_peer()]),
            runtime,
            local_free_disk: AtomicU64::new(20 * 1024 * 1024 * 1024),
            local_volumes_held: Mutex::new(BTreeSet::new()),
            remote_bids: Mutex::new(BTreeMap::new()),
        }
    }

    async fn add_remote(&self, peer: PlacementPeer, bid: Option<PlacementBid>) {
        let MachineTransport::Tailscale { ip, .. } = &peer.transport else {
            panic!("fixture remote peers use tailscale transports");
        };
        if let Some(bid) = bid {
            self.remote_bids.lock().await.insert(*ip, bid);
        }
        self.peers.lock().await.push(peer);
    }
}

#[async_trait]
impl PlacementMesh for FakeMesh {
    async fn roster(&self) -> Result<Vec<PlacementPeer>, String> {
        Ok(self.peers.lock().await.clone())
    }

    async fn handshake_age_seconds(&self, _transport: &MachineTransport) -> Option<u64> {
        Some(1)
    }

    async fn remote_bid(
        &self,
        transport: &MachineTransport,
        _request: &PlacementBidRequest,
    ) -> Result<PlacementBid, ployz_core::AnomalousSilenceReason> {
        let MachineTransport::Tailscale { ip, .. } = transport else {
            return Err(ployz_core::AnomalousSilenceReason::TransportFailed);
        };
        self.remote_bids
            .lock()
            .await
            .get(ip)
            .cloned()
            .ok_or(ployz_core::AnomalousSilenceReason::TransportFailed)
    }

    async fn local_bid(
        &self,
        peer: &PlacementPeer,
        request: &PlacementBidRequest,
    ) -> Result<PlacementBid, ployz_core::AnomalousSilenceReason> {
        let containers = self.runtime.containers.lock().await.clone();
        let service_containers = containers
            .iter()
            .filter(|container| container.identity.namespace_id == request.namespace_id)
            .map(|container| ServiceContainerObservation {
                container_id: container.container_id.clone(),
                service_id: container.identity.service_id.clone(),
                deploy: container.identity.operation_id.clone(),
                named_volumes: container.named_volume_names.clone(),
            })
            .collect();
        Ok(PlacementBid {
            machine_id: peer.id.clone(),
            machine_name: peer.name.clone(),
            architecture: "x86_64".to_owned(),
            lifecycle: peer.lifecycle,
            free_disk_bytes: self.local_free_disk.load(Ordering::SeqCst),
            free_memory_bytes: 8 * 1024 * 1024 * 1024,
            load: MachineLoadBand::Idle,
            total_container_count: containers.len(),
            service_containers,
            volumes_held: self.local_volumes_held.lock().await.clone(),
        })
    }
}

/// A scripted `/deploy/execute` client for remote targets.
struct FakeVerbClient {
    requests: Mutex<Vec<DeployExecuteRequest>>,
    created: AtomicUsize,
    remote_containers: Mutex<Vec<ServiceContainerObservation>>,
    refuse_with: Mutex<Option<DeployExecuteOutcome>>,
}

impl FakeVerbClient {
    fn new() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            created: AtomicUsize::new(0),
            remote_containers: Mutex::new(Vec::new()),
            refuse_with: Mutex::new(None),
        }
    }

    async fn verbs(&self) -> Vec<DeployVerb> {
        self.requests
            .lock()
            .await
            .iter()
            .map(|request| request.verb.clone())
            .collect()
    }
}

#[async_trait]
impl DeployVerbClient for FakeVerbClient {
    async fn execute(
        &self,
        _target: SocketAddr,
        request: &DeployExecuteRequest,
        _budget: Duration,
    ) -> Result<DeployExecuteOutcome, String> {
        self.requests.lock().await.push(request.clone());
        if let Some(refusal) = self.refuse_with.lock().await.clone() {
            return Ok(refusal);
        }
        Ok(match &request.verb {
            DeployVerb::ListServiceContainers => DeployExecuteOutcome::ServiceContainers {
                containers: self.remote_containers.lock().await.clone(),
            },
            DeployVerb::PullImage { .. } => DeployExecuteOutcome::ImagePulled,
            DeployVerb::CreateContainer { .. } => {
                let ordinal = self.created.fetch_add(1, Ordering::SeqCst) + 1;
                DeployExecuteOutcome::ContainerCreated {
                    container_id: ContainerId::try_new(format!("remote-{ordinal}"))
                        .expect("container id"),
                }
            }
            DeployVerb::StartContainer { .. } => DeployExecuteOutcome::ContainerStarted,
            DeployVerb::StopContainer { .. } => DeployExecuteOutcome::ContainerStopped,
            DeployVerb::HealthGate { .. } => DeployExecuteOutcome::HealthGated {
                ip: Ipv4Addr::new(10, 210, 30, 2),
            },
            DeployVerb::RemoveContainer { .. } => DeployExecuteOutcome::ContainerRemoved,
        })
    }
}

struct Clock(AtomicUsize);

impl DeployClock for Clock {
    fn now(&self) -> Result<CorrosionTimestamp, String> {
        let tick = self.0.fetch_add(1, Ordering::SeqCst);
        let minute = tick / 60;
        let second = tick % 60;
        CorrosionTimestamp::try_new(format!("2026-08-05T10:{minute:02}:{second:02}Z"))
            .map_err(|error| error.to_string())
    }
}

struct Fixture {
    _root: TempDir,
    driver: DeployDriver,
    store: Arc<FakeStore>,
    operations: Arc<FakeOperations>,
    runtime: Arc<FakeRuntime>,
    mesh: Arc<FakeMesh>,
    verbs: Arc<FakeVerbClient>,
    routes: Arc<RecordingRoutes>,
}

struct RecordingRoutes {
    checks: AtomicUsize,
    ensures: AtomicUsize,
    fail_ensure: AtomicBool,
}

#[async_trait]
impl crate::roles::api::http::routes::DeployRouteBindings for RecordingRoutes {
    async fn check(
        &self,
        _namespace_id: &NamespaceRowId,
        _service_id: &ServiceRowId,
        _service_name: &CorrosionServiceName,
        _provenance: ployz_core::corrosion::OperatorWriteProvenance,
    ) -> Result<(), crate::roles::api::http::routes::AutomaticRouteBindingError> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn ensure(
        &self,
        _namespace_id: &NamespaceRowId,
        _service_id: &ServiceRowId,
        _service_name: &CorrosionServiceName,
        _provenance: ployz_core::corrosion::OperatorWriteProvenance,
    ) -> Result<(), crate::roles::api::http::routes::AutomaticRouteBindingError> {
        self.ensures.fetch_add(1, Ordering::SeqCst);
        if self.fail_ensure.load(Ordering::SeqCst) {
            return Err(
                crate::roles::api::http::routes::AutomaticRouteBindingError::AllocationUnsettled,
            );
        }
        Ok(())
    }
}

fn fixture(admission: Admission, bridge_ready: bool) -> Fixture {
    let root = tempfile::tempdir().expect("evidence root");
    let store = Arc::new(FakeStore {
        admission,
        prepared: Mutex::new(None),
        fail_adjudication: AtomicBool::new(false),
        adjudication_attempts: AtomicUsize::new(0),
        claim_lost: AtomicBool::new(false),
        cleanup_attempts: AtomicUsize::new(0),
        redeploy_prepared: Mutex::new(None),
        converge_calls: AtomicUsize::new(0),
        rows: Mutex::new(Vec::new()),
        deleted: Mutex::new(Vec::new()),
    });
    let operations = Arc::new(FakeOperations::new());
    let runtime = Arc::new(FakeRuntime {
        bridge_ready: AtomicBool::new(bridge_ready),
        bridge_reads: AtomicUsize::new(0),
        containers: Mutex::new(Vec::new()),
        calls: Mutex::new(Vec::new()),
        health_failure: AtomicBool::new(false),
    });
    let mesh = Arc::new(FakeMesh::new(Arc::clone(&runtime)));
    let verbs = Arc::new(FakeVerbClient::new());
    let routes = Arc::new(RecordingRoutes {
        checks: AtomicUsize::new(0),
        ensures: AtomicUsize::new(0),
        fail_ensure: AtomicBool::new(false),
    });
    let driver = DeployDriver::new(
        cluster_id(),
        machine_id(),
        DeployDriverSeams {
            evidence: OperationEvidenceDirectory::new(root.path().to_owned(), 64 * 1024),
            store: store.clone(),
            operations: operations.clone(),
            runtime: runtime.clone(),
            mesh: mesh.clone(),
            verbs: verbs.clone(),
            routes: routes.clone(),
            api_port: 4480,
            clock: Arc::new(Clock(AtomicUsize::new(0))),
        },
    )
    .with_waits(Duration::ZERO, Duration::ZERO);
    Fixture {
        _root: root,
        driver,
        store,
        operations,
        runtime,
        mesh,
        verbs,
        routes,
    }
}

fn cluster_id() -> ClusterId {
    ClusterId::try_new("01J00000000000000000000010").expect("cluster")
}

fn machine_id() -> MachineRowId {
    MachineRowId::try_new("01J00000000000000000000012").expect("machine")
}

fn remote_machine_id() -> MachineRowId {
    MachineRowId::try_new("01J00000000000000000000022").expect("machine")
}

fn namespace_id() -> NamespaceRowId {
    NamespaceRowId::try_new("01J00000000000000000000013").expect("namespace")
}

fn service_id() -> ServiceRowId {
    ServiceRowId::try_new("01J00000000000000000000014").expect("service")
}

fn incumbent_op() -> OperationRowId {
    OperationRowId::try_new("01J00000000000000000000008").expect("operation")
}

fn debris_op() -> OperationRowId {
    OperationRowId::try_new("01J00000000000000000000007").expect("operation")
}

fn local_peer() -> PlacementPeer {
    PlacementPeer {
        id: machine_id(),
        name: MachineName::try_new("driver").expect("name"),
        lifecycle: MachineLifecycle::Active,
        transport: MachineTransport::Tailscale {
            ip: Ipv4Addr::new(100, 64, 0, 1),
            subnet_v4: MachineEndpointSubnet::try_new("10.210.20.0/24").expect("subnet"),
        },
    }
}

fn remote_peer() -> PlacementPeer {
    PlacementPeer {
        id: remote_machine_id(),
        name: MachineName::try_new("worker-1").expect("name"),
        lifecycle: MachineLifecycle::Active,
        transport: MachineTransport::Tailscale {
            ip: Ipv4Addr::new(100, 64, 0, 2),
            subnet_v4: MachineEndpointSubnet::try_new("10.210.30.0/24").expect("subnet"),
        },
    }
}

fn remote_bid(volumes_held: &[&str]) -> PlacementBid {
    PlacementBid {
        machine_id: remote_machine_id(),
        machine_name: MachineName::try_new("worker-1").expect("name"),
        architecture: "x86_64".to_owned(),
        lifecycle: MachineLifecycle::Active,
        free_disk_bytes: 20 * 1024 * 1024 * 1024,
        free_memory_bytes: 8 * 1024 * 1024 * 1024,
        load: MachineLoadBand::Idle,
        total_container_count: 0,
        service_containers: Vec::new(),
        volumes_held: volumes_held
            .iter()
            .map(|name| VolumeName::try_new(*name).expect("volume name"))
            .collect(),
    }
}

fn resolved_namespace() -> ResolvedNamespace {
    let document: NamespaceDocument = serde_json::from_value(serde_json::json!({
        "v": 1,
        "cluster_id": "01J00000000000000000000010",
        "written_by": { "kind": "peer", "peer_id": "01J00000000000000000000015" },
        "written_at": "2026-08-05T09:00:00Z",
        "name": "production"
    }))
    .expect("namespace");
    ResolvedNamespace {
        id: namespace_id(),
        exact_document: serde_json::to_string(&document).expect("namespace json"),
        document,
    }
}

fn incumbent_document(pinned: MachineRowId) -> ServiceDocument {
    serde_json::from_value(serde_json::json!({
        "v": 1,
        "cluster_id": "01J00000000000000000000010",
        "written_by": { "kind": "peer", "peer_id": "01J00000000000000000000015" },
        "written_at": "2026-08-05T09:00:00Z",
        "namespace_id": namespace_id(),
        "name": "api",
        "image": "ghcr.io/acme/api@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "env_fingerprints": {},
        "mode": "replicated",
        "replicas": 1,
        "pinned_machines": [pinned],
        "active_deploy": incumbent_op(),
        "previous_image": null,
        "deployed_at": "2026-08-05T09:00:02Z",
        "operation_id": incumbent_op()
    }))
    .expect("incumbent document")
}

fn incumbent_service(pinned: MachineRowId) -> ObservedService {
    let document = incumbent_document(pinned);
    ObservedService {
        id: service_id(),
        exact_document: serde_json::to_string(&document).expect("incumbent json"),
        document,
    }
}

fn docker_container(
    container_id: &str,
    operation_id: OperationRowId,
    volumes: &[&str],
) -> ExistingV2ManagedContainer {
    ExistingV2ManagedContainer {
        container_id: ContainerId::try_new(container_id).expect("container id"),
        identity: V2ManagedContainerIdentity {
            namespace_id: namespace_id(),
            service_id: service_id(),
            operation_id,
        },
        state: ExistingManagedContainerState::Running {
            ip: Some(IpAddr::V4(Ipv4Addr::new(10, 210, 20, 9))),
            health: ContainerHealth::None,
            started_at_unix_ms: None,
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

fn container_row(container_id: &str, operation_id: OperationRowId) -> ObservedContainer {
    let document: ContainerDocument = serde_json::from_value(serde_json::json!({
        "v": 1,
        "cluster_id": "01J00000000000000000000010",
        "machine_id": "01J00000000000000000000012",
        "service_id": service_id(),
        "namespace_id": namespace_id(),
        "ip": "10.210.20.9",
        "deploy": operation_id
    }))
    .expect("container document");
    ObservedContainer {
        id: ContainerId::try_new(container_id).expect("container id"),
        exact_document: serde_json::to_string(&document).expect("container json"),
        document,
    }
}

fn claim_candidate(id: &str, created_at: &str) -> (OperationRowId, OperationDocument) {
    let document = OperationDocument::deploy_created(
        ployz_core::corrosion::CorrosionDocumentVersion::V1,
        cluster_id(),
        machine_id(),
        Principal::Peer {
            peer_id: PeerId::try_new("01J00000000000000000000015").expect("peer"),
        },
        namespace_id(),
        ployz_core::corrosion::CorrosionDeployTargets::try_new(vec![service_id()])
            .expect("targets"),
        CorrosionTimestamp::try_new(created_at).expect("timestamp"),
    );
    (OperationRowId::try_new(id).expect("operation"), document)
}

fn request() -> ployz_core::DeployRequest {
    request_with(HealthGatePolicy::Enforce)
}

fn request_with(health_gate: HealthGatePolicy) -> ployz_core::DeployRequest {
    let mut environment = BTreeMap::new();
    environment.insert(
        EnvName::try_new("DATABASE_PASSWORD").expect("env name"),
        EnvValue::try_new("do-not-persist-this-secret").expect("env value"),
    );
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.environment = environment.into();
    ployz_core::DeployRequest {
        namespace_name: CorrosionNamespaceName::try_new("production").expect("namespace name"),
        service_name: CorrosionServiceName::try_new("api").expect("service name"),
        image: ImageReference::try_new("nginx:1.27-alpine").expect("image"),
        runtime,
        health_gate,
        placement: None,
        machines: None,
    }
}

fn spread_request(replicas: u16) -> ployz_core::DeployRequest {
    let mut request = request();
    request.placement = Some(RequestedPlacement::Replicated {
        replicas: Some(ServiceReplicaCount::try_new(replicas).expect("replicas")),
    });
    request.machines = Some(RequestedPins::Any);
    request
}

fn volume_request() -> ployz_core::DeployRequest {
    let mut request = request();
    request.runtime.volume_mounts = vec![ployz_core::deploy::ServiceVolumeMount {
        volume_name: VolumeName::try_new("data").expect("volume"),
        target: ployz_core::deploy::ContainerMountPath::try_new("/srv/data").expect("mount"),
    }];
    request
}

fn initiator() -> Principal {
    Principal::Peer {
        peer_id: PeerId::try_new("01J00000000000000000000015").expect("peer"),
    }
}

async fn evidence_kinds(
    log: &crate::roles::api::http::operation_evidence::OperationEvidenceLog,
) -> Vec<OperationEvidence> {
    log.replay_after(0)
        .await
        .expect("replay")
        .into_iter()
        .map(|event| event.evidence)
        .collect()
}

#[test]
fn drain_covers_the_dns_ttl() {
    assert!(DEPLOY_DRAIN_WAIT >= Duration::from_secs(DNS_TTL_SECONDS as u64));
}

#[tokio::test]
async fn missing_namespace_refuses_before_bridge_operation_or_docker_effects() {
    let fixture = fixture(Admission::Missing, true);
    let admission = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission");
    let Err(refusal) = admission else {
        panic!("missing namespace must refuse");
    };
    assert_eq!(
        refusal,
        DeployRefusal::namespace_not_found(
            CorrosionNamespaceName::try_new("production").expect("name")
        )
    );
    assert_eq!(fixture.runtime.bridge_reads.load(Ordering::SeqCst), 0);
    assert!(fixture.operations.writes.lock().await.is_empty());
    assert_eq!(fixture.runtime.created().await, 0);
}

#[tokio::test]
async fn populated_namespace_shapes_refuse_before_effects() {
    for (admission, expected) in [
        (
            Admission::Ambiguous,
            DeployRefusal::NamespaceAmbiguous {
                namespace_name: CorrosionNamespaceName::try_new("production").expect("name"),
                namespace_ids: vec![namespace_id()],
            },
        ),
        (
            Admission::DifferentService,
            DeployRefusal::DifferentService {
                namespace_id: namespace_id(),
                incumbent_service_name: CorrosionServiceName::try_new("web").expect("name"),
            },
        ),
        (
            Admission::MultipleServices,
            DeployRefusal::MultipleServices {
                namespace_id: namespace_id(),
                service_ids: vec![service_id()],
            },
        ),
        (
            Admission::RoutesWithoutServices,
            DeployRefusal::RoutesWithoutServices {
                namespace_id: namespace_id(),
            },
        ),
    ] {
        let fixture = fixture(admission, true);
        let admission = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission");
        let Err(refusal) = admission else {
            panic!("populated namespace must refuse");
        };
        assert_eq!(refusal, expected);
        assert!(fixture.operations.writes.lock().await.is_empty());
        assert_eq!(fixture.runtime.created().await, 0);
    }
}

#[tokio::test]
async fn unavailable_bridge_refuses_before_operation_or_docker_effects() {
    let fixture = fixture(Admission::First, false);
    let admission = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission");
    let Err(refusal) = admission else {
        panic!("unavailable bridge must refuse");
    };
    assert_eq!(refusal, DeployRefusal::BridgeUnavailable);
    assert!(fixture.operations.writes.lock().await.is_empty());
    assert_eq!(fixture.runtime.created().await, 0);
}

#[tokio::test]
async fn unknown_pinned_machine_refuses_at_admission() {
    let fixture = fixture(Admission::First, true);
    let mut request = request();
    request.machines = Some(RequestedPins::Machines {
        names: ployz_core::PinnedMachineNames::try_new([
            MachineName::try_new("ghost").expect("name")
        ])
        .expect("pins"),
    });
    let admission = fixture
        .driver
        .admit(request, initiator())
        .await
        .expect("admission");
    let Err(refusal) = admission else {
        panic!("an unknown pin must refuse");
    };
    assert_eq!(
        refusal,
        DeployRefusal::UnknownPinnedMachine {
            machine_name: MachineName::try_new("ghost").expect("name"),
        }
    );
    assert!(fixture.operations.writes.lock().await.is_empty());
}

#[tokio::test]
async fn pick_refusals_surface_as_typed_deploy_refusals_before_any_operation() {
    let fixture = fixture(Admission::First, true);
    // The only bidder reports less free disk than the placement floor.
    fixture.mesh.local_free_disk.store(1024, Ordering::SeqCst);
    let admission = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission");
    let Err(DeployRefusal::Placement {
        refusal: PlacementRefusal::NoEligibleMachines { eliminations },
    }) = admission
    else {
        panic!("an empty survivor set must refuse");
    };
    assert_eq!(eliminations.len(), 1);
    assert!(fixture.operations.writes.lock().await.is_empty());
    assert_eq!(fixture.runtime.created().await, 0);
}

#[tokio::test]
async fn two_visible_volume_holders_refuse_as_a_data_fork() {
    let fixture = fixture(Admission::Redeploy, true);
    fixture
        .mesh
        .local_volumes_held
        .lock()
        .await
        .insert(VolumeName::try_new("data").expect("volume"));
    fixture
        .mesh
        .add_remote(remote_peer(), Some(remote_bid(&["data"])))
        .await;
    let mut request = volume_request();
    request.machines = Some(RequestedPins::Any);
    let admission = fixture
        .driver
        .admit(request, initiator())
        .await
        .expect("admission");
    let Err(DeployRefusal::Placement {
        refusal: PlacementRefusal::VolumeHolderConflict { volume, holders },
    }) = admission
    else {
        panic!("two visible holders must refuse");
    };
    assert_eq!(volume, VolumeName::try_new("data").expect("volume"));
    assert_eq!(holders.len(), 2);
    assert!(fixture.operations.writes.lock().await.is_empty());
}

#[tokio::test]
async fn a_silent_plausible_volume_holder_refuses_even_with_a_visible_holder() {
    let fixture = fixture(Admission::Redeploy, true);
    fixture
        .mesh
        .local_volumes_held
        .lock()
        .await
        .insert(VolumeName::try_new("data").expect("volume"));
    // The remote peer is rostered and pinned but yields no bid.
    fixture.mesh.add_remote(remote_peer(), None).await;
    let mut request = volume_request();
    request.machines = Some(RequestedPins::Machines {
        names: ployz_core::PinnedMachineNames::try_new([
            MachineName::try_new("driver").expect("name"),
            MachineName::try_new("worker-1").expect("name"),
        ])
        .expect("pins"),
    });
    let admission = fixture
        .driver
        .admit(request, initiator())
        .await
        .expect("admission");
    let Err(DeployRefusal::Placement {
        refusal: PlacementRefusal::DarkVolumeHolder { machines },
    }) = admission
    else {
        panic!("a dark plausible holder must refuse");
    };
    let [dark] = machines.as_slice() else {
        panic!("exactly one dark holder expected");
    };
    assert_eq!(dark.machine_id, remote_machine_id());
    assert_eq!(dark.machine_name.as_str(), "worker-1");
}

#[tokio::test]
async fn admitted_task_rejected_by_shutdown_terminalizes_without_running() {
    let fixture = fixture(Admission::First, true);
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");

    accepted
        .interrupt_unspawned()
        .await
        .expect("interrupted terminal");

    assert_eq!(
        fixture.operations.writes.lock().await.as_slice(),
        [RowWrite::Created, RowWrite::Transition]
    );
    assert_eq!(fixture.runtime.created().await, 0);
    let operation = fixture
        .operations
        .operation
        .lock()
        .await
        .clone()
        .expect("operation");
    assert!(operation.is_terminal());
}

#[tokio::test]
async fn successful_first_deploy_uses_three_row_writes_and_persists_no_secret() {
    let fixture = fixture(Admission::First, true);
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let log = accepted.operation_log();
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("deploy");
    let writes: Vec<RowWrite> = fixture
        .operations
        .writes
        .lock()
        .await
        .iter()
        .copied()
        .filter(|write| *write != RowWrite::Heartbeat)
        .collect();
    assert_eq!(
        writes,
        [
            RowWrite::Created,
            RowWrite::Transition,
            RowWrite::Transition
        ]
    );
    assert_eq!(fixture.runtime.created().await, 1);
    assert_eq!(fixture.routes.checks.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.routes.ensures.load(Ordering::SeqCst), 1);
    let events = evidence_kinds(&log).await;
    assert!(events.contains(&OperationEvidence::OpClaimWon));
    // The gather and the pick replay from the evidence log.
    assert!(events.iter().any(
        |event| matches!(event, OperationEvidence::PlacementGathered { bids, silent }
                if bids.len() == 1 && silent.is_empty())
    ));
    assert!(events.iter().any(
        |event| matches!(event, OperationEvidence::PlacementPicked { pick }
                if pick.targets == vec![machine_id()])
    ));
    let prepared = fixture
        .store
        .prepared
        .lock()
        .await
        .clone()
        .expect("prepared promotion");
    let durable = serde_json::to_string(&prepared).expect("durable promotion json");
    assert!(!durable.contains("do-not-persist-this-secret"));
    assert!(durable.contains("DATABASE_PASSWORD"));
    assert!(prepared.service_document.image.pinned_digest().is_some());
    // First deploys are unpinned unless the request pinned machines.
    assert!(prepared.service_document.pinned_machines.is_empty());
    let [container] = prepared.containers.as_slice() else {
        panic!("one container row expected");
    };
    assert_eq!(container.document.machine_id, machine_id());
}

#[tokio::test]
async fn first_deploy_route_activation_failure_completes_the_active_service_with_a_warning() {
    let fixture = fixture(Admission::First, true);
    fixture.routes.fail_ensure.store(true, Ordering::SeqCst);
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("deploy");

    let CorrosionDeployState::Terminal { outcome, .. } = fixture.operations.terminal_state().await
    else {
        panic!("terminal state expected");
    };
    let CorrosionDeployOutcome::CompletedWithWarnings { results, warnings } = outcome else {
        panic!("committed service must complete with route warning");
    };
    let [result] = results.as_slice() else {
        panic!("one service result expected");
    };
    assert_eq!(result.result, CorrosionDeployServiceResultKind::Completed);
    assert!(matches!(
        warnings.as_slice(),
        [CorrosionDeployWarning::AutomaticRouteActivation {
            service_id,
            failure: CorrosionAutomaticRouteFailure::AllocationUnsettled,
        }] if service_id == &result.service_id
    ));
}

#[tokio::test]
async fn exhausted_claim_adjudication_retries_three_times_then_terminalizes() {
    let fixture = fixture(Admission::First, true);
    fixture
        .store
        .fail_adjudication
        .store(true, Ordering::SeqCst);
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("typed terminal");
    assert_eq!(
        fixture.store.adjudication_attempts.load(Ordering::SeqCst),
        3
    );
    assert!(matches!(
        fixture.operations.terminal_state().await,
        CorrosionDeployState::Terminal { .. }
    ));
}

#[tokio::test]
async fn failed_health_gate_retains_the_started_container_as_evidence() {
    let fixture = fixture(Admission::First, true);
    fixture.runtime.health_failure.store(true, Ordering::SeqCst);
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("typed failure");
    assert_eq!(fixture.runtime.created().await, 1);
    let calls = fixture.runtime.call_log().await;
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, RuntimeCall::Remove(_)))
    );
    assert!(fixture.store.prepared.lock().await.is_none());
}

#[tokio::test]
async fn loser_cleanup_that_never_converges_terminalizes_after_three_attempts() {
    let fixture = fixture(Admission::First, true);
    fixture.store.claim_lost.store(true, Ordering::SeqCst);
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("typed terminal");
    assert_eq!(fixture.store.cleanup_attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn claim_loss_terminalizes_superseded_with_zero_docker_effects() {
    let fixture = fixture(Admission::Redeploy, true);
    let (winner_id, winner_doc) =
        claim_candidate("01J00000000000000000000001", "2026-08-05T10:00:00Z");
    *fixture.operations.claim_candidates.lock().await = vec![(winner_id.clone(), winner_doc)];
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let log = accepted.operation_log();
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("typed terminal");

    assert!(fixture.runtime.call_log().await.is_empty());
    // The claim was lost before mark_running: Created then Terminal only.
    assert_eq!(
        fixture.operations.writes.lock().await.as_slice(),
        [RowWrite::Created, RowWrite::Transition]
    );
    let CorrosionDeployState::Terminal { outcome, .. } = fixture.operations.terminal_state().await
    else {
        panic!("terminal state expected");
    };
    assert!(matches!(
        outcome,
        CorrosionDeployOutcome::Failed {
            failure: ployz_core::corrosion::CorrosionDeployFailure::SupersededByOperation {
                winner,
            },
            ..
        } if winner == winner_id
    ));
    let events = evidence_kinds(&log).await;
    assert!(events.contains(&OperationEvidence::OpClaimLost { winner: winner_id }));
}

#[tokio::test]
async fn stale_lower_claim_does_not_block() {
    let fixture = fixture(Admission::First, true);
    let stale = claim_candidate("01J00000000000000000000001", "2026-08-05T08:00:00Z");
    *fixture.operations.claim_candidates.lock().await = vec![stale];
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let log = accepted.operation_log();
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("deploy");
    let events = evidence_kinds(&log).await;
    assert!(events.contains(&OperationEvidence::OpClaimWon));
    assert!(matches!(
        fixture.operations.terminal_state().await,
        CorrosionDeployState::Terminal {
            outcome: CorrosionDeployOutcome::Completed { .. },
            ..
        }
    ));
}

#[tokio::test]
async fn second_deploy_flips_atomically_with_previous_image() {
    let fixture = fixture(Admission::Redeploy, true);
    *fixture.runtime.containers.lock().await =
        vec![docker_container("incumbent-1", incumbent_op(), &[])];
    *fixture.store.rows.lock().await = vec![container_row("incumbent-1", incumbent_op())];
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let operation_id = accepted.reply.operation_id.clone();
    let log = accepted.operation_log();
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("redeploy");

    let intent = fixture
        .store
        .redeploy_prepared
        .lock()
        .await
        .clone()
        .expect("flip intent");
    assert_eq!(
        intent.exact_incumbent_document,
        incumbent_service(machine_id()).exact_document
    );
    assert_eq!(intent.service_document.active_deploy, operation_id);
    assert_eq!(intent.service_document.operation_id, operation_id);
    assert_eq!(
        intent.service_document.previous_image,
        Some(incumbent_document(machine_id()).image)
    );
    let [container] = intent.containers.as_slice() else {
        panic!("one container row expected");
    };
    assert_eq!(container.document.deploy, operation_id);
    assert_eq!(container.document.machine_id, machine_id());
    assert_eq!(fixture.store.converge_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.routes.checks.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.routes.ensures.load(Ordering::SeqCst), 1);
    let events = evidence_kinds(&log).await;
    assert!(events.contains(&OperationEvidence::RowsCommitted));
    assert!(events.contains(&OperationEvidence::Drained));
    assert!(events.contains(&OperationEvidence::IncumbentStopped {
        container_id: ContainerId::try_new("incumbent-1").expect("container"),
    }));
    assert!(events.contains(&OperationEvidence::IncumbentRemoved {
        container_id: ContainerId::try_new("incumbent-1").expect("container"),
    }));
    assert!(matches!(
        fixture.operations.terminal_state().await,
        CorrosionDeployState::Terminal {
            outcome: CorrosionDeployOutcome::Completed { .. },
            ..
        }
    ));
    // The old revision's row is deleted exactly as observed.
    let deleted = fixture.store.deleted.lock().await.clone();
    assert!(
        deleted
            .iter()
            .any(|rows| { rows == &vec![ContainerId::try_new("incumbent-1").expect("container")] })
    );
}

#[tokio::test]
async fn redeploy_route_failure_before_the_flip_remains_a_typed_failure() {
    let fixture = fixture(Admission::Redeploy, true);
    fixture.routes.fail_ensure.store(true, Ordering::SeqCst);
    *fixture.runtime.containers.lock().await =
        vec![docker_container("incumbent-1", incumbent_op(), &[])];
    *fixture.store.rows.lock().await = vec![container_row("incumbent-1", incumbent_op())];
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("typed terminal");

    assert_eq!(fixture.store.converge_calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        fixture.operations.terminal_state().await,
        CorrosionDeployState::Terminal {
            outcome: CorrosionDeployOutcome::Failed {
                failure: CorrosionDeployFailure::AutomaticRoute {
                    failure: CorrosionAutomaticRouteFailure::AllocationUnsettled,
                    ..
                },
                ..
            },
            ..
        }
    ));
}

#[tokio::test]
async fn redeploy_places_replicas_across_picked_machines() {
    let fixture = fixture(Admission::Redeploy, true);
    fixture
        .mesh
        .add_remote(remote_peer(), Some(remote_bid(&[])))
        .await;
    *fixture.runtime.containers.lock().await =
        vec![docker_container("incumbent-1", incumbent_op(), &[])];
    *fixture.store.rows.lock().await = vec![container_row("incumbent-1", incumbent_op())];
    let accepted = fixture
        .driver
        .admit(spread_request(2), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let operation_id = accepted.reply.operation_id.clone();
    let log = accepted.operation_log();
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("redeploy");

    let intent = fixture
        .store
        .redeploy_prepared
        .lock()
        .await
        .clone()
        .expect("flip intent");
    // Sticky keeps the incumbent machine first; the second replica spreads.
    let machines: Vec<MachineRowId> = intent
        .containers
        .iter()
        .map(|container| container.document.machine_id.clone())
        .collect();
    assert_eq!(machines, vec![machine_id(), remote_machine_id()]);
    assert!(
        intent
            .containers
            .iter()
            .all(|container| container.document.deploy == operation_id)
    );
    // The remote replica's endpoint comes from the remote health gate.
    let [_, remote_container] = intent.containers.as_slice() else {
        panic!("two container rows expected");
    };
    assert_eq!(remote_container.document.ip, Ipv4Addr::new(10, 210, 30, 2));
    // The effective placement written at the flip carries the request.
    assert_eq!(
        intent.service_document.placement,
        ployz_core::corrosion::ServicePlacement::Replicated {
            replicas: ServiceReplicaCount::try_new(2).expect("replicas"),
        }
    );
    assert!(intent.service_document.pinned_machines.is_empty());
    // The remote target was pulled, created, started, and gated over verbs.
    let verbs = fixture.verbs.verbs().await;
    assert!(
        verbs
            .iter()
            .any(|verb| matches!(verb, DeployVerb::PullImage { .. }))
    );
    assert!(
        verbs
            .iter()
            .any(|verb| matches!(verb, DeployVerb::CreateContainer { .. }))
    );
    assert!(
        verbs
            .iter()
            .any(|verb| matches!(verb, DeployVerb::StartContainer { .. }))
    );
    assert!(
        verbs
            .iter()
            .any(|verb| matches!(verb, DeployVerb::HealthGate { .. }))
    );
    // The local machine is dispatched in-process, never over the mesh.
    assert_eq!(fixture.runtime.created().await, 1);
    let events = evidence_kinds(&log).await;
    assert!(events.contains(&OperationEvidence::ContainerCreated {
        container_id: ContainerId::try_new("new-container").expect("container"),
    }));
    assert!(events.contains(&OperationEvidence::ContainerCreated {
        container_id: ContainerId::try_new("remote-1").expect("container"),
    }));
}

#[tokio::test]
async fn a_single_visible_volume_holder_pins_the_replacement_to_it() {
    let fixture = fixture(Admission::Redeploy, true);
    fixture
        .mesh
        .add_remote(remote_peer(), Some(remote_bid(&["data"])))
        .await;
    *fixture.runtime.containers.lock().await = Vec::new();
    let mut request = volume_request();
    request.machines = Some(RequestedPins::Any);
    let accepted = fixture
        .driver
        .admit(request, initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("redeploy");

    let intent = fixture
        .store
        .redeploy_prepared
        .lock()
        .await
        .clone()
        .expect("flip intent");
    let [container] = intent.containers.as_slice() else {
        panic!("one container row expected");
    };
    assert_eq!(container.document.machine_id, remote_machine_id());
    // The volume holder is the only target: nothing was created locally.
    assert_eq!(fixture.runtime.created().await, 0);
}

#[tokio::test]
async fn an_authorization_refusal_from_a_target_fails_the_operation_typed() {
    let fixture = fixture(Admission::Redeploy, true);
    fixture
        .mesh
        .add_remote(remote_peer(), Some(remote_bid(&[])))
        .await;
    *fixture.runtime.containers.lock().await =
        vec![docker_container("incumbent-1", incumbent_op(), &[])];
    *fixture.verbs.refuse_with.lock().await = Some(DeployExecuteOutcome::CallerNotDriver {
        driver: remote_machine_id(),
    });
    let accepted = fixture
        .driver
        .admit(spread_request(2), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("typed failure");

    assert_eq!(fixture.store.converge_calls.load(Ordering::SeqCst), 0);
    let CorrosionDeployState::Terminal { outcome, .. } = fixture.operations.terminal_state().await
    else {
        panic!("terminal state expected");
    };
    let CorrosionDeployOutcome::Failed { failure, .. } = outcome else {
        panic!("refused verb must fail the operation");
    };
    let ployz_core::corrosion::CorrosionDeployFailure::ServiceFailed { failure, .. } = failure
    else {
        panic!("service failure expected");
    };
    let message = match failure {
        ployz_core::corrosion::CorrosionDeployServiceFailure::ImagePullFailed { message }
        | ployz_core::corrosion::CorrosionDeployServiceFailure::ContainerCreateFailed { message }
        | ployz_core::corrosion::CorrosionDeployServiceFailure::ContainerStartFailed { message }
        | ployz_core::corrosion::CorrosionDeployServiceFailure::IncumbentStopFailed { message }
        | ployz_core::corrosion::CorrosionDeployServiceFailure::HealthGateFailed { message } => {
            message
        }
    };
    assert!(message.contains("not the operation's driver"));
}

#[tokio::test]
async fn pre_flip_failure_never_flips_and_leaves_the_incumbent_serving() {
    let fixture = fixture(Admission::Redeploy, true);
    fixture.runtime.health_failure.store(true, Ordering::SeqCst);
    *fixture.runtime.containers.lock().await =
        vec![docker_container("incumbent-1", incumbent_op(), &[])];
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("typed failure");

    assert_eq!(fixture.store.converge_calls.load(Ordering::SeqCst), 0);
    assert!(fixture.store.redeploy_prepared.lock().await.is_none());
    let calls = fixture.runtime.call_log().await;
    assert!(!calls.contains(&RuntimeCall::Stop("incumbent-1".to_owned())));
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, RuntimeCall::Remove(_)))
    );
    assert_eq!(fixture.runtime.created().await, 1);
    assert!(matches!(
        fixture.operations.terminal_state().await,
        CorrosionDeployState::Terminal {
            outcome: CorrosionDeployOutcome::Failed { .. },
            ..
        }
    ));
}

#[tokio::test]
async fn volume_service_stops_incumbent_only_after_create_and_restarts_on_gate_failure() {
    let fixture = fixture(Admission::Redeploy, true);
    fixture.runtime.health_failure.store(true, Ordering::SeqCst);
    *fixture.runtime.containers.lock().await =
        vec![docker_container("incumbent-1", incumbent_op(), &["data"])];
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let log = accepted.operation_log();
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("typed failure");

    let calls = fixture.runtime.call_log().await;
    let create_at = calls
        .iter()
        .position(|call| matches!(call, RuntimeCall::Create))
        .expect("create call");
    let stop_at = calls
        .iter()
        .position(|call| call == &RuntimeCall::Stop("incumbent-1".to_owned()))
        .expect("incumbent stop");
    assert!(
        create_at < stop_at,
        "pull/create must precede incumbent stop"
    );
    // The incumbent is restarted after the failed gate; the failed new
    // container is retained.
    assert!(calls.contains(&RuntimeCall::Start("incumbent-1".to_owned())));
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, RuntimeCall::Remove(_)))
    );
    assert_eq!(fixture.store.converge_calls.load(Ordering::SeqCst), 0);
    let events = evidence_kinds(&log).await;
    assert!(events.contains(&OperationEvidence::IncumbentStopped {
        container_id: ContainerId::try_new("incumbent-1").expect("container"),
    }));
    assert!(events.contains(&OperationEvidence::IncumbentRestarted {
        container_id: ContainerId::try_new("incumbent-1").expect("container"),
    }));
}

#[tokio::test]
async fn takeover_abort_before_flip_mutates_nothing_further() {
    let fixture = fixture(Admission::Redeploy, true);
    *fixture.runtime.containers.lock().await =
        vec![docker_container("incumbent-1", incumbent_op(), &[])];
    let rival = claim_candidate("7ZZZZZZZZZZZZZZZZZZZZZZZZZ", "2026-08-05T10:00:00Z");
    *fixture.operations.takeover_candidates.lock().await = vec![rival.clone()];
    // The rival becomes visible only at the pre-flip boundary.
    fixture
        .operations
        .takeover_visible_after
        .store(1, Ordering::SeqCst);
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let log = accepted.operation_log();
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("typed terminal");

    assert_eq!(fixture.store.converge_calls.load(Ordering::SeqCst), 0);
    let calls = fixture.runtime.call_log().await;
    assert!(!calls.contains(&RuntimeCall::Stop("incumbent-1".to_owned())));
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, RuntimeCall::Remove(_)))
    );
    let CorrosionDeployState::Terminal { outcome, .. } = fixture.operations.terminal_state().await
    else {
        panic!("terminal state expected");
    };
    assert!(matches!(
        outcome,
        CorrosionDeployOutcome::Failed {
            failure: ployz_core::corrosion::CorrosionDeployFailure::SupersededByOperation {
                winner,
            },
            ..
        } if winner == rival.0
    ));
    let events = evidence_kinds(&log).await;
    assert!(events.contains(&OperationEvidence::OpClaimLost { winner: rival.0 }));
}

#[tokio::test]
async fn sweep_removes_only_foreign_debris_and_their_rows() {
    let fixture = fixture(Admission::Redeploy, true);
    *fixture.runtime.containers.lock().await = vec![
        docker_container("debris-1", debris_op(), &[]),
        docker_container("incumbent-1", incumbent_op(), &[]),
    ];
    *fixture.store.rows.lock().await = vec![
        container_row("debris-1", debris_op()),
        container_row("incumbent-1", incumbent_op()),
    ];
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let log = accepted.operation_log();
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("redeploy");

    let events = evidence_kinds(&log).await;
    assert!(events.contains(&OperationEvidence::DebrisSwept {
        removed: vec![ContainerId::try_new("debris-1").expect("container")],
    }));
    // The sweep's exact row delete names only the debris row.
    let deleted = fixture.store.deleted.lock().await.clone();
    let [first_delete, ..] = deleted.as_slice() else {
        panic!("sweep must delete debris rows");
    };
    assert_eq!(
        first_delete,
        &vec![ContainerId::try_new("debris-1").expect("container")]
    );
    let calls = fixture.runtime.call_log().await;
    let debris_remove = calls
        .iter()
        .position(|call| call == &RuntimeCall::Remove("debris-1".to_owned()))
        .expect("debris removed");
    let create_at = calls
        .iter()
        .position(|call| matches!(call, RuntimeCall::Create))
        .expect("create call");
    assert!(debris_remove < create_at, "sweep precedes pull/create");
}

#[tokio::test]
async fn a_first_deploy_sweeps_an_earlier_failed_attempts_containers() {
    let fixture = fixture(Admission::First, true);
    // A failed earlier first deploy left its container behind; with no
    // service row it is debris by definition and the retry sweeps it first.
    *fixture.runtime.containers.lock().await = vec![docker_container("debris-1", debris_op(), &[])];
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let log = accepted.operation_log();
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("first deploy");

    let events = evidence_kinds(&log).await;
    assert!(events.contains(&OperationEvidence::DebrisSwept {
        removed: vec![ContainerId::try_new("debris-1").expect("container")],
    }));
    let calls = fixture.runtime.call_log().await;
    let debris_remove = calls
        .iter()
        .position(|call| call == &RuntimeCall::Remove("debris-1".to_owned()))
        .expect("debris removed");
    let create_at = calls
        .iter()
        .position(|call| matches!(call, RuntimeCall::Create))
        .expect("create call");
    assert!(debris_remove < create_at, "sweep precedes pull/create");
}

#[tokio::test]
async fn skip_gate_completes_with_the_health_gate_warning() {
    let fixture = fixture(Admission::Redeploy, true);
    // The gate would fail; skip must bypass it entirely.
    fixture.runtime.health_failure.store(true, Ordering::SeqCst);
    *fixture.runtime.containers.lock().await =
        vec![docker_container("incumbent-1", incumbent_op(), &[])];
    let accepted = fixture
        .driver
        .admit(request_with(HealthGatePolicy::Skip), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let log = accepted.operation_log();
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("redeploy");

    let events = evidence_kinds(&log).await;
    assert!(events.contains(&OperationEvidence::HealthGateSkipped));
    let CorrosionDeployState::Terminal { outcome, .. } = fixture.operations.terminal_state().await
    else {
        panic!("terminal state expected");
    };
    let CorrosionDeployOutcome::CompletedWithWarnings { warnings, .. } = outcome else {
        panic!("skip-gate must complete with warnings");
    };
    assert_eq!(
        warnings,
        vec![CorrosionDeployWarning::HealthGateSkipped {
            service_id: service_id(),
        }]
    );
}

#[tokio::test(start_paused = true)]
async fn heartbeat_threads_through_the_shared_handle_and_stops_before_terminal() {
    let fixture = fixture(Admission::Redeploy, true);
    let fixture = Fixture {
        driver: fixture
            .driver
            .clone()
            .with_waits(Duration::ZERO, Duration::from_secs(20)),
        ..fixture
    };
    *fixture.runtime.containers.lock().await =
        vec![docker_container("incumbent-1", incumbent_op(), &[])];
    let accepted = fixture
        .driver
        .admit(request(), initiator())
        .await
        .expect("admission")
        .expect("accepted");
    let (_shutdown, shutdown) = watch::channel(false);
    accepted.task.run(shutdown).await.expect("redeploy");

    // The 20s drain outlives the 15s heartbeat interval: at least one
    // refresh lands, and none may follow the terminal transition.
    let writes = fixture.operations.writes.lock().await.clone();
    assert!(writes.contains(&RowWrite::Heartbeat));
    assert_eq!(writes.last(), Some(&RowWrite::Transition));
    let after_last_transition = writes
        .iter()
        .rposition(|write| *write == RowWrite::Transition)
        .expect("terminal transition");
    assert!(
        writes
            .iter()
            .skip(after_last_transition)
            .all(|write| *write != RowWrite::Heartbeat),
        "no heartbeat may land after the terminal write"
    );
    assert!(matches!(
        fixture.operations.terminal_state().await,
        CorrosionDeployState::Terminal {
            outcome: CorrosionDeployOutcome::Completed { .. },
            ..
        }
    ));
}

#[tokio::test(start_paused = true)]
async fn stale_heartbeat_over_a_terminal_row_raises_the_superseded_flag() {
    let fixture = fixture(Admission::First, true);
    fixture
        .operations
        .stale_heartbeat
        .store(true, Ordering::SeqCst);
    let operation_id = OperationRowId::try_new("01J00000000000000000000011").expect("op");
    let (_, document) = claim_candidate("01J00000000000000000000011", "2026-08-05T10:00:00Z");
    let terminal = document
        .clone()
        .transition_deploy(ployz_core::corrosion::CorrosionDeployTransition::Terminal {
            completed_at: CorrosionTimestamp::try_new("2026-08-05T10:00:05Z").expect("timestamp"),
            outcome: CorrosionDeployOutcome::failed(
                vec![ployz_core::corrosion::CorrosionDeployServiceResult::skipped(service_id())],
                ployz_core::corrosion::CorrosionDeployFailure::Interrupted,
            )
            .expect("outcome"),
        })
        .expect("terminal");
    *fixture.operations.operation.lock().await = Some(terminal);
    let row = Arc::new(Mutex::new(
        observed_with(&operation_id, document).expect("observed"),
    ));
    let heartbeat = DeployHeartbeat::spawn(fixture.driver.clone(), row);

    tokio::time::sleep(ployz_core::corrosion::DEPLOY_HEARTBEAT_INTERVAL * 2).await;
    assert!(heartbeat.superseded());
    heartbeat.stop().await;
}

#[tokio::test]
async fn resumed_committed_first_deploy_route_failure_is_a_completion_warning() {
    let fixture = fixture(Admission::First, true);
    fixture.routes.fail_ensure.store(true, Ordering::SeqCst);
    let operation_id = OperationRowId::try_new("01J00000000000000000000011").expect("op");
    let (_, document) = claim_candidate("01J00000000000000000000011", "2026-08-05T10:00:00Z");
    *fixture.operations.operation.lock().await = Some(document);
    let directory =
        OperationEvidenceDirectory::new(fixture._root.path().join("first-resume"), 16 * 1024);
    let log = directory
        .create(
            crate::roles::api::http::operation_evidence::EvidenceIdentity::new(
                operation_id.clone(),
                machine_id(),
            ),
            CorrosionTimestamp::try_new("2026-08-05T10:00:00Z").expect("timestamp"),
        )
        .await
        .expect("create evidence");

    fixture
        .driver
        .resume_promotion(
            operation_id,
            log,
            crate::roles::api::http::operation_evidence::DurablePromotionProgress::ClaimWon {
                prepared: crate::roles::api::http::operation_evidence::prepared_promotion_fixture(),
            },
        )
        .await
        .expect("resume");

    let CorrosionDeployState::Terminal { outcome, .. } = fixture.operations.terminal_state().await
    else {
        panic!("terminal state expected");
    };
    let CorrosionDeployOutcome::CompletedWithWarnings { results, warnings } = outcome else {
        panic!("committed recovery must complete with warnings");
    };
    assert!(matches!(
        results.as_slice(),
        [result] if result.result == CorrosionDeployServiceResultKind::Completed
    ));
    assert!(matches!(
        warnings.as_slice(),
        [CorrosionDeployWarning::AutomaticRouteActivation {
            failure: CorrosionAutomaticRouteFailure::AllocationUnsettled,
            ..
        }]
    ));
}

#[tokio::test]
async fn resumed_redeploy_never_enters_the_first_deploy_claim_branch() {
    let fixture = fixture(Admission::Redeploy, true);
    let intent = crate::roles::api::http::operation_evidence::prepared_redeploy_intent_fixture();
    let operation_id = OperationRowId::try_new("01J00000000000000000000011").expect("op");
    let (_, document) = claim_candidate("01J00000000000000000000011", "2026-08-05T10:00:00Z");
    *fixture.operations.operation.lock().await = Some(document);
    let directory = OperationEvidenceDirectory::new(fixture._root.path().join("resume"), 16 * 1024);
    let log = directory
        .create(
            crate::roles::api::http::operation_evidence::EvidenceIdentity::new(
                operation_id.clone(),
                machine_id(),
            ),
            CorrosionTimestamp::try_new("2026-08-05T10:00:00Z").expect("timestamp"),
        )
        .await
        .expect("create evidence");
    log.append(
        CorrosionTimestamp::try_new("2026-08-05T10:00:00Z").expect("timestamp"),
        OperationEvidence::OpClaimWon,
    )
    .await
    .expect("claim won");
    log.append_redeploy_prepared(
        CorrosionTimestamp::try_new("2026-08-05T10:00:01Z").expect("timestamp"),
        intent.clone(),
    )
    .await
    .expect("redeploy prepared");

    fixture
        .driver
        .resume_promotion(
            operation_id,
            log.clone(),
            crate::roles::api::http::operation_evidence::DurablePromotionProgress::RedeployPrepared {
                prepared: intent,
            },
        )
        .await
        .expect("resume");

    // The redeploy resume converges the flip and never touches the
    // first-deploy service-name claim machinery.
    assert_eq!(fixture.store.converge_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.store.adjudication_attempts.load(Ordering::SeqCst),
        0
    );
    assert_eq!(fixture.store.cleanup_attempts.load(Ordering::SeqCst), 0);
    let events = evidence_kinds(&log).await;
    assert!(events.contains(&OperationEvidence::RowsCommitted));
    // A resumed flip never drains or cleans, so its completion carries the
    // cleanup warning instead of a plain Completed.
    let CorrosionDeployState::Terminal { outcome, .. } = fixture.operations.terminal_state().await
    else {
        panic!("terminal state expected");
    };
    let CorrosionDeployOutcome::CompletedWithWarnings { warnings, .. } = outcome else {
        panic!("resumed post-flip op must complete with warnings");
    };
    assert!(
        warnings
            .iter()
            .any(|warning| matches!(warning, CorrosionDeployWarning::CleanupIncomplete { .. }))
    );
}

#[tokio::test]
async fn resumed_post_flip_redeploy_completes_with_a_cleanup_warning() {
    let fixture = fixture(Admission::Redeploy, true);
    let intent = crate::roles::api::http::operation_evidence::prepared_redeploy_intent_fixture();
    let operation_id = OperationRowId::try_new("01J00000000000000000000011").expect("op");
    let (_, document) = claim_candidate("01J00000000000000000000011", "2026-08-05T10:00:00Z");
    *fixture.operations.operation.lock().await = Some(document);
    let directory =
        OperationEvidenceDirectory::new(fixture._root.path().join("post-flip"), 16 * 1024);
    let log = directory
        .create(
            crate::roles::api::http::operation_evidence::EvidenceIdentity::new(
                operation_id.clone(),
                machine_id(),
            ),
            CorrosionTimestamp::try_new("2026-08-05T10:00:00Z").expect("timestamp"),
        )
        .await
        .expect("create evidence");
    log.append(
        CorrosionTimestamp::try_new("2026-08-05T10:00:00Z").expect("timestamp"),
        OperationEvidence::OpClaimWon,
    )
    .await
    .expect("claim won");
    log.append_redeploy_prepared(
        CorrosionTimestamp::try_new("2026-08-05T10:00:01Z").expect("timestamp"),
        intent.clone(),
    )
    .await
    .expect("redeploy prepared");
    log.append(
        CorrosionTimestamp::try_new("2026-08-05T10:00:02Z").expect("timestamp"),
        OperationEvidence::RowsCommitted,
    )
    .await
    .expect("rows committed");

    fixture
        .driver
        .resume_promotion(
            operation_id,
            log.clone(),
            crate::roles::api::http::operation_evidence::DurablePromotionProgress::RedeployRowsCommitted {
                prepared: intent,
            },
        )
        .await
        .expect("resume");

    // A durably committed flip is never re-attempted; the operation
    // terminalizes with the skipped-cleanup warning.
    assert_eq!(fixture.store.converge_calls.load(Ordering::SeqCst), 0);
    let CorrosionDeployState::Terminal { outcome, .. } = fixture.operations.terminal_state().await
    else {
        panic!("terminal state expected");
    };
    let CorrosionDeployOutcome::CompletedWithWarnings { warnings, .. } = outcome else {
        panic!("resumed post-flip op must complete with warnings");
    };
    assert!(
        warnings
            .iter()
            .any(|warning| matches!(warning, CorrosionDeployWarning::CleanupIncomplete { .. }))
    );
}
