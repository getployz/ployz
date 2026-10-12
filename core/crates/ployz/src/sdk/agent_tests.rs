use std::{collections::BTreeMap, sync::Arc, time::Duration};

use ployz_core::{
    ConfiguredHealthcheck, ContainerKind, HealthcheckCommand, HealthcheckSpec, Namespace,
    ServiceName,
};
use ployz_store::{
    EnvironmentId,
    tools::{
        LineCap, LogText, LogsQuery, ServerRef,
        cloud::{ContainerState, LogsAnswer},
    },
};
use tokio::time::Instant;

use super::{ServiceCopy, Started, copies_of, logs, start_one};
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
