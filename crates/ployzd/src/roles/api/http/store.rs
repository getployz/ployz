//! Bounded Corrosion reads and parameterized writes used by API mutations.

mod error;

pub(super) use error::MutationStoreError;

use serde::Serialize;

use ployz_core::corrosion::{
    AcceptedRosterPrincipal, AcceptedRow, ClusterDocument, CorrosionTable, MachineDocument,
    OperatorWriteProvenance, PeerDocument, ReadReport, SkippedRow, SqliteParameter, Statement,
    StoredRow, TokenDocument, TransactionResponse, TransactionResult, read_named_roster_rows,
    read_rows,
};
use ployz_core::ids::{ClusterName, MachineName, PeerName, TokenName};

use crate::corrosion::{CorrosionClient, StoredRowLimit, collect_stored_rows};

const MAX_MUTATION_ROWS: usize = 10_000;

#[derive(Debug)]
pub(super) struct AcceptedRoster {
    pub(super) cluster: ClusterDocument,
    pub(super) machines: Vec<AcceptedMachine>,
    pub(super) machine_skipped: Vec<SkippedRow>,
    pub(super) peers: Vec<PeerDocument>,
    pub(super) peer_skipped: Vec<SkippedRow>,
}

impl AcceptedRoster {
    pub(super) fn principals(&self) -> Vec<AcceptedRosterPrincipal> {
        self.machines
            .iter()
            .map(|machine| {
                AcceptedRosterPrincipal::machine(
                    machine.document.name.clone(),
                    machine.document.transport.clone(),
                )
            })
            .chain(self.peers.iter().map(|peer| {
                AcceptedRosterPrincipal::peer(peer.name.clone(), peer.transport.clone())
            }))
            .collect()
    }

    fn trace_reader_evidence(&self) {
        let machine_skipped = self.machine_skipped.len();
        let peer_skipped = self.peer_skipped.len();
        if machine_skipped == 0 && peer_skipped == 0 {
            return;
        }
        tracing::warn!(
            machine_skipped,
            peer_skipped,
            machine_skipped_evidence = ?self.machine_skipped,
            peer_skipped_evidence = ?self.peer_skipped,
            "accepted roster excluded stored row evidence"
        );
    }
}

#[derive(Debug, Clone)]
pub(super) struct AcceptedMachine {
    pub(super) stored_document: String,
    pub(super) document: MachineDocument,
}

#[derive(Debug, Clone)]
pub(super) struct AcceptedToken {
    pub(super) stored_document: String,
    pub(super) document: TokenDocument,
}

pub(super) async fn read_accepted_roster(
    corrosion: &CorrosionClient,
    cluster_id: &ClusterName,
) -> Result<AcceptedRoster, MutationStoreError> {
    let cluster = query_rows(corrosion, select_cluster(cluster_id));
    let machines = query_rows(corrosion, select_all(CorrosionTable::Machines));
    let peers = query_rows(corrosion, select_all(CorrosionTable::Peers));
    let (cluster_rows, machine_rows, peer_rows) = tokio::try_join!(cluster, machines, peers)?;

    let cluster = one_cluster(cluster_id, cluster_rows)?;
    let AcceptedNamedRows {
        accepted: machines,
        skipped: machine_skipped,
    } = accepted_machine_rows(read_named_roster_rows::<MachineDocument>(
        &cluster,
        machine_rows,
    ))?;
    let AcceptedNamedRows {
        accepted: peers,
        skipped: peer_skipped,
    } = accepted_peer_rows(read_named_roster_rows::<PeerDocument>(&cluster, peer_rows))?;
    let roster = AcceptedRoster {
        cluster,
        machines,
        machine_skipped,
        peers,
        peer_skipped,
    };
    roster.trace_reader_evidence();
    Ok(roster)
}

#[derive(Debug, Clone)]
pub(super) struct AcceptedCluster {
    pub(super) document: ClusterDocument,
    pub(super) stored_document: String,
}

pub(super) async fn read_cluster(
    corrosion: &CorrosionClient,
    cluster_id: &ClusterName,
) -> Result<AcceptedCluster, MutationStoreError> {
    let row = one_cluster_row(
        cluster_id,
        query_rows(corrosion, select_cluster(cluster_id)).await?,
    )?;
    Ok(AcceptedCluster {
        stored_document: row.source.document,
        document: row.value,
    })
}

pub(super) async fn read_named_removal_rows(
    corrosion: &CorrosionClient,
    table: CorrosionTable,
) -> Result<Vec<StoredRow>, MutationStoreError> {
    ensure_named_removal_table(table, "")?;
    query_rows(corrosion, select_all(table)).await
}

#[derive(Debug)]
struct AcceptedNamedRows<Row> {
    accepted: Vec<Row>,
    skipped: Vec<SkippedRow>,
}

fn accepted_machine_rows(
    report: ReadReport<MachineDocument>,
) -> Result<AcceptedNamedRows<AcceptedMachine>, MutationStoreError> {
    let ReadReport { accepted, skipped } = report;
    let accepted = accepted
        .into_iter()
        .map(|row| {
            let stored_document = row.source.document;
            AcceptedMachine {
                stored_document,
                document: row.value,
            }
        })
        .collect::<Vec<_>>();
    Ok(AcceptedNamedRows { accepted, skipped })
}

fn accepted_peer_rows(
    report: ReadReport<PeerDocument>,
) -> Result<AcceptedNamedRows<PeerDocument>, MutationStoreError> {
    let ReadReport { accepted, skipped } = report;
    let accepted = accepted
        .into_iter()
        .map(|row| row.value)
        .collect::<Vec<_>>();
    Ok(AcceptedNamedRows { accepted, skipped })
}

pub(super) async fn read_tokens(
    corrosion: &CorrosionClient,
    cluster_id: &ClusterName,
) -> Result<Vec<(TokenName, TokenDocument)>, MutationStoreError> {
    let rows = query_rows(corrosion, select_all(CorrosionTable::Tokens)).await?;
    read_rows::<TokenDocument>(cluster_id, rows)
        .accepted
        .into_iter()
        .map(|row| {
            Ok((
                TokenName::try_new(row.source.key.clone()).map_err(|error| {
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
    cluster_id: &ClusterName,
    token_id: &TokenName,
) -> Result<Option<AcceptedToken>, MutationStoreError> {
    let rows = query_rows(
        corrosion,
        select_by_id(CorrosionTable::Tokens, token_id.as_str()),
    )
    .await?;
    let report = read_rows::<TokenDocument>(cluster_id, rows);
    let mut accepted = report.accepted.into_iter();
    let token = accepted.next().map(|row| AcceptedToken {
        stored_document: row.source.document,
        document: row.value,
    });
    if accepted.next().is_some() {
        return Err(MutationStoreError::DuplicatePrimaryKey {
            table: CorrosionTable::Tokens,
            id: token_id.as_str().to_owned(),
        });
    }
    Ok(token)
}

/// Reports whether the exact token primary key is occupied without interpreting
/// its document. A skipped row still blocks a same-name insert.
pub(super) async fn token_name_is_occupied(
    corrosion: &CorrosionClient,
    token_id: &TokenName,
) -> Result<bool, MutationStoreError> {
    let rows = query_rows(
        corrosion,
        select_by_id(CorrosionTable::Tokens, token_id.as_str()),
    )
    .await?;
    Ok(raw_token_name_is_occupied(&rows))
}

fn raw_token_name_is_occupied(rows: &[StoredRow]) -> bool {
    !rows.is_empty()
}

pub(super) async fn read_machine(
    corrosion: &CorrosionClient,
    cluster: &ClusterDocument,
    machine_id: &MachineName,
) -> Result<Option<MachineDocument>, MutationStoreError> {
    Ok(
        match read_machine_row(corrosion, cluster, machine_id).await? {
            NamedRow::Accepted(document) => Some(document),
            NamedRow::Vacant | NamedRow::OccupiedRejected => None,
        },
    )
}

#[derive(Debug)]
pub(super) enum NamedRow<Document> {
    Vacant,
    Accepted(Document),
    OccupiedRejected,
}

pub(super) async fn read_machine_row(
    corrosion: &CorrosionClient,
    cluster: &ClusterDocument,
    machine_id: &MachineName,
) -> Result<NamedRow<MachineDocument>, MutationStoreError> {
    let rows = query_rows(
        corrosion,
        select_by_id(CorrosionTable::Machines, machine_id.as_str()),
    )
    .await?;
    named_row(
        CorrosionTable::Machines,
        machine_id.as_str(),
        read_named_roster_rows::<MachineDocument>(cluster, rows),
    )
}

pub(super) async fn read_peer_row(
    corrosion: &CorrosionClient,
    cluster: &ClusterDocument,
    peer_id: &PeerName,
) -> Result<NamedRow<PeerDocument>, MutationStoreError> {
    let rows = query_rows(
        corrosion,
        select_by_id(CorrosionTable::Peers, peer_id.as_str()),
    )
    .await?;
    named_row(
        CorrosionTable::Peers,
        peer_id.as_str(),
        read_named_roster_rows::<PeerDocument>(cluster, rows),
    )
}

fn named_row<Document>(
    table: CorrosionTable,
    id: &str,
    report: ReadReport<Document>,
) -> Result<NamedRow<Document>, MutationStoreError> {
    if report.accepted.len() + report.skipped.len() > 1 {
        return Err(MutationStoreError::DuplicatePrimaryKey {
            table,
            id: id.to_owned(),
        });
    }
    if let Some(row) = report.accepted.into_iter().next() {
        Ok(NamedRow::Accepted(row.value))
    } else if report.skipped.is_empty() {
        Ok(NamedRow::Vacant)
    } else {
        Ok(NamedRow::OccupiedRejected)
    }
}

pub(super) async fn insert_token(
    corrosion: &CorrosionClient,
    token_id: &TokenName,
    document: &TokenDocument,
) -> Result<(), MutationStoreError> {
    let document = encode_document(CorrosionTable::Tokens, document)?;
    corrosion
        .execute(&[insert_token_statement(token_id, document)])
        .await?;
    Ok(())
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
    let document = encode_document(table, document)?;
    corrosion
        .execute(&[insert_statement(table, id, document)])
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TokenAuthorizedInsert {
    Inserted,
    TokenUnavailable,
}

/// Commits membership only while the exact token document validated by this
/// API instance still exists and remains unexpired at the SQLite commit point
/// in its machine-local Corrosion replica. A revoke committed on the same
/// replica therefore orders before or after this write; other machines observe
/// that delete according to Corrosion's replication convergence and do not gain
/// a stronger cross-replica ordering guarantee.
pub(super) async fn insert_machine_if_token_matches(
    corrosion: &CorrosionClient,
    machine_id: &MachineName,
    document: &MachineDocument,
    token_id: &TokenName,
    validated_token: &TokenDocument,
) -> Result<TokenAuthorizedInsert, MutationStoreError> {
    insert_typed_document_if_token_matches(
        corrosion,
        CorrosionTable::Machines,
        machine_id.as_str(),
        document,
        token_id,
        validated_token,
    )
    .await
}

pub(super) async fn insert_peer_if_token_matches(
    corrosion: &CorrosionClient,
    peer_id: &PeerName,
    document: &PeerDocument,
    token_id: &TokenName,
    validated_token: &TokenDocument,
) -> Result<TokenAuthorizedInsert, MutationStoreError> {
    insert_typed_document_if_token_matches(
        corrosion,
        CorrosionTable::Peers,
        peer_id.as_str(),
        document,
        token_id,
        validated_token,
    )
    .await
}

async fn insert_typed_document_if_token_matches<Document>(
    corrosion: &CorrosionClient,
    table: CorrosionTable,
    id: &str,
    document: &Document,
    token_id: &TokenName,
    validated_token: &TokenDocument,
) -> Result<TokenAuthorizedInsert, MutationStoreError>
where
    Document: Serialize + ?Sized,
{
    let document = encode_document(table, document)?;
    let validated_token = encode_document(CorrosionTable::Tokens, validated_token)?;
    let response = corrosion
        .execute(&[token_authorized_insert_statement(
            table,
            id,
            document,
            token_id,
            validated_token,
        )])
        .await?;
    token_authorized_insert_outcome(table, id, &response)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConditionalMachineReplace {
    Replaced,
    Stale,
}

pub(super) async fn update_wireguard_endpoint_if_matches(
    corrosion: &CorrosionClient,
    machine_id: &MachineName,
    observed: &str,
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
    let response = corrosion
        .execute(&[update_wireguard_endpoint_statement(
            machine_id,
            observed.to_owned(),
            endpoint,
            written_by,
            written_at,
        )])
        .await?;
    match conditional_machine_replace_outcome(machine_id, &response)? {
        ConditionalMachineReplace::Replaced => Ok(()),
        ConditionalMachineReplace::Stale => Err(MutationStoreError::ConcurrentMachineMutation {
            machine_id: machine_id.clone(),
        }),
    }
}

pub(super) async fn delete_token_if_matches(
    corrosion: &CorrosionClient,
    token_id: &TokenName,
    expected: String,
) -> Result<ConditionalNamedDelete, MutationStoreError> {
    let response = corrosion
        .execute(&[delete_token_if_matches_statement(token_id, expected)])
        .await?;
    conditional_delete_outcome(CorrosionTable::Tokens, token_id.as_str(), &response, 1, 0)
}

/// Sweeps testimony only while the exact resolved roster row exists, then
/// deletes that row last. The fixed batch never touches operation evidence.
pub(super) async fn remove_machine_and_sweep(
    corrosion: &CorrosionClient,
    machine_id: &MachineName,
    expected: &str,
) -> Result<ConditionalNamedDelete, MutationStoreError> {
    let statements = machine_removal_statements(machine_id, expected);
    let response = corrosion.execute(&statements).await?;
    conditional_delete_outcome(
        CorrosionTable::Machines,
        machine_id.as_str(),
        &response,
        statements.len(),
        statements.len() - 1,
    )
}

pub(super) async fn delete_machine_if_matches(
    corrosion: &CorrosionClient,
    machine_id: &MachineName,
    expected: &MachineDocument,
) -> Result<(), MutationStoreError> {
    let expected = encode_document(CorrosionTable::Machines, expected)?;
    corrosion
        .execute(&[delete_machine_if_matches_statement(machine_id, expected)])
        .await?;
    Ok(())
}

pub(super) async fn delete_peer_if_matches(
    corrosion: &CorrosionClient,
    peer_id: &PeerName,
    expected: &PeerDocument,
) -> Result<(), MutationStoreError> {
    let expected = encode_document(CorrosionTable::Peers, expected)?;
    corrosion
        .execute(&[delete_peer_if_matches_statement(peer_id, expected)])
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConditionalNamedDelete {
    Deleted,
    ConcurrentMutation,
}

pub(super) async fn delete_named_if_matches(
    corrosion: &CorrosionClient,
    table: CorrosionTable,
    id: &str,
    expected: String,
) -> Result<ConditionalNamedDelete, MutationStoreError> {
    ensure_exact_row_removal_table(table, id)?;
    let response = corrosion
        .execute(&[conditional_delete_statement(table, id, expected)])
        .await?;
    conditional_named_delete_outcome(table, id, &response)
}

pub(super) async fn delete_peer_if_cluster_and_row_match(
    corrosion: &CorrosionClient,
    cluster_id: &ClusterName,
    expected_cluster: String,
    peer_id: &PeerName,
    expected_peer: String,
) -> Result<ConditionalNamedDelete, MutationStoreError> {
    let response = corrosion
        .execute(&[delete_peer_if_cluster_and_row_match_statement(
            cluster_id,
            expected_cluster,
            peer_id,
            expected_peer,
        )])
        .await?;
    conditional_named_delete_outcome(CorrosionTable::Peers, peer_id.as_str(), &response)
}

fn conditional_named_delete_outcome(
    table: CorrosionTable,
    id: &str,
    response: &TransactionResponse,
) -> Result<ConditionalNamedDelete, MutationStoreError> {
    conditional_delete_outcome(table, id, response, 1, 0)
}

fn conditional_delete_outcome(
    table: CorrosionTable,
    id: &str,
    response: &TransactionResponse,
    expected_results: usize,
    guarded_result: usize,
) -> Result<ConditionalNamedDelete, MutationStoreError> {
    if response.results.len() != expected_results {
        return Err(MutationStoreError::UnexpectedWriteResult {
            table,
            id: id.to_owned(),
            detail: format!("transaction returned {} results", response.results.len()),
        });
    }
    let Some(result) = response.results.get(guarded_result) else {
        return Err(MutationStoreError::UnexpectedWriteResult {
            table,
            id: id.to_owned(),
            detail: "conditional statement result was missing".to_owned(),
        });
    };
    let TransactionResult::Success(result) = result else {
        return Err(MutationStoreError::UnexpectedWriteResult {
            table,
            id: id.to_owned(),
            detail: "transaction retained a statement error".to_owned(),
        });
    };
    match result.rows_affected {
        0 => Ok(ConditionalNamedDelete::ConcurrentMutation),
        1 => Ok(ConditionalNamedDelete::Deleted),
        rows_affected => Err(MutationStoreError::UnexpectedWriteResult {
            table,
            id: id.to_owned(),
            detail: format!("conditional delete affected {rows_affected} rows"),
        }),
    }
}

fn ensure_named_removal_table(table: CorrosionTable, id: &str) -> Result<(), MutationStoreError> {
    match table {
        CorrosionTable::Peers | CorrosionTable::Namespaces | CorrosionTable::RouteBindings => {
            Ok(())
        }
        CorrosionTable::Cluster
        | CorrosionTable::Machines
        | CorrosionTable::Tokens
        | CorrosionTable::MachineEndpoints
        | CorrosionTable::MachineStatus
        | CorrosionTable::GatewayObservations
        | CorrosionTable::Operations
        | CorrosionTable::Controller
        | CorrosionTable::CertHoldings
        | CorrosionTable::AcmeHttp01 => Err(MutationStoreError::UnexpectedWriteResult {
            table,
            id: id.to_owned(),
            detail: "table is not a named-removal target".to_owned(),
        }),
    }
}

fn ensure_exact_row_removal_table(
    table: CorrosionTable,
    id: &str,
) -> Result<(), MutationStoreError> {
    match table {
        CorrosionTable::Namespaces | CorrosionTable::RouteBindings => Ok(()),
        CorrosionTable::Cluster
        | CorrosionTable::Machines
        | CorrosionTable::Peers
        | CorrosionTable::Tokens
        | CorrosionTable::MachineEndpoints
        | CorrosionTable::MachineStatus
        | CorrosionTable::GatewayObservations
        | CorrosionTable::Operations
        | CorrosionTable::Controller
        | CorrosionTable::CertHoldings
        | CorrosionTable::AcmeHttp01 => Err(MutationStoreError::UnexpectedWriteResult {
            table,
            id: id.to_owned(),
            detail: "table requires a different deletion fence".to_owned(),
        }),
    }
}

fn encode_document<Document>(
    table: CorrosionTable,
    document: &Document,
) -> Result<String, MutationStoreError>
where
    Document: Serialize + ?Sized,
{
    serde_json::to_string(document).map_err(|error| MutationStoreError::Encode {
        table,
        detail: error.to_string(),
    })
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
    cluster_id: &ClusterName,
    rows: Vec<StoredRow>,
) -> Result<ClusterDocument, MutationStoreError> {
    Ok(one_cluster_row(cluster_id, rows)?.value)
}

fn one_cluster_row(
    cluster_id: &ClusterName,
    rows: Vec<StoredRow>,
) -> Result<AcceptedRow<ClusterDocument>, MutationStoreError> {
    if rows.is_empty() {
        return Err(MutationStoreError::MissingCluster);
    }
    let mut accepted = read_rows::<ClusterDocument>(cluster_id, rows)
        .accepted
        .into_iter();
    let Some(row) = accepted.next() else {
        return Err(MutationStoreError::InvalidCluster);
    };
    if accepted.next().is_some() || row.source.key != cluster_id.as_str() {
        return Err(MutationStoreError::InvalidCluster);
    }
    Ok(row)
}

fn select_cluster(cluster_id: &ClusterName) -> Statement {
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

fn insert_token_statement(token_id: &TokenName, document: String) -> Statement {
    insert_statement(CorrosionTable::Tokens, token_id.as_str(), document)
}

fn token_authorized_insert_statement(
    table: CorrosionTable,
    id: &str,
    document: String,
    token_id: &TokenName,
    validated_token: String,
) -> Statement {
    Statement::with_params(
        format!(
            "INSERT INTO {} (id, document) SELECT ?, ? WHERE EXISTS (SELECT 1 FROM tokens WHERE id = ? AND document = ? AND julianday(json_extract(document, '$.expires_at')) > julianday('now'))",
            table.as_str()
        ),
        vec![
            SqliteParameter::Text(id.to_owned()),
            SqliteParameter::Text(document),
            SqliteParameter::Text(token_id.as_str().to_owned()),
            SqliteParameter::Text(validated_token),
        ],
    )
}

fn token_authorized_insert_outcome(
    table: CorrosionTable,
    id: &str,
    response: &TransactionResponse,
) -> Result<TokenAuthorizedInsert, MutationStoreError> {
    let [result] = response.results.as_slice() else {
        return Err(MutationStoreError::UnexpectedWriteResult {
            table,
            id: id.to_owned(),
            detail: format!("transaction returned {} results", response.results.len()),
        });
    };
    let TransactionResult::Success(result) = result else {
        return Err(MutationStoreError::UnexpectedWriteResult {
            table,
            id: id.to_owned(),
            detail: "transaction retained a statement error".to_owned(),
        });
    };
    match result.rows_affected {
        0 => Ok(TokenAuthorizedInsert::TokenUnavailable),
        1 => Ok(TokenAuthorizedInsert::Inserted),
        rows_affected => Err(MutationStoreError::UnexpectedWriteResult {
            table,
            id: id.to_owned(),
            detail: format!("conditional insert affected {rows_affected} rows"),
        }),
    }
}

#[cfg(test)]
fn replace_if_matches_statement(
    table: CorrosionTable,
    id: &str,
    observed: String,
    replacement: String,
) -> Statement {
    Statement::with_params(
        format!(
            "UPDATE {} SET document = ? WHERE id = ? AND document = ?",
            table.as_str()
        ),
        vec![
            SqliteParameter::Text(replacement),
            SqliteParameter::Text(id.to_owned()),
            SqliteParameter::Text(observed),
        ],
    )
}

#[cfg(test)]
fn replace_machine_if_matches_statement(
    machine_id: &MachineName,
    observed: String,
    replacement: String,
) -> Statement {
    replace_if_matches_statement(
        CorrosionTable::Machines,
        machine_id.as_str(),
        observed,
        replacement,
    )
}

fn conditional_machine_replace_outcome(
    machine_id: &MachineName,
    response: &TransactionResponse,
) -> Result<ConditionalMachineReplace, MutationStoreError> {
    let [result] = response.results.as_slice() else {
        return Err(MutationStoreError::UnexpectedWriteResult {
            table: CorrosionTable::Machines,
            id: machine_id.as_str().to_owned(),
            detail: format!("transaction returned {} results", response.results.len()),
        });
    };
    let TransactionResult::Success(result) = result else {
        return Err(MutationStoreError::UnexpectedWriteResult {
            table: CorrosionTable::Machines,
            id: machine_id.as_str().to_owned(),
            detail: "transaction retained a statement error".to_owned(),
        });
    };
    match result.rows_affected {
        0 => Ok(ConditionalMachineReplace::Stale),
        1 => Ok(ConditionalMachineReplace::Replaced),
        rows_affected => Err(MutationStoreError::UnexpectedWriteResult {
            table: CorrosionTable::Machines,
            id: machine_id.as_str().to_owned(),
            detail: format!("conditional replacement affected {rows_affected} rows"),
        }),
    }
}

fn delete_token_if_matches_statement(token_id: &TokenName, expected: String) -> Statement {
    conditional_delete_statement(CorrosionTable::Tokens, token_id.as_str(), expected)
}

fn machine_removal_statements(machine_id: &MachineName, expected: &str) -> [Statement; 6] {
    [
        delete_testimony_for_machine_statement(CorrosionTable::MachineStatus, machine_id, expected),
        delete_testimony_for_machine_statement(
            CorrosionTable::GatewayObservations,
            machine_id,
            expected,
        ),
        delete_testimony_for_machine_statement(
            CorrosionTable::MachineEndpoints,
            machine_id,
            expected,
        ),
        delete_testimony_for_machine_statement(CorrosionTable::CertHoldings, machine_id, expected),
        delete_testimony_for_machine_statement(CorrosionTable::AcmeHttp01, machine_id, expected),
        conditional_delete_statement(
            CorrosionTable::Machines,
            machine_id.as_str(),
            expected.to_owned(),
        ),
    ]
}

fn delete_testimony_for_machine_statement(
    table: CorrosionTable,
    machine_id: &MachineName,
    expected: &str,
) -> Statement {
    match table {
        CorrosionTable::MachineStatus
        | CorrosionTable::GatewayObservations
        | CorrosionTable::CertHoldings
        | CorrosionTable::AcmeHttp01 => Statement::with_params(
            format!(
                "DELETE FROM {} WHERE machine_id = ? AND EXISTS (SELECT 1 FROM machines WHERE id = ? AND document = ?)",
                table.as_str()
            ),
            vec![
                SqliteParameter::Text(machine_id.as_str().to_owned()),
                SqliteParameter::Text(machine_id.as_str().to_owned()),
                SqliteParameter::Text(expected.to_owned()),
            ],
        ),
        CorrosionTable::MachineEndpoints => Statement::with_params(
            "DELETE FROM machine_endpoints WHERE id = ? AND EXISTS (SELECT 1 FROM machines WHERE id = ? AND document = ?)",
            vec![
                SqliteParameter::Text(machine_id.as_str().to_owned()),
                SqliteParameter::Text(machine_id.as_str().to_owned()),
                SqliteParameter::Text(expected.to_owned()),
            ],
        ),
        CorrosionTable::Cluster
        | CorrosionTable::Machines
        | CorrosionTable::Peers
        | CorrosionTable::Tokens
        | CorrosionTable::Namespaces
        | CorrosionTable::RouteBindings
        | CorrosionTable::Operations
        | CorrosionTable::Controller => {
            unreachable!("only machine-authority testimony tables are removable")
        }
    }
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

fn delete_peer_if_cluster_and_row_match_statement(
    cluster_id: &ClusterName,
    expected_cluster: String,
    peer_id: &PeerName,
    expected_peer: String,
) -> Statement {
    Statement::with_params(
        "DELETE FROM peers WHERE id = ? AND document = ? AND EXISTS (SELECT 1 FROM cluster WHERE id = ? AND document = ?)",
        vec![
            SqliteParameter::Text(peer_id.as_str().to_owned()),
            SqliteParameter::Text(expected_peer),
            SqliteParameter::Text(cluster_id.as_str().to_owned()),
            SqliteParameter::Text(expected_cluster),
        ],
    )
}

fn delete_machine_if_matches_statement(machine_id: &MachineName, expected: String) -> Statement {
    conditional_delete_statement(CorrosionTable::Machines, machine_id.as_str(), expected)
}

fn delete_peer_if_matches_statement(peer_id: &PeerName, expected: String) -> Statement {
    conditional_delete_statement(CorrosionTable::Peers, peer_id.as_str(), expected)
}

fn update_wireguard_endpoint_statement(
    machine_id: &MachineName,
    observed: String,
    endpoint: String,
    written_by: String,
    written_at: String,
) -> Statement {
    Statement::with_params(
        "UPDATE machines SET document = json_set(document, '$.transport.endpoint', json(?), '$.written_by', json(?), '$.written_at', json(?)) WHERE id = ? AND document = ?",
        vec![
            SqliteParameter::Text(endpoint),
            SqliteParameter::Text(written_by),
            SqliteParameter::Text(written_at),
            SqliteParameter::Text(machine_id.as_str().to_owned()),
            SqliteParameter::Text(observed),
        ],
    )
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::{
        RowSkipReason, TransactionResponse, TransactionResult, TransactionSuccess,
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn token_name_occupancy_does_not_depend_on_document_acceptance() {
        assert!(!raw_token_name_is_occupied(&[]));
        assert!(raw_token_name_is_occupied(&[StoredRow::new(
            "bootstrap",
            "not JSON",
        )]));
        assert!(raw_token_name_is_occupied(&[StoredRow::new(
            "bootstrap",
            r#"{"v":2,"cluster_id":"foreign"}"#,
        )]));
    }

    #[test]
    fn rejected_named_roster_row_is_occupied_not_vacant() {
        let rejected = named_row::<MachineDocument>(
            CorrosionTable::Machines,
            "edge-a",
            ReadReport {
                accepted: Vec::new(),
                skipped: vec![SkippedRow {
                    source: StoredRow::new("edge-a", "not accepted"),
                    reason: RowSkipReason::Empty,
                }],
            },
        )
        .expect("one primary-key row");
        assert!(matches!(rejected, NamedRow::OccupiedRejected));

        let vacant = named_row::<MachineDocument>(
            CorrosionTable::Machines,
            "edge-a",
            ReadReport {
                accepted: Vec::new(),
                skipped: Vec::new(),
            },
        )
        .expect("empty primary-key lookup");
        assert!(matches!(vacant, NamedRow::Vacant));
    }

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
            replace_if_matches_statement(
                CorrosionTable::Machines,
                id,
                "{\"generation\":1}".to_owned(),
                "{\"generation\":2}".to_owned(),
            ),
            Statement::with_params(
                "UPDATE machines SET document = ? WHERE id = ? AND document = ?",
                vec![
                    SqliteParameter::Text("{\"generation\":2}".to_owned()),
                    SqliteParameter::Text(id.to_owned()),
                    SqliteParameter::Text("{\"generation\":1}".to_owned()),
                ],
            )
        );
    }

    #[test]
    fn named_removals_bind_exact_table_rows_and_peer_cluster_provider_fence() {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let document = "{\"v\":1}";
        for table in [
            CorrosionTable::Peers,
            CorrosionTable::Namespaces,
            CorrosionTable::RouteBindings,
        ] {
            ensure_named_removal_table(table, id).expect("named table is removable");
        }
        for table in [CorrosionTable::Namespaces, CorrosionTable::RouteBindings] {
            ensure_exact_row_removal_table(table, id).expect("ordinary named table is removable");
            assert_eq!(
                conditional_delete_statement(table, id, document.to_owned()),
                Statement::with_params(
                    format!(
                        "DELETE FROM {} WHERE id = ? AND document = ?",
                        table.as_str()
                    ),
                    vec![
                        SqliteParameter::Text(id.to_owned()),
                        SqliteParameter::Text(document.to_owned()),
                    ],
                )
            );
        }

        let cluster_id = ClusterName::try_new(id).expect("cluster id");
        let peer_id = PeerName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("peer id");
        assert_eq!(
            delete_peer_if_cluster_and_row_match_statement(
                &cluster_id,
                "{\"provider\":\"builtin_wireguard\"}".to_owned(),
                &peer_id,
                document.to_owned(),
            ),
            Statement::with_params(
                "DELETE FROM peers WHERE id = ? AND document = ? AND EXISTS (SELECT 1 FROM cluster WHERE id = ? AND document = ?)",
                vec![
                    SqliteParameter::Text(peer_id.as_str().to_owned()),
                    SqliteParameter::Text(document.to_owned()),
                    SqliteParameter::Text(cluster_id.as_str().to_owned()),
                    SqliteParameter::Text("{\"provider\":\"builtin_wireguard\"}".to_owned()),
                ],
            )
        );
        assert!(matches!(
            ensure_exact_row_removal_table(CorrosionTable::Peers, peer_id.as_str()),
            Err(MutationStoreError::UnexpectedWriteResult { .. })
        ));
        let changed_cluster_or_peer = TransactionResponse {
            results: vec![TransactionResult::Success(TransactionSuccess {
                rows_affected: 0,
                time: 0.01,
            })],
            time: 0.01,
            version: None,
            actor_id: None,
        };
        assert_eq!(
            conditional_named_delete_outcome(
                CorrosionTable::Peers,
                peer_id.as_str(),
                &changed_cluster_or_peer,
            )
            .expect("a changed cluster or peer is a typed concurrent mutation"),
            ConditionalNamedDelete::ConcurrentMutation
        );

        for table in [
            CorrosionTable::Cluster,
            CorrosionTable::Machines,
            CorrosionTable::Tokens,
            CorrosionTable::MachineEndpoints,
            CorrosionTable::MachineStatus,
            CorrosionTable::Operations,
            CorrosionTable::CertHoldings,
            CorrosionTable::AcmeHttp01,
        ] {
            assert!(matches!(
                ensure_named_removal_table(table, id),
                Err(MutationStoreError::UnexpectedWriteResult {
                    table: found,
                    ..
                }) if found == table
            ));
        }
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
    fn machine_removal_sweeps_only_while_the_exact_observed_roster_row_exists() {
        let machine_id = MachineName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("machine id");
        let observed = r#"{"name":"edge-a"}"#;
        let statements = machine_removal_statements(&machine_id, observed);
        let parameter = SqliteParameter::Text(machine_id.as_str().to_owned());
        let expected = SqliteParameter::Text(observed.to_owned());
        assert_eq!(
            statements,
            [
                Statement::with_params(
                    "DELETE FROM machine_status WHERE machine_id = ? AND EXISTS (SELECT 1 FROM machines WHERE id = ? AND document = ?)",
                    vec![parameter.clone(), parameter.clone(), expected.clone()],
                ),
                Statement::with_params(
                    "DELETE FROM gateway_observations WHERE machine_id = ? AND EXISTS (SELECT 1 FROM machines WHERE id = ? AND document = ?)",
                    vec![parameter.clone(), parameter.clone(), expected.clone()],
                ),
                Statement::with_params(
                    "DELETE FROM machine_endpoints WHERE id = ? AND EXISTS (SELECT 1 FROM machines WHERE id = ? AND document = ?)",
                    vec![parameter.clone(), parameter.clone(), expected.clone()],
                ),
                Statement::with_params(
                    "DELETE FROM cert_holdings WHERE machine_id = ? AND EXISTS (SELECT 1 FROM machines WHERE id = ? AND document = ?)",
                    vec![parameter.clone(), parameter.clone(), expected.clone()],
                ),
                Statement::with_params(
                    "DELETE FROM acme_http01 WHERE machine_id = ? AND EXISTS (SELECT 1 FROM machines WHERE id = ? AND document = ?)",
                    vec![parameter.clone(), parameter.clone(), expected.clone()],
                ),
                Statement::with_params(
                    "DELETE FROM machines WHERE id = ? AND document = ?",
                    vec![parameter, expected],
                ),
            ]
        );
        let operations_sentinel = Statement::with_params(
            "DELETE FROM operations WHERE machine_id = ?",
            vec![SqliteParameter::Text(machine_id.as_str().to_owned())],
        );
        assert!(
            !statements.contains(&operations_sentinel),
            "machine removal must preserve operation evidence"
        );

        for (rows_affected, expected_outcome) in [
            (0, ConditionalNamedDelete::ConcurrentMutation),
            (1, ConditionalNamedDelete::Deleted),
        ] {
            let mut results = (0..5)
                .map(|_| {
                    TransactionResult::Success(TransactionSuccess {
                        rows_affected: 0,
                        time: 0.01,
                    })
                })
                .collect::<Vec<_>>();
            results.push(TransactionResult::Success(TransactionSuccess {
                rows_affected,
                time: 0.01,
            }));
            let response = TransactionResponse {
                results,
                time: 0.01,
                version: None,
                actor_id: None,
            };
            assert_eq!(
                conditional_delete_outcome(
                    CorrosionTable::Machines,
                    machine_id.as_str(),
                    &response,
                    6,
                    5,
                )
                .expect("the final exact delete decides the batch outcome"),
                expected_outcome,
            );
        }
    }

    #[test]
    fn token_revocation_is_fenced_by_the_exact_observed_document() {
        let token_id = TokenName::try_new("bootstrap").expect("token id");
        assert_eq!(
            delete_token_if_matches_statement(&token_id, "observed".to_owned()),
            Statement::with_params(
                "DELETE FROM tokens WHERE id = ? AND document = ?",
                vec![
                    SqliteParameter::Text("bootstrap".to_owned()),
                    SqliteParameter::Text("observed".to_owned()),
                ],
            )
        );

        for (rows_affected, expected) in [
            (0, ConditionalNamedDelete::ConcurrentMutation),
            (1, ConditionalNamedDelete::Deleted),
        ] {
            let response = TransactionResponse {
                results: vec![TransactionResult::Success(TransactionSuccess {
                    rows_affected,
                    time: 0.01,
                })],
                time: 0.01,
                version: None,
                actor_id: None,
            };
            assert_eq!(
                conditional_delete_outcome(
                    CorrosionTable::Tokens,
                    token_id.as_str(),
                    &response,
                    1,
                    0,
                )
                .expect("zero and one row are typed outcomes"),
                expected,
            );
        }
    }

    #[test]
    fn endpoint_update_changes_only_endpoint_and_provenance_with_bound_parameters() {
        let machine_id = MachineName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("machine id");
        assert_eq!(
            update_wireguard_endpoint_statement(
                &machine_id,
                "{\"generation\":1}".to_owned(),
                "\"203.0.113.10:51820\"".to_owned(),
                "{\"kind\":\"machine\",\"machine_id\":\"01ARZ3NDEKTSV4RRFFQ69G5FAV\"}".to_owned(),
                "\"2026-08-05T09:00:00Z\"".to_owned(),
            ),
            Statement::with_params(
                "UPDATE machines SET document = json_set(document, '$.transport.endpoint', json(?), '$.written_by', json(?), '$.written_at', json(?)) WHERE id = ? AND document = ?",
                vec![
                    SqliteParameter::Text("\"203.0.113.10:51820\"".to_owned()),
                    SqliteParameter::Text(
                        "{\"kind\":\"machine\",\"machine_id\":\"01ARZ3NDEKTSV4RRFFQ69G5FAV\"}"
                            .to_owned(),
                    ),
                    SqliteParameter::Text("\"2026-08-05T09:00:00Z\"".to_owned()),
                    SqliteParameter::Text(machine_id.as_str().to_owned()),
                    SqliteParameter::Text("{\"generation\":1}".to_owned()),
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

    #[test]
    fn peer_removal_reports_a_stale_exact_document_fence() {
        let peer_id = PeerName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("peer id");
        assert_eq!(
            delete_peer_if_matches_statement(&peer_id, r#"{"name":"observed"}"#.to_owned()),
            Statement::with_params(
                "DELETE FROM peers WHERE id = ? AND document = ?",
                vec![
                    SqliteParameter::Text(peer_id.as_str().to_owned()),
                    SqliteParameter::Text(r#"{"name":"observed"}"#.to_owned()),
                ],
            )
        );
    }

    #[test]
    fn admission_paused_after_validation_cannot_commit_after_token_revoke_or_expiry() {
        let member_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let token_id = TokenName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("token id");
        let member = r#"{"name":"edge-a"}"#.to_owned();
        let validated_token = r#"{"secret_sha256":"validated"}"#.to_owned();

        assert_eq!(
            token_authorized_insert_statement(
                CorrosionTable::Machines,
                member_id,
                member.clone(),
                &token_id,
                validated_token.clone(),
            ),
            Statement::with_params(
                "INSERT INTO machines (id, document) SELECT ?, ? WHERE EXISTS (SELECT 1 FROM tokens WHERE id = ? AND document = ? AND julianday(json_extract(document, '$.expires_at')) > julianday('now'))",
                vec![
                    SqliteParameter::Text(member_id.to_owned()),
                    SqliteParameter::Text(member),
                    SqliteParameter::Text(token_id.as_str().to_owned()),
                    SqliteParameter::Text(validated_token),
                ],
            )
        );

        let commit_after_revoke = TransactionResponse {
            results: vec![TransactionResult::Success(TransactionSuccess {
                rows_affected: 0,
                time: 0.01,
            })],
            time: 0.01,
            version: None,
            actor_id: None,
        };
        assert_eq!(
            token_authorized_insert_outcome(
                CorrosionTable::Machines,
                member_id,
                &commit_after_revoke,
            )
            .expect("a zero-row conditional insert is a valid refusal"),
            TokenAuthorizedInsert::TokenUnavailable,
        );
    }

    #[test]
    fn subnet_replacement_is_fenced_by_the_exact_observed_machine() {
        let machine_id = MachineName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("machine id");
        assert_eq!(
            replace_machine_if_matches_statement(
                &machine_id,
                "{\"subnet\":\"old\"}".to_owned(),
                "{\"subnet\":\"new\"}".to_owned(),
            ),
            Statement::with_params(
                "UPDATE machines SET document = ? WHERE id = ? AND document = ?",
                vec![
                    SqliteParameter::Text("{\"subnet\":\"new\"}".to_owned()),
                    SqliteParameter::Text(machine_id.as_str().to_owned()),
                    SqliteParameter::Text("{\"subnet\":\"old\"}".to_owned()),
                ],
            )
        );

        for (rows_affected, expected) in [
            (0, ConditionalMachineReplace::Stale),
            (1, ConditionalMachineReplace::Replaced),
        ] {
            let response = TransactionResponse {
                results: vec![TransactionResult::Success(TransactionSuccess {
                    rows_affected,
                    time: 0.01,
                })],
                time: 0.01,
                version: None,
                actor_id: None,
            };
            assert_eq!(
                conditional_machine_replace_outcome(&machine_id, &response)
                    .expect("zero and one row are valid convergence outcomes"),
                expected,
            );
        }
    }

    #[test]
    fn named_roster_evidence_survives_typed_machine_conversion() {
        let cluster: ClusterDocument = serde_json::from_value(json!({
            "v": 1,
            "cluster_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "name": "acme-prod",
            "storage_default": "plain",
            "hostname_mode": { "mode": "disabled" },
            "prefix": "10.210.0.0/16",
            "provider": "builtin_wireguard",
            "acme_directory_url": "https://acme.example/directory",
            "acme_contact": null,
            "written_by": {
                "kind": "peer",
                "peer_id": "01ARZ3NDEKTSV4RRFFQ69G5FAY"
            },
            "written_at": "2026-08-05T10:00:00Z"
        }))
        .expect("cluster document");
        let machine = json!({
            "v": 1,
            "cluster_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "name": "edge-a",
            "lifecycle": "active",
            "transport": {
                "kind": "wireguard",
                "pubkey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                "addr_v6": "fd00::20",
                "endpoint": null,
                "subnet_v4": "10.210.20.0/24"
            },
            "storage": { "mode": "plain", "reason": { "kind": "default" } },
            "written_by": {
                "kind": "peer",
                "peer_id": "01ARZ3NDEKTSV4RRFFQ69G5FAY"
            },
            "written_at": "2026-08-05T10:00:00Z"
        })
        .to_string();
        let report = read_named_roster_rows::<MachineDocument>(
            &cluster,
            [
                StoredRow::new("edge-a", machine.clone()),
                StoredRow::new("broken", ""),
            ],
        );

        let rows = accepted_machine_rows(report).expect("typed machine ids");

        let [accepted] = rows.accepted.as_slice() else {
            panic!("expected one accepted machine, got {}", rows.accepted.len());
        };
        assert_eq!(accepted.document.name.as_str(), "edge-a");
        assert_eq!(accepted.stored_document, machine);
        let [_skipped] = rows.skipped.as_slice() else {
            panic!("expected one skipped machine, got {}", rows.skipped.len());
        };
    }

    #[test]
    fn table_specific_statement_builders_bind_typed_ids() {
        let token_id = TokenName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("token id");
        let machine_id = MachineName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("machine id");
        let peer_id = PeerName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("peer id");

        assert_eq!(
            insert_token_statement(&token_id, "{}".to_owned()),
            insert_statement(CorrosionTable::Tokens, token_id.as_str(), "{}".to_owned())
        );
        assert_eq!(
            replace_machine_if_matches_statement(
                &machine_id,
                "{\"generation\":1}".to_owned(),
                "{\"generation\":2}".to_owned(),
            ),
            replace_if_matches_statement(
                CorrosionTable::Machines,
                machine_id.as_str(),
                "{\"generation\":1}".to_owned(),
                "{\"generation\":2}".to_owned(),
            )
        );
        assert_eq!(
            delete_token_if_matches_statement(&token_id, "{}".to_owned()),
            conditional_delete_statement(
                CorrosionTable::Tokens,
                token_id.as_str(),
                "{}".to_owned()
            )
        );
        assert_eq!(
            delete_peer_if_matches_statement(&peer_id, "{}".to_owned()),
            conditional_delete_statement(CorrosionTable::Peers, peer_id.as_str(), "{}".to_owned())
        );
    }
}
