//! Bounded Corrosion inputs for the DNS projection.

use std::time::Duration;

use futures_util::future::FutureExt;
use futures_util::stream::{FuturesUnordered, StreamExt};
use ployz_core::corrosion::{CorrosionTable, SqliteParameter, Statement, StoredRow};
use ployz_core::ids::ClusterName;

use crate::corrosion::{
    CorrosionClient, CorrosionClientError, StoredRowCollectionError, StoredRowLimit,
    SubscriptionStream, SubscriptionStreamEvent, collect_stored_rows,
};

const MAX_ROWS_PER_INPUT: usize = 16_384;
const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(30);
const QUERY_DEADLINE: Duration = Duration::from_secs(30);
const MAX_COALESCED_INVALIDATIONS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DnsInput {
    Cluster,
    Machines,
    Namespaces,
    Services,
    Containers,
}

impl DnsInput {
    const ALL: [Self; 5] = [
        Self::Cluster,
        Self::Machines,
        Self::Namespaces,
        Self::Services,
        Self::Containers,
    ];

    const fn table(self) -> CorrosionTable {
        match self {
            Self::Cluster => CorrosionTable::Cluster,
            Self::Machines => CorrosionTable::Machines,
            Self::Namespaces => CorrosionTable::Namespaces,
            Self::Services => CorrosionTable::Services,
            Self::Containers => CorrosionTable::Containers,
        }
    }

    fn statement(self, cluster_id: &ClusterName) -> Statement {
        match self {
            Self::Cluster => Statement::with_params(
                "SELECT id, document FROM cluster WHERE id = ?",
                vec![SqliteParameter::Text(cluster_id.as_str().to_owned())],
            ),
            Self::Machines | Self::Namespaces | Self::Services | Self::Containers => {
                Statement::with_params(
                    format!(
                        "SELECT id, document FROM {} WHERE json_extract(document, '$.cluster_id') = ?",
                        self.table().as_str()
                    ),
                    vec![SqliteParameter::Text(cluster_id.as_str().to_owned())],
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DnsRows {
    pub cluster: Vec<StoredRow>,
    pub machines: Vec<StoredRow>,
    pub namespaces: Vec<StoredRow>,
    pub services: Vec<StoredRow>,
    pub containers: Vec<StoredRow>,
}

pub(super) struct CorrosionDnsSource {
    client: CorrosionClient,
    cluster_id: ClusterName,
}

impl CorrosionDnsSource {
    #[must_use]
    pub(super) const fn new(client: CorrosionClient, cluster_id: ClusterName) -> Self {
        Self { client, cluster_id }
    }

    /// Opens every invalidation stream before any authoritative re-query.
    pub(super) async fn subscribe(&self) -> Result<DnsSubscriptions, DnsSourceError> {
        tokio::time::timeout(SNAPSHOT_DEADLINE, async {
            let mut streams = Vec::with_capacity(DnsInput::ALL.len());
            for input in DnsInput::ALL {
                let mut stream = self
                    .client
                    .subscribe(&input.statement(&self.cluster_id))
                    .await?;
                drain_snapshot(input, &mut stream).await?;
                streams.push((input, stream));
            }
            Ok(streams)
        })
        .await
        .map_err(|_| DnsSourceError::SnapshotDeadline {
            timeout: SNAPSHOT_DEADLINE,
        })?
        .map(|streams| DnsSubscriptions { streams })
    }

    pub(super) async fn query_rows(&self) -> Result<DnsRows, DnsSourceError> {
        tokio::time::timeout(QUERY_DEADLINE, async {
            Ok(DnsRows {
                cluster: self.query(DnsInput::Cluster).await?,
                machines: self.query(DnsInput::Machines).await?,
                namespaces: self.query(DnsInput::Namespaces).await?,
                services: self.query(DnsInput::Services).await?,
                containers: self.query(DnsInput::Containers).await?,
            })
        })
        .await
        .map_err(|_| DnsSourceError::QueryDeadline {
            timeout: QUERY_DEADLINE,
        })?
    }

    async fn query(&self, input: DnsInput) -> Result<Vec<StoredRow>, DnsSourceError> {
        let mut stream = self
            .client
            .query(&input.statement(&self.cluster_id))
            .await?;
        collect_stored_rows(&mut stream, StoredRowLimit::new(MAX_ROWS_PER_INPUT))
            .await
            .map_err(|source| DnsSourceError::Rows { input, source })
    }
}

pub(super) struct DnsSubscriptions {
    streams: Vec<(DnsInput, SubscriptionStream)>,
}

impl DnsSubscriptions {
    /// Subscription rows are never applied as state. A live change only
    /// invalidates the complete projection query.
    pub(super) async fn wait_for_invalidation(&mut self) -> Result<(), DnsSourceError> {
        let mut pending = FuturesUnordered::new();
        for (input, stream) in &mut self.streams {
            let input = *input;
            pending.push(async move { (input, stream.next().await) });
        }
        let Some((input, event)) = pending.next().await else {
            return Err(DnsSourceError::SubscriptionsEnded);
        };
        match event? {
            Some(SubscriptionStreamEvent::Change(_, _, _, _)) => Ok(()),
            Some(
                SubscriptionStreamEvent::Columns(_)
                | SubscriptionStreamEvent::Row(_, _)
                | SubscriptionStreamEvent::EndOfQuery(_),
            ) => Err(DnsSourceError::UnexpectedLiveFrame { input }),
            None => Err(DnsSourceError::SubscriptionsEnded),
        }
    }

    /// Drains the currently queued part of one notification burst without
    /// waiting for a later change. The cap keeps a hot writer from starving
    /// the authoritative full re-query forever.
    pub(super) async fn drain_ready_invalidations(&mut self) -> Result<(), DnsSourceError> {
        for _ in 0..MAX_COALESCED_INVALIDATIONS {
            match self.wait_for_invalidation().now_or_never() {
                Some(result) => result?,
                None => return Ok(()),
            }
        }
        Ok(())
    }
}

async fn drain_snapshot(
    input: DnsInput,
    stream: &mut SubscriptionStream,
) -> Result<(), DnsSourceError> {
    let mut saw_columns = false;
    let mut rows = 0_usize;
    loop {
        match stream.next().await? {
            Some(SubscriptionStreamEvent::Columns(columns)) if !saw_columns => {
                if columns != ["id", "document"] {
                    return Err(DnsSourceError::UnexpectedColumns { input, columns });
                }
                saw_columns = true;
            }
            Some(SubscriptionStreamEvent::Columns(_)) => {
                return Err(DnsSourceError::DuplicateColumns { input });
            }
            Some(SubscriptionStreamEvent::Row(_, _)) if saw_columns => {
                if rows == MAX_ROWS_PER_INPUT {
                    return Err(DnsSourceError::SnapshotRowLimit {
                        input,
                        limit: MAX_ROWS_PER_INPUT,
                    });
                }
                rows = rows.saturating_add(1);
            }
            Some(SubscriptionStreamEvent::Row(_, _)) => {
                return Err(DnsSourceError::RowBeforeColumns { input });
            }
            Some(SubscriptionStreamEvent::EndOfQuery(_)) if saw_columns => return Ok(()),
            Some(SubscriptionStreamEvent::EndOfQuery(_)) => {
                return Err(DnsSourceError::MissingColumns { input });
            }
            Some(SubscriptionStreamEvent::Change(_, _, _, _)) => {
                return Err(DnsSourceError::ChangeBeforeSnapshot { input });
            }
            None => return Err(DnsSourceError::SubscriptionsEnded),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum DnsSourceError {
    #[error(transparent)]
    Corrosion(#[from] CorrosionClientError),
    #[error("Corrosion DNS {input:?} query failed: {source}")]
    Rows {
        input: DnsInput,
        source: StoredRowCollectionError,
    },
    #[error("DNS subscription snapshot exceeded its {timeout:?} deadline")]
    SnapshotDeadline { timeout: Duration },
    #[error("DNS full re-query exceeded its {timeout:?} deadline")]
    QueryDeadline { timeout: Duration },
    #[error("Corrosion DNS {input:?} subscription returned unexpected columns: {columns:?}")]
    UnexpectedColumns {
        input: DnsInput,
        columns: Vec<String>,
    },
    #[error("Corrosion DNS {input:?} subscription repeated its columns frame")]
    DuplicateColumns { input: DnsInput },
    #[error("Corrosion DNS {input:?} subscription omitted its columns frame")]
    MissingColumns { input: DnsInput },
    #[error("Corrosion DNS {input:?} subscription returned a row before columns")]
    RowBeforeColumns { input: DnsInput },
    #[error("Corrosion DNS {input:?} subscription exceeded its {limit}-row snapshot bound")]
    SnapshotRowLimit { input: DnsInput, limit: usize },
    #[error("Corrosion DNS {input:?} subscription changed before its snapshot watermark")]
    ChangeBeforeSnapshot { input: DnsInput },
    #[error("Corrosion DNS {input:?} subscription returned a snapshot frame after its watermark")]
    UnexpectedLiveFrame { input: DnsInput },
    #[error("Corrosion DNS subscriptions ended")]
    SubscriptionsEnded,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dns_input_is_cluster_scoped() {
        let cluster_id = ClusterName::try_new("01HZZZZZZZZZZZZZZZZZZZZZZZ").expect("cluster id");
        for input in DnsInput::ALL {
            let Statement::WithParams(sql, parameters) = input.statement(&cluster_id) else {
                panic!("DNS queries are parameterized");
            };
            assert!(sql.contains("WHERE"));
            assert_eq!(
                parameters,
                [SqliteParameter::Text(cluster_id.as_str().to_owned())]
            );
        }
    }
}
