//! Keeper's evidence-preserving adapter to the machine-local Corrosion agent.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use ployz_core::corrosion::{
    ClusterDocument, MachineDocument, MachineEndpointDocument, MachineStatusDocument, PeerDocument,
    ReadReport, SqliteParameter, SqliteValue, Statement, StoredRow, read_named_roster_rows,
    read_rows,
};
use ployz_core::ids::{ClusterName, MachineName};

use crate::corrosion::{
    CorrosionClient, CorrosionClientError, StoredRowCollectionError, StoredRowLimit,
    SubscriptionStream, SubscriptionStreamEvent, collect_stored_rows,
};

const MAX_KEEPER_ROWS: usize = 10_000;

#[derive(Debug)]
pub(super) struct KeeperRosterSnapshot {
    pub(super) cluster: ClusterDocument,
    pub(super) machines: ReadReport<MachineDocument>,
    pub(super) peers: ReadReport<PeerDocument>,
    pub(super) local_status: Option<MachineStatusDocument>,
}

#[derive(Debug)]
pub(super) struct KeeperIsolationSnapshot {
    pub(super) cluster: ClusterDocument,
    pub(super) machines: ReadReport<MachineDocument>,
    pub(super) machine_endpoints: ReadReport<MachineEndpointDocument>,
    pub(super) local_status: Option<MachineStatusDocument>,
}

pub(super) struct KeeperInvalidationStreams {
    pub(super) roster: KeeperRosterInvalidations,
    pub(super) machine_endpoints: KeeperEndpointInvalidations,
}

pub(super) struct KeeperRosterInvalidations {
    cluster: SubscriptionStream,
    machines: SubscriptionStream,
    peers: SubscriptionStream,
    status: SubscriptionStream,
    self_status_echoes: Arc<Mutex<SelfStatusEchoQueue>>,
}

impl KeeperRosterInvalidations {
    pub(super) async fn next(&mut self) -> Result<(), KeeperStoreError> {
        tokio::select! {
            result = next_invalidation(&mut self.cluster) => result,
            result = next_invalidation(&mut self.machines) => result,
            result = next_invalidation(&mut self.peers) => result,
            result = next_status_invalidation(&mut self.status, &self.self_status_echoes) => result,
        }
    }
}

pub(super) struct KeeperEndpointInvalidations {
    machine_endpoints: SubscriptionStream,
}

impl KeeperEndpointInvalidations {
    pub(super) async fn next(&mut self) -> Result<(), KeeperStoreError> {
        next_invalidation(&mut self.machine_endpoints).await
    }
}

#[derive(Debug, Default)]
struct SelfStatusEchoQueue(VecDeque<String>);

impl SelfStatusEchoQueue {
    fn expect(&mut self, document: String) {
        self.0.push_back(document);
    }

    fn rollback(&mut self, document: &str) {
        let Some(position) = self.0.iter().rposition(|queued| queued == document) else {
            return;
        };
        self.0.remove(position);
    }

    fn consume(&mut self, values: &[SqliteValue]) -> bool {
        let [SqliteValue::Text(_), SqliteValue::Text(document)] = values else {
            return false;
        };
        if self.0.front().is_some_and(|expected| expected == document) {
            self.0.pop_front();
            true
        } else {
            false
        }
    }

    fn clear_for_resubscribe(&mut self) {
        self.0.clear();
    }
}

#[derive(Clone)]
pub(super) struct KeeperCorrosion {
    client: CorrosionClient,
    cluster_id: ClusterName,
    local_machine_id: MachineName,
    self_status_echoes: Arc<Mutex<SelfStatusEchoQueue>>,
}

impl KeeperCorrosion {
    #[must_use]
    pub(super) fn new(
        client: CorrosionClient,
        cluster_id: ClusterName,
        local_machine_id: MachineName,
    ) -> Self {
        Self {
            client,
            cluster_id,
            local_machine_id,
            self_status_echoes: Arc::new(Mutex::new(SelfStatusEchoQueue::default())),
        }
    }

    pub(super) async fn read_roster(&self) -> Result<KeeperRosterSnapshot, KeeperStoreError> {
        let cluster = query_rows(&self.client, cluster_statement(&self.cluster_id));
        let machines = query_rows(&self.client, table_statement("machines"));
        let peers = query_rows(&self.client, table_statement("peers"));
        let status = query_rows(&self.client, local_status_statement(&self.local_machine_id));
        let (cluster_rows, machine_rows, peer_rows, status_rows) =
            tokio::try_join!(cluster, machines, peers, status)?;
        let cluster = accepted_cluster(&self.cluster_id, cluster_rows)?;
        Ok(KeeperRosterSnapshot {
            machines: read_named_roster_rows::<MachineDocument>(&cluster, machine_rows),
            peers: read_named_roster_rows::<PeerDocument>(&cluster, peer_rows),
            local_status: accepted_local_status(
                &self.cluster_id,
                &self.local_machine_id,
                status_rows,
            )?,
            cluster,
        })
    }

    pub(super) async fn read_machine_endpoints(
        &self,
    ) -> Result<ReadReport<MachineEndpointDocument>, KeeperStoreError> {
        let rows = query_machine_endpoint_rows(&self.client).await?;
        Ok(read_rows::<MachineEndpointDocument>(&self.cluster_id, rows))
    }

    pub(super) async fn read_isolation(&self) -> Result<KeeperIsolationSnapshot, KeeperStoreError> {
        let cluster = query_rows(&self.client, cluster_statement(&self.cluster_id));
        let machines = query_rows(&self.client, table_statement("machines"));
        let machine_endpoints = query_machine_endpoint_rows(&self.client);
        let status = query_rows(&self.client, local_status_statement(&self.local_machine_id));
        let (cluster_rows, machine_rows, machine_endpoint_rows, status_rows) =
            tokio::try_join!(cluster, machines, machine_endpoints, status)?;
        let cluster = accepted_cluster(&self.cluster_id, cluster_rows)?;
        Ok(KeeperIsolationSnapshot {
            machines: read_named_roster_rows::<MachineDocument>(&cluster, machine_rows),
            machine_endpoints: read_rows::<MachineEndpointDocument>(
                &self.cluster_id,
                machine_endpoint_rows,
            ),
            cluster,
            local_status: accepted_local_status(
                &self.cluster_id,
                &self.local_machine_id,
                status_rows,
            )?,
        })
    }

    pub(super) async fn subscribe_invalidations(
        &self,
    ) -> Result<KeeperInvalidationStreams, KeeperStoreError> {
        let cluster_statement = invalidation_statement("cluster");
        let machine_statement = invalidation_statement("machines");
        let peer_statement = invalidation_statement("peers");
        let status_statement = local_status_statement(&self.local_machine_id);
        let endpoint_statement = machine_endpoint_invalidation_statement();
        let cluster = self.client.subscribe(&cluster_statement);
        let machines = self.client.subscribe(&machine_statement);
        let peers = self.client.subscribe(&peer_statement);
        let status = self.client.subscribe(&status_statement);
        let machine_endpoints = self.client.subscribe(&endpoint_statement);
        let (cluster, machines, peers, status, machine_endpoints) =
            tokio::try_join!(cluster, machines, peers, status, machine_endpoints)?;
        self.self_status_echoes
            .lock()
            .map_err(|_| KeeperStoreError::SelfStatusEchoPoisoned)?
            .clear_for_resubscribe();
        Ok(KeeperInvalidationStreams {
            roster: KeeperRosterInvalidations {
                cluster,
                machines,
                peers,
                status,
                self_status_echoes: Arc::clone(&self.self_status_echoes),
            },
            machine_endpoints: KeeperEndpointInvalidations { machine_endpoints },
        })
    }

    pub(super) async fn execute(&self, statement: Statement) -> Result<(), KeeperStoreError> {
        let status_document = status_document_parameter(&statement);
        if let Some(document) = &status_document {
            self.self_status_echoes
                .lock()
                .map_err(|_| KeeperStoreError::SelfStatusEchoPoisoned)?
                .expect(document.clone());
        }
        if let Err(error) = self.client.execute(&[statement]).await {
            if let Some(document) = status_document {
                self.self_status_echoes
                    .lock()
                    .map_err(|_| KeeperStoreError::SelfStatusEchoPoisoned)?
                    .rollback(&document);
            }
            return Err(error.into());
        }
        Ok(())
    }
}

async fn query_machine_endpoint_rows(
    client: &CorrosionClient,
) -> Result<Vec<StoredRow>, KeeperStoreError> {
    let statement = machine_endpoint_statement(MAX_KEEPER_ROWS);
    let mut stream = client.query(&statement).await?;
    collect_stored_rows(&mut stream, StoredRowLimit::new(MAX_KEEPER_ROWS))
        .await
        .map_err(KeeperStoreError::from)
}

fn machine_endpoint_statement(limit: usize) -> Statement {
    Statement::simple(format!(
        "SELECT id, document FROM machine_endpoints LIMIT {limit}"
    ))
}

fn machine_endpoint_invalidation_statement() -> Statement {
    Statement::simple("SELECT id, document FROM machine_endpoints".to_owned())
}

fn status_document_parameter(statement: &Statement) -> Option<String> {
    let Statement::WithParams(sql, parameters) = statement else {
        return None;
    };
    if !sql.starts_with("INSERT INTO machine_status ") {
        return None;
    }
    let [SqliteParameter::Text(_), SqliteParameter::Text(document)] = parameters.as_slice() else {
        return None;
    };
    Some(document.clone())
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

async fn next_status_invalidation(
    stream: &mut SubscriptionStream,
    self_status_echoes: &Mutex<SelfStatusEchoQueue>,
) -> Result<(), KeeperStoreError> {
    loop {
        let Some(event) = stream.next().await? else {
            return Err(CorrosionClientError::SubscriptionEnded.into());
        };
        match event {
            SubscriptionStreamEvent::Columns(_)
            | SubscriptionStreamEvent::Row(_, _)
            | SubscriptionStreamEvent::EndOfQuery(_) => {}
            SubscriptionStreamEvent::Change(_, _, values, _) => {
                if self_status_echoes
                    .lock()
                    .map_err(|_| KeeperStoreError::SelfStatusEchoPoisoned)?
                    .consume(&values)
                {
                    continue;
                }
                return Ok(());
            }
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
    cluster_id: &ClusterName,
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

fn cluster_statement(cluster_id: &ClusterName) -> Statement {
    Statement::with_params(
        "SELECT id, document FROM cluster WHERE id = ?",
        vec![SqliteParameter::Text(cluster_id.as_str().to_owned())],
    )
}

fn table_statement(table: &'static str) -> Statement {
    Statement::simple(format!("SELECT id, document FROM {table}"))
}

fn local_status_statement(local_machine_id: &MachineName) -> Statement {
    Statement::with_params(
        "SELECT machine_id AS id, document FROM machine_status WHERE machine_id = ?",
        vec![SqliteParameter::Text(local_machine_id.as_str().to_owned())],
    )
}

fn accepted_local_status(
    cluster_id: &ClusterName,
    local_machine_id: &MachineName,
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
    #[error("Keeper self-status invalidation evidence was poisoned")]
    SelfStatusEchoPoisoned,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscriptions_are_separate_and_status_is_local() {
        for table in ["cluster", "machines", "peers"] {
            assert_eq!(
                invalidation_statement(table),
                Statement::simple(format!("SELECT id, document FROM {table}"))
            );
        }
        assert_eq!(
            machine_endpoint_invalidation_statement(),
            Statement::simple("SELECT id, document FROM machine_endpoints".to_owned())
        );
        let machine = MachineName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("machine");
        assert!(matches!(
            local_status_statement(&machine),
            Statement::WithParams(_, _)
        ));
    }

    #[test]
    fn machine_endpoint_query_observes_one_row_beyond_shared_map_capacity() {
        let capacity = usize::try_from(ployz_ebpf_common::MAX_ISOLATION_ENTRIES).expect("capacity");
        assert_eq!(
            machine_endpoint_statement(capacity + 1),
            Statement::simple(format!(
                "SELECT id, document FROM machine_endpoints LIMIT {}",
                capacity + 1
            ))
        );
    }

    #[test]
    fn self_echoes_queue_before_consumption_and_preserve_order() {
        let mut queue = SelfStatusEchoQueue::default();
        queue.expect("first".to_owned());
        queue.expect("second".to_owned());
        let first = [
            SqliteValue::Text("machine".to_owned()),
            SqliteValue::Text("first".to_owned()),
        ];
        let second = [
            SqliteValue::Text("machine".to_owned()),
            SqliteValue::Text("second".to_owned()),
        ];
        assert!(queue.consume(&first));
        assert!(queue.consume(&second));
        assert!(!queue.consume(&second));
    }

    #[test]
    fn failed_write_rolls_back_and_resubscribe_clears_stale_echoes() {
        let mut queue = SelfStatusEchoQueue::default();
        queue.expect("failed".to_owned());
        queue.rollback("failed");
        queue.expect("disconnected".to_owned());
        queue.clear_for_resubscribe();
        let values = [
            SqliteValue::Text("machine".to_owned()),
            SqliteValue::Text("disconnected".to_owned()),
        ];
        assert!(!queue.consume(&values));
    }
}
