//! Departure: when a Machine leaves its cluster or is installed afresh, every root it
//! holds becomes a slot and every lease record moves on. Nothing left behind can then
//! be mistaken for the writer, and a later restore starts from a copy the run model
//! already understands.

use axum::{Json, extract::State};
use ployz_core::{Cycle, Lease, LeaseRecord, MirrorMarker, Pos, RpcError};

use super::{
    DATASET_ROOT, Dataset, DockerVolumeName, Place, VolumeStorage,
    lease::{LEASE_PROPERTY_PREFIX, MIRROR_PROPERTY, WRITER_PROPERTY, internal, slot_parent},
};

/// Prefix of the snapshot a departure takes on each root before demoting it.
pub(super) const DEPARTURE_SNAPSHOT_PREFIX: &str = "dep-";

impl VolumeStorage {
    /// Demotes every root to a slot, idles every slot marker and bumps every lease
    /// record. Answers the names demoted, for the daemon to forget in Docker.
    async fn demote_all(&self, now_unix_seconds: i64) -> super::Result<Vec<String>> {
        let _guard = self.admit_mutation().await?;
        let Some(pool) = self.pool.one_usable().await? else {
            return Ok(Vec::new());
        };
        let _pool_guard = self.pool.lock_mutation().await?;
        let datasets = self.datasets(&pool).await?;
        let mut demoted = Vec::new();
        for dataset in &datasets {
            match Place::of(&dataset.name, pool.name()) {
                Place::Root(name) => {
                    let name = name.parse::<DockerVolumeName>()?;
                    let root =
                        Self::dataset(&datasets, &pool, &name)?.expect("the root was just listed");
                    root.require_provisioned(&name)?;
                    self.demote_root(&pool, &datasets, root, &name, now_unix_seconds)
                        .await?;
                    demoted.push(name.to_string());
                }
                Place::Slot(name) if dataset.name.ends_with(&format!("/{name}")) => {
                    self.zfs(&[
                        "set",
                        &format!("{MIRROR_PROPERTY}={}", MirrorMarker::Idle),
                        &dataset.name,
                    ])
                    .await?;
                }
                Place::Slot(_) | Place::Outside => {}
            }
        }
        self.bump_lease_records(&pool, &datasets).await?;
        Ok(demoted)
    }

    async fn demote_root(
        &self,
        pool: &ployzd::machine_pool::MachinePool,
        datasets: &[Dataset],
        root: &Dataset,
        name: &DockerVolumeName,
        now_unix_seconds: i64,
    ) -> super::Result<()> {
        let parent = slot_parent(pool, name);
        if datasets.iter().any(|dataset| dataset.name == parent) {
            return Err(format!(
                "Volume {name} has both a writer and a mirror on this Machine; remove one before departing"
            )
            .into());
        }
        if root.mounted {
            self.zfs(&["unmount", &root.name]).await?;
        }
        for snapshot in self.snapshot_names(&root.name).await? {
            if snapshot
                .split_once('@')
                .is_some_and(|(_, name)| name.starts_with(DEPARTURE_SNAPSHOT_PREFIX))
            {
                self.zfs(&["destroy", &snapshot]).await?;
            }
        }
        self.zfs(&[
            "snapshot",
            &format!(
                "{}@{DEPARTURE_SNAPSHOT_PREFIX}{now_unix_seconds}",
                root.name
            ),
        ])
        .await?;
        self.ensure_mirror_root(pool, datasets).await?;
        self.create_slot_parent(&parent, root.refquota).await?;
        let fs = format!("{parent}/fs");
        self.zfs(&["rename", &root.name, &fs]).await?;
        self.zfs(&["set", "readonly=on", &fs]).await?;
        self.zfs(&["inherit", WRITER_PROPERTY, &fs]).await?;
        Ok(())
    }

    /// Every record on the managed root moves to the next lease, closed, at the
    /// position a new run's `02-lease` would write.
    async fn bump_lease_records(
        &self,
        pool: &ployzd::machine_pool::MachinePool,
        datasets: &[Dataset],
    ) -> super::Result<()> {
        let root = format!("{}/{DATASET_ROOT}", pool.name());
        if !datasets.iter().any(|dataset| dataset.name == root) {
            return Ok(());
        }
        let listed = self
            .zfs(&[
                "get",
                "-H",
                "-o",
                "property,value",
                "-s",
                "local",
                "all",
                &root,
            ])
            .await?;
        for line in listed.lines() {
            let Some((property, value)) = line.split_once('\t') else {
                continue;
            };
            if !property.starts_with(LEASE_PROPERTY_PREFIX) {
                continue;
            }
            let record = value
                .parse::<LeaseRecord>()
                .map_err(|error| format!("{error} on {root}"))?;
            let bumped = LeaseRecord {
                lease: Lease::new(record.lease.get() + 1),
                pos: Pos::ADOPT_LEASE,
                cycle: Cycle::Closed,
            };
            self.zfs(&["set", &format!("{property}={bumped}"), &root])
                .await?;
        }
        Ok(())
    }
}

pub(super) async fn demote(
    State(storage): State<VolumeStorage>,
) -> Json<Result<Vec<String>, RpcError>> {
    let now = chrono::Utc::now().timestamp();
    Json(storage.demote_all(now).await.map_err(internal))
}
