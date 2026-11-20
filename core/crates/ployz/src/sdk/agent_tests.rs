use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use ployz_core::{
    ConfiguredHealthcheck, ContainerId, ContainerKind, ContainerRuntimeObservation, CopyRotation,
    DeploymentLogId, HealthcheckCommand, HealthcheckSpec,
    Namespace, RpcError, RpcErrorCode, ServiceName,
};
use ployz_store::tools::cloud::{ContainerState, LogsAnswer};
use ployz_store::tools::{
    CloudQuery, LineCap, LogText, LogsQuery, ServerRef, Withheld,
};

use super::{
    Readable, ServiceCopy, Started, ask, copies_of, logs, remove_one, report, start_one,
    stop_one,
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

#[test]
fn a_copy_its_server_stopped_reads_stopped_and_one_that_exited_alone_reads_exited() {
    let server = machine('a', "alpha");
    let exited = |hex| {
        let mut copy = running_on(&server, hex);
        copy.try_update(|parts| parts.runtime = ContainerRuntimeObservation::Exited { code: 1 })
            .unwrap();
        copy
    };
    let crashed = report(&exited('1'), &server.name);
    let mut stopped = exited('2');
    stopped.try_update(|parts| parts.stopped = true).unwrap();
    let stopped = report(&stopped, &server.name);

    for report in [&crashed, &stopped] {
        assert_eq!(
            (report.state, report.exit_code),
            (ContainerState::Exited, Some(1))
        );
    }
    assert!(!crashed.stopped);
    assert!(stopped.stopped);
}

/// A containers answer as a reader from before the stop mark decodes it: the
/// states `ployz mcp` knew then, and no `stopped`.
#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum OldContainerState {
    Created,
    Running,
    Restarting,
    Paused,
    Exited,
    Dead,
}

#[derive(Debug, serde::Deserialize)]
struct OldContainerReport {
    state: OldContainerState,
    exit_code: Option<i64>,
}

#[test]
fn a_reader_older_than_the_stop_mark_reads_a_stopped_copy_as_exited_with_its_code() {
    let server = machine('a', "alpha");
    let mut stopped = running_on(&server, '2');
    stopped
        .try_update(|parts| {
            parts.runtime = ContainerRuntimeObservation::Exited { code: 1 };
            parts.stopped = true;
        })
        .unwrap();

    let wire = serde_json::to_value(report(&stopped, &server.name)).unwrap();
    assert_eq!(
        wire.get("stopped"),
        Some(&serde_json::json!(true)),
        "a newer reader sees the stop"
    );
    let old: OldContainerReport =
        serde_json::from_value(wire).expect("an older ployz mcp decodes a newer Cloud's answer");

    assert_eq!(old.state, OldContainerState::Exited);
    assert_eq!(old.exit_code, Some(1));
}

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

#[tokio::test]
async fn a_new_environment_gets_no_region_while_a_servers_record_is_incomplete() {
    let mut one = support::machine('a', "one");
    one.machine.labels.insert(
        ployz_core::MachineLabelKey::parse("region").unwrap(),
        ployz_core::MachineLabelValue::parse("default").unwrap(),
    );
    let pinned = |incomplete: Vec<ployz_core::MachineId>| {
        let service = support::DeployService::new(one.clone()).runtime_watch_incomplete(incomplete);
        async {
            let servers = CloudQuery::Servers { only: None };
            acted(service, |session, _| async move {
                ask(&session, servers, &Readable::All, None).await
            })
            .await
            .unwrap()
            .get("new_environment_region")
            .cloned()
            .unwrap_or_default()
        }
    };
    assert_eq!(pinned(Vec::new()).await, serde_json::json!("default"));
    let unread = support::machine('b', "two").machine.id;
    assert_eq!(pinned(vec![unread]).await, serde_json::Value::Null);
}

fn logs_in(namespace: &str) -> LogsQuery {
    LogsQuery {
        namespace: Namespace::parse(namespace).unwrap(),
        service: ServiceName::parse("api").unwrap(),
        deployment: None,
        server: None,
        lines: LineCap::default(),
        since: None,
    }
}

#[tokio::test]
async fn a_namespace_outside_readable_looks_absent_for_containers_and_logs() {
    let readable = Readable::Only([Namespace::parse("a").unwrap()].into());
    let asked = |query: CloudQuery| {
        let readable = readable.clone();
        acted(
            support::DeployService::new(support::machine('a', "one")),
            |session, _| async move { ask(&session, query, &readable, None).await },
        )
    };
    let containers = |namespace: &str| CloudQuery::Containers {
        namespace: Namespace::parse(namespace).unwrap(),
        service: None,
    };
    for query in [containers("b"), CloudQuery::Logs(logs_in("b"))] {
        let refusal = asked(query).await.unwrap_err();
        assert_eq!(refusal.code, RpcErrorCode::NotFound, "{refusal:?}");
        assert_eq!(refusal.message, "No Server runs Namespace b");
    }
    asked(containers("a")).await.unwrap();
    asked(CloudQuery::Logs(logs_in("a"))).await.unwrap();
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
async fn a_start_never_routes_a_copy_no_deploy_brought_in() {
    let service = support::DeployService::new(support::machine('a', "one"));
    let rotations = service.rotations();
    let error = acted(service, |session, machine| async move {
        start_one(&session, machine, container_id('1')).await
    })
    .await
    .unwrap_err();
    assert!(
        error.message.contains("left Pending and not routed"),
        "{error:?}"
    );
    assert_ne!(
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
async fn a_start_whose_health_cannot_be_read_is_unconfirmed() {
    let service = support::DeployService::new(support::machine('a', "one"))
        .with_rotation(container_id('1'), CopyRotation::Held(run()))
        .unread_health();
    assert_eq!(started(service).await, Started::Unconfirmed);
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

// The shared wait's own tests cover deadlines past the Serving wait in paused time.
#[tokio::test]
async fn a_start_returns_only_once_its_healthcheck_passes() {
    let service = support::DeployService::new(support::machine('a', "one"))
        .with_rotation(container_id('1'), CopyRotation::Held(run()))
        .turn_healthy_on_release();
    let start = tokio::spawn(started(service.clone()));
    tokio::time::timeout(Duration::from_secs(30), service.health_waited())
        .await
        .expect("the start never waited for health");
    assert!(
        !start.is_finished(),
        "returned before the healthcheck passed"
    );
    service.release_health();
    assert_eq!(start.await.unwrap(), Started::Serving);
}

#[tokio::test]
async fn a_start_that_never_passes_its_healthcheck_is_unhealthy_at_its_deadline() {
    let service = support::DeployService::new(support::machine('a', "one"))
        .with_rotation(container_id('1'), CopyRotation::Held(run()))
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
    assert_eq!(started(service).await, Started::Unhealthy);
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
// a redeploy removed, withholds the lines of every other Container on that Server, shows
// its own lines unscrubbed, or leaves them out without saying so.
#[tokio::test]
async fn a_removed_containers_lines_are_left_out_with_a_note_and_the_rest_still_shown() {
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
    assert_eq!(
        answer.notes,
        [
            "Lines from web/dddddddddddd on one withheld: Ployz couldn't check their variables for secret values, so it can't hide them."
        ]
    );
}

// Catches a logs ask that leaves out the lines of a removed Container whose env the Log
// Store kept, or shows them with their Sealed value in the clear.
#[tokio::test]
async fn a_removed_containers_lines_are_scrubbed_with_the_env_its_log_store_kept() {
    let one = support::machine('a', "one");
    let mut service = support::DeployService::new(one.clone());
    let removed = ContainerId::parse("d".repeat(64)).unwrap();
    service.uninspectable.insert(removed);
    service.retained_envs.as_mut().unwrap().insert(
        removed,
        BTreeMap::from([
            ("TOKEN".to_owned(), "live-only-sentinel".to_owned()),
            ("PLAIN".to_owned(), "from-docker".to_owned()),
        ]),
    );

    let [answer] = ask_logs(service, vec![logs_query(None, Some(&["TOKEN", "PLAIN"]))])
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
    assert!(answer.notes.is_empty(), "{:?}", answer.notes);
}

// Catches a logs ask that reads a Container's kept env when its Machine failed to inspect
// it rather than answering that it is gone: its lines stay left out.
#[tokio::test]
async fn a_container_whose_inspect_fails_is_left_out_even_with_a_kept_env() {
    let one = support::machine('a', "one");
    let mut service = support::DeployService::new(one.clone());
    let stalled = ContainerId::parse("d".repeat(64)).unwrap();
    service.stalled_inspects.insert(stalled);
    service.retained_envs.as_mut().unwrap().insert(
        stalled,
        BTreeMap::from([("TOKEN".to_owned(), "live-only-sentinel".to_owned())]),
    );

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

// Catches a logs ask against a daemon that predates the retained env route treating its
// refusal as anything but no env: the removed Container's lines stay left out.
#[tokio::test]
async fn a_daemon_without_the_retained_env_route_leaves_a_removed_containers_lines_out() {
    let one = support::machine('a', "one");
    let mut service = support::DeployService::new(one.clone());
    service
        .uninspectable
        .insert(ContainerId::parse("d".repeat(64)).unwrap());
    service.retained_envs = None;

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
    assert_eq!(
        sealed.notes,
        [
            "Lines from web/cccccccccccc, web/dddddddddddd on one withheld: Ployz couldn't check their variables for secret values, so it can't hide them."
        ]
    );
    assert_eq!(shown(&nothing_sealed).len(), 1);
}

#[test]
fn the_sdk_log_filter_reads_exactly_the_label_keys_the_labels_module_writes() {
    let script = include_str!("../../../ployz-sdk/runtime-logs.js");
    let read: BTreeSet<&str> = script
        .split("labels[\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .collect();
    assert_eq!(
        read,
        BTreeSet::from([
            ployz_core::labels::CLOUD_SERVICE_ID,
            ployz_core::labels::DEPLOYMENT_ID,
            ployz_core::labels::SERVICE_NAME,
        ])
    );
}
