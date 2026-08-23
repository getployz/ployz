use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use ployz::{
    connect::{
        BoxProxyStream, ConnectError, Connector, SystemConnector, connect_selected_with,
        resolve_connections,
    },
    context::{Connection, ConnectionSource, SelectedConnections},
    operator::open_machine_logs,
};
use ployz_core::{
    CapabilityName, ContractDescription, DescribeContractRequest, DockerVolume, DockerVolumeId,
    DockerVolumeName, LogsOptions, MachineId, MachineRpcServer, MembershipObservation,
    PROJECT_NAME_LABEL, PROTOCOL_MAJOR, RpcError, RpcErrorCode, op,
};
use serde_json::{Value, json};
use tokio::net::{TcpListener, UnixListener};
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tokio_util::sync::CancellationToken;
use tonic::{
    Status,
    transport::{Channel, Endpoint, Server},
};

mod relay;
mod sdk;
mod sdk_data_loss;
mod sdk_destroy_cluster;
mod sdk_destroy_project;
mod sdk_register;
mod sdk_remove_machine;
mod sdk_volumes;
mod sdk_watch;
mod support;
use support::*;

struct FakeConnector {
    outcomes: Mutex<VecDeque<bool>>,
    attempts: Mutex<Vec<String>>,
}

/// First `lazy` connects report success with a lazy channel (tunnel up, daemon
/// not reached). Later connects use [`SystemConnector`].
struct LazyThenLive {
    lazy: AtomicUsize,
    inner: SystemConnector,
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

#[tonic::async_trait]
impl Connector for LazyThenLive {
    async fn connect(&self, connection: &Connection) -> Result<Channel, ConnectError> {
        if self.lazy.fetch_sub(1, Ordering::SeqCst) > 0 {
            return Ok(Endpoint::from_static("http://127.0.0.1:1").connect_lazy());
        }
        self.inner.connect(connection).await
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
async fn lazy_first_connection_falls_through_to_a_healthy_machine() {
    let description = test_description();
    let (live, server) = serve_discovery(DiscoveryService::new(description.clone())).await;
    let live = Connection::tcp(live);

    let mut client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Context("prod".into()),
            connections: vec![
                Connection::tcp("127.0.0.1:1".parse().unwrap()),
                live.clone(),
            ],
        },
        Arc::new(LazyThenLive {
            lazy: AtomicUsize::new(1),
            inner: SystemConnector::default(),
        }),
    )
    .await
    .unwrap();

    assert_eq!(client.connection().to_string(), live.to_string());
    assert_eq!(
        client
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await
            .unwrap(),
        description
    );
    server.abort();
}

#[tokio::test]
async fn exhausting_every_connection_reports_how_many_were_tried() {
    let error = match connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Context("prod".into()),
            connections: vec![
                Connection::tcp("127.0.0.1:1".parse().unwrap()),
                Connection::tcp("127.0.0.1:2".parse().unwrap()),
            ],
        },
        Arc::new(LazyThenLive {
            lazy: AtomicUsize::new(2),
            inner: SystemConnector::default(),
        }),
    )
    .await
    {
        Ok(_) => panic!("expected every connection to fail"),
        Err(error) => error,
    };

    assert!(
        matches!(
            &error,
            ConnectError::AllFailed {
                attempts: 2,
                source: ConnectionSource::Context(name),
                ..
            } if name == "prod"
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn ordered_connections_stop_after_the_first_success() {
    let dropped = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unreachable = dropped.local_addr().unwrap();
    drop(dropped);
    let (live, server) = serve_discovery(DiscoveryService::new(test_description())).await;
    let unused = "127.0.0.1:9".parse().unwrap();
    let connects = Arc::new(AtomicUsize::new(0));

    let client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Context("prod".into()),
            connections: vec![
                Connection::tcp(unreachable),
                Connection::tcp(live),
                Connection::tcp(unused),
            ],
        },
        Arc::new(CountingConnector::new(connects.clone())),
    )
    .await
    .unwrap();

    assert_eq!(client.connection().to_string(), format!("tcp://{live}"));
    assert_eq!(
        client.connection_source(),
        &ConnectionSource::Context("prod".into())
    );
    assert_eq!(connects.load(Ordering::SeqCst), 2);
    server.abort();
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

#[tokio::test]
async fn volume_listing_retains_successes_and_target_failures() {
    let (address, server) = serve_discovery(DiscoveryService::new(ContractDescription {
        machine_id: MachineId::random(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "test".into(),
        capabilities: Default::default(),
    }))
    .await;
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

#[tokio::test]
async fn listing_commands_emit_full_json_and_preserve_human_output() {
    let mut service = DiscoveryService::new(test_description());
    service
        .listed_containers
        .lock()
        .unwrap()
        .push(listing_container());
    service.listed_volumes.lock().unwrap().insert(
        machine_id('a'),
        vec![DockerVolume {
            id: DockerVolumeId {
                machine_id: machine_id('a'),
                name: DockerVolumeName::parse("data").unwrap(),
            },
            driver: "local".into(),
            options: BTreeMap::from([("type".into(), "none".into())]),
            labels: BTreeMap::from([(PROJECT_NAME_LABEL.into(), "app".into())]),
        }],
    );
    let mut down = machine('b', "down");
    down.membership = MembershipObservation::Down;
    service.machines.push(down);
    let (address, server) = serve_discovery(service).await;

    let json_cases: &[(&[&str], &str, &str)] = &[
        (&["ls", "--output", "json"], "/0/identity", "app/api"),
        (
            &["service", "ls", "-o", "json"],
            "/0/containers/0/resolved_spec/container/image",
            "alpine:3.23.3",
        ),
        (
            &["ps", "--output", "json"],
            "/0/resolved_spec/container/image",
            "alpine:3.23.3",
        ),
        (
            &["volume", "ls", "-q", "-o", "json"],
            "/0/volume/options/type",
            "none",
        ),
        (
            &["project", "ls", "--output", "json"],
            "/0/services/0",
            "app/api",
        ),
    ];
    for (args, pointer, expected) in json_cases {
        let output = run_ployz(address, args).await;
        assert!(output.status.success(), "{args:?}: {output:?}");
        let document: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{args:?}: {error}: {output:?}"));
        assert_eq!(
            document.pointer(pointer).and_then(Value::as_str),
            Some(*expected),
            "{args:?}: {document}"
        );
        assert!(!output.stderr.is_empty(), "{args:?}: expected diagnostics");
    }

    let service_id = "c".repeat(32);
    let container_id = "c".repeat(64);
    let machine_id = "a".repeat(32);
    let human_cases = [
        (
            &["ls"][..],
            format!("SERVICE ID\tSERVICE\tCONTAINERS\tHOOKS\n{service_id}\tapp/api\t1\t0\n"),
        ),
        (
            &["service", "ls"][..],
            format!("SERVICE ID\tSERVICE\tCONTAINERS\tHOOKS\n{service_id}\tapp/api\t1\t0\n"),
        ),
        (
            &["ps"][..],
            format!(
                "CONTAINER ID\tSERVICE\tKIND\tMACHINE\tSTATE\n{container_id}\tapp/api\tServiceContainer\t{machine_id}\tRunning {{ health: Healthy }}\n"
            ),
        ),
        (
            &["volume", "ls"][..],
            "MACHINE\tVOLUME\tDRIVER\none\tdata\tlocal\n".into(),
        ),
        (
            &["project", "ls"][..],
            "PROJECT\tSERVICES\tVOLUMES\napp\t1\t1\n".into(),
        ),
    ];
    for (args, expected) in human_cases {
        let output = run_ployz(address, args).await;
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            expected,
            "{args:?}"
        );
    }
    let quiet = run_ployz(address, &["volume", "ls", "-q"]).await;
    assert!(quiet.status.success(), "{quiet:?}");
    assert_eq!(quiet.stdout, b"data\n");
    server.abort();
}

async fn run_ployz(address: std::net::SocketAddr, args: &[&str]) -> std::process::Output {
    tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("--connect")
        .arg(format!("tcp://{address}"))
        .args(args)
        .output()
        .await
        .unwrap()
}

fn listing_container() -> ployz_core::ContainerObservation {
    let service_id = ployz_core::ServiceId::parse("c".repeat(32)).unwrap();
    let service_name = ployz_core::ServiceName::parse("api").unwrap();
    ployz_core::ContainerObservation {
        container_id: ployz_core::ContainerId::parse("c".repeat(64)).unwrap(),
        display_name: "api-1".into(),
        created_at_unix_nanos: 1,
        machine_id: machine_id('a'),
        project_name: ployz_core::ProjectName::parse("app").unwrap(),
        service_id,
        service_name: service_name.clone(),
        kind: ployz_core::ContainerKind::ServiceContainer,
        runtime: ployz_core::ContainerRuntimeObservation::Running {
            health: ployz_core::HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: serde_json::from_value(json!({
            "service_id": service_id,
            "name": service_name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
        }))
        .unwrap(),
        address: None,
        labels: BTreeMap::from([("detail".into(), "preserved".into())]),
    }
}

#[tokio::test]
async fn volume_remove_succeeds_for_a_visible_owner_when_an_unrelated_machine_is_unreachable() {
    let description = test_description();
    let mut service = DiscoveryService::new(description);
    service.machines = vec![machine('a', "owner"), machine('b', "unreachable")];
    let removed_volumes = Arc::clone(&service.removed_volumes);
    let listed_volumes = Arc::clone(&service.listed_volumes);
    let (address, server) = serve_discovery(service).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "volume",
            "rm",
            "data",
            "--yes",
        ])
        .output()
        .await
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        *removed_volumes.lock().unwrap(),
        [DockerVolumeId {
            machine_id: machine_id('a'),
            name: DockerVolumeName::parse("data").unwrap(),
        }]
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("WARNING:"), "{stderr}");
    assert!(stderr.contains(machine_id('b').as_str()), "{stderr}");
    assert!(stderr.contains("not checked"), "{stderr}");
    assert!(
        stderr.contains("may hold a same-named Docker Volume"),
        "{stderr}"
    );

    removed_volumes.lock().unwrap().clear();
    let exact = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "volume",
            "rm",
            "data",
            "--machine",
            "owner",
            "--yes",
        ])
        .output()
        .await
        .unwrap();
    assert!(exact.status.success(), "{exact:?}");
    assert!(exact.stderr.is_empty(), "{exact:?}");
    assert_eq!(removed_volumes.lock().unwrap().len(), 1);

    removed_volumes.lock().unwrap().clear();
    listed_volumes.lock().unwrap().insert(
        machine_id('a'),
        vec![DockerVolume {
            id: DockerVolumeId {
                machine_id: machine_id('a'),
                name: DockerVolumeName::parse("busy").unwrap(),
            },
            driver: "local".into(),
            options: Default::default(),
            labels: Default::default(),
        }],
    );
    let failed = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "volume",
            "rm",
            "busy",
            "--yes",
        ])
        .output()
        .await
        .unwrap();
    assert!(!failed.status.success(), "{failed:?}");
    assert!(removed_volumes.lock().unwrap().is_empty());
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("volume is in use"),
        "{failed:?}"
    );

    let unseen = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "volume",
            "rm",
            "unseen",
            "--yes",
        ])
        .output()
        .await
        .unwrap();
    assert!(!unseen.status.success(), "{unseen:?}");
    assert!(
        String::from_utf8_lossy(&unseen.stderr)
            .contains("was not checked and may hold a same-named Docker Volume"),
        "{unseen:?}"
    );
    server.abort();
}

#[tokio::test]
async fn volume_listing_omits_down_and_unknown_and_probes_suspect() {
    let (address, server) = serve_discovery(DiscoveryService::new(ContractDescription {
        machine_id: MachineId::random(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "test".into(),
        capabilities: Default::default(),
    }))
    .await;
    let mut client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp(address)],
        },
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();

    let mut down = machine('e', "down");
    down.membership = MembershipObservation::Down;
    let mut unknown = machine('c', "unknown");
    unknown.membership = MembershipObservation::Unknown;
    let mut suspect = machine('b', "suspect");
    suspect.membership = MembershipObservation::Suspect;
    let result = client
        .list_volumes(&[machine('a', "one"), down, unknown, suspect])
        .await;

    assert_eq!(
        result
            .successes
            .iter()
            .map(|success| success.machine_id)
            .collect::<Vec<_>>(),
        vec![machine_id('a')]
    );
    assert_eq!(
        result
            .failures
            .iter()
            .map(|failure| failure.machine_id)
            .collect::<Vec<_>>(),
        vec![machine_id('b')]
    );
    assert_eq!(result.omissions, vec![machine_id('e'), machine_id('c')]);
    server.abort();
}

#[tokio::test]
async fn fanout_reads_retry_failed_legs_without_rerunning_successes() {
    let mut service = DiscoveryService::new(test_description());
    service.machines = vec![machine('a', "recovers"), machine('b', "fails")];
    service.container_list_outcomes.lock().unwrap().extend([
        (
            machine_id('a'),
            VecDeque::from([
                Err(Status::unavailable("transient")),
                Ok(ployz_core::ContainerList {
                    containers: Vec::new(),
                }),
            ]),
        ),
        (
            machine_id('b'),
            VecDeque::from([
                Err(Status::unavailable("permanent")),
                Err(Status::unavailable("permanent")),
                Err(Status::unavailable("permanent")),
                Err(Status::unavailable("permanent")),
            ]),
        ),
    ]);
    let (mut client, server, _) = connected_client(service.clone()).await;

    let result = client.live_services().await.unwrap().containers;

    assert_eq!(
        result
            .successes
            .iter()
            .map(|success| success.machine_id)
            .collect::<Vec<_>>(),
        [machine_id('a')]
    );
    let [failure] = result.failures.as_slice() else {
        panic!("expected one exhausted failure: {result:?}")
    };
    assert_eq!(failure.machine_id, machine_id('b'));
    assert_eq!(failure.error.code, RpcErrorCode::Unavailable);
    assert_eq!(
        *service.container_list_calls.lock().unwrap(),
        BTreeMap::from([(machine_id('a'), 2), (machine_id('b'), 4)])
    );

    server.abort();
}

#[tokio::test]
async fn machines_returns_list_machines_membership_observations() {
    let (address, server) = serve_discovery(DiscoveryService::new(ContractDescription {
        machine_id: MachineId::random(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "test".into(),
        capabilities: Default::default(),
    }))
    .await;
    let mut client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp(address)],
        },
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();

    let observed = client.machines().await.unwrap();

    assert_eq!(observed, vec![machine('a', "one")]);
    server.abort();
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
    let service = DiscoveryService::new(description.clone());
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
        assert_eq!(
            client
                .call::<op::DescribeContract>(DescribeContractRequest {}, None)
                .await
                .unwrap(),
            description
        );
    }

    let fallback = resolve_connections(&config, None, None, &socket).unwrap();
    let mut fallback = connect_selected_with(fallback, Arc::new(SystemConnector::default()))
        .await
        .unwrap();
    assert_eq!(
        fallback
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await
            .unwrap(),
        description
    );

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
    assert_eq!(
        direct
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await
            .unwrap(),
        description
    );

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
    assert_eq!(
        failed_over
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await
            .unwrap(),
        description
    );

    tcp_server.abort();
    unix_server.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn unary_call_retries_unavailable_after_redial() {
    let description = test_description();
    let service = DiscoveryService::new(description.clone());
    let (mut client, server, connects) = connected_client(service.clone()).await;
    service
        .describe_outcomes
        .lock()
        .unwrap()
        .push_back(DescribeOutcome::Status(Status::unavailable(
            "transport error",
        )));

    assert_eq!(
        client
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await
            .unwrap(),
        description
    );
    assert_eq!(connects.load(Ordering::SeqCst), 2);

    server.abort();
}

#[tokio::test]
async fn unary_call_does_not_retry_remote_or_not_found() {
    let not_found = DiscoveryService::new(test_description());
    let (mut client, server, connects) = connected_client(not_found.clone()).await;
    not_found
        .describe_outcomes
        .lock()
        .unwrap()
        .push_back(DescribeOutcome::Status(Status::not_found("missing")));
    let error = client
        .call::<op::DescribeContract>(DescribeContractRequest {}, None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ConnectError::Rpc(error) if error.is_not_found()),
        "{error:?}"
    );
    assert_eq!(connects.load(Ordering::SeqCst), 1);
    server.abort();

    let remote = DiscoveryService::new(test_description());
    let (mut client, server, connects) = connected_client(remote.clone()).await;
    remote
        .describe_outcomes
        .lock()
        .unwrap()
        .push_back(DescribeOutcome::Remote(RpcError {
            code: RpcErrorCode::Conflict,
            message: "already a member".into(),
            details: Value::Null,
        }));
    let error = client
        .call::<op::DescribeContract>(DescribeContractRequest {}, None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &error,
            ConnectError::Remote(RpcError {
                code: RpcErrorCode::Conflict,
                ..
            })
        ),
        "{error:?}"
    );
    assert_eq!(connects.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn unary_call_gives_up_after_four_unavailable_attempts() {
    let service = DiscoveryService::new(test_description());
    let (mut client, server, connects) = connected_client(service.clone()).await;
    service.describe_outcomes.lock().unwrap().extend([
        DescribeOutcome::Status(Status::unavailable("drop 1")),
        DescribeOutcome::Status(Status::unavailable("drop 2")),
        DescribeOutcome::Status(Status::unavailable("drop 3")),
        DescribeOutcome::Status(Status::unavailable("drop 4")),
    ]);

    let error = client
        .call::<op::DescribeContract>(DescribeContractRequest {}, None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ConnectError::Rpc(error) if error.is_unavailable()),
        "{error:?}"
    );
    assert_eq!(connects.load(Ordering::SeqCst), 4);

    server.abort();
}

#[tokio::test]
async fn stream_after_redial_uses_the_replaced_channel() {
    let first = DiscoveryService::new(test_description());
    let second = DiscoveryService::new(test_description());
    let (address_a, server_a) = serve_discovery(first.clone()).await;
    let (address_b, server_b) = serve_discovery(second.clone()).await;
    let connects = Arc::new(AtomicUsize::new(0));
    let mut client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp(address_a)],
        },
        Arc::new(CountingConnector::redirecting(
            connects.clone(),
            [address_a, address_b],
        )),
    )
    .await
    .unwrap();
    first
        .describe_outcomes
        .lock()
        .unwrap()
        .push_back(DescribeOutcome::Status(Status::unavailable(
            "transport error",
        )));

    client
        .call::<op::DescribeContract>(DescribeContractRequest {}, None)
        .await
        .unwrap();
    assert_eq!(connects.load(Ordering::SeqCst), 2);

    let logs = open_machine_logs(
        &mut client,
        &[],
        &[],
        LogsOptions {
            follow: false,
            tail: 0,
            since_unix_seconds: None,
            until_unix_seconds: None,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(first.stream_opens.load(Ordering::SeqCst), 0);
    assert_eq!(second.stream_opens.load(Ordering::SeqCst), 1);

    server_a.abort();
    server_b.abort();
}
