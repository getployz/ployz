//! `observe.sock`: history queries and namespace forgets for ployzd.
//!
//! A request is one frame, `[u32 BE length][JSON]`. A query answers with one
//! frame per [`HistoryRow`], a heartbeat each second while it reads, and
//! [`HistoryRow::End`] last. A forget answers with one JSON frame.

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use ployz_core::{ForgetLogsRequest, HistoryRow, LogHistoryRequest, LogsForgotten};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, BufWriter},
    net::{UnixListener, UnixStream},
    sync::{Semaphore, mpsc, oneshot},
};

use super::{
    layout::StoreRoot,
    query::{self, Bounds, Query},
};

const HEARTBEAT: Duration = Duration::from_secs(1);
const REQUEST_DEADLINE: Duration = Duration::from_secs(3);
const MAX_REQUEST: usize = 64 << 10;
const ACCEPT_RETRY: Duration = Duration::from_millis(100);
/// Pages read at once. Each can hold a page's text plus the file it decodes,
/// and the harvester shares the service's memory.
const READERS: usize = 2;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Request {
    Query(LogHistoryRequest),
    Forget(ForgetLogsRequest),
}

pub(crate) type ForgetReply = Result<LogsForgotten, String>;

/// A forget for the harvester, which owns what the store holds.
pub(super) struct ForgetJob {
    pub namespace: String,
    pub reply: oneshot::Sender<io::Result<u32>>,
}

pub(super) async fn serve(
    listener: UnixListener,
    store: StoreRoot,
    forgets: mpsc::Sender<ForgetJob>,
) {
    let reading = Reading {
        store,
        bounds: Arc::new(Bounds::default()),
        readers: Arc::new(Semaphore::new(READERS)),
    };
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(answer(stream, reading.clone(), forgets.clone()));
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
}

async fn answer(mut stream: UnixStream, reading: Reading, forgets: mpsc::Sender<ForgetJob>) {
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
        Ok(Request::Forget(request)) => answer_forget(stream, forgets, request).await,
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
    let reading = {
        let cancel = Arc::clone(&cancel);
        async move {
            let permit = shared
                .readers
                .acquire_owned()
                .await
                .map_err(io::Error::other)?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                query::page(&shared.store, &query, &shared.bounds, &cancel)
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
    let page = match read {
        Ok(page) => page,
        Err(error) => {
            write_row(&mut stream, &HistoryRow::Error(error.to_string())).await?;
            return stream.flush().await;
        }
    };
    for row in &page.rows {
        write_row(&mut stream, row).await?;
    }
    write_row(&mut stream, &HistoryRow::End { next: page.next }).await?;
    stream.flush().await
}

async fn answer_forget(
    mut stream: UnixStream,
    forgets: mpsc::Sender<ForgetJob>,
    request: ForgetLogsRequest,
) -> io::Result<()> {
    let (reply, replied) = oneshot::channel();
    let job = ForgetJob {
        namespace: request.namespace,
        reply,
    };
    let answer: ForgetReply = if forgets.send(job).await.is_err() {
        Err("the harvester stopped".into())
    } else {
        match replied.await {
            Ok(Ok(containers)) => Ok(LogsForgotten { containers }),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("the harvester stopped".into()),
        }
    };
    let bytes = serde_json::to_vec(&answer).map_err(io::Error::other)?;
    write_frame(&mut stream, &bytes).await
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

    use ployz_core::{HistoryRow, LogHistoryRequest, OpaquePayload};
    use tokio::{net::UnixStream, sync::Semaphore};

    use super::{Reading, answer_query, read_frame};
    use crate::{observe::layout::StoreRoot, test_dir::TestDir};

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
}
