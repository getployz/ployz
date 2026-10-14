//! Mock Machine RPC and fixtures for deploy_client tests.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use ployz::{
    connect::{Client, SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
    deploy::PlanOptions,
};
use ployz_core::{
    AdvertisedEndpoint, AnswerWhen, CapabilityName, ContainerDetails, ContainerId, ContainerKind,
    ContainerList, ContainerLocator, ContainerNow, ContainerPath, ContainerRuntimeObservation,
    ContractDescription, DockerVolume, DockerVolumeName, EnsuredContainer, ExecResponseFrame,
    HealthObservation, HealthVerdict, HealthcheckSpec, LocalMachinePhase,
    MACHINE_STORAGE_OBSERVATION_CAPABILITY, Machine, MachineDetails, MachineId, MachineImages,
    MachineList, MachineName, MachineObservation, MachineRpc, MachineRpcServer,
    MembershipObservation, Namespace, OpaquePayload, PROTOCOL_MAJOR, RequestedServiceSpec,
    ResolvedServiceSpec, ResolvedUpdateConfig, RpcError, RpcErrorCode, RpcRequestBody, RpcResponse,
    RunTarget, ServiceId, ServiceMount, ServiceVolume, ServiceVolumeGraph, ServiceVolumeReference,
    UpdateOrder, VolumeInventory, WireGuardPublicKey,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status, Streaming, transport::Server};

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
    /// When a Starting Container turns healthy and starts serving.
    healthy_at: Option<tokio::time::Instant>,
    effective_healthcheck: Option<HealthcheckSpec>,
    removed: Arc<Mutex<BTreeSet<ContainerId>>>,
    pub(super) history_requests: Arc<Mutex<Vec<ployz_core::LogHistoryRequest>>>,
    /// Machines whose Log Store answers with an error row, and its message.
    pub(super) history_failures: BTreeMap<MachineId, String>,
    /// Containers the Machine can no longer inspect, as when one has been removed.
    pub(super) uninspectable: BTreeSet<ContainerId>,
    start_error: Option<RpcError>,
    listing_failures: Vec<MachineId>,
    pub(super) listing_blocked: Option<Arc<tokio::sync::Notify>>,
    /// Docker Volumes created and not yet removed.
    pub(super) volumes: Arc<Mutex<Vec<DockerVolume>>>,
    /// Volume copies every Machine reports from storage.
    pub(super) copies: Arc<Mutex<BTreeMap<DockerVolumeName, ployz_core::ProvisionedCopy>>>,
}

impl DeployService {
    pub(super) fn new(machine: MachineObservation) -> Self {
        Self {
            builds: None,
            machines: vec![machine],
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
            healthy_at: None,
            effective_healthcheck: None,
            removed: Arc::new(Mutex::new(BTreeSet::new())),
            history_requests: Arc::new(Mutex::new(Vec::new())),
            history_failures: BTreeMap::new(),
            uninspectable: BTreeSet::new(),
            start_error: None,
            listing_failures: Vec::new(),
            listing_blocked: None,
            volumes: Arc::new(Mutex::new(Vec::new())),
            copies: Arc::new(Mutex::new(BTreeMap::new())),
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
            healthy_at: None,
            effective_healthcheck: None,
            removed: Arc::new(Mutex::new(BTreeSet::new())),
            history_requests: Arc::new(Mutex::new(Vec::new())),
            history_failures: BTreeMap::new(),
            uninspectable: BTreeSet::new(),
            start_error: None,
            listing_failures: Vec::new(),
            listing_blocked: None,
            volumes: Arc::new(Mutex::new(Vec::new())),
            copies: Arc::new(Mutex::new(BTreeMap::new())),
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

    pub(super) fn hold_health(mut self) -> Self {
        self.runtime = ContainerRuntimeObservation::Running {
            health: HealthObservation::Starting,
        };
        self
    }

    /// Containers report Starting until `after` passes.
    pub(super) fn turn_healthy_after(mut self, after: Duration) -> Self {
        self.runtime = ContainerRuntimeObservation::Running {
            health: HealthObservation::Starting,
        };
        self.healthy_at = Some(tokio::time::Instant::now() + after);
        self
    }

    /// Containers report `healthcheck` as the one Docker runs for them.
    pub(super) fn with_effective_healthcheck(mut self, healthcheck: HealthcheckSpec) -> Self {
        self.effective_healthcheck = Some(healthcheck);
        self
    }

    fn current_runtime(&self) -> ContainerRuntimeObservation {
        if self
            .healthy_at
            .is_some_and(|at| tokio::time::Instant::now() >= at)
        {
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            }
        } else {
            self.runtime.clone()
        }
    }

    /// The verdict a Machine answers a Settled creation with; a held health wait
    /// never answers.
    async fn settle(&self) -> HealthVerdict {
        if let Some(at) = self.healthy_at {
            tokio::time::sleep_until(at).await;
        }
        let observation = self.current_runtime();
        let ContainerRuntimeObservation::Running { health } = &observation else {
            return HealthVerdict::Failed {
                observation,
                last_check: None,
            };
        };
        if *health == HealthObservation::Starting {
            std::future::pending::<()>().await;
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
            }
        }
    }

    pub(super) fn crash_containers(mut self) -> Self {
        self.runtime = ContainerRuntimeObservation::Exited { code: 1 };
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

    fn observed(
        &self,
        machine_id: MachineId,
        container_id: ContainerId,
        resolved_spec: ResolvedServiceSpec,
    ) -> ployz_core::ContainerObservation {
        ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
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
impl MachineRpc for DeployService {
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

    type ExecStream = tokio_stream::wrappers::ReceiverStream<Result<OpaquePayload, Status>>;
    type BuildStream = tokio_stream::wrappers::ReceiverStream<Result<OpaquePayload, Status>>;

    async fn build(
        &self,
        request: Request<tonic::Streaming<OpaquePayload>>,
    ) -> Result<Response<Self::BuildStream>, Status> {
        self.builds
            .clone()
            .ok_or_else(|| Status::unimplemented("Build is not used by this fixture"))?
            .stream(request)
    }
    type LogHistoryStream = tokio_stream::Iter<std::vec::IntoIter<Result<OpaquePayload, Status>>>;
    type TailLogsStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type RuntimeWatchStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;

    async fn describe_contract(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
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
        encoded(RpcResponse::from(ContainerList {
            containers: self
                .listed_containers
                .lock()
                .unwrap()
                .iter()
                .filter(|container| container.machine_id == machine_id)
                .cloned()
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
    async fn request_machine_upgrade(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn inspect_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
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
        let spec = spec("web")
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
                    container_id: inspect.container_id,
                    display_name: "web-1".into(),
                    created_at_unix_nanos: 0,
                    machine_id: self
                        .machines
                        .first()
                        .map(|machine| machine.machine.id)
                        .unwrap_or_else(MachineId::random),
                    namespace: Namespace::parse("app").unwrap(),
                    kind: ContainerKind::ServiceContainer,
                    runtime: self.current_runtime(),
                    effective_healthcheck: self.effective_healthcheck.clone(),
                    resolved_spec: spec,
                    address: None,
                    labels: BTreeMap::new(),
                },
            )
            .unwrap(),
        }))
    }
    async fn inspect_volume_copy(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn set_volume_copy(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn seed_volume(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
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
        let settled = matches!(
            &ensure.want,
            ployz_core::ContainerWant::Running { container: RunTarget::New(new) }
                if new.answer == AnswerWhen::Settled
        );
        let now = match ensure.want {
            ployz_core::ContainerWant::Running {
                container: RunTarget::New(new),
            } => {
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
                        container_id
                    }
                };
                if let Some(error) = &self.start_error {
                    return encoded(RpcResponse::from(error.clone()));
                }
                ContainerNow::Present(self.observed(machine_id, container_id, new.resolved_spec))
            }
            ployz_core::ContainerWant::Running {
                container: RunTarget::Existing(locator),
            } => {
                if let Some(error) = &self.start_error {
                    return encoded(RpcResponse::from(error.clone()));
                }
                ContainerNow::Present(self.observed(
                    machine_id,
                    self.located(&locator)?,
                    resolved(spec("web")),
                ))
            }
            ployz_core::ContainerWant::Stopped { container, .. } => ContainerNow::Present(
                self.observed(machine_id, self.located(&container)?, resolved(spec("web"))),
            ),
            ployz_core::ContainerWant::Gone { container, .. } => {
                if let Ok(container_id) = self.located(&container) {
                    self.removed.lock().unwrap().insert(container_id);
                }
                ContainerNow::Absent { of: container }
            }
        };
        let health = if settled {
            Some(self.settle().await)
        } else {
            None
        };
        encoded(RpcResponse::from(EnsuredContainer::Ensured { now, health }))
    }
    async fn exec(
        &self,
        request: Request<Streaming<OpaquePayload>>,
    ) -> Result<Response<Self::ExecStream>, Status> {
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
        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            receiver,
        )))
    }
    async fn tail_logs(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<Self::TailLogsStream>, Status> {
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
    /// Every Container's Log Store holds `line 1`..`line 12` and then its exit,
    /// paged newest first. A Service-wide read interleaves Containers `d..d` (odd
    /// lines) and `c..c` (even lines); line 11 prints its env's PLAIN and line 12
    /// leaks its TOKEN.
    async fn log_history(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<Self::LogHistoryStream>, Status> {
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
            return Ok(Response::new(tokio_stream::iter(vec![Ok(row
                .encode()
                .unwrap())])));
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
                text: match (service_wide, n) {
                    (true, 11) => b"plain from-docker".to_vec(),
                    (true, 12) => b"token live-only-sentinel".to_vec(),
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
        Ok(Response::new(tokio_stream::iter(payloads)))
    }
    async fn runtime_watch(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::RuntimeWatchStream>, Status> {
        unused()
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
    async fn mint_build_grant(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }
    async fn inspect_build_grant(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
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
    async fn metric_history(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
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
            .add_service(MachineRpcServer::new(service))
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
