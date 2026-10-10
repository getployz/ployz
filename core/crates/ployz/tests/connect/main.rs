use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
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
    CORROSION_GOSSIP_PORT, CapabilityName, ContainerKind, ContainerRuntimeObservation,
    ContractDescription, DescribeContractRequest, DockerVolume, DockerVolumeId, DockerVolumeName,
    HealthObservation, LogsOptions, MACHINE_API_PORT, MachineId, MachineRpcServer,
    MembershipObservation, NAMESPACE_LABEL, PROTOCOL_MAJOR, RpcError, RpcErrorCode,
    UNREGISTRY_PORT, op,
};
use serde_json::{Value, json};
use tokio::net::{TcpListener, UnixListener};
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tokio_util::sync::CancellationToken;
use tonic::{
    Status,
    transport::{Channel, Endpoint, Server},
};

mod machine_storage;
mod removal_cli;
mod sdk;
mod sdk_data_loss;
mod sdk_destroy_cluster;
mod sdk_destroy_namespace;
mod sdk_drain;
mod sdk_prepare;
mod sdk_register;
mod sdk_remove_machine;
mod sdk_update_machine;
mod sdk_upgrade_machine;
mod sdk_volumes;
mod sdk_watch;
mod support;
mod unix_session;
use support::*;

#[tokio::test]
async fn cli_exec_requires_exit_status_or_detached_start_confirmation() {
    use ployz_core::ExecResponseFrame::{ExecId, Exit, Stdout};
    use std::time::Duration;
    for (detach, frames, exit) in [
        (false, vec![], 1),
        (
            false,
            vec![ExecId("started".into()), Stdout(b"partial".to_vec())],
            1,
        ),
        (false, vec![ExecId("started".into()), Exit(0)], 0),
        (false, vec![ExecId("started".into()), Exit(7)], 7),
        (true, vec![], 1),
        (true, vec![ExecId("started".into())], 0),
    ] {
        let mut service = DiscoveryService::new(test_description());
        service.exec_frames = Some(frames);
        service
            .listed_containers
            .lock()
            .unwrap()
            .push(listing_container(
                'a',
                'a',
                "web",
                ContainerKind::ServiceContainer,
                ContainerRuntimeObservation::Running {
                    health: HealthObservation::Healthy,
                },
            ));
        let (address, server) = serve_discovery(service).await;
        let config = tempfile::NamedTempFile::new().unwrap();
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
        command.kill_on_drop(true);
        command.args([
            "--connect",
            &format!("tcp://{address}"),
            "--ployz-config",
            config.path().to_str().unwrap(),
            "exec",
            "-T",
            "app/web",
        ]);
        if detach {
            command.arg("--detach");
        }
        let result = tokio::time::timeout(Duration::from_secs(10), command.arg("true").output())
            .await
            .unwrap()
            .unwrap();
        server.abort();
        assert_eq!(
            result.status.code(),
            Some(exit),
            "detach={detach}: {result:?}"
        );
        if exit == 1 {
            let error = String::from_utf8_lossy(&result.stderr);
            assert!(error.contains("Exec stream ended"), "{error}");
        }
    }
}

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
    let unregistry = format!("10.210.0.1:{UNREGISTRY_PORT}");
    assert!(matches!(
        connector.dial_proxy(&tcp, "tcp", &unregistry).await,
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
                selection: ConnectionSource::Context(name),
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
        connections: [MACHINE_API_PORT, CORROSION_GOSSIP_PORT]
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
        vec![
            format!("tcp://127.0.0.1:{MACHINE_API_PORT}"),
            format!("tcp://127.0.0.1:{CORROSION_GOSSIP_PORT}"),
        ]
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
    let [volume] = success.value.volumes.as_slice() else {
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
    service.listed_containers.lock().unwrap().extend([
        listing_container(
            'c',
            'c',
            "api",
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
        ),
        listing_container(
            'd',
            'd',
            "worker",
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Unhealthy,
            },
        ),
        listing_container(
            'e',
            'd',
            "worker",
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Starting,
            },
        ),
        listing_container(
            'f',
            'd',
            "worker",
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Exited { code: 1 },
        ),
        listing_container(
            '0',
            'd',
            "worker",
            ContainerKind::PreDeployHook,
            ContainerRuntimeObservation::Exited { code: 0 },
        ),
    ]);
    service.listed_volumes.lock().unwrap().insert(
        machine_id('a'),
        vec![DockerVolume {
            id: DockerVolumeId {
                machine_id: machine_id('a'),
                name: DockerVolumeName::parse("data").unwrap(),
            },
            options: BTreeMap::from([("type".into(), "none".into())]),
            labels: BTreeMap::from([(NAMESPACE_LABEL.into(), "app".into())]),
            storage: ployz_core::DockerVolumeStorageObservation::Plain {
                driver: "local".into(),
            },
        }],
    );
    let mut down = machine('b', "down");
    down.membership = MembershipObservation::Down;
    service.machines.push(down);
    let (address, server) = serve_discovery(service).await;

    // The down Machine is omitted from every listing, so each result is partial.
    let json_cases: &[(&[&str], &str, &str)] = &[(
        &["ps", "--json"],
        "/containers/0/resolved_spec/container/image",
        "alpine:3.23.3",
    )];
    for (args, pointer, expected) in json_cases {
        let output = run_ployz(address, args).await;
        assert_eq!(output.status.code(), Some(3), "{args:?}: {output:?}");
        let document: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{args:?}: {error}: {output:?}"));
        assert_eq!(
            document.pointer(pointer).and_then(Value::as_str),
            Some(*expected),
            "{args:?}: {document}"
        );
        assert_eq!(
            document.pointer("/omitted"),
            Some(&serde_json::json!(["b".repeat(32)])),
            "{args:?}"
        );
        assert!(!output.stderr.is_empty(), "{args:?}: expected diagnostics");
    }

    let human_cases = [(
        &["ps"][..],
        format!(
            "SERVICE\tKIND\tSERVER\tSTATE\tCONTAINER\napp/api\tservice\tone\trunning, healthy\t-\napp/worker\tpre-deploy hook\tone\texited with code 0\t-\napp/worker\tservice\tone\trunning, unhealthy\t{}\napp/worker\tservice\tone\trunning, starting\t{}\napp/worker\tservice\tone\texited with code 1\t{}\n",
            "d".repeat(12),
            "e".repeat(12),
            "f".repeat(12)
        ),
    )];
    for (args, expected) in human_cases {
        let output = run_ployz(address, args).await;
        assert_eq!(output.status.code(), Some(3), "{args:?}: {output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            expected,
            "{args:?}"
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("did not answer"), "{args:?}: {stderr}");
        assert!(!stderr.contains(&"b".repeat(32)), "{args:?}: {stderr}");
    }

    // A removed Service prints from the Servers that answered; the down one
    // is named and the command is partial.
    let output = run_ployz(address, &["logs", "app/gone"]).await;
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.ends_with(" one gone/999999999999 | shutting down\n"),
        "{stdout}"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("! down did not answer"), "{stderr}");

    // Asking only the down Server reads nothing and names it.
    let output = run_ployz(address, &["logs", "app/api", "--machine", "down"]).await;
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("! down did not answer"), "{stderr}");
    server.abort();
}

#[tokio::test]
async fn cli_exact_history_reads_without_docker_discovery() {
    let mut service = DiscoveryService::new(test_description());
    service.machines.push(machine('b', "other"));
    service.container_list_outcomes.lock().unwrap().insert(
        machine_id('a'),
        VecDeque::from([Err(Status::internal("Docker is unavailable"))]),
    );
    let lists = service.container_list_calls.clone();
    let requests = service.history_requests.clone();
    let (address, server) = serve_discovery(service).await;
    for target in ["one".to_owned(), machine_id('a').to_string()] {
        let output = run_ployz(
            address,
            &[
                "logs",
                "app/gone",
                "--machine",
                &target,
                "-n",
                "7",
                "--json",
            ],
        )
        .await;
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let record: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            record.get("line").and_then(Value::as_str),
            Some("shutting down")
        );
        assert!(output.stderr.is_empty(), "{output:?}");
    }
    assert!(lists.lock().unwrap().is_empty());
    let expected = ployz_core::LogHistoryRequest {
        namespace: Some("app".into()),
        service: Some("gone".into()),
        direction: ployz_core::LogDirection::Backward,
        limit: 7,
        ..Default::default()
    };
    assert_eq!(
        *requests.lock().unwrap(),
        vec![
            (machine_id('a'), expected.clone()),
            (machine_id('a'), expected)
        ]
    );
    server.abort();
}

#[tokio::test]
async fn cli_exact_history_reports_history_failures_and_membership_omissions() {
    let mut service = DiscoveryService::new(test_description());
    service.machines.push(machine('b', "failed"));
    let mut omitted = machine('c', "unrecognized");
    omitted.membership_evidence =
        Some(ployz_core::MembershipEvidence::Unrecognized { raw: "up".into() });
    service.machines.push(omitted);
    let mut unrelated = machine('d', "unrelated");
    unrelated.membership = MembershipObservation::Down;
    service.machines.push(unrelated);
    service
        .history_failures
        .lock()
        .unwrap()
        .insert(machine_id('b'), Status::unavailable("history unavailable"));
    let requests = service.history_requests.clone();
    let lists = service.container_list_calls.clone();
    let (address, server) = serve_discovery(service).await;
    for json in [false, true] {
        let mut args = vec![
            "logs",
            "app/gone",
            "--machine",
            "one",
            "--machine",
            "failed",
            "--machine",
            "unrecognized",
        ];
        if json {
            args.push("--json");
        }
        let output = run_ployz(address, &args).await;
        assert_eq!(output.status.code(), Some(3), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stdout.contains("shutting down"), "{stdout}");
        for name in ["failed", "unrecognized"] {
            assert_eq!(
                stderr.matches(&format!("! {name} did not answer")).count(),
                1,
                "{stderr}"
            );
        }
        assert!(!stderr.contains("unrelated"), "{stderr}");
        if json {
            let rows = stdout
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            let [record, gaps] = rows.as_slice() else {
                panic!("{rows:?}")
            };
            assert_eq!(
                record.get("line").and_then(Value::as_str),
                Some("shutting down")
            );
            assert_eq!(gaps.get("omitted"), Some(&json!([machine_id('c')])));
            let [failure] = gaps.get("failures").unwrap().as_array().unwrap().as_slice() else {
                panic!("{gaps}")
            };
            assert_eq!(failure.get("machine_id"), Some(&json!(machine_id('b'))));
            assert_eq!(
                failure.pointer("/error/code").and_then(Value::as_str),
                Some("unavailable")
            );
        }
    }
    assert!(lists.lock().unwrap().is_empty());
    let mut targets = requests
        .lock()
        .unwrap()
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    targets.sort();
    assert_eq!(
        targets,
        vec![
            machine_id('a'),
            machine_id('a'),
            machine_id('b'),
            machine_id('b')
        ]
    );
    server.abort();
}

#[tokio::test]
async fn cli_history_keeps_live_resolution_for_ineligible_requests() {
    let service = DiscoveryService::new(test_description());
    let outcomes = service.container_list_outcomes.clone();
    let lists = service.container_list_calls.clone();
    let requests = service.history_requests.clone();
    let (address, server) = serve_discovery(service).await;
    for args in [
        vec!["logs", "missing"],
        vec!["logs", "app/gone", "missing"],
        vec!["logs", "11111111111111111111111111111111"],
        vec!["logs", "app/api:api-1"],
        vec!["logs"],
        vec!["logs", "app/gone", "--follow"],
    ] {
        lists.lock().unwrap().clear();
        outcomes.lock().unwrap().insert(
            machine_id('a'),
            VecDeque::from([Err(Status::internal("Docker is unavailable"))]),
        );
        let output = run_ployz(address, &args).await;
        assert!(!output.status.success(), "{args:?}: {output:?}");
        assert!(
            lists.lock().unwrap().contains_key(&machine_id('a')),
            "{args:?}"
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        let warning = stderr
            .find("! one did not answer")
            .unwrap_or_else(|| panic!("{args:?}: {stderr}"));
        if args == ["logs", "missing"] {
            let error = stderr.find("error:").unwrap_or_else(|| panic!("{stderr}"));
            assert!(warning < error, "{stderr}");
        }
        assert!(requests.lock().unwrap().is_empty(), "{args:?}");
    }
    server.abort();
}

#[tokio::test]
async fn history_read_cancels_a_blocked_request_and_propagates_output_errors() {
    use ployz::operator::history::{HistorySelector, HistoryWindow, read_history};
    let received = Arc::new(tokio::sync::Notify::new());
    let mut service = DiscoveryService::new(test_description());
    service.history_blocked = Some(received.clone());
    let (client, server, _) = connected_client(service).await;
    let targets = [(
        machine_id('a'),
        ployz_core::MachineName::parse("one").unwrap(),
    )];
    let selectors = [HistorySelector {
        namespace: Some("app".into()),
        service: Some("gone".into()),
        deployment: Some("dep_old".into()),
        container_id: None,
    }];
    let window = HistoryWindow::Last {
        lines: 7,
        since: None,
        until: None,
    };
    let cancellation = CancellationToken::new();
    let read = read_history(
        &client,
        &targets,
        &selectors,
        window,
        &cancellation,
        |_| -> Result<(), &str> { panic!("blocked history cannot emit a record") },
    );
    let cancel = async {
        received.notified().await;
        cancellation.cancel();
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(read, cancel)
    })
    .await
    .unwrap();
    assert!(result.unwrap().is_empty());
    server.abort();

    let (client, server, _) = connected_client(DiscoveryService::new(test_description())).await;
    let result = read_history(
        &client,
        &targets,
        &selectors,
        window,
        &CancellationToken::new(),
        |_| Err("output closed"),
    )
    .await;
    assert_eq!(result, Err("output closed"));
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

fn listing_container(
    container_hex: char,
    service_hex: char,
    name: &str,
    kind: ContainerKind,
    runtime: ContainerRuntimeObservation,
) -> ployz_core::ContainerObservation {
    let service_id = ployz_core::ServiceId::parse(service_hex.to_string().repeat(32)).unwrap();
    let service_name = ployz_core::ServiceName::parse(name).unwrap();
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        container_id: ployz_core::ContainerId::parse(container_hex.to_string().repeat(64)).unwrap(),
        display_name: format!("{name}-{container_hex}"),
        created_at_unix_nanos: 1,
        machine_id: machine_id('a'),
        namespace: ployz_core::Namespace::parse("app").unwrap(),
        kind,
        runtime,
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
    })
    .unwrap()
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

#[tokio::test(start_paused = true)]
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

    let result = client
        .live_services(ployz_core::EnvironmentValues::Redacted)
        .await
        .unwrap()
        .containers;

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
        capabilities: BTreeSet::from([
            CapabilityName::parse("ployz.rpc.describe-contract.v1").unwrap()
        ]),
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

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
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
            cause: Vec::new(),
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

#[tokio::test(start_paused = true)]
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
            since_nanos: None,
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

#[tokio::test]
async fn redial_rechecks_expected_machine_identity() {
    let expected = test_description();
    let first = DiscoveryService::new(expected.clone());
    let mut replacement = expected.clone();
    replacement.machine_id = machine_id('b');
    let (address_a, server_a) = serve_discovery(first.clone()).await;
    let (address_b, server_b) = serve_discovery(DiscoveryService::new(replacement)).await;
    let connects = Arc::new(AtomicUsize::new(0));
    let mut client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp(address_a).with_machine_id(expected.machine_id)],
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
            "transport dropped",
        )));
    let error = client
        .call::<op::DescribeContract>(DescribeContractRequest {}, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("identity mismatch"));
    assert_eq!(connects.load(Ordering::SeqCst), 2);
    server_a.abort();
    server_b.abort();
}
