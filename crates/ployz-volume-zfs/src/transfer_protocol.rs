use std::net::IpAddr;

use ployz_model::MachineId;
use ployz_spec::Namespace;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

const VOLUME_NOT_AUTHORIZED: &str = "zfs transfer not authorized";

pub const MAX_HEADER_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZfsTransferOpen {
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
pub struct ZfsTransferReceived {
    pub ok: bool,
    pub snapshot_guid: Option<u64>,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ZfsReceiveRequest {
    namespace: Namespace,
    volume: String,
    snapshot: String,
    expected_guid: u64,
    from_snapshot: Option<String>,
    from_snapshot_guid: Option<u64>,
}

impl ZfsReceiveRequest {
    #[must_use]
    pub fn from_authorized_open(open: &ZfsTransferOpen, namespace: Namespace) -> Self {
        Self {
            namespace,
            volume: open.volume.clone(),
            snapshot: open.snapshot.clone(),
            expected_guid: open.expected_guid,
            from_snapshot: open.from_snapshot.clone(),
            from_snapshot_guid: open.from_snapshot_guid,
        }
    }

    #[must_use]
    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    #[must_use]
    pub fn volume(&self) -> &str {
        &self.volume
    }

    #[must_use]
    pub fn snapshot(&self) -> &str {
        &self.snapshot
    }

    #[must_use]
    pub fn expected_guid(&self) -> u64 {
        self.expected_guid
    }

    #[must_use]
    pub fn from_snapshot(&self) -> Option<&str> {
        self.from_snapshot.as_deref()
    }

    #[must_use]
    pub fn from_snapshot_guid(&self) -> Option<u64> {
        self.from_snapshot_guid
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ZfsTransferValidationError {
    #[error("connection closed before zfs transfer header")]
    HeaderClosed,
    #[error("zfs transfer header exceeded {max_bytes} bytes")]
    HeaderTooLarge { max_bytes: usize },
    #[error("zfs transfer header missing newline")]
    HeaderMissingNewline,
    #[error("zfs transfer header was not UTF-8: {message}")]
    HeaderNotUtf8 { message: String },
    #[error("zfs transfer header missing source_machine_id")]
    MissingSourceMachineId,
    #[error("list machines for zfs transfer source validation: {message}")]
    SourceMachineLookupFailed { message: String },
    #[error("source machine '{machine_id}' not found")]
    SourceMachineNotFound { machine_id: MachineId },
    #[error("zfs transfer source '{machine_id}' connected from {actual}, expected {expected}")]
    SourceOverlayIpMismatch {
        machine_id: MachineId,
        actual: IpAddr,
        expected: IpAddr,
    },
    #[error("{VOLUME_NOT_AUTHORIZED}")]
    InvalidNamespace { message: String },
    #[error("{VOLUME_NOT_AUTHORIZED}")]
    VolumeNotAuthorized {
        namespace: Namespace,
        volume: String,
        source_machine_id: MachineId,
        reason: VolumeAuthorizationRejection,
    },
    #[error(
        "snapshot '{dataset}@{snapshot}' already exists on target with guid {actual_guid}, source claims {expected_guid}"
    )]
    ExistingSnapshotGuidMismatch {
        dataset: String,
        snapshot: String,
        actual_guid: u64,
        expected_guid: u64,
    },
    #[error("incremental base snapshot '{dataset}@{snapshot}' missing on target")]
    IncrementalBaseMissing { dataset: String, snapshot: String },
    #[error(
        "incremental base snapshot '{dataset}@{snapshot}' guid {actual_guid} did not match source {expected_guid}"
    )]
    IncrementalBaseGuidMismatch {
        dataset: String,
        snapshot: String,
        actual_guid: u64,
        expected_guid: u64,
    },
    #[error("{operation}: {message}")]
    Backend {
        operation: &'static str,
        message: String,
    },
}

#[derive(Debug)]
pub enum VolumeAuthorizationRejection {
    OwnedByOtherMachine { owner: MachineId },
    VolumeNotFound,
    LookupFailed { message: String },
}

impl ZfsTransferValidationError {
    pub fn backend(operation: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Backend {
            operation,
            message: error.to_string(),
        }
    }
}

pub async fn read_transfer_header<R: AsyncBufRead + Unpin>(
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
    use crate::ZfsTransferValidationError;

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
