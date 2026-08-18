//! Client.deploy seam: unary Deploy Intent → Deploy Outcome.

use std::{
    net::Ipv6Addr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use ployz::{
    connect::{Client, SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
    deploy::{
        DeployError, DeployIntent, DeployOperation, DeployOutcome, ExecutionError, FailedOperation,
        PlanError, PlanOptions,
    },
};
use ployz_core::{
    AdvertisedEndpoint, ContainerCreated, ContainerId, ContainerList, ContainerPath,
    ContractDescription, DockerVolume, DockerVolumeId, DockerVolumeName, Machine, MachineId,
    MachineList, MachineName, MachineObservation, MachineRpc, MachineRpcServer, ManagementAddress,
    MembershipObservation, OpaquePayload, PROTOCOL_MAJOR, RequestedServiceSpec, RpcError,
    RpcErrorCode, RpcRequestBody, RpcResponse, ServiceMount, ServiceVolume, ServiceVolumeGraph,
    ServiceVolumeReference, VolumeList, VolumeSource, WireGuardPublicKey,
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
        DeployError::Plan(PlanError::NoEligibleMachines)
    ));
    server.abort();
}

#[derive(Clone)]
struct DeployService {
    machines: Vec<MachineObservation>,
    create_volume_error: Option<RpcError>,
    containers: Arc<AtomicUsize>,
}

impl DeployService {
    fn new(machine: MachineObservation) -> Self {
        Self {
            machines: vec![machine],
            create_volume_error: None,
            containers: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn empty() -> Self {
        Self {
            machines: Vec::new(),
            create_volume_error: None,
            containers: Arc::new(AtomicUsize::new(0)),
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
            containers: Vec::new(),
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
        unused()
    }
    async fn stop_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn remove_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
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
        unused()
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
