//! Mock Machine RPC and fixtures for deploy_client tests.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use ployz::{
    connect::{
        BoxProxyStream, Client, ConnectError, Connector, SystemConnector, connect_selected_with,
    },
    context::{Connection, ConnectionSource, SelectedConnections},
    deploy::PlanOptions,
};
use ployz_core::{
    AdvertisedEndpoint, AnswerWhen, CapabilityName, ContainerDetails, ContainerId, ContainerKind,
    ContainerList, ContainerLocator, ContainerNow, ContainerPath, ContainerRuntimeObservation,
    ContractDescription, CopyRotation, DockerVolume, DockerVolumeName, EnsuredContainer,
    ExecResponseFrame, HealthObservation, HealthVerdict, HealthcheckSpec, LocalMachinePhase,
    MACHINE_STORAGE_OBSERVATION_CAPABILITY, Machine, MachineDetails, MachineId, MachineImages,
    MachineList, MachineName, MachineObservation, MachineRpcServer, MembershipObservation,
    Namespace, OpaquePayload, PROTOCOL_MAJOR, QualifiedService, RequestedServiceSpec,
    ResolvedServiceSpec, ResolvedUpdateConfig, RetainedEnvReply, RotationEdge, RoutesRead,
    RpcError, RpcErrorCode, RpcRequestBody, RpcResponse, RunTarget, ServiceId, ServiceMount,
    ServiceVolume, ServiceVolumeGraph, ServiceVolumeReference, UpdateOrder, VolumeInventory,
    WireGuardPublicKey,
};
use ployz_testkit::{
    fake_machine::{Fake, FakeMachine, FakeStream},
    rotation,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{
    Request, Response, Status, Streaming,
    transport::{Channel, Server},
};

#[path = "../support/inspect_telemetry.rs"]
mod inspect_telemetry_fixture;

#[path = "../support/build.rs"]
mod build_fixture;
pub(super) use build_fixture::BuildFixture;

#[derive(Clone)]
pub(super) struct DeployService {
    pub(super) builds: Option<Arc<BuildFixture>>,
    machines: Vec<MachineObservation>,
    create_volume_error: Option<RpcError>,
    create_volume_verification_error: Option<RpcError>,
    containers: Arc<AtomicUsize>,
    created_namespaces: Arc<Mutex<Vec<Namespace>>>,
    created_specs: Arc<Mutex<Vec<ResolvedServiceSpec>>>,
    listed_containers: Arc<Mutex<Vec<ployz_core::ContainerObservation>>>,
    mutating_rpcs: Arc<AtomicUsize>,
    /// Containers created per creation key, so a resent run answers the same Container.
    slots: Arc<Mutex<BTreeMap<String, ContainerId>>>,
    inspect_error: Option<RpcError>,
    exec_exit: Option<i32>,
    runtime: ContainerRuntimeObservation,
    /// Docker OOM-killed a process of each Container, as its inspect reports.
    oom_killed: bool,
    /// Once released, a Starting Container turns healthy and starts serving.
    health_release: Option<Arc<tokio::sync::watch::Sender<bool>>>,
    health_waits: Arc<tokio::sync::Notify>,
    /// Docker restarted each Container after its exit, and the new run printed `booting`.
    rebooted: bool,
    /// Between that exit and `booting`, a run exited having printed nothing.
    silent_run: bool,
    /// The Machine answers an In ask without a health verdict and leaves the copy where it was.
    unread_health: bool,
    effective_healthcheck: Option<HealthcheckSpec>,
    removed: Arc<Mutex<BTreeSet<ContainerId>>>,
    pub(super) history_requests: Arc<Mutex<Vec<ployz_core::LogHistoryRequest>>>,
    /// Machines whose Log Store answers with an error row, and its message.
    pub(super) history_failures: BTreeMap<MachineId, String>,
    /// Containers the Machine can no longer inspect, as when one has been removed.
    pub(super) uninspectable: BTreeSet<ContainerId>,
    /// Containers whose inspect runs out of time, as when Docker stalls.
    pub(super) stalled_inspects: BTreeSet<ContainerId>,
    /// The env the Log Store kept per Container; `None` is a daemon predating the route.
    pub(super) retained_envs: Option<BTreeMap<ContainerId, BTreeMap<String, String>>>,
    start_error: Option<RpcError>,
    listing_failures: Vec<MachineId>,
    pub(super) listing_blocked: Option<Arc<tokio::sync::Notify>>,
    /// What `ListMachines` answers instead of the Machines, once set.
    pub(super) machines_error: Arc<Mutex<Option<RpcError>>>,
    /// Once set, counts `DescribeContract` calls and answers only the first; the rest
    /// never return.
    pub(super) describe_once: Option<Arc<AtomicUsize>>,
    /// Once set, `DescribeContract` is refused as unauthenticated, with this message.
    pub(super) describe_refusal: Arc<Mutex<Option<String>>>,
    /// Docker Volumes created and not yet removed.
    pub(super) volumes: Arc<Mutex<Vec<DockerVolume>>>,
    /// Volume copies every Machine reports from storage.
    pub(super) copies: Arc<Mutex<BTreeMap<DockerVolumeName, ployz_core::ProvisionedCopy>>>,
    /// Removed copies stay. A listed Container no want touched keeps the rotation it
    /// was listed with.
    rotations: Arc<Mutex<BTreeMap<ContainerId, Rotated>>>,
    /// Copies that were In and left: a proxy that never drops a route keeps them.
    unrouted: Arc<Mutex<BTreeSet<ContainerId>>>,
    unroutes: Arc<tokio::sync::Notify>,
    /// What each proxy last routed, kept by the real rule.
    served: Arc<Mutex<BTreeMap<MachineId, ployz_core::ProxyRoutes>>>,
    /// Machines whose proxy adds copies as they go In but never drops one.
    sticky_proxies: BTreeSet<MachineId>,
    image_pulls: Arc<Mutex<Vec<ImagePullCall>>>,
    /// Creations that arrived without a prepared image, so the Machine pulled inside them.
    creation_pulls: Arc<AtomicUsize>,
    /// Machines whose registry rejects every pull.
    pull_failures: BTreeSet<MachineId>,
    /// What a rejected pull answers.
    pull_failure: RpcError,
}

#[derive(Clone, Debug)]
pub(super) struct Rotated {
    service: QualifiedService,
    pub(super) rotation: CopyRotation,
}

/// One `PullImage` a Machine answered.
#[derive(Clone, Debug)]
pub(super) struct ImagePullCall {
    pub(super) machine: MachineId,
    pub(super) image: String,
    pub(super) policy: ployz_core::PullPolicy,
    /// Mutating Machine RPCs answered before this pull.
    pub(super) mutations_before: usize,
}

impl DeployService {
    pub(super) fn new(machine: MachineObservation) -> Self {
        Self {
            machines: vec![machine],
            ..Self::empty()
        }
    }

    pub(super) fn empty() -> Self {
        Self {
            builds: None,
            machines: Vec::new(),
            create_volume_error: None,
            create_volume_verification_error: None,
            containers: Arc::new(AtomicUsize::new(0)),
            created_namespaces: Arc::new(Mutex::new(Vec::new())),
            created_specs: Arc::new(Mutex::new(Vec::new())),
            listed_containers: Arc::new(Mutex::new(Vec::new())),
            mutating_rpcs: Arc::new(AtomicUsize::new(0)),
            slots: Arc::new(Mutex::new(BTreeMap::new())),
            inspect_error: None,
            exec_exit: None,
            runtime: ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            oom_killed: false,
            health_release: None,
            health_waits: Arc::new(tokio::sync::Notify::new()),
            rebooted: false,
            silent_run: false,
            unread_health: false,
            effective_healthcheck: None,
            removed: Arc::new(Mutex::new(BTreeSet::new())),
            history_requests: Arc::new(Mutex::new(Vec::new())),
            history_failures: BTreeMap::new(),
            uninspectable: BTreeSet::new(),
            stalled_inspects: BTreeSet::new(),
            retained_envs: Some(BTreeMap::new()),
            start_error: None,
            listing_failures: Vec::new(),
            listing_blocked: None,
            machines_error: Arc::new(Mutex::new(None)),
            describe_once: None,
            describe_refusal: Arc::new(Mutex::new(None)),
            volumes: Arc::new(Mutex::new(Vec::new())),
            copies: Arc::new(Mutex::new(BTreeMap::new())),
            rotations: Arc::new(Mutex::new(BTreeMap::new())),
            unrouted: Arc::new(Mutex::new(BTreeSet::new())),
            unroutes: Arc::new(tokio::sync::Notify::new()),
            served: Arc::default(),
            sticky_proxies: BTreeSet::new(),
            image_pulls: Arc::new(Mutex::new(Vec::new())),
            creation_pulls: Arc::new(AtomicUsize::new(0)),
            pull_failures: BTreeSet::new(),
            pull_failure: RpcError {
                code: RpcErrorCode::Internal,
                message: "registry refused the pull".into(),
                details: Value::Null,
                cause: Vec::new(),
            },
        }
    }

    pub(super) fn fail_create_volume(mut self, message: &str) -> Self {
        self.create_volume_error = Some(RpcError {
            code: RpcErrorCode::Internal,
            message: message.into(),
            details: Value::Null,
            cause: Vec::new(),
        });
        self
    }

    pub(super) fn fail_create_volume_verification(mut self, message: &str) -> Self {
        self.create_volume_verification_error = Some(RpcError {
            code: RpcErrorCode::Unavailable,
            message: message.into(),
            details: Value::Null,
            cause: Vec::new(),
        });
        self
    }

    pub(super) fn with_exec_exit(mut self, code: i32) -> Self {
        self.exec_exit = Some(code);
        self
    }

    pub(super) fn fail_starts(mut self, message: &str) -> Self {
        self.start_error = Some(RpcError {
            code: RpcErrorCode::Unavailable,
            message: message.into(),
            details: Value::Null,
            cause: Vec::new(),
        });
        self
    }

    /// `machine_id` answers every container listing with an error.
    pub(super) fn fail_listing_on(mut self, machine_id: MachineId) -> Self {
        self.listing_failures.push(machine_id);
        self
    }

    pub(super) fn never_unroute_on(mut self, machine_id: MachineId) -> Self {
        self.sticky_proxies.insert(machine_id);
        self
    }

    pub(super) fn unread_health(mut self) -> Self {
        self.unread_health = true;
        self
    }

    pub(super) fn hold_health(mut self) -> Self {
        self.runtime = ContainerRuntimeObservation::Running {
            health: HealthObservation::Starting,
        };
        self
    }

    /// Containers report Starting until [`Self::release_health`].
    pub(super) fn turn_healthy_on_release(mut self) -> Self {
        self.runtime = ContainerRuntimeObservation::Running {
            health: HealthObservation::Starting,
        };
        self.health_release = Some(Arc::new(tokio::sync::watch::Sender::new(false)));
        self
    }

    pub(super) fn release_health(&self) {
        self.health_release
            .as_ref()
            .expect("turn_healthy_on_release")
            .send_replace(true);
    }

    /// Returns once a health wait has begun, even one that began before this call.
    pub(super) async fn health_waited(&self) {
        self.health_waits.notified().await;
    }

    /// Containers report `healthcheck` as the one Docker runs for them.
    pub(super) fn with_effective_healthcheck(mut self, healthcheck: HealthcheckSpec) -> Self {
        self.effective_healthcheck = Some(healthcheck);
        self
    }

    fn current_runtime(&self) -> ContainerRuntimeObservation {
        if self
            .health_release
            .as_ref()
            .is_some_and(|released| *released.borrow())
        {
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            }
        } else {
            self.runtime.clone()
        }
    }

    /// The verdict a Machine answers a health wait with; a held health wait times
    /// out at once when its healthcheck has a deadline, and without one never answers.
    async fn settle(&self) -> HealthVerdict {
        self.health_waits.notify_one();
        if let Some(released) = &self.health_release {
            let _ = released.subscribe().wait_for(|released| *released).await;
        }
        let observation = self.current_runtime();
        let ContainerRuntimeObservation::Running { health } = &observation else {
            return HealthVerdict::Failed {
                observation,
                last_check: None,
                oom_killed: self.oom_killed,
            };
        };
        if *health == HealthObservation::Starting {
            let Some(deadline) = self
                .effective_healthcheck
                .as_ref()
                .and_then(HealthcheckSpec::deadline)
            else {
                return std::future::pending().await;
            };
            return HealthVerdict::TimedOut {
                deadline_millis: u64::try_from(deadline.as_millis()).ok(),
                last_check: None,
                oom_killed: false,
            };
        }
        if matches!(
            health,
            HealthObservation::Healthy | HealthObservation::NotConfigured
        ) {
            HealthVerdict::Healthy
        } else {
            HealthVerdict::Failed {
                observation,
                last_check: None,
                oom_killed: self.oom_killed,
            }
        }
    }

    pub(super) fn crash_containers(mut self) -> Self {
        self.runtime = ContainerRuntimeObservation::Exited { code: 1 };
        self
    }

    /// Containers crash and Docker keeps restarting them.
    pub(super) fn restart_containers(mut self) -> Self {
        self.runtime = ContainerRuntimeObservation::Restarting;
        self
    }

    /// Containers crash, and Docker restarts them so each new run prints `booting`
    /// after the stored exit.
    pub(super) fn reboot_containers(mut self) -> Self {
        self.runtime = ContainerRuntimeObservation::Restarting;
        self.rebooted = true;
        self
    }

    /// Like [`Self::reboot_containers`], with a run that exited printing nothing
    /// between the crash and `booting`.
    pub(super) fn reboot_containers_through_a_silent_run(mut self) -> Self {
        self.silent_run = true;
        self.reboot_containers()
    }

    /// Containers exit 137 with Docker's inspect saying it OOM-killed them.
    pub(super) fn oom_kill_containers(mut self) -> Self {
        self.runtime = ContainerRuntimeObservation::Exited { code: 137 };
        self.oom_killed = true;
        self
    }

    pub(super) fn fail_pulls_on(mut self, machine: MachineId) -> Self {
        self.pull_failures.insert(machine);
        self
    }

    pub(super) fn image_pulls(&self) -> Arc<Mutex<Vec<ImagePullCall>>> {
        self.image_pulls.clone()
    }

    pub(super) fn creation_pulls(&self) -> Arc<AtomicUsize> {
        self.creation_pulls.clone()
    }

    /// Rejected pulls answer as Docker does for a tag the registry doesn't have.
    pub(super) fn missing_image(mut self) -> Self {
        self.pull_failure = RpcError {
            code: RpcErrorCode::NotFound,
            message: "image pull failed".into(),
            details: Value::Null,
            cause: vec!["Docker responded with status code 404: nginx:latest: not found".into()],
        };
        self
    }

    pub(super) fn with_machines(mut self, machines: Vec<MachineObservation>) -> Self {
        self.machines = machines;
        self
    }

    pub(super) fn fail_inspects(mut self, message: &str) -> Self {
        self.inspect_error = Some(RpcError {
            code: RpcErrorCode::Unavailable,
            message: message.into(),
            details: Value::Null,
            cause: Vec::new(),
        });
        self
    }

    pub(super) fn mutating_rpcs(&self) -> Arc<AtomicUsize> {
        self.mutating_rpcs.clone()
    }

    pub(super) fn listed_containers(&self) -> Arc<Mutex<Vec<ployz_core::ContainerObservation>>> {
        self.listed_containers.clone()
    }

    pub(super) fn with_rotation(self, container: ContainerId, rotation: CopyRotation) -> Self {
        let service = self.identity(&container);
        self.rotations
            .lock()
            .unwrap()
            .insert(container, Rotated { service, rotation });
        self
    }

    pub(super) fn rotations(&self) -> Arc<Mutex<BTreeMap<ContainerId, Rotated>>> {
        self.rotations.clone()
    }

    pub(super) fn unrouted(&self) -> Arc<Mutex<BTreeSet<ContainerId>>> {
        self.unrouted.clone()
    }

    pub(super) async fn unrouted_copy(&self, container_id: &ContainerId) {
        loop {
            let unrouted = self.unroutes.notified();
            tokio::pin!(unrouted);
            unrouted.as_mut().enable();
            if self.unrouted.lock().unwrap().contains(container_id) {
                return;
            }
            unrouted.await;
        }
    }

    pub(super) fn created_namespaces(&self) -> Arc<Mutex<Vec<Namespace>>> {
        self.created_namespaces.clone()
    }

    pub(super) fn removed_containers(&self) -> Arc<Mutex<BTreeSet<ContainerId>>> {
        Arc::clone(&self.removed)
    }

    pub(super) fn created_specs(&self) -> Arc<Mutex<Vec<ResolvedServiceSpec>>> {
        self.created_specs.clone()
    }

    fn record_mutation(&self) {
        self.mutating_rpcs.fetch_add(1, Ordering::SeqCst);
    }

    #[expect(
        clippy::result_large_err,
        reason = "tonic Status is the MachineRpc error type"
    )]
    fn located(&self, locator: &ContainerLocator) -> Result<ContainerId, Status> {
        match locator {
            ContainerLocator::Id(container_id) => Ok(*container_id),
            ContainerLocator::Key(slot) => self
                .slots
                .lock()
                .unwrap()
                .get(&slot.creation_key.as_str().to_owned())
                .copied()
                .ok_or_else(|| Status::not_found("no Container at that slot")),
        }
    }

    fn rotation(&self, container_id: &ContainerId) -> CopyRotation {
        if let Some(rotated) = self.rotations.lock().unwrap().get(container_id) {
            return rotated.rotation.clone();
        }
        self.listed_containers
            .lock()
            .unwrap()
            .iter()
            .find(|container| container.container_id == *container_id)
            .map(|container| container.rotation().clone())
            .unwrap_or_default()
    }

    fn rotate(&self, container_id: ContainerId, rotation: CopyRotation) {
        if self.rotation(&container_id) == CopyRotation::In && rotation != CopyRotation::In {
            self.unrouted.lock().unwrap().insert(container_id);
            self.unroutes.notify_waiters();
        }
        let identity = self.identity(&container_id);
        let mut rotations = self.rotations.lock().unwrap();
        let service = rotations
            .get(&container_id)
            .map_or(identity, |rotated| rotated.service.clone());
        rotations.insert(container_id, Rotated { service, rotation });
    }

    fn identity(&self, container_id: &ContainerId) -> QualifiedService {
        self.listed_containers
            .lock()
            .unwrap()
            .iter()
            .find(|container| container.container_id == *container_id)
            .map_or_else(
                || QualifiedService::new(Namespace::parse("app").unwrap(), spec("web").name),
                ployz_core::ContainerObservation::identity,
            )
    }

    /// How a proxy sees a copy: addressed, with the rotation its owner wrote.
    fn proxied(
        &self,
        container_id: ContainerId,
        service: &QualifiedService,
    ) -> ployz_core::ContainerObservation {
        let listed = self
            .listed_containers
            .lock()
            .unwrap()
            .iter()
            .find(|container| container.container_id == container_id)
            .cloned();
        let mut observed = listed.unwrap_or_else(|| {
            let machine_id = self.machines.first().unwrap().machine.id;
            self.observed(machine_id, container_id, resolved(spec("web")))
        });
        let rotation = self.rotation(&container_id);
        observed
            .try_update(|parts| {
                parts.namespace = service.namespace.clone();
                parts.resolved_spec.name = service.name.clone();
                parts.rotation = Some(rotation);
                parts.address = parts.address.or(Some(ployz_core::ContainerAddress(
                    "10.210.1.2".parse().unwrap(),
                )));
            })
            .unwrap();
        observed
    }

    /// Each proxy routes by the real proxy rule; a sticky one still lists every
    /// copy that left In.
    fn routes(&self, service: &QualifiedService) -> RoutesRead {
        let removed = self.removed.lock().unwrap().clone();
        let mut copies: BTreeMap<ContainerId, QualifiedService> = self
            .listed_containers
            .lock()
            .unwrap()
            .iter()
            .map(|container| (container.container_id, container.identity()))
            .collect();
        copies.extend(
            self.rotations
                .lock()
                .unwrap()
                .iter()
                .map(|(id, rotated)| (*id, rotated.service.clone())),
        );
        let ours = |id: &ContainerId| copies.get(id) == Some(service) && !removed.contains(id);
        let siblings = ployz_core::service_containers(
            copies
                .keys()
                .filter(|id| ours(id))
                .map(|id| self.proxied(*id, service)),
        );
        let unrouted: BTreeSet<ContainerId> = self
            .unrouted
            .lock()
            .unwrap()
            .iter()
            .filter(|id| ours(id))
            .copied()
            .collect();
        let mut served = self.served.lock().unwrap();
        RoutesRead {
            routes: self
                .machines
                .iter()
                .map(|observed| {
                    let id = observed.machine.id;
                    let current = served.entry(id).or_default();
                    let next = ployz_core::proxy_routes(&siblings, |_| false, current);
                    let route = next.get(service).cloned().unwrap_or_default();
                    let mut listed: BTreeSet<ContainerId> = route
                        .iter()
                        .map(|serving| serving.as_observation().container_id)
                        .collect();
                    if next.contains_key(service) {
                        current.insert(service.clone(), route);
                    } else {
                        current.remove(service);
                    }
                    if self.sticky_proxies.contains(&id) {
                        listed.extend(unrouted.iter().copied());
                    }
                    (id, Some(listed))
                })
                .collect(),
            complete: true,
            ..RoutesRead::default()
        }
    }

    async fn turn(
        &self,
        machine_id: MachineId,
        container_id: ContainerId,
        resolved_spec: ResolvedServiceSpec,
        want: &ployz_core::ContainerWant,
    ) -> Result<Option<HealthVerdict>, EnsuredContainer> {
        let observed = || {
            ContainerNow::Present(self.observed(machine_id, container_id, resolved_spec.clone()))
        };
        match rotation::decide(&self.rotation(&container_id), want) {
            Err(refusal) => Err(EnsuredContainer::Refused {
                now: observed(),
                refusal,
            }),
            Ok(RotationEdge::Write(CopyRotation::In)) if self.unread_health => {
                Err(EnsuredContainer::Ensured {
                    now: observed(),
                    health: None,
                })
            }
            Ok(RotationEdge::Write(CopyRotation::In)) => {
                let health = self.settle().await;
                if health == HealthVerdict::Healthy {
                    self.rotate(container_id, CopyRotation::In);
                    Ok(Some(health))
                } else {
                    Err(EnsuredContainer::Ensured {
                        now: observed(),
                        health: Some(health),
                    })
                }
            }
            Ok(RotationEdge::Write(rotation)) => {
                self.rotate(container_id, rotation);
                Ok(None)
            }
            Ok(RotationEdge::Keep | RotationEdge::Remove | RotationEdge::Refused(_)) => Ok(None),
        }
    }

    fn observed(
        &self,
        machine_id: MachineId,
        container_id: ContainerId,
        resolved_spec: ResolvedServiceSpec,
    ) -> ployz_core::ContainerObservation {
        ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
            rotation: Some(self.rotation(&container_id)),
            r#gen: None,
            container_id,
            display_name: format!("{}-1", resolved_spec.name),
            created_at_unix_nanos: 0,
            machine_id,
            namespace: Namespace::parse("app").unwrap(),
            kind: ContainerKind::ServiceContainer,
            runtime: self.current_runtime(),
            effective_healthcheck: self.effective_healthcheck.clone(),
            resolved_spec,
            address: None,
            labels: BTreeMap::new(),
            stopped: false,
            run: None,
        })
        .unwrap()
    }
}

fn resolved(spec: RequestedServiceSpec) -> ResolvedServiceSpec {
    spec.to_resolved(
        ServiceId::random(),
        ResolvedUpdateConfig {
            order: UpdateOrder::StartFirst,
            monitor_millis: None,
        },
    )
    .expect("volume graph is scoped")
}

#[tonic::async_trait]
impl FakeMachine for DeployService {
    async fn inspect_storage(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        encoded(RpcResponse::from(ployz_core::StorageCapacity {
            backing: ployz_core::StorageBacking::Unallocated {
                host_total_bytes: 100 * ployz_core::STORAGE_GIB,
                host_available_bytes: 90 * ployz_core::STORAGE_GIB,
            },
            unmanaged_used_bytes: 0,
            slot_bound_bytes: 0,
            volumes: BTreeMap::new(),
            copies: self.copies.lock().unwrap().clone(),
        }))
    }

    async fn prepare_volumes(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        encoded(RpcResponse::from(ployz_core::PreparedVolumes {
            names: Vec::new(),
        }))
    }

    async fn build(
        &self,
        request: Request<tonic::Streaming<OpaquePayload>>,
    ) -> Result<Response<FakeStream>, Status> {
        let frames = self
            .builds
            .clone()
            .ok_or_else(|| Status::unimplemented("Build is not used by this fixture"))?
            .stream(request)?;
        Ok(frames.map(|frames| Box::pin(frames) as FakeStream))
    }

    async fn describe_contract(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if let Some(described) = &self.describe_once
            && described.fetch_add(1, Ordering::SeqCst) > 0
        {
            std::future::pending::<()>().await;
        }
        if let Some(refusal) = self.describe_refusal.lock().unwrap().clone() {
            return Err(Status::unauthenticated(refusal));
        }
        let mut capabilities: BTreeSet<_> =
            [CapabilityName::parse(MACHINE_STORAGE_OBSERVATION_CAPABILITY).unwrap()].into();
        if self.builds.is_some() {
            capabilities.extend(
                [
                    ployz_core::BUILD_CAPABILITY,
                    ployz_core::ENSURE_IMAGE_INGEST_CAPABILITY,
                    ployz_core::PULL_IMAGE_FROM_MACHINE_CAPABILITY,
                    ployz_core::PRUNE_IMAGES_CAPABILITY,
                ]
                .map(|name| CapabilityName::parse(name).unwrap()),
            );
        }
        encoded(RpcResponse::from(ContractDescription {
            machine_id: machine_from_metadata(&request)
                .ok()
                .or_else(|| self.machines.first().map(|machine| machine.machine.id))
                .unwrap_or_else(MachineId::random),
            protocol_major: PROTOCOL_MAJOR,
            daemon_version: "test".into(),
            capabilities,
        }))
    }

    async fn list_machines(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if let Some(error) = self.machines_error.lock().unwrap().clone() {
            return encoded(RpcResponse::from(error));
        }
        encoded(RpcResponse::from(MachineList {
            enrollment: None,
            machines: self.machines.clone(),
        }))
    }

    async fn list_containers(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if let Some(blocked) = &self.listing_blocked {
            blocked.notify_one();
            std::future::pending::<()>().await;
        }
        let machine_id = machine_from_metadata(&request)?;
        if self.listing_failures.contains(&machine_id) {
            return encoded(RpcResponse::from(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "listing failed".into(),
                details: Value::Null,
                cause: Vec::new(),
            }));
        }
        let listed: Vec<_> = self
            .listed_containers
            .lock()
            .unwrap()
            .iter()
            .filter(|container| container.machine_id == machine_id)
            .cloned()
            .collect();
        encoded(RpcResponse::from(ContainerList {
            containers: listed
                .into_iter()
                .map(|mut container| {
                    let rotation = self.rotation(&container.container_id);
                    container
                        .try_update(|parts| parts.rotation = Some(rotation))
                        .unwrap();
                    container
                })
                .collect(),
        }))
    }

    async fn list_volumes(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        encoded(RpcResponse::from(VolumeInventory {
            volumes: self.volumes.lock().unwrap().clone(),
            failures: Vec::new(),
        }))
    }

    async fn inspect(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let machine_id = if request.metadata().contains_key("machine") {
            machine_from_metadata(&request)?
        } else {
            self.machines
                .first()
                .map(|machine| machine.machine.id)
                .ok_or_else(|| Status::unavailable("no entry Machine"))?
        };
        let request = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::Inspect(inspect) = request.body else {
            return Err(Status::invalid_argument("expected Inspect"));
        };
        let telemetry = inspect_telemetry_fixture::observation(inspect.telemetry);
        encoded(RpcResponse::from(MachineDetails {
            id: machine_id,
            phase: LocalMachinePhase::Participating,
            machine: None,
            public_key: WireGuardPublicKey([0; 32]),
            advertised_endpoints: Vec::new(),
            store_version: BTreeMap::new(),
            rtts: Vec::new(),
            management_clients: Vec::new(),
            telemetry,
            storage: self.machines.first().and_then(|machine| machine.storage),
            upgrade: None,
            token: None,
            sealing: None,
        }))
    }
    async fn enroll(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        unused()
    }
    async fn register(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        unused()
    }
    async fn set_management_client(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        unused()
    }
    async fn retained_env(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let Some(retained) = &self.retained_envs else {
            return unused();
        };
        let RpcRequestBody::RetainedEnv(ask) = request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected retained_env"));
        };
        encoded(RpcResponse::from(RetainedEnvReply {
            environment: retained.get(&ask.container_id).cloned(),
        }))
    }
    async fn inspect_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let target = machine_from_metadata(&request).ok();
        let RpcRequestBody::InspectContainer(inspect) =
            request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected inspect_container"));
        };
        if let Some(error) = &self.inspect_error {
            return encoded(RpcResponse::from(error.clone()));
        }
        if self.uninspectable.contains(&inspect.container_id) {
            return Err(Status::not_found("no such container"));
        }
        if self.stalled_inspects.contains(&inspect.container_id) {
            return Err(Status::deadline_exceeded("docker did not answer"));
        }
        let service = self.identity(&inspect.container_id);
        let spec = spec(service.name.as_str())
            .to_resolved(
                ServiceId::random(),
                ResolvedUpdateConfig {
                    order: UpdateOrder::StartFirst,
                    monitor_millis: None,
                },
            )
            .expect("volume graph is scoped");
        encoded(RpcResponse::from(ContainerDetails {
            image_id: None,
            last_check: None,
            run: None,
            environment: Some(BTreeMap::from([
                ("TOKEN".into(), "live-only-sentinel".into()),
                ("PLAIN".into(), "from-docker".into()),
            ])),
            container: ployz_core::ContainerObservation::try_from(
                ployz_core::ContainerObservationParts {
                    rotation: Some(self.rotation(&inspect.container_id)),
                    r#gen: None,
                    container_id: inspect.container_id,
                    display_name: "web-1".into(),
                    created_at_unix_nanos: 0,
                    machine_id: target
                        .or_else(|| self.machines.first().map(|machine| machine.machine.id))
                        .unwrap_or_else(MachineId::random),
                    namespace: service.namespace,
                    kind: ContainerKind::ServiceContainer,
                    runtime: self.current_runtime(),
                    effective_healthcheck: self.effective_healthcheck.clone(),
                    resolved_spec: spec,
                    address: None,
                    labels: BTreeMap::new(),
                    stopped: false,
                    run: None,
                },
            )
            .unwrap(),
        }))
    }
    async fn remove_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        let machine_id = machine_from_metadata(&request)?;
        let RpcRequestBody::RemoveVolume(remove) =
            request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected remove_volume"));
        };
        self.volumes
            .lock()
            .unwrap()
            .retain(|volume| volume.id.machine_id != machine_id || volume.id.name != remove.name);
        encoded(RpcResponse::from(ployz_core::VolumeRemoved {}))
    }
    async fn ensure_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        let machine_id = machine_from_metadata(&request)?;
        let RpcRequestBody::EnsureContainer(ensure) =
            request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected ensure_container"));
        };
        let want = ensure.want.clone();
        let settled = matches!(
            &want,
            ployz_core::ContainerWant::Running { container: RunTarget::New(new), .. }
                if new.answer == AnswerWhen::Settled
        );
        let turned = async |container_id: ContainerId| {
            self.turn(machine_id, container_id, resolved(spec("web")), &want)
                .await
        };
        let (now, turned_health) = match ensure.want {
            ployz_core::ContainerWant::Running {
                container: RunTarget::New(new),
                ..
            } => {
                if !new.image_prepared {
                    self.creation_pulls.fetch_add(1, Ordering::SeqCst);
                    if self.pull_failures.contains(&machine_id) {
                        return encoded(RpcResponse::from(self.pull_failure.clone()));
                    }
                }
                if let Some(error) = &self.create_volume_error {
                    return encoded(RpcResponse::from(error.clone()));
                }
                if let Some(error) = &self.create_volume_verification_error {
                    let mut error = error.clone();
                    error.message = format!(
                        "Docker Volume was created but could not be verified: {}",
                        error.message
                    );
                    return encoded(RpcResponse::from(error));
                }
                let key = new.slot.creation_key.as_str().to_owned();
                let existing = self.slots.lock().unwrap().get(&key).copied();
                let container_id = match existing {
                    Some(container_id) => container_id,
                    None => {
                        self.created_namespaces
                            .lock()
                            .unwrap()
                            .push(new.slot.namespace.clone());
                        self.created_specs
                            .lock()
                            .unwrap()
                            .push(new.resolved_spec.clone());
                        let n = self.containers.fetch_add(1, Ordering::SeqCst) + 1;
                        let container_id = ContainerId::parse(format!("{n:064x}")).unwrap();
                        self.slots.lock().unwrap().insert(key, container_id);
                        self.rotations.lock().unwrap().insert(
                            container_id,
                            Rotated {
                                service: QualifiedService::new(
                                    new.slot.namespace.clone(),
                                    new.resolved_spec.name.clone(),
                                ),
                                rotation: CopyRotation::Pending,
                            },
                        );
                        container_id
                    }
                };
                if let Some(error) = &self.start_error {
                    return encoded(RpcResponse::from(error.clone()));
                }
                (
                    ContainerNow::Present(self.observed(
                        machine_id,
                        container_id,
                        new.resolved_spec,
                    )),
                    None,
                )
            }
            ployz_core::ContainerWant::Running {
                container: RunTarget::Existing(locator),
                ..
            } => {
                if let Some(error) = &self.start_error {
                    return encoded(RpcResponse::from(error.clone()));
                }
                let container_id = self.located(&locator)?;
                match turned(container_id).await {
                    Ok(health) => (
                        ContainerNow::Present(self.observed(
                            machine_id,
                            container_id,
                            resolved(spec("web")),
                        )),
                        health,
                    ),
                    Err(answer) => return encoded(RpcResponse::from(answer)),
                }
            }
            ployz_core::ContainerWant::Stopped { container, .. }
            | ployz_core::ContainerWant::Out { container, .. }
            | ployz_core::ContainerWant::Held { container, .. } => {
                let container_id = self.located(&container)?;
                if let Err(answer) = turned(container_id).await {
                    return encoded(RpcResponse::from(answer));
                }
                (
                    ContainerNow::Present(self.observed(
                        machine_id,
                        container_id,
                        resolved(spec("web")),
                    )),
                    None,
                )
            }
            ployz_core::ContainerWant::Gone { container, .. } => {
                if let Ok(container_id) = self.located(&container) {
                    if let Err(answer) = turned(container_id).await {
                        return encoded(RpcResponse::from(answer));
                    }
                    self.removed.lock().unwrap().insert(container_id);
                }
                (ContainerNow::Absent { of: container }, None)
            }
        };
        let health = if settled {
            Some(self.settle().await)
        } else {
            turned_health
        };
        encoded(RpcResponse::from(EnsuredContainer::Ensured { now, health }))
    }
    async fn exec(
        &self,
        request: Request<Streaming<OpaquePayload>>,
    ) -> Result<Response<FakeStream>, Status> {
        self.record_mutation();
        let Some(code) = self.exec_exit else {
            return unused();
        };
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        for frame in [
            ExecResponseFrame::ExecId("test-exec".into()),
            ExecResponseFrame::Exit(code),
        ] {
            sender.send(Ok(frame.encode().unwrap())).await.unwrap();
        }
        tokio::spawn(async move {
            let mut requests = request.into_inner();
            while let Ok(Some(_)) = requests.message().await {}
            drop(sender);
        });
        Ok(Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(receiver),
        )))
    }
    async fn tail_logs(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<FakeStream>, Status> {
        let RpcRequestBody::TailLogs(tail) = request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected tail_logs"));
        };
        if let ployz_core::LiveLogTarget::Container(id) = tail.target
            && self.removed.lock().unwrap().contains(&id)
        {
            return Err(Status::not_found(format!("No such container: {id}")));
        }
        unused()
    }
    /// Every Container's Log Store holds `line 1`..`line 10`, then a line printing its
    /// env's PLAIN and one leaking its TOKEN, then its exit, paged newest first. A
    /// Service-wide read interleaves Containers `d..d` (odd lines) and `c..c` (even
    /// lines).
    async fn log_history(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<FakeStream>, Status> {
        let machine = machine_from_metadata(&request)?;
        let RpcRequestBody::LogHistory(history) =
            request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected log_history"));
        };
        assert_eq!(history.direction, ployz_core::LogDirection::Backward);
        self.history_requests.lock().unwrap().push(history.clone());
        if let Some(message) = self.history_failures.get(&machine) {
            let row = ployz_core::HistoryRow::Error(message.clone());
            return Ok(Response::new(Box::pin(tokio_stream::iter(vec![Ok(row
                .encode()
                .unwrap())]))));
        }
        let service_wide = history.container_id.is_none();
        let container_id = history
            .container_id
            .unwrap_or_else(|| ContainerId::parse("c".repeat(64)).unwrap());
        let other = ContainerId::parse("d".repeat(64)).unwrap();
        let mut stored = (1..=12)
            .map(|n| ployz_core::HistoryRow::Line {
                container_id: if service_wide && n % 2 == 1 {
                    other
                } else {
                    container_id
                },
                ts: n,
                stream: ployz_core::HistoryStream::Stdout,
                text: match n {
                    11 => b"plain from-docker".to_vec(),
                    12 => b"token=live-only-sentinel".to_vec(),
                    _ => format!("line {n}").into_bytes(),
                },
            })
            .collect::<Vec<_>>();
        stored.push(ployz_core::HistoryRow::Exit {
            container_id,
            ts: 13,
            exit_code: Some(1),
            oom_killed: false,
        });
        if self.silent_run {
            stored.push(ployz_core::HistoryRow::Exit {
                container_id,
                ts: 14,
                exit_code: Some(1),
                oom_killed: false,
            });
        }
        if self.rebooted {
            stored.push(ployz_core::HistoryRow::Line {
                container_id,
                ts: 15,
                stream: ployz_core::HistoryStream::Stdout,
                text: b"booting".to_vec(),
            });
        }
        stored.reverse();
        let total = stored.len();
        let from = history.cursor.map_or(0, |cursor| cursor.parse().unwrap());
        let to = total.min(from + usize::from(history.limit));
        let mut page = stored.drain(from..to).collect::<Vec<_>>();
        page.push(ployz_core::HistoryRow::End {
            next: (to < total).then(|| to.to_string()),
        });
        let mut payloads = Vec::new();
        for row in &page {
            payloads.push(Ok(row.encode().unwrap()));
        }
        Ok(Response::new(Box::pin(tokio_stream::iter(payloads))))
    }
    async fn list_images(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let machine = machine_from_metadata(&request)?;
        let images = self
            .builds
            .as_ref()
            .map(|builds| builds.images(machine))
            .unwrap_or(MachineImages {
                containerd_store: self.builds.is_some(),
                images: Vec::new(),
                docker_root: None,
            });
        encoded(RpcResponse::from(images))
    }
    async fn ensure_image_ingest(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if let Some(builds) = &self.builds {
            let machine = machine_from_metadata(&request)?;
            let machine = self
                .machines
                .iter()
                .find(|observed| observed.machine.id == machine)
                .unwrap();
            return encoded(RpcResponse::from(builds.open(&machine.machine)));
        }
        self.record_mutation();
        unused()
    }
    async fn read_routes(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::ReadRoutes(read) = request.into_inner().decode_request().unwrap().body
        else {
            panic!("expected a routes read");
        };
        encoded(RpcResponse::from(self.routes(&read.service)))
    }
    async fn read_routed_addresses(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn prune_images(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if self.builds.is_none() {
            return unused();
        }
        encoded(RpcResponse::from(ployz_core::ImagesRemoved::default()))
    }
    async fn pull_image(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let machine = machine_from_metadata(&request)?;
        let RpcRequestBody::PullImage(pull) = request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected pull_image"));
        };
        if self.pull_failures.contains(&machine) {
            return encoded(RpcResponse::from(self.pull_failure.clone()));
        }
        self.image_pulls.lock().unwrap().push(ImagePullCall {
            machine,
            image: pull.image,
            policy: pull.policy,
            mutations_before: self.mutating_rpcs.load(Ordering::SeqCst),
        });
        encoded(RpcResponse::from(ployz_core::ImagePulled::default()))
    }
    async fn pull_image_from_machine(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if let Some(builds) = &self.builds {
            let machine = machine_from_metadata(&request)?;
            let RpcRequestBody::PullImageFromMachine(pull) =
                request.into_inner().decode_request().unwrap().body
            else {
                panic!("expected peer pull");
            };
            return encoded(builds.pull(machine, pull));
        }
        self.record_mutation();
        unused()
    }
    async fn publish_certificate_material(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        unused()
    }
    async fn update_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        unused()
    }
    async fn leave(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        unused()
    }
    async fn forget(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        unused()
    }
}

#[expect(
    clippy::result_large_err,
    reason = "tonic Status is the MachineRpc error type"
)]
fn unused<T>() -> Result<T, Status> {
    Err(Status::unimplemented("unused"))
}

#[expect(
    clippy::result_large_err,
    reason = "tonic Status is the MachineRpc error type"
)]
fn encoded(response: RpcResponse) -> Result<Response<OpaquePayload>, Status> {
    Ok(Response::new(response.encode().unwrap()))
}

#[expect(
    clippy::result_large_err,
    reason = "tonic Status is the MachineRpc error type"
)]
fn machine_from_metadata(request: &Request<OpaquePayload>) -> Result<MachineId, Status> {
    let value = request
        .metadata()
        .get("machine")
        .ok_or_else(|| Status::invalid_argument("missing machine target"))?
        .to_str()
        .map_err(|_| Status::invalid_argument("machine target is not utf-8"))?;
    MachineId::parse(value).map_err(|error| Status::invalid_argument(error.to_string()))
}

pub(super) async fn connected(
    service: DeployService,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let (address, server) = listening(service).await;
    let client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp(address)],
        },
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();
    (client, server)
}

/// `connected` over in-process streams, so a paused clock never outruns OS I/O.
pub(super) async fn connected_in_memory(
    service: DeployService,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    type Incoming =
        tokio::sync::mpsc::UnboundedSender<Result<tokio::io::DuplexStream, std::io::Error>>;
    struct Memory(Incoming);
    #[tonic::async_trait]
    impl Connector for Memory {
        async fn connect(&self, _: &Connection) -> Result<Channel, ConnectError> {
            let incoming = self.0.clone();
            Channel::from_static("http://memory.invalid")
                .connect_with_connector(tower::service_fn(move |_| {
                    let (client, server) = tokio::io::duplex(64 * 1024);
                    incoming.send(Ok(server)).unwrap();
                    async move { Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(client)) }
                }))
                .await
                .map_err(Into::into)
        }

        async fn dial_proxy(
            &self,
            _: &Connection,
            _: &str,
            _: &str,
        ) -> Result<BoxProxyStream, ConnectError> {
            Err(ConnectError::Attempt("unused".into()))
        }
    }
    let (incoming, receiver) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(Fake(service)))
            .serve_with_incoming(tokio_stream::wrappers::UnboundedReceiverStream::new(
                receiver,
            )),
    );
    let client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp("127.0.0.1:1".parse().unwrap())],
        },
        Arc::new(Memory(incoming)),
    )
    .await
    .unwrap();
    (client, server)
}

pub(super) async fn listening(
    service: DeployService,
) -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = tcp.local_addr().unwrap();
    let server = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(Fake(service)))
            .serve_with_incoming(TcpListenerStream::new(tcp)),
    );
    (address, server)
}

pub(super) fn health_spec(name: &str) -> RequestedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "nginx",
            "pull_policy": "always",
            "healthcheck": { "state": "configured", "test": ["CMD", "true"] }
        },
        "update": { "monitor_millis": 60_000 }
    }))
    .unwrap()
}

pub(super) fn spec(name: &str) -> RequestedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "always" }
    }))
    .unwrap()
}

pub(super) fn running_container(
    machine: &MachineObservation,
    spec: &RequestedServiceSpec,
) -> ployz_core::ContainerObservation {
    running_container_in(machine, spec, "app", '1')
}

/// A running Container of `spec` that a Deploy brought In.
pub(super) fn deployed_container(
    machine: &MachineObservation,
    spec: &RequestedServiceSpec,
) -> ployz_core::ContainerObservation {
    let mut container = running_container(machine, spec);
    container
        .try_update(|parts| parts.rotation = Some(ployz_core::CopyRotation::In))
        .unwrap();
    container
}

/// A running Container of `spec` in `namespace`, its ID `id` repeated.
pub(super) fn running_container_in(
    machine: &MachineObservation,
    spec: &RequestedServiceSpec,
    namespace: &str,
    id: char,
) -> ployz_core::ContainerObservation {
    let namespace = Namespace::parse(namespace).unwrap();
    let mut spec = spec.clone();
    spec.set_volume_graph(
        spec.volume_graph()
            .clone()
            .scope_to_namespace(&namespace)
            .unwrap(),
    )
    .unwrap();
    let resolved = spec
        .to_resolved(
            ServiceId::random(),
            ResolvedUpdateConfig {
                order: UpdateOrder::StartFirst,
                monitor_millis: spec.update.monitor_millis,
            },
        )
        .expect("volume graph is scoped");
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        rotation: None,
        r#gen: None,
        container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        display_name: format!("{}-1", spec.name),
        created_at_unix_nanos: 0,
        machine_id: machine.machine.id,
        namespace,
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::NotConfigured,
        },
        effective_healthcheck: None,
        resolved_spec: resolved,
        address: None,
        labels: BTreeMap::new(),
        stopped: false,
        run: None,
    })
    .unwrap()
}

pub(super) fn add_named_volume(requested: &mut RequestedServiceSpec, name: &str) {
    let reference = ServiceVolumeReference::parse(name).unwrap();
    requested
        .set_volume_graph(
            ServiceVolumeGraph::parse(
                vec![ServiceVolume {
                    reference: reference.clone(),
                    source: ployz_core::RawVolumeSource::Ordinary {
                        name: DockerVolumeName::parse(name).unwrap(),
                        driver: ployz_core::VolumeDriver::parse("local", BTreeMap::new()).unwrap(),
                        labels: Default::default(),
                    }
                    .admit()
                    .expect("valid volume declaration"),
                }],
                vec![ServiceMount {
                    volume: reference,
                    target: ContainerPath::parse(format!("/{name}")).unwrap(),
                    read_only: false,
                    no_copy: false,
                    subpath: None,
                }],
            )
            .unwrap(),
        )
        .unwrap();
}

pub(super) fn skip_health() -> PlanOptions {
    PlanOptions {
        skip_health_monitor: true,
        ..PlanOptions::default()
    }
}

pub(super) fn machine(hex: char, name: &str) -> MachineObservation {
    MachineObservation::new(
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                .parse()
                .unwrap(),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime: Default::default(),
            build_concurrency: None,
        },
        MembershipObservation::Up,
    )
}
