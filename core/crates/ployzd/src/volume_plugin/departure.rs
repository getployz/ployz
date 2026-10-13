//! Departure: when a Machine leaves its cluster or is installed afresh, every root it
//! holds becomes a slot, so nothing left behind is taken for the writer and a later restore
//! starts from an ordinary slot.

use std::collections::BTreeSet;

use axum::{Json, extract::State};
use ployz_core::RpcError;
use ployzd::machine_pool::MachinePool;

use super::{
    Dataset, DockerVolumeName, Place, VolumeStorage,
    inspect::internal,
    storage::{slot_fs, slot_parent},
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
    /// Demotes every root to a slot. Answers the names demoted, for the daemon to forget in Docker.
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
            if self
                .demote_copy(&pool, &datasets, &name, now_unix_seconds)
                .await?
            {
                demoted.push(name.to_string());
            }
        }
        Ok(demoted)
    }

    /// Turns `name`'s root into a sealed slot. Answers whether it demoted anything.
    async fn demote_copy(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        name: &DockerVolumeName,
        now_unix_seconds: i64,
    ) -> super::Result<bool> {
        let demoted = match self.departure(pool, datasets, name).await? {
            Departure::Root(root) => {
                self.freeze_root(root, name, now_unix_seconds).await?;
                self.ensure_mirror_root(pool, datasets).await?;
                self.create_slot_parent(&slot_parent(pool, name), root.refquota)
                    .await?;
                self.move_into_slot(pool, root, name).await?;
                true
            }
            Departure::RootBesideEmptySlot(root) => {
                self.freeze_root(root, name, now_unix_seconds).await?;
                self.move_into_slot(pool, root, name).await?;
                true
            }
            Departure::RootBesideMirror => {
                return Err(format!(
                    "Volume {name} has both a writer and a mirror on this Machine; remove one before departing"
                )
                .into());
            }
            Departure::UnsealedSlot(fs) => {
                self.seal(&fs.name).await?;
                true
            }
            Departure::Departed => false,
        };
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
            (None, _, Some(fs)) if !fs.readonly => Departure::UnsealedSlot(fs),
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
        Ok(())
    }
}

pub(super) async fn demote(
    State(storage): State<VolumeStorage>,
) -> Json<Result<Vec<String>, RpcError>> {
    let now = chrono::Utc::now().timestamp();
    Json(storage.demote_all(now).await.map_err(internal))
}
