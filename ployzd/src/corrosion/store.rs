use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use ipnet::Ipv4Net;
use ployz_core::{
    CERTIFICATE_POLICY_CLUSTER_KEY, ContainerId, ContainerObservation, DockerVolume,
    DockerVolumeId, DockerVolumeName, IngressHost, IssuanceClock, Machine, MachineId,
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
            .execute([
                Statement::new(
                    "DELETE FROM volumes WHERE machine_id = ?",
                    [json!(machine_id)],
                ),
                Statement::new(
                    "DELETE FROM containers WHERE machine_id = ?",
                    [json!(machine_id)],
                ),
                Statement::new("DELETE FROM machines WHERE id = ?", [json!(machine_id)]),
            ])
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

    pub(crate) async fn local_volumes(
        &self,
        machine_id: &MachineId,
    ) -> Result<LocalVolumeSnapshot, Error> {
        self.store.local_volumes(machine_id).await
    }

    pub(crate) async fn apply_volume_rows(
        &self,
        machine_id: &MachineId,
        deletions: &[DockerVolumeName],
        upserts: &[DockerVolume],
    ) -> Result<(), Error> {
        if upserts
            .iter()
            .any(|volume| &volume.id.machine_id != machine_id)
        {
            return Err(Error::Protocol(
                "local volume reconciliation crossed Machine authority".into(),
            ));
        }
        let mut statements = deletions
            .iter()
            .map(|name| {
                Statement::new(
                    "DELETE FROM volumes WHERE machine_id = ? AND name = ?",
                    [json!(machine_id), json!(name)],
                )
            })
            .collect::<Vec<_>>();
        for volume in upserts {
            statements.push(volume_upsert(volume)?);
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
        decode_json_document(text(info, "machine info")?)
    }

    pub async fn machines(&self) -> Result<ReplicatedObservations<Machine, MachineId>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT id, info FROM machines ORDER BY name",
                [],
            ))
            .await?;
        decode_observations(id_and_json(query.rows(["id", "info"])?, |id| {
            Ok(MachineId::parse(id)?)
        })?)
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
        decode_json_document(text(encoded, "replicated container JSON")?)
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
            let observation = decode_json_document(text(&encoded, "replicated container JSON")?)?;
            snapshot.inventory.insert(id);
            if let Some(observation) = observation {
                snapshot.observations.insert(id, observation);
            }
        }
        Ok(snapshot)
    }

    pub async fn containers(
        &self,
    ) -> Result<ReplicatedObservations<ContainerObservation, ContainerId>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT id, container FROM containers ORDER BY id",
                [],
            ))
            .await?;
        decode_observations(id_and_json(query.rows(["id", "container"])?, |id| {
            Ok(ContainerId::parse(id)?)
        })?)
    }

    pub(crate) async fn subscribe_container_changes(&self) -> Result<Subscription, Error> {
        self.api
            .subscribe(Statement::new("SELECT id, container FROM containers", []))
            .await
    }

    /// Publish a Docker Volume observation. Unchanged documents are not rewritten.
    ///
    /// # Errors
    ///
    /// Returns if the row cannot be read or written.
    pub async fn publish_volume(&self, volume: &DockerVolume) -> Result<(), Error> {
        if self.volume(&volume.id).await?.as_ref() == Some(volume) {
            return Ok(());
        }
        self.api.execute([volume_upsert(volume)?]).await?;
        Ok(())
    }

    /// Return one Docker Volume observation, or `None` if the row is missing or incomplete.
    ///
    /// # Errors
    ///
    /// Returns if the row cannot be read or decoded.
    pub async fn volume(&self, id: &DockerVolumeId) -> Result<Option<DockerVolume>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT volume FROM volumes WHERE machine_id = ? AND name = ?",
                [json!(id.machine_id), json!(id.name)],
            ))
            .await?;
        let rows = query.rows(["volume"])?;
        let Some([encoded]) = rows.first() else {
            return Ok(None);
        };
        decode_json_document(text(encoded, "replicated volume JSON")?)
    }

    /// Return decoded Docker Volume observations and typed incomplete volume IDs.
    ///
    /// An incomplete row is listed in `incomplete_ids`; it is not a deletion.
    ///
    /// # Errors
    ///
    /// Returns if rows cannot be read or decoded.
    pub async fn volumes(
        &self,
    ) -> Result<ReplicatedObservations<DockerVolume, DockerVolumeId>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT machine_id, name, volume FROM volumes ORDER BY machine_id, name",
                [],
            ))
            .await?;
        let mut rows = Vec::new();
        for [machine_id, name, encoded] in query.rows(["machine_id", "name", "volume"])? {
            rows.push((
                DockerVolumeId {
                    machine_id: MachineId::parse(text(&machine_id, "volume Machine ID")?)?,
                    name: DockerVolumeName::parse(text(&name, "Docker Volume name")?)?,
                },
                text(&encoded, "replicated volume JSON")?.to_owned(),
            ));
        }
        decode_observations(rows)
    }

    async fn local_volumes(&self, machine_id: &MachineId) -> Result<LocalVolumeSnapshot, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT name, volume FROM volumes WHERE machine_id = ? ORDER BY name",
                [json!(machine_id)],
            ))
            .await?;
        let mut snapshot = LocalVolumeSnapshot::default();
        for [name, encoded] in query.rows(["name", "volume"])? {
            let name = DockerVolumeName::parse(text(&name, "Docker Volume name")?)?;
            let observation = decode_json_document(text(&encoded, "replicated volume JSON")?)?;
            snapshot.inventory.insert(name.clone());
            if let Some(observation) = observation {
                snapshot.observations.insert(name, observation);
            }
        }
        Ok(snapshot)
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

    /// Return decoded certificate rows and typed incomplete Ingress Hostnames.
    ///
    /// An incomplete row is listed in `incomplete_ids`; it is not a deletion.
    ///
    /// # Errors
    ///
    /// Returns if rows cannot be read or decoded.
    pub async fn certificate_rows(
        &self,
    ) -> Result<ReplicatedObservations<(IngressHost, CertificateRow), IngressHost>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT hostname, body FROM certificates ORDER BY hostname",
                [],
            ))
            .await?;
        let mut observations = Vec::new();
        let mut incomplete_ids = Vec::new();
        for [hostname, encoded] in query.rows(["hostname", "body"])? {
            let hostname = IngressHost::parse(text(&hostname, "certificate hostname")?)?;
            let encoded = text(&encoded, "certificate body")?;
            if is_incomplete_document(encoded) {
                incomplete_ids.push(hostname);
            } else {
                observations.push((hostname, CertificateRow::decode(encoded)?));
            }
        }
        Ok(ReplicatedObservations {
            observations,
            incomplete_ids,
        })
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

#[derive(Default)]
pub(crate) struct LocalVolumeSnapshot {
    pub(crate) inventory: BTreeSet<DockerVolumeName>,
    pub(crate) observations: BTreeMap<DockerVolumeName, DockerVolume>,
}

impl LocalVolumeSnapshot {
    pub(crate) fn observed(&mut self, volume: DockerVolume) {
        self.inventory.insert(volume.id.name.clone());
        self.observations.insert(volume.id.name.clone(), volume);
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

fn volume_upsert(volume: &DockerVolume) -> Result<Statement, Error> {
    Ok(Statement::new(
        "INSERT INTO volumes (machine_id, name, volume, updated_at) VALUES (?, ?, ?, datetime('now')) ON CONFLICT (machine_id, name) DO UPDATE SET volume = excluded.volume, updated_at = excluded.updated_at",
        [
            json!(volume.id.machine_id),
            json!(volume.id.name),
            json!(serde_json::to_string(volume)?),
        ],
    ))
}

#[derive(Debug, Eq, PartialEq)]
pub struct ReplicatedObservations<T, Id> {
    pub observations: Vec<T>,
    pub incomplete_ids: Vec<Id>,
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

fn is_incomplete_document(encoded: &str) -> bool {
    encoded.is_empty() || encoded == "{}"
}

fn decode_json_document<T: DeserializeOwned>(encoded: &str) -> Result<Option<T>, Error> {
    if is_incomplete_document(encoded) {
        Ok(None)
    } else {
        Ok(Some(serde_json::from_str(encoded)?))
    }
}

fn id_and_json<Id>(
    rows: Vec<[Value; 2]>,
    parse_id: impl Fn(&str) -> Result<Id, Error>,
) -> Result<Vec<(Id, String)>, Error> {
    rows.into_iter()
        .map(|[id, encoded]| {
            Ok((
                parse_id(text(&id, "row ID")?)?,
                text(&encoded, "replicated JSON")?.to_owned(),
            ))
        })
        .collect()
}

fn decode_observations<T: DeserializeOwned, Id>(
    rows: Vec<(Id, String)>,
) -> Result<ReplicatedObservations<T, Id>, Error> {
    let mut observations = Vec::new();
    let mut incomplete_ids = Vec::new();
    for (id, encoded) in rows {
        match decode_json_document(&encoded)? {
            Some(observation) => observations.push(observation),
            None => incomplete_ids.push(id),
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
