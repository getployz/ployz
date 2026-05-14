use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use ployz_volume_api::{
    CloneVolumeRequest, EnsureVolumeRequest, Result, SnapshotRequest, VolumeBackend,
    VolumeBackendInfo, VolumeBackendKind, VolumeCapabilities, VolumeCapability, VolumeError,
    VolumeIdentity, VolumeInspection, VolumeMount, VolumeSnapshot,
};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtrfsCommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[async_trait]
pub trait BtrfsRunner: Send + Sync {
    async fn run(
        &self,
        program: &str,
        args: &[&str],
    ) -> std::result::Result<BtrfsCommandOutput, String>;
}

#[derive(Debug, Clone, Default)]
pub struct TokioBtrfsRunner;

#[async_trait]
impl BtrfsRunner for TokioBtrfsRunner {
    async fn run(
        &self,
        program: &str,
        args: &[&str],
    ) -> std::result::Result<BtrfsCommandOutput, String> {
        let output = Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|error| format!("spawn {program} {}: {error}", args.join(" ")))?;
        Ok(BtrfsCommandOutput {
            status: output.status.code().unwrap_or(1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[derive(Clone)]
pub struct BtrfsVolumeBackend<R = TokioBtrfsRunner> {
    runner: Arc<R>,
    root_path: PathBuf,
}

impl BtrfsVolumeBackend<TokioBtrfsRunner> {
    #[must_use]
    pub fn local(root_path: PathBuf) -> Self {
        Self::new(TokioBtrfsRunner, root_path)
    }
}

impl<R> BtrfsVolumeBackend<R>
where
    R: BtrfsRunner,
{
    #[must_use]
    pub fn new(runner: R, root_path: PathBuf) -> Self {
        Self {
            runner: Arc::new(runner),
            root_path,
        }
    }

    #[must_use]
    pub fn path_for(&self, identity: &VolumeIdentity) -> PathBuf {
        self.root_path
            .join(&identity.namespace)
            .join(&identity.name)
    }

    fn snapshot_path(&self, identity: &VolumeIdentity, snapshot: &str) -> PathBuf {
        self.root_path
            .join(".snapshots")
            .join(&identity.namespace)
            .join(&identity.name)
            .join(snapshot)
    }

    async fn run_success(
        &self,
        operation: &'static str,
        program: &str,
        args: &[&str],
    ) -> Result<()> {
        let output = self
            .runner
            .run(program, args)
            .await
            .map_err(|message| btrfs_error(operation, message))?;
        if output.status == 0 {
            return Ok(());
        }
        Err(btrfs_error(
            operation,
            command_failure_message(&output.stderr, output.status),
        ))
    }

    async fn ensure_parent(&self, path: &Path) -> Result<()> {
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        std::fs::create_dir_all(parent).map_err(|error| {
            btrfs_error(
                "create btrfs volume parent",
                format!("create '{}': {error}", parent.display()),
            )
        })
    }
}

#[async_trait]
impl<R> VolumeBackend for BtrfsVolumeBackend<R>
where
    R: BtrfsRunner,
{
    fn info(&self) -> VolumeBackendInfo {
        VolumeBackendInfo {
            id: format!("btrfs:{}", self.root_path.display()),
            kind: VolumeBackendKind::Btrfs,
            capabilities: VolumeCapabilities::new([
                VolumeCapability::Ensure,
                VolumeCapability::Remove,
                VolumeCapability::Mount,
                VolumeCapability::Inspect,
                VolumeCapability::EnforceMode,
                VolumeCapability::EnforceOwner,
                VolumeCapability::Snapshot,
                VolumeCapability::Clone,
            ]),
        }
    }

    async fn ensure(&self, request: EnsureVolumeRequest) -> Result<VolumeMount> {
        if request.mountpoint.is_some() {
            return Err(VolumeError::InvalidRequest {
                field: "mountpoint".to_string(),
                message: "btrfs backend derives mountpoints from volume identity".to_string(),
            });
        }
        if request.quota.is_some() {
            return Err(VolumeError::InvalidRequest {
                field: "quota".to_string(),
                message: "btrfs backend does not own qgroup setup yet".to_string(),
            });
        }
        let path = self.path_for(&request.identity);
        self.ensure_parent(&path).await?;
        if !path.exists() {
            let path_text = path.to_string_lossy();
            self.run_success(
                "btrfs subvolume create",
                "btrfs",
                &["subvolume", "create", &path_text],
            )
            .await?;
        }
        if let Some(mode) = request.mode.as_deref() {
            let path_text = path.to_string_lossy();
            self.run_success("chmod btrfs volume", "chmod", &[mode, &path_text])
                .await?;
        }
        if let Some(owner) = request.owner.as_deref() {
            let path_text = path.to_string_lossy();
            self.run_success("chown btrfs volume", "chown", &[owner, &path_text])
                .await?;
        }
        Ok(VolumeMount::host_path(request.identity, path))
    }

    async fn remove(&self, identity: VolumeIdentity) -> Result<()> {
        let path = self.path_for(&identity);
        if !path.exists() {
            return Ok(());
        }
        let path_text = path.to_string_lossy();
        self.run_success(
            "btrfs subvolume delete",
            "btrfs",
            &["subvolume", "delete", &path_text],
        )
        .await
    }

    async fn mount(&self, identity: VolumeIdentity) -> Result<VolumeMount> {
        Ok(VolumeMount::host_path(
            identity.clone(),
            self.path_for(&identity),
        ))
    }

    async fn inspect(&self, identity: VolumeIdentity) -> Result<Option<VolumeInspection>> {
        let path = self.path_for(&identity);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(VolumeInspection {
            identity,
            mountpoint: Some(path),
            used_bytes: None,
            snapshots: Vec::new(),
        }))
    }

    async fn snapshot(&self, request: SnapshotRequest) -> Result<VolumeSnapshot> {
        let source = self.path_for(&request.identity);
        let target = self.snapshot_path(&request.identity, &request.snapshot);
        self.ensure_parent(&target).await?;
        let source_text = source.to_string_lossy();
        let target_text = target.to_string_lossy();
        self.run_success(
            "btrfs readonly snapshot",
            "btrfs",
            &["subvolume", "snapshot", "-r", &source_text, &target_text],
        )
        .await?;
        Ok(VolumeSnapshot {
            identity: request.identity,
            snapshot: request.snapshot,
        })
    }

    async fn clone_volume(&self, request: CloneVolumeRequest) -> Result<VolumeMount> {
        let source = request.snapshot.as_ref().map_or_else(
            || self.path_for(&request.source),
            |snapshot| self.snapshot_path(&request.source, snapshot),
        );
        let target = self.path_for(&request.target);
        self.ensure_parent(&target).await?;
        let source_text = source.to_string_lossy();
        let target_text = target.to_string_lossy();
        self.run_success(
            "btrfs writable snapshot",
            "btrfs",
            &["subvolume", "snapshot", &source_text, &target_text],
        )
        .await?;
        Ok(VolumeMount::host_path(request.target, target))
    }
}

fn btrfs_error(operation: &'static str, message: String) -> VolumeError {
    VolumeError::Backend {
        backend: "btrfs".to_string(),
        operation,
        message,
    }
}

fn command_failure_message(stderr: &[u8], status: i32) -> String {
    let message = String::from_utf8_lossy(stderr).trim().to_string();
    if message.is_empty() {
        format!("exited with status {status}")
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    #[derive(Debug, Default)]
    struct FakeBtrfsRunner {
        calls: Mutex<Vec<Vec<String>>>,
        outputs: Mutex<VecDeque<std::result::Result<BtrfsCommandOutput, String>>>,
    }

    impl FakeBtrfsRunner {
        fn push_status(&self, status: i32, stderr: &str) {
            self.outputs
                .lock()
                .expect("outputs")
                .push_back(Ok(BtrfsCommandOutput {
                    status,
                    stdout: Vec::new(),
                    stderr: stderr.as_bytes().to_vec(),
                }));
        }
    }

    #[async_trait]
    impl BtrfsRunner for FakeBtrfsRunner {
        async fn run(
            &self,
            program: &str,
            args: &[&str],
        ) -> std::result::Result<BtrfsCommandOutput, String> {
            let mut call = vec![program.to_string()];
            call.extend(args.iter().map(|arg| (*arg).to_string()));
            self.calls.lock().expect("calls").push(call);
            self.outputs
                .lock()
                .expect("outputs")
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(BtrfsCommandOutput {
                        status: 0,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    })
                })
        }
    }

    #[test]
    fn advertises_btrfs_snapshot_and_clone_capabilities() {
        let backend = BtrfsVolumeBackend::new(FakeBtrfsRunner::default(), "/tmp/ployz".into());
        let info = backend.info();

        assert_eq!(info.kind, VolumeBackendKind::Btrfs);
        assert!(info.capabilities.contains(VolumeCapability::Snapshot));
        assert!(info.capabilities.contains(VolumeCapability::Clone));
        assert!(
            !info
                .capabilities
                .contains(VolumeCapability::CustomMountpoint)
        );
        assert!(!info.capabilities.contains(VolumeCapability::EnforceQuota));
        assert!(info.capabilities.contains(VolumeCapability::EnforceMode));
        assert!(info.capabilities.contains(VolumeCapability::EnforceOwner));
        assert!(!info.capabilities.contains(VolumeCapability::Send));
    }

    #[tokio::test]
    async fn mount_uses_host_path_under_root() {
        let backend = BtrfsVolumeBackend::new(FakeBtrfsRunner::default(), "/tmp/ployz".into());

        let mount = backend
            .mount(VolumeIdentity::new("prod", "data"))
            .await
            .expect("mount");

        assert_eq!(
            mount.mountpoint(),
            Some(&PathBuf::from("/tmp/ployz/prod/data"))
        );
    }

    #[tokio::test]
    async fn ensure_surfaces_btrfs_command_failure() {
        let root =
            std::env::temp_dir().join(format!("ployz-btrfs-failure-test-{}", std::process::id()));
        let runner = FakeBtrfsRunner::default();
        runner.push_status(1, "not a btrfs filesystem");
        let backend = BtrfsVolumeBackend::new(runner, root.clone());

        let error = backend
            .ensure(EnsureVolumeRequest {
                identity: VolumeIdentity::new("prod", "data"),
                mountpoint: None,
                quota: None,
                mode: None,
                owner: None,
            })
            .await
            .expect_err("btrfs failure should be visible");

        assert!(
            error
                .to_string()
                .contains("btrfs subvolume create: not a btrfs filesystem"),
            "got: {error}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn ensure_applies_mode_and_owner_constraints() {
        let root = std::env::temp_dir().join(format!(
            "ployz-btrfs-constraint-test-{}",
            std::process::id()
        ));
        let runner = FakeBtrfsRunner::default();
        let backend = BtrfsVolumeBackend::new(runner, root.clone());

        let mount = backend
            .ensure(EnsureVolumeRequest {
                identity: VolumeIdentity::new("prod", "data"),
                mountpoint: None,
                quota: None,
                mode: Some("0750".to_string()),
                owner: Some("999:999".to_string()),
            })
            .await
            .expect("ensure btrfs volume");

        assert_eq!(mount.mountpoint(), Some(&root.join("prod").join("data")));
        let calls = backend.runner.calls.lock().expect("calls").clone();
        assert_eq!(
            calls,
            vec![
                vec![
                    "btrfs".to_string(),
                    "subvolume".to_string(),
                    "create".to_string(),
                    root.join("prod").join("data").display().to_string(),
                ],
                vec![
                    "chmod".to_string(),
                    "0750".to_string(),
                    root.join("prod").join("data").display().to_string(),
                ],
                vec![
                    "chown".to_string(),
                    "999:999".to_string(),
                    root.join("prod").join("data").display().to_string(),
                ],
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn ensure_rejects_custom_mountpoint_before_mutation() {
        let root = std::env::temp_dir().join(format!(
            "ployz-btrfs-mountpoint-test-{}",
            std::process::id()
        ));
        let runner = FakeBtrfsRunner::default();
        let backend = BtrfsVolumeBackend::new(runner, root);

        let error = backend
            .ensure(EnsureVolumeRequest {
                identity: VolumeIdentity::new("prod", "data"),
                mountpoint: Some("/custom/path".into()),
                quota: None,
                mode: None,
                owner: None,
            })
            .await
            .expect_err("custom mountpoint is not identity-stable");

        assert_eq!(
            error.to_string(),
            "invalid volume request field 'mountpoint': btrfs backend derives mountpoints from volume identity"
        );
        assert!(backend.runner.calls.lock().expect("calls").is_empty());
    }

    #[tokio::test]
    async fn ensure_rejects_quota_before_mutation() {
        let root =
            std::env::temp_dir().join(format!("ployz-btrfs-quota-test-{}", std::process::id()));
        let runner = FakeBtrfsRunner::default();
        let backend = BtrfsVolumeBackend::new(runner, root);

        let error = backend
            .ensure(EnsureVolumeRequest {
                identity: VolumeIdentity::new("prod", "data"),
                mountpoint: None,
                quota: Some("1G".to_string()),
                mode: None,
                owner: None,
            })
            .await
            .expect_err("quota support needs explicit qgroup setup");

        assert_eq!(
            error.to_string(),
            "invalid volume request field 'quota': btrfs backend does not own qgroup setup yet"
        );
        assert!(backend.runner.calls.lock().expect("calls").is_empty());
    }

    #[tokio::test]
    async fn ensure_stops_when_mode_or_owner_constraint_fails() {
        for (label, failing_call, expected_operation) in [
            ("mode", 1_usize, "chmod btrfs volume"),
            ("owner", 2_usize, "chown btrfs volume"),
        ] {
            let root = std::env::temp_dir().join(format!(
                "ployz-btrfs-{label}-failure-test-{}",
                std::process::id()
            ));
            let runner = FakeBtrfsRunner::default();
            for call in 0..=failing_call {
                if call == failing_call {
                    runner.push_status(1, "constraint failed");
                } else {
                    runner.push_status(0, "");
                }
            }
            let backend = BtrfsVolumeBackend::new(runner, root.clone());

            let error = backend
                .ensure(EnsureVolumeRequest {
                    identity: VolumeIdentity::new("prod", "data"),
                    mountpoint: None,
                    quota: None,
                    mode: Some("0750".to_string()),
                    owner: Some("999:999".to_string()),
                })
                .await
                .expect_err("constraint failure should stop ensure");

            assert!(
                error
                    .to_string()
                    .contains(&format!("{expected_operation}: constraint failed")),
                "got: {error}"
            );
            let calls = backend.runner.calls.lock().expect("calls");
            assert_eq!(calls.len(), failing_call + 1);
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
