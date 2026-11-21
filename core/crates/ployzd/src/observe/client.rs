//! ployzd's side of `observe.sock`. Every connect and every read has a 3 s
//! deadline, and an observe process that does not answer is unavailable, so one
//! Machine without ployz-observe fails alone.

use std::{io, path::PathBuf};

use ployz_core::metrics::{MetricHistoryReply, MetricHistoryRequest};
use tokio::net::UnixStream;
use tonic::Status;

use super::wire::{DEADLINE, MAX_ANSWER, MetricsAnswer, Request, read_frame, write_frame};

#[derive(Clone, Debug)]
pub struct ObserveClient {
    socket: PathBuf,
}

impl ObserveClient {
    #[must_use]
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
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
        let frame = deadline(read_frame(&mut stream, MAX_ANSWER))
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
