//! The store's side of `observe.sock`: history queries for ployzd. The protocol is in
//! [`wire`]. A query answers with one frame per [`HistoryRow`], a heartbeat each second
//! while it reads, and [`HistoryRow::End`] last.

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use ployz_core::{ContainerId, HistoryRow, LogHistoryRequest, metrics::MetricHistoryRequest};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt as _, BufWriter},
    net::{UnixListener, UnixStream},
    sync::Semaphore,
};

use super::{
    clock::now_nanos,
    layout::{RetainedEnv, StoreRoot},
    metrics::history,
    query::{self, Bounds, Query},
    wire::{
        DEADLINE, EnvAnswer, MAX_REQUEST, METRICS_DEADLINE, MetricsAnswer, Request, read_frame,
        write_frame,
    },
};

const HEARTBEAT: Duration = Duration::from_secs(1);
const ACCEPT_RETRY: Duration = Duration::from_millis(100);
/// Pages read at once. Each can hold a page's text plus the file it decodes,
/// and the harvester shares the service's memory.
const READERS: usize = 2;
/// How long one written row may wait on a reader that stopped reading. Its
/// page keeps a reader until it is written.
const STALL: Duration = Duration::from_secs(10);

pub(super) async fn serve(listener: UnixListener, store: StoreRoot) {
    let reading = Reading {
        store,
        bounds: Arc::new(Bounds::default()),
        readers: Arc::new(Semaphore::new(READERS)),
        stall: STALL,
    };
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(answer(stream, reading.clone()));
            }
            Err(error) => {
                tracing::warn!(%error, "cannot accept a history reader");
                tokio::time::sleep(ACCEPT_RETRY).await;
            }
        }
    }
}

/// What every query shares.
#[derive(Clone)]
struct Reading {
    store: StoreRoot,
    bounds: Arc<Bounds>,
    readers: Arc<Semaphore>,
    stall: Duration,
}

async fn answer(mut stream: UnixStream, reading: Reading) {
    let request = match tokio::time::timeout(DEADLINE, read_frame(&mut stream, MAX_REQUEST)).await {
        Ok(Ok(Some(bytes))) => serde_json::from_slice::<Request>(&bytes),
        Ok(Ok(None)) => return,
        Ok(Err(error)) => {
            tracing::warn!(%error, "cannot read a history request");
            return;
        }
        Err(_) => {
            tracing::warn!("a history reader sent no request within 3 s");
            return;
        }
    };
    let result = match request {
        Ok(Request::Query(request)) => answer_query(stream, reading, request).await,
        Ok(Request::Metrics(request)) => {
            answer_metrics(stream, reading, request, METRICS_DEADLINE).await
        }
        Ok(Request::Env(container)) => answer_env(stream, &reading.store, container).await,
        Err(error) => {
            let row = HistoryRow::Error(format!("unreadable history request: {error}"));
            write_row(&mut stream, &row).await
        }
    };
    if let Err(error) = result {
        tracing::info!(%error, "history reader went away");
    }
}

async fn answer_query(
    stream: UnixStream,
    shared: Reading,
    request: LogHistoryRequest,
) -> io::Result<()> {
    let mut stream = BufWriter::new(stream);
    let query = match Query::new(request) {
        Ok(query) => query,
        Err(error) => {
            write_row(&mut stream, &HistoryRow::Error(error)).await?;
            return stream.flush().await;
        }
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let stall = shared.stall;
    let reading = {
        let cancel = Arc::clone(&cancel);
        async move {
            let permit = shared
                .readers
                .acquire_owned()
                .await
                .map_err(io::Error::other)?;
            tokio::task::spawn_blocking(move || {
                let page = query::page(&shared.store, &query, &shared.bounds, &cancel);
                (page, permit)
            })
            .await
            .map_err(io::Error::other)
        }
    };
    tokio::pin!(reading);
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.tick().await;
    let read = loop {
        tokio::select! {
            read = &mut reading => break read?,
            _ = heartbeat.tick() => {
                let beat = async {
                    write_row(&mut stream, &HistoryRow::Heartbeat).await?;
                    stream.flush().await
                };
                if let Err(error) = beat.await {
                    cancel.store(true, Ordering::Relaxed);
                    // The read stops at its next check and frees its reader then.
                    tracing::info!(%error, "history reader went away; stopping the read");
                    return Ok(());
                }
            }
        }
    };
    // The permit lives until the page is written or dropped.
    let (page, _permit) = read;
    let page = match page {
        Ok(page) => page,
        Err(error) => {
            write_row(&mut stream, &HistoryRow::Error(error.to_string())).await?;
            return stream.flush().await;
        }
    };
    let end = HistoryRow::End { next: page.next };
    for row in page.rows.iter().chain([&end]) {
        within(stall, write_row(&mut stream, row)).await?;
    }
    within(stall, stream.flush()).await
}

async fn answer_env(
    mut stream: UnixStream,
    store: &StoreRoot,
    container: ContainerId,
) -> io::Result<()> {
    let dir = store.container(&container);
    let read = tokio::task::spawn_blocking(move || RetainedEnv::read(&dir))
        .await
        .map_err(io::Error::other)
        .and_then(std::convert::identity);
    let answer = match read {
        Ok(env) => EnvAnswer::Reply(env),
        Err(error) => EnvAnswer::Failed(error.to_string()),
    };
    let bytes = serde_json::to_vec(&answer).map_err(io::Error::other)?;
    write_frame(&mut stream, &bytes).await?;
    stream.flush().await
}

/// Answers within `deadline`, waiting for a reader included. A read past it
/// is answered with a `timeout` gap and finishes in the background.
async fn answer_metrics(
    mut stream: UnixStream,
    shared: Reading,
    request: MetricHistoryRequest,
    deadline: Duration,
) -> io::Result<()> {
    let answer = match request.validate() {
        Err(error) => MetricsAnswer::Invalid(error),
        Ok(()) => {
            let now = now_nanos();
            let reading = {
                let readers = Arc::clone(&shared.readers);
                let store = shared.store.clone();
                let request = request.clone();
                async move {
                    let permit = readers.acquire_owned().await.map_err(io::Error::other)?;
                    tokio::task::spawn_blocking(move || {
                        let reply = history::history(&store, &request, now);
                        drop(permit);
                        reply
                    })
                    .await
                    .map_err(io::Error::other)?
                }
            };
            match tokio::time::timeout(deadline, reading).await {
                Ok(Ok(reply)) => MetricsAnswer::Reply(reply),
                Ok(Err(error)) => MetricsAnswer::Failed(error.to_string()),
                Err(_) => {
                    tracing::warn!("a metrics read took over {} ms", deadline.as_millis());
                    MetricsAnswer::Reply(history::timed_out(&request, now))
                }
            }
        }
    };
    let json = serde_json::to_vec(&answer).map_err(io::Error::other)?;
    within(shared.stall, write_frame(&mut stream, &json)).await
}

async fn within(stall: Duration, write: impl Future<Output = io::Result<()>>) -> io::Result<()> {
    tokio::time::timeout(stall, write).await.map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("the reader took no row for {} s", stall.as_secs()),
        )
    })?
}

async fn write_row(stream: &mut (impl AsyncWrite + Unpin), row: &HistoryRow) -> io::Result<()> {
    let payload = row.encode().map_err(io::Error::other)?;
    write_frame(stream, &payload.json).await
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use ployz_core::{
        ContainerId, HistoryRow, LogHistoryRequest, OpaquePayload,
        metrics::{
            MetricGap, MetricGapReason, MetricHistoryRequest, MetricPoints, MetricRange,
            MetricScope,
        },
    };
    use tokio::{
        net::{UnixListener, UnixStream},
        sync::Semaphore,
    };

    use super::{Reading, STALL, answer_metrics, answer_query, serve};
    use crate::observe::wire::{MetricsAnswer, read_frame};
    use crate::{
        observe::{
            ObserveClient,
            clock::now_nanos,
            layout::{RetainedEnv, StoreRoot},
            metrics::history::tests::{SEC, host, near, write},
            query::tests::{Store, T0, line},
        },
        test_dir::TestDir,
    };

    #[tokio::test]
    async fn a_query_waits_for_a_free_reader() {
        let dir = TestDir::new("ployzd-observe-serve");
        std::fs::create_dir_all(&dir.0).unwrap();
        let store = StoreRoot::under(&dir.0);
        store.prepare().unwrap();
        let readers = Arc::new(Semaphore::new(0));
        let reading = Reading {
            store,
            bounds: Arc::default(),
            readers: Arc::clone(&readers),
            stall: STALL,
        };
        let (mut ours, theirs) = UnixStream::pair().unwrap();
        let request = LogHistoryRequest {
            namespace: Some("prod".into()),
            limit: 10,
            ..LogHistoryRequest::default()
        };
        let answering = tokio::spawn(answer_query(theirs, reading, request));

        let early =
            tokio::time::timeout(Duration::from_millis(300), read_frame(&mut ours, 1 << 20)).await;
        assert!(early.is_err(), "answered with every reader busy");

        readers.add_permits(1);
        loop {
            let json = read_frame(&mut ours, 1 << 20).await.unwrap().unwrap();
            let row = HistoryRow::decode(&OpaquePayload { json }).unwrap();
            match row {
                HistoryRow::Heartbeat => {}
                HistoryRow::End { next } => {
                    assert_eq!(next, None);
                    break;
                }
                other @ (HistoryRow::Container(_)
                | HistoryRow::Line { .. }
                | HistoryRow::Gap { .. }
                | HistoryRow::Exit { .. }
                | HistoryRow::Error(_)) => panic!("unexpected {other:?}"),
            }
        }
        answering.await.unwrap().unwrap();
        assert_eq!(readers.available_permits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_page_keeps_its_reader_until_it_is_written() {
        let store = Store::new();
        let id = store.container('a', "web");
        let text = "x".repeat(64 << 10);
        let frames: Vec<_> = (0..64).map(|i| line(T0 + i, &text)).collect();
        store.file(&id, 0, &frames);
        let readers = Arc::new(Semaphore::new(1));
        let reading = Reading {
            store: store.root.clone(),
            bounds: Arc::default(),
            readers: Arc::clone(&readers),
            stall: Duration::from_secs(2),
        };
        let (mut ours, theirs) = UnixStream::pair().unwrap();
        let request = LogHistoryRequest {
            namespace: Some("prod".into()),
            limit: 100,
            ..LogHistoryRequest::default()
        };
        let answering = tokio::spawn(answer_query(theirs, reading, request));

        // The first row means the page is read; its 4 MiB outgrow the socket.
        read_frame(&mut ours, 1 << 20).await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            readers.available_permits(),
            0,
            "a written page let go of its reader"
        );

        let error = tokio::time::timeout(Duration::from_secs(10), answering)
            .await
            .expect("a stalled reader kept its page")
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(readers.available_permits(), 1);
        drop(ours);
    }

    fn machine_request(since: i64, until: i64) -> MetricHistoryRequest {
        MetricHistoryRequest {
            scope: MetricScope::Machine,
            range: MetricRange {
                since_nanos: since,
                until_nanos: until,
            },
            step_nanos: Some(60 * SEC),
        }
    }

    #[tokio::test]
    async fn a_metrics_query_through_the_socket_returns_rates() {
        let dir = TestDir::new("ployzd-observe-metrics");
        std::fs::create_dir_all(&dir.0).unwrap();
        let store = StoreRoot::under(&dir.0);
        store.prepare().unwrap();
        let now = now_nanos();
        write(&store, now - 10 * 60 * SEC, now, |ts| vec![host(ts)]);
        let socket = dir.0.join("observe.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let serving = tokio::spawn(serve(listener, store));

        let client = ObserveClient::new(socket);
        let reply = client
            .metrics(machine_request(now - 5 * 60 * SEC, now))
            .await
            .unwrap();

        let MetricPoints::Machine(points) = reply.points else {
            panic!("expected Machine points");
        };
        assert_eq!(reply.step_nanos, 60 * SEC);
        assert!(points.len() >= 4, "{points:?}");
        for point in &points {
            assert!(near(point.cpu_busy, 0.25), "{point:?}");
        }

        let error = client.metrics(machine_request(now, now)).await.unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        serving.abort();
    }

    // Catches a store that answers a retained env for a Container it never held one for,
    // or cannot hand back the one it kept.
    #[tokio::test]
    async fn a_retained_env_query_through_the_socket_answers_what_the_store_kept() {
        let dir = TestDir::new("ployzd-observe-env");
        std::fs::create_dir_all(&dir.0).unwrap();
        let store = StoreRoot::under(&dir.0);
        store.prepare().unwrap();
        let kept = ContainerId::parse("a".repeat(64)).unwrap();
        let never = ContainerId::parse("b".repeat(64)).unwrap();
        let env =
            RetainedEnv::from_inspected(&["TOKEN=live-only-sentinel".to_owned()], None).unwrap();
        std::fs::create_dir_all(store.container(&kept)).unwrap();
        env.write(&store.container(&kept)).unwrap();
        let socket = dir.0.join("observe.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let serving = tokio::spawn(serve(listener, store));

        let client = ObserveClient::new(socket);
        assert_eq!(client.retained_env(kept).await.unwrap(), Some(env));
        assert_eq!(client.retained_env(never).await.unwrap(), None);
        serving.abort();
    }

    #[tokio::test]
    async fn a_metrics_read_past_its_deadline_is_a_timeout_gap() {
        let dir = TestDir::new("ployzd-observe-metrics");
        std::fs::create_dir_all(&dir.0).unwrap();
        let store = StoreRoot::under(&dir.0);
        store.prepare().unwrap();
        let reading = Reading {
            store,
            bounds: Arc::default(),
            readers: Arc::new(Semaphore::new(0)),
            stall: STALL,
        };
        let (mut ours, theirs) = UnixStream::pair().unwrap();
        let request = machine_request(0, 3_600 * SEC);
        let deadline = Duration::from_millis(100);

        answer_metrics(theirs, reading, request, deadline)
            .await
            .unwrap();

        let frame = read_frame(&mut ours, 1 << 20).await.unwrap().unwrap();
        let MetricsAnswer::Reply(reply) = serde_json::from_slice(&frame).unwrap() else {
            panic!("expected a reply");
        };
        assert_eq!(reply.points, MetricPoints::Machine(Vec::new()));
        assert_eq!(
            reply.gaps,
            [MetricGap {
                from: 0,
                to: 3_600 * SEC,
                reason: MetricGapReason::Timeout
            }]
        );
    }
}
