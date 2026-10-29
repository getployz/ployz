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
    CERTIFICATE_POLICY_CLUSTER_KEY, CertificateHost, ClusterRoute, ContainerId,
    ContainerObservation, DockerVolume, DockerVolumeId, DockerVolumeName, ForgottenAnswer,
    IngressHost, IssuanceClock, Machine, MachineId, ManagementIdentity, QualifiedService,
    SealingPublicKey, forgotten_key,
};
use serde_json::json;

use super::{
    ApiClient, CertificateChallenge, CertificateMaterial, CertificateRow, Error, Statement,
    Subscription,
};
use crate::machine::{LocalMachineBody, LocalMachineError, LocalMachineRecord, RecordOwner};

#[derive(Clone)]
pub struct ReplicatedStore {
    api: ApiClient,
    machine_publication: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RuntimeOverlay {
    pub(crate) running_builds: u32,
    pub(crate) pool_committable_bytes: Option<u64>,
    pub(crate) sealing: Option<ployz_core::Sealing>,
}

pub(crate) struct MachinePublicationGuard<'a> {
    store: &'a ReplicatedStore,
    _guard: tokio::sync::MutexGuard<'a, ()>,
}

impl MachinePublicationGuard<'_> {
    /// Persist catch-up completion while this publication guard is held.
    ///
    /// Returns `false` when the record was not joining, so nothing changed.
    ///
    /// # Errors
    ///
    /// Returns [`LocalMachineError::RecordOwner`] when the record owner has stopped
    /// and [`LocalMachineError::Store`] when the record cannot be written.
    pub(crate) async fn complete_catch_up(
        &self,
        local: &RecordOwner,
    ) -> Result<bool, LocalMachineError> {
        let completed = local
            .mutate(|store| {
                if !matches!(store.record().body(), LocalMachineBody::Joining { .. }) {
                    return Ok(false);
                }
                store.complete_catch_up().map(|()| true)
            })
            .await??;
        Ok(completed)
    }

    /// This Machine as it publishes: its record with what it observes of itself now.
    pub(crate) fn publishable_machine(
        &self,
        local: &LocalMachineRecord,
        overlay: RuntimeOverlay,
    ) -> Option<Machine> {
        match local.body() {
            LocalMachineBody::Participating { machine, .. } => {
                let mut machine = machine.clone();
                machine.runtime.running_builds = overlay.running_builds;
                machine.runtime.pool_committable_bytes = overlay.pool_committable_bytes;
                machine.runtime.sealing = overlay.sealing;
                machine.runtime.management_identity = Some(local.management_secret().public_key());
                Some(machine)
            }
            LocalMachineBody::Uninitialized { .. }
            | LocalMachineBody::Joining { .. }
            | LocalMachineBody::Resetting { .. } => None,
        }
    }

    pub(crate) async fn publish(&self, machine: &Machine) -> Result<(), Error> {
        self.store.publish_local_machine_unlocked(machine).await
    }

    pub(crate) async fn remove(&self, machine_id: &MachineId) -> Result<(), Error> {
        self.store.api.execute(removal(machine_id)).await
    }

    /// Say in `machine_id`'s row that it is leaving, or take that back, for a deploy
    /// that plans the Ingress Proxy and checks it (#3277). An update in place: a row
    /// the Cluster already forgot stays gone rather than coming back marked.
    pub(crate) async fn mark_leaving(
        &self,
        machine_id: &MachineId,
        leaving: bool,
    ) -> Result<(), Error> {
        let statement = if leaving {
            "UPDATE machines SET info = json_set(info, '$.runtime.leaving', json('true')) WHERE id = ?"
        } else {
            "UPDATE machines SET info = json_remove(info, '$.runtime.leaving') WHERE id = ?"
        };
        self.store
            .api
            .execute([Statement::new(statement, [json!(machine_id)])])
            .await
    }

    /// Remove a Machine another Machine forgot, leaving a tombstone keyed by the
    /// row's Management Identity so the forgotten Machine can learn it was forgotten
    /// (#2927). A row without an identity leaves none. One transaction, so no
    /// reader sees the row gone and the tombstone absent.
    pub(crate) async fn forget(&self, machine_id: &MachineId) -> Result<(), Error> {
        let tombstone = Statement::new(
            "INSERT INTO cluster (key, value) \
             SELECT 'forgotten:' || json_extract(info, '$.runtime.management_identity'), id \
             FROM machines \
             WHERE id = ? AND json_extract(info, '$.runtime.management_identity') IS NOT NULL \
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [json!(machine_id)],
        );
        self.store
            .api
            .execute(std::iter::once(tombstone).chain(removal(machine_id)))
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

pub(crate) const SEALING_PUBLIC_KEY_CLUSTER_KEY: &str = "sealing_public_key";
pub(crate) const SEALING_AWAITS_RECOVERY_CLUSTER_KEY: &str = "sealing_awaits_recovery";

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

    pub async fn forget_machine(&self, machine_id: &MachineId) -> Result<(), Error> {
        self.machine_publication().await.forget(machine_id).await
    }

    /// What this Machine knows about `identity`: Listed when a Machine row carries
    /// it or the id its tombstone names is listed again, Forgotten when only the
    /// tombstone remains, NoAnswer when nothing mentions it.
    pub async fn forgotten(&self, identity: ManagementIdentity) -> Result<ForgottenAnswer, Error> {
        let key = forgotten_key(&identity);
        let query = self
            .api
            .query(Statement::new(
                "SELECT (SELECT value FROM cluster WHERE key = ?) AS forgotten_id, \
                 EXISTS (SELECT 1 FROM machines \
                         WHERE json_extract(info, '$.runtime.management_identity') = ? \
                         OR id = (SELECT value FROM cluster WHERE key = ?)) AS listed",
                [json!(key), json!(identity.to_hex()), json!(key)],
            ))
            .await?;
        let rows = query.rows(["forgotten_id", "listed"])?;
        let Some([forgotten_id, listed]) = rows.first() else {
            return Ok(ForgottenAnswer::NoAnswer);
        };
        Ok(if listed.as_i64() == Some(1) {
            ForgottenAnswer::Listed
        } else if forgotten_id.is_null() {
            ForgottenAnswer::NoAnswer
        } else {
            ForgottenAnswer::Forgotten
        })
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
        // A forgotten Machine whose daemon still runs would write its row straight back,
        // undoing the forget (#3418). Its tombstone names its own id under its identity,
        // so it stays gone; an Entry's Register carries no identity and still lists it.
        let tombstone = machine
            .runtime
            .management_identity
            .as_ref()
            .map(forgotten_key);
        self.api
            .execute([Statement::new(
                "INSERT INTO machines (id, info) SELECT ?1, ?2 \
                 WHERE EXISTS (SELECT 1 FROM machines WHERE id = ?1) \
                 OR NOT EXISTS (SELECT 1 FROM cluster WHERE key = ?3 AND value = ?1) \
                 ON CONFLICT (id) DO UPDATE SET info = excluded.info",
                [
                    json!(machine.id),
                    json!(serde_json::to_string(machine)?),
                    json!(tombstone),
                ],
            )])
            .await?;
        Ok(())
    }

    pub async fn publish_cluster_network(&self, network: Ipv4Net) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                "INSERT INTO cluster (key, value) VALUES ('network', ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [json!(network.to_string())],
            )])
            .await?;
        Ok(())
    }

    pub async fn publish_sealing_public_key(&self, key: SealingPublicKey) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                "INSERT INTO cluster (key, value) VALUES ('sealing_public_key', ?) ON CONFLICT (key) DO NOTHING",
                [json!(key.to_string())],
            )])
            .await?;
        Ok(())
    }

    /// Name the Sealing public key a keyless founder waits to recover. The
    /// first write wins and nothing clears it; a published
    /// `sealing_public_key` supersedes it.
    pub async fn publish_sealing_awaits_recovery(
        &self,
        key: SealingPublicKey,
    ) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                "INSERT INTO cluster (key, value) VALUES ('sealing_awaits_recovery', ?) ON CONFLICT (key) DO NOTHING",
                [json!(key.to_string())],
            )])
            .await?;
        Ok(())
    }

    /// The Cluster's published Sealing public key, or `None` for a Cluster
    /// founded before Sealing Keys existed and not yet backfilled.
    ///
    /// # Errors
    /// Returns an error when the Corrosion query fails, or the stored value
    /// is not a Sealing public key.
    pub async fn sealing_public_key(&self) -> Result<Option<SealingPublicKey>, Error> {
        self.sealing_cluster_key(SEALING_PUBLIC_KEY_CLUSTER_KEY, "Sealing public key")
            .await
    }

    /// The Sealing public key a recovery is waiting on, written when the
    /// Cluster lost its key.
    ///
    /// # Errors
    /// Returns an error when the Corrosion query fails, or the stored value
    /// is not a Sealing public key.
    pub async fn sealing_awaits_recovery(&self) -> Result<Option<SealingPublicKey>, Error> {
        self.sealing_cluster_key(
            SEALING_AWAITS_RECOVERY_CLUSTER_KEY,
            "Sealing public key awaiting recovery",
        )
        .await
    }

    /// The Sealing public key the Cluster awaits from recovery, unless a
    /// published key superseded the marker.
    ///
    /// # Errors
    /// Returns an error when either read fails.
    pub async fn awaited_sealing_key(&self) -> Result<Option<SealingPublicKey>, Error> {
        match self.sealing_public_key().await? {
            Some(_) => Ok(None),
            None => self.sealing_awaits_recovery().await,
        }
    }

    async fn sealing_cluster_key(
        &self,
        key: &str,
        what: &str,
    ) -> Result<Option<SealingPublicKey>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT value FROM cluster WHERE key = ?",
                [json!(key)],
            ))
            .await?;
        let rows = query.rows(["value"])?;
        let Some([value]) = rows.first() else {
            return Ok(None);
        };
        SealingPublicKey::try_from(text(value, what)?.to_owned())
            .map(Some)
            .map_err(|error| {
                Error::Protocol(format!(
                    "invalid {what}: {error}",
                    error = ployz_core::error_chain::inline(&error),
                ))
            })
    }

    /// The published Cluster network, or `None` before the founder's
    /// publisher has written it.
    ///
    /// # Errors
    /// Returns an error when the Corrosion query fails, or the stored value
    /// is not a valid Cluster network.
    pub async fn cluster_network(&self) -> Result<Option<Ipv4Net>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT value FROM cluster WHERE key = 'network'",
                [],
            ))
            .await?;
        let rows = query.rows(["value"])?;
        let Some([value]) = rows.first() else {
            return Ok(None);
        };
        text(value, "Cluster network")?
            .parse()
            .map(Some)
            .map_err(|error| {
                Error::Protocol(format!(
                    "invalid Cluster network: {error}",
                    error = ployz_core::error_chain::inline(&error),
                ))
            })
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

    /// Wake when any Machine row changes. The payload is discarded; callers re-read.
    ///
    /// # Errors
    ///
    /// Returns if the subscription cannot be opened.
    pub(crate) async fn subscribe_machine_changes(&self) -> Result<Subscription, Error> {
        self.api
            .subscribe(Statement::new("SELECT id, info FROM machines", []))
            .await
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
        route: ClusterRoute,
    ) -> Result<(), Error> {
        let Some((key, latest)) = self.acme_row(hostname).await? else {
            return Ok(());
        };
        let issued = CertificateRow::issued(material.clone(), route);
        if latest == issued {
            return Ok(());
        }
        self.upsert_certificate(&key, &issued).await
    }

    /// Hold published material for `hostname`; ACME leaves the row alone from now on.
    ///
    /// # Errors
    ///
    /// Returns if the row cannot be read or written.
    pub async fn publish_certificate_material(
        &self,
        hostname: &CertificateHost,
        material: CertificateMaterial,
    ) -> Result<(), Error> {
        if self.row(hostname).await?.published() == Some(&material) {
            return Ok(());
        }
        self.upsert_certificate(hostname, &CertificateRow::Published(material))
            .await
    }

    /// Remove published material so ACME owns `hostname` again. ACME-issued rows stay.
    ///
    /// # Errors
    ///
    /// Returns if the row cannot be read or deleted.
    pub async fn clear_published_certificate(
        &self,
        hostname: &CertificateHost,
    ) -> Result<(), Error> {
        if self.row(hostname).await?.published().is_none() {
            return Ok(());
        }
        self.api
            .execute([Statement::new(
                "DELETE FROM certificates WHERE hostname = ?",
                [json!(hostname.as_str())],
            )])
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
        self.row(&CertificateHost::from(hostname.clone())).await
    }

    /// The row ACME may write for `hostname` and its key, or `None` when it holds published material.
    async fn acme_row(
        &self,
        hostname: &IngressHost,
    ) -> Result<Option<(CertificateHost, CertificateRow)>, Error> {
        let key = CertificateHost::from(hostname.clone());
        let row = self.row(&key).await?;
        Ok(row.published().is_none().then_some((key, row)))
    }

    async fn row(&self, hostname: &CertificateHost) -> Result<CertificateRow, Error> {
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
        hostname: &CertificateHost,
        row: &CertificateRow,
    ) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                "INSERT INTO certificates (hostname, body) VALUES (?, ?) ON CONFLICT (hostname) DO UPDATE SET body = excluded.body",
                [json!(hostname.as_str()), json!(row.encode()?)],
            )])
            .await
    }

    pub async fn publish_certificate_challenge(
        &self,
        hostname: &IngressHost,
        challenge: &CertificateChallenge,
    ) -> Result<(), Error> {
        let Some((key, latest)) = self.acme_row(hostname).await? else {
            return Ok(());
        };
        if latest.challenge() == Some(challenge) {
            return Ok(());
        }
        self.upsert_certificate(&key, &latest.with_challenge(challenge.clone()))
            .await
    }

    /// Record why a hostname has no certificate, or why its renewal failed, and when the Cluster may
    /// try again.
    ///
    /// # Errors
    ///
    /// Returns if the row cannot be read or written.
    pub async fn record_certificate_failure(
        &self,
        hostname: &IngressHost,
        tried: Option<&CertificateMaterial>,
        last_error: impl Into<String>,
        clock: IssuanceClock,
    ) -> Result<(), Error> {
        let Some((key, latest)) = self.acme_row(hostname).await? else {
            return Ok(());
        };
        let Some(tried) = tried else {
            if latest.material().is_some() {
                return Ok(());
            }
            return self
                .upsert_certificate(&key, &latest.with_backoff(last_error, clock))
                .await;
        };
        // A failed renewal keeps serving its material and backs off like a first order. Only the
        // failure fields change, and only over the ACME material that failed, so a renewal, a
        // publication or another Machine's challenge landing after the read survives.
        let failed: serde_json::Value =
            serde_json::from_str(&latest.with_backoff(last_error, clock).encode()?)?;
        let field = |name: &str| failed.get(name).cloned().unwrap_or_default();
        self.api
            .execute([Statement::new(
                "UPDATE certificates SET body = json_set(body, \
                   '$.last_error', ?, '$.next_attempt_at', ?, '$.failures', ?, '$.last_failure', ?) \
                 WHERE hostname = ? \
                 AND json_extract(body, '$.certificate') = ? \
                 AND NOT coalesce(json_extract(body, '$.published'), 0)",
                [
                    field("last_error"),
                    field("next_attempt_at"),
                    field("failures"),
                    field("last_failure"),
                    json!(key.as_str()),
                    json!(tried.certificate()),
                ],
            )])
            .await
    }

    pub async fn record_certificate_error(
        &self,
        hostname: &IngressHost,
        reason: &str,
    ) -> Result<(), Error> {
        let Some((key, latest)) = self.acme_row(hostname).await? else {
            return Ok(());
        };
        if latest.last_error() == Some(reason) {
            return Ok(());
        }
        self.upsert_certificate(&key, &latest.with_error(reason))
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

    pub(crate) async fn ingress_routes(
        &self,
    ) -> Result<BTreeMap<(MachineId, QualifiedService), IngressRouteRow>, Error> {
        self.ingress_route_rows(Statement::new(INGRESS_ROUTES, []))
            .await
    }

    pub(crate) async fn service_ingress_routes(
        &self,
        service: &QualifiedService,
    ) -> Result<BTreeMap<(MachineId, QualifiedService), IngressRouteRow>, Error> {
        self.ingress_route_rows(Statement::new(
            SERVICE_INGRESS_ROUTES,
            [json!(service.to_string())],
        ))
        .await
    }

    async fn ingress_route_rows(
        &self,
        statement: Statement,
    ) -> Result<BTreeMap<(MachineId, QualifiedService), IngressRouteRow>, Error> {
        let query = self.api.query(statement).await?;
        Ok(query
            .rows(["machine_id", "service_id", "containers"])?
            .into_iter()
            .filter_map(|[machine_id, service_id, containers]| {
                let machine_id = MachineId::parse(machine_id.as_str()?).ok()?;
                let service = QualifiedService::parse(service_id.as_str()?).ok()?;
                let containers = containers
                    .as_str()
                    .and_then(|containers| serde_json::from_str(containers).ok());
                let row = match containers {
                    Some(containers) => IngressRouteRow::Acked(IngressRouteAck { containers }),
                    None => IngressRouteRow::UnknownContainers,
                };
                Some(((machine_id, service), row))
            })
            .collect())
    }

    pub(crate) async fn publish_ingress_routes(
        &self,
        machine_id: &MachineId,
        changes: &BTreeMap<QualifiedService, IngressRouteChange>,
    ) -> Result<(), Error> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut statements = Vec::with_capacity(changes.len());
        for (service, ack) in changes {
            statements.push(match ack {
                IngressRouteChange::Upsert(ack) => Statement::new(
                    INGRESS_ROUTE_UPSERT,
                    [
                        json!(machine_id),
                        json!(service.to_string()),
                        json!(serde_json::to_string(&ack.containers)?),
                    ],
                ),
                IngressRouteChange::Delete => Statement::new(
                    INGRESS_ROUTE_DELETE,
                    [json!(machine_id), json!(service.to_string())],
                ),
            });
        }
        self.api.execute(statements).await?;
        Ok(())
    }

    pub async fn certificates(
        &self,
    ) -> Result<BTreeMap<CertificateHost, CertificateMaterial>, Error> {
        Ok(self
            .certificate_state()
            .await?
            .into_iter()
            .filter_map(|(hostname, row)| row.into_material().map(|material| (hostname, material)))
            .collect())
    }

    /// Return decoded certificate rows and typed incomplete certificate hostnames.
    ///
    /// An incomplete row is listed in `incomplete_ids`; it is not a deletion. A row that
    /// won't decode is logged and left out.
    ///
    /// # Errors
    ///
    /// Returns if rows cannot be read.
    pub async fn certificate_rows(
        &self,
    ) -> Result<ReplicatedObservations<(CertificateHost, CertificateRow), CertificateHost>, Error>
    {
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
            let encoded = text(&encoded, "certificate body")?;
            let Some(hostname) = certificate_host(text(&hostname, "certificate hostname")?) else {
                continue;
            };
            if is_incomplete_document(encoded) {
                incomplete_ids.push(hostname);
            } else if let Some(row) = decode_certificate_row(&hostname, encoded) {
                observations.push((hostname, row));
            }
        }
        Ok(ReplicatedObservations {
            observations,
            incomplete_ids,
        })
    }

    pub async fn certificate_state(
        &self,
    ) -> Result<BTreeMap<CertificateHost, CertificateRow>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT hostname, body FROM certificates ORDER BY hostname",
                [],
            ))
            .await?;
        let mut rows = BTreeMap::new();
        for [hostname, encoded] in query.rows(["hostname", "body"])? {
            let encoded = text(&encoded, "certificate body")?;
            let Some(hostname) = certificate_host(text(&hostname, "certificate hostname")?) else {
                continue;
            };
            if let Some(row) = decode_certificate_row(&hostname, encoded) {
                rows.insert(hostname, row);
            }
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
            machines: self.subscribe_machine_changes().await?,
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

    /// This Machine's own Corrosion actor, which only this Machine's writes advance.
    pub(crate) async fn local_actor(&self) -> Result<String, Error> {
        let query = self
            .api
            .query(Statement::new("SELECT crsql_site_id() AS site_id", []))
            .await?;
        match query.rows(["site_id"])?.as_slice() {
            [[actor]] => actor_id(actor),
            _ => Err(Error::Protocol("invalid local actor row".into())),
        }
    }

    /// Whether this store has ever held a row for a Machine other than `own`. A removed
    /// row keeps its cr-sqlite key, so only a store created afresh, before gossip
    /// reaches it, reads `false` once another Machine exists.
    pub(crate) async fn held_another_machine(&self, own: &MachineId) -> Result<bool, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT EXISTS (SELECT 1 FROM machines__crsql_pks WHERE id != ?) AS held",
                [json!(own)],
            ))
            .await?;
        match query.rows(["held"])?.as_slice() {
            [[held]] if held.as_i64() == Some(0) => Ok(false),
            [[held]] if held.as_i64() == Some(1) => Ok(true),
            _ => Err(Error::Protocol("invalid held-another-Machine row".into())),
        }
    }

    /// Each actor's highest version this store holds with no hole below it: a catch-up
    /// target.
    pub async fn version(&self) -> Result<BTreeMap<String, i64>, Error> {
        let query = self.api.query(Statement::new(VERSION_QUERY, [])).await?;
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

    /// Checks replication bookkeeping through the target in one SQLite snapshot.
    /// Superseded versions can complete before replacement data arrives; callers
    /// requiring a particular record or condition must still verify it.
    ///
    /// # Errors
    /// Returns an error for invalid targets, failed store queries, or malformed responses.
    pub(crate) async fn has_reached_version(
        &self,
        target: &BTreeMap<String, i64>,
    ) -> Result<bool, Error> {
        let mut placeholders = Vec::with_capacity(target.len());
        let mut params = Vec::with_capacity(target.len() * 2);
        for (actor, version) in target {
            let bytes = hex::decode(actor.replace('-', ""))
                .map_err(|_| Error::InvalidCatchUpTarget("invalid actor ID"))?;
            if bytes.len() != 16 || *version < 0 {
                return Err(Error::InvalidCatchUpTarget(
                    "actor must be 16 bytes and version must be nonnegative",
                ));
            }
            if *version == 0 {
                continue;
            }
            placeholders.push("(?, ?)");
            params.extend([json!(bytes), json!(version)]);
        }
        if params.is_empty() {
            return Ok(true);
        }
        let query = format!(
            "WITH target(actor_id, version) AS (VALUES {})
             SELECT NOT EXISTS (
                 SELECT 1 FROM target
                 LEFT JOIN crsql_db_versions AS current ON current.site_id = target.actor_id
                 WHERE COALESCE(current.db_version, 0) < target.version
                 OR EXISTS (
                     SELECT 1 FROM __corro_bookkeeping_gaps
                     WHERE actor_id = target.actor_id AND start <= target.version
                 )
                 OR EXISTS (
                     SELECT 1 FROM __corro_seq_bookkeeping
                     WHERE site_id = target.actor_id AND db_version <= target.version
                 )
             ) AS reached",
            placeholders.join(", ")
        );
        let result = self.api.query(Statement::new(query, params)).await?;
        match result.rows(["reached"])?.as_slice() {
            [[value]] if value.as_i64() == Some(0) => Ok(false),
            [[value]] if value.as_i64() == Some(1) => Ok(true),
            _ => Err(Error::Protocol("invalid replication status row".into())),
        }
    }
}

/// A dead actor's hole is shared by every live store and never fills, so a target past
/// it could never be reached (#3611).
pub(super) const VERSION_QUERY: &str = "SELECT site_id, db_version FROM (
     SELECT current.site_id, MIN(
         current.db_version,
         COALESCE((SELECT MIN(start) FROM __corro_bookkeeping_gaps
                   WHERE actor_id = current.site_id) - 1, current.db_version),
         COALESCE((SELECT MIN(db_version) FROM __corro_seq_bookkeeping
                   WHERE site_id = current.site_id) - 1, current.db_version)
     ) AS db_version
     FROM crsql_db_versions AS current
 ) WHERE db_version > 0";

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
        "INSERT INTO containers (id, container, machine_id) VALUES (?, ?, ?) ON CONFLICT (id) DO UPDATE SET container = excluded.container, machine_id = excluded.machine_id",
        [
            json!(observation.container_id),
            json!(serde_json::to_string(observation)?),
            json!(observation.machine_id),
        ],
    ))
}

const VOLUME_UPSERT: &str = "INSERT INTO volumes (machine_id, name, volume) VALUES (?, ?, ?) ON CONFLICT (machine_id, name) DO UPDATE SET volume = excluded.volume";

fn volume_upsert(volume: &DockerVolume) -> Result<Statement, Error> {
    Ok(Statement::new(
        VOLUME_UPSERT,
        [
            json!(volume.id.machine_id),
            json!(volume.id.name),
            json!(serde_json::to_string(volume)?),
        ],
    ))
}

fn volume_incomplete(machine_id: &MachineId, name: &DockerVolumeName) -> Statement {
    Statement::new(
        VOLUME_UPSERT,
        [
            json!(machine_id),
            json!(name),
            json!(INCOMPLETE_JSON_DOCUMENT),
        ],
    )
}

const INGRESS_ROUTES: &str = "SELECT machine_id, service_id, containers FROM ingress_routes";
const SERVICE_INGRESS_ROUTES: &str =
    "SELECT machine_id, service_id, containers FROM ingress_routes WHERE service_id = ?";
const INGRESS_ROUTE_UPSERT: &str = "INSERT INTO ingress_routes (machine_id, service_id, containers) VALUES (?, ?, ?) ON CONFLICT (machine_id, service_id) DO UPDATE SET containers = excluded.containers";
const INGRESS_ROUTE_DELETE: &str =
    "DELETE FROM ingress_routes WHERE machine_id = ? AND service_id = ?";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IngressRouteAck {
    pub(crate) containers: BTreeSet<ContainerId>,
}

/// CRDT merge can surface a row before every column has arrived.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum IngressRouteRow {
    Acked(IngressRouteAck),
    UnknownContainers,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum IngressRouteChange {
    Upsert(IngressRouteAck),
    Delete,
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

// One row that won't decode must not hide every other certificate, so it is logged and skipped.
fn certificate_host(hostname: &str) -> Option<CertificateHost> {
    CertificateHost::parse(hostname)
        .inspect_err(|error| tracing::warn!(%error, "skipping certificate row"))
        .ok()
}

fn decode_certificate_row(hostname: &CertificateHost, encoded: &str) -> Option<CertificateRow> {
    CertificateRow::decode(encoded)
        .inspect_err(|error| {
            tracing::warn!(
                %hostname,
                error = %ployz_core::error_chain::inline(error),
                "skipping certificate row"
            );
        })
        .ok()
}

/// The deletes that take a Machine and everything it published out of the Cluster.
fn removal(machine_id: &MachineId) -> [Statement; 4] {
    [
        Statement::new(
            "DELETE FROM volumes WHERE machine_id = ?",
            [json!(machine_id)],
        ),
        Statement::new(
            "DELETE FROM containers WHERE machine_id = ?",
            [json!(machine_id)],
        ),
        Statement::new(
            "DELETE FROM ingress_routes WHERE machine_id = ?",
            [json!(machine_id)],
        ),
        Statement::new("DELETE FROM machines WHERE id = ?", [json!(machine_id)]),
    ]
}
