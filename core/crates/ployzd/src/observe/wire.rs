//! The protocol of `observe.sock`, which ployzd and the separately running Observe Store
//! speak and either can be a release behind the other.
//!
//! A request is one frame, `[u32 BE length][JSON]`. A query is answered with one frame
//! per [`HistoryRow`](ployz_core::HistoryRow), a heartbeat each second while it reads, and
//! `HistoryRow::End` last. An env request is answered with one [`EnvAnswer`] frame.

use std::{io, time::Duration};

use ployz_core::{ContainerId, LogHistoryRequest};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use super::layout::RetainedEnv;

/// How long either end waits to connect, to be sent a request, or to be sent an answer
/// frame.
pub(super) const DEADLINE: Duration = Duration::from_secs(3);
pub(super) const MAX_REQUEST: usize = 64 << 10;
/// A stored line is at most 8 MiB.
const MAX_STORED_LINE: usize = 8 << 20;
/// What a row adds around its stored line.
const ROW_HEADER: usize = 1 << 20;
pub(super) const MAX_ROW: usize = MAX_STORED_LINE + ROW_HEADER;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Request {
    Query(LogHistoryRequest),
    /// The env the store kept for one Container.
    Env(ContainerId),
}

/// The one frame an env request is answered with.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum EnvAnswer {
    /// `None` when the store kept no env for the Container.
    Reply(Option<RetainedEnv>),
    /// The store could not be read.
    Failed(String),
}

pub(super) async fn write_frame(
    stream: &mut (impl AsyncWrite + Unpin),
    bytes: &[u8],
) -> io::Result<()> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame over 4 GiB"))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(bytes).await
}

/// `None` when the peer closed before a new frame began.
pub(super) async fn read_frame(
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
    use ployz_core::{ContainerId, LogHistoryRequest};

    use super::{DEADLINE, EnvAnswer, MAX_ROW, Request, read_frame, write_frame};

    #[test]
    fn the_deadlines_and_limits_are_the_ones_a_release_behind_still_speaks() {
        assert_eq!(DEADLINE.as_millis(), 3_000);
        assert_eq!(MAX_ROW, 9 << 20);
    }

    #[test]
    fn requests_and_answers_keep_their_json_shape() {
        let container = ContainerId::parse("ab".repeat(32)).unwrap();
        for (json, shape) in [
            (
                serde_json::to_string(&Request::Env(container)).unwrap(),
                r#"{"env":"abababababababababababababababababababababababababababababababab"}"#,
            ),
            (
                serde_json::to_string(&Request::Query(LogHistoryRequest::default())).unwrap(),
                r#"{"query":{"namespace":null,"service":null,"deployment":null,"container_id":null,"since_nanos":null,"until_nanos":null,"direction":"forward","limit":0,"cursor":null}}"#,
            ),
            (
                serde_json::to_string(&EnvAnswer::Reply(None)).unwrap(),
                r#"{"reply":null}"#,
            ),
            (
                serde_json::to_string(&EnvAnswer::Failed("x".into())).unwrap(),
                r#"{"failed":"x"}"#,
            ),
        ] {
            assert_eq!(json, shape);
        }
    }

    #[tokio::test]
    async fn a_frame_is_a_big_endian_length_then_the_bytes() {
        let mut framed = Vec::new();
        write_frame(&mut framed, b"{}").await.unwrap();
        assert_eq!(framed, [0, 0, 0, 2, 123, 125]);
        let mut read = framed.as_slice();
        assert_eq!(
            read_frame(&mut read, 2).await.unwrap(),
            Some(b"{}".to_vec())
        );
        assert_eq!(read_frame(&mut read, 2).await.unwrap(), None);
        let mut over = framed.as_slice();
        let error = read_frame(&mut over, 1).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
}
