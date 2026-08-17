use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
    task::{Context, Poll},
};

use futures_util::stream;
use ployz_core::{
    ContainerKind, ContainerObservation, ContainerRef, ContainerRuntimeObservation,
    ContainerSelector, FanoutSelector, HealthObservation, HookContainer, LogMetadata, LogOrigin,
    MachineId, MachineName, MembershipObservation, ResolvedServiceSpec, RestartPolicy,
    ServiceContainer, ServiceId, ServiceName, ServiceSelector,
};

use super::*;

#[test]
fn exec_mapping_and_container_selection_match_the_operator_contract() {
    let service = observed_service();
    assert_eq!(
        select_exec_container(&service, None)
            .unwrap()
            .as_observation()
            .display_name,
        "api-one"
    );
    assert_eq!(
        select_exec_container(&service, Some(&container_selector(&"b".repeat(64))))
            .unwrap()
            .as_observation()
            .display_name,
        "api-two"
    );
    assert_eq!(
        select_exec_container(&service, Some(&container_selector("b")))
            .unwrap()
            .as_observation()
            .display_name,
        "b"
    );
    assert!(matches!(
        select_exec_container(&service, Some(&container_selector("bb"))),
        Err(OperatorError::Container(ployz_core::ContainerSelectorError::Ambiguous { selector, .. }))
            if selector.as_str() == "bb"
    ));
    assert_eq!(
        select_log_containers(&service, &[container_selector("b")])
            .unwrap()
            .first()
            .unwrap()
            .as_observation()
            .display_name,
        "b"
    );
    let hook_id = service
        .hook_containers
        .first()
        .unwrap()
        .as_observation()
        .container_id
        .to_string();
    let hook_selector = container_selector(&hook_id);
    assert!(matches!(
        select_exec_container(&service, Some(&hook_selector)),
        Err(OperatorError::Container(
            ployz_core::ContainerSelectorError::NotFound { .. }
        ))
    ));
    assert!(matches!(
        select_exec_container(&service, Some(&container_selector("api-hook"))),
        Err(OperatorError::Container(ployz_core::ContainerSelectorError::NotFound { selector }))
            if selector.as_str() == "api-hook"
    ));
    assert_eq!(
        select_proxy_container(&service)
            .unwrap()
            .as_observation()
            .display_name,
        "api-one"
    );
    assert_eq!(select_log_containers(&service, &[]).unwrap().len(), 4);
    assert!(matches!(
        select_log_containers(&service, &[hook_selector])
            .unwrap()
            .as_slice(),
        [ContainerRef::Hook(_)]
    ));
    let hook_only = ServiceObservation {
        service_id: service.service_id,
        containers: vec![],
        hook_containers: service.hook_containers.clone(),
    };
    assert!(matches!(
        select_exec_container(&hook_only, None),
        Err(OperatorError::NoRegularContainer)
    ));
    assert!(matches!(
        select_proxy_container(&hook_only),
        Err(OperatorError::NoHealthyContainer)
    ));
    let mut duplicate_names = service.clone();
    if let Some(slot) = duplicate_names.containers.get_mut(1) {
        let mut observation = slot.clone().into_observation();
        observation.display_name = "api-one".into();
        *slot = ServiceContainer::try_from(observation).unwrap();
    }
    assert!(matches!(
        select_exec_container(&duplicate_names, Some(&container_selector("api-one"))),
        Err(OperatorError::Container(ployz_core::ContainerSelectorError::Ambiguous { selector, .. }))
            if selector.as_str() == "api-one"
    ));
}

#[test]
fn exec_mode_resolves_cli_flags_without_detach_plus_tty() {
    assert_eq!(
        ExecMode::resolve(true, false, true, true).unwrap(),
        ExecMode::Detached
    );
    assert_eq!(
        ExecMode::resolve(true, true, true, true).unwrap(),
        ExecMode::Detached
    );
    let detached = exec_options(vec!["true".into()], ExecMode::Detached);
    assert!(!detached.tty);
    assert!(!detached.attach_stdin);
    assert!(!detached.attach_stdout);
    assert!(!detached.attach_stderr);
    assert!(detached.detach);

    assert!(matches!(
        ExecMode::resolve(false, false, true, false),
        Err(OperatorError::TtyRequiresStdin)
    ));

    let attached = exec_options(vec!["true".into()], ExecMode::Attached { tty: false });
    assert!(!attached.tty);
    assert!(attached.attach_stdin && attached.attach_stdout && attached.attach_stderr);
    assert!(!attached.detach);

    let tty = exec_options(Vec::new(), ExecMode::Attached { tty: true });
    assert_eq!(
        tty.command,
        DEFAULT_EXEC_COMMAND
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    assert!(tty.tty && tty.attach_stdin && tty.attach_stdout && tty.attach_stderr);
    assert!(!tty.detach);

    assert_eq!(
        ExecMode::resolve(false, true, true, true).unwrap(),
        ExecMode::Attached { tty: false }
    );
    assert_eq!(
        ExecMode::resolve(false, false, false, false).unwrap(),
        ExecMode::Attached { tty: false }
    );
    assert_eq!(
        ExecMode::resolve(false, false, true, true).unwrap(),
        ExecMode::Attached { tty: true }
    );
}

#[test]
fn service_args_tail_and_proxy_ports_cover_the_argument_tables() {
    assert!(service_logs_use_compose(&[]));
    assert!(!service_logs_use_compose(&["api".into()]));
    assert_eq!(
        parse_service_args(&strings(["api/one", "api/two", "worker/x", "api"])).unwrap(),
        [
            ServiceArg {
                service: service_selector("api"),
                containers: vec![],
            },
            ServiceArg {
                service: service_selector("worker"),
                containers: vec![container_selector("x")],
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

    let mut stalled_then_closed = merge_logs_with_options(
        vec![
            LogInput {
                identity: "quiet".into(),
                stream: Box::pin(stream::unfold((), |()| async {
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    None::<(Result<LogEntry, LogError>, ())>
                })),
            },
            input("buffered", vec![Ok(log(metadata("buffered"), 9, b"nine"))]),
        ],
        CancellationToken::new(),
        Duration::from_millis(15),
        Duration::from_millis(2),
    );
    assert!(
        stalled_then_closed
            .recv()
            .await
            .unwrap()
            .unwrap_err()
            .contains("quiet")
    );
    assert_eq!(
        stalled_then_closed.recv().await.unwrap().unwrap().message,
        b"nine"
    );
    assert!(stalled_then_closed.recv().await.is_none());
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
    assert_eq!(
        select_machines(&machines, &[FanoutSelector::parse("*").unwrap()])
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        select_machines(&machines, &[FanoutSelector::parse("all").unwrap()])
            .unwrap()
            .first()
            .unwrap()
            .machine
            .name
            .as_str(),
        "all"
    );
    assert!(select_machines(&machines, &[FanoutSelector::parse("missing").unwrap()]).is_err());
    let mut down = machine_observation(3, "down");
    down.membership = MembershipObservation::Down;
    let mut unknown = machine_observation(4, "unknown");
    unknown.membership = MembershipObservation::Unknown;
    let mixed = [
        machine_observation(1, "edge"),
        machine_observation(2, "all"),
        down,
        unknown,
    ];
    assert_eq!(select_machines(&mixed, &[]).unwrap().len(), 2);
    assert!(select_machines(&mixed, &[FanoutSelector::parse("down").unwrap()]).is_err());
}

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(ToOwned::to_owned).collect()
}

fn service_selector(value: &str) -> ServiceSelector {
    ServiceSelector::parse(value).unwrap()
}

fn container_selector(value: &str) -> ContainerSelector {
    ContainerSelector::parse(value).unwrap()
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
            service_container(&"a".repeat(64), "api-one", service_id),
            service_container(&"b".repeat(64), "api-two", service_id),
            service_container(&format!("{}c", "b".repeat(63)), "b", service_id),
        ],
        hook_containers: vec![hook_container(&"c".repeat(64), "api-hook", service_id)],
    }
}

fn service_container(id: &str, name: &str, service_id: ServiceId) -> ServiceContainer {
    ServiceContainer::try_from(container(
        id,
        name,
        service_id,
        ContainerKind::ServiceContainer,
    ))
    .unwrap()
}

fn hook_container(id: &str, name: &str, service_id: ServiceId) -> HookContainer {
    HookContainer::try_from(container(
        id,
        name,
        service_id,
        ContainerKind::PreDeployHook,
    ))
    .unwrap()
}

fn container(
    id: &str,
    name: &str,
    service_id: ServiceId,
    kind: ContainerKind,
) -> ContainerObservation {
    ContainerObservation {
        container_id: ContainerId::parse(id).unwrap(),
        display_name: name.into(),
        created_at_unix_nanos: 0,
        machine_id: MachineId::parse("2".repeat(32)).unwrap(),
        service_id,
        service_name: ServiceName::parse("api").unwrap(),
        kind,
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
                restart: RestartPolicy::default(),
            },
            placement: Default::default(),
            ports: vec![],
            volume_graph: Default::default(),
            config_graph: Default::default(),
            pre_deploy: None,
            caddy_config: None,
            update: Default::default(),
        },
        address: None,
        labels: Default::default(),
    }
}
