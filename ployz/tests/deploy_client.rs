//! Client.preview / Client.deploy seam: plan-only preview, then execute.

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
    deploy::{
        DeployError, DeployIntent, DeployOperation, DeployOutcome, DeployWarning, ExecutionError,
        FailedOperation, PlanError, PlanOptions,
    },
};
use ployz_core::{
    AdvertisedEndpoint, ContainerCreated, ContainerId, ContainerKind, ContainerList, ContainerPath,
    ContainerRuntimeObservation, ContractDescription, DockerVolume, DockerVolumeId,
    DockerVolumeName, Domain, HealthObservation, Machine, MachineId, MachineList, MachineName,
    MachineObservation, MachineRpc, MachineRpcServer, ManagementAddress, MembershipObservation,
    OpaquePayload, PROTOCOL_MAJOR, RequestedServiceSpec, ResolvedUpdateConfig, RpcError,
    RpcErrorCode, RpcRequestBody, RpcResponse, ServiceId, ServiceMount, ServiceVolume,
    ServiceVolumeGraph, ServiceVolumeReference, UpdateOrder, VolumeList, VolumeSource,
    WireGuardPublicKey,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status, Streaming, transport::Server};

#[tokio::test]
async fn deploy_returns_success_for_a_completed_run() {
    let machine = machine('a', "one");
    let (mut client, server) = connected(DeployService::new(machine.clone())).await;
    let spec = spec("web");

    let outcome = client
        .deploy(DeployIntent::apply_one(spec, skip_health()))
        .await
        .unwrap();

    let DeployOutcome::Success { completed } = outcome else {
        panic!("expected success: {outcome:?}");
    };
    assert_eq!(completed.len(), 1);
    assert!(matches!(
        completed.first(),
        Some(DeployOperation::RunContainer {
            machine_id,
            spec,
            skip_health_monitor: true,
        }) if *machine_id == machine.machine.id && spec.name.as_str() == "web"
    ));
    server.abort();
}

#[tokio::test]
async fn deploy_returns_the_completed_prefix_failed_op_and_unexecuted_suffix() {
    let machine = machine('a', "one");
    let (mut client, server) =
        connected(DeployService::new(machine.clone()).fail_create_volume("volume create failed"))
            .await;
    let mut spec = spec("web");
    add_named_volume(&mut spec, "data");

    let outcome = client
        .deploy(DeployIntent::apply_one(spec, skip_health()))
        .await
        .unwrap();

    let DeployOutcome::Failed {
        completed,
        failed,
        unexecuted,
    } = outcome
    else {
        panic!("expected partial failure: {outcome:?}");
    };
    assert!(completed.is_empty());
    assert!(matches!(
        failed,
        FailedOperation::Operation {
            operation: DeployOperation::CreateVolume { volume, .. },
            error: ExecutionError::Machine { .. },
        } if volume.reference.as_str() == "data"
    ));
    assert_eq!(unexecuted.len(), 1);
    assert!(matches!(
        unexecuted.first(),
        Some(DeployOperation::RunContainer { spec, .. }) if spec.name.as_str() == "web"
    ));
    server.abort();
}

#[tokio::test]
async fn deploy_surfaces_a_planning_error_instead_of_an_outcome() {
    let (mut client, server) = connected(DeployService::empty()).await;

    let error = client
        .deploy(DeployIntent::apply_one(spec("web"), skip_health()))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        DeployError::Plan(PlanError::NoEligibleMachines { .. })
    ));
    assert!(
        error
            .to_string()
            .contains("no Machines in the Deploy Snapshot"),
        "{error}"
    );
    server.abort();
}

#[tokio::test]
async fn preview_returns_operations_and_mutates_nothing() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone());
    let mutating = service.mutating_rpcs();
    let (mut client, server) = connected(service).await;
    let spec = spec("web");

    let preview = client
        .preview(DeployIntent::apply_one(spec, skip_health()))
        .await
        .unwrap();

    assert_eq!(mutating.load(Ordering::SeqCst), 0);
    assert_eq!(preview.operations.len(), 1);
    assert!(matches!(
        preview.operations.first(),
        Some(DeployOperation::RunContainer {
            machine_id,
            spec,
            skip_health_monitor: true,
        }) if *machine_id == machine.machine.id && spec.name.as_str() == "web"
    ));
    server.abort();
}

#[tokio::test]
async fn executing_a_previewed_intent_re_plans_against_a_fresh_snapshot() {
    let machine = machine('a', "one");
    let spec = spec_missing_pull("web");
    let service = DeployService::new(machine.clone());
    let mutating = service.mutating_rpcs();
    let listed = service.listed_containers();
    let (mut client, server) = connected(service).await;
    let intent = DeployIntent::apply_one(spec.clone(), skip_health());

    let preview = client.preview(intent.clone()).await.unwrap();
    assert_eq!(mutating.load(Ordering::SeqCst), 0);
    assert!(matches!(
        preview.operations.first(),
        Some(DeployOperation::RunContainer { spec, .. }) if spec.name.as_str() == "web"
    ));

    listed
        .lock()
        .unwrap()
        .push(running_container(&machine, &spec));

    let outcome = client.deploy(intent).await.unwrap();
    assert_eq!(mutating.load(Ordering::SeqCst), 0);
    let DeployOutcome::Success { completed } = outcome else {
        panic!("expected success: {outcome:?}");
    };
    assert!(
        completed.is_empty(),
        "re-plan must not replay the previewed RunContainer: {completed:?}"
    );
    server.abort();
}

#[tokio::test]
async fn preview_expands_ingress_and_includes_dns_warnings() {
    let mut machine = machine('a', "one");
    machine.machine.public_ip = Some("192.0.2.1".parse().unwrap());
    let service = DeployService::new(machine).with_domain("opaque.ployz.example");
    let mutating = service.mutating_rpcs();
    let (mut client, server) = connected(service).await;
    let spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "web",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "always" },
        "ports": [
            {
                "mode": "ingress",
                "hostname": { "kind": "assign_from_cluster_domain" },
                "load_balancer_port": 443,
                "container_port": 8080,
                "http_protocol": "https"
            },
            {
                "mode": "ingress",
                "hostname": { "kind": "explicit", "hostname": "preview-deploy.invalid" },
                "load_balancer_port": 80,
                "container_port": 8080,
                "http_protocol": "http"
            }
        ]
    }))
    .unwrap();

    let preview = client
        .preview(DeployIntent::apply_one(spec, skip_health()))
        .await
        .unwrap();

    assert_eq!(mutating.load(Ordering::SeqCst), 0);
    let Some(DeployOperation::RunContainer { spec, .. }) = preview.operations.first() else {
        panic!("expected RunContainer: {preview:?}");
    };
    let hostnames: Vec<_> = spec
        .ports
        .iter()
        .filter_map(|port| match port {
            ployz_core::PortPublication::Ingress {
                hostname: ployz_core::IngressHostname::Explicit { hostname },
                ..
            } => Some(hostname.as_str()),
            ployz_core::PortPublication::Ingress {
                hostname: ployz_core::IngressHostname::AssignFromClusterDomain,
                ..
            }
            | ployz_core::PortPublication::Host { .. } => None,
        })
        .collect();
    assert!(
        hostnames.contains(&"web.opaque.ployz.example"),
        "ingress expansion must assign the hosted hostname: {hostnames:?}"
    );
    assert!(
        hostnames.contains(&"preview-deploy.invalid"),
        "explicit ingress hostname must remain: {hostnames:?}"
    );
    assert!(
        preview.warnings.iter().any(|warning| match warning {
            DeployWarning::IngressHostname(message) => {
                message.contains("preview-deploy.invalid")
                    && message.contains("192.0.2.1")
                    && !message.to_ascii_lowercase().contains("certificate")
            }
            DeployWarning::ObservationFailed { .. } | DeployWarning::ObservationOmitted { .. } => {
                false
            }
        }),
        "DNS warning must match the CLI body: {:?}",
        preview.warnings
    );
    server.abort();
}

#[tokio::test]
async fn preview_surfaces_a_planning_error_instead_of_a_preview() {
    let (mut client, server) = connected(DeployService::empty()).await;

    let error = client
        .preview(DeployIntent::apply_one(spec("web"), skip_health()))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        DeployError::Plan(PlanError::NoEligibleMachines { .. })
    ));
    server.abort();
}

#[derive(Clone)]
struct DeployService {
    machines: Vec<MachineObservation>,
    create_volume_error: Option<RpcError>,
    containers: Arc<AtomicUsize>,
    listed_containers: Arc<Mutex<Vec<ployz_core::ContainerObservation>>>,
    mutating_rpcs: Arc<AtomicUsize>,
    domain: Option<String>,
}

impl DeployService {
    fn new(machine: MachineObservation) -> Self {
        Self {
            machines: vec![machine],
            create_volume_error: None,
            containers: Arc::new(AtomicUsize::new(0)),
            listed_containers: Arc::new(Mutex::new(Vec::new())),
            mutating_rpcs: Arc::new(AtomicUsize::new(0)),
            domain: None,
        }
    }

    fn empty() -> Self {
        Self {
            machines: Vec::new(),
            create_volume_error: None,
            containers: Arc::new(AtomicUsize::new(0)),
            listed_containers: Arc::new(Mutex::new(Vec::new())),
            mutating_rpcs: Arc::new(AtomicUsize::new(0)),
            domain: None,
        }
    }

    fn fail_create_volume(mut self, message: &str) -> Self {
        self.create_volume_error = Some(RpcError {
            code: RpcErrorCode::Unavailable,
            message: message.into(),
            details: Value::Null,
        });
        self
    }

    fn with_domain(mut self, name: &str) -> Self {
        self.domain = Some(name.into());
        self
    }

    fn mutating_rpcs(&self) -> Arc<AtomicUsize> {
        self.mutating_rpcs.clone()
    }

    fn listed_containers(&self) -> Arc<Mutex<Vec<ployz_core::ContainerObservation>>> {
        self.listed_containers.clone()
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
            capabilities: Default::default(),
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
        encoded(RpcResponse::from(VolumeList {
            volumes: Vec::new(),
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
        encoded(RpcResponse::from(DockerVolume {
            id: DockerVolumeId {
                machine_id,
                name: create.name,
            },
            driver: create.driver,
            options: create.options,
            labels: create.labels,
        }))
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
        let n = self.containers.fetch_add(1, Ordering::SeqCst) + 1;
        let container_id = ContainerId::parse(format!("{n:064x}")).unwrap();
        encoded(RpcResponse::from(ContainerCreated {
            container_id,
            display_name: format!("{}-{n}", create.resolved_spec.name),
        }))
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
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
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
    async fn inspect_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
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
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        unused()
    }
    async fn remove_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.record_mutation();
        unused()
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
        unused()
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
    async fn get_caddy_config(
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

async fn connected(
    service: DeployService,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = tcp.local_addr().unwrap();
    let server = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(tcp)),
    );
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

fn spec(name: &str) -> RequestedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "always" }
    }))
    .unwrap()
}

fn spec_missing_pull(name: &str) -> RequestedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "missing" }
    }))
    .unwrap()
}

fn running_container(
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

fn add_named_volume(requested: &mut RequestedServiceSpec, name: &str) {
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

fn skip_health() -> PlanOptions {
    PlanOptions {
        skip_health_monitor: true,
        ..PlanOptions::default()
    }
}

fn machine(hex: char, name: &str) -> MachineObservation {
    MachineObservation {
        machine: Machine {
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
        membership: MembershipObservation::Up,
        selected_endpoint: None,
        rtt: None,
    }
}
