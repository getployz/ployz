//! The Trash: every copy of a Volume this Machine deletes moves to `<pool>/ployz-trash/<id>`
//! and waits there for 168 hourly sweeps, the hours the Server ran in 7 days.
//!
//! A delete ([`VolumeStorage::discard`]) is a rename, so it frees no space and never empties
//! the Trash to make room. A rename still writes a few blocks, which ZFS refuses on a Pool
//! with none left, though it lets a destroy through; so the Trash parent holds back
//! [`TRASH_SLACK`], which a rename that ran out of space gives up and the sweep takes back.
//! In order:
//!
//! 1. The trash marks go on the live copy, set locally: its Docker Volume name, its place,
//!    its own `readonly` and `canmount` (`ployz:trash-was`), and the sweep hour. Nothing on a
//!    live copy reads them, so a stop here leaves a working Volume, and the next delete
//!    keeps the `trash-was` of the first attempt.
//! 2. The rename into the Trash. It unmounts a mounted copy, since the Trash parent mounts
//!    nowhere, and refuses one in use. Nothing touched the live copy's mounts before it.
//! 3. `canmount=off` on the entry, which the sweep sets on any entry missing it.
//! 4. The tombstone of each Copy step the entry carries ([`VolumeStorage::bury`]), only
//!    once the rename committed, so an attempt that stopped before it revokes no step.
//!
//! The sweep advances the Trash's hour counter by at most one per run, and only once an
//! hour of wall time passed since its last advance, so a Server that was off, or a clock
//! that jumped, adds at most one hour. It destroys an entry only when every mark reads and
//! its hour is 168 behind the counter.

use std::fmt;

use axum::{Json, extract::State};
use chrono::DateTime;
use ployz_core::{
    EmptyTrashRequest, RestoreTrashRequest, RpcError, SlotRole, TRASH_SWEEP_HOURS, TrashEmptied,
    TrashEntry, TrashFailure, TrashId, TrashList, TrashPlace, TrashRestored, UnreadableTrash,
};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::{Deserialize, Serialize};

use super::{
    Dataset, PluginError, Result, VolumeName, VolumeStorage,
    holders::{Unheld, copy_held_for_other_data, copy_unlisted},
    marks::{Mark, Write},
    origin::Origin,
    storage::{Commitment, Holds},
    transfer::Stop,
};
use crate::{
    dataset_layout::{DatasetLayout, Place},
    machine_pool::MachinePool,
};

const HOUR_SECONDS: i64 = 3600;

/// The `refreservation` the Trash parent keeps for a rename on a full Pool.
const TRASH_SLACK: &str = "1M";

/// The marks the move writes, which a restore takes off again.
const ENTRY_MARKS: [Mark; 5] = [
    Mark::TrashName,
    Mark::TrashPlace,
    Mark::TrashWas,
    Mark::TrashedHour,
    Mark::TrashedAt,
];

/// What a copy set on itself before the move: its own `readonly` and `canmount`, each
/// `None` when it inherited the value.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Was {
    readonly: Option<String>,
    canmount: Option<String>,
}

impl Was {
    async fn read(storage: &VolumeStorage, dataset: &str) -> Result<Self> {
        Ok(Self {
            readonly: storage.local_property(dataset, "readonly").await?,
            canmount: storage.local_property(dataset, "canmount").await?,
        })
    }

    fn parse(value: &str) -> Option<Self> {
        let (readonly, canmount) = value.split_once(',')?;
        let field = |pair: &str, key: &str| {
            let value = pair.strip_prefix(key)?.strip_prefix('=')?;
            Some((value != "-").then(|| value.to_owned()))
        };
        Some(Self {
            readonly: field(readonly, "readonly")?,
            canmount: field(canmount, "canmount")?,
        })
    }

    /// The writes that put each value back on a restored copy.
    fn writes(&self) -> [Vec<String>; 2] {
        [("readonly", &self.readonly), ("canmount", &self.canmount)].map(|(property, value)| {
            match value {
                Some(value) => vec!["set".to_owned(), format!("{property}={value}")],
                // canmount isn't inherited: its default is on.
                None if property == "canmount" => vec!["set".to_owned(), "canmount=on".to_owned()],
                None => vec!["inherit".to_owned(), property.to_owned()],
            }
        })
    }
}

impl fmt::Display for Was {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shown = |value: &Option<String>| value.clone().unwrap_or_else(|| "-".to_owned());
        write!(
            formatter,
            "readonly={},canmount={}",
            shown(&self.readonly),
            shown(&self.canmount)
        )
    }
}

/// One entry whose every mark reads.
struct Entry<'datasets> {
    id: TrashId,
    dataset: &'datasets Dataset,
    name: VolumeName,
    place: TrashPlace,
    was: Was,
    hour: u64,
    at: i64,
}

impl Entry<'_> {
    /// Sweep hours left before the sweep destroys it, at `hours`.
    fn left(&self, hours: u64) -> u64 {
        (self.hour + TRASH_SWEEP_HOURS).saturating_sub(hours)
    }

    fn expires_at(&self, hours: u64, now: i64) -> i64 {
        now.saturating_add(i64::try_from(self.left(hours)).unwrap_or(i64::MAX) * HOUR_SECONDS)
    }
}

/// What the Trash holds when a create, grow or pull finds no room: the sentence a
/// capacity refusal ends with.
#[derive(Clone, Copy, Debug)]
pub(super) struct TrashHeld {
    bytes: u64,
    until: i64,
}

impl fmt::Display for TrashHeld {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let until = DateTime::from_timestamp(self.until, 0).map_or_else(
            || self.until.to_string(),
            |at| at.format("%Y-%m-%d").to_string(),
        );
        let held = if self.bytes < 100_000_000 {
            format!("{} MB", self.bytes.div_ceil(1_000_000))
        } else {
            format!("{:.1} GB", self.bytes as f64 / 1e9)
        };
        write!(
            formatter,
            "{held} is in this Server's Trash until {until}. Run `sudo ployz volume trash empty` on this Server to free it."
        )
    }
}

/// What one sweep did.
#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct TrashSwept {
    pub(crate) hours: u64,
    pub(crate) advanced: bool,
    pub(crate) destroyed: Vec<TrashId>,
    pub(crate) failed: Vec<TrashFailure>,
}

impl VolumeStorage {
    fn now_seconds(&self) -> i64 {
        i64::try_from((self.now)().as_millis() / 1000).unwrap_or(i64::MAX)
    }

    /// `<pool>/ployz-trash`, made when `datasets` lack it.
    pub(super) async fn ensure_trash_parent(
        &self,
        pool: &str,
        datasets: &[Dataset],
    ) -> Result<String> {
        let parent = DatasetLayout::new(pool).trash_parent();
        if !datasets.iter().any(|dataset| dataset.name == parent) {
            self.zfs(&[
                "create",
                "-o",
                "mountpoint=none",
                "-o",
                "canmount=off",
                &parent,
            ])
            .await?;
            self.keep_slack(&parent).await;
        }
        Ok(parent)
    }

    /// Makes the Trash of a Pool created now, while it has room for the parent and its
    /// slack. A failure only leaves that to the first delete or sweep.
    pub(super) async fn trash_new_pool(&self, pool: &MachinePool) {
        if let Err(error) = self.ensure_trash_parent(pool.name(), &[]).await {
            tracing::warn!(%error, pool = pool.name(), "could not make the new Pool's Trash yet");
        }
    }

    /// Reserves [`TRASH_SLACK`] on the Trash parent when it holds none. A Pool too full for
    /// it keeps none until a later sweep finds room.
    async fn keep_slack(&self, parent: &str) {
        let slack = format!("refreservation={TRASH_SLACK}");
        match self.local_property(parent, "refreservation").await {
            Ok(Some(value)) if value == TRASH_SLACK => {}
            Ok(_) => {
                if let Err(error) = self.zfs(&["set", &slack, parent]).await {
                    tracing::debug!(%error, parent, "the Trash keeps no slack for now");
                }
            }
            Err(error) => tracing::debug!(%error, parent, "could not read the Trash's slack"),
        }
    }

    /// Renames `dataset` to `entry` under `parent`, giving up the parent's slack once if
    /// the Pool has no space left for the rename.
    async fn rename_into(&self, parent: &str, dataset: &str, entry: &str) -> Result<()> {
        match self.zfs(&["rename", dataset, entry]).await {
            Err(error) if error.to_string().contains("out of space") => {
                self.zfs(&["set", "refreservation=none", parent]).await?;
                self.zfs(&["rename", dataset, entry]).await.map(drop)
            }
            result => result.map(drop),
        }
    }

    /// The Trash's sweep hours; a parent no sweep advanced reads 0.
    async fn sweep_hours(&self, parent: &str) -> Result<u64> {
        let Some(hours) = self.marks(parent).local(Mark::TrashHours).await? else {
            return Ok(0);
        };
        hours.parse().map_err(|_| {
            PluginError::from(format!(
                "{parent} carries an unreadable {} {hours:?}",
                Mark::TrashHours
            ))
        })
    }

    /// Moves `dataset`, the copy of `name` at `place`, into the Trash; see the module doc.
    pub(super) async fn discard(
        &self,
        dataset: &str,
        place: TrashPlace,
        name: &VolumeName,
    ) -> Result<TrashId> {
        let pool = dataset.split('/').next().unwrap_or(dataset);
        let parent = DatasetLayout::new(pool).trash_parent();
        if !self.exists(&parent).await? {
            self.ensure_trash_parent(pool, &[]).await?;
        }
        let hour = self.sweep_hours(&parent).await?;
        let marks = self.marks(dataset);
        let earlier = (
            marks.local(Mark::TrashWas).await?,
            marks.local(Mark::TrashName).await?,
            marks.local(Mark::TrashPlace).await?,
        );
        let was = match earlier {
            (Some(was), Some(earlier_name), Some(earlier_place))
                if earlier_name == name.as_str() && earlier_place == place.as_str() =>
            {
                was
            }
            _ => Was::read(self, dataset).await?.to_string(),
        };
        marks
            .apply(&[
                Write::Set(Mark::TrashName, name.to_string()),
                Write::Set(Mark::TrashPlace, place.as_str().to_owned()),
                Write::Set(Mark::TrashWas, was),
                Write::Set(Mark::TrashedHour, hour.to_string()),
                Write::Set(Mark::TrashedAt, self.now_seconds().to_string()),
            ])
            .await?;
        crate::faults::kill_inside(crate::faults::Point::Verb("Trash.Marked"), None);
        let mut bytes = [0; 16];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| "could not draw a Trash ID from the system's random source")?;
        let id = TrashId::from_bytes(bytes);
        let entry = DatasetLayout::new(pool).trash_entry(&id);
        self.rename_into(&parent, dataset, &entry).await?;
        crate::faults::kill_inside(crate::faults::Point::Verb("Trash.Renamed"), None);
        self.settle_entry(&entry).await?;
        self.bury(&entry, true).await?;
        Ok(id)
    }

    /// Leaves the tombstone of every Copy step an entry in the Trash carries. A move that
    /// stopped between its rename and its tombstone leaves the step unburied until the
    /// next sweep, so a seed reads this first: a step whose Volume is in the Trash was
    /// deleted.
    pub(super) async fn bury_trash(&self, pool: &MachinePool, datasets: &[Dataset]) -> Result<()> {
        let parent = DatasetLayout::of(pool).trash_parent();
        if datasets.iter().any(|dataset| dataset.name == parent) {
            self.bury(&parent, true).await?;
        }
        Ok(())
    }

    /// Keeps the entry from mounting anywhere: `canmount=off`, and an unmount should a
    /// mountpoint it set itself have mounted it again after the rename.
    async fn settle_entry(&self, entry: &str) -> Result<()> {
        if self.local_property(entry, "canmount").await?.as_deref() != Some("off") {
            self.zfs(&["set", "canmount=off", entry]).await?;
        }
        if self.property(entry, "mounted").await?.as_deref() == Some("yes") {
            self.zfs(&["unmount", entry]).await?;
        }
        Ok(())
    }

    /// Deletes `name`'s slot: into the Trash when its copy holds a point, outright when
    /// the slot holds no copy or only a receive that never completed one.
    pub(super) async fn discard_slot(&self, parent: &str, name: &VolumeName) -> Result<()> {
        let fs = format!("{parent}/fs");
        if !self.exists(&fs).await? {
            return self.destroy(parent, Holds::Nothing).await;
        }
        if self.newest_guid(&fs).await?.is_none() {
            return self.destroy(parent, Holds::Unreceived).await;
        }
        self.discard(parent, TrashPlace::Slot, name).await.map(drop)
    }

    /// Takes the copy `fs` out of the slot `parent` so a full receive can start over in
    /// it, with its verified origin stamped on it first. The Pool must hold its bytes
    /// beside the slot's bound; otherwise the copy stays and this refuses with what the
    /// Trash holds. A copy that never completed a point goes outright.
    pub(super) async fn discard_copy(
        &self,
        parent: &str,
        fs: &str,
        name: &VolumeName,
        stop: Option<&Stop>,
    ) -> Result<()> {
        if self.newest_guid(fs).await?.is_none() {
            return self.destroy(fs, Holds::Unreceived).await;
        }
        let newest = self.newest_snapshot(fs).await?;
        let origin = self
            .origin_of(
                Some(parent),
                Some((fs, newest.as_ref().map(|newest| &newest.name))),
            )
            .await?;
        // A receive task waits for the Pool lock only until a departure stops it, since the
        // departure holds that lock while it stops receives.
        let _pool_guard = match stop {
            Some(stop) => tokio::select! {
                guard = self.pool.lock_mutation() => guard?,
                () = stop.cancelled() => {
                    return Err("stopped before the old copy moved to the Trash".into());
                }
            },
            None => self.pool.lock_mutation().await?,
        };
        let pool = self.one_pool().await?;
        let datasets = self.datasets(&pool).await?;
        let used = datasets
            .iter()
            .find(|dataset| dataset.name == fs)
            .map_or(0, |dataset| dataset.used_bytes);
        let commitment = Commitment::of(&pool, &datasets, 0)?;
        self.ensure_pool_capacity(
            &pool,
            commitment.bytes,
            commitment.unmanaged_used_bytes.saturating_add(used),
        )
        .await?;
        if let Some(origin) = origin
            && self.local_origin(fs, Mark::Origin).await? != Some(origin)
        {
            self.set_verified(fs, Mark::Origin.name(), &origin.to_string())
                .await?;
        }
        self.discard(fs, TrashPlace::Copy, name).await.map(drop)
    }

    /// Every entry under the Trash parent of `pool`, with its marks read.
    async fn entries<'datasets>(
        &self,
        pool: &MachinePool,
        datasets: &'datasets [Dataset],
    ) -> Result<Vec<std::result::Result<Entry<'datasets>, (TrashId, &'datasets Dataset, String)>>>
    {
        let layout = DatasetLayout::of(pool);
        let mut entries = Vec::new();
        for dataset in datasets {
            let Place::Trash(id) = layout.place(&dataset.name) else {
                continue;
            };
            if id.contains('/') {
                continue;
            }
            let Ok(id) = TrashId::parse(id) else {
                continue;
            };
            entries.push(
                self.entry(id, dataset)
                    .await?
                    .map_err(|why| (id, dataset, why)),
            );
        }
        Ok(entries)
    }

    /// The entry `dataset` with every mark read, or why one doesn't read.
    async fn entry<'datasets>(
        &self,
        id: TrashId,
        dataset: &'datasets Dataset,
    ) -> Result<std::result::Result<Entry<'datasets>, String>> {
        let marks = self.marks(&dataset.name);
        let mut values = Vec::new();
        for mark in ENTRY_MARKS {
            match marks.local(mark).await? {
                Some(value) => values.push(value),
                None => return Ok(Err(format!("it carries no {mark}"))),
            }
        }
        let [name, place, was, hour, at] = &values[..] else {
            unreachable!("one value per entry mark");
        };
        let unreadable = |mark: Mark, value: &str| format!("its {mark} {value:?} doesn't read");
        Ok((|| {
            Ok(Entry {
                id,
                dataset,
                name: name
                    .parse()
                    .map_err(|_| unreadable(Mark::TrashName, name))?,
                place: place
                    .parse()
                    .map_err(|_| unreadable(Mark::TrashPlace, place))?,
                was: Was::parse(was).ok_or_else(|| unreadable(Mark::TrashWas, was))?,
                hour: hour
                    .parse()
                    .map_err(|_| unreadable(Mark::TrashedHour, hour))?,
                at: at.parse().map_err(|_| unreadable(Mark::TrashedAt, at))?,
            })
        })())
    }

    /// The origin of the data an entry holds: what a root or a copy carries, or what a
    /// slot's record and copy say.
    async fn entry_origin(&self, entry: &Entry<'_>) -> Result<Origin> {
        let dataset = &entry.dataset.name;
        match entry.place {
            TrashPlace::Root | TrashPlace::Copy => {
                let newest = self.newest_snapshot(dataset).await?;
                self.origin_of(
                    None,
                    Some((dataset, newest.as_ref().map(|newest| &newest.name))),
                )
                .await
            }
            TrashPlace::Slot => {
                let fs = format!("{dataset}/fs");
                let newest = self.newest_snapshot(&fs).await?;
                self.origin_of(
                    Some(dataset),
                    Some((&fs, newest.as_ref().map(|newest| &newest.name))),
                )
                .await
            }
        }
    }

    /// What the Trash holds, when it holds anything, for a capacity refusal to say.
    pub(super) async fn trash_held(&self, pool: &MachinePool) -> Option<TrashHeld> {
        let datasets = self.datasets(pool).await.ok()?;
        let parent = DatasetLayout::of(pool).trash_parent();
        let hours = self.sweep_hours(&parent).await.unwrap_or(0);
        let now = self.now_seconds();
        let mut held = TrashHeld {
            bytes: 0,
            until: now,
        };
        for entry in self.entries(pool, &datasets).await.ok()? {
            match entry {
                Ok(entry) => {
                    held.bytes = held.bytes.saturating_add(entry.dataset.used_bytes);
                    held.until = held.until.max(entry.expires_at(hours, now));
                }
                Err((_, dataset, _)) => held.bytes = held.bytes.saturating_add(dataset.used_bytes),
            }
        }
        (held.bytes > 0).then_some(held)
    }

    async fn list_trash(&self) -> Result<TrashList> {
        let _held = self.admit_mutation().await?;
        let _pool_guard = self.pool.lock_mutation().await?;
        let Some(pool) = self.pool.one_usable().await? else {
            return Ok(TrashList::default());
        };
        let datasets = self.datasets(&pool).await?;
        let parent = DatasetLayout::of(&pool).trash_parent();
        if !datasets.iter().any(|dataset| dataset.name == parent) {
            return Ok(TrashList::default());
        }
        let hours = self.sweep_hours(&parent).await?;
        let now = self.now_seconds();
        let mut list = TrashList::default();
        for entry in self.entries(&pool, &datasets).await? {
            match entry {
                Ok(entry) => list.entries.push(TrashEntry {
                    id: entry.id,
                    place: entry.place,
                    name: entry.name.wire(),
                    origin: self.entry_origin(&entry).await.ok().flatten(),
                    trashed_at: entry.at,
                    expires_at: entry.expires_at(hours, now),
                    used_bytes: entry.dataset.used_bytes,
                }),
                Err((id, dataset, reason)) => list.unreadable.push(UnreadableTrash {
                    id,
                    used_bytes: dataset.used_bytes,
                    reason,
                }),
            }
        }
        list.entries
            .sort_by_key(|entry| std::cmp::Reverse(entry.trashed_at));
        Ok(list)
    }

    /// Puts entry `id` back where it was taken from; see `ployz volume trash restore`.
    async fn restore_trash(&self, id: TrashId) -> std::result::Result<TrashRestored, RpcError> {
        let _held = self.admit_mutation().await?;
        let _pool_guard = self.pool.lock_mutation().await?;
        let pool = self.one_pool().await?;
        let layout = DatasetLayout::of(&pool);
        let datasets = self.datasets(&pool).await?;
        let path = layout.trash_entry(&id);
        let Some(dataset) = datasets.iter().find(|dataset| dataset.name == path) else {
            return Err(RpcError::not_found(format!(
                "This Server's Trash holds no {id}"
            )));
        };
        let entry = self.entry(id, dataset).await?.map_err(|why| {
            RpcError::conflict(format!("Trash entry {id} can't be restored: {why}"))
        })?;
        let name = &entry.name;
        if Self::dataset(&datasets, &pool, name)?.is_some()
            || Self::slot(&datasets, &pool, name).is_some()
        {
            return Err(RpcError::conflict(format!(
                "Volume {name} already has a copy on this Server; delete it or restore this entry after it is gone"
            )));
        }
        if entry.place == TrashPlace::Slot
            && self.marks(&path).local(Mark::CopyStep).await?.is_some()
        {
            return Err(RpcError::conflict(format!(
                "Trash entry {id} is a copy a restore was still making into {name}; approve that restore again instead"
            )));
        }
        let origin = self.entry_origin(&entry).await?;
        self.admit_holders(&name.wire(), origin)
            .await
            .map_err(|unheld| match unheld {
                Unheld::Foreign => RpcError::conflict(copy_held_for_other_data(&name.wire())),
                Unheld::Unlisted(cause) => {
                    RpcError::unavailable(copy_unlisted(&name.wire(), &cause))
                }
            })?;
        let commitment = Commitment::of(&pool, &datasets, dataset.refquota)?;
        self.ensure_pool_capacity(
            &pool,
            commitment.bytes,
            commitment
                .unmanaged_used_bytes
                .saturating_sub(dataset.used_bytes),
        )
        .await?;
        match entry.place {
            TrashPlace::Root => {
                self.ensure_root(&datasets, &layout.root_parent()).await?;
                self.marks(&path)
                    .apply(&[Write::Set(Mark::Restored, "on".to_owned())])
                    .await?;
                for write in entry.was.writes() {
                    let mut args: Vec<&str> = write.iter().map(String::as_str).collect();
                    args.push(&path);
                    self.zfs(&args).await?;
                }
                self.zfs(&["rename", &path, &layout.root(name)]).await?;
                self.unmark(&layout.root(name)).await?;
            }
            TrashPlace::Slot => {
                self.ensure_mirror_root(&pool, &datasets).await?;
                self.marks(&path)
                    .apply(&[
                        Write::Set(Mark::Role, SlotRole::OldCopy.as_str().to_owned()),
                        Write::Inherit(Mark::Pull),
                    ])
                    .await?;
                self.zfs(&["set", "canmount=off", &path]).await?;
                self.zfs(&["rename", &path, &layout.slot_parent(name)])
                    .await?;
                self.unmark(&layout.slot_parent(name)).await?;
            }
            TrashPlace::Copy => {
                self.ensure_mirror_root(&pool, &datasets).await?;
                let parent = layout.slot_parent(name);
                self.create_marked_slot(
                    &parent,
                    dataset.refquota,
                    &[(Mark::Role, SlotRole::OldCopy.as_str().to_owned())],
                )
                .await?;
                for write in entry.was.writes() {
                    let mut args: Vec<&str> = write.iter().map(String::as_str).collect();
                    args.push(&path);
                    self.zfs(&args).await?;
                }
                self.zfs(&["rename", &path, &layout.slot_fs(name)]).await?;
                self.unmark(&layout.slot_fs(name)).await?;
            }
        }
        Ok(TrashRestored {
            id,
            name: name.wire(),
            place: entry.place,
        })
    }

    /// Takes the trash marks off a restored copy.
    async fn unmark(&self, dataset: &str) -> Result<()> {
        self.marks(dataset)
            .apply(&ENTRY_MARKS.map(Write::Inherit))
            .await
    }

    async fn empty_trash(&self, ids: &[TrashId]) -> Result<TrashEmptied> {
        let _held = self.admit_mutation().await?;
        let _pool_guard = self.pool.lock_mutation().await?;
        let mut emptied = TrashEmptied::default();
        let Some(pool) = self.pool.one_usable().await? else {
            emptied.missing = ids.to_vec();
            return Ok(emptied);
        };
        let datasets = self.datasets(&pool).await?;
        for &id in ids {
            let path = DatasetLayout::of(&pool).trash_entry(&id);
            let Some(dataset) = datasets.iter().find(|dataset| dataset.name == path) else {
                emptied.missing.push(id);
                continue;
            };
            let destroyed = match self.entry(id, dataset).await? {
                Ok(_) => self.destroy(&path, Holds::Expired).await,
                Err(why) => Err(PluginError::from(format!(
                    "its marks don't read, so Ployz keeps it: {why}"
                ))),
            };
            match destroyed {
                Ok(()) => emptied.emptied.push(id),
                Err(error) => emptied.failed.push(TrashFailure {
                    id,
                    reason: error.to_string(),
                }),
            }
        }
        Ok(emptied)
    }

    /// One hourly sweep; see the module doc.
    async fn sweep_trash(&self) -> Result<TrashSwept> {
        let _held = self.admit_mutation().await?;
        let _pool_guard = self.pool.lock_mutation().await?;
        let Some(pool) = self.pool.one_usable().await? else {
            return Ok(TrashSwept::default());
        };
        let datasets = self.datasets(&pool).await?;
        let parent = self.ensure_trash_parent(pool.name(), &datasets).await?;
        self.keep_slack(&parent).await;
        let mut hours = self.sweep_hours(&parent).await?;
        let now = self.now_seconds();
        let last = self.marks(&parent).local(Mark::TrashLast).await?;
        let last = last.and_then(|last| last.parse::<i64>().ok());
        let advanced = last.is_some_and(|last| now.saturating_sub(last) >= HOUR_SECONDS);
        if last.is_none_or(|last| advanced || now < last) {
            // The time goes first: a stop between the two writes loses an hour rather than
            // adding one.
            self.marks(&parent)
                .apply(&[Write::Set(Mark::TrashLast, now.to_string())])
                .await?;
        }
        if advanced {
            hours += 1;
            self.marks(&parent)
                .apply(&[Write::Set(Mark::TrashHours, hours.to_string())])
                .await?;
        }
        let mut swept = TrashSwept {
            hours,
            advanced,
            ..TrashSwept::default()
        };
        for entry in self.entries(&pool, &datasets).await? {
            let (id, dataset, expired) = match &entry {
                Ok(entry) => (entry.id, entry.dataset, entry.left(hours) == 0),
                Err((id, dataset, _)) => (*id, *dataset, false),
            };
            let result = async {
                self.settle_entry(&dataset.name).await?;
                // A move that stopped before its tombstone leaves it here.
                self.bury(&dataset.name, true).await?;
                if expired {
                    self.destroy(&dataset.name, Holds::Expired).await?;
                }
                Ok::<_, PluginError>(())
            }
            .await;
            match result {
                Ok(()) if expired => swept.destroyed.push(id),
                Ok(()) => {}
                Err(error) => swept.failed.push(TrashFailure {
                    id,
                    reason: error.to_string(),
                }),
            }
        }
        Ok(swept)
    }
}

pub(super) async fn list(
    State(storage): State<VolumeStorage>,
) -> Json<std::result::Result<TrashList, RpcError>> {
    Json(storage.list_trash().await.map_err(RpcError::from))
}

pub(super) async fn restore(
    State(storage): State<VolumeStorage>,
    Json(request): Json<RestoreTrashRequest>,
) -> Json<std::result::Result<TrashRestored, RpcError>> {
    Json(storage.restore_trash(request.id).await)
}

pub(super) async fn empty(
    State(storage): State<VolumeStorage>,
    Json(request): Json<EmptyTrashRequest>,
) -> Json<std::result::Result<TrashEmptied, RpcError>> {
    Json(
        storage
            .empty_trash(&request.ids)
            .await
            .map_err(RpcError::from),
    )
}

pub(super) async fn sweep(
    State(storage): State<VolumeStorage>,
) -> Json<std::result::Result<TrashSwept, RpcError>> {
    Json(storage.sweep_trash().await.map_err(RpcError::from))
}
