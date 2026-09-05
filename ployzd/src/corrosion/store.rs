//! Replicated publication, subscriptions, and observer-relative reads.

mod decode;
use decode::{
    INCOMPLETE_JSON_DOCUMENT, actor_id, decode_container, decode_machine, decode_observations,
    decode_volume, id_and_json, is_incomplete_document, text,
};

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use futures_util::Stream;
use ipnet::Ipv4Net;
use ployz_core::{
    CERTIFICATE_POLICY_CLUSTER_KEY, ContainerId, ContainerObservation, DockerVolume,
    DockerVolumeId, DockerVolumeName, IngressHost, IssuanceClock, Machine, MachineId,
};
use serde_json::{Value, json};

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

// Solo founder backdates so the first Register is already quiet. ON CONFLICT
// leaves a later steal's `updated_at = now` alone.
pub(crate) const CLAIM_FOUNDER_ALLOCATOR: &str = "INSERT INTO cluster (key, value, updated_at) VALUES ('allocator', ?, datetime('now', '-5 seconds')) ON CONFLICT (key) DO NOTHING";
pub(crate) const STEAL_ALLOCATOR: &str = "INSERT INTO cluster (key, value, updated_at) VALUES ('allocator', ?, datetime('now')) ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at";
#[cfg(test)]
pub(crate) const AGE_ALLOCATOR: &str =
    "UPDATE cluster SET updated_at = datetime('now', '-5 seconds') WHERE key = 'allocator'";
pub(crate) const ALLOCATOR_ROW: &str = "SELECT value AS allocator, updated_at <= datetime('now', '-5 seconds') AS quiet FROM cluster WHERE key = 'allocator'";

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AllocatorRow {
    pub machine_id: MachineId,
    pub quiet: bool,
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
        if !matches!(local.record().body(), LocalMachineBody::Joining { .. }) {
            return Ok(false);
        }
        local.complete_catch_up()?;
        Ok(true)
    }

    pub(crate) fn publishable_machine(&self, local: &LocalMachineRecord) -> Option<Machine> {
        match local.body() {
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
        incomplete: &[DockerVolumeName],
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
        for name in incomplete {
            statements.push(volume_incomplete(machine_id, name));
        }
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

    /// Write `cluster.allocator` naming this Machine, with `updated_at = now - 5s`.
    ///
    /// # Errors
    ///
    /// Returns if the Cluster store cannot be written.
    pub async fn publish_founder_allocator(&self, machine_id: &MachineId) -> Result<(), Error> {
        self.api
            .execute([Statement::new(CLAIM_FOUNDER_ALLOCATOR, [json!(machine_id)])])
            .await
    }

    /// Write `cluster.allocator` naming this Machine, with `updated_at = now`.
    ///
    /// # Errors
    ///
    /// Returns if the Cluster store cannot be written.
    pub(crate) async fn steal_allocator(&self, machine_id: &MachineId) -> Result<(), Error> {
        self.api
            .execute([Statement::new(STEAL_ALLOCATOR, [json!(machine_id)])])
            .await
    }

    /// Local Allocator observation: named Machine ID and whether the row is ≥5s old.
    ///
    /// # Errors
    ///
    /// Returns if the Cluster store cannot be read or the row is malformed.
    pub(crate) async fn allocator(&self) -> Result<Option<AllocatorRow>, Error> {
        let query = self.api.query(Statement::new(ALLOCATOR_ROW, [])).await?;
        match query.rows(["allocator", "quiet"])?.as_slice() {
            [] => Ok(None),
            [[value, Value::Number(quiet)]] => Ok(Some(AllocatorRow {
                machine_id: MachineId::parse(text(value, "Allocator")?)?,
                quiet: quiet
                    .as_u64()
                    .filter(|value| *value <= 1)
                    .map(|value| value == 1)
                    .ok_or_else(|| Error::Protocol("invalid Allocator quiet".into()))?,
            })),
            _ => Err(Error::Protocol("invalid Allocator row".into())),
        }
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
        let document = text(value, "hosted DNS reservation")?;
        serde_json::from_str(document)
            .map(Some)
            .map_err(Error::InvalidDomainReservation)
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
        decode_machine(id, text(info, "machine info")?)
    }

    pub async fn machines(&self) -> Result<ReplicatedObservations<Machine, MachineId>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT id, info FROM machines ORDER BY name",
                [],
            ))
            .await?;
        decode_observations(
            id_and_json(query.rows(["id", "info"])?, |id| Ok(MachineId::parse(id)?))?,
            |id, encoded| decode_machine(id.as_str(), encoded),
        )
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
                "SELECT machine_id, container FROM containers WHERE id = ?",
                [json!(id)],
            ))
            .await?;
        let rows = query.rows(["machine_id", "container"])?;
        let Some([machine_id, encoded]) = rows.first() else {
            return Ok(None);
        };
        decode_container(
            id,
            text(machine_id, "container Machine ID")?,
            text(encoded, "replicated container JSON")?,
        )
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
            let observation = decode_container(
                &id,
                machine_id.as_str(),
                text(&encoded, "replicated container JSON")?,
            )?;
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
                "SELECT id, machine_id, container FROM containers ORDER BY id",
                [],
            ))
            .await?;
        let mut observations = Vec::new();
        let mut incomplete_ids = Vec::new();
        for [id, machine_id, encoded] in query.rows(["id", "machine_id", "container"])? {
            let id = ContainerId::parse(text(&id, "container ID")?)?;
            match decode_container(
                &id,
                text(&machine_id, "container Machine ID")?,
                text(&encoded, "replicated container JSON")?,
            )? {
                Some(observation) => observations.push(observation),
                None => incomplete_ids.push(id),
            }
        }
        Ok(ReplicatedObservations {
            observations,
            incomplete_ids,
        })
    }

    pub(crate) async fn subscribe_container_changes(&self) -> Result<Subscription, Error> {
        self.api
            .subscribe(Statement::new(
                "SELECT id, machine_id, container FROM containers",
                [],
            ))
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
        decode_volume(id, text(encoded, "replicated volume JSON")?)
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
        decode_observations(rows, decode_volume)
    }

    async fn local_volumes(&self, machine_id: &MachineId) -> Result<LocalVolumeSnapshot, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT name, volume FROM volumes WHERE machine_id = ? ORDER BY name",
                [json!(machine_id)],
            ))
            .await?;
        let mut names = Vec::new();
        let mut observations = Vec::new();
        for [name, encoded] in query.rows(["name", "volume"])? {
            let name = DockerVolumeName::parse(text(&name, "Docker Volume name")?)?;
            let observation = decode_volume(
                &DockerVolumeId {
                    machine_id: *machine_id,
                    name: name.clone(),
                },
                text(&encoded, "replicated volume JSON")?,
            )?;
            names.push(name);
            if let Some(observation) = observation {
                observations.push(observation);
            }
        }
        let mut snapshot = LocalVolumeSnapshot::from_inventory(names);
        for observation in observations {
            snapshot.observed(observation);
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

    /// Wake when any replicated observation table changes.
    ///
    /// The Corrosion `Change` payload is discarded; callers re-read the store.
    ///
    /// # Errors
    ///
    /// Returns if a subscription cannot be opened.
    pub(crate) async fn subscribe_runtime_watch_changes(
        &self,
    ) -> Result<impl Stream<Item = Result<(), Error>> + Send + use<>, Error> {
        let changes = RuntimeWatchChanges {
            machines: self
                .api
                .subscribe(Statement::new("SELECT id, info FROM machines", []))
                .await?,
            containers: self.subscribe_container_changes().await?,
            volumes: self
                .api
                .subscribe(Statement::new(
                    "SELECT machine_id, name, volume FROM volumes",
                    [],
                ))
                .await?,
            certificates: self.subscribe_certificate_changes().await?,
            cluster: self
                .api
                .subscribe(Statement::new("SELECT key, value FROM cluster", []))
                .await?,
        };
        Ok(futures_util::stream::unfold(changes, |mut changes| async {
            Some((changes.changed().await, changes))
        }))
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
    volumes: BTreeMap<DockerVolumeName, Option<DockerVolume>>,
}

impl LocalVolumeSnapshot {
    pub(crate) fn from_inventory(names: impl IntoIterator<Item = DockerVolumeName>) -> Self {
        Self {
            volumes: names.into_iter().map(|name| (name, None)).collect(),
        }
    }

    pub(crate) fn observed(&mut self, volume: DockerVolume) {
        self.volumes.insert(volume.id.name.clone(), Some(volume));
    }

    pub(crate) fn get(&self, name: &DockerVolumeName) -> Option<Option<&DockerVolume>> {
        self.volumes.get(name).map(Option::as_ref)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&DockerVolumeName, Option<&DockerVolume>)> {
        self.volumes
            .iter()
            .map(|(name, volume)| (name, volume.as_ref()))
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

fn volume_incomplete(machine_id: &MachineId, name: &DockerVolumeName) -> Statement {
    Statement::new(
        "INSERT INTO volumes (machine_id, name, volume, updated_at) VALUES (?, ?, ?, datetime('now')) ON CONFLICT (machine_id, name) DO UPDATE SET volume = excluded.volume, updated_at = excluded.updated_at",
        [
            json!(machine_id),
            json!(name),
            json!(INCOMPLETE_JSON_DOCUMENT),
        ],
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplicatedObservations<T, Id> {
    pub observations: Vec<T>,
    pub incomplete_ids: Vec<Id>,
}

/// Store subscriptions that wake observation readers. Change payloads are ignored.
struct RuntimeWatchChanges {
    machines: Subscription,
    containers: Subscription,
    volumes: Subscription,
    certificates: Subscription,
    cluster: Subscription,
}

impl RuntimeWatchChanges {
    async fn changed(&mut self) -> Result<(), Error> {
        tokio::select! {
            result = self.machines.changed() => result,
            result = self.containers.changed() => result,
            result = self.volumes.changed() => result,
            result = self.certificates.changed() => result,
            result = self.cluster.changed() => result,
        }
    }
}
