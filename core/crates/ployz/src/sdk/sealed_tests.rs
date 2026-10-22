use super::*;
use crate::connect::test_support::{LOG_ROUTES, rpc_routes};
use crate::global_fixtures::{machine, running_on};
use crate::sdk::SessionInner;
use ployz_core::{
    ContainerDetails, HistoryContainer, HistoryContainerKind, HistoryGapReason, HistoryStream,
    LogDirection, LogOrigin, OpaquePayload, RpcRequestBody, RpcResponse, ServiceId, ServiceName,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// One log stream the fake Machine serves, fed by the test.
struct Frames(mpsc::Sender<Result<OpaquePayload, tonic::Status>>);

impl Frames {
    async fn line(&self, timestamp: i64, text: &str) {
        self.send(frame(timestamp, LogBody::Stdout(text.as_bytes().to_vec())))
            .await;
    }

    async fn err(&self, timestamp: i64, text: &str) {
        self.send(frame(timestamp, LogBody::Stderr(text.as_bytes().to_vec())))
            .await;
    }

    async fn rows(&self, rows: impl IntoIterator<Item = HistoryRow>) {
        for row in rows {
            self.send(row.encode().unwrap()).await;
        }
    }

    async fn send(&self, payload: OpaquePayload) {
        self.0.send(Ok(payload)).await.unwrap();
    }
}

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
/// None, serving its `STREAMS` log streams in order, and a feed answering every names read from
/// `answer`.
async fn fake<const STREAMS: usize>(env: Option<&str>) -> (Fake, [Frames; STREAMS]) {
    let mut receivers = std::collections::VecDeque::new();
    let senders = std::array::from_fn(|_| {
        let (sender, receiver) = mpsc::channel(8);
        receivers.push_back(receiver);
        Frames(sender)
    });
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
        LOG_ROUTES,
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

fn frame(timestamp: i64, body: LogBody) -> OpaquePayload {
    LogEntry {
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
        body,
    }
    .encode()
    .unwrap()
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
    let (fake, [frames]) = fake(Some(value)).await;
    *fake.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    let reader = history(&fake).await;
    frames
        .rows([
            history_container(),
            history_line(1, &format!("{value} connected")),
            HistoryRow::End { next: None },
        ])
        .await;

    let records = page(&reader).await;
    let [_, LogHistoryRecord::Line { message, level, .. }, _] = records.as_slice() else {
        panic!("expected a line, got {records:?}");
    };
    assert_eq!(message, "[secret TOKEN] connected");
    assert_eq!(*level, LogLevel::Info);
}

#[tokio::test(start_paused = true)]
async fn a_name_sealed_mid_stream_is_scrubbed_from_then_on() {
    let (fake, [frames]) = fake(Some(VALUE)).await;
    let reader = live(&fake).await;
    frames.line(1, &format!("before {VALUE}\n")).await;
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
    frames.line(2, &format!("after {VALUE}\n")).await;
    drop(frames);

    let lines = shown(&reader).await;
    assert_eq!(messages(&lines), "after [secret TOKEN]\n");
}

#[tokio::test(start_paused = true)]
async fn a_value_split_across_frames_is_scrubbed() {
    let (fake, [first, second]) = fake(Some(VALUE)).await;
    *fake.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    let (head, tail) = VALUE.split_at(10);

    let reader = live(&fake).await;
    first.line(1, &format!("pass {head}")).await;
    first.line(2, &format!("{tail} ok\n")).await;
    first.line(3, &format!("key={head}")).await;
    drop(first);
    let before = shown(&reader).await;

    let reopened = live(&fake).await;
    second.line(3, &format!("key={head}")).await;
    second.line(4, &format!("{tail}\n")).await;
    drop(second);
    let after = shown(&reopened).await;

    let joined = messages(&before) + &messages(&after);
    assert_eq!(
        joined,
        "pass [secret TOKEN] ok\nkey=[secret TOKEN]key=[secret TOKEN]\n"
    );
    assert!(
        !joined.bytes().any(|byte| byte.is_ascii_digit()),
        "{joined}"
    );
}

#[tokio::test(start_paused = true)]
async fn unreadable_names_or_env_hide_the_lines() {
    let (names_failed, [frames]) = fake(Some(VALUE)).await;
    *names_failed.answer.lock().unwrap() = Err(());
    let reader = live(&names_failed).await;
    frames.line(1, &format!("live {VALUE}\n")).await;
    drop(frames);
    assert!(matches!(shown(&reader).await.as_slice(), [LogLine::Hidden]));

    let (env_unreadable, [frames]) = fake(None).await;
    *env_unreadable.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    let reader = history(&env_unreadable).await;
    frames
        .rows([
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
        ])
        .await;
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

    let (history_failed, [frames]) = fake(Some(VALUE)).await;
    *history_failed.answer.lock().unwrap() = Err(());
    let reader = history(&history_failed).await;
    frames
        .rows([
            history_container(),
            history_line(1, &format!("stored {VALUE}")),
            HistoryRow::End { next: None },
        ])
        .await;
    assert!(matches!(
        page(&reader).await.as_slice(),
        [
            LogHistoryRecord::Container(_),
            LogHistoryRecord::Hidden { .. },
            LogHistoryRecord::End { .. },
        ]
    ));
}

#[tokio::test(start_paused = true)]
async fn a_names_read_after_a_failure_shows_lines_again() {
    let (fake, [frames]) = fake(Some(VALUE)).await;
    *fake.answer.lock().unwrap() = Err(());
    let reader = live(&fake).await;
    frames.line(1, &format!("before {VALUE}\n")).await;
    let first = reader.next().await.unwrap().map(|record| record.line);
    assert!(matches!(first, Some(LogLine::Hidden)), "{first:?}");

    *fake.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    fake.names.begin().done(sealed("TOKEN"));
    frames
        .line(2, "the first line after a failed read is veiled\n")
        .await;
    frames.line(3, &format!("after {VALUE}\n")).await;
    drop(frames);
    assert_eq!(messages(&shown(&reader).await), "after [secret TOKEN]\n");
}

/// The next record's line, which the test expects to arrive.
async fn next(reader: &ScrubbedLogStream) -> LogLine {
    reader.next().await.unwrap().unwrap().line
}

#[tokio::test(start_paused = true)]
async fn a_value_begun_in_a_hidden_frame_at_stream_start_stays_hidden() {
    let (head, tail) = VALUE.split_at(10);
    let (started, [frames]) = fake(Some(VALUE)).await;
    *started.answer.lock().unwrap() = Err(());
    let reader = live(&started).await;
    frames.line(1, &format!("start TOKEN={head}")).await;
    assert!(matches!(next(&reader).await, LogLine::Hidden));
    *started.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    started.names.begin().done(sealed("TOKEN"));
    frames.line(2, &format!("{tail}\n")).await;
    frames.line(3, "the line the veil ends in\n").await;
    frames.line(4, &format!("after {VALUE}\n")).await;
    drop(frames);
    let joined = messages(&shown(&reader).await);
    assert_eq!(joined, "after [secret TOKEN]\n");
}

#[tokio::test(start_paused = true)]
async fn a_value_begun_in_a_hidden_frame_after_a_failed_read_stays_hidden() {
    let (head, tail) = VALUE.split_at(10);
    let (recovered, [frames]) = fake(Some(VALUE)).await;
    *recovered.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    let reader = live(&recovered).await;
    frames.line(1, "before\n").await;
    assert_eq!(messages(&[next(&reader).await]), "before\n");
    *recovered.answer.lock().unwrap() = Err(());
    recovered.names.begin().failed();
    frames.line(2, &format!("TOKEN={head}")).await;
    assert!(matches!(next(&reader).await, LogLine::Hidden));
    *recovered.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    recovered.names.begin().done(sealed("TOKEN"));
    frames.line(3, &format!("{tail}\n")).await;
    frames.line(4, "the line the veil ends in\n").await;
    frames.line(5, "later\n").await;
    drop(frames);
    let joined = messages(&shown(&reader).await);
    assert_eq!(joined, "later\n");
}

#[tokio::test(start_paused = true)]
async fn a_hidden_frame_on_one_channel_keeps_the_other_channel_scrubbing() {
    let (head, tail) = VALUE.split_at(10);
    let (fake, [frames]) = fake(Some(VALUE)).await;
    *fake.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    let reader = live(&fake).await;
    frames.err(1, &format!("err {head}")).await;
    frames.line(2, "out\n").await;
    assert_eq!(messages(&[next(&reader).await]), "out\n");
    *fake.answer.lock().unwrap() = Err(());
    fake.names.begin().failed();
    frames.line(3, "gone\n").await;
    assert!(matches!(next(&reader).await, LogLine::Hidden));
    *fake.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    fake.names.begin().done(sealed("TOKEN"));
    frames.err(4, &format!("{tail} done\n")).await;
    drop(frames);
    let joined = messages(&shown(&reader).await);
    assert_eq!(joined, "err [secret TOKEN] done\n");
}

#[tokio::test(start_paused = true)]
async fn a_multiline_value_begun_in_the_veil_stays_hidden() {
    let value = "abcde\nfghij\nklmno";
    let (recovered, [frames]) = fake(Some(value)).await;
    *recovered.answer.lock().unwrap() = Err(());
    let reader = live(&recovered).await;
    frames.line(1, "hidden\n").await;
    assert!(matches!(next(&reader).await, LogLine::Hidden));
    *recovered.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    recovered.names.begin().done(sealed("TOKEN"));
    frames
        .line(2, &format!("{}{value}\n", "x".repeat(18)))
        .await;
    frames.line(3, "later\n").await;
    drop(frames);
    let joined = messages(&shown(&reader).await);
    assert!(
        !joined.contains("fghij") && !joined.contains("klmno"),
        "{joined:?}"
    );
    assert_eq!(joined, "later\n");
}

#[tokio::test(start_paused = true)]
async fn a_multiline_value_begun_in_the_veil_and_ending_in_the_next_frame_stays_hidden() {
    let value = "abcde\nfghij\nklmno";
    let (recovered, [frames]) = fake(Some(value)).await;
    *recovered.answer.lock().unwrap() = Err(());
    let reader = live(&recovered).await;
    frames.line(1, "hidden\n").await;
    assert!(matches!(next(&reader).await, LogLine::Hidden));
    *recovered.answer.lock().unwrap() = Ok(sealed("TOKEN"));
    recovered.names.begin().done(sealed("TOKEN"));
    frames
        .line(2, &format!("{}abcde\nfg", "x".repeat(18)))
        .await;
    frames.line(3, "hij\nklmno\n").await;
    frames.line(4, "later\n").await;
    drop(frames);
    let joined = messages(&shown(&reader).await);
    assert!(
        !joined.contains("fghij") && !joined.contains("klmno"),
        "{joined:?}"
    );
    assert_eq!(joined, "later\n");
}

#[tokio::test(start_paused = true)]
async fn close_ends_every_names_wait() {
    let names = SealedNames::new();
    let pending = tokio::spawn({
        let names = names.clone();
        async move { names.wanted().await }
    });
    tokio::task::yield_now().await;
    names.close();
    tokio::time::timeout(std::time::Duration::from_secs(1), pending)
        .await
        .expect("a pending wait resolves on close")
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), names.wanted())
        .await
        .expect("a later wait resolves at once");
}
