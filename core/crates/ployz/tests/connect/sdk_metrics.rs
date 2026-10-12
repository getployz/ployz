//! Façade tests for metric history across a Cluster's Machines.

use ployz::sdk::{MetricHistoryInput, MetricSubject};
use ployz_core::{
    DESCRIBE_CONTRACT_CAPABILITY, MachineId, MembershipObservation, QualifiedService,
    metrics::{MetricHistoryReply, MetricPoints, MetricScope, ServicePoint},
};
use tonic::Status;

use super::support::{DiscoveryService, machine, machine_id};
use super::unix_session::{self, UnixSession};

const SINCE: i64 = 1_700_000_000_000_000_000;
const MIN: i64 = 60_000_000_000;

#[tokio::test]
async fn a_service_sums_the_machines_that_answer_and_names_the_rest() {
    let web = QualifiedService::parse("prod/web").unwrap();
    let mut down = machine('c', "three");
    down.membership = MembershipObservation::Down;
    let mut service = DiscoveryService::new(description());
    service.machines = vec![machine('a', "one"), machine('b', "two"), down];
    service.metric_replies.lock().unwrap().extend([
        (machine_id('a'), Ok(reply(vec![point(0, 2.0)]))),
        (
            machine_id('b'),
            Err(Status::internal("ployz-observe cannot read its metrics")),
        ),
    ]);
    let requests = service.metric_requests.clone();
    let (client, _session, _machine) = session(service).await;

    let history = client
        .metric_history(MetricHistoryInput {
            subject: MetricSubject::Service {
                service: web.clone(),
            },
            since_nanos: SINCE.to_string(),
            until_nanos: (SINCE + 10 * MIN).to_string(),
            step_nanos: None,
        })
        .await
        .unwrap();

    assert_eq!(history.points, MetricPoints::Service(vec![point(0, 2.0)]));
    assert_eq!(history.step_nanos, Some(MIN));
    assert_eq!(
        history
            .machines
            .iter()
            .map(|answer| answer.machine_id)
            .collect::<Vec<_>>(),
        [machine_id('a')]
    );
    let mut unanswered: Vec<MachineId> = history
        .unanswered
        .iter()
        .map(|failure| failure.machine_id)
        .collect();
    unanswered.sort();
    assert_eq!(unanswered, [machine_id('b'), machine_id('c')]);
    let asked = requests.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "a Machine that is down is not asked");
    assert!(asked.iter().all(|(_, request)| {
        request.scope
            == MetricScope::Service {
                service: web.clone(),
            }
    }));
}

#[tokio::test]
async fn a_machine_reads_only_that_machine() {
    let mut service = DiscoveryService::new(description());
    service.machines = vec![machine('a', "one"), machine('b', "two")];
    service
        .metric_replies
        .lock()
        .unwrap()
        .insert(machine_id('b'), Ok(reply(vec![point(0, 1.0)])));
    let requests = service.metric_requests.clone();
    let (client, _session, _machine) = session(service).await;

    let history = client
        .metric_history(MetricHistoryInput {
            subject: MetricSubject::Machine {
                machine_id: machine_id('b'),
            },
            since_nanos: SINCE.to_string(),
            until_nanos: (SINCE + 10 * MIN).to_string(),
            step_nanos: Some(MIN.to_string()),
        })
        .await
        .unwrap();

    assert!(history.unanswered.is_empty());
    let asked = requests.lock().unwrap().clone();
    let [(asked_machine, request)] = asked.as_slice() else {
        panic!("only the named Machine is asked: {asked:?}");
    };
    assert_eq!(*asked_machine, machine_id('b'));
    assert_eq!(request.scope, MetricScope::Machine);
    assert_eq!(request.step_nanos, Some(MIN));
}

#[tokio::test]
async fn a_backwards_range_is_refused_before_any_machine_is_asked() {
    let service = DiscoveryService::new(description());
    let requests = service.metric_requests.clone();
    let (client, _session, _machine) = session(service).await;

    let error = client
        .metric_history(MetricHistoryInput {
            subject: MetricSubject::Service {
                service: QualifiedService::parse("prod/web").unwrap(),
            },
            since_nanos: (SINCE + MIN).to_string(),
            until_nanos: SINCE.to_string(),
            step_nanos: None,
        })
        .await
        .unwrap_err();

    assert_eq!(error.code, ployz_core::RpcErrorCode::InvalidArgument);
    assert!(requests.lock().unwrap().is_empty());
}

fn reply(points: Vec<ServicePoint>) -> MetricHistoryReply {
    MetricHistoryReply {
        points: MetricPoints::Service(points),
        step_nanos: MIN,
        gaps: Vec::new(),
        markers: Vec::new(),
    }
}

fn point(minute: i64, cpu: f64) -> ServicePoint {
    ServicePoint {
        ts: SINCE + minute * MIN,
        cpu_usec: Some(cpu),
        mem_bytes: Some(1_024),
        mem_limit: None,
        rx: None,
        tx: None,
        io_r: None,
        io_w: None,
    }
}

async fn session(
    service: DiscoveryService,
) -> (ployz::sdk::Session, UnixSession, unix_session::FakeMachine) {
    let description = description();
    let session = UnixSession::start().await;
    let machine = session.spawn_machine(description.machine_id, service).await;
    let client = unix_session::connect(&session.directory, description.machine_id.as_str())
        .await
        .unwrap();
    (client, session, machine)
}

fn description() -> ployz_core::ContractDescription {
    ployz_core::ContractDescription {
        machine_id: MachineId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        protocol_major: ployz_core::PROTOCOL_MAJOR,
        daemon_version: "test".into(),
        capabilities: [ployz_core::CapabilityName::parse(DESCRIBE_CONTRACT_CAPABILITY).unwrap()]
            .into(),
    }
}
