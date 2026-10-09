//! Departure: when a Machine leaves its cluster or is installed afresh, every root it
//! holds becomes a slot and every lease record closes past its last step, so nothing left
//! behind is taken for the writer and a later restore starts from an ordinary slot.

use std::collections::BTreeSet;

use axum::{Json, extract::State};
use ployz_core::{Cycle, LeaseRecord, MirrorMarker, Pos, RpcError};
use ployzd::machine_pool::MachinePool;

use super::{
    DATASET_ROOT, Dataset, DockerVolumeName, Place, VolumeStorage,
    lease::{
        LEASE_PROPERTY_PREFIX, MIRROR_PROPERTY, WRITER_PROPERTY, internal, slot_fs, slot_parent,
    },
};

/// Prefix of the snapshot a departure takes on each root before demoting it.
pub(super) const DEPARTURE_SNAPSHOT_PREFIX: &str = "dep-";

enum Departure<'datasets> {
    Root(&'datasets Dataset),
    RootBesideEmptySlot(&'datasets Dataset),
    RootBesideMirror,
    UnsealedSlot(&'datasets Dataset),
    Departed,
}

impl VolumeStorage {
    /// Demotes every root to a slot, idles every slot marker and closes every lease
    /// record. Answers the names demoted, for the daemon to forget in Docker.
    async fn demote_all(&self, now_unix_seconds: i64) -> super::Result<Vec<String>> {
        let _guard = self.admit_mutation().await?;
        let Some(pool) = self.pool.one_usable().await? else {
            return Ok(Vec::new());
        };
        let _pool_guard = self.pool.lock_mutation().await?;
        let datasets = self.datasets(&pool).await?;
        let names: BTreeSet<&str> = datasets
            .iter()
            .filter_map(|dataset| match Place::of(&dataset.name, pool.name()) {
                Place::Root(name) | Place::Slot(name) => Some(name),
                Place::Outside => None,
            })
            .collect();
        let mut demoted = Vec::new();
        for name in names {
            let name = name.parse::<DockerVolumeName>()?;
            match self.departure(&pool, &datasets, &name).await? {
                Departure::Root(root) => {
                    self.freeze_root(root, &name, now_unix_seconds).await?;
                    self.ensure_mirror_root(&pool, &datasets).await?;
                    self.create_slot_parent(&slot_parent(&pool, &name), root.refquota)
                        .await?;
                    self.move_into_slot(&pool, root, &name).await?;
                    demoted.push(name.to_string());
                }
                Departure::RootBesideEmptySlot(root) => {
                    self.freeze_root(root, &name, now_unix_seconds).await?;
                    self.move_into_slot(&pool, root, &name).await?;
                    demoted.push(name.to_string());
                }
                Departure::RootBesideMirror => {
                    return Err(format!(
                        "Volume {name} has both a writer and a mirror on this Machine; remove one before departing"
                    )
                    .into());
                }
                Departure::UnsealedSlot(fs) => {
                    self.seal(&fs.name).await?;
                    demoted.push(name.to_string());
                }
                Departure::Departed => {}
            }
            if let Some(slot) = Self::slot(&datasets, &pool, &name) {
                self.zfs(&[
                    "set",
                    &format!("{MIRROR_PROPERTY}={}", MirrorMarker::Idle),
                    &slot.name,
                ])
                .await?;
            }
        }
        self.close_lease_records(&pool, &datasets).await?;
        Ok(demoted)
    }

    async fn departure<'datasets>(
        &self,
        pool: &MachinePool,
        datasets: &'datasets [Dataset],
        name: &DockerVolumeName,
    ) -> super::Result<Departure<'datasets>> {
        let root = Self::dataset(datasets, pool, name)?;
        let slot = Self::slot(datasets, pool, name);
        let fs = Self::slot_fs(datasets, pool, name);
        Ok(match (root, slot, fs) {
            (Some(_), _, Some(_)) => Departure::RootBesideMirror,
            (Some(root), Some(_), None) => Departure::RootBesideEmptySlot(root),
            (Some(root), None, None) => Departure::Root(root),
            (None, _, Some(fs))
                if !fs.readonly || self.property(&fs.name, WRITER_PROPERTY).await?.is_some() =>
            {
                Departure::UnsealedSlot(fs)
            }
            (None, _, _) => Departure::Departed,
        })
    }

    async fn freeze_root(
        &self,
        root: &Dataset,
        name: &DockerVolumeName,
        now_unix_seconds: i64,
    ) -> super::Result<()> {
        root.require_provisioned(name)?;
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
        Ok(())
    }

    async fn move_into_slot(
        &self,
        pool: &MachinePool,
        root: &Dataset,
        name: &DockerVolumeName,
    ) -> super::Result<()> {
        let fs = slot_fs(pool, name);
        self.zfs(&["rename", &root.name, &fs]).await?;
        self.seal(&fs).await
    }

    async fn seal(&self, fs: &str) -> super::Result<()> {
        self.zfs(&["set", "readonly=on", fs]).await?;
        self.zfs(&["inherit", WRITER_PROPERTY, fs]).await?;
        Ok(())
    }

    /// Every record on the managed root keeps its lease and closes at [`Pos::DEPARTED`]:
    /// each late request of that run is behind it, and the next run's newer lease adopts.
    async fn close_lease_records(
        &self,
        pool: &MachinePool,
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
            let closed = LeaseRecord {
                pos: Pos::DEPARTED,
                cycle: Cycle::Closed,
                ..record
            };
            self.zfs(&["set", &format!("{property}={closed}"), &root])
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
