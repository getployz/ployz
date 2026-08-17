use std::{
    collections::VecDeque,
    net::{Ipv6Addr, SocketAddr},
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
};
use ployz_core::{
    AdvertisedEndpoint, ContractDescription, DockerVolume, DockerVolumeId, DockerVolumeName,
    Machine, MachineId, MachineList, MachineName, MachineObservation, MachineRpc, MachineRpcServer,
    ManagementAddress, MembershipObservation, OpaquePayload, PROTOCOL_MAJOR, RpcError,
    RpcErrorCode, RpcRequestBody, RpcResponse, VolumeList, WireGuardPublicKey,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{
    Request, Response, Status, Streaming,
    transport::{Channel, Server},
};

pub(super) async fn serve_discovery(
    service: DiscoveryService,
) -> (
    SocketAddr,
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

pub(super) enum DescribeOutcome {
    Status(Status),
    Remote(RpcError),
}

#[derive(Clone)]
pub(super) struct DiscoveryService {
    description: ContractDescription,
    pub(super) describe_outcomes: Arc<Mutex<VecDeque<DescribeOutcome>>>,
    pub(super) stream_opens: Arc<AtomicUsize>,
}

impl DiscoveryService {
    pub(super) fn new(description: ContractDescription) -> Self {
        Self {
            description,
            describe_outcomes: Arc::new(Mutex::new(VecDeque::new())),
            stream_opens: Arc::new(AtomicUsize::new(0)),
        }
    }
}

pub(super) struct CountingConnector {
    inner: SystemConnector,
    connects: Arc<AtomicUsize>,
    redirects: Mutex<VecDeque<SocketAddr>>,
}

impl CountingConnector {
    pub(super) fn new(connects: Arc<AtomicUsize>) -> Self {
        Self::redirecting(connects, std::iter::empty())
    }

    pub(super) fn redirecting(
        connects: Arc<AtomicUsize>,
        redirects: impl IntoIterator<Item = SocketAddr>,
    ) -> Self {
        Self {
            inner: SystemConnector::default(),
            connects,
            redirects: Mutex::new(redirects.into_iter().collect()),
        }
    }
}

#[tonic::async_trait]
impl Connector for CountingConnector {
    async fn connect(&self, connection: &Connection) -> Result<Channel, ConnectError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let redirected = self.redirects.lock().unwrap().pop_front();
        match redirected {
            Some(address) => self.inner.connect(&Connection::tcp(address)).await,
            None => self.inner.connect(connection).await,
        }
    }

    async fn dial_proxy(
        &self,
        _connection: &Connection,
        _network: &str,
        _address: &str,
    ) -> Result<BoxProxyStream, ConnectError> {
        Err(ConnectError::Attempt("unused".into()))
    }
}

#[tonic::async_trait]
impl MachineRpc for DiscoveryService {
    type ExecStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type ContainerLogsStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type MachineLogsStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;

    async fn describe_contract(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        match self.describe_outcomes.lock().unwrap().pop_front() {
            Some(DescribeOutcome::Status(status)) => return Err(status),
            Some(DescribeOutcome::Remote(error)) => {
                return Ok(Response::new(RpcResponse::from(error).encode().unwrap()));
            }
            None => {}
        }
        let request = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        if !matches!(request.body, RpcRequestBody::DescribeContract(_)) {
            return Err(Status::invalid_argument("expected discovery request"));
        }
        Ok(Response::new(
            RpcResponse::from(self.description.clone())
                .encode()
                .unwrap(),
        ))
    }

    async fn inspect(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn machine_token(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn initialize(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn register(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn join(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn list_machines(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Ok(Response::new(
            RpcResponse::from(MachineList {
                machines: vec![machine('a', "one")],
            })
            .encode()
            .unwrap(),
        ))
    }

    async fn list_containers(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn create_volume(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn inspect_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn list_volumes(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let machine_id =
            MachineId::parse(request.metadata().get("machine").unwrap().to_str().unwrap()).unwrap();
        let request = request.into_inner().decode_request().unwrap();
        assert!(matches!(request.body, RpcRequestBody::ListVolumes(_)));
        let response = if machine_id.as_str().starts_with('b') {
            RpcResponse::from(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "target unavailable".into(),
                details: Value::Null,
            })
        } else {
            RpcResponse::from(VolumeList {
                volumes: vec![DockerVolume {
                    id: DockerVolumeId {
                        machine_id,
                        name: DockerVolumeName::parse("data").unwrap(),
                    },
                    driver: "local".into(),
                    options: Default::default(),
                    labels: Default::default(),
                }],
            })
        };
        Ok(Response::new(response.encode().unwrap()))
    }

    async fn inspect_volume(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn create_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn remove_volume(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn start_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn stop_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn remove_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn exec(
        &self,
        _request: Request<Streaming<OpaquePayload>>,
    ) -> Result<Response<Self::ExecStream>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn container_logs(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::ContainerLogsStream>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn machine_logs(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::MachineLogsStream>, Status> {
        self.stream_opens.fetch_add(1, Ordering::SeqCst);
        Ok(Response::new(tokio_stream::empty()))
    }

    async fn list_images(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn ensure_image_ingest(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn pull_image_from_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn get_caddy_config(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn reserve_domain(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn get_domain(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn release_domain(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn create_domain_records(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn reset(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn update_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn remove_local_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn remove_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn inspect_wireguard(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }
}

pub(super) fn machine(hex: char, name: &str) -> MachineObservation {
    MachineObservation {
        machine: Machine {
            id: machine_id(hex),
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
    }
}

pub(super) fn machine_id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

pub(super) fn test_description() -> ContractDescription {
    ContractDescription {
        machine_id: MachineId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "test".into(),
        capabilities: Default::default(),
    }
}

pub(super) async fn connected_client(
    service: DiscoveryService,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    Arc<AtomicUsize>,
) {
    let (address, server) = serve_discovery(service).await;
    let connects = Arc::new(AtomicUsize::new(0));
    let client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp(address)],
        },
        Arc::new(CountingConnector::new(connects.clone())),
    )
    .await
    .unwrap();
    (client, server, connects)
}
