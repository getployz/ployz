use super::execute::{
    DeployApplyPreconditions, apply_prepared_with_certificate_coordination,
    apply_with_certificate_coordination, apply_with_deploy_id_and_preconditions,
    apply_with_initial_plan, ensure_plan_stable, run_phase_startup,
};
use super::lifecycle::PreparedDeploy;
use super::participant_set::ParticipantSet;
use super::plan::{deployable_machines, desired_slots, resolve_plan};
use super::probe::{NoopParticipantProbe, ParticipantProbe, ProbeError, ProbeErrorKind};
use super::{prepare, preview};
use crate::certificates::LocalHttp01ChallengeReadiness;
use crate::deploy::participant::{
    CleanupVolumeCloneRequest, CloneVolumeRequest, CloneVolumeResult, DeployParticipantClient,
    MoveVolumeRequest, MoveVolumeResult, StartCandidateRequest,
};
use crate::error::{DeployError, Error, Result};
use crate::model::RegionRole;
use crate::model::{
    AcmeAccountRecord, AcmeChallengeReadinessRecord, AcmeChallengeRecord, BranchEnvironmentFailure,
    BranchEnvironmentRecord, CertificateRecord, DeployBaselineComponent, DeployChangeKind,
    DeployId, DeployPhaseAdvancePolicy, DeployPhaseCommitPolicy, DeployPhaseId, DeployPhaseRecord,
    DeployPhaseRollbackPolicy, DeployPhaseState, DeployPhaseWork, DeployPreview,
    DeployPreviewBaseline, DeployPreviewBaselineComponents, DeployRecord, DeployRecordState,
    DeployState, DrainState, ImageArtifact, ImageArtifactProvenance, ImageAvailabilityRecord,
    ImageDigest, ImagePresence, ImageRef, InstanceId, InstancePhase, InstanceStatusRecord,
    MachineId, MachineLifecycle, MachineMembership, MachineStorageRole, MachineTopology, OverlayIp,
    PreparedDeployRecord, PreparedDeployState, PublicKey, ServiceBranchLineageRecord,
    ServiceRelease, ServiceReleaseRecord, ServiceReleaseSlot, ServiceRevisionRecord,
    ServiceSourceMode, SlotId, StorageParticipation, VolumeBranchLineageRecord,
    VolumeClonePreflightAction, VolumeClonePreflightScope, VolumeMovementRecord, VolumeRecord,
};
use async_trait::async_trait;
use ployz_cert_acme_api::{
    NoopAcmeAccountCoordinator, NoopAcmeIssuerFactory, NoopIssuanceCoordinator,
};
use ployz_error::Result as PloyzResult;
use ployz_spec::{
    ContainerSpec, DeployIntent, DeployManifest, DeployPhaseIntent, HttpRoute, Mount, MountSource,
    Namespace, NetworkMode, Placement, PortProtocol, PullPolicy, Resources, RestartPolicy,
    RolloutStrategy, RouteSpec, ServiceIntent, ServiceIntentHint, ServicePort, ServiceSpec,
    VolumeCloneConsistency, VolumeCloneDataPolicy, VolumeDeclaration, VolumeIntent,
    VolumeIntentHint, VolumeScope,
};
use ployz_store_api::{
    CertificateStore, DeployCommit, DeployStore, ImageAvailabilityStore, InstanceStatusStore,
    InviteStore, MachineMembershipStore, MachineSubscription, PeerRttStore,
    RoutingEventSubscription, RoutingStateStore, StoreDriver, StoreRuntimeControl, SyncProbe,
};
use ployz_store_memory::{MemoryService, MemoryStore, StoreDriverMemoryExt as _};
use std::collections::{BTreeMap, HashMap};
use std::net::Ipv6Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::sleep;

mod apply;
mod branch;
mod export;
mod image_availability;
mod managed_domains;
mod migrate_manifest;
mod prepared;
mod preview;
mod volume_moves;

fn empty_deploy_baseline() -> DeployPreviewBaseline {
    DeployPreviewBaseline {
        fingerprint: String::new(),
        components: DeployPreviewBaselineComponents {
            manifest: String::new(),
            participants: String::new(),
            phases: String::new(),
            services: String::new(),
            service_sources: String::new(),
            volumes: String::new(),
            volume_moves: String::new(),
            volume_clones: String::new(),
        },
    }
}

#[async_trait]
trait TestStoreSeed {
    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> PloyzResult<()>;
    async fn list_service_releases(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<ServiceReleaseRecord>>;
}

#[async_trait]
impl TestStoreSeed for StoreDriver {
    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> PloyzResult<()> {
        self.commit_deploy(&DeployCommit {
            namespace: record.namespace.clone(),
            revisions: Vec::new(),
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases: vec![record.clone()],
            volumes: Vec::new(),
            deploy: test_deploy_record(&record.namespace, "seed-deploy"),
        })
        .await
    }

    async fn list_service_releases(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<ServiceReleaseRecord>> {
        self.list_deploy_releases(namespace).await
    }
}

fn prepared_deploy_status(
    prepared: &PreparedDeployRecord,
    state: DeployState,
) -> crate::model::DeployRecord {
    crate::model::DeployRecord {
        deploy_id: prepared.prepared_deploy_id.clone(),
        namespace: prepared.namespace.clone(),
        coordinator_machine_id: prepared.coordinator_machine_id.clone(),
        manifest_hash: prepared.manifest_hash.clone(),
        started_at: prepared.created_at,
        state: match state {
            DeployState::Committed => DeployRecordState::Committed {
                committed_at: prepared.updated_at,
                finished_at: prepared.updated_at,
                summary_json: serde_json::to_string(&prepared.preview).expect("serialize preview"),
            },
            DeployState::CleanupPending => DeployRecordState::CleanupPending {
                committed_at: prepared.updated_at,
                finished_at: prepared.updated_at,
                summary_json: serde_json::to_string(&prepared.preview).expect("serialize preview"),
            },
            DeployState::CheckpointCommitted => DeployRecordState::CheckpointCommitted {
                summary_json: serde_json::to_string(&prepared.preview).expect("serialize preview"),
            },
            DeployState::FailedAfterCheckpoint => DeployRecordState::FailedAfterCheckpoint {
                finished_at: prepared.updated_at,
                summary_json: serde_json::to_string(&prepared.preview).expect("serialize preview"),
            },
            DeployState::Failed => DeployRecordState::Failed {
                finished_at: prepared.updated_at,
                summary_json: serde_json::to_string(&prepared.preview).expect("serialize preview"),
            },
            DeployState::Planning => DeployRecordState::Planning {
                summary_json: serde_json::to_string(&prepared.preview).expect("serialize preview"),
            },
            DeployState::Applying => DeployRecordState::Applying {
                summary_json: serde_json::to_string(&prepared.preview).expect("serialize preview"),
            },
        },
    }
}

#[derive(Clone, Default)]
struct FakeController {
    open_delay: Duration,
    start_delay: Duration,
    fail_open_machine: Option<String>,
    fail_start_service: Option<String>,
    fail_start_after_create_service: Option<String>,
    fail_remove_instance: Option<String>,
    fail_move_volume: Option<String>,
    fail_clone_volume: Option<String>,
    fail_cleanup_clone_volume: Option<String>,
    open_active: Arc<AtomicUsize>,
    max_open: Arc<AtomicUsize>,
    move_count: Arc<AtomicUsize>,
    clone_count: Arc<AtomicUsize>,
    clone_cleanup_count: Arc<AtomicUsize>,
    start_count: Arc<AtomicUsize>,
    start_active: Arc<AtomicUsize>,
    max_global_start: Arc<AtomicUsize>,
    drain_count: Arc<AtomicUsize>,
    remove_count: Arc<AtomicUsize>,
    machine_state: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    machine_max: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    start_log_entries: Arc<Mutex<Vec<String>>>,
    operation_log_entries: Arc<Mutex<Vec<String>>>,
    start_requests: Arc<Mutex<Vec<StartCandidateRequest>>>,
    move_requests: Arc<Mutex<Vec<MoveVolumeRequest>>>,
    clone_requests: Arc<Mutex<Vec<CloneVolumeRequest>>>,
    clone_cleanup_requests: Arc<Mutex<Vec<String>>>,
    inspect_instances: Arc<Mutex<Vec<InstanceStatusRecord>>>,
}

impl FakeController {
    fn max_open_seen(&self) -> usize {
        self.max_open.load(Ordering::SeqCst)
    }

    fn start_count(&self) -> usize {
        self.start_count.load(Ordering::SeqCst)
    }

    fn move_count(&self) -> usize {
        self.move_count.load(Ordering::SeqCst)
    }

    fn clone_count(&self) -> usize {
        self.clone_count.load(Ordering::SeqCst)
    }

    fn clone_cleanup_count(&self) -> usize {
        self.clone_cleanup_count.load(Ordering::SeqCst)
    }

    fn max_global_start_seen(&self) -> usize {
        self.max_global_start.load(Ordering::SeqCst)
    }

    fn drain_count(&self) -> usize {
        self.drain_count.load(Ordering::SeqCst)
    }

    fn remove_count(&self) -> usize {
        self.remove_count.load(Ordering::SeqCst)
    }

    fn max_machine_start_seen(&self, machine_id: &str) -> usize {
        self.machine_max
            .lock()
            .expect("machine max lock")
            .get(machine_id)
            .copied()
            .unwrap_or_default()
    }

    async fn start_log(&self) -> Vec<String> {
        self.start_log_entries.lock().await.clone()
    }

    async fn operation_log(&self) -> Vec<String> {
        self.operation_log_entries.lock().await.clone()
    }

    async fn start_requests(&self) -> Vec<StartCandidateRequest> {
        self.start_requests.lock().await.clone()
    }

    async fn move_requests(&self) -> Vec<MoveVolumeRequest> {
        self.move_requests.lock().await.clone()
    }

    async fn clone_requests(&self) -> Vec<CloneVolumeRequest> {
        self.clone_requests.lock().await.clone()
    }

    async fn set_inspect_instances(&self, instances: Vec<InstanceStatusRecord>) {
        *self.inspect_instances.lock().await = instances;
    }

    async fn on_open_start(&self) {
        let current = self.open_active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_open.fetch_max(current, Ordering::SeqCst);
        sleep(self.open_delay).await;
        self.open_active.fetch_sub(1, Ordering::SeqCst);
    }

    fn should_fail_open(&self, machine_id: &MachineId) -> bool {
        self.fail_open_machine.as_deref() == Some(machine_id.as_str())
    }

    async fn on_start_begin(&self, machine_id: &MachineId, service: &str, slot_id: &SlotId) {
        self.start_count.fetch_add(1, Ordering::SeqCst);
        let global = self.start_active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_global_start.fetch_max(global, Ordering::SeqCst);
        {
            let mut machine_state = self.machine_state.lock().expect("machine state lock");
            let current = machine_state
                .entry(machine_id.as_str().to_owned())
                .or_default();
            *current += 1;
            let mut machine_max = self.machine_max.lock().expect("machine max lock");
            let max = machine_max
                .entry(machine_id.as_str().to_owned())
                .or_default();
            *max = (*max).max(*current);
        }
        self.start_log_entries
            .lock()
            .await
            .push(format!("{service}:{machine_id}:{slot_id}"));
        self.operation_log_entries
            .lock()
            .await
            .push(format!("start:{service}:{machine_id}:{slot_id}"));
    }

    fn should_fail_start(&self, service: &str) -> bool {
        self.fail_start_service.as_deref() == Some(service)
    }

    fn should_fail_start_after_create(&self, service: &str) -> bool {
        self.fail_start_after_create_service.as_deref() == Some(service)
    }

    async fn on_start_end(&self, machine_id: &MachineId) {
        self.start_active.fetch_sub(1, Ordering::SeqCst);
        let mut machine_state = self.machine_state.lock().expect("machine state lock");
        let Some(current) = machine_state.get_mut(machine_id.as_str()) else {
            return;
        };
        *current -= 1;
    }

    async fn on_drain(&self, instance_id: &InstanceId) {
        self.drain_count.fetch_add(1, Ordering::SeqCst);
        self.operation_log_entries
            .lock()
            .await
            .push(format!("drain:{instance_id}"));
    }

    async fn on_remove(&self, instance_id: &InstanceId) {
        self.remove_count.fetch_add(1, Ordering::SeqCst);
        self.operation_log_entries
            .lock()
            .await
            .push(format!("remove:{instance_id}"));
    }

    fn should_fail_remove(&self, instance_id: &InstanceId) -> bool {
        self.fail_remove_instance.as_deref() == Some(instance_id.as_str())
    }

    async fn on_move_volume(&self, machine_id: &MachineId, request: &MoveVolumeRequest) {
        self.move_count.fetch_add(1, Ordering::SeqCst);
        self.move_requests.lock().await.push(request.clone());
        self.operation_log_entries.lock().await.push(format!(
            "move:{}:{}:{}:{}",
            request.volume, machine_id, request.from_machine, request.to_machine
        ));
    }

    fn should_fail_move(&self, volume: &str) -> bool {
        self.fail_move_volume.as_deref() == Some(volume)
    }

    async fn on_clone_volume(&self, machine_id: &MachineId, request: &CloneVolumeRequest) {
        self.clone_count.fetch_add(1, Ordering::SeqCst);
        self.clone_requests.lock().await.push(request.clone());
        self.operation_log_entries.lock().await.push(format!(
            "clone:{}:{}:{}/{}",
            request.volume, machine_id, request.source_namespace, request.source_volume
        ));
    }

    fn should_fail_clone(&self, volume: &str) -> bool {
        self.fail_clone_volume.as_deref() == Some(volume)
    }

    fn should_fail_cleanup_clone(&self, volume: &str) -> bool {
        self.fail_cleanup_clone_volume.as_deref() == Some(volume)
    }

    async fn on_cleanup_uncommitted_volume_clone(&self, volume: &str) {
        self.clone_cleanup_count.fetch_add(1, Ordering::SeqCst);
        self.clone_cleanup_requests
            .lock()
            .await
            .push(volume.to_string());
        self.operation_log_entries
            .lock()
            .await
            .push(format!("cleanup_clone:{volume}"));
    }
}

struct FakeParticipantClient {
    controller: FakeController,
}

impl FakeParticipantClient {
    fn new(controller: FakeController) -> Self {
        Self { controller }
    }
}

#[async_trait::async_trait]
impl DeployParticipantClient for FakeParticipantClient {
    fn supports_volume_moves(&self) -> bool {
        true
    }

    fn supports_volume_clones(&self) -> bool {
        true
    }

    async fn inspect_namespace(
        &self,
        machine: &MachineMembership,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        _coordinator_id: &MachineId,
    ) -> Result<Vec<InstanceStatusRecord>> {
        self.controller.on_open_start().await;
        if self.controller.should_fail_open(&machine.id) {
            return Err(ployz_error::Error::operation(
                "fake_open",
                format!("injected open failure for '{}'", machine.id),
            ));
        }
        Ok(self
            .controller
            .inspect_instances
            .lock()
            .await
            .iter()
            .filter(|status| status.machine_id == machine.id)
            .cloned()
            .collect())
    }

    async fn start_candidate(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        req: StartCandidateRequest,
    ) -> Result<InstanceStatusRecord> {
        self.controller
            .start_requests
            .lock()
            .await
            .push(req.clone());
        self.controller
            .on_start_begin(machine_id, &req.service, &req.slot_id)
            .await;
        if self.controller.should_fail_start(&req.service) {
            self.controller.on_start_end(machine_id).await;
            return Err(ployz_error::Error::operation(
                "fake_start",
                format!("injected start failure for '{}'", req.service),
            ));
        }
        sleep(self.controller.start_delay).await;
        if self.controller.should_fail_start_after_create(&req.service) {
            self.controller.on_start_end(machine_id).await;
            return Err(ployz_error::Error::operation(
                "fake_start_after_create",
                format!("injected post-create start failure for '{}'", req.service),
            ));
        }
        self.controller.on_start_end(machine_id).await;
        Ok(InstanceStatusRecord {
            instance_id: req.instance_id.clone(),
            namespace: namespace.clone(),
            service: req.service,
            slot_id: req.slot_id,
            machine_id: machine_id.clone(),
            revision_hash: "fake-revision".into(),
            deploy_id: deploy_id.clone(),
            docker_container_id: format!("container-{}", req.instance_id.as_str()),
            overlay_ip: None,
            backend_ports: BTreeMap::new(),
            phase: InstancePhase::Ready,
            ready: true,
            drain_state: DrainState::None,
            error: None,
            started_at: 0,
            updated_at: 0,
        })
    }

    async fn move_volume(
        &self,
        machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        request: MoveVolumeRequest,
    ) -> Result<MoveVolumeResult> {
        self.controller.on_move_volume(machine_id, &request).await;
        if self.controller.should_fail_move(&request.volume) {
            return Err(ployz_error::Error::operation(
                "fake_move_volume",
                format!("injected move failure for '{}'", request.volume),
            ));
        }
        Ok(MoveVolumeResult {
            snapshot: request.snapshot,
            snapshot_guid: 42,
            bytes_transferred: 4096,
        })
    }

    async fn clone_volume(
        &self,
        machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        request: CloneVolumeRequest,
    ) -> Result<CloneVolumeResult> {
        self.controller.on_clone_volume(machine_id, &request).await;
        if self.controller.should_fail_clone(&request.volume) {
            return Err(ployz_error::Error::operation(
                "fake_clone_volume",
                format!("injected clone failure for '{}'", request.volume),
            ));
        }
        Ok(CloneVolumeResult {
            snapshot: request.snapshot,
            snapshot_guid: 84,
            target_dataset: format!("tank/ployz/{}", request.volume),
        })
    }

    async fn cleanup_volume_clone(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        request: CleanupVolumeCloneRequest,
    ) -> Result<()> {
        let volume = request.volume.as_str();
        self.controller
            .on_cleanup_uncommitted_volume_clone(volume)
            .await;
        if self.controller.should_fail_cleanup_clone(volume) {
            return Err(ployz_error::Error::operation(
                "fake_cleanup_volume_clone",
                format!("injected cleanup failure for '{volume}'"),
            ));
        }
        Ok(())
    }

    async fn drain_instance(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> Result<()> {
        self.controller.on_drain(instance_id).await;
        Ok(())
    }

    async fn remove_instance(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> Result<()> {
        self.controller.on_remove(instance_id).await;
        if self.controller.should_fail_remove(instance_id) {
            return Err(ployz_error::Error::operation(
                "fake_remove",
                format!("injected remove failure for '{}'", instance_id),
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
struct UnsupportedParticipantClient {
    inspect_count: AtomicUsize,
}

impl UnsupportedParticipantClient {
    fn inspect_count(&self) -> usize {
        self.inspect_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl DeployParticipantClient for UnsupportedParticipantClient {
    async fn inspect_namespace(
        &self,
        _machine: &MachineMembership,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        _coordinator_id: &MachineId,
    ) -> Result<Vec<InstanceStatusRecord>> {
        self.inspect_count.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }

    async fn start_candidate(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        _request: StartCandidateRequest,
    ) -> Result<InstanceStatusRecord> {
        Err(Error::operation("unsupported_participant", "start"))
    }

    async fn drain_instance(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        _instance_id: &InstanceId,
    ) -> Result<()> {
        Err(Error::operation("unsupported_participant", "drain"))
    }

    async fn remove_instance(
        &self,
        _machine_id: &MachineId,
        _namespace: &Namespace,
        _deploy_id: &DeployId,
        _instance_id: &InstanceId,
    ) -> Result<()> {
        Err(Error::operation("unsupported_participant", "remove"))
    }
}

struct FailingParticipantProbe {
    machine_id: MachineId,
}

#[async_trait]
impl ParticipantProbe for FailingParticipantProbe {
    async fn ping(&self, machine: &MachineMembership) -> std::result::Result<(), ProbeError> {
        if machine.id == self.machine_id {
            return Err(ProbeError {
                kind: ProbeErrorKind::Timeout,
                detail: "injected probe timeout".into(),
            });
        }
        Ok(())
    }
}

async fn seeded_store_with_machines(machine_ids: &[&str]) -> StoreDriver {
    let store = StoreDriver::memory();
    for machine_id in machine_ids {
        store
            .upsert_self_machine(&test_machine(machine_id, MachineLifecycle::Active))
            .await
            .expect("seed machine");
    }
    store
}

async fn counting_store_with_machines(machine_ids: &[&str]) -> (StoreDriver, Arc<CountingBackend>) {
    let backend = Arc::new(CountingBackend::new());
    let store = StoreDriver::new(
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
    );
    for machine_id in machine_ids {
        store
            .upsert_self_machine(&test_machine(machine_id, MachineLifecycle::Active))
            .await
            .expect("seed machine");
    }
    (store, backend)
}

async fn assert_default_phase_record(
    store: &StoreDriver,
    namespace: &Namespace,
    deploy_id: &DeployId,
    expected_state: &str,
    failure_contains: Option<&str>,
) {
    let phases = store
        .list_deploy_phases(namespace, deploy_id)
        .await
        .expect("list deploy phases");
    let [phase] = phases.as_slice() else {
        panic!("expected one default deploy phase record, got {phases:?}");
    };
    assert_eq!(phase.namespace, *namespace);
    assert_eq!(phase.deploy_id, *deploy_id);
    assert_eq!(phase.phase_id, DeployPhaseId::new("deploy"));
    assert_eq!(phase.name, "Deploy");
    assert_eq!(phase.order, 0);
    assert_eq!(phase.commit_policy(), DeployPhaseCommitPolicy::EndOfDeploy);
    assert_eq!(phase.rollback_policy, DeployPhaseRollbackPolicy::Reversible);
    match (phase.lifecycle_state(), expected_state) {
        (DeployPhaseState::Running, "running") => {}
        (DeployPhaseState::Succeeded { completed_at }, "succeeded") => {
            assert!(completed_at >= phase.started_at);
        }
        (
            DeployPhaseState::Failed {
                completed_at,
                failure,
            },
            "failed",
        ) => {
            assert!(completed_at >= phase.started_at);
            let Some(expected) = failure_contains else {
                panic!("failed phase assertion requires expected failure text");
            };
            assert!(!failure.code.is_empty());
            assert!(
                failure.message.contains(expected),
                "failure message should contain {expected:?}: {}",
                failure.message
            );
        }
        (actual, expected) => panic!("expected phase state {expected}, got {actual:?}"),
    }
}

async fn assert_phase_record_state(
    store: &StoreDriver,
    namespace: &Namespace,
    deploy_id: &DeployId,
    phase_id: &str,
    expected_state: &str,
    failure_contains: Option<&str>,
) {
    let phase = store
        .get_deploy_phase(namespace, deploy_id, &DeployPhaseId::new(phase_id))
        .await
        .expect("get deploy phase")
        .unwrap_or_else(|| panic!("expected deploy phase {phase_id}"));
    match (phase.lifecycle_state(), expected_state) {
        (DeployPhaseState::Running, "running") => {}
        (DeployPhaseState::Succeeded { completed_at }, "succeeded") => {
            assert!(completed_at >= phase.started_at);
        }
        (
            DeployPhaseState::Failed {
                completed_at,
                failure,
            },
            "failed",
        ) => {
            assert!(completed_at >= phase.started_at);
            let Some(expected) = failure_contains else {
                panic!("failed phase assertion requires expected failure text");
            };
            assert!(
                failure.message.contains(expected),
                "failure message should contain {expected:?}: {}",
                failure.message
            );
        }
        (actual, expected) => panic!("expected phase state {expected}, got {actual:?}"),
    }
}

fn test_manifest(services: Vec<ServiceSpec>) -> DeployManifest {
    DeployManifest {
        namespace: Namespace::new("test"),
        intent: None,
        volumes: Vec::new(),
        services,
    }
}

fn checkpointed_db_web_manifest() -> DeployManifest {
    let mut manifest = test_manifest(vec![
        test_service_spec("db", Placement::replicated(1), "postgres:17"),
        test_service_spec("web", Placement::replicated(1), "nginx:1.27"),
    ]);
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![
            DeployPhaseIntent {
                phase_id: "db".into(),
                name: Some("Database".into()),
                after: Vec::new(),
                services: vec!["db".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::Checkpoint,
                rollback_policy: DeployPhaseRollbackPolicy::ForwardOnly,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
            DeployPhaseIntent {
                phase_id: "web".into(),
                name: Some("Web".into()),
                after: vec!["db".into()],
                services: vec!["web".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
        ],
    });
    manifest
}

fn end_of_deploy_db_web_manifest() -> DeployManifest {
    let mut manifest = test_manifest(vec![
        test_service_spec("db", Placement::replicated(1), "postgres:17"),
        test_service_spec("web", Placement::replicated(1), "nginx:1.27"),
    ]);
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![
            DeployPhaseIntent {
                phase_id: "db".into(),
                name: Some("Database".into()),
                after: Vec::new(),
                services: vec!["db".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
            DeployPhaseIntent {
                phase_id: "web".into(),
                name: Some("Web".into()),
                after: vec!["db".into()],
                services: vec!["web".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
        ],
    });
    manifest
}

fn end_of_deploy_db_checkpoint_web_manifest() -> DeployManifest {
    let mut manifest = end_of_deploy_db_web_manifest();
    let Some(intent) = manifest.intent.as_mut() else {
        panic!("expected deploy intent");
    };
    let Some(web_phase) = intent
        .phases
        .iter_mut()
        .find(|phase| phase.phase_id == "web")
    else {
        panic!("expected web phase");
    };
    web_phase.commit_policy = DeployPhaseCommitPolicy::Checkpoint;
    web_phase.rollback_policy = DeployPhaseRollbackPolicy::ForwardOnly;
    manifest
}

fn checkpointed_db_volume_web_manifest() -> DeployManifest {
    let mut db = test_service_spec("db", Placement::replicated(1), "postgres:17");
    db.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![
        db,
        test_service_spec("web", Placement::replicated(1), "nginx:1.27"),
    ]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![
            DeployPhaseIntent {
                phase_id: "db".into(),
                name: Some("Database".into()),
                after: Vec::new(),
                services: vec!["db".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::Checkpoint,
                rollback_policy: DeployPhaseRollbackPolicy::ForwardOnly,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
            DeployPhaseIntent {
                phase_id: "web".into(),
                name: Some("Web".into()),
                after: vec!["db".into()],
                services: vec!["web".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
        ],
    });
    manifest
}

fn volume_manifest() -> DeployManifest {
    let mut service = test_service_spec("db", Placement::replicated(1), "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest
}

fn test_volume(name: &str, scope: VolumeScope) -> VolumeDeclaration {
    VolumeDeclaration {
        name: name.into(),
        scope,
        quota: "1G".into(),
        mode: "0750".into(),
        owner: "999:999".into(),
    }
}

async fn seed_volume(
    store: &StoreDriver,
    namespace: &Namespace,
    volume_name: &str,
    machine_id: &str,
) {
    seed_volume_with(
        store,
        namespace,
        volume_name,
        machine_id,
        "1G",
        "0750",
        "999:999",
    )
    .await;
}

async fn seed_volume_with(
    store: &StoreDriver,
    namespace: &Namespace,
    volume_name: &str,
    machine_id: &str,
    quota: &str,
    mode: &str,
    owner: &str,
) {
    seed_volume_with_attached_services(
        store,
        namespace,
        volume_name,
        machine_id,
        quota,
        mode,
        owner,
        Vec::new(),
    )
    .await;
}

async fn seed_volume_with_attached_services(
    store: &StoreDriver,
    namespace: &Namespace,
    volume_name: &str,
    machine_id: &str,
    quota: &str,
    mode: &str,
    owner: &str,
    attached_services: Vec<String>,
) {
    seed_volume_with_scope_and_attached_services(
        store,
        namespace,
        volume_name,
        machine_id,
        VolumeScope::Single,
        quota,
        mode,
        owner,
        attached_services,
    )
    .await;
}

async fn seed_volume_with_scope(
    store: &StoreDriver,
    namespace: &Namespace,
    volume_name: &str,
    machine_id: &str,
    scope: VolumeScope,
) {
    seed_volume_with_scope_and_attached_services(
        store,
        namespace,
        volume_name,
        machine_id,
        scope,
        "1G",
        "0750",
        "999:999",
        Vec::new(),
    )
    .await;
}

async fn seed_volume_with_scope_and_attached_services(
    store: &StoreDriver,
    namespace: &Namespace,
    volume_name: &str,
    machine_id: &str,
    scope: VolumeScope,
    quota: &str,
    mode: &str,
    owner: &str,
    attached_services: Vec<String>,
) {
    let deploy_id = DeployId::new(format!("seed-{volume_name}"));
    let volume = VolumeRecord {
        namespace: namespace.clone(),
        volume_name: volume_name.into(),
        scope,
        machine_id: MachineId::new(machine_id),
        quota: quota.into(),
        mode: mode.into(),
        owner: owner.into(),
        attached_services,
        created_at: 1,
        created_by_deploy_id: deploy_id.clone(),
        last_modified_at: 1,
        last_modified_by_deploy_id: deploy_id.clone(),
    };
    let deploy = DeployRecord {
        deploy_id,
        namespace: namespace.clone(),
        coordinator_machine_id: MachineId::new("local"),
        manifest_hash: "seed".into(),
        started_at: 1,
        state: DeployRecordState::Committed {
            committed_at: 1,
            finished_at: 1,
            summary_json: "{}".into(),
        },
    };
    store
        .commit_deploy(&DeployCommit {
            namespace: namespace.clone(),
            revisions: Vec::new(),
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases: Vec::new(),
            volumes: vec![volume],
            deploy,
        })
        .await
        .expect("seed volume");
}

fn test_service_spec(name: &str, placement: Placement, image: &str) -> ServiceSpec {
    ServiceSpec {
        name: name.into(),
        placement,
        template: ContainerSpec {
            image: image.into(),
            command: None,
            entrypoint: None,
            env: BTreeMap::new(),
            mounts: Vec::new(),
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            privileged: false,
            user: None,
            stop_grace_period: None,
            pid_mode: None,
            pull_policy: PullPolicy::IfNotPresent,
            resources: Resources::empty(),
            sysctls: BTreeMap::new(),
        },
        network: NetworkMode::Overlay,
        service_ports: Vec::new(),
        publish: Vec::new(),
        routes: Vec::new(),
        readiness: None,
        rollout: RolloutStrategy::Recreate,
        labels: BTreeMap::new(),
        restart: RestartPolicy::UnlessStopped,
    }
}

fn test_image_digest(hex: char) -> ImageDigest {
    ImageDigest::try_new(format!("sha256:{}", hex.to_string().repeat(64))).expect("valid digest")
}

fn present_image_record(machine_id: &str, digest: ImageDigest) -> ImageAvailabilityRecord {
    ImageAvailabilityRecord {
        machine_id: MachineId::new(machine_id),
        digest: digest.clone(),
        presence: ImagePresence::Present {
            artifact: ImageArtifact {
                image: ImageRef::digest_only(digest),
                platform: None,
                provenance: ImageArtifactProvenance::External { source: None },
                created_at: 10,
            },
            recorded_at: 10,
            source_operation_id: None,
        },
        updated_at: 10,
    }
}

fn absent_image_record(machine_id: &str, digest: ImageDigest) -> ImageAvailabilityRecord {
    ImageAvailabilityRecord {
        machine_id: MachineId::new(machine_id),
        digest,
        presence: ImagePresence::Absent { observed_at: 10 },
        updated_at: 10,
    }
}

fn http_route_service_spec(name: &str, hostname: &str) -> ServiceSpec {
    let mut spec = test_service_spec(name, Placement::replicated(1), "nginx:1.27");
    spec.service_ports = vec![ServicePort {
        name: "http".into(),
        container_port: 8080,
        protocol: PortProtocol::Tcp,
    }];
    spec.routes = vec![RouteSpec::Http(HttpRoute {
        service_port: "http".into(),
        hostnames: vec![hostname.into()],
        path_prefix: "/".into(),
    })];
    spec
}

async fn seed_committed_http_release(
    store: &StoreDriver,
    namespace: &str,
    service: &str,
    hostname: &str,
) {
    let namespace = Namespace::new(namespace);
    let spec = http_route_service_spec(service, hostname);
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .commit_deploy(&DeployCommit {
            namespace: namespace.clone(),
            revisions: vec![crate::model::ServiceRevisionRecord {
                namespace: namespace.clone(),
                service: service.into(),
                revision_hash: revision_hash.clone(),
                spec_json: spec
                    .canonical_revision_json()
                    .expect("canonical revision json"),
                created_by: MachineId::new("seed"),
                created_at: 0,
            }],
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases: vec![test_release(
                &namespace,
                service,
                &revision_hash,
                Vec::new(),
            )],
            volumes: Vec::new(),
            deploy: test_deploy_record(&namespace, "seed-deploy"),
        })
        .await
        .expect("seed release");
}

async fn seed_committed_service_release(
    store: &StoreDriver,
    namespace: &Namespace,
    spec: ServiceSpec,
) {
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .commit_deploy(&DeployCommit {
            namespace: namespace.clone(),
            revisions: vec![crate::model::ServiceRevisionRecord {
                namespace: namespace.clone(),
                service: spec.name.clone(),
                revision_hash: revision_hash.clone(),
                spec_json: spec
                    .canonical_revision_json()
                    .expect("canonical revision json"),
                created_by: MachineId::new("seed"),
                created_at: 0,
            }],
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases: vec![test_release(
                namespace,
                &spec.name,
                &revision_hash,
                Vec::new(),
            )],
            volumes: Vec::new(),
            deploy: test_deploy_record(namespace, "seed-deploy"),
        })
        .await
        .expect("seed service release");
}

fn test_release(
    namespace: &Namespace,
    service: &str,
    revision_hash: &str,
    slots: Vec<ServiceReleaseSlot>,
) -> ServiceReleaseRecord {
    ServiceReleaseRecord {
        namespace: namespace.clone(),
        service: service.into(),
        release: ServiceRelease::direct(revision_hash, slots, DeployId::new("deploy-1"), 0),
    }
}

fn test_deploy_record(namespace: &Namespace, deploy_id: &str) -> DeployRecord {
    DeployRecord {
        deploy_id: DeployId::new(deploy_id),
        namespace: namespace.clone(),
        coordinator_machine_id: MachineId::new("local"),
        manifest_hash: "manifest".into(),
        started_at: 0,
        state: DeployRecordState::Committed {
            committed_at: 0,
            finished_at: 0,
            summary_json: "{}".into(),
        },
    }
}

fn test_slot(
    slot_id: &str,
    machine_id: &str,
    instance_id: &str,
    revision_hash: &str,
) -> ServiceReleaseSlot {
    ServiceReleaseSlot {
        slot_id: SlotId::new(slot_id),
        machine_id: MachineId::new(machine_id),
        active_instance_id: InstanceId::new(instance_id),
        revision_hash: revision_hash.into(),
    }
}

fn test_machine(id: &str, lifecycle: MachineLifecycle) -> MachineMembership {
    test_machine_in_region(id, lifecycle, RegionRole::HomeData)
}

fn test_machine_in_region(
    id: &str,
    lifecycle: MachineLifecycle,
    region_role: RegionRole,
) -> MachineMembership {
    MachineMembership {
        id: MachineId::new(id),
        public_key: PublicKey([7; 32]),
        overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
        topology: MachineTopology::local(),
        region_role,
        subnet: None,
        bridge_ip: None,
        endpoints: vec!["127.0.0.1:51820".into()],
        lifecycle,
        storage_role: ployz_model::StorageParticipation::default_authority().into(),
        created_at: 0,
        updated_at: 0,
        labels: BTreeMap::new(),
    }
}

fn test_instance_status(
    namespace: &Namespace,
    service: &str,
    slot_id: &str,
    machine_id: &str,
    instance_id: &str,
    revision_hash: &str,
) -> InstanceStatusRecord {
    InstanceStatusRecord {
        instance_id: InstanceId::new(instance_id),
        namespace: namespace.clone(),
        service: service.into(),
        slot_id: SlotId::new(slot_id),
        machine_id: MachineId::new(machine_id),
        revision_hash: revision_hash.into(),
        deploy_id: DeployId::new("previous-deploy"),
        docker_container_id: format!("container-{instance_id}"),
        overlay_ip: None,
        backend_ports: BTreeMap::new(),
        phase: InstancePhase::Ready,
        ready: true,
        drain_state: DrainState::None,
        error: None,
        started_at: 0,
        updated_at: 0,
    }
}

struct CountingBackend {
    store: Arc<MemoryStore>,
    service: Arc<MemoryService>,
    commit_calls: AtomicUsize,
    deploy_status_writes: AtomicUsize,
    committed_status_writes: AtomicUsize,
    fail_commit: AtomicBool,
    fail_committed_status_writes: AtomicBool,
    fail_committed_status_writes_after_first: AtomicBool,
    fail_succeeded_phase_upsert: AtomicBool,
    fail_running_phase_upsert: AtomicBool,
    succeeded_phase_upserts: AtomicUsize,
    pending_phase_upserts: AtomicUsize,
    fail_pending_phase_upsert_on: AtomicUsize,
    deploy_status_records: Mutex<Vec<DeployRecord>>,
}

impl CountingBackend {
    fn new() -> Self {
        Self {
            store: Arc::new(MemoryStore::new()),
            service: Arc::new(MemoryService::new()),
            commit_calls: AtomicUsize::new(0),
            deploy_status_writes: AtomicUsize::new(0),
            committed_status_writes: AtomicUsize::new(0),
            fail_commit: AtomicBool::new(false),
            fail_committed_status_writes: AtomicBool::new(false),
            fail_committed_status_writes_after_first: AtomicBool::new(false),
            fail_succeeded_phase_upsert: AtomicBool::new(false),
            fail_running_phase_upsert: AtomicBool::new(false),
            succeeded_phase_upserts: AtomicUsize::new(0),
            pending_phase_upserts: AtomicUsize::new(0),
            fail_pending_phase_upsert_on: AtomicUsize::new(0),
            deploy_status_records: Mutex::new(Vec::new()),
        }
    }

    fn commit_count(&self) -> usize {
        self.commit_calls.load(Ordering::SeqCst)
    }

    fn deploy_status_write_count(&self) -> usize {
        self.deploy_status_writes.load(Ordering::SeqCst)
    }

    fn reset_counts(&self) {
        self.commit_calls.store(0, Ordering::SeqCst);
        self.deploy_status_writes.store(0, Ordering::SeqCst);
        self.committed_status_writes.store(0, Ordering::SeqCst);
        self.succeeded_phase_upserts.store(0, Ordering::SeqCst);
    }

    fn fail_committed_status_writes_after_first(&self, fail: bool) {
        self.fail_committed_status_writes_after_first
            .store(fail, Ordering::SeqCst);
    }

    fn fail_committed_status_writes(&self, fail: bool) {
        self.fail_committed_status_writes
            .store(fail, Ordering::SeqCst);
    }

    fn fail_commit(&self, fail: bool) {
        self.fail_commit.store(fail, Ordering::SeqCst);
    }

    fn fail_succeeded_phase_upsert(&self, fail: bool) {
        self.fail_succeeded_phase_upsert
            .store(fail, Ordering::SeqCst);
    }

    fn succeeded_phase_upsert_count(&self) -> usize {
        self.succeeded_phase_upserts.load(Ordering::SeqCst)
    }

    fn fail_running_phase_upsert(&self, fail: bool) {
        self.fail_running_phase_upsert.store(fail, Ordering::SeqCst);
    }

    fn fail_pending_phase_upsert_on(&self, call: usize) {
        self.pending_phase_upserts.store(0, Ordering::SeqCst);
        self.fail_pending_phase_upsert_on
            .store(call, Ordering::SeqCst);
    }

    async fn last_deploy_status_write(&self) -> Option<DeployRecord> {
        self.deploy_status_records.lock().await.last().cloned()
    }

    async fn deploy_status_writes(&self) -> Vec<DeployRecord> {
        self.deploy_status_records.lock().await.clone()
    }
}

#[async_trait]
impl MachineMembershipStore for CountingBackend {
    async fn init(&self) -> PloyzResult<()> {
        self.store.init().await
    }

    async fn list_machines(&self) -> PloyzResult<Vec<MachineMembership>> {
        self.store.list_machines().await
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> PloyzResult<()> {
        self.store.upsert_self_machine(record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> PloyzResult<()> {
        self.store.delete_machine(id).await
    }

    async fn subscribe_machines(&self) -> PloyzResult<MachineSubscription> {
        self.store.subscribe_machines().await
    }
}

#[async_trait]
impl InviteStore for CountingBackend {
    async fn create_invite(&self, invite: &ployz_model::InviteRecord) -> PloyzResult<()> {
        self.store.create_invite(invite).await
    }

    async fn get_invite(&self, invite_id: &str) -> PloyzResult<Option<ployz_model::InviteRecord>> {
        self.store.get_invite(invite_id).await
    }

    async fn list_invites(&self) -> PloyzResult<Vec<ployz_model::InviteRecord>> {
        self.store.list_invites().await
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> PloyzResult<ployz_model::InviteRecord> {
        self.store
            .redeem_invite(invite_id, machine_id, now_unix_secs)
            .await
    }

    async fn revoke_invite(
        &self,
        invite_id: &str,
        now_unix_secs: u64,
    ) -> PloyzResult<ployz_model::InviteRecord> {
        self.store.revoke_invite(invite_id, now_unix_secs).await
    }
}

#[async_trait]
impl RoutingStateStore for CountingBackend {
    async fn load_routing_state(&self) -> PloyzResult<crate::model::RoutingState> {
        self.store.load_routing_state().await
    }

    async fn subscribe_routing_events(&self) -> PloyzResult<RoutingEventSubscription> {
        RoutingStateStore::subscribe_routing_events(self.store.as_ref()).await
    }
}

#[async_trait]
impl ImageAvailabilityStore for CountingBackend {
    async fn upsert_image_availability(
        &self,
        record: &ployz_model::ImageAvailabilityRecord,
    ) -> PloyzResult<()> {
        self.store.upsert_image_availability(record).await
    }

    async fn get_image_availability(
        &self,
        machine_id: &MachineId,
        digest: &ployz_model::ImageDigest,
    ) -> PloyzResult<Option<ployz_model::ImageAvailabilityRecord>> {
        self.store.get_image_availability(machine_id, digest).await
    }

    async fn list_image_availability(
        &self,
    ) -> PloyzResult<Vec<ployz_model::ImageAvailabilityRecord>> {
        self.store.list_image_availability().await
    }
}

#[async_trait]
impl CertificateStore for CountingBackend {
    async fn get_acme_account(&self, issuer_url: &str) -> PloyzResult<Option<AcmeAccountRecord>> {
        self.store.get_acme_account(issuer_url).await
    }

    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> PloyzResult<()> {
        self.store.upsert_acme_account(record).await
    }

    async fn list_certificates(&self) -> PloyzResult<Vec<CertificateRecord>> {
        self.store.list_certificates().await
    }

    async fn get_certificate(&self, hostname: &str) -> PloyzResult<Option<CertificateRecord>> {
        self.store.get_certificate(hostname).await
    }

    async fn upsert_certificate(&self, record: &CertificateRecord) -> PloyzResult<()> {
        self.store.upsert_certificate(record).await
    }

    async fn list_acme_challenges(&self) -> PloyzResult<Vec<AcmeChallengeRecord>> {
        self.store.list_acme_challenges().await
    }

    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> PloyzResult<()> {
        self.store.upsert_acme_challenge(record).await
    }

    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> PloyzResult<()> {
        self.store.delete_acme_challenge(hostname, token).await
    }

    async fn upsert_acme_challenge_readiness(
        &self,
        record: &AcmeChallengeReadinessRecord,
    ) -> PloyzResult<()> {
        self.store.upsert_acme_challenge_readiness(record).await
    }

    async fn list_acme_challenge_readiness(
        &self,
        hostname: &str,
        token: &str,
    ) -> PloyzResult<Vec<AcmeChallengeReadinessRecord>> {
        self.store
            .list_acme_challenge_readiness(hostname, token)
            .await
    }

    async fn subscribe_certificates(
        &self,
    ) -> PloyzResult<ployz_store_api::CertificateSubscription> {
        self.store.subscribe_certificates().await
    }

    async fn subscribe_acme_challenges(
        &self,
    ) -> PloyzResult<ployz_store_api::AcmeChallengeSubscription> {
        self.store.subscribe_acme_challenges().await
    }
}

#[async_trait]
impl DeployStore for CountingBackend {
    async fn list_deploy_revisions(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<ServiceRevisionRecord>> {
        self.store.list_deploy_revisions(namespace).await
    }

    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<ServiceReleaseRecord>> {
        self.store.list_deploy_releases(namespace).await
    }

    async fn list_volumes(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<crate::model::VolumeRecord>> {
        self.store.list_volumes(namespace).await
    }

    async fn list_service_branch_lineage(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<ServiceBranchLineageRecord>> {
        self.store.list_service_branch_lineage(namespace).await
    }

    async fn list_volume_movements(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<VolumeMovementRecord>> {
        self.store.list_volume_movements(namespace).await
    }

    async fn list_volume_branches(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<VolumeBranchLineageRecord>> {
        self.store.list_volume_branches(namespace).await
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> PloyzResult<Option<crate::model::VolumeRecord>> {
        self.store.get_volume(namespace, volume_name).await
    }

    async fn write_deploy_status(&self, deploy: &DeployRecord) -> PloyzResult<()> {
        self.deploy_status_writes.fetch_add(1, Ordering::SeqCst);
        self.deploy_status_records.lock().await.push(deploy.clone());
        if matches!(
            deploy.state(),
            DeployState::Committed | DeployState::CleanupPending
        ) {
            let committed_writes = self.committed_status_writes.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_committed_status_writes.load(Ordering::SeqCst)
                || (committed_writes > 1
                    && self
                        .fail_committed_status_writes_after_first
                        .load(Ordering::SeqCst))
            {
                return Err(Error::operation(
                    "counting_write_deploy_status",
                    "injected committed status failure",
                ));
            }
        }
        self.store.write_deploy_status(deploy).await
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> PloyzResult<()> {
        self.commit_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_commit.load(Ordering::SeqCst) {
            return Err(Error::operation(
                "counting_commit_deploy",
                "injected commit failure",
            ));
        }
        self.store.commit_deploy(command).await
    }

    async fn get_deploy_commit(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
    ) -> PloyzResult<Option<DeployCommit>> {
        self.store.get_deploy_commit(namespace, deploy_id).await
    }

    async fn get_deploy(
        &self,
        deploy_id: &DeployId,
    ) -> PloyzResult<Option<crate::model::DeployRecord>> {
        self.store.get_deploy(deploy_id).await
    }

    async fn write_prepared_deploy(&self, prepared: &PreparedDeployRecord) -> PloyzResult<()> {
        self.store.write_prepared_deploy(prepared).await
    }

    async fn get_prepared_deploy(
        &self,
        prepared_deploy_id: &DeployId,
    ) -> PloyzResult<Option<PreparedDeployRecord>> {
        self.store.get_prepared_deploy(prepared_deploy_id).await
    }

    async fn mark_prepared_deploy_applied(
        &self,
        prepared_deploy_id: &DeployId,
        updated_at: u64,
    ) -> PloyzResult<PreparedDeployRecord> {
        self.store
            .mark_prepared_deploy_applied(prepared_deploy_id, updated_at)
            .await
    }

    async fn expire_prepared_deploy(
        &self,
        prepared_deploy_id: &DeployId,
        updated_at: u64,
    ) -> PloyzResult<PreparedDeployRecord> {
        self.store
            .expire_prepared_deploy(prepared_deploy_id, updated_at)
            .await
    }

    async fn supersede_prepared_deploy(
        &self,
        prepared_deploy_id: &DeployId,
        updated_at: u64,
    ) -> PloyzResult<PreparedDeployRecord> {
        self.store
            .supersede_prepared_deploy(prepared_deploy_id, updated_at)
            .await
    }

    async fn upsert_branch_environment(&self, record: &BranchEnvironmentRecord) -> PloyzResult<()> {
        self.store.upsert_branch_environment(record).await
    }

    async fn get_branch_environment(
        &self,
        target_namespace: &Namespace,
    ) -> PloyzResult<Option<BranchEnvironmentRecord>> {
        self.store.get_branch_environment(target_namespace).await
    }

    async fn list_branch_environments(&self) -> PloyzResult<Vec<BranchEnvironmentRecord>> {
        self.store.list_branch_environments().await
    }

    async fn mark_branch_environment_applying(
        &self,
        target_namespace: &Namespace,
        prepared_deploy_id: &DeployId,
        updated_at: u64,
    ) -> PloyzResult<BranchEnvironmentRecord> {
        self.store
            .mark_branch_environment_applying(target_namespace, prepared_deploy_id, updated_at)
            .await
    }

    async fn mark_branch_environment_active(
        &self,
        target_namespace: &Namespace,
        prepared_deploy_id: &DeployId,
        applied_deploy_id: &DeployId,
        updated_at: u64,
    ) -> PloyzResult<BranchEnvironmentRecord> {
        self.store
            .mark_branch_environment_active(
                target_namespace,
                prepared_deploy_id,
                applied_deploy_id,
                updated_at,
            )
            .await
    }

    async fn mark_branch_environment_failed(
        &self,
        target_namespace: &Namespace,
        prepared_deploy_id: &DeployId,
        failure: &BranchEnvironmentFailure,
        updated_at: u64,
    ) -> PloyzResult<BranchEnvironmentRecord> {
        self.store
            .mark_branch_environment_failed(
                target_namespace,
                prepared_deploy_id,
                failure,
                updated_at,
            )
            .await
    }

    async fn upsert_deploy_phase(&self, phase: &DeployPhaseRecord) -> PloyzResult<()> {
        if matches!(phase.lifecycle_state(), DeployPhaseState::Pending) {
            let call = self.pending_phase_upserts.fetch_add(1, Ordering::SeqCst) + 1;
            if call == self.fail_pending_phase_upsert_on.load(Ordering::SeqCst) {
                return Err(Error::operation(
                    "counting_upsert_deploy_phase",
                    "injected pending phase failure",
                ));
            }
        }
        if matches!(phase.lifecycle_state(), DeployPhaseState::Running)
            && self.fail_running_phase_upsert.load(Ordering::SeqCst)
        {
            return Err(Error::operation(
                "counting_upsert_deploy_phase",
                "injected running phase failure",
            ));
        }
        if matches!(phase.lifecycle_state(), DeployPhaseState::Succeeded { .. })
            && self.fail_succeeded_phase_upsert.load(Ordering::SeqCst)
        {
            self.succeeded_phase_upserts.fetch_add(1, Ordering::SeqCst);
            return Err(Error::operation(
                "counting_upsert_deploy_phase",
                "injected succeeded phase failure",
            ));
        }
        if matches!(phase.lifecycle_state(), DeployPhaseState::Succeeded { .. }) {
            self.succeeded_phase_upserts.fetch_add(1, Ordering::SeqCst);
        }
        self.store.upsert_deploy_phase(phase).await
    }

    async fn get_deploy_phase(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
        phase_id: &DeployPhaseId,
    ) -> PloyzResult<Option<DeployPhaseRecord>> {
        self.store
            .get_deploy_phase(namespace, deploy_id, phase_id)
            .await
    }

    async fn list_deploy_phases(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
    ) -> PloyzResult<Vec<DeployPhaseRecord>> {
        self.store.list_deploy_phases(namespace, deploy_id).await
    }
}

#[async_trait]
impl InstanceStatusStore for CountingBackend {
    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<InstanceStatusRecord>> {
        self.store.list_instance_status(namespace).await
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> PloyzResult<()> {
        self.store.record_instance_status(record).await
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> PloyzResult<()> {
        self.store.remove_instance_status(instance_id).await
    }
}

impl SyncProbe for CountingBackend {}

impl PeerRttStore for CountingBackend {}

#[async_trait]
impl StoreRuntimeControl for CountingBackend {
    async fn start(&self) -> PloyzResult<()> {
        self.service.start().await
    }

    async fn stop(&self) -> PloyzResult<()> {
        self.service.stop().await
    }

    async fn healthy(&self) -> bool {
        self.service.healthy().await
    }
}
