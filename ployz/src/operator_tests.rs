use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
    task::{Context, Poll},
};

use futures_util::stream;
use ployz_core::{
    ContainerRuntimeObservation, LogMetadata, LogOrigin, MachineId, MachineName,
    ResolvedServiceSpec, RestartPolicy, ServiceId, ServiceName,
};

use super::*;

#[test]
fn exec_mapping_and_container_selection_match_the_operator_contract() {
    let service = observed_service();
    assert_eq!(
        select_exec_container(&service, None).unwrap().display_name,
        "api-one"
    );
    assert_eq!(
        select_exec_container(&service, Some(&"b".repeat(64)))
            .unwrap()
            .display_name,
        "api-two"
    );
    assert_eq!(
        select_exec_container(&service, Some("b"))
            .unwrap()
            .display_name,
        "b"
    );
    assert!(matches!(
        select_exec_container(&service, Some("bb")),
        Err(ContainerSelectorError::Ambiguous { .. })
    ));
    assert_eq!(
        select_log_containers(&service, &["b".into()])
            .unwrap()
            .first()
            .unwrap()
            .display_name,
        "b"
    );
    let mut duplicate_names = service.clone();
    duplicate_names.containers.get_mut(1).unwrap().display_name = "api-one".into();
    assert!(matches!(
        select_exec_container(&duplicate_names, Some("api-one")),
        Err(ContainerSelectorError::Ambiguous { .. })
    ));
    let options = exec_options(
        Vec::new(),
        ExecMode {
            detach: false,
            no_tty: false,
            stdout_terminal: true,
            stdin_terminal: true,
        },
    )
    .unwrap();
    assert_eq!(
        options.command,
        DEFAULT_EXEC_COMMAND
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    assert!(options.tty && options.attach_stdin && options.attach_stdout);
    assert!(
        exec_options(
            Vec::new(),
            ExecMode {
                detach: false,
                no_tty: false,
                stdout_terminal: true,
                stdin_terminal: false,
            },
        )
        .is_err()
    );
    let detached = exec_options(
        vec!["true".into()],
        ExecMode {
            detach: true,
            no_tty: false,
            stdout_terminal: true,
            stdin_terminal: true,
        },
    )
    .unwrap();
    assert!(!detached.tty && !detached.attach_stdin && detached.detach);
}

#[test]
fn service_args_tail_and_proxy_ports_cover_the_argument_tables() {
    assert!(service_logs_use_compose(&[]));
    assert!(!service_logs_use_compose(&["api".into()]));
    assert_eq!(
        parse_service_args(&strings(["api/one", "api/two", "worker/x", "api"])).unwrap(),
        [
            ServiceArg {
                service: "api".into(),
                containers: vec![],
            },
            ServiceArg {
                service: "worker".into(),
                containers: vec!["x".into()],
            },
        ]
    );
    assert_eq!(parse_tail("all").unwrap(), -1);
    assert_eq!(parse_tail("0").unwrap(), 0);
    assert!(parse_tail("-1").is_err());
    assert_eq!(
        parse_proxy_ports("0:65535").unwrap(),
        ProxyPorts {
            local: 0,
            remote: 65535
        }
    );
    assert_eq!(parse_proxy_ports("80").unwrap().local, 0);
    for invalid in ["-1:80", "65536:80", "0", "1:0", "1:65536", "1:2:3"] {
        assert!(parse_proxy_ports(invalid).is_err(), "{invalid}");
    }
}

#[tokio::test]
async fn log_merger_orders_after_watermarks_and_surfaces_zero_errors_and_stalls() {
    let first_metadata = metadata("first");
    let first = vec![
        Ok(log(first_metadata.clone(), 2, b"two")),
        Ok(LogEntry::heartbeat(first_metadata, 4)),
    ];
    let second_metadata = metadata("second");
    let second = vec![
        Ok(log(second_metadata.clone(), 1, b"one")),
        Ok(log(second_metadata, 0, b"raw")),
    ];
    let mut output = merge_logs_with_options(
        vec![input("first", first), input("second", second)],
        CancellationToken::new(),
        Duration::from_secs(1),
        Duration::from_millis(10),
    );
    let mut entries = Vec::new();
    while let Some(entry) = output.recv().await {
        entries.push(entry.unwrap());
    }
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.message.clone())
            .collect::<Vec<_>>(),
        [b"one".to_vec(), b"raw".to_vec(), b"two".to_vec()]
    );
    assert_eq!(entries.first().unwrap().metadata, metadata("second"));

    let cancel = CancellationToken::new();
    let mut stalled = merge_logs_with_options(
        vec![
            LogInput {
                identity: "quiet".into(),
                stream: Box::pin(stream::pending()),
            },
            LogInput {
                identity: "buffered".into(),
                stream: Box::pin(stream::once(async {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    Ok(log(metadata("buffered"), 7, b"released"))
                })),
            },
        ],
        cancel.clone(),
        Duration::from_millis(15),
        Duration::from_millis(2),
    );
    assert!(stalled.recv().await.unwrap().unwrap_err().contains("quiet"));
    assert_eq!(stalled.recv().await.unwrap().unwrap().message, b"released");
    cancel.cancel();
}

#[tokio::test]
async fn log_merger_closes_empty_flushes_and_surfaces_stream_errors() {
    let mut empty = merge_logs(Vec::new(), CancellationToken::new());
    assert!(empty.recv().await.is_none());

    let mut errors = merge_logs(
        vec![input(
            "broken",
            vec![
                Err(LogError::Message("transport failed".into())),
                Ok(LogEntry::error(metadata("broken"), "remote failed")),
            ],
        )],
        CancellationToken::new(),
    );
    assert!(
        errors
            .recv()
            .await
            .unwrap()
            .unwrap_err()
            .contains("transport failed")
    );
    assert!(
        errors
            .recv()
            .await
            .unwrap()
            .unwrap_err()
            .contains("remote failed")
    );

    let mut closing = merge_logs(
        vec![
            input("buffered", vec![Ok(log(metadata("buffered"), 9, b"nine"))]),
            input("empty", vec![]),
        ],
        CancellationToken::new(),
    );
    assert_eq!(closing.recv().await.unwrap().unwrap().message, b"nine");
    assert!(closing.recv().await.is_none());
}

#[tokio::test]
async fn partial_open_streams_survive_until_parent_cancellation() {
    for boundary in ["Service", "Machine"] {
        let dropped = Arc::new(AtomicBool::new(false));
        let cancellation = CancellationToken::new();
        let mut inputs = Vec::new();
        open_log_input(&mut inputs, &cancellation, async {
            Ok(LogInput {
                identity: boundary.into(),
                stream: Box::pin(TrackedPending(dropped.clone())),
            })
        })
        .await
        .unwrap();
        assert!(
            open_log_input(&mut inputs, &cancellation, async {
                Err(crate::connect::TransportError::from(
                    tonic::Status::unavailable(format!("later {boundary} failed")),
                ))
            })
            .await
            .unwrap_err()
            .message()
            .contains(boundary)
        );
        assert!(inputs.is_empty());
        tokio::task::yield_now().await;
        assert!(!dropped.load(AtomicOrdering::SeqCst));
        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !dropped.load(AtomicOrdering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}

struct TrackedPending(Arc<AtomicBool>);

impl Stream for TrackedPending {
    type Item = Result<LogEntry, LogError>;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

impl Drop for TrackedPending {
    fn drop(&mut self) {
        self.0.store(true, AtomicOrdering::SeqCst);
    }
}

fn input(identity: &str, entries: Vec<Result<LogEntry, LogError>>) -> LogInput {
    LogInput {
        identity: identity.into(),
        stream: Box::pin(stream::iter(entries)),
    }
}

fn log(metadata: LogMetadata, timestamp: i64, message: &[u8]) -> LogEntry {
    LogEntry {
        metadata,
        stream: LogStream::Stdout,
        timestamp_unix_nanos: timestamp,
        message: message.to_vec(),
        error: None,
    }
}

fn metadata(name: &str) -> LogMetadata {
    LogMetadata {
        origin: LogOrigin::Service {
            service_id: ServiceId::parse("1".repeat(32)).unwrap(),
            service_name: ServiceName::parse(name).unwrap(),
            container_id: ContainerId::parse("f".repeat(64)).unwrap(),
            hook: None,
        },
        machine_id: MachineId::parse("a".repeat(32)).unwrap(),
        machine_name: MachineName::parse("machine").unwrap(),
    }
}

#[test]
fn machine_selection_treats_star_as_all_and_all_as_a_name() {
    let machines = [
        machine_observation(1, "edge"),
        machine_observation(2, "all"),
    ];
    assert_eq!(select_machines(&machines, &[]).unwrap().len(), 2);
    assert_eq!(select_machines(&machines, &["*".into()]).unwrap().len(), 2);
    assert_eq!(
        select_machines(&machines, &["all".into()])
            .unwrap()
            .first()
            .unwrap()
            .machine
            .name
            .as_str(),
        "all"
    );
    assert!(select_machines(&machines, &["missing".into()]).is_err());
}

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(ToOwned::to_owned).collect()
}

fn machine_observation(seed: u8, name: &str) -> MachineObservation {
    MachineObservation {
        machine: ployz_core::Machine {
            id: MachineId::parse(format!("{seed:032x}")).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
            management_address: ployz_core::ManagementAddress("fd00::1".parse().unwrap()),
            public_key: ployz_core::WireGuardPublicKey([seed; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: Default::default(),
        },
        membership: MembershipObservation::Up,
        selected_endpoint: None,
    }
}

fn observed_service() -> ServiceObservation {
    let service_id = ServiceId::parse("1".repeat(32)).unwrap();
    ServiceObservation {
        service_id,
        containers: vec![
            container(&"a".repeat(64), "api-one", service_id),
            container(&"b".repeat(64), "api-two", service_id),
            container(&format!("{}c", "b".repeat(63)), "b", service_id),
        ],
        hook_containers: vec![],
    }
}

fn container(id: &str, name: &str, service_id: ServiceId) -> ContainerObservation {
    ContainerObservation {
        container_id: ContainerId::parse(id).unwrap(),
        display_name: name.into(),
        created_at_unix_nanos: 0,
        machine_id: MachineId::parse("2".repeat(32)).unwrap(),
        service_id,
        service_name: ServiceName::parse("api").unwrap(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: ResolvedServiceSpec {
            service_id,
            name: ServiceName::parse("api").unwrap(),
            mode: ployz_core::ServiceMode::Replicated {
                replicas: std::num::NonZeroU32::new(1).unwrap(),
            },
            container: ployz_core::ServiceContainerSpec {
                image: "api".into(),
                command: vec![],
                entrypoint: vec![],
                environment: Default::default(),
                cap_add: vec![],
                cap_drop: vec![],
                healthcheck: None,
                pull_policy: ployz_core::PullPolicy::Missing,
                init: None,
                user: None,
                working_directory: None,
                tty: false,
                open_stdin: false,
                privileged: false,
                pid_mode: None,
                log_driver: None,
                resources: Default::default(),
                stop_timeout_secs: None,
                sysctls: Default::default(),
                config_mounts: vec![],
                restart: RestartPolicy::default(),
            },
            placement: Default::default(),
            ports: vec![],
            volumes: vec![],
            mounts: vec![],
            configs: vec![],
            pre_deploy: None,
            caddy_config: None,
            update: Default::default(),
        },
        address: None,
        labels: Default::default(),
    }
}
