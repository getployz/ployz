//! What the call engine sends again, and when it stops.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use ployz_core::{
    ContractDescription, ListMachinesRequest, MachineId, MachineList, OpaquePayload,
    RpcRequestBody, RpcResponse, op,
};
use tokio::time::Instant;

use super::{Call, Client, Deadline, Replay, Wait, call::Pacing};
use crate::connect::{ConnectError, UNARY_RETRY_DELAYS, target_read_timeout};

fn machine() -> MachineId {
    MachineId::parse("a".repeat(32)).unwrap()
}

type Server = tokio::task::JoinHandle<Result<(), tonic::transport::Error>>;

/// What the fake daemon did, in the order it was asked.
#[derive(Clone, Default)]
struct Record {
    /// The requests that were not the redial's identity check, as the daemon decoded them.
    sent: Arc<Mutex<Vec<String>>>,
    /// The identity checks a redial made.
    redials: Arc<AtomicUsize>,
}

impl Record {
    fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }

    fn redials(&self) -> usize {
        self.redials.load(Ordering::SeqCst)
    }
}

/// A Machine whose `ListMachines` answers `answer` by how many times it has been asked, and
/// whose identity check for a redial always succeeds.
async fn daemon<F, Fut>(answer: F) -> (Client, Record, Server)
where
    F: Fn(usize) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<MachineList, tonic::Status>> + Send + 'static,
{
    let record = Record::default();
    let seen = record.clone();
    let (mut client, server) =
        crate::connect::test_support::rpc_client(move |rpc: tonic::Request<OpaquePayload>| {
            let record = seen.clone();
            let answer = answer.clone();
            async move {
                let body = rpc.into_inner().decode_request().unwrap().body;
                if matches!(body, RpcRequestBody::DescribeContract(_)) {
                    record.redials.fetch_add(1, Ordering::SeqCst);
                    let described = RpcResponse::from(ContractDescription {
                        attest: None,
                        machine_id: machine(),
                        protocol_major: ployz_core::PROTOCOL_MAJOR,
                        daemon_version: "test".into(),
                        capabilities: Default::default(),
                        sealing: None,
                    });
                    return Ok(tonic::Response::new(described.encode().unwrap()));
                }
                let attempt = {
                    let mut sent = record.sent.lock().unwrap();
                    sent.push(format!("{body:?}"));
                    sent.len()
                };
                let listed = answer(attempt).await?;
                Ok(tonic::Response::new(
                    RpcResponse::from(listed).encode().unwrap(),
                ))
            }
        })
        .await;
    client.session.entry = Some(machine());
    client.session.confirmed = Some(machine());
    (client, record, server)
}

fn listed() -> MachineList {
    MachineList {
        enrollment: None,
        machines: Vec::new(),
    }
}

/// Drops the first `drops` requests as a restarting daemon does, then answers.
async fn drops(drops: usize) -> (Client, Record, Server) {
    daemon(move |attempt| async move {
        if attempt <= drops {
            Err(tonic::Status::unavailable("restarting"))
        } else {
            Ok(listed())
        }
    })
    .await
}

async fn ask(client: &mut Client, pacing: impl Pacing) -> Result<MachineList, ConnectError> {
    client
        .call::<op::ListMachines>(ListMachinesRequest {}, pacing)
        .await
}

#[tokio::test(start_paused = true)]
async fn a_read_goes_out_again_after_a_dropped_connection() {
    let (mut client, record, server) = drops(1).await;

    let answer = ask(&mut client, Call::read()).await;

    assert_eq!(answer.unwrap(), listed());
    assert_eq!(record.sent().len(), 2);
    assert_eq!(record.redials(), 1);
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_keyed_mutation_is_sent_again_exactly_as_it_was() {
    let (mut client, record, server) = drops(2).await;

    let answer = ask(&mut client, Call::keyed()).await;

    assert_eq!(answer.unwrap(), listed());
    let sent = record.sent();
    assert_eq!(sent.len(), 3);
    assert!(
        sent.iter().all(|request| Some(request) == sent.first()),
        "the engine mints nothing: {sent:?}"
    );
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_call_made_once_is_never_sent_again() {
    let (mut client, record, server) = drops(1).await;

    let answer = ask(&mut client, Call::once()).await;

    assert!(answer.is_err(), "{answer:?}");
    assert_eq!(record.sent().len(), 1);
    assert_eq!(record.redials(), 0);
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_call_made_once_on_a_shared_client_is_never_sent_again() {
    let (client, record, server) = drops(1).await;

    let answer = client
        .call_shared::<op::ListMachines>(ListMachinesRequest {}, Call::once())
        .await;

    assert!(answer.is_err(), "{answer:?}");
    assert_eq!(record.sent().len(), 1);
    server.abort();
}

#[test]
fn what_a_call_lets_the_engine_send_again_is_what_its_constructor_says() {
    assert_eq!(Call::read().replay(), Replay::Read);
    assert_eq!(Call::keyed().replay(), Replay::Keyed);
    assert_eq!(Call::once().replay(), Replay::Once);
    assert_eq!(
        Call::read().budget(Duration::from_secs(1)).replay(),
        Replay::Read
    );
    assert_eq!(
        Call::keyed().waiting(Wait::standard()).replay(),
        Replay::Keyed
    );
}

#[tokio::test(start_paused = true)]
async fn a_read_sends_again_only_as_often_as_the_retry_delays_allow() {
    let (mut client, record, server) = drops(usize::MAX).await;
    let started = Instant::now();

    let answer = ask(&mut client, Call::read()).await;

    assert!(answer.is_err(), "{answer:?}");
    assert_eq!(record.sent().len(), 1 + UNARY_RETRY_DELAYS.len());
    assert_eq!(
        started.elapsed(),
        UNARY_RETRY_DELAYS.iter().sum::<Duration>()
    );
    server.abort();
}

#[test]
fn the_retry_delays_are_the_ones_the_entry_has_always_used() {
    assert_eq!(
        UNARY_RETRY_DELAYS,
        [
            Duration::from_millis(500),
            Duration::from_millis(1500),
            Duration::from_secs(4),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn an_answer_that_is_not_a_dropped_connection_is_not_sent_again() {
    let (mut client, record, server) =
        daemon(|_| async { Err(tonic::Status::permission_denied("not yours")) }).await;

    let answer = ask(&mut client, Call::read()).await;

    assert!(answer.is_err(), "{answer:?}");
    assert_eq!(record.sent().len(), 1);
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn one_budget_covers_every_attempt_and_a_silent_machine_costs_it_once() {
    let (mut client, record, server) = daemon(|_| async {
        std::future::pending::<()>().await;
        Ok(listed())
    })
    .await;
    let started = Instant::now();

    let answer = ask(&mut client, Call::read().budget(target_read_timeout())).await;

    assert_eq!(started.elapsed(), target_read_timeout());
    assert!(format!("{:?}", answer.unwrap_err()).contains("target Machine RPC timed out"),);
    assert_eq!(record.sent().len(), 1);
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_deadline_on_a_call_made_once_speaks_in_its_own_words() {
    for (deadline, words, after) in [
        (
            Deadline::SetupRead,
            "Machine setup read timed out",
            Duration::from_secs(5),
        ),
        (
            Deadline::MutationReply(Duration::from_secs(7)),
            "Machine setup mutation reply timed out",
            Duration::from_secs(7),
        ),
        (
            Deadline::Target(Duration::from_secs(3)),
            "target Machine RPC timed out",
            Duration::from_secs(3),
        ),
    ] {
        let (mut client, record, server) = daemon(|_| async {
            std::future::pending::<()>().await;
            Ok(listed())
        })
        .await;
        let started = Instant::now();

        let answer = ask(&mut client, Call::once().within(deadline)).await;

        assert_eq!(started.elapsed(), after, "{deadline:?}");
        assert!(
            format!("{:?}", answer.unwrap_err()).contains(words),
            "{deadline:?}"
        );
        assert_eq!(record.sent().len(), 1);
        server.abort();
    }
}

#[tokio::test(start_paused = true)]
async fn a_wait_sends_again_a_second_apart_until_the_machine_answers() {
    let (mut client, record, server) = drops(3).await;
    let started = Instant::now();

    let answer = ask(
        &mut client,
        Call::read().waiting(Wait::of(Duration::from_secs(30))),
    )
    .await;

    assert_eq!(answer.unwrap(), listed());
    assert_eq!(record.sent().len(), 4);
    assert_eq!(started.elapsed(), Duration::from_secs(3));
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn a_wait_that_runs_out_says_how_long_it_waited() {
    let (mut client, _record, server) = drops(usize::MAX).await;

    let answer = ask(
        &mut client,
        Call::read().waiting(Wait::of(Duration::from_secs(5))),
    )
    .await;

    assert!(
        matches!(answer, Err(ConnectError::Exhausted(_))),
        "{answer:?}"
    );
    server.abort();
}
