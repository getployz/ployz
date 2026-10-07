//! The mirror verbs: a slot's life from DeclareMirror to Destroy, and the writer's
//! snapshot retention between rounds.

use std::num::NonZeroU64;

use axum::{Json, extract::State};
use ployz_core::{
    CommitRequest, DeclareMirrorRequest, FenceDecision, MirrorMarker, MirrorRequest, RpcError,
    Snapshot, SnapshotName, Switch, SwitchError, SwitchReply, WarmRequest,
};
use ployzd::machine_pool::MachinePool;
use tokio::sync::OwnedMutexGuard;

use super::{
    Dataset, DockerVolumeName, MIRROR_ROOT, VolumeStorage,
    lease::{
        Admitted, MIRROR_PROPERTY, RESUME_TOKEN_PROPERTY, internal, root_dataset, slot_parent,
    },
    transfer::RECEIVE_PROPERTY,
};

pub(super) const MIRROR_MOUNT_ROOT: &str = "/var/lib/ployz-mirror";

/// A leased verb's admitted scope: the lock, the Pool and the datasets it read under it.
pub(super) struct Leased {
    _guard: (OwnedMutexGuard<()>, ployzd::mutation::MutationGuard),
    pub(super) pool: MachinePool,
    pub(super) datasets: Vec<Dataset>,
    pub(super) admitted: Admitted,
}

impl Leased {
    pub(super) fn replayed(&self) -> bool {
        self.admitted.decision == FenceDecision::Replay
    }

    pub(super) fn slot(&self, name: &DockerVolumeName) -> Option<&Dataset> {
        VolumeStorage::slot(&self.datasets, &self.pool, name)
    }

    pub(super) fn fs(&self, name: &DockerVolumeName) -> Option<&Dataset> {
        VolumeStorage::slot_fs(&self.datasets, &self.pool, name)
    }

    pub(super) fn require_slot(&self, name: &DockerVolumeName) -> Result<&Dataset, RpcError> {
        self.slot(name).ok_or_else(|| {
            SwitchError::Precondition.rpc_error(format!(
                "no mirror of Volume {name} is declared on this Machine"
            ))
        })
    }

    fn require_root(&self, name: &DockerVolumeName) -> Result<String, RpcError> {
        let root = root_dataset(&self.pool, name);
        self.datasets
            .iter()
            .any(|dataset| dataset.name == root)
            .then_some(root)
            .ok_or_else(|| {
                SwitchError::Precondition
                    .rpc_error(format!("Volume {name} has no writer on this Machine"))
            })
    }
}

pub(super) fn name(name: &ployz_core::DockerVolumeName) -> Result<DockerVolumeName, RpcError> {
    name.as_str().parse().map_err(internal)
}

impl VolumeStorage {
    /// Takes the lock and fences `switch`; every leased verb starts here.
    pub(super) async fn leased(
        &self,
        name: &DockerVolumeName,
        switch: &Switch,
    ) -> Result<Leased, RpcError> {
        let guard = self.admit_mutation().await.map_err(internal)?;
        let pool = self.one_pool().await.map_err(internal)?;
        let datasets = self.datasets(&pool).await.map_err(internal)?;
        let admitted = self.admit(&pool, &datasets, name, switch).await?;
        Ok(Leased {
            _guard: guard,
            pool,
            datasets,
            admitted,
        })
    }

    pub(super) fn require_no_receive(&self, name: &DockerVolumeName) -> Result<(), RpcError> {
        if self.receives.running(name) {
            return Err(SwitchError::Busy
                .rpc_error(format!("a receive into the mirror of {name} is running")));
        }
        Ok(())
    }

    async fn declare_mirror(
        &self,
        request: &DeclareMirrorRequest,
    ) -> Result<SwitchReply, RpcError> {
        let bound = NonZeroU64::new(request.refquota_bytes).ok_or_else(|| {
            SwitchError::Precondition.rpc_error("a mirror's bound must be nonzero")
        })?;
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        if scope.slot(&name).is_some() {
            if scope.replayed() {
                return self.reply(&scope.pool, &name, scope.admitted).await;
            }
            return Err(SwitchError::Precondition.rpc_error(format!(
                "a mirror of Volume {name} is already declared here"
            )));
        }
        if scope.require_root(&name).is_ok() {
            return Err(SwitchError::Precondition.rpc_error(format!(
                "Volume {name} has its writer on this Machine; a mirror goes elsewhere"
            )));
        }
        self.ensure_commitment(&scope.pool, &scope.datasets, bound.get())
            .await
            .map_err(internal)?;
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        self.ensure_mirror_root(&scope.pool, &scope.datasets)
            .await
            .map_err(internal)?;
        self.create_slot_parent(&slot_parent(&scope.pool, &name), bound.get())
            .await
            .map_err(internal)?;
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    /// `<pool>/ployz-mirror`, the read-only parent every slot inherits from.
    pub(super) async fn ensure_mirror_root(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
    ) -> super::Result<()> {
        let mirror_root = format!("{}/{MIRROR_ROOT}", pool.name());
        if datasets.iter().any(|dataset| dataset.name == mirror_root) {
            return Ok(());
        }
        self.zfs(&[
            "create",
            "-o",
            "canmount=off",
            "-o",
            &format!("mountpoint={MIRROR_MOUNT_ROOT}"),
            "-o",
            "readonly=on",
            &mirror_root,
        ])
        .await?;
        Ok(())
    }

    /// A slot parent bounded by `refquota_bytes`, marked idle.
    pub(super) async fn create_slot_parent(
        &self,
        parent: &str,
        refquota_bytes: u64,
    ) -> super::Result<()> {
        self.zfs(&[
            "create",
            "-o",
            "canmount=off",
            "-o",
            "readonly=on",
            "-o",
            &format!("refquota={refquota_bytes}"),
            parent,
        ])
        .await?;
        self.zfs(&[
            "set",
            &format!("{MIRROR_PROPERTY}={}", MirrorMarker::Idle),
            parent,
        ])
        .await?;
        Ok(())
    }

    async fn begin_round(&self, request: &MirrorRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        scope.require_slot(&name)?;
        self.require_no_receive(&name)?;
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        let slot = scope.require_slot(&name)?;
        if let Some(fs) = scope.fs(&name)
            && self
                .property(&fs.name, RESUME_TOKEN_PROPERTY)
                .await
                .map_err(internal)?
                .is_some()
        {
            self.zfs(&["receive", "-A", &fs.name])
                .await
                .map_err(internal)?;
        }
        if self
            .property(&slot.name, RECEIVE_PROPERTY)
            .await
            .map_err(internal)?
            .is_some()
        {
            self.zfs(&["inherit", RECEIVE_PROPERTY, &slot.name])
                .await
                .map_err(internal)?;
        }
        // The run reaches a round only once the writer is idle again, so a final
        // snapshot the writer still holds is an ordinary synced mirror.
        if let MirrorMarker::Final { .. } = self.mirror_marker(slot).await.map_err(internal)? {
            self.zfs(&[
                "set",
                &format!("{MIRROR_PROPERTY}={}", MirrorMarker::Idle),
                &slot.name,
            ])
            .await
            .map_err(internal)?;
        }
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    async fn commit_snapshots(&self, request: &CommitRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        let root = scope.require_root(&name)?;
        let snapshots = self.snapshots(&root).await.map_err(internal)?;
        let Some(held) = snapshots
            .iter()
            .position(|snapshot| snapshot.guid == request.mirror_newest)
        else {
            return Err(SwitchError::Precondition.rpc_error(format!(
                "snapshot {} is not on {root}; the mirror's newest must come from this writer",
                request.mirror_newest
            )));
        };
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        self.destroy_snapshots(&root, snapshots.get(held + 1..).unwrap_or(&[]))
            .await?;
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    async fn warm_snapshot(&self, request: &WarmRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        let root = scope.require_root(&name)?;
        // Named for its round, so a replay finds the snapshot it took in ZFS, and takes it
        // when the plugin died after recording and before `zfs snapshot`.
        let target = SnapshotName::warm(request.switch.lease, request.switch.pos.round);
        let snapshots = self.snapshots(&root).await.map_err(internal)?;
        if snapshots.iter().any(|snapshot| snapshot.name == target) {
            return self.reply(&scope.pool, &name, scope.admitted).await;
        }
        let captured = match snapshots.first() {
            Some(newest) => {
                let written = self
                    .zfs(&[
                        "get",
                        "-Hp",
                        "-o",
                        "value",
                        &format!("written@{}", newest.name),
                        &root,
                    ])
                    .await
                    .map_err(internal)?;
                written.trim() == "0"
            }
            None => false,
        };
        if !captured {
            self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
                .await?;
            ployzd::faults::kill_after_record("WarmSnapshot");
            self.zfs(&["snapshot", &format!("{root}@{target}")])
                .await
                .map_err(internal)?;
        }
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    async fn prune_mirror(&self, request: &MirrorRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        let slot = scope.require_slot(&name)?;
        self.require_no_receive(&name)?;
        if let Some(fs) = scope.fs(&name).map(|fs| fs.name.clone()) {
            let in_flight = self.receive_record(slot).await?.map(|record| record.target);
            let snapshots = self.snapshots(&fs).await.map_err(internal)?;
            let stale: Vec<Snapshot> = snapshots
                .iter()
                .skip(1)
                .filter(|snapshot| Some(&snapshot.name) != in_flight.as_ref())
                .cloned()
                .collect();
            self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
                .await?;
            self.destroy_snapshots(&fs, &stale).await?;
        }
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    async fn destroy_mirror(&self, request: &MirrorRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        if let Some(slot) = scope.slot(&name).map(|slot| slot.name.clone()) {
            self.require_no_receive(&name)?;
            self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
                .await?;
            self.zfs(&["destroy", "-r", &slot])
                .await
                .map_err(internal)?;
        }
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    async fn forget_snapshots(&self, request: &MirrorRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        let root = scope.require_root(&name)?;
        let snapshots = self.snapshots(&root).await.map_err(internal)?;
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        self.destroy_snapshots(&root, &snapshots).await?;
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    async fn destroy_snapshots(
        &self,
        dataset: &str,
        snapshots: &[Snapshot],
    ) -> Result<(), RpcError> {
        for snapshot in snapshots {
            self.zfs(&["destroy", &format!("{dataset}@{}", snapshot.name)])
                .await
                .map_err(internal)?;
        }
        Ok(())
    }
}

pub(super) async fn declare(
    State(storage): State<VolumeStorage>,
    Json(request): Json<DeclareMirrorRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.declare_mirror(&request).await)
}

pub(super) async fn begin_round(
    State(storage): State<VolumeStorage>,
    Json(request): Json<MirrorRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.begin_round(&request).await)
}

pub(super) async fn commit(
    State(storage): State<VolumeStorage>,
    Json(request): Json<CommitRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.commit_snapshots(&request).await)
}

pub(super) async fn warm(
    State(storage): State<VolumeStorage>,
    Json(request): Json<WarmRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.warm_snapshot(&request).await)
}

pub(super) async fn prune(
    State(storage): State<VolumeStorage>,
    Json(request): Json<MirrorRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.prune_mirror(&request).await)
}

pub(super) async fn destroy(
    State(storage): State<VolumeStorage>,
    Json(request): Json<MirrorRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.destroy_mirror(&request).await)
}

pub(super) async fn forget(
    State(storage): State<VolumeStorage>,
    Json(request): Json<MirrorRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.forget_snapshots(&request).await)
}
