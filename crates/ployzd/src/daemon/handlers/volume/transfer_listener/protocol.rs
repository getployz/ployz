use ployz_model::MachineId;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

use super::validation::ZfsTransferValidationError;

pub(super) const MAX_HEADER_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ZfsTransferOpen {
    pub namespace: String,
    pub volume: String,
    pub snapshot: String,
    pub expected_guid: u64,
    /// Identifier the source daemon claims for itself. The receiver validates
    /// it against the remote overlay address before accepting the stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_machine_id: Option<MachineId>,
    /// Set when the source is sending an incremental stream. The receiver
    /// requires the named base snapshot to already exist on the target with
    /// the matching GUID before piping into `zfs recv`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_snapshot_guid: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ZfsTransferReceived {
    pub ok: bool,
    pub snapshot_guid: Option<u64>,
    pub message: String,
}

pub(super) async fn read_transfer_header<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<String, ZfsTransferValidationError> {
    let mut header = Vec::new();
    let bytes_read = {
        let mut limited = reader.take((MAX_HEADER_BYTES + 1) as u64);
        limited
            .read_until(b'\n', &mut header)
            .await
            .map_err(|error| ZfsTransferValidationError::backend("read_transfer_header", error))?
    };
    if bytes_read == 0 {
        return Err(ZfsTransferValidationError::HeaderClosed);
    }
    // take() lets us read up to MAX_HEADER_BYTES of content plus the trailing
    // newline. If we hit that limit without finding `\n`, the content itself
    // already exceeded the limit; otherwise the read terminated early.
    if header.last() != Some(&b'\n') {
        if header.len() > MAX_HEADER_BYTES {
            return Err(ZfsTransferValidationError::HeaderTooLarge {
                max_bytes: MAX_HEADER_BYTES,
            });
        }
        return Err(ZfsTransferValidationError::HeaderMissingNewline);
    }
    header.pop();
    String::from_utf8(header).map_err(|error| ZfsTransferValidationError::HeaderNotUtf8 {
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{MAX_HEADER_BYTES, read_transfer_header};
    use crate::daemon::handlers::volume::transfer_listener::validation::ZfsTransferValidationError;

    #[tokio::test]
    async fn read_transfer_header_rejects_oversized_header() {
        let mut body = vec![b'a'; MAX_HEADER_BYTES + 1];
        body.push(b'\n');
        let mut reader = tokio::io::BufReader::new(body.as_slice());

        let err = read_transfer_header(&mut reader)
            .await
            .expect_err("oversized header rejected");
        assert!(matches!(
            err,
            ZfsTransferValidationError::HeaderTooLarge {
                max_bytes: MAX_HEADER_BYTES
            }
        ));
    }

    #[tokio::test]
    async fn read_transfer_header_rejects_missing_newline() {
        let body = b"{\"namespace\":\"default\"}".as_slice();
        let mut reader = tokio::io::BufReader::new(body);

        let err = read_transfer_header(&mut reader)
            .await
            .expect_err("unterminated header rejected");
        assert!(matches!(
            err,
            ZfsTransferValidationError::HeaderMissingNewline
        ));
    }
}
