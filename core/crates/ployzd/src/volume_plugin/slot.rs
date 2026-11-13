//! The verbs on a Volume's slot and root datasets that the copy, seed, departure and
//! storage routes share: the mirror root, the slot parent, parking a root in its slot, the
//! receive into a slot's copy, and the snapshot cleanup a slot or root needs.

use std::process::Stdio;

use ployz_core::{RpcError, SlotRole, Snapshot};
use ployzd::{
    dataset_layout::{DatasetLayout, MIRROR_MOUNT_ROOT},
    machine_pool::MachinePool,
};
use tokio::process::Command;

use super::{
    Dataset, VolumeName, VolumeStorage,
    inspect::internal,
    transfer::{Stop, ZfsChild},
};

pub(super) fn name(name: &ployz_core::DockerVolumeName) -> Result<VolumeName, RpcError> {
    VolumeName::try_from(name).map_err(|error| internal(error.into()))
}

/// The arguments of the `zfs receive` that fills a slot's copy `fs` bounded by `bound`:
/// unmounted, resumable, read-only.
fn receive_args(fs: &str, bound: u64) -> Vec<String> {
    [
        "receive",
        "-u",
        "-s",
        "-o",
        "readonly=on",
        "-o",
        &format!("refquota={bound}"),
        fs,
    ]
    .map(str::to_owned)
    .to_vec()
}

impl VolumeStorage {
    /// `<pool>/ployz-mirror`, the read-only parent every slot inherits from.
    pub(super) async fn ensure_mirror_root(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
    ) -> super::Result<()> {
        let mirror_root = DatasetLayout::of(pool).mirror_root();
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

    /// `root`, the unmounted parent of the Machine's writer roots, made when `datasets`
    /// lack it.
    pub(super) async fn ensure_root(&self, datasets: &[Dataset], root: &str) -> super::Result<()> {
        if datasets.iter().any(|dataset| dataset.name == root) {
            return Ok(());
        }
        self.create_root(root).await
    }

    /// A slot parent bounded by `refquota_bytes`, read-only like the mirror root it
    /// inherits from.
    pub(super) async fn create_slot(&self, parent: &str, refquota_bytes: u64) -> super::Result<()> {
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
        Ok(())
    }

    /// Renames the unmounted writer root `root` into `name`'s slot, in the order a crash
    /// can be read back from: the mirror root, the slot parent (bounded like the root,
    /// unless `datasets` already hold one), the parent's `OldCopy` role, then the rename.
    pub(super) async fn park_root_in_slot(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        name: &VolumeName,
        root: &Dataset,
    ) -> super::Result<()> {
        let layout = DatasetLayout::of(pool);
        self.ensure_mirror_root(pool, datasets).await?;
        let parent = layout.slot_parent(name);
        if Self::slot(datasets, pool, name).is_none() {
            self.create_slot(&parent, root.refquota).await?;
        }
        self.marks(&parent).set_role(SlotRole::OldCopy).await?;
        self.zfs(&["rename", &root.name, &layout.slot_fs(name)])
            .await?;
        Ok(())
    }

    /// Starts the `zfs receive` of a slot's copy `fs`, reading `stdin`, as a child of the
    /// receive task `stop` belongs to.
    pub(super) fn spawn_receive(
        &self,
        fs: &str,
        bound: u64,
        stdin: Stdio,
        stop: &Stop,
    ) -> std::io::Result<ZfsChild> {
        ZfsChild::spawn(
            Command::new(&self.zfs)
                .args(receive_args(fs, bound))
                .stdin(stdin)
                .stdout(Stdio::null())
                .stderr(Stdio::piped()),
            stop,
        )
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

#[cfg(test)]
mod tests {
    use super::receive_args;

    #[test]
    fn a_slots_receive_is_unmounted_resumable_read_only_and_bounded() {
        assert_eq!(
            receive_args("ployz/ployz-mirror/web_data/fs", 4096).join(" "),
            "receive -u -s -o readonly=on -o refquota=4096 ployz/ployz-mirror/web_data/fs"
        );
    }
}
