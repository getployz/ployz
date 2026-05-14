use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, Result, StorageError};
use crate::spec::parse_quota_bytes;
use ployz_storage_api::{CloneMetadata, DatasetInspection, DatasetSpec, MountInfo, SnapshotInfo};
use ployz_volume_api::{
    EnsureVolumeRequest, SnapshotRequest, VolumeBackend, VolumeBackendInfo, VolumeBackendKind,
    VolumeCapabilities, VolumeError, VolumeIdentity, VolumeInspection, VolumeMount, VolumeSnapshot,
};

use super::{ShellOutput, ShellRunner, ShellStdio, ShellStreamer, TokioShellRunner};

const ZFS_MUTATION_COMMAND_TIMEOUT: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone)]
pub struct ZfsDriver<R> {
    runner: R,
    zfs_root_dataset: String,
    zfs_root_mountpoint: PathBuf,
    overcommit_ratio: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExistingDataset {
    quota: String,
    mountpoint: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathPermissions {
    mode: String,
    owner: String,
}

impl<R: ShellRunner> ZfsDriver<R> {
    pub async fn new(runner: R, zfs_root: &str, overcommit_ratio: f64) -> Result<Self> {
        if !overcommit_ratio.is_finite() || overcommit_ratio <= 0.0 {
            return Err(Error::Storage(StorageError::InvalidOvercommitRatio {
                value: overcommit_ratio.to_string(),
            }));
        }
        let output = runner
            .run("zfs", &["list", "-H", "-o", "mountpoint", zfs_root])
            .await?;
        ensure_success("zfs list root", &output.stderr, output.status)?;
        let mountpoint = parse_single_field(&output.stdout, "root mountpoint")?;
        Ok(Self {
            runner,
            zfs_root_dataset: zfs_root.to_string(),
            zfs_root_mountpoint: PathBuf::from(mountpoint),
            overcommit_ratio,
        })
    }

    pub async fn ensure(&self, spec: &DatasetSpec) -> Result<MountInfo> {
        let existing = self.read_dataset(&spec.dataset).await?;
        match existing {
            Some(existing) => {
                if existing.mountpoint != spec.mountpoint {
                    return Err(Error::Storage(StorageError::DatasetMountpointMismatch {
                        dataset: spec.dataset.clone(),
                        actual: existing.mountpoint.display().to_string(),
                        expected: spec.mountpoint.display().to_string(),
                    }));
                }
                if existing.quota != spec.quota {
                    self.update_quota(&spec.dataset, &existing.quota, &spec.quota)
                        .await?;
                }
                self.reconcile_permissions(spec).await?;
            }
            None => self.create_dataset(spec).await?,
        }
        Ok(MountInfo {
            mountpoint: spec.mountpoint.clone(),
        })
    }

    async fn reconcile_permissions(&self, spec: &DatasetSpec) -> Result<()> {
        let mountpoint = spec.mountpoint.to_string_lossy();
        let current = self.read_path_permissions(&mountpoint).await?;
        if !mode_matches(&current.mode, &spec.mode) {
            self.run_success("chmod", "chmod", &[&spec.mode, &mountpoint])
                .await?;
        }
        if current.owner != spec.owner {
            self.run_success("chown", "chown", &[&spec.owner, &mountpoint])
                .await?;
        }
        Ok(())
    }

    async fn read_path_permissions(&self, mountpoint: &str) -> Result<PathPermissions> {
        let output = self
            .runner
            .run("stat", &["-c", "%a:%u:%g", mountpoint])
            .await?;
        ensure_success("stat mountpoint", &output.stderr, output.status)?;
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let mut parts = raw.splitn(3, ':');
        let mode = parts
            .next()
            .ok_or_else(|| {
                Error::Storage(StorageError::StatFieldMissing {
                    field: "mode",
                    raw: raw.clone(),
                })
            })?
            .to_string();
        let uid = parts.next().ok_or_else(|| {
            Error::Storage(StorageError::StatFieldMissing {
                field: "uid",
                raw: raw.clone(),
            })
        })?;
        let gid = parts.next().ok_or_else(|| {
            Error::Storage(StorageError::StatFieldMissing {
                field: "gid",
                raw: raw.clone(),
            })
        })?;
        Ok(PathPermissions {
            mode,
            owner: format!("{uid}:{gid}"),
        })
    }

    pub async fn inspect_dataset(&self, dataset: &str) -> Result<DatasetInspection> {
        let existing = self.read_dataset(dataset).await?.ok_or_else(|| {
            Error::Storage(StorageError::DatasetMissing {
                dataset: dataset.to_string(),
            })
        })?;
        let used_bytes = self.used_bytes(dataset).await?;
        let snapshots = self.list_snapshots(dataset).await?;
        Ok(DatasetInspection {
            dataset: dataset.to_string(),
            quota: existing.quota,
            mountpoint: existing.mountpoint,
            used_bytes,
            snapshots,
        })
    }

    pub async fn dataset_exists(&self, dataset: &str) -> Result<bool> {
        Ok(self.read_dataset(dataset).await?.is_some())
    }

    /// Idempotently create the parent dataset of `dataset` (and any ancestors up
    /// to the root) so that a subsequent `zfs recv` can land into it. The root
    /// dataset itself is assumed to exist; we only create intermediate
    /// namespaces.
    pub async fn ensure_parent_dataset(&self, dataset: &str) -> Result<()> {
        let Some((parent, _)) = dataset.rsplit_once('/') else {
            return Err(Error::Storage(StorageError::DatasetMissingParent {
                dataset: dataset.to_string(),
            }));
        };
        if parent.is_empty() || parent == self.zfs_root_dataset {
            return Ok(());
        }
        if self.dataset_exists(parent).await? {
            return Ok(());
        }
        self.run_success("zfs create parent", "zfs", &["create", "-p", parent])
            .await
    }

    pub async fn destroy_snapshot(&self, dataset: &str, snapshot: &str) -> Result<()> {
        let full = snapshot_name(dataset, snapshot);
        let output = self
            .run_with_timeout("zfs destroy snapshot", "zfs", &["destroy", &full])
            .await?;
        if output.status == 0 {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("does not exist") || stderr.contains("dataset does not exist") {
            return Ok(());
        }
        Err(Error::operation(
            "zfs destroy snapshot",
            stderr.trim().to_string(),
        ))
    }

    pub async fn destroy_dataset_recursive(&self, dataset: &str) -> Result<()> {
        let output = self
            .run_with_timeout("zfs destroy dataset", "zfs", &["destroy", "-r", dataset])
            .await?;
        if output.status == 0 {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("does not exist") || stderr.contains("dataset does not exist") {
            return Ok(());
        }
        Err(Error::operation(
            "zfs destroy dataset",
            stderr.trim().to_string(),
        ))
    }

    pub async fn create_snapshot(&self, dataset: &str, snapshot: &str) -> Result<SnapshotInfo> {
        if self.snapshot_exists(dataset, snapshot).await? {
            return Ok(SnapshotInfo {
                name: snapshot.to_string(),
                guid: self.snapshot_guid(dataset, snapshot).await?,
            });
        }
        let full = snapshot_name(dataset, snapshot);
        self.run_success_with_timeout("zfs snapshot", "zfs", &["snapshot", &full])
            .await?;
        Ok(SnapshotInfo {
            name: snapshot.to_string(),
            guid: self.snapshot_guid(dataset, snapshot).await?,
        })
    }

    pub async fn clone_snapshot(
        &self,
        source_dataset: &str,
        snapshot: &str,
        target: &DatasetSpec,
        metadata: &CloneMetadata,
    ) -> Result<SnapshotInfo> {
        if source_dataset == target.dataset {
            return Err(Error::operation(
                "zfs clone",
                format!("source and target dataset are both '{source_dataset}'"),
            ));
        }
        let source_snapshot = snapshot_name(source_dataset, snapshot);
        let snapshot_info = SnapshotInfo {
            name: snapshot.to_string(),
            guid: self.snapshot_guid(source_dataset, snapshot).await?,
        };
        if self.dataset_exists(&target.dataset).await? {
            if !self
                .destroy_stale_volume_clone_target(&target.dataset, source_dataset, metadata)
                .await?
            {
                let origin = self.dataset_origin(&target.dataset).await?;
                return Err(Error::operation(
                    "zfs clone",
                    format!(
                        "target dataset '{}' already exists with origin {:?}; uncommitted clone targets are not adopted",
                        target.dataset, origin
                    ),
                ));
            }
        }

        let requested_bytes =
            parse_quota_bytes(&target.quota).map_err(|err| zfs_parse_error("quota", err))?;
        self.check_overcommit(&target.dataset, requested_bytes)
            .await?;
        self.ensure_parent_dataset(&target.dataset).await?;

        let mountpoint = target.mountpoint.to_string_lossy();
        let quota = format!("quota={}", target.quota);
        let mountpoint_opt = format!("mountpoint={mountpoint}");
        let role = "com.ployz:role=volume_clone".to_string();
        let deploy_id = format!("com.ployz:deploy_id={}", metadata.deploy_id);
        let namespace = format!("com.ployz:namespace={}", metadata.namespace);
        let volume = format!("com.ployz:volume={}", metadata.volume);
        let source_namespace = format!("com.ployz:source_namespace={}", metadata.source_namespace);
        let source_volume = format!("com.ployz:source_volume={}", metadata.source_volume);
        let snapshot = format!("com.ployz:snapshot={}", metadata.snapshot);
        let snapshot_guid = format!("com.ployz:snapshot_guid={}", snapshot_info.guid);
        let mut clone_args = Vec::from(["clone".to_string()]);
        for option in [
            &quota,
            &mountpoint_opt,
            &role,
            &deploy_id,
            &namespace,
            &volume,
            &source_namespace,
            &source_volume,
            &snapshot,
            &snapshot_guid,
        ] {
            clone_args.push("-o".to_string());
            clone_args.push(option.clone());
        }
        clone_args.push(source_snapshot.clone());
        clone_args.push(target.dataset.clone());
        let clone_arg_refs = clone_args.iter().map(String::as_str).collect::<Vec<_>>();
        self.run_success_with_timeout("zfs clone", "zfs", &clone_arg_refs)
            .await?;
        if let Err(error) = self
            .run_success_with_timeout("chmod", "chmod", &[&target.mode, &mountpoint])
            .await
        {
            let _ = self.destroy_dataset_recursive(&target.dataset).await;
            return Err(error);
        }
        if let Err(error) = self
            .run_success_with_timeout("chown", "chown", &[&target.owner, &mountpoint])
            .await
        {
            let _ = self.destroy_dataset_recursive(&target.dataset).await;
            return Err(error);
        }
        Ok(snapshot_info)
    }

    pub async fn destroy_marked_volume_clone(
        &self,
        dataset: &str,
        metadata: &CloneMetadata,
    ) -> Result<bool> {
        if !self.dataset_exists(dataset).await? {
            return Ok(false);
        }
        for (property, expected) in [
            ("com.ployz:role", "volume_clone"),
            ("com.ployz:deploy_id", metadata.deploy_id.as_str()),
            ("com.ployz:namespace", metadata.namespace.as_str()),
            ("com.ployz:volume", metadata.volume.as_str()),
            (
                "com.ployz:source_namespace",
                metadata.source_namespace.as_str(),
            ),
            ("com.ployz:source_volume", metadata.source_volume.as_str()),
            ("com.ployz:snapshot", metadata.snapshot.as_str()),
        ] {
            let actual = self.dataset_property(dataset, property).await?;
            if actual.as_deref() != Some(expected) {
                return Ok(false);
            }
        }
        self.destroy_dataset_recursive(dataset).await?;
        Ok(true)
    }

    async fn destroy_stale_volume_clone_target(
        &self,
        dataset: &str,
        source_dataset: &str,
        metadata: &CloneMetadata,
    ) -> Result<bool> {
        for (property, expected) in [
            ("com.ployz:role", "volume_clone"),
            ("com.ployz:namespace", metadata.namespace.as_str()),
            ("com.ployz:volume", metadata.volume.as_str()),
            (
                "com.ployz:source_namespace",
                metadata.source_namespace.as_str(),
            ),
            ("com.ployz:source_volume", metadata.source_volume.as_str()),
        ] {
            let actual = self.dataset_property(dataset, property).await?;
            if actual.as_deref() != Some(expected) {
                return Ok(false);
            }
        }
        let stale_snapshot = self.dataset_property(dataset, "com.ployz:snapshot").await?;
        self.destroy_dataset_recursive(dataset).await?;
        if let Some(snapshot) = stale_snapshot {
            if snapshot != metadata.snapshot {
                self.destroy_snapshot(source_dataset, &snapshot).await?;
            }
        }
        Ok(true)
    }

    pub async fn snapshot_exists(&self, dataset: &str, snapshot: &str) -> Result<bool> {
        let full = snapshot_name(dataset, snapshot);
        let output = self
            .runner
            .run(
                "zfs",
                &["list", "-H", "-t", "snapshot", "-o", "name", &full],
            )
            .await?;
        if output.status == 0 {
            return Ok(true);
        }
        if zfs_reports_not_found(&output.stderr) {
            return Ok(false);
        }
        Err(Error::operation(
            "zfs list snapshot",
            command_failure_message(&output.stderr, output.status),
        ))
    }

    pub async fn snapshot_guid(&self, dataset: &str, snapshot: &str) -> Result<u64> {
        let full = snapshot_name(dataset, snapshot);
        let output = self
            .runner
            .run("zfs", &["get", "-Hp", "-o", "value", "guid", &full])
            .await?;
        ensure_success("zfs get snapshot guid", &output.stderr, output.status)?;
        let value = parse_single_field(&output.stdout, "snapshot guid")?;
        value
            .parse::<u64>()
            .map_err(|err| zfs_parse_error("snapshot_guid", err.to_string()))
    }

    pub async fn list_snapshots(&self, dataset: &str) -> Result<Vec<SnapshotInfo>> {
        let output = self
            .runner
            .run(
                "zfs",
                &[
                    "list",
                    "-Hp",
                    "-t",
                    "snapshot",
                    "-o",
                    "name,guid",
                    "-r",
                    dataset,
                ],
            )
            .await?;
        if output.status != 0 {
            if zfs_reports_not_found(&output.stderr) {
                return Ok(Vec::new());
            }
            return Err(Error::operation(
                "zfs list snapshots",
                command_failure_message(&output.stderr, output.status),
            ));
        }
        let text = String::from_utf8(output.stdout).map_err(|err| {
            Error::Storage(StorageError::ZfsParse {
                field: "snapshots",
                message: err.to_string(),
            })
        })?;
        let mut snapshots = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parts = trimmed.split('\t').collect::<Vec<_>>();
            let [name, guid] = parts.as_slice() else {
                return Err(Error::Storage(StorageError::ZfsParse {
                    field: "snapshots",
                    message: format!("expected snapshot name and guid for line '{trimmed}'"),
                }));
            };
            let Some((listed_dataset, snap)) = name.split_once('@') else {
                continue;
            };
            if listed_dataset != dataset {
                continue;
            }
            snapshots.push(SnapshotInfo {
                name: snap.to_string(),
                guid: guid.parse::<u64>().map_err(|err| {
                    Error::Storage(StorageError::ZfsParse {
                        field: "snapshot_guid",
                        message: err.to_string(),
                    })
                })?,
            });
        }
        Ok(snapshots)
    }

    #[must_use]
    pub fn root_dataset(&self) -> &str {
        &self.zfs_root_dataset
    }

    #[must_use]
    pub fn root_mountpoint(&self) -> &Path {
        &self.zfs_root_mountpoint
    }

    fn dataset_for_identity(&self, identity: &VolumeIdentity) -> String {
        format!(
            "{}/{}/{}",
            self.zfs_root_dataset, identity.namespace, identity.name
        )
    }

    fn mountpoint_for_identity(&self, identity: &VolumeIdentity) -> PathBuf {
        self.zfs_root_mountpoint
            .join(&identity.namespace)
            .join(&identity.name)
    }

    async fn read_dataset(&self, dataset: &str) -> Result<Option<ExistingDataset>> {
        let output = self
            .runner
            .run(
                "zfs",
                &["list", "-Hp", "-o", "name,quota,mountpoint", dataset],
            )
            .await?;
        if output.status != 0 {
            if zfs_reports_not_found(&output.stderr) {
                return Ok(None);
            }
            return Err(Error::operation(
                "zfs list dataset",
                command_failure_message(&output.stderr, output.status),
            ));
        }
        let text = String::from_utf8(output.stdout).map_err(|err| {
            Error::Storage(StorageError::ZfsParse {
                field: "dataset",
                message: err.to_string(),
            })
        })?;
        let line = text.trim();
        let parts = line.split('\t').collect::<Vec<_>>();
        let [name, quota, mountpoint] = parts.as_slice() else {
            return Err(Error::Storage(StorageError::ZfsParse {
                field: "dataset",
                message: format!("expected name, quota, mountpoint for dataset '{dataset}'"),
            }));
        };
        if *name != dataset {
            return Err(Error::Storage(StorageError::ZfsParse {
                field: "dataset",
                message: format!("zfs listed dataset '{name}', expected '{dataset}'"),
            }));
        }
        Ok(Some(ExistingDataset {
            quota: normalize_quota(quota),
            mountpoint: PathBuf::from(mountpoint),
        }))
    }

    async fn dataset_origin(&self, dataset: &str) -> Result<Option<String>> {
        let output = self
            .runner
            .run("zfs", &["get", "-Hp", "-o", "value", "origin", dataset])
            .await?;
        ensure_success("zfs get origin", &output.stderr, output.status)?;
        let origin = parse_single_field(&output.stdout, "dataset origin")?;
        if origin == "-" {
            Ok(None)
        } else {
            Ok(Some(origin))
        }
    }

    async fn dataset_property(&self, dataset: &str, property: &str) -> Result<Option<String>> {
        let output = self
            .runner
            .run("zfs", &["get", "-Hp", "-o", "value", property, dataset])
            .await?;
        ensure_success("zfs get dataset property", &output.stderr, output.status)?;
        let value = parse_single_field(&output.stdout, "dataset property")?;
        if value == "-" {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    async fn create_dataset(&self, spec: &DatasetSpec) -> Result<()> {
        let requested_bytes =
            parse_quota_bytes(&spec.quota).map_err(|err| zfs_parse_error("quota", err))?;
        self.check_overcommit(&spec.dataset, requested_bytes)
            .await?;

        let mountpoint = spec.mountpoint.to_string_lossy();
        let quota = format!("quota={}", spec.quota);
        let mountpoint_opt = format!("mountpoint={mountpoint}");
        let output = self
            .runner
            .run(
                "zfs",
                &[
                    "create",
                    "-p",
                    "-o",
                    &quota,
                    "-o",
                    &mountpoint_opt,
                    "-o",
                    "compression=lz4",
                    &spec.dataset,
                ],
            )
            .await?;
        ensure_success("zfs create", &output.stderr, output.status)?;
        self.run_success("chmod", "chmod", &[&spec.mode, &mountpoint])
            .await?;
        self.run_success("chown", "chown", &[&spec.owner, &mountpoint])
            .await
    }

    async fn update_quota(&self, dataset: &str, current: &str, requested: &str) -> Result<()> {
        let current_bytes =
            parse_quota_bytes(current).map_err(|err| zfs_parse_error("quota", err))?;
        let requested_bytes =
            parse_quota_bytes(requested).map_err(|err| zfs_parse_error("quota", err))?;
        if requested_bytes == current_bytes {
            return Ok(());
        }
        if requested_bytes < current_bytes {
            let used = self.used_bytes(dataset).await?;
            if used > requested_bytes {
                return Err(Error::Storage(StorageError::QuotaBelowUsed {
                    dataset: dataset.to_string(),
                    requested: requested.to_string(),
                    used,
                }));
            }
        } else if requested_bytes > current_bytes {
            self.check_overcommit(dataset, requested_bytes).await?;
        }
        let quota = format!("quota={requested}");
        self.run_success("zfs set quota", "zfs", &["set", &quota, dataset])
            .await
    }

    async fn check_overcommit(&self, dataset: &str, requested_bytes: u64) -> Result<()> {
        let pool_size = self.pool_size_bytes().await?;
        let other = self.declared_quota_bytes_excluding(dataset).await?;
        let total = other.saturating_add(requested_bytes);
        let budget = (pool_size as f64 * self.overcommit_ratio).floor() as u64;
        if total > budget {
            return Err(Error::Storage(StorageError::QuotaWouldOvercommit {
                dataset: dataset.to_string(),
                pool: self.pool_name().to_string(),
                total,
                budget,
            }));
        }
        Ok(())
    }

    fn pool_name(&self) -> &str {
        self.zfs_root_dataset
            .split('/')
            .next()
            .unwrap_or(&self.zfs_root_dataset)
    }

    async fn pool_size_bytes(&self) -> Result<u64> {
        let pool = self.pool_name();
        let output = self
            .runner
            .run("zpool", &["list", "-Hp", "-o", "size", pool])
            .await?;
        ensure_success("zpool list size", &output.stderr, output.status)?;
        let value = parse_single_field(&output.stdout, "pool size")?;
        value
            .parse::<u64>()
            .map_err(|err| zfs_parse_error("pool_size", err.to_string()))
    }

    async fn declared_quota_bytes_excluding(&self, exclude_dataset: &str) -> Result<u64> {
        let output = self
            .runner
            .run(
                "zfs",
                &[
                    "list",
                    "-Hp",
                    "-r",
                    "-o",
                    "name,quota",
                    &self.zfs_root_dataset,
                ],
            )
            .await?;
        ensure_success("zfs list quotas", &output.stderr, output.status)?;
        let text = String::from_utf8(output.stdout)
            .map_err(|err| zfs_parse_error("quotas", err.to_string()))?;
        let mut sum: u64 = 0;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parts = trimmed.split('\t').collect::<Vec<_>>();
            let [name, quota] = parts.as_slice() else {
                return Err(zfs_parse_error(
                    "quotas",
                    format!("expected name and quota for line '{trimmed}'"),
                ));
            };
            if *name == exclude_dataset || *name == self.zfs_root_dataset {
                continue;
            }
            // `zfs list -Hp -o quota` prints "0" for unset (and "-" on some
            // dataset types). Anything else must parse as bytes; failing
            // closed prevents a malformed value from silently relaxing the
            // overcommit guard.
            let bytes = match quota.trim() {
                "" | "-" => 0,
                value => value.parse::<u64>().map_err(|err| {
                    zfs_parse_error("quota", format!("parse quota for '{name}': {err}"))
                })?,
            };
            sum = sum.saturating_add(bytes);
        }
        Ok(sum)
    }

    async fn used_bytes(&self, dataset: &str) -> Result<u64> {
        let output = self
            .runner
            .run("zfs", &["get", "-Hp", "-o", "value", "used", dataset])
            .await?;
        ensure_success("zfs get used", &output.stderr, output.status)?;
        let value = parse_single_field(&output.stdout, "used bytes")?;
        value
            .parse::<u64>()
            .map_err(|err| zfs_parse_error("used_bytes", err.to_string()))
    }

    async fn run_success(&self, context: &'static str, program: &str, args: &[&str]) -> Result<()> {
        let output = self.runner.run(program, args).await?;
        ensure_success(context, &output.stderr, output.status)
    }

    async fn run_success_with_timeout(
        &self,
        context: &'static str,
        program: &str,
        args: &[&str],
    ) -> Result<()> {
        let output = self.run_with_timeout(context, program, args).await?;
        ensure_success(context, &output.stderr, output.status)
    }

    async fn run_with_timeout(
        &self,
        context: &'static str,
        program: &str,
        args: &[&str],
    ) -> Result<ShellOutput> {
        tokio::time::timeout(ZFS_MUTATION_COMMAND_TIMEOUT, self.runner.run(program, args))
            .await
            .map_err(|_| {
                Error::operation(
                    context,
                    format!(
                        "{program} timed out after {} seconds",
                        ZFS_MUTATION_COMMAND_TIMEOUT.as_secs()
                    ),
                )
            })?
    }
}

#[async_trait::async_trait]
impl<R> VolumeBackend for ZfsDriver<R>
where
    R: ShellRunner,
{
    fn info(&self) -> VolumeBackendInfo {
        VolumeBackendInfo {
            id: format!("zfs:{}", self.zfs_root_dataset),
            kind: VolumeBackendKind::Zfs,
            capabilities: VolumeCapabilities::zfs_like(),
        }
    }

    async fn ensure(&self, request: EnsureVolumeRequest) -> ployz_volume_api::Result<VolumeMount> {
        let identity = request.identity;
        let mountpoint = request
            .mountpoint
            .unwrap_or_else(|| self.mountpoint_for_identity(&identity));
        let spec = DatasetSpec {
            dataset: self.dataset_for_identity(&identity),
            mountpoint,
            quota: required_volume_field(&request.quota, "quota")?,
            mode: required_volume_field(&request.mode, "mode")?,
            owner: required_volume_field(&request.owner, "owner")?,
        };
        let info = ZfsDriver::ensure(self, &spec)
            .await
            .map_err(|error| zfs_volume_error("ensure", error))?;
        Ok(VolumeMount::host_path(identity, info.mountpoint))
    }

    async fn remove(&self, identity: VolumeIdentity) -> ployz_volume_api::Result<()> {
        let dataset = self.dataset_for_identity(&identity);
        self.destroy_dataset_recursive(&dataset)
            .await
            .map_err(|error| zfs_volume_error("remove", error))
    }

    async fn mount(&self, identity: VolumeIdentity) -> ployz_volume_api::Result<VolumeMount> {
        let dataset = self.dataset_for_identity(&identity);
        let inspection = self
            .inspect_dataset(&dataset)
            .await
            .map_err(|error| zfs_volume_error("mount", error))?;
        Ok(VolumeMount::host_path(identity, inspection.mountpoint))
    }

    async fn inspect(
        &self,
        identity: VolumeIdentity,
    ) -> ployz_volume_api::Result<Option<VolumeInspection>> {
        let dataset = self.dataset_for_identity(&identity);
        if !self
            .dataset_exists(&dataset)
            .await
            .map_err(|error| zfs_volume_error("inspect", error))?
        {
            return Ok(None);
        }
        let inspection = self
            .inspect_dataset(&dataset)
            .await
            .map_err(|error| zfs_volume_error("inspect", error))?;
        Ok(Some(VolumeInspection {
            identity,
            mountpoint: Some(inspection.mountpoint),
            used_bytes: Some(inspection.used_bytes),
            snapshots: inspection
                .snapshots
                .into_iter()
                .map(|snapshot| snapshot.name)
                .collect(),
        }))
    }

    async fn snapshot(&self, request: SnapshotRequest) -> ployz_volume_api::Result<VolumeSnapshot> {
        let dataset = self.dataset_for_identity(&request.identity);
        self.create_snapshot(&dataset, &request.snapshot)
            .await
            .map_err(|error| zfs_volume_error("snapshot", error))?;
        Ok(VolumeSnapshot {
            identity: request.identity,
            snapshot: request.snapshot,
        })
    }
}

impl ZfsDriver<TokioShellRunner> {
    pub fn spawn_send_full(&self, dataset: &str, snapshot: &str) -> Result<tokio::process::Child> {
        let full = snapshot_name(dataset, snapshot);
        self.runner
            .spawn("zfs", &["send", &full], ShellStdio::PipedStdout)
    }

    pub fn spawn_send_incremental(
        &self,
        dataset: &str,
        from_snapshot: &str,
        snapshot: &str,
    ) -> Result<tokio::process::Child> {
        let from = snapshot_name(dataset, from_snapshot);
        let to = snapshot_name(dataset, snapshot);
        self.runner
            .spawn("zfs", &["send", "-i", &from, &to], ShellStdio::PipedStdout)
    }

    /// Spawn `zfs recv` against `dataset`. We deliberately do not pass `-F` so
    /// that an unexpected pre-existing dataset or divergent snapshot list is
    /// surfaced as an error rather than silently rolled back. Callers are
    /// responsible for pre-checking idempotency (see
    /// [`ZfsDriver::snapshot_exists`]) and cleaning up partial state on
    /// failure (see [`ZfsDriver::destroy_snapshot`]).
    pub fn spawn_recv(&self, dataset: &str) -> Result<tokio::process::Child> {
        self.runner
            .spawn("zfs", &["recv", dataset], ShellStdio::PipedStdin)
    }
}

fn required_volume_field(
    value: &Option<String>,
    field: &'static str,
) -> ployz_volume_api::Result<String> {
    value.clone().ok_or_else(|| VolumeError::InvalidRequest {
        field: field.to_string(),
        message: "field is required by the ZFS volume backend".to_string(),
    })
}

fn zfs_volume_error(operation: &'static str, error: Error) -> VolumeError {
    VolumeError::Backend {
        backend: "zfs".to_string(),
        operation,
        message: error.to_string(),
    }
}

fn ensure_success(context: &'static str, stderr: &[u8], status: i32) -> Result<()> {
    if status == 0 {
        return Ok(());
    }
    Err(Error::operation(
        context,
        command_failure_message(stderr, status),
    ))
}

fn command_failure_message(stderr: &[u8], status: i32) -> String {
    let message = String::from_utf8_lossy(stderr).trim().to_string();
    if message.is_empty() {
        format!("exited with status {status}")
    } else {
        message
    }
}

fn zfs_reports_not_found(stderr: &[u8]) -> bool {
    let message = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    message.contains("dataset does not exist") || message.contains("snapshot does not exist")
}

fn parse_single_field(stdout: &[u8], field: &'static str) -> Result<String> {
    let text = String::from_utf8(stdout.to_vec())
        .map_err(|err| zfs_parse_error(field, err.to_string()))?;
    let value = text.trim();
    if value.is_empty() {
        return Err(zfs_parse_error(field, format!("missing {field}")));
    }
    Ok(value.to_string())
}

fn zfs_parse_error(field: &'static str, message: impl Into<String>) -> Error {
    Error::Storage(StorageError::ZfsParse {
        field,
        message: message.into(),
    })
}

fn normalize_quota(value: &str) -> String {
    if value == "-" {
        String::new()
    } else {
        value.to_string()
    }
}

fn snapshot_name(dataset: &str, snapshot: &str) -> String {
    format!("{dataset}@{snapshot}")
}

fn mode_matches(actual: &str, expected: &str) -> bool {
    parse_octal_mode(actual)
        .zip(parse_octal_mode(expected))
        .is_some_and(|(a, e)| a == e)
        || actual == expected
}

fn parse_octal_mode(value: &str) -> Option<u32> {
    let trimmed = value.trim_start_matches('0');
    let normalized = if trimmed.is_empty() { "0" } else { trimmed };
    u32::from_str_radix(normalized, 8).ok()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::ShellOutput;
    use ployz_volume_api::VolumeCapability;

    #[derive(Debug, Clone, Default)]
    struct FakeShellRunner {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        outputs: Arc<Mutex<VecDeque<ShellOutput>>>,
    }

    impl FakeShellRunner {
        fn push(&self, status: i32, stdout: &str, stderr: &str) {
            self.outputs
                .lock()
                .expect("outputs")
                .push_back(ShellOutput {
                    status,
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: stderr.as_bytes().to_vec(),
                });
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("calls").clone()
        }
    }

    #[async_trait]
    impl ShellRunner for FakeShellRunner {
        async fn run(&self, program: &str, args: &[&str]) -> Result<ShellOutput> {
            let mut call = vec![program.to_string()];
            call.extend(args.iter().map(|arg| (*arg).to_string()));
            self.calls.lock().expect("calls").push(call);
            self.outputs
                .lock()
                .expect("outputs")
                .pop_front()
                .ok_or_else(|| Error::operation("fake_shell", "missing output"))
        }
    }

    fn spec() -> DatasetSpec {
        DatasetSpec {
            dataset: "tank/ployz/prod/data".into(),
            mountpoint: "/tank/ployz/prod/data".into(),
            quota: "1G".into(),
            mode: "0750".into(),
            owner: "999:999".into(),
        }
    }

    fn clone_metadata() -> CloneMetadata {
        CloneMetadata {
            deploy_id: "deploy-1".into(),
            namespace: "pr-39".into(),
            volume: "data".into(),
            source_namespace: "default".into(),
            source_volume: "data".into(),
            snapshot: "branch".into(),
        }
    }

    async fn driver_with_ratio(fake: &FakeShellRunner, ratio: f64) -> ZfsDriver<FakeShellRunner> {
        fake.push(0, "/tank/ployz\n", "");
        ZfsDriver::new(fake.clone(), "tank/ployz", ratio)
            .await
            .expect("driver")
    }

    async fn driver(fake: &FakeShellRunner) -> ZfsDriver<FakeShellRunner> {
        driver_with_ratio(fake, 1.0).await
    }

    #[tokio::test]
    async fn volume_backend_info_advertises_zfs_snapshot_capability() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;

        let info = VolumeBackend::info(&driver);

        assert_eq!(info.kind, VolumeBackendKind::Zfs);
        assert!(info.capabilities.contains(VolumeCapability::Ensure));
        assert!(info.capabilities.contains(VolumeCapability::Snapshot));
        assert!(!info.capabilities.contains(VolumeCapability::Clone));
    }

    #[tokio::test]
    async fn volume_backend_ensure_derives_dataset_and_mountpoint() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(1, "", "dataset does not exist");
        push_overcommit_lookup(&fake, 1024_u64.pow(4), "tank/ployz\t0\n");
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(0, "", "");

        let mount = VolumeBackend::ensure(
            &driver,
            EnsureVolumeRequest {
                identity: VolumeIdentity::new("prod", "data"),
                mountpoint: None,
                quota: Some("1G".to_string()),
                mode: Some("0750".to_string()),
                owner: Some("999:999".to_string()),
            },
        )
        .await
        .expect("ensure volume");

        assert_eq!(
            mount.mountpoint(),
            Some(&PathBuf::from("/tank/ployz/prod/data"))
        );
        let calls = fake.calls();
        assert_eq!(calls[1][5], "tank/ployz/prod/data");
        assert!(
            calls
                .iter()
                .any(|call| call.iter().map(String::as_str).collect::<Vec<_>>()
                    == [
                        "zfs",
                        "create",
                        "-p",
                        "-o",
                        "quota=1G",
                        "-o",
                        "mountpoint=/tank/ployz/prod/data",
                        "-o",
                        "compression=lz4",
                        "tank/ployz/prod/data",
                    ])
        );
    }

    fn push_overcommit_lookup(fake: &FakeShellRunner, pool_size_bytes: u64, quota_list: &str) {
        fake.push(0, &format!("{pool_size_bytes}\n"), "");
        fake.push(0, quota_list, "");
    }

    #[tokio::test]
    async fn ensure_creates_missing_dataset() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(1, "", "dataset does not exist");
        push_overcommit_lookup(&fake, 1024_u64.pow(4), "tank/ployz\t0\n");
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(0, "", "");

        driver.ensure(&spec()).await.expect("ensure");

        let calls = fake.calls();
        assert_eq!(calls[1][..4], ["zfs", "list", "-Hp", "-o"]);
        assert_eq!(calls[2][..3], ["zpool", "list", "-Hp"]);
        assert_eq!(calls[3][..4], ["zfs", "list", "-Hp", "-r"]);
        assert_eq!(calls[4][..4], ["zfs", "create", "-p", "-o"]);
        assert_eq!(calls[5], ["chmod", "0750", "/tank/ployz/prod/data"]);
        assert_eq!(calls[6], ["chown", "999:999", "/tank/ployz/prod/data"]);
    }

    #[tokio::test]
    async fn ensure_propagates_zfs_list_failure() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(1, "", "permission denied");

        let err = driver
            .ensure(&spec())
            .await
            .expect_err("zfs list failure should fail");

        assert!(err.to_string().contains("permission denied"), "got: {err}");
        assert_eq!(fake.calls().len(), 2);
    }

    #[tokio::test]
    async fn ensure_adopts_matching_dataset() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(
            0,
            "tank/ployz/prod/data\t1073741824\t/tank/ployz/prod/data\n",
            "",
        );
        fake.push(0, "750:999:999\n", "");

        driver.ensure(&spec()).await.expect("ensure");

        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[2],
            ["stat", "-c", "%a:%u:%g", "/tank/ployz/prod/data"]
        );
    }

    #[tokio::test]
    async fn ensure_reconciles_drifted_mode_and_owner_on_adopt() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(
            0,
            "tank/ployz/prod/data\t1073741824\t/tank/ployz/prod/data\n",
            "",
        );
        fake.push(0, "777:0:0\n", "");
        fake.push(0, "", "");
        fake.push(0, "", "");

        driver.ensure(&spec()).await.expect("ensure");

        let calls = fake.calls();
        assert_eq!(
            calls[2],
            ["stat", "-c", "%a:%u:%g", "/tank/ployz/prod/data"]
        );
        assert_eq!(calls[3], ["chmod", "0750", "/tank/ployz/prod/data"]);
        assert_eq!(calls[4], ["chown", "999:999", "/tank/ployz/prod/data"]);
    }

    #[tokio::test]
    async fn ensure_skips_quota_update_when_numeric_zfs_quota_matches_request() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(
            0,
            "tank/ployz/prod/data\t1073741824\t/tank/ployz/prod/data\n",
            "",
        );
        fake.push(0, "750:999:999\n", "");

        driver.ensure(&spec()).await.expect("ensure");

        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[1],
            vec![
                "zfs",
                "list",
                "-Hp",
                "-o",
                "name,quota,mountpoint",
                "tank/ployz/prod/data"
            ]
        );
    }

    #[tokio::test]
    async fn ensure_grows_quota() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(
            0,
            "tank/ployz/prod/data\t1073741824\t/tank/ployz/prod/data\n",
            "",
        );
        push_overcommit_lookup(
            &fake,
            1024_u64.pow(4),
            "tank/ployz\t0\ntank/ployz/prod/data\t1073741824\n",
        );
        fake.push(0, "", "");
        fake.push(0, "750:999:999\n", "");
        let mut next = spec();
        next.quota = "2G".into();

        driver.ensure(&next).await.expect("ensure");

        let calls = fake.calls();
        assert_eq!(calls[4], ["zfs", "set", "quota=2G", "tank/ployz/prod/data"]);
        assert_eq!(
            calls[5],
            ["stat", "-c", "%a:%u:%g", "/tank/ployz/prod/data"]
        );
    }

    #[tokio::test]
    async fn ensure_refuses_shrink_below_used() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(
            0,
            "tank/ployz/prod/data\t2147483648\t/tank/ployz/prod/data\n",
            "",
        );
        fake.push(0, "1610612736\n", "");

        let err = driver
            .ensure(&spec())
            .await
            .expect_err("shrink should fail");

        assert_eq!(
            err,
            Error::Storage(StorageError::QuotaBelowUsed {
                dataset: "tank/ployz/prod/data".into(),
                requested: "1G".into(),
                used: 1_610_612_736
            })
        );
    }

    #[tokio::test]
    async fn ensure_rejects_mountpoint_mismatch() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "tank/ployz/prod/data\t1G\t/other\n", "");

        let err = driver
            .ensure(&spec())
            .await
            .expect_err("mismatch should fail");

        assert_eq!(
            err,
            Error::Storage(StorageError::DatasetMountpointMismatch {
                dataset: "tank/ployz/prod/data".into(),
                actual: "/other".into(),
                expected: "/tank/ployz/prod/data".into()
            })
        );
    }

    #[tokio::test]
    async fn snapshot_creation_reads_guid() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(1, "", "snapshot does not exist");
        fake.push(0, "", "");
        fake.push(0, "42\n", "");

        let snapshot = driver
            .create_snapshot("tank/ployz/prod/data", "base")
            .await
            .expect("snapshot");

        assert_eq!(snapshot.name, "base");
        assert_eq!(snapshot.guid, 42);
        let calls = fake.calls();
        assert_eq!(
            calls[1],
            [
                "zfs",
                "list",
                "-H",
                "-t",
                "snapshot",
                "-o",
                "name",
                "tank/ployz/prod/data@base"
            ]
        );
        assert_eq!(calls[2], ["zfs", "snapshot", "tank/ployz/prod/data@base"]);
        assert_eq!(
            calls[3],
            [
                "zfs",
                "get",
                "-Hp",
                "-o",
                "value",
                "guid",
                "tank/ployz/prod/data@base"
            ]
        );
    }

    #[tokio::test]
    async fn snapshot_exists_returns_errors_other_than_not_found() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(1, "", "permission denied");

        let err = driver
            .snapshot_exists("tank/ployz/prod/data", "base")
            .await
            .expect_err("permission failure should not be treated as missing");

        assert!(err.to_string().contains("permission denied"));
    }

    #[tokio::test]
    async fn clone_snapshot_creates_target_from_source_snapshot() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        let mut target = spec();
        target.dataset = "tank/ployz/pr-39/data".into();
        target.mountpoint = "/tank/ployz/pr-39/data".into();
        fake.push(0, "42\n", "");
        fake.push(1, "", "dataset does not exist");
        push_overcommit_lookup(&fake, 1024_u64.pow(4), "tank/ployz\t0\n");
        fake.push(1, "", "dataset does not exist");
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(0, "", "");

        let snapshot = driver
            .clone_snapshot(
                "tank/ployz/default/data",
                "branch",
                &target,
                &clone_metadata(),
            )
            .await
            .expect("clone snapshot");

        assert_eq!(
            snapshot,
            SnapshotInfo {
                name: "branch".into(),
                guid: 42,
            }
        );
        let calls = fake.calls();
        assert_eq!(
            calls[2],
            [
                "zfs",
                "list",
                "-Hp",
                "-o",
                "name,quota,mountpoint",
                "tank/ployz/pr-39/data"
            ]
        );
        assert_eq!(calls[6], ["zfs", "create", "-p", "tank/ployz/pr-39"]);
        assert_eq!(
            calls[7],
            [
                "zfs",
                "clone",
                "-o",
                "quota=1G",
                "-o",
                "mountpoint=/tank/ployz/pr-39/data",
                "-o",
                "com.ployz:role=volume_clone",
                "-o",
                "com.ployz:deploy_id=deploy-1",
                "-o",
                "com.ployz:namespace=pr-39",
                "-o",
                "com.ployz:volume=data",
                "-o",
                "com.ployz:source_namespace=default",
                "-o",
                "com.ployz:source_volume=data",
                "-o",
                "com.ployz:snapshot=branch",
                "-o",
                "com.ployz:snapshot_guid=42",
                "tank/ployz/default/data@branch",
                "tank/ployz/pr-39/data"
            ]
        );
        assert_eq!(calls[8], ["chmod", "0750", "/tank/ployz/pr-39/data"]);
        assert_eq!(calls[9], ["chown", "999:999", "/tank/ployz/pr-39/data"]);
    }

    #[tokio::test]
    async fn clone_snapshot_recovers_stale_matching_clone_target() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        let mut target = spec();
        target.dataset = "tank/ployz/pr-39/data".into();
        target.mountpoint = "/tank/ployz/pr-39/data".into();
        fake.push(0, "42\n", "");
        fake.push(
            0,
            "tank/ployz/pr-39/data\t1073741824\t/tank/ployz/pr-39/data\n",
            "",
        );
        fake.push(0, "volume_clone\n", "");
        fake.push(0, "pr-39\n", "");
        fake.push(0, "data\n", "");
        fake.push(0, "default\n", "");
        fake.push(0, "data\n", "");
        fake.push(0, "stale-branch\n", "");
        fake.push(0, "", "");
        fake.push(0, "", "");
        push_overcommit_lookup(&fake, 1024_u64.pow(4), "tank/ployz\t0\n");
        fake.push(1, "", "dataset does not exist");
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(0, "", "");

        let snapshot = driver
            .clone_snapshot(
                "tank/ployz/default/data",
                "branch",
                &target,
                &clone_metadata(),
            )
            .await
            .expect("stale clone target should be replaced");

        assert_eq!(snapshot.name, "branch");
        let calls = fake.calls();
        assert!(
            calls
                .iter()
                .any(|call| call.as_slice() == ["zfs", "destroy", "-r", "tank/ployz/pr-39/data"])
        );
        assert!(
            calls.iter().any(|call| call.as_slice()
                == ["zfs", "destroy", "tank/ployz/default/data@stale-branch"])
        );
        assert!(
            calls
                .iter()
                .any(|call| call.first().map(String::as_str) == Some("chmod"))
        );
    }

    #[tokio::test]
    async fn clone_snapshot_keeps_requested_snapshot_when_replacing_retry_target() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        let mut target = spec();
        target.dataset = "tank/ployz/pr-39/data".into();
        target.mountpoint = "/tank/ployz/pr-39/data".into();
        fake.push(0, "42\n", "");
        fake.push(
            0,
            "tank/ployz/pr-39/data\t1073741824\t/tank/ployz/pr-39/data\n",
            "",
        );
        fake.push(0, "volume_clone\n", "");
        fake.push(0, "pr-39\n", "");
        fake.push(0, "data\n", "");
        fake.push(0, "default\n", "");
        fake.push(0, "data\n", "");
        fake.push(0, "branch\n", "");
        fake.push(0, "", "");
        push_overcommit_lookup(&fake, 1024_u64.pow(4), "tank/ployz\t0\n");
        fake.push(1, "", "dataset does not exist");
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(0, "", "");

        let snapshot = driver
            .clone_snapshot(
                "tank/ployz/default/data",
                "branch",
                &target,
                &clone_metadata(),
            )
            .await
            .expect("retry clone target should be replaced");

        assert_eq!(snapshot.name, "branch");
        let calls = fake.calls();
        assert!(
            calls
                .iter()
                .any(|call| call.as_slice() == ["zfs", "destroy", "-r", "tank/ployz/pr-39/data"])
        );
        assert!(!calls.iter().any(|call| call.as_slice()
            == ["zfs", "destroy", "tank/ployz/default/data@branch"]));
        assert!(calls.iter().any(|call| call.as_slice()
            == [
                "zfs",
                "clone",
                "-o",
                "quota=1G",
                "-o",
                "mountpoint=/tank/ployz/pr-39/data",
                "-o",
                "com.ployz:role=volume_clone",
                "-o",
                "com.ployz:deploy_id=deploy-1",
                "-o",
                "com.ployz:namespace=pr-39",
                "-o",
                "com.ployz:volume=data",
                "-o",
                "com.ployz:source_namespace=default",
                "-o",
                "com.ployz:source_volume=data",
                "-o",
                "com.ployz:snapshot=branch",
                "-o",
                "com.ployz:snapshot_guid=42",
                "tank/ployz/default/data@branch",
                "tank/ployz/pr-39/data"
            ]));
    }

    #[tokio::test]
    async fn clone_snapshot_rejects_matching_clone_after_create_race() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        let mut target = spec();
        target.dataset = "tank/ployz/pr-39/data".into();
        target.mountpoint = "/tank/ployz/pr-39/data".into();
        fake.push(0, "42\n", "");
        fake.push(1, "", "dataset does not exist");
        push_overcommit_lookup(&fake, 1024_u64.pow(4), "tank/ployz\t0\n");
        fake.push(1, "", "dataset does not exist");
        fake.push(0, "", "");
        fake.push(
            1,
            "",
            "cannot create 'tank/ployz/pr-39/data': dataset already exists",
        );

        let error = driver
            .clone_snapshot(
                "tank/ployz/default/data",
                "branch",
                &target,
                &clone_metadata(),
            )
            .await
            .expect_err("raced clone target should not be adopted");

        assert!(error.to_string().contains("already exists"), "got: {error}");

        let calls = fake.calls();
        assert_eq!(
            calls[7],
            [
                "zfs",
                "clone",
                "-o",
                "quota=1G",
                "-o",
                "mountpoint=/tank/ployz/pr-39/data",
                "-o",
                "com.ployz:role=volume_clone",
                "-o",
                "com.ployz:deploy_id=deploy-1",
                "-o",
                "com.ployz:namespace=pr-39",
                "-o",
                "com.ployz:volume=data",
                "-o",
                "com.ployz:source_namespace=default",
                "-o",
                "com.ployz:source_volume=data",
                "-o",
                "com.ployz:snapshot=branch",
                "-o",
                "com.ployz:snapshot_guid=42",
                "tank/ployz/default/data@branch",
                "tank/ployz/pr-39/data"
            ]
        );
    }

    #[tokio::test]
    async fn clone_snapshot_rolls_back_target_when_permission_reconcile_fails() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        let mut target = spec();
        target.dataset = "tank/ployz/pr-39/data".into();
        target.mountpoint = "/tank/ployz/pr-39/data".into();
        fake.push(0, "42\n", "");
        fake.push(1, "", "dataset does not exist");
        push_overcommit_lookup(&fake, 1024_u64.pow(4), "tank/ployz\t0\n");
        fake.push(1, "", "dataset does not exist");
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(1, "", "chmod denied");
        fake.push(0, "", "");

        let error = driver
            .clone_snapshot(
                "tank/ployz/default/data",
                "branch",
                &target,
                &clone_metadata(),
            )
            .await
            .expect_err("permission failure should roll back clone target");

        assert!(error.to_string().contains("chmod denied"), "got: {error}");
        let calls = fake.calls();
        assert_eq!(calls[8], ["chmod", "0750", "/tank/ployz/pr-39/data"]);
        assert_eq!(calls[9], ["zfs", "destroy", "-r", "tank/ployz/pr-39/data"]);
    }

    #[tokio::test]
    async fn clone_snapshot_rejects_existing_target_with_wrong_origin() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        let mut target = spec();
        target.dataset = "tank/ployz/pr-39/data".into();
        target.mountpoint = "/tank/ployz/pr-39/data".into();
        fake.push(0, "42\n", "");
        fake.push(
            0,
            "tank/ployz/pr-39/data\t1073741824\t/tank/ployz/pr-39/data\n",
            "",
        );
        fake.push(0, "-\n", "");
        fake.push(0, "tank/ployz/default/data@other\n", "");

        let error = driver
            .clone_snapshot(
                "tank/ployz/default/data",
                "branch",
                &target,
                &clone_metadata(),
            )
            .await
            .expect_err("wrong origin should fail");

        assert!(error.to_string().contains("already exists"), "got: {error}");
    }

    #[tokio::test]
    async fn inspect_dataset_returns_snapshot_lineage() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "tank/ployz/prod/data\t1G\t/tank/ployz/prod/data\n", "");
        fake.push(0, "123\n", "");
        fake.push(
            0,
            "tank/ployz/prod/data@base\t42\ntank/ployz/prod/data@cutover\t43\n",
            "",
        );

        let info = driver
            .inspect_dataset("tank/ployz/prod/data")
            .await
            .expect("inspect");

        assert_eq!(info.used_bytes, 123);
        assert_eq!(
            info.snapshots,
            vec![
                SnapshotInfo {
                    name: "base".into(),
                    guid: 42
                },
                SnapshotInfo {
                    name: "cutover".into(),
                    guid: 43
                }
            ]
        );
    }

    #[tokio::test]
    async fn inspect_dataset_surfaces_snapshot_listing_failures() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "tank/ployz/prod/data\t1G\t/tank/ployz/prod/data\n", "");
        fake.push(0, "123\n", "");
        fake.push(1, "", "permission denied");

        let error = driver
            .inspect_dataset("tank/ployz/prod/data")
            .await
            .expect_err("snapshot listing failure should fail inspection");

        assert!(error.to_string().contains("zfs list snapshots"));
        assert!(error.to_string().contains("permission denied"));
    }

    #[tokio::test]
    async fn ensure_parent_dataset_creates_when_missing() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        // dataset_exists -> read_dataset returns missing
        fake.push(1, "", "dataset does not exist");
        // zfs create -p success
        fake.push(0, "", "");

        driver
            .ensure_parent_dataset("tank/ployz/prod/data")
            .await
            .expect("ensure parent");

        let calls = fake.calls();
        assert_eq!(
            calls[1],
            [
                "zfs",
                "list",
                "-Hp",
                "-o",
                "name,quota,mountpoint",
                "tank/ployz/prod"
            ]
        );
        assert_eq!(calls[2], ["zfs", "create", "-p", "tank/ployz/prod"]);
    }

    #[tokio::test]
    async fn ensure_parent_dataset_no_op_when_parent_is_root() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        driver
            .ensure_parent_dataset("tank/ployz/data")
            .await
            .expect("ensure parent root");
        // Only the root validation call from driver construction.
        assert_eq!(fake.calls().len(), 1);
    }

    #[tokio::test]
    async fn destroy_snapshot_treats_missing_as_success() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(
            1,
            "",
            "could not find any snapshots to destroy: dataset does not exist",
        );
        driver
            .destroy_snapshot("tank/ployz/prod/data", "missing")
            .await
            .expect("destroy missing snapshot");
    }

    #[tokio::test]
    async fn destroy_dataset_recursive_treats_missing_as_success() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(
            1,
            "",
            "cannot open 'tank/ployz/prod/data': dataset does not exist",
        );

        driver
            .destroy_dataset_recursive("tank/ployz/prod/data")
            .await
            .expect("destroy missing dataset");

        let calls = fake.calls();
        assert_eq!(calls[1], ["zfs", "destroy", "-r", "tank/ployz/prod/data"]);
    }

    #[tokio::test]
    async fn ensure_rejects_overcommit_on_create() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(1, "", "dataset does not exist");
        push_overcommit_lookup(
            &fake,
            1024_u64.pow(3),
            "tank/ployz\t0\ntank/ployz/other\t536870912\n",
        );
        let mut greedy = spec();
        greedy.quota = "1G".into();

        let err = driver
            .ensure(&greedy)
            .await
            .expect_err("overcommit should fail");

        assert_eq!(
            err,
            Error::Storage(StorageError::QuotaWouldOvercommit {
                dataset: "tank/ployz/prod/data".into(),
                pool: "tank".into(),
                total: 1_610_612_736,
                budget: 1_073_741_824
            })
        );
    }

    #[tokio::test]
    async fn ensure_rejects_overcommit_on_grow() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "tank/ployz/prod/data\t1G\t/tank/ployz/prod/data\n", "");
        push_overcommit_lookup(
            &fake,
            1024_u64.pow(3),
            "tank/ployz\t0\ntank/ployz/prod/data\t1073741824\ntank/ployz/other\t536870912\n",
        );
        let mut next = spec();
        next.quota = "1G".into();
        // Force the grow path: existing 1G in the read result, requested 2G
        next.quota = "2G".into();

        let err = driver
            .ensure(&next)
            .await
            .expect_err("overcommit on grow should fail");

        assert_eq!(
            err,
            Error::Storage(StorageError::QuotaWouldOvercommit {
                dataset: "tank/ployz/prod/data".into(),
                pool: "tank".into(),
                total: 2_684_354_560,
                budget: 1_073_741_824
            })
        );
    }

    #[tokio::test]
    async fn ensure_respects_ratio_above_one() {
        let fake = FakeShellRunner::default();
        let driver = driver_with_ratio(&fake, 2.0).await;
        fake.push(1, "", "dataset does not exist");
        push_overcommit_lookup(
            &fake,
            1024_u64.pow(3),
            "tank/ployz\t0\ntank/ployz/other\t1073741824\n",
        );
        fake.push(0, "", "");
        fake.push(0, "", "");
        fake.push(0, "", "");
        let mut greedy = spec();
        greedy.quota = "1G".into();

        driver
            .ensure(&greedy)
            .await
            .expect("ratio 2.0 should allow");
    }

    #[tokio::test]
    async fn new_rejects_invalid_ratio() {
        let fake = FakeShellRunner::default();
        let err = ZfsDriver::new(fake, "tank/ployz", 0.0)
            .await
            .expect_err("zero ratio should be rejected");
        assert_eq!(
            err,
            Error::Storage(StorageError::InvalidOvercommitRatio { value: "0".into() })
        );
    }
}
