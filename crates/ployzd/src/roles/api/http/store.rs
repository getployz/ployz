//! Bounded Corrosion reads and parameterized writes used by API mutations.

use serde::Serialize;

use ployz_core::corrosion::{
    ClusterDocument, CorrosionTable, MachineDocument, OperatorWriteProvenance, PeerDocument,
    SqliteParameter, Statement, StoredRow, TokenDocument, read_named_roster_rows, read_rows,
};
use ployz_core::ids::{ClusterId, MachineRowId, PeerId, TokenId};

use crate::corrosion::{
    CorrosionClient, CorrosionClientError, StoredRowCollectionError, StoredRowLimit,
    collect_stored_rows,
};

const MAX_MUTATION_ROWS: usize = 10_000;

#[derive(Debug)]
pub(super) struct AcceptedRoster {
    pub(super) cluster: ClusterDocument,
    pub(super) machines: Vec<AcceptedMachine>,
    pub(super) peers: Vec<AcceptedPeer>,
}

#[derive(Debug, Clone)]
pub(super) struct AcceptedMachine {
    pub(super) id: MachineRowId,
    pub(super) document: MachineDocument,
}

#[derive(Debug, Clone)]
pub(super) struct AcceptedPeer {
    pub(super) id: PeerId,
    pub(super) document: PeerDocument,
}

pub(super) async fn read_accepted_roster(
    corrosion: &CorrosionClient,
    cluster_id: &ClusterId,
) -> Result<AcceptedRoster, MutationStoreError> {
    let cluster = query_rows(corrosion, select_cluster(cluster_id));
    let machines = query_rows(corrosion, select_all(CorrosionTable::Machines));
    let peers = query_rows(corrosion, select_all(CorrosionTable::Peers));
    let (cluster_rows, machine_rows, peer_rows) = tokio::try_join!(cluster, machines, peers)?;

    let cluster = one_cluster(cluster_id, cluster_rows)?;
    let machines = read_named_roster_rows::<MachineDocument>(&cluster, machine_rows)
        .accepted
        .into_iter()
        .map(|row| {
            Ok(AcceptedMachine {
                id: MachineRowId::try_new(row.id.as_str().to_owned()).map_err(|error| {
                    MutationStoreError::InvalidAcceptedId {
                        table: CorrosionTable::Machines,
                        detail: error.to_string(),
                    }
                })?,
                document: row.value,
            })
        })
        .collect::<Result<Vec<_>, MutationStoreError>>()?;
    let peers = read_named_roster_rows::<PeerDocument>(&cluster, peer_rows)
        .accepted
        .into_iter()
        .map(|row| {
            Ok(AcceptedPeer {
                id: PeerId::try_new(row.id.as_str().to_owned()).map_err(|error| {
                    MutationStoreError::InvalidAcceptedId {
                        table: CorrosionTable::Peers,
                        detail: error.to_string(),
                    }
                })?,
                document: row.value,
            })
        })
        .collect::<Result<Vec<_>, MutationStoreError>>()?;
    Ok(AcceptedRoster {
        cluster,
        machines,
        peers,
    })
}

pub(super) async fn read_tokens(
    corrosion: &CorrosionClient,
    cluster_id: &ClusterId,
) -> Result<Vec<(TokenId, TokenDocument)>, MutationStoreError> {
    let rows = query_rows(corrosion, select_all(CorrosionTable::Tokens)).await?;
    read_rows::<TokenDocument>(cluster_id, rows)
        .accepted
        .into_iter()
        .map(|row| {
            Ok((
                TokenId::try_new(row.source.key.clone()).map_err(|error| {
                    MutationStoreError::InvalidAcceptedId {
                        table: CorrosionTable::Tokens,
                        detail: error.to_string(),
                    }
                })?,
                row.value,
            ))
        })
        .collect()
}

pub(super) async fn read_token(
    corrosion: &CorrosionClient,
    cluster_id: &ClusterId,
    token_id: &TokenId,
) -> Result<Option<TokenDocument>, MutationStoreError> {
    let rows = query_rows(
        corrosion,
        select_by_id(CorrosionTable::Tokens, token_id.as_str()),
    )
    .await?;
    let report = read_rows::<TokenDocument>(cluster_id, rows);
    let mut accepted = report.accepted.into_iter();
    let token = accepted.next().map(|row| row.value);
    if accepted.next().is_some() {
        return Err(MutationStoreError::DuplicatePrimaryKey {
            table: CorrosionTable::Tokens,
            id: token_id.as_str().to_owned(),
        });
    }
    Ok(token)
}

pub(super) async fn read_machine(
    corrosion: &CorrosionClient,
    cluster: &ClusterDocument,
    machine_id: &MachineRowId,
) -> Result<Option<MachineDocument>, MutationStoreError> {
    let rows = query_rows(
        corrosion,
        select_by_id(CorrosionTable::Machines, machine_id.as_str()),
    )
    .await?;
    let report = read_named_roster_rows::<MachineDocument>(cluster, rows);
    let mut accepted = report.accepted.into_iter();
    let machine = accepted.next().map(|row| row.value);
    if accepted.next().is_some() {
        return Err(MutationStoreError::DuplicatePrimaryKey {
            table: CorrosionTable::Machines,
            id: machine_id.as_str().to_owned(),
        });
    }
    Ok(machine)
}

pub(super) async fn read_peer(
    corrosion: &CorrosionClient,
    cluster: &ClusterDocument,
    peer_id: &PeerId,
) -> Result<Option<PeerDocument>, MutationStoreError> {
    let rows = query_rows(
        corrosion,
        select_by_id(CorrosionTable::Peers, peer_id.as_str()),
    )
    .await?;
    let report = read_named_roster_rows::<PeerDocument>(cluster, rows);
    let mut accepted = report.accepted.into_iter();
    let peer = accepted.next().map(|row| row.value);
    if accepted.next().is_some() {
        return Err(MutationStoreError::DuplicatePrimaryKey {
            table: CorrosionTable::Peers,
            id: peer_id.as_str().to_owned(),
        });
    }
    Ok(peer)
}

pub(super) async fn insert_document<Document>(
    corrosion: &CorrosionClient,
    table: CorrosionTable,
    id: &str,
    document: &Document,
) -> Result<(), MutationStoreError>
where
    Document: Serialize + ?Sized,
{
    let document = serde_json::to_string(document).map_err(|error| MutationStoreError::Encode {
        table,
        detail: error.to_string(),
    })?;
    corrosion
        .execute(&[insert_statement(table, id, document)])
        .await?;
    Ok(())
}

pub(super) async fn replace_document<Document>(
    corrosion: &CorrosionClient,
    table: CorrosionTable,
    id: &str,
    document: &Document,
) -> Result<(), MutationStoreError>
where
    Document: Serialize + ?Sized,
{
    let document = serde_json::to_string(document).map_err(|error| MutationStoreError::Encode {
        table,
        detail: error.to_string(),
    })?;
    corrosion
        .execute(&[replace_statement(table, id, document)])
        .await?;
    Ok(())
}

pub(super) async fn update_wireguard_endpoint(
    corrosion: &CorrosionClient,
    machine_id: &MachineRowId,
    endpoint: std::net::SocketAddr,
    provenance: &OperatorWriteProvenance,
) -> Result<(), MutationStoreError> {
    let endpoint =
        serde_json::to_string(&endpoint).map_err(|error| MutationStoreError::Encode {
            table: CorrosionTable::Machines,
            detail: error.to_string(),
        })?;
    let written_by = serde_json::to_string(&provenance.written_by).map_err(|error| {
        MutationStoreError::Encode {
            table: CorrosionTable::Machines,
            detail: error.to_string(),
        }
    })?;
    let written_at = serde_json::to_string(&provenance.written_at).map_err(|error| {
        MutationStoreError::Encode {
            table: CorrosionTable::Machines,
            detail: error.to_string(),
        }
    })?;
    corrosion
        .execute(&[update_wireguard_endpoint_statement(
            machine_id, endpoint, written_by, written_at,
        )])
        .await?;
    Ok(())
}

pub(super) async fn delete_document(
    corrosion: &CorrosionClient,
    table: CorrosionTable,
    id: &str,
) -> Result<(), MutationStoreError> {
    corrosion.execute(&[delete_statement(table, id)]).await?;
    Ok(())
}

pub(super) async fn delete_document_if_matches<Document>(
    corrosion: &CorrosionClient,
    table: CorrosionTable,
    id: &str,
    expected: &Document,
) -> Result<(), MutationStoreError>
where
    Document: Serialize + ?Sized,
{
    let expected = serde_json::to_string(expected).map_err(|error| MutationStoreError::Encode {
        table,
        detail: error.to_string(),
    })?;
    corrosion
        .execute(&[conditional_delete_statement(table, id, expected)])
        .await?;
    Ok(())
}

async fn query_rows(
    corrosion: &CorrosionClient,
    statement: Statement,
) -> Result<Vec<StoredRow>, MutationStoreError> {
    let mut stream = corrosion.query(&statement).await?;
    collect_stored_rows(&mut stream, StoredRowLimit::new(MAX_MUTATION_ROWS))
        .await
        .map_err(MutationStoreError::from)
}

fn one_cluster(
    cluster_id: &ClusterId,
    rows: Vec<StoredRow>,
) -> Result<ClusterDocument, MutationStoreError> {
    let mut accepted = read_rows::<ClusterDocument>(cluster_id, rows)
        .accepted
        .into_iter();
    let Some(row) = accepted.next() else {
        return Err(MutationStoreError::MissingCluster);
    };
    if accepted.next().is_some() || row.source.key != cluster_id.as_str() {
        return Err(MutationStoreError::InvalidCluster);
    }
    Ok(row.value)
}

fn select_cluster(cluster_id: &ClusterId) -> Statement {
    select_by_id(CorrosionTable::Cluster, cluster_id.as_str())
}

fn select_all(table: CorrosionTable) -> Statement {
    Statement::simple(format!("SELECT id, document FROM {}", table.as_str()))
}

fn select_by_id(table: CorrosionTable, id: &str) -> Statement {
    Statement::with_params(
        format!("SELECT id, document FROM {} WHERE id = ?", table.as_str()),
        vec![SqliteParameter::Text(id.to_owned())],
    )
}

fn insert_statement(table: CorrosionTable, id: &str, document: String) -> Statement {
    Statement::with_params(
        format!(
            "INSERT INTO {} (id, document) VALUES (?, ?)",
            table.as_str()
        ),
        vec![
            SqliteParameter::Text(id.to_owned()),
            SqliteParameter::Text(document),
        ],
    )
}

fn replace_statement(table: CorrosionTable, id: &str, document: String) -> Statement {
    Statement::with_params(
        format!("UPDATE {} SET document = ? WHERE id = ?", table.as_str()),
        vec![
            SqliteParameter::Text(document),
            SqliteParameter::Text(id.to_owned()),
        ],
    )
}

fn delete_statement(table: CorrosionTable, id: &str) -> Statement {
    Statement::with_params(
        format!("DELETE FROM {} WHERE id = ?", table.as_str()),
        vec![SqliteParameter::Text(id.to_owned())],
    )
}

fn conditional_delete_statement(table: CorrosionTable, id: &str, expected: String) -> Statement {
    Statement::with_params(
        format!(
            "DELETE FROM {} WHERE id = ? AND document = ?",
            table.as_str()
        ),
        vec![
            SqliteParameter::Text(id.to_owned()),
            SqliteParameter::Text(expected),
        ],
    )
}

fn update_wireguard_endpoint_statement(
    machine_id: &MachineRowId,
    endpoint: String,
    written_by: String,
    written_at: String,
) -> Statement {
    Statement::with_params(
        "UPDATE machines SET document = json_set(document, '$.transport.endpoint', json(?), '$.written_by', json(?), '$.written_at', json(?)) WHERE id = ?",
        vec![
            SqliteParameter::Text(endpoint),
            SqliteParameter::Text(written_by),
            SqliteParameter::Text(written_at),
            SqliteParameter::Text(machine_id.as_str().to_owned()),
        ],
    )
}

#[derive(Debug, thiserror::Error)]
pub(super) enum MutationStoreError {
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
    #[error("{table:?} contains duplicate primary key {id}")]
    DuplicatePrimaryKey { table: CorrosionTable, id: String },
    #[error("could not encode {table:?} document: {detail}")]
    Encode {
        table: CorrosionTable,
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mutation_statement_is_parameterized() {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        assert_eq!(
            insert_statement(CorrosionTable::Tokens, id, "{}".to_owned()),
            Statement::with_params(
                "INSERT INTO tokens (id, document) VALUES (?, ?)",
                vec![
                    SqliteParameter::Text(id.to_owned()),
                    SqliteParameter::Text("{}".to_owned()),
                ],
            )
        );
        assert_eq!(
            replace_statement(CorrosionTable::Machines, id, "{}".to_owned()),
            Statement::with_params(
                "UPDATE machines SET document = ? WHERE id = ?",
                vec![
                    SqliteParameter::Text("{}".to_owned()),
                    SqliteParameter::Text(id.to_owned()),
                ],
            )
        );
        assert_eq!(
            delete_statement(CorrosionTable::Tokens, id),
            Statement::with_params(
                "DELETE FROM tokens WHERE id = ?",
                vec![SqliteParameter::Text(id.to_owned())],
            )
        );
    }

    #[test]
    fn token_lookup_is_an_o_one_primary_key_query() {
        assert_eq!(
            select_by_id(CorrosionTable::Tokens, "TOKEN"),
            Statement::with_params(
                "SELECT id, document FROM tokens WHERE id = ?",
                vec![SqliteParameter::Text("TOKEN".to_owned())],
            )
        );
    }

    #[test]
    fn endpoint_update_changes_only_endpoint_and_provenance_with_bound_parameters() {
        let machine_id = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("machine id");
        assert_eq!(
            update_wireguard_endpoint_statement(
                &machine_id,
                "\"203.0.113.10:51820\"".to_owned(),
                "{\"kind\":\"machine\",\"machine_id\":\"01ARZ3NDEKTSV4RRFFQ69G5FAV\"}".to_owned(),
                "\"2026-08-05T09:00:00Z\"".to_owned(),
            ),
            Statement::with_params(
                "UPDATE machines SET document = json_set(document, '$.transport.endpoint', json(?), '$.written_by', json(?), '$.written_at', json(?)) WHERE id = ?",
                vec![
                    SqliteParameter::Text("\"203.0.113.10:51820\"".to_owned()),
                    SqliteParameter::Text(
                        "{\"kind\":\"machine\",\"machine_id\":\"01ARZ3NDEKTSV4RRFFQ69G5FAV\"}"
                            .to_owned(),
                    ),
                    SqliteParameter::Text("\"2026-08-05T09:00:00Z\"".to_owned()),
                    SqliteParameter::Text(machine_id.as_str().to_owned()),
                ],
            )
        );
    }

    #[test]
    fn admission_cleanup_is_fenced_by_the_exact_document_it_wrote() {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let admitted = r#"{"name":"admitted"}"#.to_owned();
        let concurrently_replaced = r#"{"name":"replacement"}"#.to_owned();

        let machine_cleanup =
            conditional_delete_statement(CorrosionTable::Machines, id, admitted.clone());
        assert_eq!(
            machine_cleanup,
            Statement::with_params(
                "DELETE FROM machines WHERE id = ? AND document = ?",
                vec![
                    SqliteParameter::Text(id.to_owned()),
                    SqliteParameter::Text(admitted.clone()),
                ],
            )
        );
        assert_ne!(
            machine_cleanup,
            conditional_delete_statement(CorrosionTable::Machines, id, concurrently_replaced)
        );
        assert_eq!(
            conditional_delete_statement(CorrosionTable::Peers, id, admitted.clone()),
            Statement::with_params(
                "DELETE FROM peers WHERE id = ? AND document = ?",
                vec![
                    SqliteParameter::Text(id.to_owned()),
                    SqliteParameter::Text(admitted),
                ],
            )
        );
    }
}
