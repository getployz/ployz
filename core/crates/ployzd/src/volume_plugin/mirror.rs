//! Slot helpers the copy and seed routes share: the mirror root, slot naming and the
//! snapshot cleanup a slot or root needs.

use ployz_core::{MirrorMarker, RpcError, Snapshot};
use ployzd::machine_pool::MachinePool;

use super::{
    Dataset, DockerVolumeName, MIRROR_ROOT, VolumeStorage,
    inspect::{MIRROR_PROPERTY, internal},
};

pub(super) const MIRROR_MOUNT_ROOT: &str = "/var/lib/ployz-mirror";

pub(super) fn name(name: &ployz_core::DockerVolumeName) -> Result<DockerVolumeName, RpcError> {
    name.as_str().parse().map_err(internal)
}

impl VolumeStorage {
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
