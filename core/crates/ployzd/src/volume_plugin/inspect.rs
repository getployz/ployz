//! What this Machine holds for one Volume name: its root or slot and its newest snapshot.

use axum::{Json, extract::State};
use ployz_core::{
    InspectVolumeCopyRequest, Pull, RpcError, RpcErrorCode, SlotId, Snapshot, SnapshotGuid,
    SnapshotName, VolumeCopy, VolumeCopyView, WriterEpoch,
};
use ployzd::machine_pool::MachinePool;

use super::{
    Dataset, DockerVolumeName, Result, VolumeError, VolumeStorage,
    seed::SEED_PROPERTY,
    storage::{
        EPOCH_PROPERTY, PROMOTE_PROPERTY, PULL_PROPERTY, RESUME_TOKEN_PROPERTY, slot_fs,
        slot_parent,
    },
};

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

    pub(super) async fn unregistered(&self, dataset: &str) -> Result<bool> {
        Ok(self.property(dataset, PROMOTE_PROPERTY).await?.is_some())
    }

    pub(super) async fn unadopted_seed(&self, dataset: &str) -> Result<bool> {
        Ok(self.unregistered(dataset).await?
            && self.property(dataset, SEED_PROPERTY).await?.is_some())
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

    async fn slot_id(&self, parent: &str) -> Result<SlotId> {
        let value = self.property(parent, "guid").await?.unwrap_or_default();
        value.parse().map(SlotId::new).map_err(|error| {
            VolumeError::from(format!(
                "{parent} carries an unreadable guid {value:?}: {error}"
            ))
        })
    }

    /// The SetVolumeCopy pull in flight on a slot, with the failure its last receive hit.
    pub(super) async fn pull(
        &self,
        slot: &Dataset,
        name: &DockerVolumeName,
    ) -> Result<Option<Pull>> {
        let Some(target) = self.property(&slot.name, PULL_PROPERTY).await? else {
            return Ok(None);
        };
        let target = target.parse().map_err(|error| {
            VolumeError::from(format!(
                "{} carries an unreadable {PULL_PROPERTY} {target:?}: {error}",
                slot.name
            ))
        })?;
        Ok(Some(Pull {
            target,
            failure: self.receives.failure(name),
        }))
    }

    pub(super) async fn copy(
        &self,
        datasets: &[Dataset],
        pool: &MachinePool,
        name: &DockerVolumeName,
    ) -> Result<Option<VolumeCopy>> {
        if let Some(root) = Self::dataset(datasets, pool, name)? {
            root.require_provisioned(name)?;
            return Ok(Some(VolumeCopy::Root {
                readonly: root.readonly,
                newest: self.newest_snapshot(&root.name).await?,
                epoch: self.epoch(&root.name).await?,
                seed: self.unadopted_seed(&root.name).await?,
            }));
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
        Ok(Some(VolumeCopy::Slot {
            slot: self.slot_id(&slot.name).await?,
            readonly,
            newest,
            resume_token,
            pull: self.pull(slot, name).await?,
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

    async fn inspect_copy(&self, name: &DockerVolumeName) -> Result<VolumeCopyView> {
        let _guard = self.mutation.lock().await;
        let Some(pool) = self.pool.one_usable().await? else {
            return Ok(VolumeCopyView { copy: None });
        };
        let datasets = self.datasets(&pool).await?;
        Ok(VolumeCopyView {
            copy: self.copy(&datasets, &pool, name).await?,
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
