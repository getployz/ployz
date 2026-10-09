//! The per-Volume lease record on the managed root and the fence every switch verb passes.

use axum::{Json, extract::State};
use ployz_core::{
    Cycle, FenceDecision, InspectVolumeCopyRequest, LeaseRecord, MirrorMarker, RpcError,
    RpcErrorCode, Snapshot, SnapshotGuid, SnapshotName, Switch, SwitchError, SwitchReply,
    VolumeCopy, VolumeCopyView, WriterMarker,
};
use ployzd::machine_pool::MachinePool;

use super::{
    DATASET_ROOT, Dataset, DockerVolumeName, MIRROR_ROOT, Result, VolumeError, VolumeStorage,
};

pub(super) const LEASE_PROPERTY_PREFIX: &str = "ployz:lease.";
pub(super) const WRITER_PROPERTY: &str = "ployz:writer";
pub(super) const MIRROR_PROPERTY: &str = "ployz:mirror";
/// Set on a root renamed into place until Docker's Create registers it with its labels.
/// Get and List hide a marked root, so Docker never records it without them.
pub(super) const PROMOTE_PROPERTY: &str = "ployz:promote";
/// Set by ZFS on a dataset whose `receive -s` was interrupted; `-` otherwise.
pub(super) const RESUME_TOKEN_PROPERTY: &str = "receive_resume_token";

/// `ployz:lease.<name>`, each uppercase letter written as `:` and its lowercase. ZFS refuses
/// uppercase in a user property name, and a Docker Volume name never holds `:`, so the
/// escape keeps `Data` and `data` apart.
fn lease_property(name: &DockerVolumeName) -> String {
    let mut property = LEASE_PROPERTY_PREFIX.to_owned();
    for character in name.0.chars() {
        if character.is_ascii_uppercase() {
            property.push(':');
        }
        property.push(character.to_ascii_lowercase());
    }
    property
}

pub(super) fn fence(recorded: Option<LeaseRecord>, request: &Switch) -> FenceDecision {
    let Some(recorded) = recorded else {
        return FenceDecision::Adopt;
    };
    if request.lease < recorded.lease {
        return FenceDecision::RefuseStaleLease;
    }
    if request.lease > recorded.lease {
        return FenceDecision::Adopt;
    }
    match request.pos.cmp(&recorded.pos) {
        std::cmp::Ordering::Less => FenceDecision::RefuseStaleStep,
        std::cmp::Ordering::Equal => FenceDecision::Replay,
        std::cmp::Ordering::Greater => FenceDecision::Admit,
    }
}

pub(super) fn refusal(
    decision: FenceDecision,
    request: &Switch,
    recorded: Option<LeaseRecord>,
) -> Option<RpcError> {
    let recorded = recorded.map_or_else(|| "no record".to_owned(), |record| record.to_string());
    match decision {
        FenceDecision::Admit | FenceDecision::Replay | FenceDecision::Adopt => None,
        FenceDecision::RefuseStaleLease => Some(SwitchError::StaleLease.rpc_error(format!(
            "lease {} is older than this Machine's record {recorded}",
            request.lease
        ))),
        FenceDecision::RefuseStaleStep => Some(SwitchError::StaleStep.rpc_error(format!(
            "step {} of lease {} is behind this Machine's record {recorded}",
            request.pos, request.lease
        ))),
    }
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

    pub(super) async fn lease_record(
        &self,
        datasets: &[Dataset],
        pool: &MachinePool,
        name: &DockerVolumeName,
    ) -> Result<Option<LeaseRecord>> {
        let root = format!("{}/{DATASET_ROOT}", pool.name());
        if !datasets.iter().any(|dataset| dataset.name == root) {
            return Ok(None);
        }
        let Some(value) = self.property(&root, &lease_property(name)).await? else {
            return Ok(None);
        };
        value
            .parse::<LeaseRecord>()
            .map(Some)
            .map_err(|error| format!("{error} on {root}").into())
    }

    async fn write_lease_record(
        &self,
        datasets: &[Dataset],
        pool: &MachinePool,
        name: &DockerVolumeName,
        record: LeaseRecord,
    ) -> Result<()> {
        let root = format!("{}/{DATASET_ROOT}", pool.name());
        if !datasets.iter().any(|dataset| dataset.name == root) {
            self.create_root(&root).await?;
        }
        self.zfs(&["set", &format!("{}={record}", lease_property(name)), &root])
            .await?;
        Ok(())
    }

    /// Removes `name`'s record from `<pool>/ployz`, once the Machine holds no copy of it.
    pub(super) async fn clear_lease_record(
        &self,
        pool: &MachinePool,
        name: &DockerVolumeName,
    ) -> Result<()> {
        let root = format!("{}/{DATASET_ROOT}", pool.name());
        self.zfs(&["inherit", &lease_property(name), &root]).await?;
        Ok(())
    }

    pub(super) async fn unregistered(&self, dataset: &str) -> Result<bool> {
        Ok(self.property(dataset, PROMOTE_PROPERTY).await?.is_some())
    }

    pub(super) async fn writer_marker(&self, root: &Dataset) -> Result<WriterMarker> {
        match self.property(&root.name, WRITER_PROPERTY).await? {
            None => Ok(WriterMarker::Idle),
            Some(value) => value
                .parse()
                .map_err(|error| format!("{error} on {}", root.name).into()),
        }
    }

    pub(super) async fn mirror_marker(&self, slot: &Dataset) -> Result<MirrorMarker> {
        match self.property(&slot.name, MIRROR_PROPERTY).await? {
            None => Ok(MirrorMarker::Idle),
            Some(value) => value
                .parse()
                .map_err(|error| format!("{error} on {}", slot.name).into()),
        }
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

    pub(super) async fn copy(
        &self,
        datasets: &[Dataset],
        pool: &MachinePool,
        name: &DockerVolumeName,
    ) -> Result<Option<VolumeCopy>> {
        if let Some(root) = Self::dataset(datasets, pool, name)? {
            root.require_provisioned(name)?;
            return Ok(Some(VolumeCopy::Root {
                writer: self.writer_marker(root).await?,
                readonly: root.readonly,
                newest: self.newest_snapshot(&root.name).await?,
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
            mirror: self.mirror_marker(slot).await?,
            readonly,
            newest,
            resume_token,
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

    /// Fences `request` for `name`. The caller holds the mutation lock and calls
    /// [`Self::record`] once its preconditions pass, so a refusal leaves the record as it was.
    pub(super) async fn admit(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        name: &DockerVolumeName,
        request: &Switch,
    ) -> std::result::Result<Admitted, RpcError> {
        let recorded = self
            .lease_record(datasets, pool, name)
            .await
            .map_err(internal)?;
        let decision = fence(recorded, request);
        if let Some(error) = refusal(decision, request, recorded) {
            return Err(error);
        }
        let lease = LeaseRecord {
            lease: request.lease,
            pos: request.pos,
            cycle: recorded.map_or(Cycle::Closed, |record| record.cycle),
        };
        Ok(Admitted {
            decision,
            lease,
            recorded,
        })
    }

    /// Records the admitted position as the effect begins; a no-op once recorded.
    pub(super) async fn record(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        name: &DockerVolumeName,
        admitted: &mut Admitted,
    ) -> std::result::Result<(), RpcError> {
        if admitted.recorded != Some(admitted.lease) {
            self.write_lease_record(datasets, pool, name, admitted.lease)
                .await
                .map_err(internal)?;
            admitted.recorded = Some(admitted.lease);
        }
        Ok(())
    }

    /// The reply every leased verb ends with: the decision, the record and the copy as
    /// it is after the effect. Records the position if the verb had no effect to record it.
    pub(super) async fn reply(
        &self,
        pool: &MachinePool,
        name: &DockerVolumeName,
        mut admitted: Admitted,
    ) -> std::result::Result<SwitchReply, RpcError> {
        let datasets = self.datasets(pool).await.map_err(internal)?;
        self.record(pool, &datasets, name, &mut admitted).await?;
        Ok(SwitchReply {
            decision: admitted.decision,
            lease: admitted.lease,
            copy: self.copy(&datasets, pool, name).await.map_err(internal)?,
        })
    }

    async fn inspect_copy(&self, name: &DockerVolumeName) -> Result<VolumeCopyView> {
        let _guard = self.mutation.lock().await;
        let Some(pool) = self.pool.one_usable().await? else {
            return Ok(VolumeCopyView {
                copy: None,
                lease: None,
            });
        };
        let datasets = self.datasets(&pool).await?;
        Ok(VolumeCopyView {
            copy: self.copy(&datasets, &pool, name).await?,
            lease: self.lease_record(&datasets, &pool, name).await?,
        })
    }
}

/// A fenced request's decision, the record it writes, and the record on disk.
#[derive(Clone, Copy, Debug)]
pub(super) struct Admitted {
    pub(super) decision: FenceDecision,
    pub(super) lease: LeaseRecord,
    pub(super) recorded: Option<LeaseRecord>,
}

pub(super) fn slot_parent(pool: &MachinePool, name: &DockerVolumeName) -> String {
    format!("{}/{MIRROR_ROOT}/{name}", pool.name())
}

pub(super) fn slot_fs(pool: &MachinePool, name: &DockerVolumeName) -> String {
    format!("{}/fs", slot_parent(pool, name))
}

pub(super) fn root_dataset(pool: &MachinePool, name: &DockerVolumeName) -> String {
    format!("{}/{DATASET_ROOT}/{name}", pool.name())
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

#[cfg(test)]
mod tests {
    use ployz_core::{Lease, Pos};

    use super::*;

    fn record(lease: u64, pos: Pos, cycle: Cycle) -> LeaseRecord {
        LeaseRecord {
            lease: Lease::new(lease),
            pos,
            cycle,
        }
    }

    fn pos(seq: u16, round: u32, sub: u8) -> Pos {
        Pos { seq, round, sub }
    }

    #[test]
    fn lease_fence_table() {
        let recorded = Some(record(5, pos(4, 2, 5), Cycle::Closed));
        let departed = Some(record(5, Pos::DEPARTED, Cycle::Closed));
        let rows = [
            (recorded, 5, pos(5, 0, 0), FenceDecision::Admit),
            (recorded, 5, pos(4, 2, 5), FenceDecision::Replay),
            (recorded, 6, pos(2, 0, 0), FenceDecision::Adopt),
            (None, 1, pos(2, 0, 0), FenceDecision::Adopt),
            (recorded, 4, pos(9, 0, 0), FenceDecision::RefuseStaleLease),
            (recorded, 5, pos(4, 2, 1), FenceDecision::RefuseStaleStep),
            (recorded, 5, pos(4, 1, 9), FenceDecision::RefuseStaleStep),
            (departed, 5, pos(9, 0, 0), FenceDecision::RefuseStaleStep),
            (departed, 6, pos(3, 0, 0), FenceDecision::Adopt),
        ];
        for (recorded, lease, pos, expected) in rows {
            let request = Switch {
                lease: Lease::new(lease),
                pos,
            };
            assert_eq!(
                fence(recorded, &request),
                expected,
                "record {recorded:?}, request {request:?}"
            );
        }
    }

    #[test]
    fn refusals_name_their_reason() {
        let request = Switch {
            lease: Lease::new(3),
            pos: Pos::step(2),
        };
        let recorded = Some(record(4, Pos::step(2), Cycle::Closed));
        let error = refusal(FenceDecision::RefuseStaleLease, &request, recorded).unwrap();
        assert_eq!(
            SwitchError::from_details(&error.details),
            Some(SwitchError::StaleLease)
        );
        assert!(
            error.message.contains("4:2.0.0:closed"),
            "{}",
            error.message
        );
        for admitted in [
            FenceDecision::Admit,
            FenceDecision::Replay,
            FenceDecision::Adopt,
        ] {
            assert!(refusal(admitted, &request, recorded).is_none());
        }
    }
}
