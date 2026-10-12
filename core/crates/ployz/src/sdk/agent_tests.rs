use std::{collections::BTreeMap, sync::Arc, time::Duration};

use ployz_core::metrics::{
    MarkerKind, MetricHistoryReply, MetricMarker, MetricPoints, ServicePoint,
};
use ployz_core::{
    ConfiguredHealthcheck, ContainerId, ContainerKind, HealthcheckCommand, HealthcheckSpec,
    MachineFailure, MachineSuccess, Namespace, QualifiedService, RpcError, RpcErrorCode, ServiceId,
    ServiceName,
};
use ployz_store::tools::cloud::{ContainerState, LogsAnswer, MetricRow};
use ployz_store::tools::{LineCap, LogText, LogsQuery, ServerName, ServerRef, Timestamp, Withheld};
use ployz_store::{DeploymentId, EnvironmentId};
use tokio::time::Instant;

use super::{NANOS_PER_SECOND, ServiceCopy, Started, copies_of, logs, start_one, summarize};
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
    MetricMarker {
        machine_id: machine,
        container_id: ContainerId::parse("b".repeat(64)).unwrap(),
        service_id: ServiceId::parse("c".repeat(32)).unwrap(),
        service: QualifiedService::parse("prod/web").unwrap(),
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
                    exit_code: Some(137)
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

async fn started(service: support::DeployService) -> Started {
    let machine = support::machine('a', "one").machine.id;
    let (address, server) = support::listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![crate::context::Connection::tcp(address)],
        Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap();
    let started = start_one(&session, machine, container_id('1'))
        .await
        .unwrap();
    session.close().await;
    server.abort();
    started
}

#[tokio::test]
async fn a_start_is_serving_once_the_machine_sees_it_serve() {
    let service =
        support::DeployService::new(support::machine('a', "one")).with_observation_barrier();
    assert_eq!(started(service).await, Started::Serving);
}

#[tokio::test(start_paused = true)]
async fn a_start_the_machine_never_sees_serve_is_unconfirmed() {
    let service = support::DeployService::new(support::machine('a', "one"))
        .with_observation_barrier()
        .with_dropped_observations();
    assert_eq!(started(service).await, Started::Unconfirmed);
}

// Real Machines are on the network, so these wait in real time with short healthchecks.
// The shared wait's own tests cover deadlines past the Serving wait in paused time.
#[tokio::test]
async fn a_start_returns_only_once_its_healthcheck_passes() {
    let service = support::DeployService::new(support::machine('a', "one"))
        .with_observation_barrier()
        .turn_healthy_after(Duration::from_millis(1_500));
    let begun = Instant::now();
    assert_eq!(started(service).await, Started::Serving);
    assert!(begun.elapsed() >= Duration::from_millis(1_500));
}

#[tokio::test]
async fn a_start_that_never_passes_its_healthcheck_is_unhealthy_at_its_deadline() {
    let service = support::DeployService::new(support::machine('a', "one"))
        .with_observation_barrier()
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

fn logs_query(server: Option<ServerRef>, plain: &[(&str, &str)]) -> LogsQuery {
    LogsQuery {
        environment: EnvironmentId::parse("cccccccc-cccc-4ccc-8ccc-cccccccccccc").unwrap(),
        namespace: Namespace::parse("app").unwrap(),
        service: ServiceName::parse("web").unwrap(),
        deployment: None,
        server,
        lines: LineCap::new(3).unwrap(),
        since: Some(Duration::from_secs(600)),
        plain: plain
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
    }
}

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

async fn ask_logs(service: support::DeployService, queries: Vec<LogsQuery>) -> Vec<LogsAnswer> {
    let (address, server) = support::listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![crate::context::Connection::tcp(address)],
        Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap();
    let mut answers = Vec::new();
    for query in queries {
        answers.push(logs(&session, query).await.unwrap());
    }
    session.close().await;
    server.abort();
    answers
}

// Catches a logs ask that leaks a Container's env value, scrubs one the Store shows as
// plain, caps lines per Container rather than per Server, ignores `lines`, `since` or the
// Service, or reads Servers the ask didn't name.
#[tokio::test]
async fn a_logs_ask_returns_each_servers_scrubbed_tail_of_the_service() {
    let one = support::machine('a', "one");
    let two = support::machine('b', "two");
    let service = support::DeployService::new(one.clone()).with_machines(vec![one.clone(), two]);
    let requests = Arc::clone(&service.history_requests);
    let plain = [("PLAIN", "from-docker")];
    let only = ServerRef {
        machine: one.machine.id,
        name: one.machine.name.clone(),
    };

    let [every, only] = ask_logs(
        service,
        vec![logs_query(None, &plain), logs_query(Some(only), &plain)],
    )
    .await
    .try_into()
    .unwrap();

    let tail = vec![
        "line 10".to_owned(),
        "plain from-docker".to_owned(),
        "token [secret TOKEN]".to_owned(),
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

// Catches a logs ask that shows an env value the Store does not show as plain with that
// same value.
#[tokio::test]
async fn a_logs_ask_scrubs_env_values_the_store_does_not_show_as_plain() {
    let one = support::machine('a', "one");
    let service = support::DeployService::new(one.clone());

    let answers = ask_logs(
        service,
        vec![
            logs_query(None, &[]),
            logs_query(None, &[("PLAIN", "staged-value")]),
        ],
    )
    .await;

    for answer in &answers {
        assert_eq!(
            shown(answer),
            [(
                "one".to_owned(),
                vec![
                    "line 10".to_owned(),
                    "plain [secret PLAIN]".to_owned(),
                    "token [secret TOKEN]".to_owned(),
                ]
            )]
        );
    }
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

    let [answer] = ask_logs(service, vec![logs_query(None, &[("PLAIN", "from-docker")])])
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

    let [answer] = ask_logs(service, vec![logs_query(None, &[("PLAIN", "from-docker")])])
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
                "token [secret TOKEN]".to_owned(),
            ]
        )]
    );
}

// Catches a logs ask that shows lines when no Container's env could be read to scrub them.
#[tokio::test]
async fn a_server_whose_containers_cannot_be_inspected_withholds_its_lines() {
    let one = support::machine('a', "one");
    let mut service = support::DeployService::new(one.clone());
    service
        .uninspectable
        .extend(["c", "d"].map(|id| ContainerId::parse(id.repeat(64)).unwrap()));

    let [answer] = ask_logs(service, vec![logs_query(None, &[("PLAIN", "from-docker")])])
        .await
        .try_into()
        .unwrap();

    assert_eq!(
        answer
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
}
