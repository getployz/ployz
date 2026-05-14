use std::sync::Arc;

use ployz_volume_api::{
    EnsureVolumeRequest, Result, SnapshotRequest, VolumeBackend, VolumeBackendInfo,
    VolumeCapability, VolumeIdentity, VolumeInspection, VolumeMount, VolumeSnapshot,
};

#[derive(Clone)]
pub struct VolumeService {
    backend: Arc<dyn VolumeBackend>,
}

impl VolumeService {
    #[must_use]
    pub fn new(backend: Arc<dyn VolumeBackend>) -> Self {
        Self { backend }
    }

    #[must_use]
    pub fn backend_info(&self) -> VolumeBackendInfo {
        self.backend.info()
    }

    pub async fn ensure(&self, request: EnsureVolumeRequest) -> Result<VolumeMount> {
        let info = self.backend.info();
        info.capabilities
            .require(&info.id, VolumeCapability::Ensure)?;
        require_ensure_constraints(&info, &request)?;
        self.backend.ensure(request).await
    }

    pub async fn remove(&self, identity: VolumeIdentity) -> Result<()> {
        let info = self.backend.info();
        info.capabilities
            .require(&info.id, VolumeCapability::Remove)?;
        self.backend.remove(identity).await
    }

    pub async fn mount(&self, identity: VolumeIdentity) -> Result<VolumeMount> {
        let info = self.backend.info();
        info.capabilities
            .require(&info.id, VolumeCapability::Mount)?;
        self.backend.mount(identity).await
    }

    pub async fn inspect(&self, identity: VolumeIdentity) -> Result<Option<VolumeInspection>> {
        let info = self.backend.info();
        info.capabilities
            .require(&info.id, VolumeCapability::Inspect)?;
        self.backend.inspect(identity).await
    }

    pub async fn snapshot(&self, request: SnapshotRequest) -> Result<VolumeSnapshot> {
        let info = self.backend.info();
        info.capabilities
            .require(&info.id, VolumeCapability::Snapshot)?;
        self.backend.snapshot(request).await
    }
}

fn require_ensure_constraints(
    info: &VolumeBackendInfo,
    request: &EnsureVolumeRequest,
) -> Result<()> {
    if request.mountpoint.is_some() {
        info.capabilities
            .require(&info.id, VolumeCapability::CustomMountpoint)?;
    }
    if request.quota.is_some() {
        info.capabilities
            .require(&info.id, VolumeCapability::EnforceQuota)?;
    }
    if request.mode.is_some() {
        info.capabilities
            .require(&info.id, VolumeCapability::EnforceMode)?;
    }
    if request.owner.is_some() {
        info.capabilities
            .require(&info.id, VolumeCapability::EnforceOwner)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use ployz_volume_api::{VolumeBackendKind, VolumeCapabilities, VolumeError, VolumeSnapshot};

    use super::*;

    #[derive(Debug)]
    struct FakeBackend {
        info: VolumeBackendInfo,
        ensure_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl VolumeBackend for FakeBackend {
        fn info(&self) -> VolumeBackendInfo {
            self.info.clone()
        }

        async fn ensure(&self, request: EnsureVolumeRequest) -> Result<VolumeMount> {
            self.ensure_calls.fetch_add(1, Ordering::SeqCst);
            Ok(VolumeMount::host_path(request.identity, "/tmp/fake".into()))
        }

        async fn remove(&self, _identity: VolumeIdentity) -> Result<()> {
            Ok(())
        }

        async fn mount(&self, identity: VolumeIdentity) -> Result<VolumeMount> {
            Ok(VolumeMount::host_path(identity, "/tmp/fake".into()))
        }

        async fn inspect(&self, _identity: VolumeIdentity) -> Result<Option<VolumeInspection>> {
            Ok(None)
        }

        async fn snapshot(&self, request: SnapshotRequest) -> Result<VolumeSnapshot> {
            Ok(VolumeSnapshot {
                identity: request.identity,
                snapshot: request.snapshot,
            })
        }
    }

    fn service_with(capabilities: VolumeCapabilities) -> VolumeService {
        service_with_calls(capabilities).0
    }

    fn service_with_calls(capabilities: VolumeCapabilities) -> (VolumeService, Arc<AtomicUsize>) {
        let ensure_calls = Arc::new(AtomicUsize::new(0));
        let service = VolumeService::new(Arc::new(FakeBackend {
            info: VolumeBackendInfo {
                id: "fake".to_string(),
                kind: VolumeBackendKind::Other("fake".to_string()),
                capabilities,
            },
            ensure_calls: ensure_calls.clone(),
        }));
        (service, ensure_calls)
    }

    #[tokio::test]
    async fn preflights_backend_capability_before_snapshot() {
        let service = service_with(VolumeCapabilities::docker_local());

        let error = service
            .snapshot(SnapshotRequest {
                identity: VolumeIdentity::new("prod", "data"),
                snapshot: "before-move".to_string(),
            })
            .await
            .expect_err("docker-local style backend should not snapshot");

        assert_eq!(
            error,
            VolumeError::UnsupportedCapability {
                backend: "fake".to_string(),
                capability: VolumeCapability::Snapshot,
            }
        );
    }

    #[tokio::test]
    async fn calls_backend_when_capability_is_present() {
        let service = service_with(VolumeCapabilities::zfs_like());

        let snapshot = service
            .snapshot(SnapshotRequest {
                identity: VolumeIdentity::new("prod", "data"),
                snapshot: "before-move".to_string(),
            })
            .await
            .expect("snapshot");

        assert_eq!(snapshot.snapshot, "before-move");
    }

    #[tokio::test]
    async fn ensure_preflights_filesystem_constraints_before_backend_mutation() {
        let (service, ensure_calls) = service_with_calls(VolumeCapabilities::docker_local());

        let error = service
            .ensure(EnsureVolumeRequest {
                identity: VolumeIdentity::new("prod", "data"),
                mountpoint: None,
                quota: Some("1G".to_string()),
                mode: None,
                owner: None,
            })
            .await
            .expect_err("docker-local style backend should not enforce quota");

        assert_eq!(
            error,
            VolumeError::UnsupportedCapability {
                backend: "fake".to_string(),
                capability: VolumeCapability::EnforceQuota,
            }
        );
        assert_eq!(ensure_calls.load(Ordering::SeqCst), 0);
    }
}
