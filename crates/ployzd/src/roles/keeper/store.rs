//! Keeper's evidence-preserving adapter to the machine-local Corrosion agent.

use ployz_core::corrosion::{
    ClusterDocument, MachineDocument, MachineStatusDocument, NamedReadReport, PeerDocument,
    Principal, SqliteParameter, Statement, StoredRow, TransactionResponse, TransactionResult,
    read_named_roster_rows, read_rows,
};
use ployz_core::ids::{ClusterId, MachineRowId};

use crate::corrosion::{
    CorrosionClient, CorrosionClientError, StoredRowCollectionError, StoredRowLimit,
    SubscriptionStream, SubscriptionStreamEvent, collect_stored_rows,
};

const MAX_KEEPER_ROWS: usize = 10_000;

/// One complete roster read. Reader evidence stays attached to each collection.
#[derive(Debug)]
pub(super) struct KeeperRosterSnapshot {
    pub(super) cluster: ClusterDocument,
    pub(super) machines: NamedReadReport<MachineDocument>,
    pub(super) peers: NamedReadReport<PeerDocument>,
    pub(super) local_status: Option<MachineStatusDocument>,
}

/// Concrete transport adapter used by the Keeper loop.
#[derive(Clone)]
pub(super) struct KeeperCorrosion {
    client: CorrosionClient,
    cluster_id: ClusterId,
    local_machine_id: MachineRowId,
}

impl KeeperCorrosion {
    #[must_use]
    pub(super) const fn new(
        client: CorrosionClient,
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
    ) -> Self {
        Self {
            client,
            cluster_id,
            local_machine_id,
        }
    }

    /// Re-queries cluster, machines, and peers as one logical observation.
    pub(super) async fn read_roster(&self) -> Result<KeeperRosterSnapshot, KeeperStoreError> {
        let cluster = query_rows(&self.client, cluster_statement(&self.cluster_id));
        let machines = query_rows(&self.client, table_statement("machines"));
        let peers = query_rows(&self.client, table_statement("peers"));
        let status = query_rows(&self.client, local_status_statement(&self.local_machine_id));
        let (cluster_rows, machine_rows, peer_rows, status_rows) =
            tokio::try_join!(cluster, machines, peers, status)?;
        let cluster = accepted_cluster(&self.cluster_id, cluster_rows)?;
        let local_status =
            accepted_local_status(&self.cluster_id, &self.local_machine_id, status_rows)?;
        Ok(KeeperRosterSnapshot {
            machines: read_named_roster_rows::<MachineDocument>(&cluster, machine_rows),
            peers: read_named_roster_rows::<PeerDocument>(&cluster, peer_rows),
            cluster,
            local_status,
        })
    }

    /// Waits for any roster subscription change, treating it only as invalidation.
    pub(super) async fn wait_for_invalidation(&self) -> Result<(), KeeperStoreError> {
        let cluster_statement = invalidation_statement("cluster");
        let machine_statement = invalidation_statement("machines");
        let peer_statement = invalidation_statement("peers");
        let cluster = self.client.subscribe(&cluster_statement);
        let machines = self.client.subscribe(&machine_statement);
        let peers = self.client.subscribe(&peer_statement);
        let (mut cluster, mut machines, mut peers) = tokio::try_join!(cluster, machines, peers)?;
        tokio::select! {
            result = next_invalidation(&mut cluster) => result,
            result = next_invalidation(&mut machines) => result,
            result = next_invalidation(&mut peers) => result,
        }
    }

    pub(super) async fn execute(&self, statement: Statement) -> Result<(), KeeperStoreError> {
        self.client.execute(&[statement]).await?;
        Ok(())
    }

    /// Rewrites only this Keeper's accepted machine row during subnet self-heal.
    pub(super) async fn rewrite_local_machine(
        &self,
        machine_id: &MachineRowId,
        observed_document: &str,
        document: &MachineDocument,
    ) -> Result<(), KeeperStoreError> {
        if machine_id != &self.local_machine_id
            || document.cluster_id != self.cluster_id
            || !matches!(
                &document.provenance.written_by,
                Principal::Machine { machine_id } if machine_id == &self.local_machine_id
            )
        {
            return Err(KeeperStoreError::InvalidLocalMachineRepairOwnership);
        }
        let encoded = serde_json::to_string(document).map_err(|source| {
            KeeperStoreError::EncodeLocalMachineRepair {
                detail: source.to_string(),
            }
        })?;
        let response = self
            .client
            .execute(&[local_machine_repair_statement(
                &self.local_machine_id,
                observed_document,
                encoded,
            )])
            .await?;
        require_local_machine_repair(&self.local_machine_id, &response)
    }
}

fn local_machine_repair_statement(
    machine_id: &MachineRowId,
    observed_document: &str,
    replacement_document: String,
) -> Statement {
    Statement::with_params(
        "UPDATE machines SET document = ? WHERE id = ? AND document = ?",
        vec![
            SqliteParameter::Text(replacement_document),
            SqliteParameter::Text(machine_id.as_str().to_owned()),
            SqliteParameter::Text(observed_document.to_owned()),
        ],
    )
}

fn require_local_machine_repair(
    machine_id: &MachineRowId,
    response: &TransactionResponse,
) -> Result<(), KeeperStoreError> {
    match response.results.as_slice() {
        [TransactionResult::Success(success)] if success.rows_affected == 1 => Ok(()),
        [TransactionResult::Success(success)] if success.rows_affected == 0 => {
            Err(KeeperStoreError::StaleLocalMachineRepair {
                machine_id: machine_id.clone(),
            })
        }
        [TransactionResult::Success(success)] => {
            Err(KeeperStoreError::UnexpectedLocalMachineRepairCount {
                machine_id: machine_id.clone(),
                rows_affected: success.rows_affected,
            })
        }
        [TransactionResult::Error(_)] | [] | [_, _, ..] => {
            Err(KeeperStoreError::UnexpectedLocalMachineRepairResult {
                machine_id: machine_id.clone(),
            })
        }
    }
}

async fn next_invalidation(stream: &mut SubscriptionStream) -> Result<(), KeeperStoreError> {
    loop {
        let Some(event) = stream.next().await? else {
            return Err(CorrosionClientError::SubscriptionEnded.into());
        };
        match event {
            SubscriptionStreamEvent::Columns(_)
            | SubscriptionStreamEvent::Row(_, _)
            | SubscriptionStreamEvent::EndOfQuery(_) => {}
            SubscriptionStreamEvent::Change(_, _, _, _) => return Ok(()),
        }
    }
}

async fn query_rows(
    client: &CorrosionClient,
    statement: Statement,
) -> Result<Vec<StoredRow>, KeeperStoreError> {
    let mut stream = client.query(&statement).await?;
    collect_stored_rows(&mut stream, StoredRowLimit::new(MAX_KEEPER_ROWS))
        .await
        .map_err(KeeperStoreError::from)
}

fn accepted_cluster(
    cluster_id: &ClusterId,
    rows: Vec<StoredRow>,
) -> Result<ClusterDocument, KeeperStoreError> {
    let report = read_rows::<ClusterDocument>(cluster_id, rows);
    let mut accepted = report.accepted.into_iter();
    let Some(cluster) = accepted.next() else {
        return Err(KeeperStoreError::ClusterNotAccepted);
    };
    if accepted.next().is_some() || cluster.source.key != cluster_id.as_str() {
        return Err(KeeperStoreError::AmbiguousCluster);
    }
    Ok(cluster.value)
}

fn cluster_statement(cluster_id: &ClusterId) -> Statement {
    Statement::with_params(
        "SELECT id, document FROM cluster WHERE id = ?",
        vec![SqliteParameter::Text(cluster_id.as_str().to_owned())],
    )
}

fn table_statement(table: &'static str) -> Statement {
    Statement::simple(format!("SELECT id, document FROM {table}"))
}

fn local_status_statement(local_machine_id: &MachineRowId) -> Statement {
    Statement::with_params(
        "SELECT machine_id AS id, document FROM machine_status WHERE machine_id = ?",
        vec![SqliteParameter::Text(local_machine_id.as_str().to_owned())],
    )
}

fn accepted_local_status(
    cluster_id: &ClusterId,
    local_machine_id: &MachineRowId,
    rows: Vec<StoredRow>,
) -> Result<Option<MachineStatusDocument>, KeeperStoreError> {
    let report = read_rows::<MachineStatusDocument>(cluster_id, rows);
    let mut accepted = report.accepted.into_iter();
    let Some(status) = accepted.next() else {
        return Ok(None);
    };
    if accepted.next().is_some()
        || status.source.key != local_machine_id.as_str()
        || status.value.machine_id != *local_machine_id
    {
        return Err(KeeperStoreError::InvalidLocalStatusOwnership);
    }
    Ok(Some(status.value))
}

fn invalidation_statement(table: &'static str) -> Statement {
    table_statement(table)
}

#[derive(Debug, thiserror::Error)]
pub(super) enum KeeperStoreError {
    #[error("local Corrosion request failed: {0}")]
    Client(#[from] CorrosionClientError),
    #[error(transparent)]
    Collection(#[from] StoredRowCollectionError),
    #[error("the configured cluster row is not accepted")]
    ClusterNotAccepted,
    #[error("the configured cluster identity resolved ambiguously")]
    AmbiguousCluster,
    #[error("the local machine_status row key and document ownership disagree")]
    InvalidLocalStatusOwnership,
    #[error("Keeper subnet repair did not target its own machine row and cluster")]
    InvalidLocalMachineRepairOwnership,
    #[error("Keeper could not encode its repaired local machine row: {detail}")]
    EncodeLocalMachineRepair { detail: String },
    #[error(
        "Keeper subnet repair evidence for local machine {machine_id} became stale before the write"
    )]
    StaleLocalMachineRepair { machine_id: MachineRowId },
    #[error(
        "Keeper subnet repair for local machine {machine_id} changed an unexpected {rows_affected} rows"
    )]
    UnexpectedLocalMachineRepairCount {
        machine_id: MachineRowId,
        rows_affected: usize,
    },
    #[error("Keeper subnet repair for local machine {machine_id} returned an unexpected result")]
    UnexpectedLocalMachineRepairResult { machine_id: MachineRowId },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscriptions_cover_all_roster_tables_without_a_union() {
        for table in ["cluster", "machines", "peers"] {
            assert_eq!(
                invalidation_statement(table),
                Statement::simple(format!("SELECT id, document FROM {table}"))
            );
        }
    }

    #[test]
    fn cluster_query_keeps_the_id_as_a_parameter() {
        let cluster_id = ClusterId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster id");
        assert_eq!(
            cluster_statement(&cluster_id),
            Statement::with_params(
                "SELECT id, document FROM cluster WHERE id = ?",
                vec![SqliteParameter::Text(
                    "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
                )],
            )
        );
    }

    #[test]
    fn local_machine_repair_is_fenced_by_the_exact_observed_document() {
        let machine_id = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("machine id");
        let observed = r#"{"lifecycle":"active","endpoint":"192.0.2.1:51820"}"#;
        let replacement =
            r#"{"lifecycle":"active","endpoint":"192.0.2.1:51820","subnet":"10.210.2.0/24"}"#;

        assert_eq!(
            local_machine_repair_statement(&machine_id, observed, replacement.to_owned()),
            Statement::with_params(
                "UPDATE machines SET document = ? WHERE id = ? AND document = ?",
                vec![
                    SqliteParameter::Text(replacement.to_owned()),
                    SqliteParameter::Text(machine_id.as_str().to_owned()),
                    SqliteParameter::Text(observed.to_owned()),
                ],
            )
        );
    }

    #[test]
    fn changed_machine_row_is_not_reported_as_repaired() {
        let machine_id = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("machine id");
        let response = TransactionResponse {
            results: vec![TransactionResult::Success(
                ployz_core::corrosion::TransactionSuccess {
                    rows_affected: 0,
                    time: 0.01,
                },
            )],
            time: 0.02,
            version: None,
            actor_id: None,
        };

        assert!(matches!(
            require_local_machine_repair(&machine_id, &response),
            Err(KeeperStoreError::StaleLocalMachineRepair {
                machine_id: stale_machine_id,
            }) if stale_machine_id == machine_id
        ));
    }
}
