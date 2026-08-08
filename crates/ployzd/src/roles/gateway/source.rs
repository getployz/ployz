//! Bounded complete-table Corrosion inputs for the gateway projection.

use std::time::Duration;

use futures_util::future::FutureExt;
use futures_util::stream::{FuturesUnordered, StreamExt};
use ployz_core::corrosion::{CorrosionTable, SqliteParameter, Statement, StoredRow};
use ployz_core::ids::ClusterId;

use crate::corrosion::{
    CorrosionClient, CorrosionClientError, StoredRowCollectionError, StoredRowLimit,
    SubscriptionStream, SubscriptionStreamEvent, collect_stored_rows,
};

const MAX_ROWS_PER_INPUT: usize = 16_384;
pub(super) const REFRESH_DEADLINE: Duration = Duration::from_secs(2);
const MAX_COALESCED_INVALIDATIONS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GatewayInput {
    Services,
    RouteBindings,
    Containers,
}

impl GatewayInput {
    const ALL: [Self; 3] = [Self::Services, Self::RouteBindings, Self::Containers];

    const fn table(self) -> CorrosionTable {
        match self {
            Self::Services => CorrosionTable::Services,
            Self::RouteBindings => CorrosionTable::RouteBindings,
            Self::Containers => CorrosionTable::Containers,
        }
    }

    fn statement(self, cluster_id: &ClusterId) -> Statement {
        Statement::with_params(
            format!(
                "SELECT id, document FROM {} WHERE json_extract(document, '$.cluster_id') = ?",
                self.table().as_str()
            ),
            vec![SqliteParameter::Text(cluster_id.as_str().to_owned())],
        )
    }
}

#[derive(Debug)]
pub(super) struct GatewayRows {
    pub services: Vec<StoredRow>,
    pub route_bindings: Vec<StoredRow>,
    pub containers: Vec<StoredRow>,
}

pub(super) struct CorrosionGatewaySource {
    client: CorrosionClient,
    cluster_id: ClusterId,
}

impl CorrosionGatewaySource {
    #[must_use]
    pub(super) const fn new(client: CorrosionClient, cluster_id: ClusterId) -> Self {
        Self { client, cluster_id }
    }

    /// Every wake stream is active before the first authoritative read.
    pub(super) async fn subscribe(&self) -> Result<GatewaySubscriptions, GatewaySourceError> {
        tokio::time::timeout(REFRESH_DEADLINE, async {
            let mut streams = Vec::with_capacity(GatewayInput::ALL.len());
            for input in GatewayInput::ALL {
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
        .map_err(|_| GatewaySourceError::SubscriptionDeadline {
            timeout: REFRESH_DEADLINE,
        })?
        .map(|streams| GatewaySubscriptions { streams })
    }

    pub(super) async fn query_rows(&self) -> Result<GatewayRows, GatewaySourceError> {
        tokio::time::timeout(REFRESH_DEADLINE, async {
            Ok(GatewayRows {
                services: self.query(GatewayInput::Services).await?,
                route_bindings: self.query(GatewayInput::RouteBindings).await?,
                containers: self.query(GatewayInput::Containers).await?,
            })
        })
        .await
        .map_err(|_| GatewaySourceError::QueryDeadline {
            timeout: REFRESH_DEADLINE,
        })?
    }

    async fn query(&self, input: GatewayInput) -> Result<Vec<StoredRow>, GatewaySourceError> {
        let mut stream = self
            .client
            .query(&input.statement(&self.cluster_id))
            .await?;
        collect_stored_rows(&mut stream, StoredRowLimit::new(MAX_ROWS_PER_INPUT))
            .await
            .map_err(|source| GatewaySourceError::Rows { input, source })
    }
}

pub(super) struct GatewaySubscriptions {
    streams: Vec<(GatewayInput, SubscriptionStream)>,
}

impl GatewaySubscriptions {
    /// Subscription data is an invalidation only; it is never folded as truth.
    pub(super) async fn wait_for_invalidation(&mut self) -> Result<(), GatewaySourceError> {
        let mut pending = FuturesUnordered::new();
        for (input, stream) in &mut self.streams {
            let input = *input;
            pending.push(async move { (input, stream.next().await) });
        }
        let Some((input, event)) = pending.next().await else {
            return Err(GatewaySourceError::SubscriptionsEnded);
        };
        match event? {
            Some(SubscriptionStreamEvent::Change(_, _, _, _)) => Ok(()),
            Some(
                SubscriptionStreamEvent::Columns(_)
                | SubscriptionStreamEvent::Row(_, _)
                | SubscriptionStreamEvent::EndOfQuery(_),
            ) => Err(GatewaySourceError::UnexpectedLiveFrame { input }),
            None => Err(GatewaySourceError::SubscriptionsEnded),
        }
    }

    pub(super) async fn drain_ready_invalidations(&mut self) -> Result<(), GatewaySourceError> {
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
    input: GatewayInput,
    stream: &mut SubscriptionStream,
) -> Result<(), GatewaySourceError> {
    let mut saw_columns = false;
    let mut rows = 0_usize;
    loop {
        match stream.next().await? {
            Some(SubscriptionStreamEvent::Columns(columns)) if !saw_columns => {
                if columns != ["id", "document"] {
                    return Err(GatewaySourceError::UnexpectedColumns { input, columns });
                }
                saw_columns = true;
            }
            Some(SubscriptionStreamEvent::Columns(_)) => {
                return Err(GatewaySourceError::DuplicateColumns { input });
            }
            Some(SubscriptionStreamEvent::Row(_, _)) if saw_columns => {
                if rows == MAX_ROWS_PER_INPUT {
                    return Err(GatewaySourceError::SnapshotRowLimit {
                        input,
                        limit: MAX_ROWS_PER_INPUT,
                    });
                }
                rows = rows.saturating_add(1);
            }
            Some(SubscriptionStreamEvent::Row(_, _)) => {
                return Err(GatewaySourceError::RowBeforeColumns { input });
            }
            Some(SubscriptionStreamEvent::EndOfQuery(_)) if saw_columns => return Ok(()),
            Some(SubscriptionStreamEvent::EndOfQuery(_)) => {
                return Err(GatewaySourceError::MissingColumns { input });
            }
            Some(SubscriptionStreamEvent::Change(_, _, _, _)) => {
                return Err(GatewaySourceError::ChangeBeforeSnapshot { input });
            }
            None => return Err(GatewaySourceError::SubscriptionsEnded),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum GatewaySourceError {
    #[error(transparent)]
    Corrosion(#[from] CorrosionClientError),
    #[error("Corrosion gateway {input:?} query failed: {source}")]
    Rows {
        input: GatewayInput,
        source: StoredRowCollectionError,
    },
    #[error("gateway subscription setup exceeded its {timeout:?} deadline")]
    SubscriptionDeadline { timeout: Duration },
    #[error("gateway complete refresh exceeded its {timeout:?} deadline")]
    QueryDeadline { timeout: Duration },
    #[error("gateway {input:?} subscription returned unexpected columns: {columns:?}")]
    UnexpectedColumns {
        input: GatewayInput,
        columns: Vec<String>,
    },
    #[error("gateway {input:?} subscription repeated its columns frame")]
    DuplicateColumns { input: GatewayInput },
    #[error("gateway {input:?} subscription omitted its columns frame")]
    MissingColumns { input: GatewayInput },
    #[error("gateway {input:?} subscription returned a row before columns")]
    RowBeforeColumns { input: GatewayInput },
    #[error("gateway {input:?} subscription exceeded its {limit}-row snapshot bound")]
    SnapshotRowLimit { input: GatewayInput, limit: usize },
    #[error("gateway {input:?} subscription changed before its snapshot watermark")]
    ChangeBeforeSnapshot { input: GatewayInput },
    #[error("gateway {input:?} subscription returned a snapshot frame after its watermark")]
    UnexpectedLiveFrame { input: GatewayInput },
    #[error("gateway Corrosion subscriptions ended")]
    SubscriptionsEnded,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gateway_input_is_cluster_scoped_and_refresh_is_two_seconds() {
        let cluster_id = ClusterId::try_new("01HZZZZZZZZZZZZZZZZZZZZZZZ").expect("cluster");
        for input in GatewayInput::ALL {
            let Statement::WithParams(sql, parameters) = input.statement(&cluster_id) else {
                panic!("gateway queries are parameterized");
            };
            assert!(sql.contains(input.table().as_str()));
            assert_eq!(
                parameters,
                [SqliteParameter::Text(cluster_id.as_str().to_owned())]
            );
        }
        assert_eq!(REFRESH_DEADLINE, Duration::from_secs(2));
    }
}
