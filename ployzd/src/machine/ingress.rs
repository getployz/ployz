//! Reserved Ingress Proxy container policy at the Machine trust boundary.

use std::io;

use ployz_core::{
    ContainerKind, IngressProxyBackend, IngressProxyNetworkMode, ProjectName, QualifiedService,
    ResolvedServiceSpec,
};

use super::{LocalMachine, LocalMachineError};
use crate::docker::NetworkAttachment;

/// Failure while preparing backend-owned runtime state before container creation.
#[derive(Debug, thiserror::Error)]
#[error("cannot prepare Ingress Proxy runtime: {source}")]
pub struct IngressRuntimeError {
    #[source]
    source: io::Error,
}

impl From<io::Error> for IngressRuntimeError {
    fn from(source: io::Error) -> Self {
        Self { source }
    }
}

impl From<crate::ingress::zentinel::Error> for IngressRuntimeError {
    fn from(source: crate::ingress::zentinel::Error) -> Self {
        Self {
            source: io::Error::other(source),
        }
    }
}

impl From<crate::ingress::envoy::Error> for IngressRuntimeError {
    fn from(source: crate::ingress::envoy::Error) -> Self {
        Self {
            source: io::Error::other(source),
        }
    }
}

impl LocalMachine {
    /// Prepare and return the runtime wiring authorized for one container creation.
    ///
    /// # Errors
    ///
    /// Returns when the Cluster backend is absent, invalid, or differs from
    /// the reserved Service's concrete wiring, or when a backend's initial
    /// configuration cannot be installed before the process starts.
    pub(crate) async fn prepare_service_runtime(
        &self,
        kind: ContainerKind,
        project: &ProjectName,
        spec: &ResolvedServiceSpec,
    ) -> Result<NetworkAttachment, LocalMachineError> {
        if QualifiedService::new(project.clone(), spec.name.clone())
            != QualifiedService::system_ingress()
        {
            return Ok(NetworkAttachment::Bridge);
        }
        let backend = ployz_core::ingress_proxy_backend(spec)?;
        self.replicated()?
            .require_ingress_proxy_backend(backend)
            .await
            .map_err(LocalMachineError::IngressProxyBackend)?;
        let network = match (kind, backend.network_mode()) {
            (ContainerKind::ServiceContainer, IngressProxyNetworkMode::Host) => {
                NetworkAttachment::Host
            }
            (ContainerKind::ServiceContainer, IngressProxyNetworkMode::Bridge)
            | (ContainerKind::PreDeployHook, _) => NetworkAttachment::Bridge,
        };
        if kind != ContainerKind::ServiceContainer {
            return Ok(network);
        }
        match backend {
            IngressProxyBackend::Caddy => return Ok(network),
            IngressProxyBackend::Envoy | IngressProxyBackend::Zentinel => {}
        }
        let _guard = self.ingress_runtime_lock.lock().await;
        let (machine, data_dir) = {
            let store = self.lock_store()?;
            let machine = store
                .record()
                .machine()
                .cloned()
                .ok_or(LocalMachineError::NotParticipating)?;
            (machine, store.data_dir.clone())
        };
        match backend {
            IngressProxyBackend::Caddy => {
                unreachable!("Caddy returned before the initial-config lock")
            }
            IngressProxyBackend::Envoy => crate::ingress::envoy::write_initial_config(
                &machine,
                &crate::ingress::envoy::config_path(&data_dir),
            )
            .map_err(IngressRuntimeError::from)?,
            IngressProxyBackend::Zentinel => crate::ingress::zentinel::write_initial_config(
                &machine,
                &crate::ingress::zentinel::config_path(&data_dir),
            )
            .map_err(IngressRuntimeError::from)?,
        }
        Ok(network)
    }
}
