use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use ipnet::Ipv4Net;
use ployz_core::{
    CERTIFICATE_POLICY_CLUSTER_KEY, ContainerId, ContainerObservation, IngressHost, IssuanceClock,
    Machine, MachineId,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{
    ApiClient, CertificateChallenge, CertificateMaterial, CertificateRow, Error, Statement,
    Subscription,
};
use crate::{
    hosted_dns::Reservation,
    machine::{LocalMachineBody, LocalMachineRecord, LocalMachineStore, StoreError},
};

#[derive(Clone)]
pub struct ReplicatedStore {
    api: ApiClient,
    machine_publication: Arc<tokio::sync::Mutex<()>>,
}

pub(crate) struct MachinePublicationGuard<'a> {
    store: &'a ReplicatedStore,
    _guard: tokio::sync::MutexGuard<'a, ()>,
}

impl MachinePublicationGuard<'_> {
    pub(crate) fn complete_catch_up(
        &self,
        local: &mut LocalMachineStore,
    ) -> Result<bool, StoreError> {
        if !matches!(local.record().body, LocalMachineBody::Joining { .. }) {
            return Ok(false);
        }
        local.complete_catch_up()?;
        Ok(true)
    }

    pub(crate) fn publishable_machine(&self, local: &LocalMachineRecord) -> Option<Machine> {
        match &local.body {
            LocalMachineBody::Participating { machine, .. } => Some(machine.clone()),
            LocalMachineBody::Uninitialized { .. }
            | LocalMachineBody::Joining { .. }
            | LocalMachineBody::Resetting { .. } => None,
        }
    }

    pub(crate) async fn publish(&self, machine: &Machine) -> Result<(), Error> {
        self.store.publish_local_machine_unlocked(machine).await
    }

    pub(crate) async fn remove(&self, machine_id: &MachineId) -> Result<(), Error> {
        self.store
            .api
            .execute([Statement::new(
                "DELETE FROM containers WHERE machine_id = ?",
                [json!(machine_id)],
            )])
            .await?;
        self.store
            .api
            .execute([Statement::new(
                "DELETE FROM machines WHERE id = ?",
                [json!(machine_id)],
            )])
            .await
    }

    pub(crate) async fn local_containers(
        &self,
        machine_id: &MachineId,
    ) -> Result<LocalContainerSnapshot, Error> {
        self.store.local_containers(machine_id).await
    }

    pub(crate) async fn apply_container_rows(
        &self,
        machine_id: &MachineId,
        deletions: &[ContainerId],
        upserts: &[ContainerObservation],
    ) -> Result<(), Error> {
        if upserts
            .iter()
            .any(|observation| &observation.machine_id != machine_id)
        {
            return Err(Error::Protocol(
                "local container reconciliation crossed Machine authority".into(),
            ));
        }
        let mut statements = deletions
            .iter()
            .map(|id| {
                Statement::new(
                    "DELETE FROM containers WHERE id = ? AND machine_id = ?",
                    [json!(id), json!(machine_id)],
                )
            })
            .collect::<Vec<_>>();
        for observation in upserts {
            statements.push(container_upsert(observation)?);
        }
        if !statements.is_empty() {
            self.store.api.execute(statements).await?;
        }
        Ok(())
    }
}

impl ReplicatedStore {
    #[must_use]
    pub(crate) fn new(api: ApiClient) -> Self {
        Self {
            api,
            machine_publication: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    #[cfg(test)]
    pub(super) fn api(&self) -> &ApiClient {
        &self.api
    }

    pub async fn publish_local_machine(&self, machine: &Machine) -> Result<(), Error> {
        self.machine_publication().await.publish(machine).await
    }

    pub async fn remove_machine(&self, machine_id: &MachineId) -> Result<(), Error> {
        self.machine_publication().await.remove(machine_id).await
    }

    pub(crate) async fn machine_publication(&self) -> MachinePublicationGuard<'_> {
        MachinePublicationGuard {
            store: self,
            _guard: self.machine_publication.lock().await,
        }
    }

    async fn publish_local_machine_unlocked(&self, machine: &Machine) -> Result<(), Error> {
        let current = self.machine(machine.id.as_str()).await?;
        if current.as_ref() == Some(machine) {
            return Ok(());
        }
        self.api
            .execute([Statement::new(
                "INSERT INTO machines (id, info, created_at, updated_at) VALUES (?, ?, datetime('now'), datetime('now')) ON CONFLICT (id) DO UPDATE SET info = excluded.info, updated_at = excluded.updated_at",
                [json!(machine.id), json!(serde_json::to_string(machine)?)],
            )])
            .await?;
        Ok(())
    }

    pub async fn publish_cluster_network(&self, network: Ipv4Net) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                "INSERT INTO cluster (key, value, updated_at) VALUES ('network', ?, datetime('now')) ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                [json!(network.to_string())],
            )])
            .await?;
        Ok(())
    }

    pub async fn cluster_network(&self) -> Result<Ipv4Net, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT value FROM cluster WHERE key = 'network'",
                [],
            ))
            .await?;
        let rows = query.rows(["value"])?;
        let Some([value]) = rows.first() else {
            return Err(Error::Protocol("Cluster network is missing".into()));
        };
        text(value, "Cluster network")?
            .parse()
            .map_err(|error| Error::Protocol(format!("invalid Cluster network: {error}")))
    }

    pub(crate) async fn domain_reservation(&self) -> Result<Option<Reservation>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT value FROM cluster WHERE key = 'hosted_dns'",
                [],
            ))
            .await?;
        let rows = query.rows(["value"])?;
        let Some([value]) = rows.first() else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(text(
            value,
            "hosted DNS reservation",
        )?)?))
    }

    pub(crate) async fn publish_domain_reservation(
        &self,
        reservation: &Reservation,
    ) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                "INSERT INTO cluster (key, value, updated_at) VALUES ('hosted_dns', ?, datetime('now')) ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                [json!(serde_json::to_string(reservation)?)],
            )])
            .await
    }

    pub(crate) async fn remove_domain_reservation(&self) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                "DELETE FROM cluster WHERE key = 'hosted_dns'",
                [],
            )])
            .await
    }

    pub async fn machine(&self, id: &str) -> Result<Option<Machine>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT info FROM machines WHERE id = ?",
                [json!(id)],
            ))
            .await?;
        let rows = query.rows(["info"])?;
        let Some([info]) = rows.first() else {
            return Ok(None);
        };
        let info = text(info, "machine info")?;
        if info.is_empty() || info == "{}" {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(info)?))
    }

    pub async fn machines(&self) -> Result<ReplicatedObservations<Machine>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT id, info FROM machines ORDER BY name",
                [],
            ))
            .await?;
        decode_observations(query.rows(["id", "info"])?)
    }

    pub async fn publish_container(&self, observation: &ContainerObservation) -> Result<(), Error> {
        if self.container(&observation.container_id).await? == Some(observation.clone()) {
            return Ok(());
        }
        self.api.execute([container_upsert(observation)?]).await?;
        Ok(())
    }

    pub async fn container(&self, id: &ContainerId) -> Result<Option<ContainerObservation>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT container FROM containers WHERE id = ?",
                [json!(id)],
            ))
            .await?;
        let rows = query.rows(["container"])?;
        let Some([encoded]) = rows.first() else {
            return Ok(None);
        };
        let encoded = text(encoded, "replicated container JSON")?;
        if encoded.is_empty() || encoded == "{}" {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(encoded)?))
    }

    #[cfg(test)]
    pub(crate) async fn raw_container(&self, id: &ContainerId) -> Result<Option<String>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT container FROM containers WHERE id = ?",
                [json!(id)],
            ))
            .await?;
        query
            .rows(["container"])?
            .first()
            .map(|[encoded]| text(encoded, "replicated container JSON").map(ToOwned::to_owned))
            .transpose()
    }

    async fn local_containers(
        &self,
        machine_id: &MachineId,
    ) -> Result<LocalContainerSnapshot, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT id, container FROM containers WHERE machine_id = ? ORDER BY id",
                [json!(machine_id)],
            ))
            .await?;
        let mut snapshot = LocalContainerSnapshot::default();
        for [id, encoded] in query.rows(["id", "container"])? {
            let id = ContainerId::parse(text(&id, "container ID")?)?;
            let encoded = text(&encoded, "replicated container JSON")?;
            let observation = if encoded.is_empty() || encoded == "{}" {
                None
            } else {
                Some(serde_json::from_str(encoded)?)
            };
            snapshot.inventory.insert(id);
            if let Some(observation) = observation {
                snapshot.observations.insert(id, observation);
            }
        }
        Ok(snapshot)
    }

    pub async fn containers(&self) -> Result<ReplicatedObservations<ContainerObservation>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT id, container FROM containers ORDER BY id",
                [],
            ))
            .await?;
        decode_observations(query.rows(["id", "container"])?)
    }

    pub(crate) async fn subscribe_container_changes(&self) -> Result<Subscription, Error> {
        self.api
            .subscribe(Statement::new("SELECT id, container FROM containers", []))
            .await
    }

    pub async fn publish_certificate(
        &self,
        hostname: &IngressHost,
        material: &CertificateMaterial,
    ) -> Result<(), Error> {
        let latest = self.certificate_row(hostname).await?;
        if latest.material() == Some(material) && latest.challenge().is_none() {
            return Ok(());
        }
        self.upsert_certificate(hostname, &CertificateRow::issued(material.clone()))
            .await
    }

    pub async fn certificate(
        &self,
        hostname: &IngressHost,
    ) -> Result<Option<CertificateMaterial>, Error> {
        Ok(self.certificate_row(hostname).await?.into_material())
    }

    pub(crate) async fn certificate_row(
        &self,
        hostname: &IngressHost,
    ) -> Result<CertificateRow, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT body FROM certificates WHERE hostname = ?",
                [json!(hostname.as_str())],
            ))
            .await?;
        let rows = query.rows(["body"])?;
        let Some([encoded]) = rows.first() else {
            return Ok(CertificateRow::default());
        };
        CertificateRow::decode(text(encoded, "certificate body")?)
    }

    async fn upsert_certificate(
        &self,
        hostname: &IngressHost,
        row: &CertificateRow,
    ) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                "INSERT INTO certificates (hostname, body, updated_at) VALUES (?, ?, datetime('now')) ON CONFLICT (hostname) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
                [json!(hostname.as_str()), json!(row.encode()?)],
            )])
            .await
    }

    pub async fn publish_certificate_challenge(
        &self,
        hostname: &IngressHost,
        challenge: &CertificateChallenge,
    ) -> Result<(), Error> {
        let latest = self.certificate_row(hostname).await?;
        if latest.challenge() == Some(challenge) {
            return Ok(());
        }
        self.upsert_certificate(hostname, &latest.with_challenge(challenge.clone()))
            .await
    }

    /// Record why a hostname has no certificate and when the Cluster may try again.
    ///
    /// # Errors
    ///
    /// Returns if the row cannot be read or written.
    pub async fn record_certificate_failure(
        &self,
        hostname: &IngressHost,
        last_error: impl Into<String>,
        clock: IssuanceClock,
    ) -> Result<(), Error> {
        let latest = self.certificate_row(hostname).await?;
        if latest.material().is_some() {
            return Ok(());
        }
        self.upsert_certificate(hostname, &latest.with_backoff(last_error, clock))
            .await
    }

    pub async fn record_certificate_error(
        &self,
        hostname: &IngressHost,
        reason: &str,
    ) -> Result<(), Error> {
        let latest = self.certificate_row(hostname).await?;
        if latest.last_error() == Some(reason) {
            return Ok(());
        }
        self.upsert_certificate(hostname, &latest.with_error(reason))
            .await
    }

    pub async fn certificate_policy(&self) -> Result<Option<String>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT value FROM cluster WHERE key = ?",
                [json!(CERTIFICATE_POLICY_CLUSTER_KEY)],
            ))
            .await?;
        let rows = query.rows(["value"])?;
        let Some([value]) = rows.first() else {
            return Ok(None);
        };
        let encoded = text(value, "certificate policy")?;
        Ok((!encoded.is_empty()).then(|| encoded.to_owned()))
    }

    pub async fn certificates(&self) -> Result<BTreeMap<IngressHost, CertificateMaterial>, Error> {
        Ok(self
            .certificate_state()
            .await?
            .into_iter()
            .filter_map(|(hostname, row)| row.into_material().map(|material| (hostname, material)))
            .collect())
    }

    pub async fn certificate_state(&self) -> Result<BTreeMap<IngressHost, CertificateRow>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT hostname, body FROM certificates ORDER BY hostname",
                [],
            ))
            .await?;
        let mut rows = BTreeMap::new();
        for [hostname, encoded] in query.rows(["hostname", "body"])? {
            let hostname = IngressHost::parse(text(&hostname, "certificate hostname")?)?;
            rows.insert(
                hostname,
                CertificateRow::decode(text(&encoded, "certificate body")?)?,
            );
        }
        Ok(rows)
    }

    pub(crate) async fn subscribe_certificate_changes(&self) -> Result<Subscription, Error> {
        self.api
            .subscribe(Statement::new(
                "SELECT hostname, body FROM certificates",
                [],
            ))
            .await
    }

    pub async fn version(&self) -> Result<BTreeMap<String, i64>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT site_id, db_version FROM crsql_db_versions",
                [],
            ))
            .await?;
        query
            .rows(["site_id", "db_version"])?
            .into_iter()
            .map(|[actor, version]| {
                let actor = actor_id(&actor)?;
                let version = version
                    .as_i64()
                    .ok_or_else(|| Error::Protocol("invalid actor version".into()))?;
                Ok((actor, version))
            })
            .collect()
    }

    pub async fn has_known_missing_changes(&self) -> Result<bool, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT EXISTS (SELECT 1 FROM __corro_bookkeeping_gaps) AS has_gaps",
                [],
            ))
            .await?;
        match query.rows(["has_gaps"])?.as_slice() {
            [[Value::Number(value)]] => value
                .as_u64()
                .filter(|value| *value <= 1)
                .map(|value| value == 1)
                .ok_or_else(|| Error::Protocol("invalid gap status".into())),
            _ => Err(Error::Protocol("invalid gap status row".into())),
        }
    }
}

#[derive(Default)]
pub(crate) struct LocalContainerSnapshot {
    pub(crate) inventory: BTreeSet<ContainerId>,
    pub(crate) observations: BTreeMap<ContainerId, ContainerObservation>,
}

impl LocalContainerSnapshot {
    pub(crate) fn from_inventory(ids: Vec<ContainerId>) -> Self {
        Self {
            inventory: ids.into_iter().collect(),
            observations: BTreeMap::new(),
        }
    }

    pub(crate) fn ids(&self) -> impl Iterator<Item = &ContainerId> {
        self.inventory.iter()
    }

    pub(crate) fn observed(&mut self, observation: ContainerObservation) {
        debug_assert!(self.inventory.contains(&observation.container_id));
        self.observations
            .insert(observation.container_id, observation);
    }
}

fn container_upsert(observation: &ContainerObservation) -> Result<Statement, Error> {
    Ok(Statement::new(
        "INSERT INTO containers (id, container, machine_id, updated_at) VALUES (?, ?, ?, datetime('now')) ON CONFLICT (id) DO UPDATE SET container = excluded.container, machine_id = excluded.machine_id, updated_at = excluded.updated_at",
        [
            json!(observation.container_id),
            json!(serde_json::to_string(observation)?),
            json!(observation.machine_id),
        ],
    ))
}

#[derive(Debug, Eq, PartialEq)]
pub struct ReplicatedObservations<T> {
    pub observations: Vec<T>,
    pub incomplete_ids: Vec<String>,
}

pub async fn wait_for_catch_up(
    store: &ReplicatedStore,
    target: &BTreeMap<String, i64>,
) -> Result<(), Error> {
    if target.is_empty() {
        return Ok(());
    }
    let warning_interval = Duration::from_secs(5 * 60);
    let mut warning_at = tokio::time::Instant::now() + warning_interval;
    loop {
        let status = match store.version().await {
            Ok(local) => {
                let lagging = target
                    .iter()
                    .filter(|(actor, target)| {
                        local.get(*actor).copied().unwrap_or_default() < **target
                    })
                    .count();
                if lagging == 0 {
                    match store.has_known_missing_changes().await {
                        Ok(false) => return Ok(()),
                        Ok(true) => "known bookkeeping gaps remain".to_owned(),
                        Err(error) => error.to_string(),
                    }
                } else {
                    format!("{lagging} actor(s) remain behind the target")
                }
            }
            Err(error) => error.to_string(),
        };
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(500)) => {}
            () = tokio::time::sleep_until(warning_at) => {
                eprintln!("cluster store catch-up is still pending: {status}");
                warning_at = tokio::time::Instant::now() + warning_interval;
            }
        }
    }
}

pub async fn run_machine_publisher(
    replicated: Option<ReplicatedStore>,
    local: Arc<Mutex<LocalMachineStore>>,
    participating: watch::Sender<bool>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    if let Some(replicated) = &replicated {
        let (joining, target) = {
            let local = local
                .lock()
                .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
            match &local.record().body {
                LocalMachineBody::Joining {
                    min_store_version, ..
                } => (true, min_store_version.clone()),
                LocalMachineBody::Uninitialized { .. }
                | LocalMachineBody::Participating { .. }
                | LocalMachineBody::Resetting { .. } => (false, BTreeMap::new()),
            }
        };
        if joining {
            tokio::select! {
                result = wait_for_catch_up(replicated, &target) => {
                    result.map_err(io::Error::other)?;
                }
                () = shutdown.cancelled() => {
                    return Ok(());
                }
            }
            let publication = replicated.machine_publication().await;
            let completed = {
                let mut local = local
                    .lock()
                    .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
                publication
                    .complete_catch_up(&mut local)
                    .map_err(io::Error::other)?
            };
            if completed {
                // Join already restarted into Joining. Flip Participating
                // in-process so DNS/Caddy start; another process restart
                // kills an in-flight Caddy Deploy against this Machine.
                tracing::info!("catch-up complete");
                participating.send_replace(true);
            }
        }
    }
    loop {
        if let Some(replicated) = &replicated {
            let cluster_network = {
                let local = local
                    .lock()
                    .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
                local.record().cluster_network()
            };
            if let Some(network) = cluster_network
                && let Err(error) = replicated.publish_cluster_network(network).await
            {
                eprintln!("failed to publish Cluster network: {error}");
            }
            let publication = replicated.machine_publication().await;
            let machine = {
                let local = local
                    .lock()
                    .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
                publication.publishable_machine(local.record())
            };
            if let Some(machine) = machine
                && let Err(error) = publication.publish(&machine).await
            {
                eprintln!("failed to publish local Machine: {error}");
            }
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(60)) => {}
            () = shutdown.cancelled() => {
                return Ok(());
            }
        }
    }
}

fn decode_observations<T: DeserializeOwned>(
    rows: Vec<[Value; 2]>,
) -> Result<ReplicatedObservations<T>, Error> {
    let mut observations = Vec::new();
    let mut incomplete_ids = Vec::new();
    for [id, encoded] in rows {
        let id = text(&id, "row ID")?.to_owned();
        let encoded = text(&encoded, "replicated JSON")?;
        if encoded.is_empty() || encoded == "{}" {
            incomplete_ids.push(id);
        } else {
            observations.push(serde_json::from_str(encoded)?);
        }
    }
    Ok(ReplicatedObservations {
        observations,
        incomplete_ids,
    })
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, Error> {
    value
        .as_str()
        .ok_or_else(|| Error::Protocol(format!("invalid {field}")))
}

fn actor_id(value: &Value) -> Result<String, Error> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Array(bytes) => bytes
            .iter()
            .map(|byte| {
                byte.as_u64()
                    .and_then(|byte| u8::try_from(byte).ok())
                    .map(|byte| format!("{byte:02x}"))
                    .ok_or_else(|| Error::Protocol("invalid actor ID byte".into()))
            })
            .collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => {
            Err(Error::Protocol("invalid actor ID".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        net::TcpListener,
        sync::{Arc, Mutex},
        time::SystemTime,
    };

    use ployz_core::{IngressHost, IssuanceClock, IssuanceFailure, Machine};
    use serde_json::json;

    use super::ReplicatedStore;
    use crate::corrosion::ApiClient;
    use crate::machine::{
        LocalMachineBody, LocalMachinePrior, LocalMachineRecord, LocalMachineStore,
    };

    #[tokio::test]
    async fn catch_up_waits_for_removal_and_rechecks_phase() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let store = ReplicatedStore::new(
            ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
        );
        let data_dir = std::env::temp_dir().join(format!(
            "ployzd-catch-up-reset-{}",
            ployz_core::MachineId::random()
        ));
        let mut local = LocalMachineStore::open(&data_dir).unwrap();
        let public_key = local.record().wireguard_private_key.public_key();
        let machine: Machine = serde_json::from_value(json!({
            "id": "b".repeat(32),
            "name": "joining",
            "subnet": "10.210.1.0/24",
            "management_address": "fdcc::1",
            "public_key": public_key.0,
        }))
        .unwrap();
        local
            .join(machine.clone(), vec![machine], BTreeMap::new(), None)
            .unwrap();
        let local = Arc::new(Mutex::new(local));

        let first = store.machine_publication().await;
        let clone = store.clone();
        let task_local = Arc::clone(&local);
        let (started, waiting) = tokio::sync::oneshot::channel();
        let second = tokio::spawn(async move {
            started.send(()).unwrap();
            let publication = clone.machine_publication().await;
            publication.complete_catch_up(&mut task_local.lock().unwrap())
        });
        waiting.await.unwrap();
        tokio::task::yield_now().await;
        assert!(!second.is_finished());

        local.lock().unwrap().begin_reset().unwrap();
        drop(first);
        let completed = tokio::time::timeout(std::time::Duration::from_secs(1), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!completed);
        drop(local);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn container_publication_waits_for_removal() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let store = ReplicatedStore::new(
            ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
        );
        let (machine, _local) = participating_record();
        let machine_id = machine.id;
        let first = store.machine_publication().await;
        let clone = store.clone();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let second = tokio::spawn(async move {
            started.send(()).unwrap();
            let publication = clone.machine_publication().await;
            publication
                .apply_container_rows(&machine_id, &[], &[])
                .await
        });
        waiting.await.unwrap();
        tokio::task::yield_now().await;
        assert!(!second.is_finished());

        drop(first);
        tokio::time::timeout(std::time::Duration::from_secs(1), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn record_certificate_failure_is_an_error_when_the_store_is_unreachable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let store = ReplicatedStore::new(ApiClient::new(address, &"a".repeat(64)).unwrap());
        let hostname = IngressHost::parse("app.example.com").unwrap();
        assert!(
            store
                .record_certificate_failure(
                    &hostname,
                    "does not resolve",
                    IssuanceClock::new(1, SystemTime::UNIX_EPOCH, IssuanceFailure::DoesNotResolve,),
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn publication_guard_rechecks_the_local_phase() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let store = ReplicatedStore::new(
            ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
        );
        let (machine, local) = participating_record();
        let publication = store.machine_publication().await;
        assert_eq!(publication.publishable_machine(&local), Some(machine));

        let LocalMachineBody::Participating {
            machine: body_machine,
            cluster_network,
            bootstrap,
        } = local.body
        else {
            panic!("fixture is participating");
        };
        let local = LocalMachineRecord {
            body: LocalMachineBody::Resetting {
                prior: Box::new(LocalMachinePrior::Participating {
                    machine: body_machine,
                    cluster_network,
                    bootstrap,
                }),
            },
            ..local
        };
        assert_eq!(publication.publishable_machine(&local), None);
    }

    fn participating_record() -> (Machine, LocalMachineRecord) {
        let machine: Machine = serde_json::from_value(json!({
            "id": "b".repeat(32),
            "name": "machine",
            "subnet": "10.210.1.0/24",
            "management_address": "fdcc::1",
            "public_key": vec![3; 32],
        }))
        .unwrap();
        let local = LocalMachineRecord {
            body: LocalMachineBody::Participating {
                machine: machine.clone(),
                cluster_network: None,
                bootstrap: Vec::new(),
            },
            wireguard_private_key: crate::network::WireGuardPrivateKey::from_bytes([0; 32]),
            wireguard_mtu: None,
            selected_endpoints: BTreeMap::new(),
        };
        (machine, local)
    }
}
