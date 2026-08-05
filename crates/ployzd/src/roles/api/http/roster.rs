//! Reader-fenced roster loading and socket-peer resolution.

use std::net::{IpAddr, SocketAddr};

use ployz_core::corrosion::{
    AcceptedRosterPrincipal, ClusterDocument, CorrosionTable, MachineDocument, PeerDocument,
    Principal, SqliteParameter, SqliteValue, Statement, StoredRow, read_named_roster_rows,
    read_rows, resolve_source_principal,
};
use ployz_core::ids::{ClusterId, MachineRowId, PeerId};
use ployz_core::{ApiRefusal, CorrosionRetryAfterSeconds};

use crate::corrosion::{
    CorrosionClient, CorrosionClientError, StoredRowCollectionError, StoredRowLimit,
    collect_stored_rows,
};

const MAX_ROSTER_ROWS: usize = 10_000;

pub(super) async fn resolve_peer_principal(
    corrosion: &CorrosionClient,
    cluster_id: &ClusterId,
    source: IpAddr,
) -> Result<Principal, ApiRefusal> {
    let roster = read_accepted_roster(corrosion, cluster_id).await?;
    resolve_source_principal(source, &roster.principals).map_err(ApiRefusal::from)
}

pub(super) async fn validate_listener_identity(
    corrosion: &CorrosionClient,
    cluster_id: &ClusterId,
    local_machine_id: &MachineRowId,
    listen_addr: SocketAddr,
) -> Result<MachineDocument, ApiListenerValidationError> {
    let roster = read_accepted_roster(corrosion, cluster_id)
        .await
        .map_err(|refusal| ApiListenerValidationError::Refusal { refusal })?;
    let principal = resolve_source_principal(listen_addr.ip(), &roster.principals)
        .map_err(ApiRefusal::from)
        .map_err(|refusal| ApiListenerValidationError::Refusal { refusal })?;
    validate_listener_principal(local_machine_id, listen_addr, principal)?;
    roster
        .machines
        .into_iter()
        .find_map(|(machine_id, document)| (machine_id == *local_machine_id).then_some(document))
        .ok_or(ApiListenerValidationError::Refusal {
            refusal: ApiRefusal::InvalidCluster,
        })
}

pub(super) fn validate_listener_principal(
    local_machine_id: &MachineRowId,
    listen_addr: SocketAddr,
    principal: Principal,
) -> Result<(), ApiListenerValidationError> {
    match principal {
        Principal::Machine { machine_id } if machine_id == *local_machine_id => Ok(()),
        Principal::Machine { machine_id } => Err(ApiListenerValidationError::OtherMachine {
            listen_addr,
            expected_machine_id: local_machine_id.clone(),
            found_machine_id: machine_id,
        }),
        Principal::Peer { peer_id } => Err(ApiListenerValidationError::Peer {
            listen_addr,
            expected_machine_id: local_machine_id.clone(),
            peer_id,
        }),
        Principal::ApiToken { token_id } => Err(ApiListenerValidationError::ApiToken {
            listen_addr,
            expected_machine_id: local_machine_id.clone(),
            token_id,
        }),
    }
}

pub(super) fn corrosion_unavailable_refusal() -> ApiRefusal {
    ApiRefusal::CorrosionUnavailable {
        retry_after_seconds: CorrosionRetryAfterSeconds::DEFAULT,
    }
}

struct AcceptedRoster {
    principals: Vec<AcceptedRosterPrincipal>,
    machines: Vec<(MachineRowId, MachineDocument)>,
}

async fn read_accepted_roster(
    corrosion: &CorrosionClient,
    cluster_id: &ClusterId,
) -> Result<AcceptedRoster, ApiRefusal> {
    let cluster = query_stored_rows(corrosion, cluster_statement(cluster_id));
    let machines = query_stored_rows(
        corrosion,
        roster_statement(CorrosionTable::Machines.as_str()),
    );
    let peers = query_stored_rows(corrosion, roster_statement(CorrosionTable::Peers.as_str()));
    let (cluster_rows, machine_rows, peer_rows) = match tokio::try_join!(cluster, machines, peers) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(error = %error, "could not query the accepted API roster");
            return Err(corrosion_unavailable_refusal());
        }
    };
    let cluster = accepted_cluster(cluster_id, cluster_rows)?;
    let machines = read_named_roster_rows::<MachineDocument>(&cluster, machine_rows);
    let peers = read_named_roster_rows::<PeerDocument>(&cluster, peer_rows);
    let mut principals = Vec::with_capacity(machines.accepted.len() + peers.accepted.len());
    let mut accepted_machines = Vec::with_capacity(machines.accepted.len());

    for row in machines.accepted {
        let machine_id = MachineRowId::try_new(row.id.as_str().to_owned()).map_err(|error| {
            tracing::warn!(error = %error, "accepted machine roster winner had an invalid id");
            ApiRefusal::InvalidCluster
        })?;
        principals.push(AcceptedRosterPrincipal::machine(
            machine_id.clone(),
            row.value.transport.clone(),
        ));
        accepted_machines.push((machine_id, row.value));
    }
    for row in peers.accepted {
        let peer_id = PeerId::try_new(row.id.as_str().to_owned()).map_err(|error| {
            tracing::warn!(error = %error, "accepted peer roster winner had an invalid id");
            ApiRefusal::InvalidCluster
        })?;
        principals.push(AcceptedRosterPrincipal::peer(peer_id, row.value.transport));
    }
    Ok(AcceptedRoster {
        principals,
        machines: accepted_machines,
    })
}

fn accepted_cluster(
    cluster_id: &ClusterId,
    rows: Vec<StoredRow>,
) -> Result<ClusterDocument, ApiRefusal> {
    if rows.is_empty() {
        return Err(ApiRefusal::MissingCluster);
    }
    let report = read_rows::<ClusterDocument>(cluster_id, rows);
    let mut accepted = report.accepted.into_iter();
    let Some(cluster) = accepted.next() else {
        return Err(ApiRefusal::InvalidCluster);
    };
    if accepted.next().is_some() || cluster.source.key != cluster_id.as_str() {
        return Err(ApiRefusal::InvalidCluster);
    }
    Ok(cluster.value)
}

async fn query_stored_rows(
    corrosion: &CorrosionClient,
    statement: Statement,
) -> Result<Vec<StoredRow>, RosterQueryError> {
    let mut stream = corrosion.query(&statement).await?;
    collect_stored_rows(&mut stream, StoredRowLimit::new(MAX_ROSTER_ROWS))
        .await
        .map_err(RosterQueryError::from)
}

fn cluster_statement(cluster_id: &ClusterId) -> Statement {
    Statement::with_params(
        "SELECT id, document FROM cluster WHERE id = ?",
        vec![SqliteParameter::Text(cluster_id.as_str().to_owned())],
    )
}

fn roster_statement(table: &'static str) -> Statement {
    Statement::simple(format!("SELECT id, document FROM {table}"))
}

#[derive(Debug, thiserror::Error)]
enum RosterQueryError {
    #[error("local Corrosion query failed: {0}")]
    Client(#[from] CorrosionClientError),
    #[error("local Corrosion query omitted its columns frame")]
    MissingColumns,
    #[error("local Corrosion query repeated its columns frame")]
    RepeatedColumns,
    #[error("local Corrosion query returned a row before columns")]
    RowBeforeColumns,
    #[error("local Corrosion query returned columns other than id and document: {columns:?}")]
    UnexpectedColumns { columns: Vec<String> },
    #[error("local Corrosion query returned an unexpected roster row: {values:?}")]
    UnexpectedRow { values: Vec<SqliteValue> },
    #[error("local Corrosion query exceeded the {limit}-row roster bound")]
    RowLimit { limit: usize },
}

impl From<StoredRowCollectionError> for RosterQueryError {
    fn from(error: StoredRowCollectionError) -> Self {
        match error {
            StoredRowCollectionError::Client(source) => Self::Client(source),
            StoredRowCollectionError::MissingColumns => Self::MissingColumns,
            StoredRowCollectionError::DuplicateColumns => Self::RepeatedColumns,
            StoredRowCollectionError::RowBeforeColumns => Self::RowBeforeColumns,
            StoredRowCollectionError::UnexpectedColumns { columns } => {
                Self::UnexpectedColumns { columns }
            }
            StoredRowCollectionError::UnexpectedRow { values } => Self::UnexpectedRow { values },
            StoredRowCollectionError::RowLimit { limit } => Self::RowLimit { limit },
        }
    }
}

/// A roster-backed listener identity validation failure.
#[derive(Debug, thiserror::Error)]
pub enum ApiListenerValidationError {
    #[error("API listener source was refused by the accepted roster: {refusal:?}")]
    Refusal { refusal: ApiRefusal },
    #[error(
        "API listener {listen_addr} resolves to machine {found_machine_id}, not local machine {expected_machine_id}"
    )]
    OtherMachine {
        listen_addr: SocketAddr,
        expected_machine_id: MachineRowId,
        found_machine_id: MachineRowId,
    },
    #[error(
        "API listener {listen_addr} resolves to peer {peer_id}, not local machine {expected_machine_id}"
    )]
    Peer {
        listen_addr: SocketAddr,
        expected_machine_id: MachineRowId,
        peer_id: PeerId,
    },
    #[error(
        "API listener {listen_addr} resolves to API token {token_id}, not local machine {expected_machine_id}"
    )]
    ApiToken {
        listen_addr: SocketAddr,
        expected_machine_id: MachineRowId,
        token_id: ployz_core::ids::TokenId,
    },
}
