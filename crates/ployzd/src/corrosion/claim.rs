use std::time::Duration;

use async_trait::async_trait;
use ployz_core::corrosion::{
    ClusterDocument, CorrosionTable, MeshProvider, NameClaim, NamedCorrosionDocument,
    NamedReadReport, NamedRowEvidence, OrdinaryCorrosionDocument, RosterCorrosionDocument,
    SqliteParameter, SqliteValue, Statement, StoredRow, read_named_roster_rows, read_named_rows,
};
use ployz_core::ids::{ClusterId, CorrosionUlid};

use super::{
    CorrosionClient, CorrosionClientError, StoredRowCollectionError, StoredRowLimit,
    collect_stored_rows,
};

const NAME_CLAIM_COURTESY_WAIT: Duration = Duration::from_secs(1);
const MAX_NAME_CLAIM_ROWS: usize = 10_000;

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
        Document: NamedCorrosionDocument + OrdinaryCorrosionDocument + Send + Sync,
    {
        claim_named_with_store(self, id, document, NAME_CLAIM_COURTESY_WAIT).await
    }

    /// Inserts and adjudicates a named roster document against an accepted
    /// cluster's identity and mesh provider.
    pub async fn claim_roster<Document>(
        &self,
        cluster: &ClusterDocument,
        id: CorrosionUlid,
        document: &Document,
    ) -> Result<NameClaimOutcome<Document>, NameClaimError>
    where
        Document: NamedCorrosionDocument + RosterCorrosionDocument + Send + Sync,
    {
        claim_roster_with_store(self, cluster, id, document, NAME_CLAIM_COURTESY_WAIT).await
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
    Document: NamedCorrosionDocument + OrdinaryCorrosionDocument + Send + Sync,
{
    let cluster_id = document.cluster_id().clone();
    claim_with_store(store, id, document, courtesy_wait, move |rows| {
        read_named_rows::<Document>(&cluster_id, rows)
    })
    .await
}

async fn claim_roster_with_store<Store, Document>(
    store: &Store,
    cluster: &ClusterDocument,
    id: CorrosionUlid,
    document: &Document,
    courtesy_wait: Duration,
) -> Result<NameClaimOutcome<Document>, NameClaimError>
where
    Store: ClaimStore + Sync,
    Document: NamedCorrosionDocument + RosterCorrosionDocument + Send + Sync,
{
    if document.cluster_id() != &cluster.cluster_id {
        return Err(NameClaimError::RosterClusterMismatch {
            expected: cluster.cluster_id.clone(),
            found: document.cluster_id().clone(),
        });
    }
    let found_provider = document.mesh_provider();
    if found_provider != cluster.provider {
        return Err(NameClaimError::RosterProviderMismatch {
            expected: cluster.provider,
            found: found_provider,
        });
    }

    claim_with_store(store, id, document, courtesy_wait, |rows| {
        read_named_roster_rows::<Document>(cluster, rows)
    })
    .await
}

async fn claim_with_store<Store, Document, Read>(
    store: &Store,
    id: CorrosionUlid,
    document: &Document,
    courtesy_wait: Duration,
    read: Read,
) -> Result<NameClaimOutcome<Document>, NameClaimError>
where
    Store: ClaimStore + Sync,
    Document: NamedCorrosionDocument,
    Read: FnOnce(Vec<StoredRow>) -> NamedReadReport<Document>,
{
    let table = Document::TABLE;
    let claim = document.name_claim();
    let encoded = serde_json::to_string(document).map_err(|source| NameClaimError::Encode {
        detail: source.to_string(),
    })?;

    store.insert(table, &id, encoded).await?;
    tokio::time::sleep(courtesy_wait).await;

    let rows = store.rows(table, &claim).await?;
    let report = read(rows);
    finish_claim(store, table, id, claim, report).await
}

async fn finish_claim<Store, Document>(
    store: &Store,
    table: CorrosionTable,
    id: CorrosionUlid,
    claim: NameClaim,
    report: NamedReadReport<Document>,
) -> Result<NameClaimOutcome<Document>, NameClaimError>
where
    Store: ClaimStore + Sync,
    Document: NamedCorrosionDocument,
{
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
        collect_stored_rows(&mut stream, StoredRowLimit::new(MAX_NAME_CLAIM_ROWS))
            .await
            .map_err(NameClaimError::from)
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
    #[error("named Corrosion query repeated its columns frame")]
    DuplicateColumns,
    #[error("named Corrosion query returned a row before its columns frame")]
    RowBeforeColumns,
    #[error("named Corrosion query returned an unexpected row: {values:?}")]
    UnexpectedRow { values: Vec<SqliteValue> },
    #[error("named Corrosion query exceeded the {limit}-row claim bound")]
    RowLimit { limit: usize },
    #[error("inserted claim was not visible after the courtesy period: {claim:?}")]
    ClaimNotVisible { claim: NameClaim },
    #[error("roster claim targets cluster {found}, but accepted cluster is {expected}")]
    RosterClusterMismatch {
        expected: ClusterId,
        found: ClusterId,
    },
    #[error("roster claim uses mesh provider {found:?}, but accepted cluster uses {expected:?}")]
    RosterProviderMismatch {
        expected: MeshProvider,
        found: MeshProvider,
    },
    #[error("name claim {claim:?} cannot be queried from Corrosion table {table:?}")]
    TableMismatch {
        table: CorrosionTable,
        claim: NameClaim,
    },
}

impl From<StoredRowCollectionError> for NameClaimError {
    fn from(error: StoredRowCollectionError) -> Self {
        match error {
            StoredRowCollectionError::Client(source) => Self::Client(source),
            StoredRowCollectionError::MissingColumns => Self::MissingColumns,
            StoredRowCollectionError::DuplicateColumns => Self::DuplicateColumns,
            StoredRowCollectionError::RowBeforeColumns => Self::RowBeforeColumns,
            StoredRowCollectionError::UnexpectedColumns { columns } => {
                Self::UnexpectedColumns { columns }
            }
            StoredRowCollectionError::UnexpectedRow { values } => Self::UnexpectedRow { values },
            StoredRowCollectionError::RowLimit { limit } => Self::RowLimit { limit },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use ployz_core::corrosion::{ClusterDocument, MachineDocument, NamespaceDocument};
    use ployz_core::ids::{ClusterId, NamespaceRowId};
    use ployz_core::operation::RouteHostname;
    use serde_json::json;
    use tokio::sync::{Barrier, Mutex};

    use super::*;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const LOWER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const HIGHER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";

    fn cluster_document() -> ClusterDocument {
        serde_json::from_value(json!({
            "v": 1,
            "cluster_id": CLUSTER,
            "name": "acme-prod",
            "storage_default": "plain",
            "hostname_mode": { "mode": "disabled" },
            "prefix": "10.210.0.0/16",
            "provider": "builtin_wireguard",
            "acme_directory_url": "https://acme.example/directory",
            "acme_contact": null,
            "written_by": { "kind": "peer", "peer_id": LOWER },
            "written_at": "2026-08-04T10:00:00Z"
        }))
        .expect("cluster document")
    }

    fn machine_document(cluster_id: &str, provider: MeshProvider) -> MachineDocument {
        let transport = match provider {
            MeshProvider::BuiltinWireguard => json!({
                "kind": "wireguard",
                "pubkey": "wireguard-public-key",
                "addr_v6": "fd00::20",
                "endpoint": null,
                "subnet_v4": "10.210.20.0/24"
            }),
            MeshProvider::Tailscale => json!({
                "kind": "tailscale",
                "ip": "100.64.0.10",
                "subnet_v4": "10.210.10.0/24"
            }),
        };
        serde_json::from_value(json!({
            "v": 1,
            "cluster_id": cluster_id,
            "name": "edge-a",
            "lifecycle": "active",
            "transport": transport,
            "storage": { "mode": "plain", "reason": { "kind": "default" } },
            "written_by": { "kind": "peer", "peer_id": LOWER },
            "written_at": "2026-08-04T10:00:00Z"
        }))
        .expect("machine document")
    }

    #[derive(Debug)]
    struct RacingStore {
        rows: Mutex<BTreeMap<CorrosionUlid, String>>,
        readers: Barrier,
        deletes: Mutex<Vec<CorrosionUlid>>,
    }

    #[derive(Debug, Default)]
    struct RecordingStore {
        rows: Mutex<BTreeMap<CorrosionUlid, String>>,
        inserts: Mutex<Vec<CorrosionUlid>>,
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

    impl RecordingStore {
        fn with_row(id: CorrosionUlid, document: String) -> Self {
            Self {
                rows: Mutex::new(BTreeMap::from([(id, document)])),
                ..Self::default()
            }
        }
    }

    #[test]
    fn claim_queries_use_the_indexed_collision_scope() {
        let namespace_id = NamespaceRowId::try_new(LOWER).expect("namespace id");
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
                    [LOWER, "api"]
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

    #[async_trait]
    impl ClaimStore for RecordingStore {
        async fn insert(
            &self,
            table: CorrosionTable,
            id: &CorrosionUlid,
            document: String,
        ) -> Result<(), NameClaimError> {
            assert_eq!(table, CorrosionTable::Machines);
            self.rows.lock().await.insert(id.clone(), document);
            self.inserts.lock().await.push(id.clone());
            Ok(())
        }

        async fn rows(
            &self,
            table: CorrosionTable,
            claim: &NameClaim,
        ) -> Result<Vec<StoredRow>, NameClaimError> {
            assert_eq!(table, CorrosionTable::Machines);
            assert_eq!(
                claim,
                &NameClaim::Machine {
                    name: "edge-a".to_owned(),
                }
            );
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
            assert_eq!(table, CorrosionTable::Machines);
            self.rows.lock().await.remove(id);
            self.deletes.lock().await.push(id.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn roster_claim_skips_wrong_provider_lower_ulid_without_deleting_evidence() {
        let cluster = cluster_document();
        let wrong_provider = machine_document(CLUSTER, MeshProvider::Tailscale);
        let accepted_candidate = machine_document(CLUSTER, MeshProvider::BuiltinWireguard);

        let lower = CorrosionUlid::try_new(LOWER).expect("lower id");
        let higher = CorrosionUlid::try_new(HIGHER).expect("higher id");
        let store = RecordingStore::with_row(
            lower.clone(),
            serde_json::to_string(&wrong_provider).expect("document JSON"),
        );

        let outcome = claim_roster_with_store(
            &store,
            &cluster,
            higher.clone(),
            &accepted_candidate,
            Duration::ZERO,
        )
        .await
        .expect("roster claim");

        let NameClaimOutcome::Claimed { id, report } = outcome else {
            panic!("accepted roster candidate lost")
        };
        assert_eq!(id, higher);
        assert!(matches!(
            report.accepted.as_slice(),
            [accepted] if accepted.id == higher
        ));
        assert!(matches!(
            report.skipped.as_slice(),
            [skipped] if skipped.source.key == lower.as_str()
        ));
        assert!(report.shadows.is_empty());
        assert_eq!(
            store.rows.lock().await.keys().cloned().collect::<Vec<_>>(),
            [lower, higher.clone()]
        );
        assert_eq!(*store.inserts.lock().await, [higher]);
        assert!(store.deletes.lock().await.is_empty());
    }

    #[tokio::test]
    async fn roster_claim_rejects_candidates_outside_accepted_context_before_insert() {
        const FOREIGN_CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";

        let cluster = cluster_document();
        let foreign_cluster = machine_document(FOREIGN_CLUSTER, MeshProvider::BuiltinWireguard);
        let wrong_provider = machine_document(CLUSTER, MeshProvider::Tailscale);
        let claim_id = CorrosionUlid::try_new(HIGHER).expect("claim id");
        let accepted_cluster_id = ClusterId::try_new(CLUSTER).expect("accepted cluster id");
        let foreign_cluster_id = ClusterId::try_new(FOREIGN_CLUSTER).expect("foreign cluster id");

        for (candidate, expected) in [
            (
                foreign_cluster,
                NameClaimError::RosterClusterMismatch {
                    expected: accepted_cluster_id,
                    found: foreign_cluster_id,
                },
            ),
            (
                wrong_provider,
                NameClaimError::RosterProviderMismatch {
                    expected: MeshProvider::BuiltinWireguard,
                    found: MeshProvider::Tailscale,
                },
            ),
        ] {
            let store = RecordingStore::default();
            let error = claim_roster_with_store(
                &store,
                &cluster,
                claim_id.clone(),
                &candidate,
                Duration::ZERO,
            )
            .await
            .expect_err("invalid roster claim context");

            assert_eq!(error, expected);
            assert!(store.rows.lock().await.is_empty());
            assert!(store.inserts.lock().await.is_empty());
            assert!(store.deletes.lock().await.is_empty());
        }
    }

    #[tokio::test]
    async fn racing_claims_converge_on_lower_ulid_and_delete_only_loser() {
        let store = Arc::new(RacingStore::new());
        let document = Arc::new(
            serde_json::from_value::<NamespaceDocument>(json!({
                "v": 1,
                "cluster_id": CLUSTER,
                "name": "production",
                "written_by": { "kind": "peer", "peer_id": LOWER },
                "written_at": "2026-08-04T10:00:00Z"
            }))
            .expect("namespace document"),
        );

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
