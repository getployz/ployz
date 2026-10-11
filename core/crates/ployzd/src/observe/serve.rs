//! `observe.sock`: history queries for ployzd.
//!
//! A request is one frame, `[u32 BE length][JSON]`. A query answers with one
//! frame per [`HistoryRow`], a heartbeat each second while it reads, and
//! [`HistoryRow::End`] last. A metrics request answers with one
//! [`MetricsAnswer`] frame.

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use ployz_core::{
    HistoryRow, LogHistoryRequest,
    metrics::{MetricHistoryReply, MetricHistoryRequest},
};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, BufWriter},
    net::{UnixListener, UnixStream},
    sync::Semaphore,
};

use super::{
    harvest::now_nanos,
    layout::StoreRoot,
    metrics::history,
    query::{self, Bounds, Query},
};

const HEARTBEAT: Duration = Duration::from_secs(1);
const REQUEST_DEADLINE: Duration = Duration::from_secs(3);
const MAX_REQUEST: usize = 64 << 10;
const ACCEPT_RETRY: Duration = Duration::from_millis(100);
/// Pages read at once. Each can hold a page's text plus the file it decodes,
/// and the harvester shares the service's memory.
const READERS: usize = 2;
/// How long one written row may wait on a reader that stopped reading. Its
/// page keeps a reader until it is written.
const STALL: Duration = Duration::from_secs(10);
/// How long a metrics read may take, inside the client's 3 s deadline.
const METRICS_DEADLINE: Duration = Duration::from_millis(2_500);

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Request {
    Query(LogHistoryRequest),
    Metrics(MetricHistoryRequest),
}

/// The one frame a metrics request is answered with.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MetricsAnswer {
    Reply(MetricHistoryReply),
    /// The request is outside the contract.
    Invalid(String),
    /// The store could not be read.
    Failed(String),
}

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
    let request =
        match tokio::time::timeout(REQUEST_DEADLINE, read_frame(&mut stream, MAX_REQUEST)).await {
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

pub(crate) async fn write_frame(
    stream: &mut (impl AsyncWrite + Unpin),
    bytes: &[u8],
) -> io::Result<()> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame over 4 GiB"))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(bytes).await
}

/// `None` when the peer closed before a new frame began.
pub(crate) async fn read_frame(
    stream: &mut (impl AsyncRead + Unpin),
    max: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0; 4];
    match stream.read_exact(&mut len).await {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        result => result?,
    };
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes is over the {max} byte limit"),
        ));
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await?;
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use ployz_core::{
        HistoryRow, LogHistoryRequest, OpaquePayload,
        metrics::{
            MetricGap, MetricGapReason, MetricHistoryRequest, MetricPoints, MetricRange,
            MetricScope,
        },
    };
    use tokio::{
        net::{UnixListener, UnixStream},
        sync::Semaphore,
    };

    use super::{MetricsAnswer, Reading, STALL, answer_metrics, answer_query, read_frame, serve};
    use crate::{
        observe::{
            ObserveClient,
            harvest::now_nanos,
            layout::StoreRoot,
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

    #[tokio::test]
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
