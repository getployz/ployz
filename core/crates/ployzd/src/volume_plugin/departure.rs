//! Departure: when a Machine leaves its cluster or is installed afresh, every root it
//! holds becomes a sealed slot, so nothing left behind is taken for the writer and a later
//! restore starts from an ordinary slot. Every receive stops before the slots are read, so
//! each slot is judged as it will stay. A Volume whose receive will not stop is left as it
//! is and named in the answer; the daemon fails the departure on it once Docker has
//! forgotten the rest, and a retry judges it after its receive has stopped.
//!
//! A slot is kept when it may hold the only copy of its point. A departure snapshot, a
//! completed seed or a promote that has not renamed keeps it whatever else it holds: the
//! seed's source root may since have pruned that point. A Move's cut or a restore's epoch keeps it only while no pull has
//! landed over the point it marks, which the slot's parent records by naming the pulled
//! snapshot. Only this Machine sets these marks and a send carries none, so every other
//! slot is a received copy, a mirror or a partial seed, and is destroyed: the writer or
//! the seed's source root keeps that data.

use std::collections::BTreeSet;

use axum::{Json, extract::State};
use ployz_core::{RpcError, StorageDeparture};
use ployzd::machine_pool::MachinePool;

use super::{
    Dataset, DockerVolumeName, MIRROR_ROOT, Place, VolumeStorage,
    inspect::internal,
    seed::SEED_PROPERTY,
    storage::{
        CUT_PROPERTY, EPOCH_PROPERTY, PROMOTE_PROPERTY, PULLED_PROPERTY, slot_fs, slot_parent,
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
    Mirror,
}

impl VolumeStorage {
    /// Demotes every root to a slot and destroys every received mirror, except where a copy
    /// would not stop. Answers the names it changed, for the daemon to forget in Docker, and
    /// the names it left.
    async fn demote_all(&self, now_unix_seconds: i64) -> super::Result<StorageDeparture> {
        let _guard = self.admit_mutation().await?;
        let Some(pool) = self.pool.one_usable().await? else {
            return Ok(StorageDeparture::default());
        };
        let _pool_guard = self.pool.lock_mutation().await?;
        // Every receive starts under admission, so once these stop no slot changes
        // between the scan and the decisions taken from it. A stuck receive may still
        // write, so its Volume is not judged.
        let stuck = self.receives.stop_all(self.stop_deadline).await;
        if !stuck.is_empty() {
            tracing::warn!(stuck = %stuck.join(", "), deadline = ?self.stop_deadline, "copies did not stop; leaving their Volumes");
        }
        let datasets = self.datasets(&pool).await?;
        let names: BTreeSet<&str> = datasets
            .iter()
            .filter_map(|dataset| match Place::of(&dataset.name, pool.name()) {
                Place::Root(name) | Place::Slot(name) => Some(name),
                Place::Outside => None,
            })
            .filter(|name| !stuck.iter().any(|stuck| stuck == name))
            .collect();
        let mut demoted = Vec::new();
        for name in names {
            let name = name.parse::<DockerVolumeName>()?;
            if self
                .demote_copy(&pool, &datasets, &name, now_unix_seconds)
                .await?
            {
                demoted.push(departed_name(&name.to_string())?);
            }
        }
        self.drop_empty_mirror_root(&pool).await;
        Ok(StorageDeparture {
            demoted,
            stuck: stuck
                .iter()
                .map(|name| departed_name(name))
                .collect::<super::Result<_>>()?,
        })
    }

    /// Destroys `<pool>/ployz-mirror` once no slot is left in it, so a departed Machine
    /// holds no mirror dataset at all. [`Self::ensure_mirror_root`] makes it again for the
    /// next slot, and a slot made meanwhile keeps it. The root holds no data, so a
    /// failure here only leaves it behind; the departure's answer stands.
    async fn drop_empty_mirror_root(&self, pool: &MachinePool) {
        let mirror_root = format!("{}/{MIRROR_ROOT}", pool.name());
        let children = format!("{mirror_root}/");
        let datasets = match self.datasets(pool).await {
            Ok(datasets) => datasets,
            Err(error) => {
                tracing::warn!(%error, "could not list the mirror root to drop it");
                return;
            }
        };
        if !datasets.iter().any(|dataset| dataset.name == mirror_root)
            || datasets
                .iter()
                .any(|dataset| dataset.name.starts_with(&children))
        {
            return;
        }
        if let Err(error) = self.zfs(&["destroy", &mirror_root]).await {
            tracing::warn!(%error, %mirror_root, "left the empty mirror root in place");
        }
    }

    /// Turns `name`'s root into a sealed slot, or destroys `name`'s received mirror. Answers
    /// whether it changed anything.
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
            Departure::Mirror => {
                self.zfs(&["destroy", "-r", &slot_parent(pool, name)])
                    .await?;
                self.receives.forget(name);
                true
            }
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
        let Some(root) = root else {
            return Ok(match fs {
                Some(fs) if self.written_here(&fs.name).await? => {
                    if fs.readonly {
                        Departure::Departed
                    } else {
                        Departure::UnsealedSlot(fs)
                    }
                }
                _ => Departure::Mirror,
            });
        };
        Ok(match (slot, fs) {
            (_, Some(_)) => Departure::RootBesideMirror,
            (Some(_), None) => Departure::RootBesideEmptySlot(root),
            (None, None) => Departure::Root(root),
        })
    }

    /// Whether the slot's copy may be the only one. Only a mirror pull marks a slot
    /// pulled, once its receive made a snapshot a writer elsewhere had made, and that is
    /// the only evidence the departure destroys on: every slot without it is kept, so a
    /// state no release before the mark could tell apart leaks rather than loses data. A
    /// departure snapshot or a seed, promote, cut or epoch mark keeps a slot even so, as
    /// each describes a point this Machine may alone hold. A slot with no snapshot at all
    /// holds only a receive that never completed, which every receive makes resumable with
    /// `-s`, so it holds no point at all.
    async fn written_here(&self, fs: &str) -> super::Result<bool> {
        if self
            .snapshot_names(fs)
            .await?
            .iter()
            .filter_map(|snapshot| snapshot.split_once('@'))
            .any(|(_, point)| point.starts_with(DEPARTURE_SNAPSHOT_PREFIX))
        {
            return Ok(true);
        }
        for mark in [
            SEED_PROPERTY,
            PROMOTE_PROPERTY,
            CUT_PROPERTY,
            EPOCH_PROPERTY,
        ] {
            if self.property(fs, mark).await?.is_some() {
                return Ok(true);
            }
        }
        let Some(newest) = self.newest_guid(fs).await? else {
            return Ok(false);
        };
        Ok(self.local_property(fs, PULLED_PROPERTY).await? != Some(newest))
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
) -> Json<Result<StorageDeparture, RpcError>> {
    let now = chrono::Utc::now().timestamp();
    Json(storage.demote_all(now).await.map_err(internal))
}

fn departed_name(name: &str) -> super::Result<ployz_core::DockerVolumeName> {
    name.parse()
        .map_err(|error| format!("invalid Volume name {name:?}: {error}").into())
}
