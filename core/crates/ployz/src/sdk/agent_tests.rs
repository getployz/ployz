use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use ployz_core::metrics::{
    HostPoint, MarkerKind, MetricHistoryReply, MetricMarker, MetricPoints, ServicePoint,
};
use ployz_core::{
    ConfiguredHealthcheck, ContainerId, ContainerKind, CopyRotation, DeploymentLogId,
    HealthcheckCommand, HealthcheckSpec, MachineFailure, MachineSuccess, Namespace,
    QualifiedService, RpcError, RpcErrorCode, ServiceId, ServiceName,
};
use ployz_store::DeploymentId;
use ployz_store::tools::cloud::{ContainerState, LogsAnswer, MetricRow};
use ployz_store::tools::{
    LineCap, LogText, LogsQuery, SealedValues, ServerName, ServerRef, Timestamp, Withheld,
};
use tokio::time::Instant;

use super::{
    NANOS_PER_SECOND, Readable, ServiceCopy, Started, copies_of, logs, remove_one, start_one,
    stop_one, summarize,
};
use crate::global_fixtures::{container_id, machine, running_on};
use crate::sdk::test_support as support;

#[test]
fn a_services_copies_include_its_hook_containers_marked_as_hooks() {
    let server = machine('a', "alpha");
    let web = running_on(&server, '1');
    let mut hook = running_on(&server, '2');
    hook.try_update(|parts| parts.kind = ContainerKind::PreDeployHook)
        .unwrap();
    let names = BTreeMap::from([(server.id, &server.name)]);

    let copies = copies_of(
        [&web, &hook].into_iter(),
        &names,
        &Namespace::parse("app").unwrap(),
        &ServiceName::parse("api").unwrap(),
    );

    let copy = |hex, hook| ServiceCopy {
        server: server.name.clone(),
        machine: server.id,
        container: container_id(hex),
        state: ContainerState::Running,
        hook,
    };
    assert_eq!(copies, [copy('1', false), copy('2', true)]);
}

const SECOND: i64 = NANOS_PER_SECOND;

fn marked(
    machine: ployz_core::MachineId,
    at: i64,
    deployment: &str,
    kind: MarkerKind,
) -> MetricMarker {
    marked_in("prod/web", machine, at, deployment, kind)
}

fn marked_in(
    service: &str,
    machine: ployz_core::MachineId,
    at: i64,
    deployment: &str,
    kind: MarkerKind,
) -> MetricMarker {
    MetricMarker {
        machine_id: machine,
        container_id: ContainerId::parse("b".repeat(64)).unwrap(),
        service_id: ServiceId::parse("c".repeat(32)).unwrap(),
        service: QualifiedService::parse(service).unwrap(),
        deployment_id: Some(deployment.to_owned()),
        replica: "web-1".into(),
        ts: at * SECOND,
        kind,
    }
}

#[test]
fn a_metrics_answer_keeps_one_deployments_deaths_and_names_the_silent_server() {
    let one = support::machine('a', "one");
    let two = support::machine('b', "two");
    let ours = "00000000-0000-4000-8000-000000000101";
    let older = "00000000-0000-4000-8000-000000000100";
    let points = vec![ServicePoint {
        ts: 1_000 * SECOND,
        cpu_usec: Some(250_000.0),
        mem_bytes: Some(500_000_000),
        mem_limit: Some(512_000_000),
        rx: None,
        tx: None,
        io_r: None,
        io_w: None,
    }];
    let history = crate::sdk::MetricHistory {
        step_nanos: Some(10 * SECOND),
        points: MetricPoints::Service(points.clone()),
        machines: vec![MachineSuccess {
            machine_id: one.machine.id,
            value: MetricHistoryReply {
                points: MetricPoints::Service(points),
                step_nanos: 10 * SECOND,
                gaps: Vec::new(),
                markers: vec![
                    marked(one.machine.id, 990, ours, MarkerKind::Oom),
                    marked(
                        one.machine.id,
                        991,
                        ours,
                        MarkerKind::Exit {
                            exit_code: Some(137),
                            oom: false,
                            stopped: false,
                        },
                    ),
                    marked(one.machine.id, 995, older, MarkerKind::Restart),
                ],
            },
        }],
        unanswered: vec![MachineFailure {
            machine_id: two.machine.id,
            error: RpcError {
                code: RpcErrorCode::Unsupported,
                message: "unknown operation metric_history".into(),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            },
        }],
    };
    let ours = DeploymentId::parse(ours).unwrap();

    let answer = summarize(
        &history,
        &[one.clone(), two],
        Some(&ours),
        &Readable::All,
        (Timestamp(900), 1_000 * SECOND),
    );

    assert_eq!((answer.oom_kills, answer.exits, answer.restarts), (1, 1, 0));
    let kinds = answer
        .events
        .iter()
        .map(|event| {
            (
                event.at,
                event.server.as_str(),
                event.kind,
                event.deployment.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            (
                Timestamp(991),
                "one",
                MarkerKind::Exit {
                    exit_code: Some(137),
                    oom: false,
                    stopped: false,
                },
                Some(&ours)
            ),
            (Timestamp(990), "one", MarkerKind::Oom, Some(&ours)),
        ]
    );
    assert_eq!(
        answer.notes,
        ["two did not answer: unknown operation metric_history"]
    );
    assert_eq!(answer.seen.unanswered, [ServerName::parse("two").unwrap()]);
    assert_eq!(
        answer.metrics.get(1),
        Some(&MetricRow {
            metric: "Memory".into(),
            now: Some("500 MB / 512 MB".into()),
            avg: Some("500 MB".into()),
            peak: Some("500 MB".into()),
        })
    );
    assert_eq!(
        answer.metrics.get(2),
        Some(&MetricRow {
            metric: "Net in".into(),
            now: None,
            avg: None,
            peak: None,
        }),
        "a metric with no reading is absent, never zero"
    );
}

// Catches a Server answer that lists, counts or filters to Service events outside the
// Namespaces its caller may read.
#[test]
fn a_server_answer_describes_only_the_readable_namespaces_and_keeps_host_totals() {
    let one = support::machine('a', "one");
    let staging = "00000000-0000-4000-8000-000000000201";
    let production = "00000000-0000-4000-8000-000000000202";
    let host = HostPoint {
        ts: 1_000 * SECOND,
        cpu_busy: Some(0.25),
        memory: None,
        interfaces: BTreeMap::new(),
        disk: None,
    };
    let markers = vec![
        marked_in(
            "shop-stage/web",
            one.machine.id,
            990,
            staging,
            MarkerKind::Restart,
        ),
        marked_in(
            "shop-prod/web",
            one.machine.id,
            991,
            production,
            MarkerKind::Oom,
        ),
        marked_in(
            "shop-prod/web",
            one.machine.id,
            992,
            production,
            MarkerKind::Exit {
                exit_code: Some(137),
                oom: true,
                stopped: false,
            },
        ),
    ];
    let history = crate::sdk::MetricHistory {
        step_nanos: Some(10 * SECOND),
        points: MetricPoints::Machine(vec![host.clone()]),
        machines: vec![MachineSuccess {
            machine_id: one.machine.id,
            value: MetricHistoryReply {
                points: MetricPoints::Machine(vec![host]),
                step_nanos: 10 * SECOND,
                gaps: Vec::new(),
                markers,
            },
        }],
        unanswered: Vec::new(),
    };
    let staging_only = Readable::Only([Namespace::parse("shop-stage").unwrap()].into());
    let summary = |deployment: Option<&DeploymentId>, readable: &Readable| {
        let answer = summarize(
            &history,
            std::slice::from_ref(&one),
            deployment,
            readable,
            (Timestamp(900), 1_000 * SECOND),
        );
        let namespaces = answer
            .events
            .iter()
            .map(|event| event.namespace.to_string())
            .collect::<Vec<_>>();
        (
            (answer.oom_kills, answer.exits, answer.restarts),
            namespaces,
            answer.metrics.first().and_then(|row| row.now.clone()),
        )
    };

    assert_eq!(
        summary(None, &staging_only),
        (
            (0, 0, 1),
            vec!["shop-stage".to_owned()],
            Some("25%".to_owned())
        )
    );
    let production = DeploymentId::parse(production).unwrap();
    assert_eq!(
        summary(Some(&production), &staging_only),
        ((0, 0, 0), Vec::new(), Some("25%".to_owned())),
        "a production Deployment filter finds nothing outside the ceiling"
    );
    assert_eq!(
        summary(None, &Readable::All),
        (
            (1, 1, 1),
            vec![
                "shop-prod".to_owned(),
                "shop-prod".to_owned(),
                "shop-stage".to_owned()
            ],
            Some("25%".to_owned())
        )
    );
}

/// Run one act against Container `1` on Machine `a`, served by `service`.
async fn acted<T, F: std::future::Future<Output = Result<T, RpcError>>>(
    service: support::DeployService,
    act: impl FnOnce(crate::sdk::Session, ployz_core::MachineId) -> F,
) -> Result<T, RpcError> {
    let machine = support::machine('a', "one").machine.id;
    let (address, server) = support::listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![crate::context::Connection::tcp(address)],
        Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap();
    let acted = act(session.clone(), machine).await;
    session.close().await;
    server.abort();
    acted
}

async fn started(service: support::DeployService) -> Started {
    acted(service, |session, machine| async move {
        start_one(&session, machine, container_id('1')).await
    })
    .await
    .unwrap()
}

fn run() -> DeploymentLogId {
    DeploymentLogId::parse("stopped-by-you").unwrap()
}

#[tokio::test]
async fn a_start_is_serving_once_its_health_wait_passes() {
    let service = support::DeployService::new(support::machine('a', "one"));
    let rotations = service.rotations();
    assert_eq!(started(service).await, Started::Serving);
    assert_eq!(
        rotations
            .lock()
            .unwrap()
            .get(&container_id('1'))
            .map(|rotated| &rotated.rotation),
        Some(&CopyRotation::In)
    );
}

#[tokio::test]
async fn a_start_of_a_held_copy_brings_it_back_in() {
    let service = support::DeployService::new(support::machine('a', "one"))
        .with_rotation(container_id('1'), CopyRotation::Held(run()));
    let rotations = service.rotations();
    assert_eq!(started(service).await, Started::Serving);
    assert_eq!(
        rotations
            .lock()
            .unwrap()
            .get(&container_id('1'))
            .map(|rotated| &rotated.rotation),
        Some(&CopyRotation::In)
    );
}

#[tokio::test]
async fn a_start_whose_copy_cannot_be_read_starts_nothing() {
    let service =
        support::DeployService::new(support::machine('a', "one")).fail_inspects("inspect failed");
    let mutations = service.mutating_rpcs();
    let error = acted(service, |session, machine| async move {
        start_one(&session, machine, container_id('1')).await
    })
    .await
    .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unavailable, "{error:?}");
    assert_eq!(mutations.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_start_of_an_out_copy_is_refused_and_says_to_deploy_again() {
    let service = support::DeployService::new(support::machine('a', "one"))
        .with_rotation(container_id('1'), CopyRotation::Out);
    let mutations = service.mutating_rpcs();
    let error = acted(service, |session, machine| async move {
        start_one(&session, machine, container_id('1')).await
    })
    .await
    .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
    assert!(
        error.message.contains("Deploy the Service again"),
        "{error:?}"
    );
    assert_eq!(mutations.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_stop_of_an_in_copy_holds_it_so_a_start_can_bring_it_back() {
    let service = support::DeployService::new(support::machine('a', "one"))
        .with_rotation(container_id('1'), CopyRotation::In);
    let rotations = service.rotations();
    let removed = service.removed_containers();
    acted(service, |session, machine| async move {
        stop_one(&session, machine, container_id('1')).await
    })
    .await
    .unwrap();
    assert!(matches!(
        rotations
            .lock()
            .unwrap()
            .get(&container_id('1'))
            .map(|rotated| &rotated.rotation),
        Some(CopyRotation::Held(_))
    ));
    assert!(removed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_removal_of_a_stopped_in_copy_takes_it_out_then_removes_it() {
    let service = support::DeployService::new(support::machine('a', "one"))
        .crash_containers()
        .with_rotation(container_id('1'), CopyRotation::In);
    let rotations = service.rotations();
    let removed = service.removed_containers();
    acted(service, |session, machine| async move {
        remove_one(&session, machine, container_id('1')).await
    })
    .await
    .unwrap();
    assert_eq!(
        rotations
            .lock()
            .unwrap()
            .get(&container_id('1'))
            .map(|rotated| &rotated.rotation),
        Some(&CopyRotation::Out)
    );
    assert!(removed.lock().unwrap().contains(&container_id('1')));
}

// Real Machines are on the network, so these wait in real time with short healthchecks.
// The shared wait's own tests cover deadlines past the Serving wait in paused time.
#[tokio::test]
async fn a_start_returns_only_once_its_healthcheck_passes() {
    let service = support::DeployService::new(support::machine('a', "one"))
        .turn_healthy_after(Duration::from_millis(1_500));
    let begun = Instant::now();
    assert_eq!(started(service).await, Started::Serving);
    assert!(begun.elapsed() >= Duration::from_millis(1_500));
}

#[tokio::test]
async fn a_start_that_never_passes_its_healthcheck_is_unhealthy_at_its_deadline() {
    let service = support::DeployService::new(support::machine('a', "one"))
        .hold_health()
        .with_effective_healthcheck(HealthcheckSpec::Configured(ConfiguredHealthcheck {
            test: HealthcheckCommand::parse(["CMD", "true"]).unwrap(),
            interval_millis: None,
            timeout_millis: None,
            start_period_millis: None,
            start_interval_millis: None,
            retries: None,
            deadline_millis: Some(1_000),
        }));
    let begun = Instant::now();
    assert_eq!(started(service).await, Started::Unhealthy);
    let waited = begun.elapsed();
    assert!(
        (Duration::from_secs(1)..Duration::from_secs(5)).contains(&waited),
        "{waited:?}"
    );
}

fn logs_query(server: Option<ServerRef>, sealed: Option<&[&str]>) -> LogsAsk {
    let query = LogsQuery {
        namespace: Namespace::parse("app").unwrap(),
        service: ServiceName::parse("web").unwrap(),
        deployment: None,
        server,
        lines: LineCap::new(3).unwrap(),
        since: Some(Duration::from_secs(600)),
    };
    let sealed = sealed.map(|names| names.iter().map(|name| (*name).to_owned()).collect());
    (query, sealed)
}

type LogsAsk = (LogsQuery, Option<BTreeSet<String>>);

fn shown(answer: &LogsAnswer) -> Vec<(String, Vec<String>)> {
    answer
        .servers
        .iter()
        .map(|log| {
            let LogText::Shown(excerpt) = &log.text else {
                panic!("{:?} withheld its tail: {:?}", log.server, log.text);
            };
            (log.server.to_string(), excerpt.lines().to_vec())
        })
        .collect()
}

async fn ask_logs(service: support::DeployService, queries: Vec<LogsAsk>) -> Vec<LogsAnswer> {
    let (address, server) = support::listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![crate::context::Connection::tcp(address)],
        Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap();
    let mut answers = Vec::new();
    for (query, sealed) in queries {
        answers.push(logs(&session, query, sealed.as_ref()).await.unwrap());
    }
    session.close().await;
    server.abort();
    answers
}

// Catches a logs ask that leaks a Sealed value, scrubs a value that is not Sealed, caps
// lines per Container rather than per Server, ignores `lines`, `since` or the Service, or
// reads Servers the ask didn't name.
#[tokio::test]
async fn a_logs_ask_returns_each_servers_scrubbed_tail_of_the_service() {
    let one = support::machine('a', "one");
    let two = support::machine('b', "two");
    let service = support::DeployService::new(one.clone()).with_machines(vec![one.clone(), two]);
    let requests = Arc::clone(&service.history_requests);
    let sealed: Option<&[&str]> = Some(&["TOKEN"]);
    let only = ServerRef {
        machine: one.machine.id,
        name: one.machine.name.clone(),
    };

    let [every, only] = ask_logs(
        service,
        vec![logs_query(None, sealed), logs_query(Some(only), sealed)],
    )
    .await
    .try_into()
    .unwrap();

    let tail = vec![
        "line 10".to_owned(),
        "plain from-docker".to_owned(),
        "token=[secret TOKEN]".to_owned(),
    ];
    assert_eq!(
        shown(&every),
        [
            ("one".to_owned(), tail.clone()),
            ("two".to_owned(), tail.clone())
        ]
    );
    assert!(every.seen.unanswered.is_empty());
    assert!(every.notes.is_empty());
    assert_eq!(shown(&only), [("one".to_owned(), tail)]);

    let requests = requests.lock().unwrap();
    assert!(!requests.is_empty());
    for request in requests.iter() {
        assert_eq!(request.namespace.as_deref(), Some("app"));
        assert_eq!(request.service.as_deref(), Some("web"));
        assert_eq!(request.container_id, None);
        let since = request.since_nanos.expect("since reaches the Log Store");
        let ago = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            - u128::try_from(since).unwrap();
        assert!(
            (Duration::from_secs(600).as_nanos()..Duration::from_secs(660).as_nanos())
                .contains(&ago)
        );
    }
}

// Catches a logs ask that scrubs env values when the Store names nothing Sealed, drops the
// secret-shaped pass, or shows any env value when the Store could not say what is Sealed.
#[tokio::test]
async fn a_logs_ask_scrubs_only_sealed_values_unless_the_store_could_not_say() {
    let one = support::machine('a', "one");
    let service = support::DeployService::new(one.clone());

    let [nothing_sealed, unknown] = ask_logs(
        service,
        vec![logs_query(None, Some(&[])), logs_query(None, None)],
    )
    .await
    .try_into()
    .unwrap();

    let tail = |plain: &str, token: &str| {
        vec![(
            "one".to_owned(),
            vec!["line 10".to_owned(), plain.to_owned(), token.to_owned()],
        )]
    };
    assert_eq!(
        shown(&nothing_sealed),
        tail("plain from-docker", "token=[redacted]")
    );
    assert_eq!(
        shown(&unknown),
        tail("plain [secret PLAIN]", "token=[secret TOKEN]")
    );
}

#[tokio::test]
async fn a_logs_ask_scrubs_every_name_the_store_holds_sealed_whatever_the_ask_says() {
    let one = support::machine('a', "one");
    let service = support::DeployService::new(one.clone());
    let decoded: Result<LogsQuery, _> = serde_json::from_value(serde_json::json!({
        "namespace": "app",
        "service": "web",
        "lines": 3,
        "sealed": [],
    }));
    assert!(decoded.is_err(), "an ask cannot carry its own scrub set");

    let [answer] = ask_logs(service, vec![logs_query(None, Some(&["PLAIN", "TOKEN"]))])
        .await
        .try_into()
        .unwrap();

    assert_eq!(
        shown(&answer),
        [(
            "one".to_owned(),
            vec![
                "line 10".to_owned(),
                "plain [secret PLAIN]".to_owned(),
                "token=[secret TOKEN]".to_owned(),
            ]
        )]
    );
}

// Catches a direct apply's failed tail scrubbing env values it has no Sealed reason to,
// or showing secret-shaped text.
#[tokio::test]
async fn a_failed_tail_without_sealed_values_shows_env_values_and_redacts_secret_shaped_text() {
    let one = support::machine('a', "one");
    let (address, server) = support::listening(support::DeployService::new(one.clone())).await;
    let session = crate::sdk::connect_connections(
        vec![crate::context::Connection::tcp(address)],
        Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap();

    let log = crate::sdk::failed_tail(
        &session.client().unwrap(),
        one.machine.id,
        container_id('c'),
        &SealedValues::none(),
    )
    .await;
    session.close().await;
    server.abort();

    let LogText::Shown(excerpt) = log else {
        panic!("a read tail is shown: {log:?}");
    };
    assert!(
        excerpt.lines().ends_with(&[
            "plain from-docker".to_owned(),
            "token=[redacted]".to_owned()
        ]),
        "{:?}",
        excerpt.lines()
    );
}

// Catches a logs ask that drops why a Server's Log Store did not answer, or that lets the
// failure hide the other Servers' lines.
#[tokio::test]
async fn a_server_whose_log_store_fails_is_unanswered_with_its_reason() {
    let one = support::machine('a', "one");
    let two = support::machine('b', "two");
    let mut service =
        support::DeployService::new(one.clone()).with_machines(vec![one, two.clone()]);
    service
        .history_failures
        .insert(two.machine.id, "log store is rebuilding".to_owned());

    let [answer] = ask_logs(service, vec![logs_query(None, Some(&["TOKEN"]))])
        .await
        .try_into()
        .unwrap();

    assert_eq!(
        shown(&answer)
            .into_iter()
            .map(|(server, _)| server)
            .collect::<Vec<_>>(),
        ["one"]
    );
    assert_eq!(answer.seen.unanswered, [two.machine.name]);
    assert_eq!(
        answer.notes,
        ["two did not answer: log store is rebuilding"]
    );
}

// Catches a logs ask where a Container its Machine can no longer inspect, such as the one
// a redeploy removed, withholds the lines of every other Container on that Server, or
// shows its own lines unscrubbed.
#[tokio::test]
async fn a_removed_containers_lines_are_left_out_and_the_rest_still_shown() {
    let one = support::machine('a', "one");
    let mut service = support::DeployService::new(one.clone());
    service
        .uninspectable
        .insert(ContainerId::parse("d".repeat(64)).unwrap());

    let [answer] = ask_logs(service, vec![logs_query(None, Some(&["TOKEN"]))])
        .await
        .try_into()
        .unwrap();

    assert_eq!(
        shown(&answer),
        [(
            "one".to_owned(),
            vec![
                "line 8".to_owned(),
                "line 10".to_owned(),
                "token=[secret TOKEN]".to_owned(),
            ]
        )]
    );
}

// Catches a logs ask that shows lines when no Container's env could be read to scrub its
// Sealed values, or withholds them from a Service with none.
#[tokio::test]
async fn a_server_whose_containers_cannot_be_inspected_withholds_only_sealed_lines() {
    let one = support::machine('a', "one");
    let mut service = support::DeployService::new(one.clone());
    service
        .uninspectable
        .extend(["c", "d"].map(|id| ContainerId::parse(id.repeat(64)).unwrap()));

    let [sealed, nothing_sealed] = ask_logs(
        service,
        vec![
            logs_query(None, Some(&["TOKEN"])),
            logs_query(None, Some(&[])),
        ],
    )
    .await
    .try_into()
    .unwrap();

    assert_eq!(
        sealed
            .servers
            .iter()
            .map(|log| (log.server.to_string(), log.text.clone()))
            .collect::<Vec<_>>(),
        [(
            "one".to_owned(),
            LogText::Withheld {
                why: Withheld::NoSecretContext
            }
        )]
    );
    assert_eq!(shown(&nothing_sealed).len(), 1);
}
