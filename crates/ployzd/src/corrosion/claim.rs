use std::time::Duration;

use async_trait::async_trait;
use ployz_core::corrosion::{
    CorrosionTable, NameClaim, NamedCorrosionDocument, NamedReadReport, NamedRowEvidence,
    SqliteParameter, SqliteValue, Statement, StoredRow, read_named_rows,
};
use ployz_core::ids::CorrosionUlid;

use super::{CorrosionClient, CorrosionClientError, QueryStreamEvent};

const NAME_CLAIM_COURTESY_WAIT: Duration = Duration::from_secs(1);

/// The result of resolving one optimistic named-document claim.
#[derive(Debug)]
pub enum NameClaimOutcome<Document> {
    /// This row is the lowest canonical ULID for its claim.
    Claimed {
        id: CorrosionUlid,
        report: NamedReadReport<Document>,
    },
    /// Another row won; this caller's losing row was deleted.
    Lost {
        id: CorrosionUlid,
        winner: NamedRowEvidence,
        report: NamedReadReport<Document>,
    },
}

impl CorrosionClient {
    /// Inserts and adjudicates a named document using the lowest-ULID reader law.
    ///
    /// The caller owns the read-only free-name precondition. Once that check
    /// passes, this method performs the mutating claim steps: insert, courtesy
    /// wait, adjudicate, and delete only its own row if it loses.
    pub async fn claim_named<Document>(
        &self,
        id: CorrosionUlid,
        document: &Document,
    ) -> Result<NameClaimOutcome<Document>, NameClaimError>
    where
        Document: NamedCorrosionDocument + Send + Sync,
    {
        claim_named_with_store(self, id, document, NAME_CLAIM_COURTESY_WAIT).await
    }
}

async fn claim_named_with_store<Store, Document>(
    store: &Store,
    id: CorrosionUlid,
    document: &Document,
    courtesy_wait: Duration,
) -> Result<NameClaimOutcome<Document>, NameClaimError>
where
    Store: ClaimStore + Sync,
    Document: NamedCorrosionDocument + Send + Sync,
{
    let table = Document::TABLE;
    let cluster_id = document.cluster_id().clone();
    let claim = document.name_claim();
    let encoded = serde_json::to_string(document).map_err(|source| NameClaimError::Encode {
        detail: source.to_string(),
    })?;

    store.insert(table, &id, encoded).await?;
    tokio::time::sleep(courtesy_wait).await;

    let rows = store.rows(table, &claim).await?;
    let report = read_named_rows::<Document>(&cluster_id, rows);
    let winner = report
        .accepted
        .iter()
        .find(|row| row.value.name_claim() == claim)
        .map(|row| NamedRowEvidence {
            id: row.id.clone(),
            source: row.source.clone(),
        })
        .ok_or_else(|| NameClaimError::ClaimNotVisible {
            claim: claim.clone(),
        })?;

    if winner.id == id {
        return Ok(NameClaimOutcome::Claimed { id, report });
    }

    store.delete(table, &id).await?;
    Ok(NameClaimOutcome::Lost { id, winner, report })
}

#[async_trait]
trait ClaimStore {
    async fn insert(
        &self,
        table: CorrosionTable,
        id: &CorrosionUlid,
        document: String,
    ) -> Result<(), NameClaimError>;

    async fn rows(
        &self,
        table: CorrosionTable,
        claim: &NameClaim,
    ) -> Result<Vec<StoredRow>, NameClaimError>;

    async fn delete(&self, table: CorrosionTable, id: &CorrosionUlid)
    -> Result<(), NameClaimError>;
}

#[async_trait]
impl ClaimStore for CorrosionClient {
    async fn insert(
        &self,
        table: CorrosionTable,
        id: &CorrosionUlid,
        document: String,
    ) -> Result<(), NameClaimError> {
        self.execute(&[Statement::with_params(
            format!(
                "INSERT INTO {} (id, document) VALUES (?, ?)",
                table.as_str()
            ),
            vec![
                SqliteParameter::Text(id.as_str().to_owned()),
                SqliteParameter::Text(document),
            ],
        )])
        .await?;
        Ok(())
    }

    async fn rows(
        &self,
        table: CorrosionTable,
        claim: &NameClaim,
    ) -> Result<Vec<StoredRow>, NameClaimError> {
        let mut stream = self.query(&claim_query(table, claim)?).await?;
        let mut saw_columns = false;
        let mut rows = Vec::new();

        while let Some(event) = stream.next().await? {
            match event {
                QueryStreamEvent::Columns(columns) if columns == ["id", "document"] => {
                    saw_columns = true;
                }
                QueryStreamEvent::Columns(columns) => {
                    return Err(NameClaimError::UnexpectedColumns { columns });
                }
                QueryStreamEvent::Row(_, values) => {
                    let [SqliteValue::Text(key), SqliteValue::Text(document)] = values.as_slice()
                    else {
                        return Err(NameClaimError::UnexpectedRow { values });
                    };
                    rows.push(StoredRow::new(key.clone(), document.clone()));
                }
                QueryStreamEvent::EndOfQuery(_) => break,
            }
        }

        if !saw_columns {
            return Err(NameClaimError::MissingColumns);
        }
        Ok(rows)
    }

    async fn delete(
        &self,
        table: CorrosionTable,
        id: &CorrosionUlid,
    ) -> Result<(), NameClaimError> {
        self.execute(&[Statement::with_params(
            format!("DELETE FROM {} WHERE id = ?", table.as_str()),
            vec![SqliteParameter::Text(id.as_str().to_owned())],
        )])
        .await?;
        Ok(())
    }
}

fn claim_query(table: CorrosionTable, claim: &NameClaim) -> Result<Statement, NameClaimError> {
    let (expected_table, predicate, parameters) = match claim {
        NameClaim::Machine { name } => (
            CorrosionTable::Machines,
            "name = ?",
            vec![SqliteParameter::Text(name.clone())],
        ),
        NameClaim::Peer { name } => (
            CorrosionTable::Peers,
            "name = ?",
            vec![SqliteParameter::Text(name.clone())],
        ),
        NameClaim::Namespace { name } => (
            CorrosionTable::Namespaces,
            "name = ?",
            vec![SqliteParameter::Text(name.clone())],
        ),
        NameClaim::Service { namespace_id, name } => (
            CorrosionTable::Services,
            "namespace_id = ? AND name = ?",
            vec![
                SqliteParameter::Text(namespace_id.as_str().to_owned()),
                SqliteParameter::Text(name.clone()),
            ],
        ),
        NameClaim::RouteBinding { hostname } => (
            CorrosionTable::RouteBindings,
            "hostname = ?",
            vec![SqliteParameter::Text(hostname.as_str().to_owned())],
        ),
    };
    if table != expected_table {
        return Err(NameClaimError::TableMismatch {
            table,
            claim: claim.clone(),
        });
    }

    Ok(Statement::with_params(
        format!(
            "SELECT id, document FROM {} WHERE {predicate}",
            table.as_str()
        ),
        parameters,
    ))
}

/// A failure to insert, observe, adjudicate, or clean up one name claim.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum NameClaimError {
    #[error("named Corrosion document could not be encoded: {detail}")]
    Encode { detail: String },
    #[error(transparent)]
    Client(#[from] CorrosionClientError),
    #[error("named Corrosion query returned unexpected columns: {columns:?}")]
    UnexpectedColumns { columns: Vec<String> },
    #[error("named Corrosion query returned no columns frame")]
    MissingColumns,
    #[error("named Corrosion query returned an unexpected row: {values:?}")]
    UnexpectedRow { values: Vec<SqliteValue> },
    #[error("inserted claim was not visible after the courtesy period: {claim:?}")]
    ClaimNotVisible { claim: NameClaim },
    #[error("name claim {claim:?} cannot be queried from Corrosion table {table:?}")]
    TableMismatch {
        table: CorrosionTable,
        claim: NameClaim,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use ployz_core::corrosion::{CorrosionDocumentVersion, NamespaceDocument};
    use ployz_core::ids::{ClusterId, NamespaceId};
    use ployz_core::operation::RouteHostname;
    use serde_json::json;
    use tokio::sync::{Barrier, Mutex};

    use super::*;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const LOWER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const HIGHER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";

    #[derive(Debug)]
    struct RacingStore {
        rows: Mutex<BTreeMap<CorrosionUlid, String>>,
        readers: Barrier,
        deletes: Mutex<Vec<CorrosionUlid>>,
    }

    impl RacingStore {
        fn new() -> Self {
            Self {
                rows: Mutex::new(BTreeMap::new()),
                readers: Barrier::new(2),
                deletes: Mutex::new(Vec::new()),
            }
        }
    }

    #[test]
    fn claim_queries_use_the_indexed_collision_scope() {
        let namespace_id = NamespaceId::try_new("namespace-production").expect("namespace id");
        let hostname = RouteHostname::try_new("api.example.com").expect("hostname");
        let cases = [
            (
                CorrosionTable::Machines,
                NameClaim::Machine {
                    name: "edge-a".to_owned(),
                },
                json!([
                    "SELECT id, document FROM machines WHERE name = ?",
                    ["edge-a"]
                ]),
            ),
            (
                CorrosionTable::Peers,
                NameClaim::Peer {
                    name: "operator".to_owned(),
                },
                json!([
                    "SELECT id, document FROM peers WHERE name = ?",
                    ["operator"]
                ]),
            ),
            (
                CorrosionTable::Namespaces,
                NameClaim::Namespace {
                    name: "production".to_owned(),
                },
                json!([
                    "SELECT id, document FROM namespaces WHERE name = ?",
                    ["production"]
                ]),
            ),
            (
                CorrosionTable::Services,
                NameClaim::Service {
                    namespace_id: namespace_id.clone(),
                    name: "api".to_owned(),
                },
                json!([
                    "SELECT id, document FROM services WHERE namespace_id = ? AND name = ?",
                    ["namespace-production", "api"]
                ]),
            ),
            (
                CorrosionTable::RouteBindings,
                NameClaim::RouteBinding {
                    hostname: hostname.clone(),
                },
                json!([
                    "SELECT id, document FROM route_bindings WHERE hostname = ?",
                    ["api.example.com"]
                ]),
            ),
        ];

        for (table, claim, expected) in cases {
            let statement = claim_query(table, &claim).expect("claim query");
            assert_eq!(
                serde_json::to_value(statement).expect("statement JSON"),
                expected
            );
        }
    }

    #[async_trait]
    impl ClaimStore for RacingStore {
        async fn insert(
            &self,
            table: CorrosionTable,
            id: &CorrosionUlid,
            document: String,
        ) -> Result<(), NameClaimError> {
            assert_eq!(table, CorrosionTable::Namespaces);
            self.rows.lock().await.insert(id.clone(), document);
            Ok(())
        }

        async fn rows(
            &self,
            table: CorrosionTable,
            claim: &NameClaim,
        ) -> Result<Vec<StoredRow>, NameClaimError> {
            assert_eq!(table, CorrosionTable::Namespaces);
            assert_eq!(
                claim,
                &NameClaim::Namespace {
                    name: "production".to_owned(),
                }
            );
            self.readers.wait().await;
            Ok(self
                .rows
                .lock()
                .await
                .iter()
                .map(|(id, document)| StoredRow::new(id.as_str(), document))
                .collect())
        }

        async fn delete(
            &self,
            table: CorrosionTable,
            id: &CorrosionUlid,
        ) -> Result<(), NameClaimError> {
            assert_eq!(table, CorrosionTable::Namespaces);
            self.rows.lock().await.remove(id);
            self.deletes.lock().await.push(id.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn racing_claims_converge_on_lower_ulid_and_delete_only_loser() {
        let store = Arc::new(RacingStore::new());
        let document = Arc::new(NamespaceDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: ClusterId::try_new(CLUSTER).expect("cluster id"),
            name: "production".to_owned(),
        });

        let lower = CorrosionUlid::try_new(LOWER).expect("lower id");
        let higher = CorrosionUlid::try_new(HIGHER).expect("higher id");
        let lower_task = tokio::spawn({
            let store = Arc::clone(&store);
            let document = Arc::clone(&document);
            let lower = lower.clone();
            async move {
                claim_named_with_store(store.as_ref(), lower, document.as_ref(), Duration::ZERO)
                    .await
            }
        });
        let higher_task = tokio::spawn({
            let store = Arc::clone(&store);
            let document = Arc::clone(&document);
            let higher = higher.clone();
            async move {
                claim_named_with_store(store.as_ref(), higher, document.as_ref(), Duration::ZERO)
                    .await
            }
        });

        let lower_outcome = lower_task.await.expect("lower task").expect("lower claim");
        let higher_outcome = higher_task
            .await
            .expect("higher task")
            .expect("higher claim");

        match lower_outcome {
            NameClaimOutcome::Claimed { id, .. } => assert_eq!(id, lower),
            NameClaimOutcome::Lost { winner, .. } => {
                panic!("lower ULID lost to {}", winner.id)
            }
        }
        match higher_outcome {
            NameClaimOutcome::Lost { id, winner, .. } => {
                assert_eq!(id, higher);
                assert_eq!(winner.id, lower);
            }
            NameClaimOutcome::Claimed { id, .. } => panic!("higher ULID {id} won"),
        }

        let remaining = store.rows.lock().await.keys().cloned().collect::<Vec<_>>();
        assert_eq!(remaining, [lower]);
        assert_eq!(*store.deletes.lock().await, [higher]);
    }
}
