//! Mock Machine RPC and fixtures for deploy_client tests.

use std::{
    collections::BTreeMap,
    net::Ipv6Addr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use ployz::{
    connect::{Client, SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
    deploy::PlanOptions,
};
use ployz_core::{
    AdvertisedEndpoint, CapabilityName, ContainerCreated, ContainerDetails, ContainerId,
    ContainerKind, ContainerList, ContainerPath, ContainerRuntimeObservation, ContractDescription,
    CreateVolumeReport, DockerVolume, DockerVolumeId, DockerVolumeName, Domain, HealthObservation,
    LocalMachinePhase, MACHINE_STORAGE_OBSERVATION_CAPABILITY, Machine, MachineDetails, MachineId,
    MachineImages, MachineList, MachineName, MachineObservation, MachineRpc, MachineRpcServer,
    ManagementAddress, MembershipObservation, OpaquePayload, PROTOCOL_MAJOR, ProjectName,
    RequestedServiceSpec, ResolvedServiceSpec, ResolvedUpdateConfig, RpcError, RpcErrorCode,
    RpcRequestBody, RpcResponse, ServiceId, ServiceMount, ServiceVolume, ServiceVolumeGraph,
    ServiceVolumeReference, UpdateOrder, VolumeInventory, VolumeSource, WireGuardPublicKey,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status, Streaming, transport::Server};

#[path = "../support/inspect_telemetry.rs"]
mod inspect_telemetry_fixture;

#[derive(Clone)]
pub(super) struct DeployService {
    machines: Vec<MachineObservation>,
    create_volume_error: Option<RpcError>,
    create_volume_verification_error: Option<RpcError>,
    containers: Arc<AtomicUsize>,
    created_projects: Arc<Mutex<Vec<ProjectName>>>,
    created_specs: Arc<Mutex<Vec<ResolvedServiceSpec>>>,
    listed_containers: Arc<Mutex<Vec<ployz_core::ContainerObservation>>>,
    mutating_rpcs: Arc<AtomicUsize>,
    domain: Option<String>,
    hold_health: bool,
    ingress_backend: Option<ployz_core::IngressProxyBackend>,
}

impl DeployService {
    pub(super) fn new(machine: MachineObservation) -> Self {
        Self {
            machines: vec![machine],
            create_volume_error: None,
            create_volume_verification_error: None,
            containers: Arc::new(AtomicUsize::new(0)),
            created_projects: Arc::new(Mutex::new(Vec::new())),
            created_specs: Arc::new(Mutex::new(Vec::new())),
            listed_containers: Arc::new(Mutex::new(Vec::new())),
            mutating_rpcs: Arc::new(AtomicUsize::new(0)),
            domain: None,
            hold_health: false,
            ingress_backend: Some(ployz_core::IngressProxyBackend::Caddy),
        }
    }

    pub(super) fn empty() -> Self {
        Self {
            machines: Vec::new(),
            create_volume_error: None,
            create_volume_verification_error: None,
            containers: Arc::new(AtomicUsize::new(0)),
            created_projects: Arc::new(Mutex::new(Vec::new())),
            created_specs: Arc::new(Mutex::new(Vec::new())),
            listed_containers: Arc::new(Mutex::new(Vec::new())),
            mutating_rpcs: Arc::new(AtomicUsize::new(0)),
            domain: None,
            hold_health: false,
            ingress_backend: Some(ployz_core::IngressProxyBackend::Caddy),
        }
    }

    pub(super) fn fail_create_volume(mut self, message: &str) -> Self {
        self.create_volume_error = Some(RpcError {
            code: RpcErrorCode::Unavailable,
            message: message.into(),
            details: Value::Null,
        });
        self
    }

    pub(super) fn fail_create_volume_verification(mut self, message: &str) -> Self {
        self.create_volume_verification_error = Some(RpcError {
            code: RpcErrorCode::Unavailable,
            message: message.into(),
            details: Value::Null,
        });
        self
    }

    pub(super) fn with_domain(mut self, name: &str) -> Self {
        self.domain = Some(name.into());
        self
    }

    pub(super) fn hold_health(mut self) -> Self {
        self.hold_health = true;
        self
    }

    pub(super) fn with_ingress_backend(
        mut self,
        backend: Option<ployz_core::IngressProxyBackend>,
    ) -> Self {
        self.ingress_backend = backend;
        self
    }

    pub(super) fn mutating_rpcs(&self) -> Arc<AtomicUsize> {
        self.mutating_rpcs.clone()
    }

    pub(super) fn listed_containers(&self) -> Arc<Mutex<Vec<ployz_core::ContainerObservation>>> {
        self.listed_containers.clone()
    }

    pub(super) fn created_projects(&self) -> Arc<Mutex<Vec<ProjectName>>> {
        self.created_projects.clone()
    }

    pub(super) fn created_specs(&self) -> Arc<Mutex<Vec<ResolvedServiceSpec>>> {
        self.created_specs.clone()
    }

    fn record_mutation(&self) {
        self.mutating_rpcs.fetch_add(1, Ordering::SeqCst);
    }
}

#[tonic::async_trait]
impl MachineRpc for DeployService {
    type ExecStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type ContainerLogsStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type MachineLogsStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type RuntimeWatchStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;

    async fn describe_contract(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        encoded(RpcResponse::from(ContractDescription {
            machine_id: self
                .machines
                .first()
                .map(|machine| machine.machine.id)
                .unwrap_or_else(MachineId::random),
            protocol_major: PROTOCOL_MAJOR,
            daemon_version: "test".into(),
            capabilities: [CapabilityName::parse(MACHINE_STORAGE_OBSERVATION_CAPABILITY).unwrap()]
                .into(),
        }))
    }

    async fn list_machines(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        encoded(RpcResponse::from(MachineList {
            machines: self.machines.clone(),
        }))
    }

    async fn list_containers(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        encoded(RpcResponse::from(ContainerList {
            containers: self.listed_containers.lock().unwrap().clone(),
        }))
    }

    async fn list_volumes(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        encoded(RpcResponse::from(VolumeInventory {
            volumes: Vec::new(),
            failures: Vec::new(),
        }))
    }

    async fn create_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        if let Some(error) = &self.create_volume_error {
            return encoded(RpcResponse::from(error.clone()));
        }
        let machine_id = machine_from_metadata(&request)?;
        let RpcRequestBody::CreateVolume(create) =
            request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected create_volume"));
        };
        let storage = if create.driver == "ployz" {
            let bound_bytes = create
                .options
                .get("size")
                .and_then(|size| size.strip_suffix('b'))
                .unwrap()
                .parse()
                .unwrap();
            ployz_core::DockerVolumeStorageObservation::Provisioned {
                mountpoint: ployz_core::MachinePath::parse(format!(
                    "/var/lib/ployz-volumes/{}",
                    create.name
                ))
                .unwrap(),
                bound_bytes,
                used_bytes: 0,
            }
        } else {
            ployz_core::DockerVolumeStorageObservation::Plain {
                driver: create.driver,
            }
        };
        let volume = DockerVolume {
            id: DockerVolumeId {
                machine_id,
                name: create.name,
            },
            options: create.options,
            labels: create.labels,
            storage,
        };
        let report = self.create_volume_verification_error.clone().map_or(
            CreateVolumeReport::Verified {
                volume: volume.clone(),
            },
            |error| CreateVolumeReport::Unverified {
                id: volume.id,
                error,
            },
        );
        encoded(RpcResponse::from(report))
    }

    async fn create_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        let RpcRequestBody::CreateContainer(create) =
            request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected create_container"));
        };
        self.created_projects
            .lock()
            .unwrap()
            .push(create.project_name);
        self.created_specs
            .lock()
            .unwrap()
            .push(create.resolved_spec.clone());
        let n = self.containers.fetch_add(1, Ordering::SeqCst) + 1;
        let container_id = ContainerId::parse(format!("{n:064x}")).unwrap();
        encoded(RpcResponse::from(ContainerCreated {
            container_id,
            display_name: format!("{}-{n}", create.resolved_spec.name),
        }))
    }

    async fn ensure_global_slot(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }

    async fn start_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        let RpcRequestBody::StartContainer(start) =
            request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected start_container"));
        };
        encoded(RpcResponse::from(ployz_core::ContainerChanged {
            container_id: start.container_id,
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
            cloud_paired: false,
            telemetry,
            storage: self.machines.first().and_then(|machine| machine.storage),
            ingress_proxy_backend: self.ingress_backend,
        }))
    }
    async fn machine_token(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn initialize(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn register(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn join(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn set_cloud_pairing(
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
        let health = if self.hold_health {
            HealthObservation::Starting
        } else {
            HealthObservation::Healthy
        };
        let spec = spec("web").to_resolved(
            ServiceId::random(),
            ResolvedUpdateConfig {
                order: UpdateOrder::StartFirst,
                monitor_millis: None,
            },
        );
        encoded(RpcResponse::from(ContainerDetails {
            container: ployz_core::ContainerObservation {
                container_id: inspect.container_id,
                display_name: "web-1".into(),
                created_at_unix_nanos: 0,
                machine_id: self
                    .machines
                    .first()
                    .map(|machine| machine.machine.id)
                    .unwrap_or_else(MachineId::random),
                project_name: ProjectName::parse("app").unwrap(),
                service_id: spec.service_id,
                service_name: spec.name.clone(),
                kind: ContainerKind::ServiceContainer,
                runtime: ContainerRuntimeObservation::Running { health },
                effective_healthcheck: None,
                resolved_spec: spec,
                address: None,
                labels: BTreeMap::new(),
            },
        }))
    }
    async fn inspect_volume(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn remove_volume(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        unused()
    }
    async fn stop_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        let RpcRequestBody::StopContainer(stop) =
            request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected stop_container"));
        };
        encoded(RpcResponse::from(ployz_core::ContainerChanged {
            container_id: stop.container_id,
        }))
    }
    async fn remove_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        let RpcRequestBody::RemoveContainer(remove) =
            request.into_inner().decode_request().unwrap().body
        else {
            return Err(Status::invalid_argument("expected remove_container"));
        };
        encoded(RpcResponse::from(ployz_core::ContainerChanged {
            container_id: remove.container_id,
        }))
    }
    async fn exec(
        &self,
        _request: Request<Streaming<OpaquePayload>>,
    ) -> Result<Response<Self::ExecStream>, Status> {
        unused()
    }
    async fn container_logs(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::ContainerLogsStream>, Status> {
        unused()
    }
    async fn machine_logs(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::MachineLogsStream>, Status> {
        unused()
    }
    async fn runtime_watch(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::RuntimeWatchStream>, Status> {
        unused()
    }
    async fn list_images(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        encoded(RpcResponse::from(MachineImages {
            containerd_store: false,
            images: Vec::new(),
        }))
    }
    async fn ensure_image_ingest(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn pull_image_from_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn get_ingress_proxy_config(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn reserve_domain(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn get_domain(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        match &self.domain {
            Some(name) => encoded(RpcResponse::from(Domain { name: name.clone() })),
            None => encoded(RpcResponse::from(RpcError {
                code: RpcErrorCode::NotFound,
                message: "domain is not reserved".into(),
                details: Value::Null,
            })),
        }
    }
    async fn release_domain(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn create_domain_records(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn reset(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn update_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn remove_local_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn remove_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn inspect_wireguard(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
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
    let resolved = spec.to_resolved(
        ServiceId::random(),
        ResolvedUpdateConfig {
            order: UpdateOrder::StartFirst,
            monitor_millis: spec.update.monitor_millis,
        },
    );
    ployz_core::ContainerObservation {
        container_id: ContainerId::parse("1".repeat(64)).unwrap(),
        display_name: format!("{}-1", spec.name),
        created_at_unix_nanos: 0,
        machine_id: machine.machine.id,
        project_name: ProjectName::parse("app").unwrap(),
        service_id: resolved.service_id,
        service_name: spec.name.clone(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::NotConfigured,
        },
        effective_healthcheck: None,
        resolved_spec: resolved,
        address: None,
        labels: BTreeMap::new(),
    }
}

pub(super) fn add_named_volume(requested: &mut RequestedServiceSpec, name: &str) {
    let reference = ServiceVolumeReference::parse(name).unwrap();
    requested.volume_graph = ServiceVolumeGraph::parse(
        vec![ServiceVolume {
            reference: reference.clone(),
            source: VolumeSource::Named {
                name: DockerVolumeName::parse(name).unwrap(),
                external: false,
                driver: None,
                labels: Default::default(),
                no_copy: false,
                subpath: None,
            },
        }],
        vec![ServiceMount {
            volume: reference,
            target: ContainerPath::parse(format!("/{name}")).unwrap(),
            read_only: false,
        }],
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
            id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                .parse()
                .unwrap(),
            management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime: Default::default(),
        },
        MembershipObservation::Up,
    )
}
