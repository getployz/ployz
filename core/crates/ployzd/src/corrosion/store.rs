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
    CERTIFICATE_POLICY_CLUSTER_KEY, CertificateHost, ClusterName, ClusterRoute, ContainerId,
    ContainerObservation, DockerVolume, DockerVolumeId, DockerVolumeName, ForgottenAnswer,
    IngressHost, IssuanceClock, Machine, MachineId, ManagementIdentity, QualifiedService,
    RecoverySeed, SealingPublicKey, forgotten_key,
};
use serde_json::json;

use super::{
    ApiClient, CertificateChallenge, CertificateMaterial, CertificateRow, Error, REFRESH_INTERVAL,
    Statement, Subscription,
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

    /// Store a Register's row, as [`registration_upsert`] decides, and return the row
    /// it sent. The call succeeds whether the row was written, kept, or held back by a
    /// forget, and nothing tells the caller which: the returned row isn't read back, so
    /// a kept row is not the one returned.
    ///
    /// A Register is an explicit add, so it takes back the forget of a row that had no
    /// Management Identity: its `forgotten-id:` tombstone goes in the same transaction,
    /// and a rerun of a failed add keeps its Server (#3342). A row a Register wrote names
    /// no identity, so forgetting it leaves that tombstone. A row a Register kept carries
    /// the mark, which outside tests only a publish of [`Self::publishable_machine`]
    /// writes, and that row names a Management Identity. So forgetting a kept row leaves
    /// `forgotten:<identity>`, which the #2927 fence reads. A tombstone keyed by a
    /// Management Identity stays, so a joined Server that was forgotten is still not
    /// listed again (#3613).
    pub(crate) async fn register(&self, machine: Machine) -> Result<Machine, Error> {
        let row = RegisterRow::new(machine);
        self.store
            .api
            .execute([
                Statement::new(
                    "DELETE FROM cluster WHERE key = 'forgotten-id:' || ?",
                    [json!(row.0.id)],
                ),
                registration_upsert(&row)?,
            ])
            .await?;
        Ok(row.0)
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

    /// Remove a Machine another Machine forgot, leaving a tombstone so no publish of
    /// its own lists it again. A row with a Management Identity leaves one keyed by
    /// that identity, so the forgotten Machine can learn it was forgotten (#2927). A
    /// row without one, such as a Registered row, leaves `forgotten-id:<id>`. That key
    /// is outside the `forgotten:` prefix older daemons' guards read, so an older
    /// Machine lists the id again as before. One transaction, so no reader sees the
    /// row gone and the tombstone absent.
    pub(crate) async fn forget(&self, machine_id: &MachineId) -> Result<(), Error> {
        let identity_tombstone = Statement::new(
            "INSERT INTO cluster (key, value) \
             SELECT 'forgotten:' || json_extract(info, '$.runtime.management_identity'), id \
             FROM machines \
             WHERE id = ? AND json_extract(info, '$.runtime.management_identity') IS NOT NULL \
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [json!(machine_id)],
        );
        let id_tombstone = Statement::new(
            "INSERT INTO cluster (key, value) \
             SELECT 'forgotten-id:' || id, id \
             FROM machines \
             WHERE id = ? AND json_extract(info, '$.runtime.management_identity') IS NULL \
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [json!(machine_id)],
        );
        self.store
            .api
            .execute(
                [identity_tombstone, id_tombstone]
                    .into_iter()
                    .chain(removal(machine_id)),
            )
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

/// Write `machine`'s row, as this Machine's own publish, unless its id is forgotten
/// and not listed.
///
/// A forgotten id comes back only through a Register: a reset Machine rejoins under a
/// new one. So neither the forgotten Machine's still-running publisher (#3418) nor an
/// Entry's Register for a plain add of a joined Server (#3613) may write its row
/// again. A joined row's tombstone is keyed by its identity and names the id in its
/// value; a Registered row's is keyed `forgotten-id:<id>`, which only a Register
/// clears.
///
/// The row gets `"self_published": true`, a key outside [`Machine`]'s fields that
/// readers ignore. In the daemon only this statement writes it, and outside tests only
/// the publisher reaches it, with [`MachinePublicationGuard::publishable_machine`].
/// [`MachinePublicationGuard::mark_leaving`] edits the row in place and keeps it. A
/// Register under the same WireGuard key keeps a row that has it
/// ([`registration_upsert`]), so keep the name out of [`Machine`]: a field by that name
/// would let a Register write it.
fn machine_upsert(machine: &Machine) -> Result<Statement, Error> {
    Ok(Statement::new(
        "INSERT INTO machines (id, info) \
         SELECT ?1, json_set(?2, '$.self_published', json('true')) \
         WHERE EXISTS (SELECT 1 FROM machines WHERE id = ?1) \
         OR (NOT EXISTS (SELECT 1 FROM cluster \
                         WHERE key LIKE 'forgotten:%' AND value = ?1) \
             AND NOT EXISTS (SELECT 1 FROM cluster WHERE key = 'forgotten-id:' || ?1)) \
         ON CONFLICT (id) DO UPDATE SET info = excluded.info",
        [json!(machine.id), json!(serde_json::to_string(machine)?)],
    ))
}

/// A Machine row as a Register writes it. It names no Management Identity, because a
/// Register can't vouch for one: [`RegisterRow::new`] drops any the request carried,
/// and [`MachinePublicationGuard::register`] builds its row only through it. It is only
/// [`Machine`]'s fields, so it never carries the mark [`machine_upsert`] writes.
struct RegisterRow(Machine);

impl RegisterRow {
    fn new(mut machine: Machine) -> Self {
        machine.runtime.management_identity = None;
        Self(machine)
    }
}

/// Write a Register's row unless its id is forgotten and not listed, as
/// [`machine_upsert`] does. An existing row is kept only when this Entry's copy
/// carries the `self_published` mark and names the Register's WireGuard key. Every
/// other row is replaced: a Register's row, a marked row under another key, and a row
/// whose whole body a daemon without the mark wrote, including one that names a
/// Management Identity. So no Register keeps a row on the strength of an identity an
/// older Entry stored from a Register request. The check reads only this Entry's
/// replica: a self-publish that hasn't arrived is still overwritten, until that
/// Machine's publisher writes its row back.
fn registration_upsert(row: &RegisterRow) -> Result<Statement, Error> {
    let RegisterRow(machine) = row;
    Ok(Statement::new(
        "INSERT INTO machines (id, info) SELECT ?1, ?2 \
         WHERE EXISTS (SELECT 1 FROM machines WHERE id = ?1) \
         OR (NOT EXISTS (SELECT 1 FROM cluster \
                         WHERE key LIKE 'forgotten:%' AND value = ?1) \
             AND NOT EXISTS (SELECT 1 FROM cluster WHERE key = 'forgotten-id:' || ?1)) \
         ON CONFLICT (id) DO UPDATE SET info = excluded.info \
         WHERE json_extract(machines.info, '$.self_published') IS NULL \
            OR json_extract(machines.info, '$.public_key') \
               IS NOT json_extract(excluded.info, '$.public_key')",
        [json!(machine.id), json!(serde_json::to_string(machine)?)],
    ))
}

#[cfg(test)]
pub(crate) const SEALING_PUBLIC_KEY_CLUSTER_KEY: &str = "sealing_public_key";
#[cfg(test)]
pub(crate) const SEALING_AWAITS_RECOVERY_CLUSTER_KEY: &str = "sealing_awaits_recovery";

pub(super) const CLUSTER_SEALING_QUERY: &str = "SELECT \
     (SELECT value FROM cluster WHERE key = 'sealing_public_key') AS published, \
     (SELECT value FROM cluster WHERE key = 'sealing_awaits_recovery') AS awaits";

/// The Cluster's Sealing Key as this Machine reads it: one query on its own
/// replica for the published key and the recovery marker, then its own
/// record's [`RecoverySeed`]. A key that replicates after the read is not seen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClusterSealing {
    /// Neither row is on this replica, and this Machine's record holds no
    /// seed.
    Unsealed,
    /// No key is published on this replica. The recovery marker names this
    /// key, or, with no marker, this Machine's seed does.
    AwaitsRecovery(SealingPublicKey),
    /// A key is published on this replica. Nothing clears the recovery marker,
    /// so one may still be present beside it.
    Published {
        /// The key published on this replica.
        key: SealingPublicKey,
        /// The key the recovery marker on this replica names, when it names a
        /// key other than `key`. `None` means one of three things: no marker,
        /// a marker naming `key`, or a marker that is not a Sealing public
        /// key. The published key decides, so a marker that does not decode
        /// beside it is not an error. The seed never fills this.
        superseded: Option<SealingPublicKey>,
    },
}

impl ClusterSealing {
    /// Decides from the two row values one read returned, in precedence
    /// order: a published key, then the recovery marker, then the seed's
    /// expected key.
    ///
    /// - A published key decides [`Self::Published`]. The marker beside it
    ///   only fills `superseded`, and one that does not decode fills `None`
    ///   rather than failing the read.
    /// - With no key published, the marker decides [`Self::AwaitsRecovery`].
    /// - With neither row, the seed's expected key decides `AwaitsRecovery`,
    ///   and with no seed either, [`Self::Unsealed`].
    ///
    /// # Errors
    /// Returns [`Error::Protocol`] when the deciding row value is not a
    /// Sealing public key: the published key, or, with none published, the
    /// marker. A malformed deciding value never reads as absent.
    pub(crate) fn decide(
        published: &serde_json::Value,
        awaits: &serde_json::Value,
        seed: Option<&RecoverySeed>,
    ) -> Result<Self, Error> {
        if let Some(key) = sealing_key_value(published, "Sealing public key")? {
            let superseded = sealing_key_value(awaits, "Sealing public key awaiting recovery")
                .ok()
                .flatten()
                .filter(|marker| *marker != key);
            return Ok(Self::Published { key, superseded });
        }
        if let Some(awaits) = sealing_key_value(awaits, "Sealing public key awaiting recovery")? {
            return Ok(Self::AwaitsRecovery(awaits));
        }
        Ok(seed.map_or(Self::Unsealed, |seed| {
            Self::AwaitsRecovery(seed.expected_public_key)
        }))
    }

    /// `Some(awaited)` only for [`Self::AwaitsRecovery`] naming a key other
    /// than `key`; `None` for [`Self::Unsealed`], for `AwaitsRecovery` naming
    /// `key`, and for every [`Self::Published`].
    ///
    /// `AwaitsRecovery` already covers a seed with no marker on this replica
    /// ([`Self::decide`]). Under `Published` this never bars: inserting a key
    /// there is a no-op (`ON CONFLICT (key) DO NOTHING`), and whether a held
    /// key beside another published key should be refused is not decided
    /// here. Install of a new key states the same rule in its own match, which
    /// also refuses another published key.
    pub(crate) fn awaits_another(self, key: SealingPublicKey) -> Option<SealingPublicKey> {
        match self {
            Self::AwaitsRecovery(awaited) if awaited != key => Some(awaited),
            Self::Unsealed | Self::AwaitsRecovery(_) | Self::Published { .. } => None,
        }
    }
}

fn sealing_key_value(
    value: &serde_json::Value,
    what: &str,
) -> Result<Option<SealingPublicKey>, Error> {
    if value.is_null() {
        return Ok(None);
    }
    SealingPublicKey::try_from(text(value, what)?.to_owned())
        .map(Some)
        .map_err(|error| {
            Error::Protocol(format!(
                "invalid {what}: {error}",
                error = ployz_core::error_chain::inline(&error),
            ))
        })
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

    #[cfg(test)]
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
        self.api.execute([machine_upsert(machine)?]).await?;
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

    /// Name the Cluster. The last write wins, here and across replicas.
    pub async fn publish_cluster_name(&self, name: &ClusterName) -> Result<(), Error> {
        self.api
            .execute([Statement::new(
                "INSERT INTO cluster (key, value) VALUES ('name', ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [json!(name.as_str())],
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

    /// The Cluster's Sealing Key as this Machine reads it, given the
    /// [`RecoverySeed`] its own record holds, decided by
    /// [`ClusterSealing::decide`].
    ///
    /// # Errors
    /// Returns an error when the Corrosion query fails or answers no row, or
    /// the deciding stored value is not a Sealing public key.
    pub(crate) async fn cluster_sealing(
        &self,
        seed: Option<&RecoverySeed>,
    ) -> Result<ClusterSealing, Error> {
        let query = self
            .api
            .query(Statement::new(CLUSTER_SEALING_QUERY, []))
            .await?;
        let rows = query.rows(["published", "awaits"])?;
        let Some([published, awaits]) = rows.first() else {
            return Err(Error::Protocol(
                "the Cluster's Sealing read answered no row".to_owned(),
            ));
        };
        ClusterSealing::decide(published, awaits, seed)
    }

    /// The Cluster's published Sealing public key, or `None` for a Cluster
    /// founded before Sealing Keys existed and not yet backfilled.
    ///
    /// # Errors
    /// Returns an error when the Corrosion query fails, or the stored value
    /// is not a Sealing public key.
    #[cfg(test)]
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
    #[cfg(test)]
    pub async fn sealing_awaits_recovery(&self) -> Result<Option<SealingPublicKey>, Error> {
        self.sealing_cluster_key(
            SEALING_AWAITS_RECOVERY_CLUSTER_KEY,
            "Sealing public key awaiting recovery",
        )
        .await
    }

    #[cfg(test)]
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
        sealing_key_value(value, what)
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

    /// The Cluster Name on this replica, or `None` when the Cluster has none or
    /// its row has not replicated here yet.
    ///
    /// # Errors
    /// Returns an error when the Corrosion query fails, or the stored value is
    /// not a Cluster Name.
    pub async fn cluster_name(&self) -> Result<Option<ClusterName>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT value FROM cluster WHERE key = 'name'",
                [],
            ))
            .await?;
        let rows = query.rows(["value"])?;
        let Some([value]) = rows.first() else {
            return Ok(None);
        };
        ClusterName::parse(text(value, "Cluster Name")?)
            .map(Some)
            .map_err(|error| {
                Error::Protocol(format!(
                    "invalid Cluster Name: {error}",
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

    /// Write material newly issued along `route` over this replica's ACME row for `hostname`,
    /// clearing its challenge and refusal. Keeps a row that already holds exactly that.
    ///
    /// # Errors
    ///
    /// Returns if the row cannot be read or written.
    pub(crate) async fn publish_certificate(
        &self,
        hostname: &IngressHost,
        material: &CertificateMaterial,
        route: ClusterRoute,
    ) -> Result<AcmeWrite, Error> {
        let issued = CertificateRow::issued(material.clone(), route);
        self.write_acme_row(hostname, |latest| {
            if *latest == issued {
                AcmeEdge::Keep
            } else {
                AcmeEdge::Write(issued.clone())
            }
        })
        .await
    }

    /// Hold published material for `hostname`. Once this replica holds it, this daemon's ACME
    /// writers leave the row alone. A write another Machine made before replication delivered
    /// this one, or an older daemon's write, can still replace it.
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

    /// This replica's row for `hostname` as ACME may write it, or `None` when it holds published
    /// material.
    pub(super) async fn acme_row(
        &self,
        hostname: &IngressHost,
    ) -> Result<Option<AcmeRowRead>, Error> {
        let key = CertificateHost::from(hostname.clone());
        let (row, stored) = match self.stored_row(&key).await? {
            Some((row, stored)) => (row, Some(stored)),
            None => (CertificateRow::default(), None),
        };
        Ok(row
            .published()
            .is_none()
            .then_some(AcmeRowRead { key, row, stored }))
    }

    async fn row(&self, hostname: &CertificateHost) -> Result<CertificateRow, Error> {
        Ok(self
            .stored_row(hostname)
            .await?
            .map_or_else(CertificateRow::default, |(row, _)| row))
    }

    /// This replica's row for `hostname`, decoded and as its body text stands; `None` with no row.
    async fn stored_row(
        &self,
        hostname: &CertificateHost,
    ) -> Result<Option<(CertificateRow, String)>, Error> {
        let query = self
            .api
            .query(Statement::new(
                "SELECT body FROM certificates WHERE hostname = ?",
                [json!(hostname.as_str())],
            ))
            .await?;
        let rows = query.rows(["body"])?;
        let Some([encoded]) = rows.first() else {
            return Ok(None);
        };
        let stored = text(encoded, "certificate body")?;
        Ok(Some((CertificateRow::decode(stored)?, stored.to_owned())))
    }

    /// Read this replica's ACME row for `hostname` and write the edge `edge` decides from it, only
    /// over the body that was read. A row that moved is read again, up to
    /// [`ACME_WRITE_ATTEMPTS`] reads in all.
    async fn write_acme_row(
        &self,
        hostname: &IngressHost,
        edge: impl Fn(&CertificateRow) -> AcmeEdge,
    ) -> Result<AcmeWrite, Error> {
        for _ in 0..ACME_WRITE_ATTEMPTS {
            let Some(read) = self.acme_row(hostname).await? else {
                return Ok(AcmeWrite::Kept);
            };
            let next = match edge(&read.row) {
                AcmeEdge::Write(next) => next,
                AcmeEdge::Keep => return Ok(AcmeWrite::Kept),
            };
            match self.compare_and_set(&read, &next).await? {
                AcmeWrite::Moved => {}
                written @ (AcmeWrite::Changed | AcmeWrite::Kept) => return Ok(written),
            }
        }
        Ok(AcmeWrite::Moved)
    }

    /// Write `next` for `read`'s hostname only if this replica still holds the body text `read`
    /// found, or still holds no row if it found none. Answers `Changed` or `Moved`.
    pub(super) async fn compare_and_set(
        &self,
        read: &AcmeRowRead,
        next: &CertificateRow,
    ) -> Result<AcmeWrite, Error> {
        let key = json!(read.key.as_str());
        let body = json!(next.encode()?);
        let statement = match &read.stored {
            None => Statement::new(
                "INSERT INTO certificates (hostname, body) VALUES (?, ?) ON CONFLICT (hostname) DO NOTHING",
                [key, body],
            ),
            // No `NOT published` clause: text that `CertificateRow::decode` read as an ACME row
            // already holds `published` false.
            Some(stored) => Statement::new(
                "UPDATE certificates SET body = ? WHERE hostname = ? AND body = ?",
                [body, key, json!(stored)],
            ),
        };
        acme_written(self.api.execute_counted([statement]).await?)
    }

    /// Write `row` for `hostname` whatever the row holds. Only published material is written so.
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

    /// Set the pending HTTP-01 challenge on this replica's ACME row for `hostname`, keeping its
    /// material and refusal. Keeps a row that already holds that challenge.
    ///
    /// # Errors
    ///
    /// Returns if the row cannot be read or written.
    pub(crate) async fn publish_certificate_challenge(
        &self,
        hostname: &IngressHost,
        challenge: &CertificateChallenge,
    ) -> Result<AcmeWrite, Error> {
        self.write_acme_row(hostname, |latest| {
            if latest.challenge() == Some(challenge) {
                AcmeEdge::Keep
            } else {
                AcmeEdge::Write(latest.clone().with_challenge(challenge.clone()))
            }
        })
        .await
    }

    /// Record why a hostname has no certificate, or why its renewal failed, and when the Cluster may
    /// try again.
    ///
    /// With `tried` `None`, this keeps a row that holds material. With `tried`, it writes only the
    /// failure fields, and answers `Moved` when the row no longer held `tried`, unpublished.
    ///
    /// # Errors
    ///
    /// Returns if the row cannot be read or written.
    pub(crate) async fn record_certificate_failure(
        &self,
        hostname: &IngressHost,
        tried: Option<&CertificateMaterial>,
        last_error: impl Into<String>,
        clock: IssuanceClock,
    ) -> Result<AcmeWrite, Error> {
        let last_error = last_error.into();
        let Some(tried) = tried else {
            return self
                .write_acme_row(hostname, |latest| {
                    if latest.material().is_some() {
                        AcmeEdge::Keep
                    } else {
                        AcmeEdge::Write(latest.clone().with_backoff(last_error.clone(), clock))
                    }
                })
                .await;
        };
        let Some(AcmeRowRead {
            key, row: latest, ..
        }) = self.acme_row(hostname).await?
        else {
            return Ok(AcmeWrite::Kept);
        };
        // A failed renewal keeps serving its material and backs off like a first order. Only the
        // failure fields change, and only over the ACME material that failed, so a renewal, a
        // publication or another Machine's challenge landing after the read survives.
        let failed: serde_json::Value =
            serde_json::from_str(&latest.with_backoff(last_error, clock).encode()?)?;
        let field = |name: &str| failed.get(name).cloned().unwrap_or_default();
        let counts = self
            .api
            .execute_counted([Statement::new(
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
            .await?;
        acme_written(counts)
    }

    /// Record why the Certificate Policy refuses issuance on this replica's ACME row for
    /// `hostname`, keeping its material and challenge and dropping any backoff clock. Keeps a row
    /// that already holds this reason, clock and all.
    ///
    /// # Errors
    ///
    /// Returns if the row cannot be read or written.
    pub(crate) async fn record_certificate_error(
        &self,
        hostname: &IngressHost,
        reason: &str,
    ) -> Result<AcmeWrite, Error> {
        self.write_acme_row(hostname, |latest| {
            if latest.last_error() == Some(reason) {
                AcmeEdge::Keep
            } else {
                AcmeEdge::Write(latest.clone().with_error(reason))
            }
        })
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
            refresh: RuntimeWatchChanges::refresh(),
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
    /// Ticks whatever the tables do, so changes on one table can't put off the
    /// stall check on another.
    refresh: tokio::time::Interval,
}

impl RuntimeWatchChanges {
    fn refresh() -> tokio::time::Interval {
        let mut refresh = tokio::time::interval_at(
            tokio::time::Instant::now() + REFRESH_INTERVAL,
            REFRESH_INTERVAL,
        );
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        refresh
    }

    /// Wakes on a change, and every `REFRESH_INTERVAL` so the reader re-reads
    /// tables whose subscription went quiet.
    async fn changed(&mut self) -> Result<(), Error> {
        tokio::select! {
            result = self.machines.changed() => result,
            result = self.containers.changed() => result,
            result = self.volumes.changed() => result,
            result = self.certificates.changed() => result,
            result = self.cluster.changed() => result,
            _ = self.refresh.tick() => {
                if self.snapshot_in_progress() {
                    Err(Error::Protocol("subscription snapshot stalled".into()))
                } else {
                    Ok(())
                }
            }
        }
    }

    fn snapshot_in_progress(&self) -> bool {
        [
            &self.machines,
            &self.containers,
            &self.volumes,
            &self.certificates,
            &self.cluster,
        ]
        .into_iter()
        .any(Subscription::snapshot_in_progress)
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

/// How many times an ACME write reads its row before it answers [`AcmeWrite::Moved`]. Only
/// repeated misses use it up: a re-read can still answer `Kept` or `Changed`. A cap, not a
/// guarantee: each miss means a write landed on this replica between one read and its write, and
/// the certificate loop's next pass reads again.
pub(super) const ACME_WRITE_ATTEMPTS: usize = 3;

/// What an ACME write to a certificate row did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AcmeWrite {
    /// The write landed.
    Changed,
    /// No write was sent, because the row is Published or the writer's no-write rule held.
    Kept,
    /// The row no longer met the write's condition when it ran, so nothing was written.
    Moved,
}

/// One read of this replica's row for an Ingress Hostname, taken only when the row holds no
/// published material. An ACME write lands only over the body text it holds.
pub(super) struct AcmeRowRead {
    key: CertificateHost,
    row: CertificateRow,
    /// The body text as this replica stored it; `None` when there was no row.
    stored: Option<String>,
}

/// What an ACME writer does with the row it read, as `RotationEdge` does for a rotation.
enum AcmeEdge {
    Write(CertificateRow),
    /// Allowed with nothing to write: the writer's no-write rule holds for the row.
    Keep,
}

/// `hostname` is the primary key, so one certificate write changes one row or none.
fn acme_written(counts: Vec<usize>) -> Result<AcmeWrite, Error> {
    match counts.as_slice() {
        [1] => Ok(AcmeWrite::Changed),
        [0] => Ok(AcmeWrite::Moved),
        counts => Err(Error::Protocol(format!(
            "a certificate write changed {counts:?} rows"
        ))),
    }
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

#[cfg(test)]
mod runtime_watch_changes_tests {
    use std::{io, time::Duration};

    use bytes::Bytes;
    use futures_util::{StreamExt, stream};

    use super::{REFRESH_INTERVAL, RuntimeWatchChanges, Subscription};

    fn quiet() -> Subscription {
        Subscription::streaming(stream::pending::<Result<Bytes, io::Error>>())
    }

    fn changes(certificates: Subscription) -> RuntimeWatchChanges {
        RuntimeWatchChanges {
            machines: quiet(),
            containers: quiet(),
            volumes: quiet(),
            certificates,
            cluster: quiet(),
            refresh: RuntimeWatchChanges::refresh(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn quiet_subscriptions_wake_the_reader_on_the_refresh_interval() {
        let mut changes = changes(quiet());
        let started = tokio::time::Instant::now();

        changes
            .changed()
            .await
            .expect("a quiet table is not a failure");
        assert_eq!(started.elapsed(), REFRESH_INTERVAL);
    }

    #[tokio::test(start_paused = true)]
    async fn a_snapshot_still_in_progress_at_the_refresh_ends_the_watch() {
        let stalled = Subscription::streaming(
            stream::iter([Ok::<_, io::Error>(Bytes::from_static(
                b"{\"columns\":[\"hostname\",\"body\"]}\n",
            ))])
            .chain(stream::pending()),
        );
        let mut changes = changes(stalled);

        let error =
            tokio::time::timeout(REFRESH_INTERVAL + Duration::from_secs(1), changes.changed())
                .await
                .expect("the refresh bounds a stalled snapshot")
                .expect_err("a stalled snapshot must end the watch");
        assert!(
            error.to_string().contains("subscription snapshot stalled"),
            "{error}"
        );
    }
    #[tokio::test(start_paused = true)]
    async fn changes_on_another_table_do_not_put_off_the_stall_check() {
        let stalled = Subscription::streaming(
            stream::iter([Ok::<_, io::Error>(Bytes::from_static(
                b"{\"columns\":[\"hostname\",\"body\"]}\n",
            ))])
            .chain(stream::pending()),
        );
        let mut changes = changes(stalled);
        let (machine_changes, received) = tokio::sync::mpsc::unbounded_channel();
        changes.machines = Subscription::streaming(
            tokio_stream::wrappers::UnboundedReceiverStream::new(received),
        );
        let writer = tokio::spawn(async move {
            loop {
                tokio::time::sleep(REFRESH_INTERVAL / 3).await;
                if machine_changes
                    .send(Ok(Bytes::from_static(b"{\"change\":null}\n")))
                    .is_err()
                {
                    return;
                }
            }
        });

        let error = tokio::time::timeout(REFRESH_INTERVAL + Duration::from_secs(1), async {
            loop {
                if let Err(error) = changes.changed().await {
                    return error;
                }
            }
        })
        .await
        .expect("the stall is caught within one refresh interval");
        assert!(
            error.to_string().contains("subscription snapshot stalled"),
            "{error}"
        );
        writer.abort();
    }
}
