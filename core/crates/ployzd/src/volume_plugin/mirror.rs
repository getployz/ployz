//! Slot helpers the copy and seed routes share: the mirror root, slot naming and the
//! snapshot cleanup a slot or root needs.

use ployz_core::{MirrorMarker, RpcError, Snapshot, Switch, SwitchError};
use ployzd::machine_pool::MachinePool;

use super::{
    Dataset, DockerVolumeName, MIRROR_ROOT, VolumeStorage,
    lease::{Admitted, MIRROR_PROPERTY, internal},
    storage::{HeldMutation, root_dataset},
};

pub(super) const MIRROR_MOUNT_ROOT: &str = "/var/lib/ployz-mirror";

/// A leased verb's admitted scope: the lock, the Pool and the datasets it read under it.
pub(super) struct Leased {
    _guard: (HeldMutation, ployzd::mutation::MutationGuard),
    pub(super) pool: MachinePool,
    pub(super) datasets: Vec<Dataset>,
    pub(super) admitted: Admitted,
}

impl Leased {
    pub(super) fn slot(&self, name: &DockerVolumeName) -> Option<&Dataset> {
        VolumeStorage::slot(&self.datasets, &self.pool, name)
    }

    pub(super) fn fs(&self, name: &DockerVolumeName) -> Option<&Dataset> {
        VolumeStorage::slot_fs(&self.datasets, &self.pool, name)
    }

    pub(super) fn require_root(&self, name: &DockerVolumeName) -> Result<String, RpcError> {
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

    pub(super) async fn destroy_snapshots(
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
