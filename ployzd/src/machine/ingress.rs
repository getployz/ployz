//! Reserved Ingress Proxy container policy at the Machine trust boundary.

use ployz_core::{
    ContainerKind, IngressProxyNetworkMode, ProjectName, QualifiedService, ResolvedServiceSpec,
};

use super::{LocalMachine, LocalMachineError};
use crate::docker::NetworkAttachment;

/// Failure while preparing backend-owned runtime state before container creation.
#[derive(Debug, thiserror::Error)]
#[error("cannot prepare Ingress Proxy runtime: {source}")]
pub struct IngressRuntimeError {
    #[source]
    source: crate::ingress::zentinel::Error,
}

impl From<crate::ingress::zentinel::Error> for IngressRuntimeError {
    fn from(source: crate::ingress::zentinel::Error) -> Self {
        Self { source }
    }
}

impl LocalMachine {
    /// Prepare and return the runtime wiring authorized for one container creation.
    ///
    /// # Errors
    ///
    /// Returns when the Cluster backend is absent, invalid, or differs from
    /// the reserved Service's concrete wiring, or when Zentinel's initial
    /// configuration cannot be installed before host networking is granted.
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
        if matches!(network, NetworkAttachment::Bridge) {
            return Ok(network);
        }

        let _guard = self.ingress_runtime_lock.lock().await;
        let (machine, config_file) = {
            let store = self.lock_store()?;
            let machine = store
                .record()
                .machine()
                .cloned()
                .ok_or(LocalMachineError::NotParticipating)?;
            let config_file = crate::ingress::zentinel::config_path(&store.data_dir);
            (machine, config_file)
        };
        crate::ingress::zentinel::write_initial_config(&machine, &config_file)
            .map_err(IngressRuntimeError::from)?;
        Ok(network)
    }
}
