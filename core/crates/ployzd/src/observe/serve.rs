//! The observe process's side of `observe.sock`: metric history for ployzd. The protocol is
//! in [`wire`].

use std::{io, sync::Arc, time::Duration};

use ployz_core::metrics::MetricHistoryRequest;
use tokio::{
    io::AsyncWriteExt as _,
    net::{UnixListener, UnixStream},
    sync::Semaphore,
};

use super::{
    clock::now_nanos,
    layout::StoreRoot,
    metrics::history,
    wire::{
        DEADLINE, MAX_REQUEST, METRICS_DEADLINE, MetricsAnswer, Request, read_frame, write_frame,
    },
};

const ACCEPT_RETRY: Duration = Duration::from_millis(100);
/// Metric reads at once. The process shares the service's memory with the
/// sampler and the shippers.
const READERS: usize = 2;
/// How long the answer may wait on a reader that stopped reading.
const STALL: Duration = Duration::from_secs(10);

pub(super) async fn serve(listener: UnixListener, store: StoreRoot) {
    let reading = Reading {
        store,
        readers: Arc::new(Semaphore::new(READERS)),
        stall: STALL,
    };
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(answer(stream, reading.clone()));
            }
            Err(error) => {
                tracing::warn!(%error, "cannot accept a metrics reader");
                tokio::time::sleep(ACCEPT_RETRY).await;
            }
        }
    }
}

#[derive(Clone)]
struct Reading {
    store: StoreRoot,
    readers: Arc<Semaphore>,
    stall: Duration,
}

async fn answer(mut stream: UnixStream, reading: Reading) {
    let request = match tokio::time::timeout(DEADLINE, read_frame(&mut stream, MAX_REQUEST)).await {
        Ok(Ok(Some(bytes))) => serde_json::from_slice::<Request>(&bytes),
        Ok(Ok(None)) => return,
        Ok(Err(error)) => {
            tracing::warn!(%error, "cannot read a metrics request");
            return;
        }
        Err(_) => {
            tracing::warn!("a metrics reader sent no request within 3 s");
            return;
        }
    };
    let result = match request {
        Ok(Request::Metrics(request)) => {
            answer_metrics(stream, reading, request, METRICS_DEADLINE).await
        }
        Err(error) => {
            let answer = MetricsAnswer::Invalid(format!("unreadable request: {error}"));
            write_answer(&mut stream, reading.stall, &answer).await
        }
    };
    if let Err(error) = result {
        tracing::info!(%error, "metrics reader went away");
    }
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
    write_answer(&mut stream, shared.stall, &answer).await
}

async fn write_answer(
    stream: &mut UnixStream,
    stall: Duration,
    answer: &MetricsAnswer,
) -> io::Result<()> {
    let json = serde_json::to_vec(answer).map_err(io::Error::other)?;
    within(stall, async {
        write_frame(stream, &json).await?;
        stream.flush().await
    })
    .await
}

async fn within(stall: Duration, write: impl Future<Output = io::Result<()>>) -> io::Result<()> {
    tokio::time::timeout(stall, write).await.map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("the reader took no row for {} s", stall.as_secs()),
        )
    })?
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use ployz_core::metrics::{
        MetricGap, MetricGapReason, MetricHistoryRequest, MetricPoints, MetricRange, MetricScope,
    };
    use tokio::{
        net::{UnixListener, UnixStream},
        sync::Semaphore,
    };

    use super::{Reading, STALL, answer_metrics, serve};
    use crate::observe::wire::{MetricsAnswer, read_frame};
    use crate::{
        observe::{
            ObserveClient,
            clock::now_nanos,
            layout::StoreRoot,
            metrics::history::tests::{SEC, host, near, write},
        },
        test_dir::TestDir,
    };

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
