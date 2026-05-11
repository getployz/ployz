use std::net::SocketAddr;
use std::sync::Arc;

use ployz_dns_config::DnsConfig;
use ployz_gateway_config::GatewayConfig;
use ployz_runtime_api::{
    ImageArchiveReader, RuntimeHandle, RuntimeImage, RuntimeImageBackend, RuntimeImageError,
    RuntimeImageImportResult,
};
use ployz_runtime_backends::runtime::ContainerEngine;
use ployz_runtime_backends::storage::{TokioShellRunner, ZfsDriver};
use ployz_types::model::OverlayIp;

use super::DaemonState;
use crate::runtime_profile::{MeshBuildRequest, MeshRuntimeComponents};

impl DaemonState {
    pub(crate) async fn build_runtime_mesh_components(
        &self,
        request: MeshBuildRequest<'_>,
    ) -> Result<MeshRuntimeComponents, String> {
        self.runtime_profile.build_mesh_components(request).await
    }

    #[must_use]
    pub(crate) fn zfs_transfer_bind_addr(
        &self,
        zfs_transfer_port: u16,
        overlay_ip: OverlayIp,
    ) -> SocketAddr {
        self.runtime_profile
            .zfs_transfer_bind_addr(zfs_transfer_port, overlay_ip)
    }

    #[must_use]
    pub(crate) fn image_receiver_bind_addr(
        &self,
        zfs_transfer_port: u16,
        overlay_ip: OverlayIp,
    ) -> Option<SocketAddr> {
        self.runtime_profile
            .image_receiver_bind_addr(zfs_transfer_port, overlay_ip)
    }

    pub(crate) async fn start_runtime_gateway(
        &self,
        config: GatewayConfig,
    ) -> Result<Box<dyn RuntimeHandle>, String> {
        self.runtime_profile
            .start_gateway(config)
            .await
            .map(|handle| Box::new(handle) as Box<dyn RuntimeHandle>)
    }

    pub(crate) async fn start_runtime_dns(
        &self,
        config: DnsConfig,
    ) -> Result<Box<dyn RuntimeHandle>, String> {
        self.runtime_profile
            .start_dns(config)
            .await
            .map(|handle| Box::new(handle) as Box<dyn RuntimeHandle>)
    }

    #[must_use]
    pub(crate) fn runtime_is_memory_test(&self) -> bool {
        self.runtime_profile.is_memory_test()
    }

    pub(crate) async fn zfs_storage_driver(
        &self,
    ) -> Result<Option<Arc<ZfsDriver<TokioShellRunner>>>, String> {
        let Some(root) = self.storage.zfs_root.as_ref() else {
            return Ok(None);
        };
        let root = root
            .to_str()
            .ok_or_else(|| format!("storage zfs_root is not valid UTF-8: {}", root.display()))?;
        ZfsDriver::new(TokioShellRunner, root, self.storage.overcommit_ratio)
            .await
            .map(Arc::new)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn runtime_image_backend(
        &self,
    ) -> Result<Arc<dyn RuntimeImageBackend>, String> {
        if self.runtime_is_memory_test() {
            return Ok(Arc::new(UnsupportedRuntimeImageBackend));
        }
        ContainerEngine::connect()
            .await
            .map(|engine| Arc::new(engine) as Arc<dyn RuntimeImageBackend>)
            .map_err(|error| error.to_string())
    }
}

struct UnsupportedRuntimeImageBackend;

#[async_trait::async_trait]
impl RuntimeImageBackend for UnsupportedRuntimeImageBackend {
    async fn inspect_image(
        &self,
        reference: &str,
    ) -> Result<Option<RuntimeImage>, RuntimeImageError> {
        let _ = reference;
        Err(RuntimeImageError::unsupported("memory", "image inspect"))
    }

    async fn export_image_archive<'a>(
        &'a self,
        reference: &'a str,
    ) -> Result<ImageArchiveReader<'a>, RuntimeImageError> {
        let _ = reference;
        Err(RuntimeImageError::unsupported(
            "memory",
            "image archive export",
        ))
    }

    async fn import_image_archive(
        &self,
        archive: ImageArchiveReader<'static>,
    ) -> Result<RuntimeImageImportResult, RuntimeImageError> {
        let _ = archive;
        Err(RuntimeImageError::unsupported(
            "memory",
            "image archive import",
        ))
    }
}

#[cfg(test)]
mod tests {
    use ployz_runtime_api::Identity;
    use ployz_types::model::MachineId;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[tokio::test]
    async fn memory_runtime_image_backend_reports_unsupported_without_docker() {
        let state = DaemonState::new_for_tests(
            &unique_temp_dir("ployz-runtime-image-backend"),
            Identity::generate(MachineId("founder".into()), [41; 32]),
            "10.210.0.0/16".into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        );

        let backend = state.runtime_image_backend().await.expect("memory backend");
        let error = backend
            .inspect_image("example/app:latest")
            .await
            .expect_err("memory runtime should not inspect images");

        assert!(matches!(
            error,
            RuntimeImageError::UnsupportedCapability {
                backend: "memory",
                capability: "image inspect"
            }
        ));
    }

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}-{sequence}", std::process::id()))
    }
}
