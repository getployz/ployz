//! ployzd's side of `observe.sock`. Every connect and every read has a 3 s
//! deadline, and a store that does not answer is unavailable, so one Machine
//! without ployz-observe fails alone.

use std::{io, path::PathBuf, time::Duration};

use ployz_core::{
    LogHistoryRequest, OpaquePayload,
    metrics::{MetricHistoryReply, MetricHistoryRequest},
};
use tokio::{net::UnixStream, sync::mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;

use super::serve::{MetricsAnswer, Request, read_frame, write_frame};
use crate::logs::RpcStream;

const DEADLINE: Duration = Duration::from_secs(3);
/// A stored line is at most 8 MiB; the rest is the row header.
const MAX_ROW: usize = 9 << 20;

#[derive(Clone, Debug)]
pub struct ObserveClient {
    socket: PathBuf,
}

impl ObserveClient {
    #[must_use]
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    /// Streams one history page, row by row, as the store sends it.
    ///
    /// # Errors
    ///
    /// Returns `Unavailable` when ployz-observe does not take the request
    /// within 3 s. The stream ends with `Unavailable` when it stops answering.
    pub async fn history(&self, request: LogHistoryRequest) -> Result<RpcStream, Status> {
        let mut stream = self.send(&Request::Query(request)).await?;
        let (rows, receiver) = mpsc::channel(256);
        tokio::spawn(async move {
            loop {
                let row = match deadline(read_frame(&mut stream, MAX_ROW)).await {
                    Ok(Some(row)) => Ok(OpaquePayload::new(row)),
                    Ok(None) => return,
                    Err(status) => Err(status),
                };
                let failed = row.is_err();
                if rows.send(row).await.is_err() || failed {
                    return;
                }
            }
        });
        Ok(ReceiverStream::new(receiver))
    }

    /// Reads one metric history. A read the store cannot finish within 3 s
    /// comes back as a `timeout` gap.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a request outside the contract, and
    /// `Unavailable` when ployz-observe does not answer within 3 s or cannot
    /// read its store.
    pub async fn metrics(
        &self,
        request: MetricHistoryRequest,
    ) -> Result<MetricHistoryReply, Status> {
        let mut stream = self.send(&Request::Metrics(request)).await?;
        let frame = deadline(read_frame(&mut stream, MAX_ROW))
            .await?
            .ok_or_else(|| Status::unavailable("ployz-observe closed without answering"))?;
        match serde_json::from_slice(&frame) {
            Ok(MetricsAnswer::Reply(reply)) => Ok(reply),
            Ok(MetricsAnswer::Invalid(error)) => Err(Status::invalid_argument(error)),
            Ok(MetricsAnswer::Failed(error)) => Err(Status::unavailable(format!(
                "ployz-observe cannot read its metrics: {error}"
            ))),
            Err(error) => Err(Status::internal(format!(
                "unreadable metrics answer: {error}"
            ))),
        }
    }

    async fn send(&self, request: &Request) -> Result<UnixStream, Status> {
        let bytes = serde_json::to_vec(request)
            .map_err(|error| Status::internal(format!("cannot encode request: {error}")))?;
        let mut stream = deadline(UnixStream::connect(&self.socket)).await?;
        deadline(write_frame(&mut stream, &bytes)).await?;
        Ok(stream)
    }
}

async fn deadline<T>(work: impl Future<Output = io::Result<T>>) -> Result<T, Status> {
    match tokio::time::timeout(DEADLINE, work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(Status::unavailable(format!(
            "ployz-observe is not answering: {error}"
        ))),
        Err(_) => Err(Status::unavailable(
            "ployz-observe did not answer within 3 s",
        )),
    }
}
