//! The per-Volume lease record on the managed root and the fence every switch verb passes.

use axum::{Json, extract::State};
use ployz_core::{
    AdoptLeaseRequest, Cycle, FenceDecision, InspectVolumeCopyRequest, LeaseRecord, MirrorMarker,
    RpcError, RpcErrorCode, Snapshot, SnapshotGuid, Switch, SwitchError, SwitchReply, VolumeCopy,
    VolumeCopyView, WriterMarker,
};
use ployzd::machine_pool::MachinePool;

use super::{DATASET_ROOT, Dataset, DockerVolumeName, Place, Result, VolumeError, VolumeStorage};

const LEASE_PROPERTY_PREFIX: &str = "ployz:lease.";
const WRITER_PROPERTY: &str = "ployz:writer";
const MIRROR_PROPERTY: &str = "ployz:mirror";

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

pub(super) fn fence(
    recorded: Option<LeaseRecord>,
    request: &Switch,
    now_unix_seconds: i64,
) -> FenceDecision {
    if now_unix_seconds > request.not_after_unix_seconds {
        return FenceDecision::RefuseExpired;
    }
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

fn refusal(
    decision: FenceDecision,
    request: &Switch,
    recorded: Option<LeaseRecord>,
    now_unix_seconds: i64,
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
        FenceDecision::RefuseExpired => {
            let skew_seconds = now_unix_seconds - request.not_after_unix_seconds;
            Some(SwitchError::Expired { skew_seconds }.rpc_error(format!(
                "request expired {skew_seconds} s ago on this Machine's clock"
            )))
        }
    }
}

fn internal(error: VolumeError) -> RpcError {
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
    async fn property(&self, dataset: &str, property: &str) -> Result<Option<String>> {
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

    pub(super) async fn writer_marker(&self, root: &Dataset) -> Result<WriterMarker> {
        match self.property(&root.name, WRITER_PROPERTY).await? {
            None => Ok(WriterMarker::Idle),
            Some(value) => value
                .parse()
                .map_err(|error| format!("{error} on {}", root.name).into()),
        }
    }

    async fn mirror_marker(&self, slot: &Dataset) -> Result<MirrorMarker> {
        match self.property(&slot.name, MIRROR_PROPERTY).await? {
            None => Ok(MirrorMarker::Idle),
            Some(value) => value
                .parse()
                .map_err(|error| format!("{error} on {}", slot.name).into()),
        }
    }

    async fn newest_snapshot(&self, dataset: &Dataset) -> Result<Option<Snapshot>> {
        let output = self
            .zfs(&[
                "list",
                "-Hp",
                "-t",
                "snapshot",
                "-o",
                "guid,creation",
                "-S",
                "creation",
                "-d",
                "1",
                &dataset.name,
            ])
            .await?;
        let Some(line) = output.lines().next() else {
            return Ok(None);
        };
        let invalid = || VolumeError::from(format!("invalid ZFS snapshot output: {line}"));
        let (guid, created) = line.split_once('\t').ok_or_else(invalid)?;
        Ok(Some(Snapshot {
            guid: SnapshotGuid::new(guid.parse().map_err(|_| invalid())?),
            created_unix_seconds: created.parse().map_err(|_| invalid())?,
        }))
    }

    async fn copy(
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
                newest: self.newest_snapshot(root).await?,
            }));
        }
        let slot = datasets.iter().find(|dataset| {
            matches!(Place::of(&dataset.name, pool.name()), Place::Slot(slot) if slot == name.0)
                && dataset.name.ends_with("/fs")
        });
        let Some(slot) = slot else {
            return Ok(None);
        };
        Ok(Some(VolumeCopy::Slot {
            mirror: self.mirror_marker(slot).await?,
            readonly: slot.readonly,
            newest: self.newest_snapshot(slot).await?,
        }))
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

    async fn adopt_lease(
        &self,
        name: &DockerVolumeName,
        request: &Switch,
    ) -> std::result::Result<SwitchReply, RpcError> {
        let _guard = self.admit_mutation().await.map_err(internal)?;
        let pool = self.one_pool().await.map_err(internal)?;
        let datasets = self.datasets(&pool).await.map_err(internal)?;
        let recorded = self
            .lease_record(&datasets, &pool, name)
            .await
            .map_err(internal)?;
        let now = chrono::Utc::now().timestamp();
        let decision = fence(recorded, request, now);
        if let Some(error) = refusal(decision, request, recorded, now) {
            return Err(error);
        }
        let lease = LeaseRecord {
            lease: request.lease,
            pos: request.pos,
            cycle: recorded.map_or(Cycle::Closed, |record| record.cycle),
        };
        if recorded != Some(lease) {
            self.write_lease_record(&datasets, &pool, name, lease)
                .await
                .map_err(internal)?;
        }
        Ok(SwitchReply {
            decision,
            lease,
            copy: self.copy(&datasets, &pool, name).await.map_err(internal)?,
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

pub(super) async fn adopt_lease(
    State(storage): State<VolumeStorage>,
    Json(request): Json<AdoptLeaseRequest>,
) -> Json<std::result::Result<SwitchReply, RpcError>> {
    let result = match request.name.as_str().parse::<DockerVolumeName>() {
        Ok(name) => storage.adopt_lease(&name, &request.switch()).await,
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
        let rows = [
            (recorded, 5, pos(5, 0, 0), 100, FenceDecision::Admit),
            (recorded, 5, pos(4, 2, 5), 100, FenceDecision::Replay),
            (recorded, 6, pos(2, 0, 0), 100, FenceDecision::Adopt),
            (None, 1, pos(2, 0, 0), 100, FenceDecision::Adopt),
            (
                recorded,
                4,
                pos(9, 0, 0),
                100,
                FenceDecision::RefuseStaleLease,
            ),
            (
                recorded,
                5,
                pos(4, 2, 1),
                100,
                FenceDecision::RefuseStaleStep,
            ),
            (
                recorded,
                5,
                pos(4, 1, 9),
                100,
                FenceDecision::RefuseStaleStep,
            ),
            (recorded, 6, pos(2, 0, 0), 101, FenceDecision::RefuseExpired),
            (recorded, 4, pos(1, 0, 0), 101, FenceDecision::RefuseExpired),
        ];
        for (recorded, lease, pos, now, expected) in rows {
            let request = Switch {
                lease: Lease::new(lease),
                pos,
                not_after_unix_seconds: 100,
            };
            assert_eq!(
                fence(recorded, &request, now),
                expected,
                "record {recorded:?}, request {request:?}, now {now}"
            );
        }
    }

    #[test]
    fn refusals_name_their_reason() {
        let request = Switch {
            lease: Lease::new(3),
            pos: Pos::ADOPT_LEASE,
            not_after_unix_seconds: 100,
        };
        let recorded = Some(record(4, Pos::ADOPT_LEASE, Cycle::Closed));
        let error = refusal(FenceDecision::RefuseStaleLease, &request, recorded, 50).unwrap();
        assert_eq!(
            SwitchError::from_details(&error.details),
            Some(SwitchError::StaleLease)
        );
        assert!(
            error.message.contains("4:2.0.0:closed"),
            "{}",
            error.message
        );
        let error = refusal(FenceDecision::RefuseExpired, &request, recorded, 130).unwrap();
        assert_eq!(
            SwitchError::from_details(&error.details),
            Some(SwitchError::Expired { skew_seconds: 30 })
        );
        for admitted in [
            FenceDecision::Admit,
            FenceDecision::Replay,
            FenceDecision::Adopt,
        ] {
            assert!(refusal(admitted, &request, recorded, 50).is_none());
        }
    }
}
