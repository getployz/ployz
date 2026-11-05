//! What this Machine holds for one Volume name: its root or slot and its newest snapshot.

use axum::{Json, extract::State};
use ployz_core::{
    InspectVolumeCopyRequest, Pull, PullState, RootMark, RpcError, RpcErrorCode, SlotId, SlotRole,
    Snapshot, SnapshotGuid, SnapshotName, VolumeCopy, VolumeCopyView, WriterEpoch,
};
use ployzd::machine_pool::MachinePool;

use super::{
    Dataset, DockerVolumeName, Result, VolumeError, VolumeStorage,
    seed::SEED_PROPERTY,
    storage::{
        CUT_PROPERTY, EPOCH_PROPERTY, PROMOTE_PROPERTY, PULL_PROPERTY, RESUME_TOKEN_PROPERTY,
        ROLE_PROPERTY, slot_fs, slot_parent,
    },
    transfer::Status,
};

/// The `ployz:*` marks [`RootCopy::parse`] reads off a root. They narrow which steps can
/// have left it read-only and name none.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RootMarks {
    /// `ployz:cut`, read only on a read-only root, the one place `parse` looks at it.
    pub(super) cut: bool,
    pub(super) promote: PromoteMark,
}

/// `ployz:promote`, and whether `ployz:seed` is beside it. `ployz:seed` without
/// `ployz:promote` counts for nothing, so it has no place here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PromoteMark {
    Unset,
    Set { seed: bool },
}

/// A Volume's root on this Machine, parsed once from its `readonly` and its marks for
/// `Volume.Inspect` and every reply built by [`VolumeStorage::copy`]. Each variant claims
/// only what those show. `seed` is a root Volume.Seed landed that Docker
/// has not adopted: it carries both `ployz:seed` and `ployz:promote`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RootCopy {
    /// Writable, whatever its marks.
    Writable { seed: bool },
    /// Read-only with `ployz:cut`. A cut (`copy_cut`) leaves it from that mark until its
    /// rename into the slot, and a promote of a cut copy (`copy_writer`) from its rename
    /// back until `make_writable`'s `readonly=off`. The marks do not tell those two apart.
    Cut { seed: bool },
    /// Read-only with `ployz:promote` and no `ployz:cut`. A promote of a pulled copy
    /// (`copy_writer`) leaves it from its rename until `make_writable`'s `readonly=off`,
    /// and a Seed landing from `Copied::land`'s rename until `Renamed::finish_landing`'s
    /// `readonly=off`. So does a cut (`copy_cut`) of a root Docker has not registered, from
    /// `readonly=on` until `ployz:cut`.
    Promote { seed: bool },
    /// Read-only with neither mark. A cut (`copy_cut`) of a registered root leaves it from
    /// `readonly=on` until `ployz:cut`.
    Unmarked,
}

impl RootCopy {
    /// The read-only split is [`Dataset::switching`], the one [`super::storage::Held`]
    /// reads too.
    pub(super) fn parse(root: &Dataset, marks: RootMarks) -> Self {
        let seed = marks.promote == PromoteMark::Set { seed: true };
        match (root.switching(), marks.cut, marks.promote) {
            (false, _, _) => Self::Writable { seed },
            (true, true, _) => Self::Cut { seed },
            (true, false, PromoteMark::Set { .. }) => Self::Promote { seed },
            (true, false, PromoteMark::Unset) => Self::Unmarked,
        }
    }

    /// The root as `Volume.Inspect` and every `SetVolumeCopy` reply carry it.
    pub(super) fn view(self, newest: Option<Snapshot>, epoch: WriterEpoch) -> VolumeCopy {
        let (readonly, seed, mark) = match self {
            Self::Writable { seed } => (false, seed, None),
            Self::Cut { seed } => (true, seed, Some(RootMark::Cut)),
            Self::Promote { seed } => (true, seed, Some(RootMark::Promote)),
            Self::Unmarked => (true, false, None),
        };
        VolumeCopy::Root {
            readonly,
            newest,
            epoch,
            seed,
            mark,
        }
    }
}

/// `copy` answering `failure`, which a route took from the receive registry to answer
/// once, as its pull's failure.
pub(super) fn answering(mut copy: Option<VolumeCopy>, failure: String) -> Option<VolumeCopy> {
    if let Some(VolumeCopy::Slot {
        pull: Some(pull),
        pull_state,
        ..
    }) = &mut copy
    {
        *pull_state = Some(PullState::Failed {
            target: pull.target.clone(),
            failure: failure.clone(),
        });
        pull.failure = Some(failure);
    }
    copy
}

pub(super) fn internal(error: VolumeError) -> RpcError {
    match error {
        VolumeError::Capacity(error) => error.into_rpc_error(),
        VolumeError::Message(message) => RpcError {
            code: RpcErrorCode::Internal,
            message,
            details: serde_json::Value::Null,
            cause: Vec::new(),
        },
    }
}

impl VolumeStorage {
    /// A dataset's user property, or `None` when ZFS reports it unset (`-`).
    pub(super) async fn property(&self, dataset: &str, property: &str) -> Result<Option<String>> {
        let value = self
            .zfs(&["get", "-H", "-o", "value", property, dataset])
            .await?;
        let value = value.trim_end_matches('\n');
        Ok((value != "-").then(|| value.to_owned()))
    }

    /// A user property set on the dataset itself, ignoring any value it inherits.
    pub(super) async fn local_property(
        &self,
        dataset: &str,
        property: &str,
    ) -> Result<Option<String>> {
        let value = self
            .zfs(&["get", "-H", "-s", "local", "-o", "value", property, dataset])
            .await?;
        let value = value.trim_end_matches('\n');
        Ok((!value.is_empty() && value != "-").then(|| value.to_owned()))
    }

    /// The guid of a dataset's newest snapshot, whatever its name.
    pub(super) async fn newest_guid(&self, dataset: &str) -> Result<Option<String>> {
        Ok(self
            .list_snapshots(dataset)
            .await?
            .lines()
            .next()
            .and_then(|line| line.split('\t').nth(1))
            .map(str::to_owned))
    }

    pub(super) async fn unregistered(&self, dataset: &str) -> Result<bool> {
        Ok(self.property(dataset, PROMOTE_PROPERTY).await?.is_some())
    }

    pub(super) async fn unadopted_seed(&self, dataset: &str) -> Result<bool> {
        Ok(self.unregistered(dataset).await?
            && self.property(dataset, SEED_PROPERTY).await?.is_some())
    }

    async fn root_marks(&self, root: &Dataset) -> Result<RootMarks> {
        let promote = if self.unregistered(&root.name).await? {
            PromoteMark::Set {
                seed: self.property(&root.name, SEED_PROPERTY).await?.is_some(),
            }
        } else {
            PromoteMark::Unset
        };
        let cut = root.switching() && self.property(&root.name, CUT_PROPERTY).await?.is_some();
        Ok(RootMarks { cut, promote })
    }

    /// Every snapshot of a dataset as `name<TAB>guid<TAB>creation`, newest first by
    /// transaction group (creation time ties within a second).
    async fn list_snapshots(&self, dataset: &str) -> Result<String> {
        self.zfs(&[
            "list",
            "-Hp",
            "-t",
            "snapshot",
            "-o",
            "name,guid,creation",
            "-S",
            "createtxg",
            "-d",
            "1",
            dataset,
        ])
        .await
    }

    /// The full `<dataset>@<name>` of every snapshot, whatever its name.
    pub(super) async fn snapshot_names(&self, dataset: &str) -> Result<Vec<String>> {
        Ok(self
            .list_snapshots(dataset)
            .await?
            .lines()
            .filter_map(|line| line.split('\t').next())
            .map(str::to_owned)
            .collect())
    }

    /// The run snapshots of a dataset, newest first. Snapshots with other names are not
    /// the run's and are left out.
    pub(super) async fn snapshots(&self, dataset: &str) -> Result<Vec<Snapshot>> {
        let output = self.list_snapshots(dataset).await?;
        let mut snapshots = Vec::new();
        for line in output.lines() {
            let invalid = || VolumeError::from(format!("invalid ZFS snapshot output: {line}"));
            let mut fields = line.split('\t');
            let (Some(full_name), Some(guid), Some(created), None) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                return Err(invalid());
            };
            let Some(name) = full_name
                .split_once('@')
                .and_then(|(_, name)| name.parse::<SnapshotName>().ok())
            else {
                continue;
            };
            snapshots.push(Snapshot {
                name,
                guid: SnapshotGuid::new(guid.parse().map_err(|_| invalid())?),
                created_unix_seconds: created.parse().map_err(|_| invalid())?,
            });
        }
        Ok(snapshots)
    }

    async fn newest_snapshot(&self, dataset: &str) -> Result<Option<Snapshot>> {
        Ok(self.snapshots(dataset).await?.into_iter().next())
    }

    /// The run of writes a root belongs to; a root SetVolumeCopy never numbered reads 0.
    pub(super) async fn epoch(&self, root: &str) -> Result<WriterEpoch> {
        match self.property(root, EPOCH_PROPERTY).await? {
            None => Ok(WriterEpoch::default()),
            Some(value) => value.parse().map(WriterEpoch::new).map_err(|error| {
                VolumeError::from(format!(
                    "{root} carries an unreadable {EPOCH_PROPERTY} {value:?}: {error}"
                ))
            }),
        }
    }

    /// What this Machine recorded a slot parent as.
    pub(super) async fn slot_role(&self, parent: &str) -> Result<SlotRole> {
        match self.property(parent, ROLE_PROPERTY).await? {
            None => Ok(SlotRole::default()),
            Some(value) => value.parse().map_err(|error| {
                VolumeError::from(format!(
                    "{parent} carries an unreadable {ROLE_PROPERTY} {value:?}: {error}"
                ))
            }),
        }
    }

    pub(super) async fn mark_slot_role(&self, parent: &str, role: SlotRole) -> Result<()> {
        self.zfs(&["set", &format!("{ROLE_PROPERTY}={}", role.as_str()), parent])
            .await
            .map(drop)
    }

    async fn slot_id(&self, parent: &str) -> Result<SlotId> {
        let value = self.property(parent, "guid").await?.unwrap_or_default();
        value.parse().map(SlotId::new).map_err(|error| {
            VolumeError::from(format!(
                "{parent} carries an unreadable guid {value:?}: {error}"
            ))
        })
    }

    /// The target a slot parent's `ployz:pull` names.
    pub(super) async fn pull_target(&self, parent: &str) -> Result<Option<SnapshotName>> {
        let Some(target) = self.property(parent, PULL_PROPERTY).await? else {
            return Ok(None);
        };
        target.parse().map(Some).map_err(|error| {
            VolumeError::from(format!(
                "{parent} carries an unreadable {PULL_PROPERTY} {target:?}: {error}"
            ))
        })
    }

    /// The SetVolumeCopy pull a slot names, with the failure of its own target's receive,
    /// and what attends it.
    async fn pull(
        &self,
        slot: &Dataset,
        name: &DockerVolumeName,
        newest: Option<&Snapshot>,
        resumable: bool,
    ) -> Result<Option<(Pull, PullState)>> {
        let Some(target) = self.pull_target(&slot.name).await? else {
            return Ok(None);
        };
        let status = self.receives.status_of(name, &target);
        let pull = Pull {
            target: target.clone(),
            failure: match &status {
                Status::Failed { failure, .. } => Some(failure.clone()),
                Status::Idle | Status::Receiving => None,
            },
        };
        let state = match status {
            Status::Receiving => PullState::Receiving { target },
            Status::Failed { failure, .. } => PullState::Failed { target, failure },
            // The landed check copy_pull makes before it starts a receive.
            Status::Idle if newest.is_some_and(|newest| newest.name == target) => {
                PullState::Landed { target }
            }
            Status::Idle => PullState::Unattended { target, resumable },
        };
        Ok(Some((pull, state)))
    }

    pub(super) async fn copy(
        &self,
        datasets: &[Dataset],
        pool: &MachinePool,
        name: &DockerVolumeName,
    ) -> Result<Option<VolumeCopy>> {
        if let Some(root) = Self::dataset(datasets, pool, name)? {
            root.require_provisioned(name)?;
            let copy = RootCopy::parse(root, self.root_marks(root).await?);
            return Ok(Some(copy.view(
                self.newest_snapshot(&root.name).await?,
                self.epoch(&root.name).await?,
            )));
        }
        let Some(slot) = Self::slot(datasets, pool, name) else {
            return Ok(None);
        };
        let fs = Self::slot_fs(datasets, pool, name);
        let (readonly, newest, resume_token) = match fs {
            Some(fs) => (
                fs.readonly,
                self.newest_snapshot(&fs.name).await?,
                self.property(&fs.name, RESUME_TOKEN_PROPERTY).await?,
            ),
            None => (slot.readonly, None, None),
        };
        let (pull, pull_state) = self
            .pull(slot, name, newest.as_ref(), resume_token.is_some())
            .await?
            .unzip();
        Ok(Some(VolumeCopy::Slot {
            slot: self.slot_id(&slot.name).await?,
            readonly,
            newest,
            resume_token,
            pull,
            role: Some(self.slot_role(&slot.name).await?),
            pull_state,
        }))
    }

    /// The slot parent `<pool>/ployz-mirror/<name>` when this Machine mirrors `name`.
    pub(super) fn slot<'datasets>(
        datasets: &'datasets [Dataset],
        pool: &MachinePool,
        name: &DockerVolumeName,
    ) -> Option<&'datasets Dataset> {
        let parent = slot_parent(pool, name);
        datasets.iter().find(|dataset| dataset.name == parent)
    }

    /// The received copy `<pool>/ployz-mirror/<name>/fs`, present once a receive began.
    pub(super) fn slot_fs<'datasets>(
        datasets: &'datasets [Dataset],
        pool: &MachinePool,
        name: &DockerVolumeName,
    ) -> Option<&'datasets Dataset> {
        let fs = slot_fs(pool, name);
        datasets.iter().find(|dataset| dataset.name == fs)
    }

    /// The copy as one moment saw it. A receive task finishing between the reads would
    /// mix the slot before and after it, so those reads are taken again; a finished task
    /// and its children change nothing more, and the caller's mutation keeps every route
    /// out.
    pub(super) async fn current_copy(
        &self,
        pool: &MachinePool,
        name: &DockerVolumeName,
    ) -> Result<Option<VolumeCopy>> {
        let busy = self.receives.status(name).busy();
        let copy = self.copy(&self.datasets(pool).await?, pool, name).await?;
        if busy && !self.receives.status(name).busy() {
            return self.copy(&self.datasets(pool).await?, pool, name).await;
        }
        Ok(copy)
    }

    async fn inspect_copy(&self, name: &DockerVolumeName) -> Result<VolumeCopyView> {
        let _guard = self.mutation.lock().await;
        let Some(pool) = self.pool.one_usable().await? else {
            return Ok(VolumeCopyView { copy: None });
        };
        Ok(VolumeCopyView {
            copy: self.current_copy(&pool, name).await?,
        })
    }
}

pub(super) async fn inspect(
    State(storage): State<VolumeStorage>,
    Json(request): Json<InspectVolumeCopyRequest>,
) -> Json<std::result::Result<VolumeCopyView, RpcError>> {
    let result = match request.name.as_str().parse::<DockerVolumeName>() {
        Ok(name) => storage.inspect_copy(&name).await.map_err(internal),
        Err(error) => Err(internal(error)),
    };
    Json(result)
}
