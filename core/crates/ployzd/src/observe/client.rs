//! ployzd's side of `observe.sock`. Every connect and every read has a 3 s
//! deadline, and a store that does not answer is unavailable, so one Machine
//! without ployz-observe fails alone.

use std::{io, path::PathBuf, time::Duration};

use ployz_core::{LogHistoryRequest, OpaquePayload};
use tokio::{net::UnixStream, sync::mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;

use super::serve::{Request, read_frame, write_frame};
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
