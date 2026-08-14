use std::{
    collections::VecDeque,
    net::Ipv6Addr,
    sync::{Arc, Mutex},
};

use ployz::{
    connect::{
        BoxProxyStream, ConnectError, Connector, SystemConnector, connect_selected_with,
        resolve_connections,
    },
    context::{Connection, ConnectionSource, SelectedConnections},
};
use ployz_core::{
    AdvertisedEndpoint, CapabilityName, ContractDescription, DockerVolume, DockerVolumeId,
    DockerVolumeName, Machine, MachineId, MachineName, MachineObservation, MachineRpc,
    MachineRpcServer, MachineSubnet, ManagementAddress, MembershipObservation, OpaquePayload,
    PROTOCOL_MAJOR, RpcError, RpcErrorCode, RpcRequestBody, RpcResponse, WireGuardPublicKey,
};
use serde_json::Value;
use tokio::net::{TcpListener, UnixListener};
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tonic::{
    Request, Response, Status, Streaming,
    transport::{Channel, Endpoint, Server},
};

struct FakeConnector {
    outcomes: Mutex<VecDeque<bool>>,
    attempts: Mutex<Vec<String>>,
}

#[tokio::test]
async fn unix_proxy_dialing_is_direct_and_tcp_is_explicitly_unsupported() {
    let connector = SystemConnector::default();
    let unix = Connection::unix("/path/that/does/not/exist.sock").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stream = connector
        .dial_proxy(&unix, "tcp", &listener.local_addr().unwrap().to_string())
        .await
        .unwrap();
    let (accepted, _) = listener.accept().await.unwrap();
    drop((stream, accepted));

    let tcp = Connection::tcp("127.0.0.1:1".parse().unwrap());
    assert!(matches!(
        connector.dial_proxy(&tcp, "tcp", "10.210.0.1:51500").await,
        Err(ConnectError::ProxyUnsupported(_))
    ));
    assert!(matches!(
        connector.dial_proxy(&unix, "udp", "127.0.0.1:1").await,
        Err(ConnectError::UnsupportedNetwork(_))
    ));
}

#[tonic::async_trait]
impl Connector for FakeConnector {
    async fn connect(&self, connection: &Connection) -> Result<Channel, ConnectError> {
        self.attempts.lock().unwrap().push(connection.to_string());
        if self.outcomes.lock().unwrap().pop_front().unwrap() {
            Ok(Endpoint::from_static("http://127.0.0.1:1").connect_lazy())
        } else {
            Err(ConnectError::Attempt("unreachable".into()))
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

#[tokio::test]
async fn ordered_connections_stop_after_the_first_success() {
    let connector = Arc::new(FakeConnector {
        outcomes: Mutex::new(VecDeque::from([false, true, true])),
        attempts: Mutex::new(Vec::new()),
    });
    let selected = SelectedConnections {
        source: ConnectionSource::Context("prod".into()),
        connections: [51000, 51001, 51002]
            .map(|port| Connection::tcp(format!("127.0.0.1:{port}").parse().unwrap()))
            .into(),
    };

    let client = connect_selected_with(selected, connector.clone())
        .await
        .unwrap();

    assert_eq!(client.connection().to_string(), "tcp://127.0.0.1:51001");
    assert_eq!(
        client.connection_source(),
        &ConnectionSource::Context("prod".into())
    );
    assert_eq!(
        *connector.attempts.lock().unwrap(),
        ["tcp://127.0.0.1:51000", "tcp://127.0.0.1:51001"]
    );
}

#[tokio::test]
async fn failed_connection_attempts_do_not_reorder_the_context() {
    let connector = Arc::new(FakeConnector {
        outcomes: Mutex::new(VecDeque::from([false, false])),
        attempts: Mutex::new(Vec::new()),
    });
    let selected = SelectedConnections {
        source: ConnectionSource::Context("prod".into()),
        connections: [51000, 51001]
            .map(|port| Connection::tcp(format!("127.0.0.1:{port}").parse().unwrap()))
            .into(),
    };
    let original = selected.connections.clone();

    assert!(
        connect_selected_with(selected, connector.clone())
            .await
            .is_err()
    );

    assert_eq!(
        *connector.attempts.lock().unwrap(),
        original.iter().map(ToString::to_string).collect::<Vec<_>>()
    );
    assert_eq!(
        original.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["tcp://127.0.0.1:51000", "tcp://127.0.0.1:51001"]
    );
}

#[derive(Clone)]
struct DiscoveryService {
    description: ContractDescription,
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
        let request = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        if !matches!(request.body, RpcRequestBody::DescribeContract(_)) {
            return Err(Status::invalid_argument("expected discovery request"));
        }
        Ok(Response::new(
            RpcResponse::contract_description(self.description.clone())
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
        Err(Status::unimplemented("unused"))
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
            RpcResponse::error(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "target unavailable".into(),
                details: Value::Null,
            })
        } else {
            RpcResponse::volume_list(vec![DockerVolume {
                id: DockerVolumeId {
                    machine_id,
                    name: DockerVolumeName::parse("data").unwrap(),
                },
                driver: "local".into(),
                options: Default::default(),
                labels: Default::default(),
            }])
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
        Err(Status::unimplemented("unused"))
    }

    async fn list_images(
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

#[tokio::test]
async fn volume_listing_retains_successes_and_target_failures() {
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = tcp.local_addr().unwrap();
    let description = ContractDescription {
        machine_id: MachineId::random(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "test".into(),
        capabilities: Default::default(),
    };
    let server = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(DiscoveryService { description }))
            .serve_with_incoming(TcpListenerStream::new(tcp)),
    );
    let mut client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp(address)],
        },
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();

    let result = client
        .list_volumes(&[machine('a', "one"), machine('b', "two")])
        .await;

    let [success] = result.successes.as_slice() else {
        panic!("expected one success: {result:?}")
    };
    let [volume] = success.value.as_slice() else {
        panic!("expected one Volume: {success:?}")
    };
    assert_eq!(volume.id.machine_id, machine_id('a'));
    let [failure] = result.failures.as_slice() else {
        panic!("expected one failure: {result:?}")
    };
    assert_eq!(failure.machine_id, machine_id('b'));
    assert_eq!(failure.error.code, RpcErrorCode::Unavailable);
    server.abort();
}

fn machine(hex: char, name: &str) -> MachineObservation {
    MachineObservation {
        machine: Machine {
            id: machine_id(hex),
            name: MachineName::parse(name).unwrap(),
            subnet: MachineSubnet(
                format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                    .parse()
                    .unwrap(),
            ),
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

fn machine_id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

#[tokio::test]
async fn machine_discovery_uses_the_same_rpc_over_tcp_and_unix() {
    let root = std::env::temp_dir().join(format!("ployz-connect-{}", std::process::id()));
    let config = root.join("config.yaml");
    let socket = root.join("ployz.sock");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp_address = tcp.local_addr().unwrap();
    let unix = UnixListener::bind(&socket).unwrap();
    let description = ContractDescription {
        machine_id: MachineId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "test".into(),
        capabilities: [CapabilityName::parse("ployz.rpc.describe-contract.v1").unwrap()]
            .into_iter()
            .collect(),
    };
    let service = DiscoveryService {
        description: description.clone(),
    };
    let tcp_server = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(service.clone()))
            .serve_with_incoming(TcpListenerStream::new(tcp)),
    );
    let unix_server = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(service))
            .serve_with_incoming(UnixListenerStream::new(unix)),
    );
    for connection in [
        Connection::tcp(tcp_address),
        Connection::unix(&socket).unwrap(),
    ] {
        let mut client = connect_selected_with(
            SelectedConnections {
                source: ConnectionSource::Direct,
                connections: vec![connection],
            },
            Arc::new(SystemConnector::default()),
        )
        .await
        .unwrap();
        assert_eq!(client.describe_contract().await.unwrap(), description);
    }

    let fallback = resolve_connections(&config, None, None, &socket).unwrap();
    let mut fallback = connect_selected_with(fallback, Arc::new(SystemConnector::default()))
        .await
        .unwrap();
    assert_eq!(fallback.describe_contract().await.unwrap(), description);

    std::fs::write(&config, "deliberately: [unusable").unwrap();
    let direct = resolve_connections(
        &config,
        Some(&format!("tcp://{tcp_address}")),
        Some("missing"),
        &socket,
    )
    .unwrap();
    let mut direct = connect_selected_with(direct, Arc::new(SystemConnector::default()))
        .await
        .unwrap();
    assert_eq!(direct.describe_contract().await.unwrap(), description);

    let unavailable_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable = unavailable_listener.local_addr().unwrap();
    drop(unavailable_listener);
    let mut failed_over = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Context("prod".into()),
            connections: vec![
                Connection::tcp(unavailable),
                Connection::tcp(tcp_address),
                Connection::unix(&socket).unwrap(),
            ],
        },
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();
    assert_eq!(
        failed_over.connection().to_string(),
        format!("tcp://{tcp_address}")
    );
    assert_eq!(failed_over.describe_contract().await.unwrap(), description);

    tcp_server.abort();
    unix_server.abort();
    std::fs::remove_dir_all(root).unwrap();
}
