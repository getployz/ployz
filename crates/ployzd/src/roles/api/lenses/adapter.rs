//! Corrosion query and subscription adaptation for API lenses.

use std::time::Duration;

use async_trait::async_trait;
use ployz_core::LensCollection;
use ployz_core::corrosion::{
    ChangeId, CorrosionTable, QueryIdentity, SqliteParameter, SqliteValue, Statement, StoredRow,
};
use ployz_core::ids::ClusterName;

use crate::corrosion::{
    CorrosionClient, CorrosionClientError, StoredRowCollectionError, StoredRowLimit,
    SubscriptionStream, SubscriptionStreamEvent, collect_stored_rows,
};

/// One fixed Corrosion input that can invalidate a v2 API lens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum LensInput {
    Cluster,
    Machines,
    Namespaces,
    Endpoints,
    MachineStatus,
    Operations,
}

impl LensInput {
    pub(super) const fn inputs_for(collection: LensCollection) -> &'static [Self] {
        match collection {
            LensCollection::Machines => &[Self::Cluster, Self::Machines],
            LensCollection::Services => &[Self::Namespaces],
            LensCollection::Endpoints => &[Self::Endpoints],
            LensCollection::MachineStatus => &[Self::MachineStatus],
            LensCollection::Operations => &[Self::Operations],
        }
    }

    const fn table(self) -> CorrosionTable {
        match self {
            Self::Cluster => CorrosionTable::Cluster,
            Self::Machines => CorrosionTable::Machines,
            Self::Namespaces => CorrosionTable::Namespaces,
            Self::Endpoints => CorrosionTable::MachineEndpoints,
            Self::MachineStatus => CorrosionTable::MachineStatus,
            Self::Operations => CorrosionTable::Operations,
        }
    }

    pub(super) fn statement(self, cluster_id: &ClusterName) -> Statement {
        match self {
            Self::Cluster => Statement::with_params(
                "SELECT id, document FROM cluster WHERE id = ?",
                vec![SqliteParameter::Text(cluster_id.as_str().to_owned())],
            ),
            Self::MachineStatus => Statement::with_params(
                "SELECT machine_id AS id, document FROM machine_status WHERE json_extract(document, '$.cluster_id') = ?",
                vec![SqliteParameter::Text(cluster_id.as_str().to_owned())],
            ),
            Self::Endpoints => Statement::with_params(
                "SELECT id, document FROM machine_endpoints WHERE json_extract(document, '$.cluster_id') = ?",
                vec![SqliteParameter::Text(cluster_id.as_str().to_owned())],
            ),
            Self::Machines | Self::Namespaces | Self::Operations => Statement::with_params(
                format!(
                    "SELECT id, document FROM {} WHERE json_extract(document, '$.cluster_id') = ?",
                    self.table().as_str()
                ),
                vec![SqliteParameter::Text(cluster_id.as_str().to_owned())],
            ),
        }
    }
}

/// A bounded local Corrosion failure while collecting one lens input.
#[derive(Debug, thiserror::Error)]
pub(super) enum LensStoreError {
    #[error(transparent)]
    Corrosion(#[from] CorrosionClientError),
    #[error("Corrosion {input:?} query returned no columns frame")]
    MissingColumns { input: LensInput },
    #[error("Corrosion {input:?} query returned duplicate columns frames")]
    DuplicateColumns { input: LensInput },
    #[error("Corrosion {input:?} query returned unexpected columns: {columns:?}")]
    UnexpectedColumns {
        input: LensInput,
        columns: Vec<String>,
    },
    #[error("Corrosion {input:?} query returned a row before its columns frame")]
    RowBeforeColumns { input: LensInput },
    #[error("Corrosion {input:?} query returned an unexpected row")]
    UnexpectedRow { input: LensInput },
    #[error("Corrosion {input:?} subscription ended before its snapshot watermark")]
    SnapshotEnded { input: LensInput },
    #[error("lens full requery exceeded its {timeout:?} deadline")]
    QueryDeadline { timeout: Duration },
    #[error("lens initial subscription snapshot exceeded its {timeout:?} deadline")]
    InitialSnapshotDeadline { timeout: Duration },
}

/// An input-scoped subscription frame after the Corrosion adapter validated
/// the transport-level shape.
#[derive(Debug)]
pub(super) enum LensSubscriptionEvent {
    SnapshotRow(StoredRow),
    SnapshotEnd { watermark: ChangeId },
    Invalidation,
}

#[async_trait]
pub(super) trait LensSubscription: Send {
    fn identity(&self) -> &QueryIdentity;

    fn cursor(&self) -> Option<ChangeId>;

    async fn next(&mut self) -> Result<LensSubscriptionEvent, LensStoreError>;
}

/// The hard test seam around the machine-local Corrosion client.
///
/// The production implementation is [`CorrosionClient`]; tests use a scripted
/// fake so recovery behavior is exercised without treating a mock as a
/// Corrosion transport certification.
#[async_trait]
pub(super) trait LensStore: Send + Sync {
    async fn query_rows(
        &self,
        input: LensInput,
        cluster_id: &ClusterName,
        max_rows: usize,
    ) -> Result<Vec<StoredRow>, LensStoreError>;

    async fn subscribe(
        &self,
        input: LensInput,
        cluster_id: &ClusterName,
    ) -> Result<Box<dyn LensSubscription>, LensStoreError>;

    async fn resume(
        &self,
        input: LensInput,
        cluster_id: &ClusterName,
        identity: &QueryIdentity,
        cursor: ChangeId,
    ) -> Result<Box<dyn LensSubscription>, LensStoreError>;
}

#[async_trait]
impl LensStore for CorrosionClient {
    async fn query_rows(
        &self,
        input: LensInput,
        cluster_id: &ClusterName,
        max_rows: usize,
    ) -> Result<Vec<StoredRow>, LensStoreError> {
        let mut stream = self.query(&input.statement(cluster_id)).await?;
        collect_query_rows(input, &mut stream, StoredRowLimit::new(max_rows)).await
    }

    async fn subscribe(
        &self,
        input: LensInput,
        cluster_id: &ClusterName,
    ) -> Result<Box<dyn LensSubscription>, LensStoreError> {
        let stream = self.subscribe(&input.statement(cluster_id)).await?;
        Ok(Box::new(ClientLensSubscription::snapshot(input, stream)))
    }

    async fn resume(
        &self,
        input: LensInput,
        _cluster_id: &ClusterName,
        identity: &QueryIdentity,
        cursor: ChangeId,
    ) -> Result<Box<dyn LensSubscription>, LensStoreError> {
        let stream = self.resume(identity, cursor).await?;
        Ok(Box::new(ClientLensSubscription::live(input, stream)))
    }
}

async fn collect_query_rows(
    input: LensInput,
    stream: &mut crate::corrosion::QueryStream,
    limit: StoredRowLimit,
) -> Result<Vec<StoredRow>, LensStoreError> {
    collect_stored_rows(stream, limit)
        .await
        .map_err(|error| lens_query_error(input, error))
}

fn lens_query_error(input: LensInput, error: StoredRowCollectionError) -> LensStoreError {
    match error {
        StoredRowCollectionError::Client(source) => LensStoreError::Corrosion(source),
        StoredRowCollectionError::MissingColumns => LensStoreError::MissingColumns { input },
        StoredRowCollectionError::DuplicateColumns => LensStoreError::DuplicateColumns { input },
        StoredRowCollectionError::RowBeforeColumns => LensStoreError::RowBeforeColumns { input },
        StoredRowCollectionError::UnexpectedColumns { columns } => {
            LensStoreError::UnexpectedColumns { input, columns }
        }
        StoredRowCollectionError::UnexpectedRow { .. } => LensStoreError::UnexpectedRow { input },
        StoredRowCollectionError::RowLimit { limit } => {
            LensStoreError::Corrosion(CorrosionClientError::ResponseTooLarge { limit })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowPhase {
    AwaitingColumns,
    Rows,
}

fn validate_columns(input: LensInput, columns: &[String]) -> Result<(), LensStoreError> {
    if columns == ["id", "document"] {
        return Ok(());
    }
    Err(LensStoreError::UnexpectedColumns {
        input,
        columns: columns.to_owned(),
    })
}

fn decode_row(input: LensInput, values: Vec<SqliteValue>) -> Result<StoredRow, LensStoreError> {
    let [SqliteValue::Text(key), SqliteValue::Text(document)] = values.as_slice() else {
        return Err(LensStoreError::UnexpectedRow { input });
    };
    Ok(StoredRow::new(key.clone(), document.clone()))
}

struct ClientLensSubscription {
    input: LensInput,
    stream: SubscriptionStream,
    phase: ClientSubscriptionPhase,
}

impl ClientLensSubscription {
    fn snapshot(input: LensInput, stream: SubscriptionStream) -> Self {
        Self {
            input,
            stream,
            phase: ClientSubscriptionPhase::Snapshot(RowPhase::AwaitingColumns),
        }
    }

    fn live(input: LensInput, stream: SubscriptionStream) -> Self {
        Self {
            input,
            stream,
            phase: ClientSubscriptionPhase::Live,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientSubscriptionPhase {
    Snapshot(RowPhase),
    Live,
}

#[async_trait]
impl LensSubscription for ClientLensSubscription {
    fn identity(&self) -> &QueryIdentity {
        self.stream.identity()
    }

    fn cursor(&self) -> Option<ChangeId> {
        self.stream.last_contiguous_change_id()
    }

    async fn next(&mut self) -> Result<LensSubscriptionEvent, LensStoreError> {
        loop {
            let event = self.stream.next().await?;
            let Some(event) = event else {
                return match self.phase {
                    ClientSubscriptionPhase::Snapshot(_) => {
                        Err(LensStoreError::SnapshotEnded { input: self.input })
                    }
                    ClientSubscriptionPhase::Live => Err(LensStoreError::Corrosion(
                        CorrosionClientError::SubscriptionEnded,
                    )),
                };
            };

            match (&mut self.phase, event) {
                (
                    ClientSubscriptionPhase::Snapshot(RowPhase::AwaitingColumns),
                    SubscriptionStreamEvent::Columns(columns),
                ) => {
                    validate_columns(self.input, &columns)?;
                    self.phase = ClientSubscriptionPhase::Snapshot(RowPhase::Rows);
                }
                (
                    ClientSubscriptionPhase::Snapshot(RowPhase::Rows),
                    SubscriptionStreamEvent::Columns(_),
                ) => return Err(LensStoreError::DuplicateColumns { input: self.input }),
                (
                    ClientSubscriptionPhase::Snapshot(RowPhase::AwaitingColumns),
                    SubscriptionStreamEvent::Row(_, _),
                ) => return Err(LensStoreError::RowBeforeColumns { input: self.input }),
                (
                    ClientSubscriptionPhase::Snapshot(RowPhase::Rows),
                    SubscriptionStreamEvent::Row(_, values),
                ) => {
                    return Ok(LensSubscriptionEvent::SnapshotRow(decode_row(
                        self.input, values,
                    )?));
                }
                (
                    ClientSubscriptionPhase::Snapshot(RowPhase::AwaitingColumns),
                    SubscriptionStreamEvent::EndOfQuery(_),
                ) => return Err(LensStoreError::MissingColumns { input: self.input }),
                (
                    ClientSubscriptionPhase::Snapshot(RowPhase::Rows),
                    SubscriptionStreamEvent::EndOfQuery(end),
                ) => {
                    let Some(watermark) = end.change_id else {
                        return Err(LensStoreError::SnapshotEnded { input: self.input });
                    };
                    self.phase = ClientSubscriptionPhase::Live;
                    return Ok(LensSubscriptionEvent::SnapshotEnd { watermark });
                }
                (ClientSubscriptionPhase::Live, SubscriptionStreamEvent::Change(_, _, _, _)) => {
                    return Ok(LensSubscriptionEvent::Invalidation);
                }
                (
                    ClientSubscriptionPhase::Snapshot(_),
                    SubscriptionStreamEvent::Change(_, _, _, _),
                )
                | (ClientSubscriptionPhase::Live, SubscriptionStreamEvent::Columns(_))
                | (ClientSubscriptionPhase::Live, SubscriptionStreamEvent::Row(_, _))
                | (ClientSubscriptionPhase::Live, SubscriptionStreamEvent::EndOfQuery(_)) => {
                    return Err(LensStoreError::Corrosion(CorrosionClientError::Protocol {
                        detail: "Corrosion subscription event arrived outside its lens phase"
                            .to_owned(),
                    }));
                }
            }
        }
    }
}
