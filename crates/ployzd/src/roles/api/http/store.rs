//! Bounded Corrosion reads and parameterized writes used by API mutations.

mod error;

pub(super) use error::MutationStoreError;

use serde::Serialize;

use ployz_core::corrosion::{
    ClusterDocument, CorrosionTable, MachineDocument, NamedReadReport, OperatorWriteProvenance,
    PeerDocument, ShadowConflict, SkippedRow, SqliteParameter, Statement, StoredRow, TokenDocument,
    TransactionResponse, TransactionResult, read_named_roster_rows, read_rows,
};
use ployz_core::ids::{ClusterId, MachineRowId, PeerId, TokenId};

use crate::corrosion::{CorrosionClient, StoredRowLimit, collect_stored_rows};

const MAX_MUTATION_ROWS: usize = 10_000;

#[derive(Debug)]
pub(super) struct AcceptedRoster {
    pub(super) cluster: ClusterDocument,
    pub(super) machines: Vec<AcceptedMachine>,
    pub(super) machine_removal_candidates: Vec<MachineRemovalCandidate>,
    pub(super) machine_skipped: Vec<SkippedRow>,
    pub(super) machine_shadows: Vec<ShadowConflict>,
    pub(super) peers: Vec<AcceptedPeer>,
    pub(super) peer_skipped: Vec<SkippedRow>,
    pub(super) peer_shadows: Vec<ShadowConflict>,
}

impl AcceptedRoster {
    /// Returns whether an exact machine row id exists in this stored roster,
    /// including entries the reader excluded from cluster truth.
    pub(super) fn contains_stored_machine_id(&self, machine_id: &MachineRowId) -> bool {
        let machine_id = machine_id.as_str();
        self.machines
            .iter()
            .any(|machine| machine.id.as_str() == machine_id)
            || self
                .machine_skipped
                .iter()
                .any(|machine| machine.source.key == machine_id)
            || self.machine_shadows.iter().any(|machine| {
                machine.winner.id.as_str() == machine_id || machine.loser.id.as_str() == machine_id
            })
    }

    /// Returns whether an exact peer row id exists in this stored roster,
    /// including entries the reader excluded from cluster truth.
    pub(super) fn contains_stored_peer_id(&self, peer_id: &PeerId) -> bool {
        let peer_id = peer_id.as_str();
        self.peers.iter().any(|peer| peer.id.as_str() == peer_id)
            || self
                .peer_skipped
                .iter()
                .any(|peer| peer.source.key == peer_id)
            || self
                .peer_shadows
                .iter()
                .any(|peer| peer.winner.id.as_str() == peer_id || peer.loser.id.as_str() == peer_id)
    }

    /// Derives selectable peer rows from the canonical accepted and shadow
    /// evidence without storing a parallel roster projection.
    pub(super) fn peer_removal_candidates(
        &self,
    ) -> Result<Vec<PeerRemovalCandidate>, MutationStoreError> {
        let mut candidates = self
            .peers
            .iter()
            .map(|peer| PeerRemovalCandidate {
                id: peer.id.clone(),
                name: peer.document.name.clone(),
                stored_document: peer.stored_document.clone(),
            })
            .collect::<Vec<_>>();
        for shadow in &self.peer_shadows {
            let document = serde_json::from_str::<PeerDocument>(&shadow.loser.source.document)
                .map_err(|error| MutationStoreError::InvalidAcceptedShadow {
                    table: CorrosionTable::Peers,
                    detail: error.to_string(),
                })?;
            let id = PeerId::try_new(shadow.loser.id.as_str().to_owned()).map_err(|error| {
                MutationStoreError::InvalidAcceptedId {
                    table: CorrosionTable::Peers,
                    detail: error.to_string(),
                }
            })?;
            candidates.push(PeerRemovalCandidate {
                id,
                name: document.name,
                stored_document: shadow.loser.source.document.clone(),
            });
        }
        Ok(candidates)
    }

    fn trace_reader_evidence(&self) {
        let machine_skipped = self.machine_skipped.len();
        let machine_shadows = self.machine_shadows.len();
        let peer_skipped = self.peer_skipped.len();
        let peer_shadows = self.peer_shadows.len();
        if machine_skipped == 0 && machine_shadows == 0 && peer_skipped == 0 && peer_shadows == 0 {
            return;
        }
        tracing::warn!(
            machine_skipped,
            machine_shadows,
            peer_skipped,
            peer_shadows,
            machine_skipped_evidence = ?self.machine_skipped,
            machine_shadow_evidence = ?self.machine_shadows,
            peer_skipped_evidence = ?self.peer_skipped,
            peer_shadow_evidence = ?self.peer_shadows,
            "accepted roster excluded stored row evidence"
        );
    }
}

#[derive(Debug, Clone)]
pub(super) struct AcceptedMachine {
    pub(super) id: MachineRowId,
    pub(super) stored_document: String,
    pub(super) document: MachineDocument,
}

/// A valid machine row eligible for explicit removal, including a named row
/// hidden by the reader's lowest-ULID presentation rule.
#[derive(Debug, Clone)]
pub(super) struct MachineRemovalCandidate {
    pub(super) id: MachineRowId,
    pub(super) name: ployz_core::machine::MachineName,
}

#[derive(Debug, Clone)]
pub(super) struct AcceptedPeer {
    pub(super) id: PeerId,
    pub(super) stored_document: String,
    pub(super) document: PeerDocument,
}

/// A valid peer row eligible for explicit removal, including a named row
/// hidden by the reader's lowest-ULID presentation rule.
#[derive(Debug, Clone)]
pub(super) struct PeerRemovalCandidate {
    pub(super) id: PeerId,
    pub(super) name: String,
    pub(super) stored_document: String,
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
    let AcceptedMachineRows {
        accepted: machines,
        removal_candidates: machine_removal_candidates,
        skipped: machine_skipped,
        shadows: machine_shadows,
    } = accepted_machine_rows(read_named_roster_rows::<MachineDocument>(
        &cluster,
        machine_rows,
    ))?;
    let AcceptedNamedRows {
        accepted: peers,
        skipped: peer_skipped,
        shadows: peer_shadows,
    } = accepted_peer_rows(read_named_roster_rows::<PeerDocument>(&cluster, peer_rows))?;
    let roster = AcceptedRoster {
        cluster,
        machines,
        machine_removal_candidates,
        machine_skipped,
        machine_shadows,
        peers,
        peer_skipped,
        peer_shadows,
    };
    roster.trace_reader_evidence();
    Ok(roster)
}

#[derive(Debug)]
struct AcceptedNamedRows<Row> {
    accepted: Vec<Row>,
    skipped: Vec<SkippedRow>,
    shadows: Vec<ShadowConflict>,
}

#[derive(Debug)]
struct AcceptedMachineRows {
    accepted: Vec<AcceptedMachine>,
    removal_candidates: Vec<MachineRemovalCandidate>,
    skipped: Vec<SkippedRow>,
    shadows: Vec<ShadowConflict>,
}

fn accepted_machine_rows(
    report: NamedReadReport<MachineDocument>,
) -> Result<AcceptedMachineRows, MutationStoreError> {
    let NamedReadReport {
        accepted,
        skipped,
        shadows,
    } = report;
    let accepted = accepted
        .into_iter()
        .map(|row| {
            let stored_document = row.source.document;
            Ok(AcceptedMachine {
                id: MachineRowId::try_new(row.id.as_str().to_owned()).map_err(|error| {
                    MutationStoreError::InvalidAcceptedId {
                        table: CorrosionTable::Machines,
                        detail: error.to_string(),
                    }
                })?,
                stored_document,
                document: row.value,
            })
        })
        .collect::<Result<Vec<_>, MutationStoreError>>()?;
    let mut removal_candidates = accepted
        .iter()
        .map(|machine| MachineRemovalCandidate {
            id: machine.id.clone(),
            name: machine.document.name.clone(),
        })
        .collect::<Vec<_>>();
    for shadow in &shadows {
        let document = serde_json::from_str::<MachineDocument>(&shadow.loser.source.document)
            .map_err(|error| MutationStoreError::InvalidAcceptedShadow {
                table: CorrosionTable::Machines,
                detail: error.to_string(),
            })?;
        let id = MachineRowId::try_new(shadow.loser.id.as_str().to_owned()).map_err(|error| {
            MutationStoreError::InvalidAcceptedId {
                table: CorrosionTable::Machines,
                detail: error.to_string(),
            }
        })?;
        removal_candidates.push(MachineRemovalCandidate {
            id,
            name: document.name,
        });
    }
    Ok(AcceptedMachineRows {
        accepted,
        removal_candidates,
        skipped,
        shadows,
    })
}

fn accepted_peer_rows(
    report: NamedReadReport<PeerDocument>,
) -> Result<AcceptedNamedRows<AcceptedPeer>, MutationStoreError> {
    let NamedReadReport {
        accepted,
        skipped,
        shadows,
    } = report;
    let accepted = accepted
        .into_iter()
        .map(|row| {
            Ok(AcceptedPeer {
                id: PeerId::try_new(row.id.as_str().to_owned()).map_err(|error| {
                    MutationStoreError::InvalidAcceptedId {
                        table: CorrosionTable::Peers,
                        detail: error.to_string(),
                    }
                })?,
                stored_document: row.source.document,
                document: row.value,
            })
        })
        .collect::<Result<Vec<_>, MutationStoreError>>()?;
    Ok(AcceptedNamedRows {
        accepted,
        skipped,
        shadows,
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

pub(super) async fn insert_token(
    corrosion: &CorrosionClient,
    token_id: &TokenId,
    document: &TokenDocument,
) -> Result<(), MutationStoreError> {
    let document = encode_document(CorrosionTable::Tokens, document)?;
    corrosion
        .execute(&[insert_token_statement(token_id, document)])
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
    machine_id: &MachineRowId,
    document: &MachineDocument,
    token_id: &TokenId,
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
    peer_id: &PeerId,
    document: &PeerDocument,
    token_id: &TokenId,
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
    token_id: &TokenId,
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

pub(super) async fn replace_machine_if_matches(
    corrosion: &CorrosionClient,
    machine_id: &MachineRowId,
    observed: &str,
    replacement: &MachineDocument,
) -> Result<ConditionalMachineReplace, MutationStoreError> {
    let replacement = encode_document(CorrosionTable::Machines, replacement)?;
    let response = corrosion
        .execute(&[replace_machine_if_matches_statement(
            machine_id,
            observed.to_owned(),
            replacement,
        )])
        .await?;
    conditional_machine_replace_outcome(machine_id, &response)
}

pub(super) async fn update_wireguard_endpoint_if_matches(
    corrosion: &CorrosionClient,
    machine_id: &MachineRowId,
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

pub(super) async fn delete_token(
    corrosion: &CorrosionClient,
    token_id: &TokenId,
) -> Result<(), MutationStoreError> {
    corrosion
        .execute(&[delete_token_statement(token_id)])
        .await?;
    Ok(())
}

/// Deletes the resolved roster row before every testimony row that identity
/// authored. The fixed batch deliberately never touches operation evidence.
pub(super) async fn remove_machine_and_sweep(
    corrosion: &CorrosionClient,
    machine_id: &MachineRowId,
) -> Result<(), MutationStoreError> {
    let statements = machine_removal_statements(machine_id);
    corrosion.execute(&statements).await?;
    Ok(())
}

pub(super) async fn delete_machine_if_matches(
    corrosion: &CorrosionClient,
    machine_id: &MachineRowId,
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
    peer_id: &PeerId,
    expected: &PeerDocument,
) -> Result<(), MutationStoreError> {
    let expected = encode_document(CorrosionTable::Peers, expected)?;
    corrosion
        .execute(&[delete_peer_if_matches_statement(peer_id, expected)])
        .await?;
    Ok(())
}

/// Deletes exactly the peer document resolved by the accepted roster read.
/// A concurrent row change is reported instead of deleting newer truth.
pub(super) async fn remove_peer_if_matches(
    corrosion: &CorrosionClient,
    peer_id: &PeerId,
    expected: &str,
) -> Result<(), MutationStoreError> {
    let response = corrosion
        .execute(&[delete_peer_if_matches_statement(
            peer_id,
            expected.to_owned(),
        )])
        .await?;
    match conditional_delete_outcome(CorrosionTable::Peers, peer_id.as_str(), &response)? {
        ConditionalDelete::Deleted => Ok(()),
        ConditionalDelete::Stale => Err(MutationStoreError::ConcurrentPeerMutation {
            peer_id: peer_id.clone(),
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

fn insert_token_statement(token_id: &TokenId, document: String) -> Statement {
    insert_statement(CorrosionTable::Tokens, token_id.as_str(), document)
}

fn token_authorized_insert_statement(
    table: CorrosionTable,
    id: &str,
    document: String,
    token_id: &TokenId,
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

fn replace_machine_if_matches_statement(
    machine_id: &MachineRowId,
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
    machine_id: &MachineRowId,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConditionalDelete {
    Deleted,
    Stale,
}

fn conditional_delete_outcome(
    table: CorrosionTable,
    id: &str,
    response: &TransactionResponse,
) -> Result<ConditionalDelete, MutationStoreError> {
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
        0 => Ok(ConditionalDelete::Stale),
        1 => Ok(ConditionalDelete::Deleted),
        rows_affected => Err(MutationStoreError::UnexpectedWriteResult {
            table,
            id: id.to_owned(),
            detail: format!("conditional delete affected {rows_affected} rows"),
        }),
    }
}

fn delete_statement(table: CorrosionTable, id: &str) -> Statement {
    Statement::with_params(
        format!("DELETE FROM {} WHERE id = ?", table.as_str()),
        vec![SqliteParameter::Text(id.to_owned())],
    )
}

fn delete_token_statement(token_id: &TokenId) -> Statement {
    delete_statement(CorrosionTable::Tokens, token_id.as_str())
}

fn machine_removal_statements(machine_id: &MachineRowId) -> [Statement; 5] {
    [
        delete_statement(CorrosionTable::Machines, machine_id.as_str()),
        delete_testimony_for_machine_statement(CorrosionTable::MachineStatus, machine_id),
        delete_testimony_for_machine_statement(CorrosionTable::Containers, machine_id),
        delete_testimony_for_machine_statement(CorrosionTable::CertHoldings, machine_id),
        delete_testimony_for_machine_statement(CorrosionTable::AcmeHttp01, machine_id),
    ]
}

fn delete_testimony_for_machine_statement(
    table: CorrosionTable,
    machine_id: &MachineRowId,
) -> Statement {
    match table {
        CorrosionTable::MachineStatus
        | CorrosionTable::Containers
        | CorrosionTable::CertHoldings
        | CorrosionTable::AcmeHttp01 => Statement::with_params(
            format!("DELETE FROM {} WHERE machine_id = ?", table.as_str()),
            vec![SqliteParameter::Text(machine_id.as_str().to_owned())],
        ),
        CorrosionTable::Cluster
        | CorrosionTable::Machines
        | CorrosionTable::Peers
        | CorrosionTable::Tokens
        | CorrosionTable::Namespaces
        | CorrosionTable::Services
        | CorrosionTable::RouteBindings
        | CorrosionTable::Operations => {
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

fn delete_machine_if_matches_statement(machine_id: &MachineRowId, expected: String) -> Statement {
    conditional_delete_statement(CorrosionTable::Machines, machine_id.as_str(), expected)
}

fn delete_peer_if_matches_statement(peer_id: &PeerId, expected: String) -> Statement {
    conditional_delete_statement(CorrosionTable::Peers, peer_id.as_str(), expected)
}

fn update_wireguard_endpoint_statement(
    machine_id: &MachineRowId,
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
    use ployz_core::corrosion::{TransactionResponse, TransactionResult, TransactionSuccess};
    use serde_json::json;

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
    fn machine_removal_is_one_parameterized_roster_first_testimony_batch() {
        let machine_id = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("machine id");
        let statements = machine_removal_statements(&machine_id);
        let parameter = SqliteParameter::Text(machine_id.as_str().to_owned());
        assert_eq!(
            statements,
            [
                Statement::with_params(
                    "DELETE FROM machines WHERE id = ?",
                    vec![parameter.clone()],
                ),
                Statement::with_params(
                    "DELETE FROM machine_status WHERE machine_id = ?",
                    vec![parameter.clone()],
                ),
                Statement::with_params(
                    "DELETE FROM containers WHERE machine_id = ?",
                    vec![parameter.clone()],
                ),
                Statement::with_params(
                    "DELETE FROM cert_holdings WHERE machine_id = ?",
                    vec![parameter.clone()],
                ),
                Statement::with_params(
                    "DELETE FROM acme_http01 WHERE machine_id = ?",
                    vec![parameter],
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
    }

    #[test]
    fn endpoint_update_changes_only_endpoint_and_provenance_with_bound_parameters() {
        let machine_id = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("machine id");
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
        let peer_id = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("peer id");
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
        for (rows_affected, expected) in [
            (0, ConditionalDelete::Stale),
            (1, ConditionalDelete::Deleted),
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
                conditional_delete_outcome(CorrosionTable::Peers, peer_id.as_str(), &response)
                    .expect("bounded conditional delete result"),
                expected,
            );
        }
    }

    #[test]
    fn admission_paused_after_validation_cannot_commit_after_token_revoke_or_expiry() {
        let member_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let token_id = TokenId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("token id");
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
        let machine_id = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("machine id");
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
                StoredRow::new("01ARZ3NDEKTSV4RRFFQ69G5FAW", machine.clone()),
                StoredRow::new("01ARZ3NDEKTSV4RRFFQ69G5FAX", machine.clone()),
                StoredRow::new("01ARZ3NDEKTSV4RRFFQ69G5FAZ", ""),
            ],
        );

        let rows = accepted_machine_rows(report).expect("typed machine ids");

        let [accepted] = rows.accepted.as_slice() else {
            panic!("expected one accepted machine, got {}", rows.accepted.len());
        };
        assert_eq!(accepted.id.as_str(), "01ARZ3NDEKTSV4RRFFQ69G5FAW");
        assert_eq!(accepted.stored_document, machine);
        assert_eq!(
            rows.removal_candidates
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            ["01ARZ3NDEKTSV4RRFFQ69G5FAW", "01ARZ3NDEKTSV4RRFFQ69G5FAX"]
        );
        let [_skipped] = rows.skipped.as_slice() else {
            panic!("expected one skipped machine, got {}", rows.skipped.len());
        };
        let [shadow] = rows.shadows.as_slice() else {
            panic!("expected one shadowed machine, got {}", rows.shadows.len());
        };
        assert_eq!(shadow.loser.id.as_str(), "01ARZ3NDEKTSV4RRFFQ69G5FAX");
    }

    #[test]
    fn peer_removal_candidates_are_derived_from_canonical_roster_evidence() {
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
        let peer = json!({
            "v": 1,
            "cluster_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "name": "operator-laptop",
            "transport": {
                "kind": "wireguard",
                "pubkey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                "addr_v6": "fd00::30",
                "endpoint": null
            },
            "written_by": {
                "kind": "peer",
                "peer_id": "01ARZ3NDEKTSV4RRFFQ69G5FAY"
            },
            "written_at": "2026-08-05T10:00:00Z"
        })
        .to_string();
        let report = read_named_roster_rows::<PeerDocument>(
            &cluster,
            [
                StoredRow::new("01ARZ3NDEKTSV4RRFFQ69G5FAW", peer.clone()),
                StoredRow::new("01ARZ3NDEKTSV4RRFFQ69G5FAX", peer.clone()),
                StoredRow::new("01ARZ3NDEKTSV4RRFFQ69G5FAZ", ""),
            ],
        );
        let rows = accepted_peer_rows(report).expect("typed peer ids");
        let roster = AcceptedRoster {
            cluster,
            machines: Vec::new(),
            machine_removal_candidates: Vec::new(),
            machine_skipped: Vec::new(),
            machine_shadows: Vec::new(),
            peers: rows.accepted,
            peer_skipped: rows.skipped,
            peer_shadows: rows.shadows,
        };

        let candidates = roster
            .peer_removal_candidates()
            .expect("accepted and shadow peers are selectable");
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            ["01ARZ3NDEKTSV4RRFFQ69G5FAW", "01ARZ3NDEKTSV4RRFFQ69G5FAX"]
        );
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.stored_document == peer),
            "each candidate keeps the exact stored document for the delete fence"
        );
    }

    #[test]
    fn table_specific_statement_builders_bind_typed_ids() {
        let token_id = TokenId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("token id");
        let machine_id = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("machine id");
        let peer_id = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("peer id");

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
            delete_token_statement(&token_id),
            delete_statement(CorrosionTable::Tokens, token_id.as_str())
        );
        assert_eq!(
            delete_peer_if_matches_statement(&peer_id, "{}".to_owned()),
            conditional_delete_statement(CorrosionTable::Peers, peer_id.as_str(), "{}".to_owned())
        );
    }
}
