use ployz_core::corrosion::CorrosionTable;
use ployz_core::ids::{MachineRowId, PeerId};

use crate::corrosion::{CorrosionClientError, StoredRowCollectionError};

#[derive(Debug, thiserror::Error)]
pub(in crate::roles::api::http) enum MutationStoreError {
    #[error("local Corrosion request failed: {0}")]
    Client(#[from] CorrosionClientError),
    #[error("stored-row collection failed: {0}")]
    StoredRows(#[from] StoredRowCollectionError),
    #[error("accepted cluster row is missing")]
    MissingCluster,
    #[error("accepted cluster row is invalid or ambiguous")]
    InvalidCluster,
    #[error("accepted {table:?} row has an invalid id: {detail}")]
    InvalidAcceptedId {
        table: CorrosionTable,
        detail: String,
    },
    #[error("accepted {table:?} shadow could not be decoded: {detail}")]
    InvalidAcceptedShadow {
        table: CorrosionTable,
        detail: String,
    },
    #[error("{table:?} contains duplicate primary key {id}")]
    DuplicatePrimaryKey { table: CorrosionTable, id: String },
    #[error("could not encode {table:?} document: {detail}")]
    Encode {
        table: CorrosionTable,
        detail: String,
    },
    #[error("unexpected {table:?} write result for {id}: {detail}")]
    UnexpectedWriteResult {
        table: CorrosionTable,
        id: String,
        detail: String,
    },
    #[error("machine {machine_id} changed while its endpoint mutation was being committed")]
    ConcurrentMachineMutation { machine_id: MachineRowId },
    #[error("peer {peer_id} changed while its removal was being committed")]
    ConcurrentPeerMutation { peer_id: PeerId },
}
