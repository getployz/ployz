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
//!    its own `canmount` (`ployz:trash-was`), and the sweep hour. Nothing on a live copy
//!    reads them, so a stop here leaves a working Volume. Nothing changes `canmount` before
//!    the rename, so a delete run again reads the value the first attempt read.
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
//!
//! A deleted root comes back in two calls. The first pins the entry (`ployz:trash-pin`),
//! which needs no other Server and restarts its 7 days. Once every Server showed it holds
//! no copy, the second makes it the Volume's root under the Pool lock: it checks this
//! Server holds no copy and no other pinned entry of the Volume, readies the entry inside
//! the Trash with `canmount=off`, renames it into place, then puts its `canmount` back and
//! takes the trash marks off. The root keeps the entry's ID until Docker's Create, so a
//! call stopped after the rename finishes when run again.

use std::fmt;

use axum::{Json, extract::State};
use chrono::DateTime;
use ployz_core::{
    EmptyTrashRequest, RestoreTrashRequest, RpcError, SlotRole, TRASH_SWEEP_HOURS, TrashEmptied,
    TrashEntry, TrashFailure, TrashId, TrashList, TrashPlace, TrashRestore, TrashRestored,
    UnreadableTrash, WriterEpoch,
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
    machine_pool::{MachinePool, Read},
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

/// The `canmount` a copy set on itself before the move, `None` when it had the default.
/// The move keeps the copy's `readonly`, so a restore has nothing of it to put back.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Was {
    canmount: Option<String>,
}

impl Was {
    async fn read(storage: &VolumeStorage, dataset: &str) -> Result<Self> {
        Ok(Self {
            canmount: storage.local_property(dataset, "canmount").await?,
        })
    }

    fn parse(value: &str) -> Option<Self> {
        let value = value.strip_prefix("canmount=")?;
        Some(Self {
            canmount: (value != "-").then(|| value.to_owned()),
        })
    }

    /// Puts the `canmount` back on `dataset`, a restored copy. It isn't inherited: its
    /// default is on.
    async fn put_back(&self, storage: &VolumeStorage, dataset: &str) -> Result<()> {
        let canmount = format!("canmount={}", self.canmount.as_deref().unwrap_or("on"));
        storage.zfs(&["set", &canmount, dataset]).await.map(drop)
    }
}

/// A restore's pin on a root entry: the entry's ID and the epoch it becomes the Volume's
/// data at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Pin {
    id: TrashId,
    epoch: WriterEpoch,
}

impl Pin {
    fn parse(value: &str) -> Option<Self> {
        let (id, epoch) = value.split_once(':')?;
        Some(Self {
            id: TrashId::parse(id).ok()?,
            epoch: WriterEpoch::new(epoch.parse().ok()?),
        })
    }
}

impl fmt::Display for Pin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.id, self.epoch)
    }
}

impl fmt::Display for Was {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "canmount={}",
            self.canmount.as_deref().unwrap_or("-")
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
    pin: Option<Pin>,
}

impl Entry<'_> {
    /// The epoch a restore of this entry pinned it at; a pin naming another ID is stale.
    fn pinned(&self) -> Option<WriterEpoch> {
        self.pin
            .filter(|pin| pin.id == self.id)
            .map(|pin| pin.epoch)
    }

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
    /// When the last entry no restore pinned expires, if any.
    until: Option<i64>,
    /// Whether a restore pinned any of it.
    restoring: bool,
}

impl fmt::Display for TrashHeld {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let until = self.until.map(|until| {
            DateTime::from_timestamp(until, 0)
                .map_or_else(|| until.to_string(), |at| at.format("%Y-%m-%d").to_string())
        });
        let when = match (self.restoring, until) {
            (false, Some(until)) => format!(" until {until}"),
            (true, Some(until)) => format!(", kept while restoring or until {until}"),
            (_, None) => ", kept while restoring".to_owned(),
        };
        let held = if self.bytes < 100_000_000 {
            format!("{} MB", self.bytes.div_ceil(1_000_000))
        } else {
            format!("{:.1} GB", self.bytes as f64 / 1e9)
        };
        write!(
            formatter,
            "{held} is in this Server's Trash{when}. Run `sudo ployz volume trash empty` on this Server to free it."
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
        // A root whose restore stopped before `Was` came back still carries the `canmount`
        // it was first deleted with; reading it live would keep the restore's `off` for good.
        let was = match self.marks(dataset).local(Mark::TrashWas).await? {
            Some(was) => was,
            None => Was::read(self, dataset).await?.to_string(),
        };
        // A copy a restore made carries what that restore left; its new entry takes none.
        self.marks(dataset)
            .apply(&[
                Write::Inherit(Mark::TrashPin),
                Write::Inherit(Mark::TrashRestored),
                Write::Inherit(Mark::TrashRestoring),
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
        // The copy's own bytes leave the managed count and join the Trash; its snapshots'
        // bytes were never managed and only change sides.
        let copy = datasets.iter().find(|dataset| dataset.name == fs);
        let mut commitment = Commitment::of(&pool, &datasets, 0)?;
        commitment.unmanaged_used_bytes = commitment
            .unmanaged_used_bytes
            .saturating_add(copy.map_or(0, |copy| copy.active_used_bytes));
        commitment.trash_bytes = commitment
            .trash_bytes
            .saturating_add(copy.map_or(0, |copy| copy.used_bytes));
        self.ensure_pool_capacity(&pool, &commitment).await?;
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
        let pin = marks.local(Mark::TrashPin).await?;
        let unreadable = |mark: Mark, value: &str| format!("its {mark} {value:?} doesn't read");
        Ok((|| {
            Ok(Entry {
                pin: pin
                    .as_deref()
                    .map(|pin| Pin::parse(pin).ok_or_else(|| unreadable(Mark::TrashPin, pin)))
                    .transpose()?,
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
            until: None,
            restoring: false,
        };
        for entry in self.entries(pool, &datasets).await.ok()? {
            match entry {
                Ok(entry) => {
                    held.bytes = held.bytes.saturating_add(entry.dataset.used_bytes);
                    if entry.pin.is_some() {
                        held.restoring = true;
                    } else {
                        let until = entry.expires_at(hours, now);
                        held.until = Some(held.until.map_or(until, |held| held.max(until)));
                    }
                }
                Err((_, dataset, _)) => {
                    held.bytes = held.bytes.saturating_add(dataset.used_bytes);
                    held.until = Some(held.until.map_or(now, |held| held.max(now)));
                }
            }
        }
        (held.bytes > 0).then_some(held)
    }

    /// The Pool whose Trash List and Empty read: none on a Machine that never held one,
    /// and a refusal on one whose Pool isn't imported, whose Trash is unknown, not empty.
    async fn trash_pool(&self) -> Result<Option<MachinePool>> {
        match self.pool.one_usable().await? {
            None if self.pool.has_held_a_pool()? => Err(PluginError::HeldPoolNotImported),
            pool => Ok(pool),
        }
    }

    async fn list_trash(&self) -> Result<TrashList> {
        let _held = self.admit_mutation().await?;
        let _pool_guard = self.pool.lock_mutation().await?;
        let Some(pool) = self.trash_pool().await? else {
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
                    restoring: entry.pin.is_some(),
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

    /// Pins a root entry, makes a pinned one the Volume's root, or puts a mirror or old copy
    /// back as an old copy; see the module doc and `ployz volume trash restore`.
    async fn restore_trash(
        &self,
        id: TrashId,
        promote: bool,
    ) -> std::result::Result<TrashRestored, RpcError> {
        let _held = self.admit_mutation().await?;
        let _pool_guard = self.pool.lock_mutation().await?;
        let pool = self.one_pool().await?;
        let layout = DatasetLayout::of(&pool);
        let datasets = self.complete_datasets(&pool).await?;
        // A promote that renamed the entry left its ID on the root, so a call run again
        // finishes it there before it looks for the entry.
        if let Some(root) = self.restored_root(&layout, &datasets, id).await? {
            return self.finish_restore(&layout, &root, id).await;
        }
        let path = layout.trash_entry(&id);
        let Some(dataset) = datasets.iter().find(|dataset| dataset.name == path) else {
            return Err(RpcError::not_found(format!(
                "This Server's Trash holds no {id}, and no Volume on it came from that entry. Check `ployz volume ls`."
            )));
        };
        let entry = self.entry(id, dataset).await?.map_err(|why| {
            RpcError::conflict(format!("Trash entry {id} can't be restored: {why}"))
        })?;
        if entry.place == TrashPlace::Root {
            self.own_pool(&pool).await?;
        }
        match (entry.place, promote) {
            (TrashPlace::Root, false) => self.pin(&layout, &entry).await,
            (TrashPlace::Root, true) => self.promote(&pool, &datasets, &entry).await,
            (TrashPlace::Slot | TrashPlace::Copy, _) => {
                self.land(&pool, &datasets, dataset, &entry).await
            }
        }
    }

    /// Refuses unless `pool` is the Pool this Machine knows is its own, by its records and
    /// what ZFS shows: the proof a restore reads is answered from that Pool, so a root comes
    /// back only there.
    async fn own_pool(&self, pool: &MachinePool) -> std::result::Result<(), RpcError> {
        match self.pool.read().await? {
            Read::Pool { pool: own, .. } if own.name() == pool.name() => Ok(()),
            Read::Pool { pool: own, .. } => Err(RpcError::conflict(format!(
                "This Server's own Pool is {}, not {}, so it won't restore from {}'s Trash.",
                own.name(),
                pool.name(),
                pool.name()
            ))),
            Read::Unheld => Err(RpcError::conflict(
                "This Server records no Pool as its own, so it won't restore from its Trash."
                    .to_owned(),
            )),
            Read::Unproven(why) => Err(RpcError::conflict(format!(
                "This Server won't restore from its Trash, since {why}."
            ))),
        }
    }

    /// The root carrying `id`'s restore, when a promote of it renamed it into place.
    async fn restored_root(
        &self,
        layout: &DatasetLayout<'_>,
        datasets: &[Dataset],
        id: TrashId,
    ) -> Result<Option<String>> {
        let parent = layout.root_parent();
        if !datasets.iter().any(|dataset| dataset.name == parent) {
            return Ok(None);
        }
        let output = self
            .zfs(&[
                "get",
                "-r",
                "-H",
                "-s",
                "local",
                "-o",
                "name,value",
                Mark::TrashRestored.name(),
                &parent,
            ])
            .await?;
        Ok(output.lines().find_map(|line| {
            let (name, value) = line.split_once('\t')?;
            (value == id.as_str()
                && matches!(layout.place(name), Place::Root(root) if !root.contains('/')))
            .then(|| name.to_owned())
        }))
    }

    /// Pins the root entry for its restore, or refreshes the pin it has: either way its 7
    /// days start again. The epoch it becomes the Volume's data at is the entry's next one,
    /// kept from an earlier pin.
    async fn pin(
        &self,
        layout: &DatasetLayout<'_>,
        entry: &Entry<'_>,
    ) -> std::result::Result<TrashRestored, RpcError> {
        let name = &entry.name;
        if entry.dataset.refquota == 0 {
            return Err(RpcError::conflict(format!(
                "Trash entry {} can't be restored: it carries no Volume bound",
                entry.id
            )));
        }
        let epoch = match entry.pinned() {
            Some(epoch) => epoch,
            None => {
                let held = self.marks(&entry.dataset.name).local(Mark::Epoch).await?;
                let next = match held.as_deref() {
                    None => Some(1),
                    Some(held) => held
                        .parse::<u32>()
                        .ok()
                        .and_then(|held| held.checked_add(1)),
                };
                let Some(next) = next else {
                    return Err(RpcError::conflict(format!(
                        "{name}'s deleted copy shows epoch {}, the highest there is.",
                        held.unwrap_or_default()
                    )));
                };
                WriterEpoch::new(next)
            }
        };
        let hours = self.sweep_hours(&layout.trash_parent()).await?;
        let pin = Pin {
            id: entry.id,
            epoch,
        };
        // The clock first: a pin left on the old clock would let the next sweep destroy it.
        self.marks(&entry.dataset.name)
            .apply(&[
                Write::Set(Mark::TrashedHour, hours.to_string()),
                Write::Set(Mark::TrashPin, pin.to_string()),
            ])
            .await?;
        Ok(TrashRestored {
            id: entry.id,
            name: name.wire(),
            place: entry.place,
            state: TrashRestore::Pinned { epoch },
        })
    }

    /// Makes the pinned root entry the Volume's root; see the module doc.
    async fn promote(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        entry: &Entry<'_>,
    ) -> std::result::Result<TrashRestored, RpcError> {
        let id = entry.id;
        let name = &entry.name;
        let path = &entry.dataset.name;
        let layout = DatasetLayout::of(pool);
        let Some(epoch) = entry.pinned() else {
            return Err(RpcError::conflict(format!(
                "Trash entry {id} isn't being restored any more."
            )));
        };
        if Self::dataset(datasets, pool, name)?.is_some()
            || Self::slot(datasets, pool, name).is_some()
        {
            return Err(RpcError::conflict(format!(
                "Volume {name} already has a copy on this Server; delete it, then run this again."
            )));
        }
        if let Some(other) = self
            .pinned_entries(pool, datasets, name)
            .await?
            .into_iter()
            .find(|other| *other != id)
        {
            return Err(RpcError::conflict(format!(
                "Volume {name} is also being restored on this Server from Trash entry {other}. Pick the one to keep. Delete the other with `sudo ployz volume trash empty ID`, or stop restoring it and it expires in 7 days."
            )));
        }
        let origin = self.entry_origin(entry).await?;
        self.admit_holders(&name.wire(), origin)
            .await
            .map_err(|unheld| match unheld {
                Unheld::Foreign => RpcError::conflict(copy_held_for_other_data(&name.wire())),
                Unheld::Unlisted(cause) => {
                    RpcError::unavailable(copy_unlisted(&name.wire(), &cause))
                }
            })?;
        self.admit_restore(pool, datasets, entry.dataset, false)
            .await?;
        // A delete stopped before it settled the entry left it mountable.
        self.settle_entry(path).await?;
        self.marks(path)
            .apply(&[
                Write::Set(Mark::Epoch, epoch.to_string()),
                Write::Set(Mark::Restored, "on".to_owned()),
                Write::Set(Mark::TrashRestored, id.to_string()),
                Write::Inherit(Mark::Cut),
                Write::Inherit(Mark::Promote),
                Write::Inherit(Mark::Seed),
            ])
            .await?;
        self.zfs(&["set", "readonly=off", path]).await?;
        self.ensure_root(datasets, &layout.root_parent()).await?;
        crate::faults::kill_inside(crate::faults::Point::Verb("Trash.Readied"), None);
        let root = layout.root(name);
        self.zfs(&["rename", path, &root]).await?;
        crate::faults::kill_inside(crate::faults::Point::Verb("Trash.Promoted"), None);
        self.finish_restore(&layout, &root, id).await
    }

    /// The last steps of a promote, which a call run again repeats; it answers that the
    /// restore committed.
    async fn finish_restore(
        &self,
        layout: &DatasetLayout<'_>,
        root: &str,
        id: TrashId,
    ) -> std::result::Result<TrashRestored, RpcError> {
        self.finish_promote(root).await?;
        let marks = self.marks(root);
        let Place::Root(name) = layout.place(root) else {
            return Err(PluginError::from(format!("{root} is not a Volume root")).into());
        };
        let name: VolumeName = name
            .parse()
            .map_err(|_| PluginError::from(format!("{root} names no Volume")))?;
        Ok(TrashRestored {
            id,
            name: name.wire(),
            place: TrashPlace::Root,
            state: TrashRestore::Restored {
                epoch: marks.epoch().await?,
            },
        })
    }

    /// Finishes a promote that renamed its entry into `root`: the `canmount` the copy had
    /// before its delete, while its mark still says what it was, then the trash marks and
    /// the pin off. Each step is safe to repeat.
    pub(super) async fn finish_promote(&self, root: &str) -> Result<()> {
        if let Some(was) = self.marks(root).local(Mark::TrashWas).await? {
            let was = Was::parse(&was).ok_or_else(|| {
                PluginError::from(format!(
                    "{root} carries an unreadable {} {was:?}",
                    Mark::TrashWas
                ))
            })?;
            was.put_back(self, root).await?;
            crate::faults::kill_inside(crate::faults::Point::Verb("Trash.WasBack"), None);
        }
        self.unmark(root).await
    }

    /// The IDs of `name`'s entries a restore pinned, whether or not their marks read. One
    /// whose name doesn't read may be `name`'s, so it refuses.
    pub(super) async fn pinned_entries(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        name: &VolumeName,
    ) -> Result<Vec<TrashId>> {
        let layout = DatasetLayout::of(pool);
        let mut pinned = Vec::new();
        for dataset in datasets {
            let Place::Trash(id) = layout.place(&dataset.name) else {
                continue;
            };
            let Ok(id) = TrashId::parse(id) else {
                continue;
            };
            let marks = self.marks(&dataset.name);
            if marks.local(Mark::TrashPin).await?.is_none() {
                continue;
            }
            match marks
                .local(Mark::TrashName)
                .await?
                .map(|held| held.parse::<VolumeName>())
            {
                Some(Ok(held)) if held == *name => pinned.push(id),
                Some(Ok(_)) => {}
                _ => {
                    return Err(PluginError::from(format!(
                        "Trash entry {id} is being restored, and its {} doesn't read",
                        Mark::TrashName
                    )));
                }
            }
        }
        Ok(pinned)
    }

    /// Refuses a restore the Pool can't hold beside what it holds: the entry's own bytes
    /// join the managed count, as `Commitment::of` reads a live copy, and its snapshots'
    /// bytes only leave the Trash. A copy whose slot a stopped restore already made adds
    /// no bound.
    async fn admit_restore(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        dataset: &Dataset,
        resumed: bool,
    ) -> Result<()> {
        let path = &dataset.name;
        let within = format!("{path}/");
        let active = datasets
            .iter()
            .filter(|held| held.name == *path || held.name.starts_with(&within))
            .map(|held| held.active_used_bytes)
            .fold(0u64, u64::saturating_add);
        let bound = if resumed { 0 } else { dataset.refquota };
        let mut commitment = Commitment::of(pool, datasets, bound)?;
        commitment.unmanaged_used_bytes = commitment.unmanaged_used_bytes.saturating_sub(active);
        commitment.trash_bytes = commitment.trash_bytes.saturating_sub(dataset.used_bytes);
        self.ensure_pool_capacity(pool, &commitment).await
    }

    /// Puts a mirror or old copy entry back as an old copy, as trash-1 restores it.
    async fn land(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        dataset: &Dataset,
        entry: &Entry<'_>,
    ) -> std::result::Result<TrashRestored, RpcError> {
        let id = entry.id;
        let layout = DatasetLayout::of(pool);
        let path = &dataset.name;
        let name = &entry.name;
        let slot = Self::slot(datasets, pool, name);
        // A restore of a copy that stopped before its rename left the slot parent it made,
        // which it takes up again.
        let resumed = match slot {
            Some(slot) if entry.place == TrashPlace::Copy => {
                self.restoring_into(datasets, slot, id).await?
            }
            _ => false,
        };
        if Self::dataset(datasets, pool, name)?.is_some() || (slot.is_some() && !resumed) {
            return Err(RpcError::conflict(format!(
                "Volume {name} already has a copy on this Server; delete it or restore this entry after it is gone"
            )));
        }
        if entry.place == TrashPlace::Slot
            && self.marks(path).local(Mark::CopyStep).await?.is_some()
        {
            return Err(RpcError::conflict(format!(
                "Trash entry {id} is a copy a restore was still making into {name}; approve that restore again instead"
            )));
        }
        let origin = self.entry_origin(entry).await?;
        self.admit_holders(&name.wire(), origin)
            .await
            .map_err(|unheld| match unheld {
                Unheld::Foreign => RpcError::conflict(copy_held_for_other_data(&name.wire())),
                Unheld::Unlisted(cause) => {
                    RpcError::unavailable(copy_unlisted(&name.wire(), &cause))
                }
            })?;
        self.admit_restore(pool, datasets, dataset, resumed).await?;
        self.ensure_mirror_root(pool, datasets).await?;
        if entry.place == TrashPlace::Slot {
            self.marks(path)
                .apply(&[
                    Write::Set(Mark::Role, SlotRole::OldCopy.as_str().to_owned()),
                    Write::Inherit(Mark::Pull),
                ])
                .await?;
            self.zfs(&["set", "canmount=off", path]).await?;
            self.zfs(&["rename", path, &layout.slot_parent(name)])
                .await?;
            self.unmark(&layout.slot_parent(name)).await?;
        } else {
            let parent = layout.slot_parent(name);
            if !resumed {
                self.create_marked_slot(
                    &parent,
                    dataset.refquota,
                    &[
                        (Mark::Role, SlotRole::OldCopy.as_str().to_owned()),
                        (Mark::TrashRestoring, id.to_string()),
                    ],
                )
                .await?;
            }
            crate::faults::kill_inside(crate::faults::Point::Verb("Trash.SlotMade"), None);
            entry.was.put_back(self, path).await?;
            self.zfs(&["rename", path, &layout.slot_fs(name)]).await?;
            self.unmark(&layout.slot_fs(name)).await?;
            self.marks(&parent)
                .apply(&[Write::Inherit(Mark::TrashRestoring)])
                .await?;
        }
        Ok(TrashRestored {
            id,
            name: name.wire(),
            place: entry.place,
            state: TrashRestore::Landed,
        })
    }

    /// Whether `slot` is the parent a restore of entry `id` made, with no copy under it yet.
    async fn restoring_into(
        &self,
        datasets: &[Dataset],
        slot: &Dataset,
        id: TrashId,
    ) -> Result<bool> {
        let within = format!("{}/", slot.name);
        if datasets.iter().any(|held| held.name.starts_with(&within)) {
            return Ok(false);
        }
        let mark = self.marks(&slot.name).local(Mark::TrashRestoring).await?;
        Ok(mark.as_deref() == Some(id.to_string().as_str()))
    }

    /// Takes the trash marks and any pin off a restored copy. `ployz:trash-was` goes after
    /// the `canmount` it records is back.
    async fn unmark(&self, dataset: &str) -> Result<()> {
        self.marks(dataset)
            .apply(&ENTRY_MARKS.map(Write::Inherit))
            .await?;
        self.marks(dataset)
            .apply(&[Write::Inherit(Mark::TrashPin)])
            .await
    }

    /// Destroys the entries `ids` names. Emptying the whole Trash (`bulk`) keeps any a
    /// restore pinned, read again here under the lock; named, they go.
    async fn empty_trash(&self, ids: &[TrashId], bulk: bool) -> Result<TrashEmptied> {
        let _held = self.admit_mutation().await?;
        let _pool_guard = self.pool.lock_mutation().await?;
        let mut emptied = TrashEmptied::default();
        let Some(pool) = self.trash_pool().await? else {
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
            if bulk && self.marks(&path).local(Mark::TrashPin).await?.is_some() {
                emptied.restoring.push(id);
                continue;
            }
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
        // An advance counts the hour after `last`, so a sweep that runs late loses no hour.
        // A gap of two hours or more, or a clock behind `last`, starts again from now: the
        // hours never run ahead of the wall time the sweep saw pass.
        let counted = match last {
            Some(last) if advanced && now.saturating_sub(last) < 2 * HOUR_SECONDS => {
                Some(last + HOUR_SECONDS)
            }
            Some(last) if !advanced && now >= last => None,
            _ => Some(now),
        };
        if let Some(counted) = counted {
            // The time goes first: a stop between the two writes loses an hour rather than
            // adding one.
            self.marks(&parent)
                .apply(&[Write::Set(Mark::TrashLast, counted.to_string())])
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
    Json(storage.restore_trash(request.id, request.promote).await)
}

pub(super) async fn empty(
    State(storage): State<VolumeStorage>,
    Json(request): Json<EmptyTrashRequest>,
) -> Json<std::result::Result<TrashEmptied, RpcError>> {
    Json(
        storage
            .empty_trash(&request.ids, request.bulk)
            .await
            .map_err(RpcError::from),
    )
}

pub(super) async fn sweep(
    State(storage): State<VolumeStorage>,
) -> Json<std::result::Result<TrashSwept, RpcError>> {
    Json(storage.sweep_trash().await.map_err(RpcError::from))
}
