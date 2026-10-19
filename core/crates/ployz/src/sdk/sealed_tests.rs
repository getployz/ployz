use super::*;
use crate::connect::test_support::rpc_routes;
use crate::global_fixtures::{machine, running_on};
use crate::sdk::SessionInner;
use ployz_core::{
    ContainerDetails, HistoryContainer, LogDirection, HistoryContainerKind, HistoryGapReason, HistoryStream,
    LogOrigin, OpaquePayload, RpcRequestBody, RpcResponse, ServiceId, ServiceName,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

type Frames = mpsc::Sender<Result<OpaquePayload, tonic::Status>>;

const VALUE: &str = "73519086427351908642";

/// What the fake Store answers each names read with: the names, None for Every, or a failure.
type Answer = Arc<std::sync::Mutex<Result<Option<BTreeSet<String>>, ()>>>;

struct Fake {
    session: Session,
    names: SealedNames,
    answer: Answer,
    _server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

/// One Machine whose Container holds `TOKEN=env`, or whose env cannot be read when `env` is
/// None, serving one log stream per entry of `streams` in order, and a feed answering every
/// names read from `answer`.
async fn fake(env: Option<&str>, streams: usize) -> (Fake, Vec<Frames>) {
    let (senders, receivers): (Vec<_>, std::collections::VecDeque<_>) =
        (0..streams).map(|_| mpsc::channel(8)).unzip();
    let receivers = Arc::new(std::sync::Mutex::new(receivers));
    let container = running_on(&machine('a', "one"), '1');
    let env = env.map(|value| BTreeMap::from([("TOKEN".to_owned(), value.to_owned())]));
    let (client, server) = rpc_routes(
        move |rpc: tonic::Request<OpaquePayload>| {
            let container = container.clone();
            let env = env.clone();
            async move {
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "the fixture answers only the env read"
                )]
                match rpc.into_inner().decode_request().unwrap().body {
                    RpcRequestBody::InspectContainer(_) => {
                        let Some(environment) = env else {
                            return Err(tonic::Status::unavailable("the Machine did not answer"));
                        };
                        Ok(tonic::Response::new(
                            RpcResponse::from(ContainerDetails {
                                container,
                                environment: Some(environment),
                                image_id: None,
                                last_check: None,
                                run: None,
                            })
                            .encode()
                            .unwrap(),
                        ))
                    }
                    body => panic!("unexpected request {body:?}"),
                }
            }
        },
        move |_| {
            let receiver = receivers.lock().unwrap().pop_front().unwrap();
            async move { Ok(tonic::Response::new(ReceiverStream::new(receiver))) }
        },
    )
    .await;
    let session = Session {
        inner: Arc::new(SessionInner {
            client: std::sync::Mutex::new(Some(client)),
            cancel: CancellationToken::new(),
        }),
    };
    let names = SealedNames::new();
    let answer: Answer = Arc::new(std::sync::Mutex::new(Ok(Some(BTreeSet::new()))));
    let feed = names.clone();
    let answered = Arc::clone(&answer);
    tokio::spawn(async move {
        loop {
            feed.wanted().await;
            let read = feed.begin();
            match answered.lock().unwrap().clone() {
                Ok(names) => read.done(names),
                Err(()) => read.failed(),
            }
        }
    });
    let fake = Fake {
        session,
        names,
        answer,
        _server: server,
    };
    (fake, senders)
}

fn sealed(name: &str) -> Option<BTreeSet<String>> {
    Some(BTreeSet::from([name.to_owned()]))
}

fn container_id() -> ContainerId {
    ContainerId::parse("1".repeat(64)).unwrap()
}

fn machine_id() -> MachineId {
    MachineId::parse("a".repeat(32)).unwrap()
}

fn frame(timestamp: i64, text: &str) -> Result<OpaquePayload, tonic::Status> {
    Ok(LogEntry {
        metadata: LogMetadata {
            origin: LogOrigin::Service {
                service_id: ServiceId::parse("1".repeat(32)).unwrap(),
                service_name: ServiceName::parse("api").unwrap(),
                container_id: container_id(),
                hook: None,
            },
            machine_id: machine_id(),
            machine_name: machine('a', "one").name,
        },
        timestamp_unix_nanos: timestamp,
        body: LogBody::Stdout(text.as_bytes().to_vec()),
    }
    .encode()
    .unwrap())
}

fn row(row: HistoryRow) -> Result<OpaquePayload, tonic::Status> {
    Ok(row.encode().unwrap())
}

fn history_container() -> HistoryRow {
    HistoryRow::Container(HistoryContainer {
        container_id: container_id(),
        namespace: Some("app".into()),
        service: Some("api".into()),
        deployment: None,
        replica: "1".into(),
        kind: HistoryContainerKind::Service,
    })
}

fn history_line(ts: i64, text: &str) -> HistoryRow {
    HistoryRow::Line {
        container_id: container_id(),
        ts,
        stream: HistoryStream::Stdout,
        text: text.as_bytes().to_vec(),
    }
}

async fn live(fake: &Fake) -> ScrubbedLogStream {
    fake.session
        .scrubbed_logs(
            ContainerLogInput {
                machine_id: machine_id(),
                container_id: container_id(),
                tail: 100,
                follow: true,
                since_unix_seconds: None,
            },
            &fake.names,
        )
        .await
        .unwrap()
}

async fn history(fake: &Fake) -> ScrubbedHistoryStream {
    fake.session
        .scrubbed_history(
            LogHistoryInput {
                machine_id: machine_id(),
                namespace: Some("app".into()),
                service: None,
                deployment: None,
                container_id: None,
                since_nanos: None,
                until_nanos: None,
                direction: LogDirection::Forward,
                limit: 200,
                cursor: None,
            },
            &fake.names,
        )
        .await
        .unwrap()
}

async fn page(reader: &ScrubbedHistoryStream) -> Vec<LogHistoryRecord> {
    let mut records = Vec::new();
    while let Some(record) = reader.next().await.unwrap() {
        let end = matches!(record, LogHistoryRecord::End { .. });
        records.push(record);
        if end {
            return records;
        }
    }
    records
}

async fn shown(reader: &ScrubbedLogStream) -> Vec<LogLine> {
    let mut lines = Vec::new();
    while let Some(record) = reader.next().await.unwrap() {
        lines.push(record.line);
    }
    lines
}

fn messages(lines: &[LogLine]) -> String {
    lines
        .iter()
        .map(|line| match line {
            LogLine::Shown { message, .. } => message.as_str(),
            LogLine::Hidden => "<hidden>",
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_history_page_never_shows_a_sealed_value() {
    let value = "ERROR-0123456789abcd";
    let (fake, streams) = fake(Some(value), 1).await;
    *fake.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    let reader = history(&fake).await;
    for history_row in [
        history_container(),
        history_line(1, &format!("{value} connected")),
        HistoryRow::End { next: None },
    ] {
        streams[0].send(row(history_row)).await.unwrap();
    }

    let records = page(&reader).await;
    let LogHistoryRecord::Line { message, level, .. } = &records[1] else {
        panic!("expected a line, got {records:?}");
    };
    assert_eq!(message, "[secret TOKEN] connected");
    assert_eq!(*level, LogLevel::Info);
}

#[tokio::test(start_paused = true)]
async fn a_name_sealed_mid_stream_is_scrubbed_from_then_on() {
    let (fake, streams) = fake(Some(VALUE), 1).await;
    let reader = live(&fake).await;
    streams[0]
        .send(frame(1, &format!("before {VALUE}\n")))
        .await
        .unwrap();
    let Some(LiveLogRecord {
        line: LogLine::Shown { message, .. },
        ..
    }) = reader.next().await.unwrap()
    else {
        panic!("the first line was not shown");
    };
    assert_eq!(message, format!("before {VALUE}\n"));

    *fake.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    fake.names.begin().done(sealed("TOKEN"));
    streams[0]
        .send(frame(2, &format!("after {VALUE}\n")))
        .await
        .unwrap();
    drop(streams);

    let lines = shown(&reader).await;
    assert_eq!(messages(&lines), "after [secret TOKEN]\n");
}

#[tokio::test(start_paused = true)]
async fn a_value_split_across_frames_is_scrubbed() {
    let (fake, mut streams) = fake(Some(VALUE), 2).await;
    *fake.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    let (head, tail) = VALUE.split_at(10);

    let reader = live(&fake).await;
    let first = streams.remove(0);
    first.send(frame(1, &format!("pass {head}"))).await.unwrap();
    first.send(frame(2, &format!("{tail} ok\n"))).await.unwrap();
    first.send(frame(3, &format!("key={head}"))).await.unwrap();
    drop(first);
    let before = shown(&reader).await;

    let reopened = live(&fake).await;
    let second = streams.remove(0);
    second.send(frame(3, &format!("key={head}"))).await.unwrap();
    second.send(frame(4, &format!("{tail}\n"))).await.unwrap();
    drop(second);
    let after = shown(&reopened).await;

    let joined = messages(&before) + &messages(&after);
    assert_eq!(
        joined,
        "pass [secret TOKEN] ok\nkey=[secret TOKEN]key=[secret TOKEN]\n"
    );
    assert!(!joined.bytes().any(|byte| byte.is_ascii_digit()), "{joined}");
}

#[tokio::test(start_paused = true)]
async fn unreadable_names_or_env_hide_the_lines() {
    let (names_failed, streams) = fake(Some(VALUE), 1).await;
    *names_failed.answer.lock().unwrap() = Err(());
    let reader = live(&names_failed).await;
    streams[0]
        .send(frame(1, &format!("live {VALUE}\n")))
        .await
        .unwrap();
    drop(streams);
    assert!(matches!(shown(&reader).await.as_slice(), [LogLine::Hidden]));

    let (env_unreadable, streams) = fake(None, 1).await;
    *env_unreadable.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    let reader = history(&env_unreadable).await;
    for history_row in [
        history_container(),
        history_line(1, &format!("first {VALUE}")),
        HistoryRow::Gap {
            container_id: container_id(),
            from: 2,
            to: 3,
            reason: HistoryGapReason::NotCaptured,
        },
        history_line(4, &format!("second {VALUE}")),
        HistoryRow::End { next: None },
    ] {
        streams[0].send(row(history_row)).await.unwrap();
    }
    assert!(matches!(
        page(&reader).await.as_slice(),
        [
            LogHistoryRecord::Container(_),
            LogHistoryRecord::Hidden { .. },
            LogHistoryRecord::Gap { .. },
            LogHistoryRecord::Hidden { .. },
            LogHistoryRecord::End { .. },
        ]
    ));

    let (history_failed, streams) = fake(Some(VALUE), 1).await;
    *history_failed.answer.lock().unwrap() = Err(());
    let reader = history(&history_failed).await;
    for history_row in [
        history_container(),
        history_line(1, &format!("stored {VALUE}")),
        HistoryRow::End { next: None },
    ] {
        streams[0].send(row(history_row)).await.unwrap();
    }
    assert!(matches!(
        page(&reader).await.as_slice(),
        [
            LogHistoryRecord::Container(_),
            LogHistoryRecord::Hidden { .. },
            LogHistoryRecord::End { .. },
        ]
    ));
}
