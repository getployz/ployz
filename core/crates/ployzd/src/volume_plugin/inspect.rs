//! What this Machine holds for one Volume name: its root or slot and its newest snapshot.

use crate::{dataset_layout::DatasetLayout, machine_pool::MachinePool};
use axum::{Json, extract::State};
use ployz_core::{
    InspectVolumeCopyRequest, Pull, PullState, RpcError, SlotId, Snapshot, SnapshotGuid,
    SnapshotName, VolumeCopy, VolumeCopyView,
};

use ployz_decide::volume_root::{PromoteMark, RootCopy, RootMarks};

use super::{
    Dataset, PluginError, Result, VolumeName, VolumeStorage, marks::Mark, transfer::Status,
};

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

/// Every snapshot of `dataset` as `name<TAB>guid<TAB>creation`, newest first by
/// transaction group (creation time ties within a second).
fn snapshot_listing(dataset: &str) -> [&str; 11] {
    [
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
    ]
}

impl VolumeStorage {
    /// `copy` as a Seed's reply shows it: with no failure its decision did not answer. A
    /// pull that failed reads as still receiving, as it did when the decision found it, and
    /// the Seed's next resend answers it. While a child the failed task dropped is still
    /// alive, no Seed answers the failure, so the reply shows it as receiving with that
    /// failure. Read after `copy`: a child that exited since then shows no failure here.
    pub(super) fn receiving(
        &self,
        name: &VolumeName,
        mut copy: Option<VolumeCopy>,
    ) -> Option<VolumeCopy> {
        if let Some(VolumeCopy::Slot {
            pull: Some(pull),
            pull_state,
            ..
        }) = &mut copy
        {
            let lingering = match self.receives.status_of(name, &pull.target) {
                Status::Failed {
                    failure,
                    settled: false,
                } => Some(failure),
                Status::Idle | Status::Receiving | Status::Failed { .. } => None,
            };
            if pull.failure.take().is_some() || lingering.is_some() {
                *pull_state = Some(PullState::Receiving {
                    target: pull.target.clone(),
                    failure: lingering,
                });
            }
        }
        copy
    }

    /// A dataset's user property, or `None` when ZFS reports it unset (`-`).
    pub(super) async fn property(&self, dataset: &str, property: &str) -> Result<Option<String>> {
        let value = self
            .zfs(&["get", "-H", "-o", "value", property, dataset])
            .await?;
        let value = value.trim_end_matches('\n');
        Ok((value != "-").then(|| value.to_owned()))
    }

    /// A dataset's property as set on the dataset itself, or `None` when it inherits it
    /// or nothing sets it.
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

    async fn root_marks(&self, root: &Dataset) -> Result<RootMarks> {
        let marks = self.marks(&root.name);
        let promote = if marks.unregistered().await? {
            PromoteMark::Set {
                seed: marks.is_set(Mark::Seed).await?,
            }
        } else {
            PromoteMark::Unset
        };
        let cut = root.switching() && marks.is_set(Mark::Cut).await?;
        Ok(RootMarks { cut, promote })
    }

    /// Every snapshot of `dataset` from a listing ZFS finished. Whatever reads it judges a
    /// snapshot missing from it absent, or the first one newest.
    async fn list_snapshots(&self, dataset: &str) -> Result<String> {
        self.zfs_listing(&snapshot_listing(dataset)).await
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
            let invalid = || PluginError::from(format!("invalid ZFS snapshot output: {line}"));
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

    pub(super) async fn newest_snapshot(&self, dataset: &str) -> Result<Option<Snapshot>> {
        Ok(self.snapshots(dataset).await?.into_iter().next())
    }

    async fn slot_id(&self, parent: &str) -> Result<SlotId> {
        let value = self.property(parent, "guid").await?.unwrap_or_default();
        value.parse().map(SlotId::new).map_err(|error| {
            PluginError::from(format!(
                "{parent} carries an unreadable guid {value:?}: {error}"
            ))
        })
    }

    /// The SetVolumeCopy pull a slot names, with the failure of its own target's receive,
    /// and what attends it.
    async fn pull(
        &self,
        slot: &Dataset,
        name: &VolumeName,
        newest: Option<&Snapshot>,
        resumable: bool,
    ) -> Result<Option<(Pull, PullState)>> {
        let Some(target) = self.marks(&slot.name).pull_target().await? else {
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
            Status::Receiving => PullState::Receiving {
                target,
                failure: None,
            },
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
        name: &VolumeName,
    ) -> Result<Option<VolumeCopy>> {
        if let Some(root) = Self::dataset(datasets, pool, name)? {
            root.require_provisioned(name)?;
            let copy = RootCopy::parse(root.switching(), self.root_marks(root).await?);
            let newest = self.newest_snapshot(&root.name).await?;
            let origin = self
                .origin_of(
                    None,
                    Some((&root.name, newest.as_ref().map(|newest| &newest.name))),
                )
                .await?;
            return Ok(Some(copy.view(
                newest,
                self.marks(&root.name).epoch().await?,
                origin,
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
                self.marks(&fs.name).get(Mark::ResumeToken).await?,
            ),
            None => (slot.readonly, None, None),
        };
        let (pull, pull_state) = self
            .pull(slot, name, newest.as_ref(), resume_token.is_some())
            .await?
            .unzip();
        let origin = self
            .origin_of(
                Some(&slot.name),
                fs.map(|fs| (fs.name.as_str(), newest.as_ref().map(|newest| &newest.name))),
            )
            .await?;
        Ok(Some(VolumeCopy::Slot {
            slot: self.slot_id(&slot.name).await?,
            readonly,
            newest,
            resume_token,
            pull,
            role: Some(self.marks(&slot.name).role().await?),
            pull_state,
            origin,
        }))
    }

    /// The slot parent `<pool>/ployz-mirror/<name>` when this Machine mirrors `name`.
    pub(super) fn slot<'datasets>(
        datasets: &'datasets [Dataset],
        pool: &MachinePool,
        name: &VolumeName,
    ) -> Option<&'datasets Dataset> {
        let parent = DatasetLayout::of(pool).slot_parent(name);
        datasets.iter().find(|dataset| dataset.name == parent)
    }

    /// The received copy `<pool>/ployz-mirror/<name>/fs`, present once a receive began.
    pub(super) fn slot_fs<'datasets>(
        datasets: &'datasets [Dataset],
        pool: &MachinePool,
        name: &VolumeName,
    ) -> Option<&'datasets Dataset> {
        let fs = DatasetLayout::of(pool).slot_fs(name);
        datasets.iter().find(|dataset| dataset.name == fs)
    }

    /// The copy as one moment saw it. A receive task finishing between the reads would
    /// mix the slot before and after it, so those reads are taken again; a finished task
    /// and its children change nothing more, and the caller's mutation keeps every route
    /// out.
    pub(super) async fn current_copy(
        &self,
        pool: &MachinePool,
        name: &VolumeName,
    ) -> Result<Option<VolumeCopy>> {
        // An answer of no copy is an absence, so it comes only from a listing ZFS finished.
        let busy = self.receives.status(name).busy();
        let copy = self
            .copy(&self.complete_datasets(pool).await?, pool, name)
            .await?;
        if busy && !self.receives.status(name).busy() {
            return self
                .copy(&self.complete_datasets(pool).await?, pool, name)
                .await;
        }
        Ok(copy)
    }

    async fn inspect_copy(&self, name: &VolumeName) -> Result<VolumeCopyView> {
        let _guard = self.mutation.lock().await;
        // A Pool this Machine held but can't read now may hold a copy, so it never answers none.
        let Some(pool) = self.pool.one_usable().await? else {
            if self.pool.has_held_a_pool()? {
                return Err(PluginError::HeldPoolNotImported);
            }
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
    let result = match request.name.as_str().parse::<VolumeName>() {
        Ok(name) => storage.inspect_copy(&name).await.map_err(RpcError::from),
        Err(error) => Err(error.into()),
    };
    Json(result)
}
