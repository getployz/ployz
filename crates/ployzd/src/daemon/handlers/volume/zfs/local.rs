use ployz_api::{
    VolumeZfsClonePayload, VolumeZfsInspectPayload, VolumeZfsSnapshotInfo, VolumeZfsSnapshotPayload,
};
use ployz_model::VolumeRecord;
use ployz_spec::{Namespace, VolumeScope};
use ployz_storage_api::{CloneMetadata, DatasetSpec};
use ployz_store_api::DeployStore;
use ployz_volume_zfs::{TokioShellRunner, ZfsDriver};

use super::volume_dataset;
use crate::daemon::DaemonState;

impl DaemonState {
    pub(super) async fn inspect_local_volume_zfs(
        &self,
        namespace: &Namespace,
        volume: &str,
    ) -> Result<VolumeZfsInspectPayload, String> {
        let record = self.volume_record(namespace, volume).await?;
        let driver = self.local_zfs_driver().await?;
        let dataset = volume_dataset(driver.root_dataset(), namespace, volume);
        let info = driver
            .inspect_dataset(&dataset)
            .await
            .map_err(|error| error.to_string())?;
        Ok(VolumeZfsInspectPayload {
            namespace: namespace.as_str().to_string(),
            volume: volume.to_string(),
            machine_id: record.machine_id,
            dataset,
            mountpoint: info.mountpoint.display().to_string(),
            quota: info.quota,
            used_bytes: info.used_bytes,
            snapshots: info
                .snapshots
                .into_iter()
                .map(|snapshot| VolumeZfsSnapshotInfo {
                    name: snapshot.name,
                    guid: snapshot.guid,
                })
                .collect(),
        })
    }

    pub(super) async fn snapshot_local_volume_zfs(
        &self,
        namespace: &Namespace,
        volume: &str,
        snapshot: &str,
    ) -> Result<VolumeZfsSnapshotPayload, String> {
        let record = self.volume_record(namespace, volume).await?;
        let driver = self.local_zfs_driver().await?;
        let dataset = volume_dataset(driver.root_dataset(), namespace, volume);
        let snapshot = driver
            .create_snapshot(&dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        Ok(VolumeZfsSnapshotPayload {
            namespace: namespace.as_str().to_string(),
            volume: volume.to_string(),
            machine_id: record.machine_id,
            dataset,
            snapshot: snapshot.name,
            guid: snapshot.guid,
        })
    }

    pub(super) async fn snapshot_local_source_volume_zfs(
        &self,
        namespace: &Namespace,
        volume: &str,
        snapshot: &str,
    ) -> Result<VolumeZfsSnapshotPayload, String> {
        let record = self.volume_record(namespace, volume).await?;
        if record.machine_id != self.identity.machine_id {
            return Err(format!(
                "volume '{}/{}' is pinned to machine '{}', not local machine '{}'",
                namespace.as_str(),
                volume,
                record.machine_id,
                self.identity.machine_id
            ));
        }
        self.snapshot_local_volume_zfs(namespace, volume, snapshot)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn clone_local_volume_zfs(
        &self,
        namespace: &Namespace,
        deploy_id: &str,
        volume: &str,
        source_namespace: &Namespace,
        source_volume: &str,
        snapshot: &str,
        quota: &str,
        mode: &str,
        owner: &str,
    ) -> Result<VolumeZfsClonePayload, String> {
        let source_record = self.volume_record(source_namespace, source_volume).await?;
        if source_record.scope != VolumeScope::Single {
            return Err(format!(
                "volume '{}/{}' has scope {:?}, expected single",
                source_namespace.as_str(),
                source_volume,
                source_record.scope
            ));
        }
        if source_record.machine_id != self.identity.machine_id {
            return Err(format!(
                "volume '{}/{}' is pinned to machine '{}', not local machine '{}'",
                source_namespace.as_str(),
                source_volume,
                source_record.machine_id,
                self.identity.machine_id
            ));
        }
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no mesh is running".to_string())?;
        if active
            .mesh
            .store
            .get_volume(namespace, volume)
            .await
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err(format!(
                "volume '{}/{}' already has a committed record",
                namespace.as_str(),
                volume
            ));
        }

        let driver = self.local_zfs_driver().await?;
        let source_dataset = volume_dataset(driver.root_dataset(), source_namespace, source_volume);
        let target_dataset = volume_dataset(driver.root_dataset(), namespace, volume);
        let target = DatasetSpec {
            dataset: target_dataset.clone(),
            mountpoint: driver
                .root_mountpoint()
                .join(namespace.as_str())
                .join(volume),
            quota: quota.to_string(),
            mode: mode.to_string(),
            owner: owner.to_string(),
        };
        let snapshot_info = driver
            .create_snapshot(&source_dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        let clone_info = driver
            .clone_snapshot(
                &source_dataset,
                &snapshot_info.name,
                &target,
                &CloneMetadata {
                    deploy_id: deploy_id.to_string(),
                    namespace: namespace.as_str().to_string(),
                    volume: volume.to_string(),
                    source_namespace: source_namespace.as_str().to_string(),
                    source_volume: source_volume.to_string(),
                    snapshot: snapshot.to_string(),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(VolumeZfsClonePayload {
            namespace: namespace.as_str().to_string(),
            volume: volume.to_string(),
            source_namespace: source_namespace.as_str().to_string(),
            source_volume: source_volume.to_string(),
            machine_id: self.identity.machine_id.clone(),
            source_dataset,
            target_dataset,
            snapshot: clone_info.name,
            guid: clone_info.guid,
        })
    }

    pub(super) async fn cleanup_uncommitted_local_volume_clone_zfs(
        &self,
        namespace: &Namespace,
        deploy_id: &str,
        volume: &str,
        source_namespace: &Namespace,
        source_volume: &str,
        snapshot: &str,
    ) -> Result<(), String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no mesh is running".to_string())?;
        if active
            .mesh
            .store
            .get_volume(namespace, volume)
            .await
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Ok(());
        }
        let driver = self.local_zfs_driver().await?;
        let dataset = volume_dataset(driver.root_dataset(), namespace, volume);
        let mut target_cleanup_error = None;
        if driver
            .dataset_exists(&dataset)
            .await
            .map_err(|error| error.to_string())?
        {
            let destroyed = driver
                .destroy_marked_volume_clone(
                    &dataset,
                    &CloneMetadata {
                        deploy_id: deploy_id.to_string(),
                        namespace: namespace.as_str().to_string(),
                        volume: volume.to_string(),
                        source_namespace: source_namespace.as_str().to_string(),
                        source_volume: source_volume.to_string(),
                        snapshot: snapshot.to_string(),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            if !destroyed {
                target_cleanup_error = Some(format!(
                    "refusing to clean up uncommitted clone '{}/{}': dataset '{}' exists without matching clone metadata",
                    namespace.as_str(),
                    volume,
                    dataset
                ));
            }
        }
        let source_dataset = volume_dataset(driver.root_dataset(), source_namespace, source_volume);
        driver
            .destroy_snapshot(&source_dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(error) = target_cleanup_error {
            return Err(error);
        }
        Ok(())
    }

    pub(super) async fn snapshot_guid_local_volume_zfs(
        &self,
        namespace: &Namespace,
        volume: &str,
        snapshot: &str,
    ) -> Result<VolumeZfsSnapshotPayload, String> {
        let driver = self.local_zfs_driver().await?;
        let dataset = volume_dataset(driver.root_dataset(), namespace, volume);
        let guid = driver
            .snapshot_guid(&dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        Ok(VolumeZfsSnapshotPayload {
            namespace: namespace.as_str().to_string(),
            volume: volume.to_string(),
            machine_id: self.identity.machine_id.clone(),
            dataset,
            snapshot: snapshot.to_string(),
            guid,
        })
    }

    pub(super) async fn volume_record(
        &self,
        namespace: &Namespace,
        volume: &str,
    ) -> Result<VolumeRecord, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no mesh is running".to_string())?;
        active
            .mesh
            .store
            .get_volume(namespace, volume)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("volume '{namespace}/{volume}' not found"))
    }

    pub(super) async fn local_zfs_driver(&self) -> Result<ZfsDriver<TokioShellRunner>, String> {
        self.zfs_storage_driver()
            .await?
            .ok_or_else(|| "daemon does not have the ZFS storage backend configured".to_string())
            .map(|driver| driver.as_ref().clone())
    }
}
