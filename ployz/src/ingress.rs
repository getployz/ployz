//! Ingress Proxy identity and deployment boundaries.

use ployz_core::{
    ContainerObservation, IngressProxyBackend, MachineTarget, QualifiedService,
    RequestedServiceSpec,
};
use thiserror::Error;

mod caddy;
mod zentinel;

pub use caddy::IngressImageError;
pub use zentinel::ZENTINEL_IMAGE;

/// Failure while selecting one concrete Ingress Proxy deployment.
#[derive(Debug, Error)]
pub enum DeploymentError {
    /// Caddy's release image could not be discovered.
    #[error(transparent)]
    Image(#[from] IngressImageError),
    /// Caddy-only configuration was supplied for a Zentinel Cluster.
    #[error("Caddy configuration cannot be deployed to the Zentinel Ingress Proxy Backend")]
    CaddyFragmentOnZentinel,
}

/// Build exactly one concrete spec from the immutable Cluster backend.
///
/// # Errors
///
/// Returns when the Caddy image cannot be discovered or Caddy-only
/// configuration is supplied to Zentinel.
pub async fn service_spec_for_backend(
    backend: IngressProxyBackend,
    image: Option<String>,
    machines: Vec<MachineTarget>,
    caddy_config: Option<String>,
) -> Result<RequestedServiceSpec, DeploymentError> {
    Ok(match backend {
        IngressProxyBackend::Caddy => caddy::service_spec(
            match image {
                Some(image) => image,
                None => caddy::latest_image().await?,
            },
            machines,
            caddy_config,
        ),
        IngressProxyBackend::Zentinel => {
            if caddy_config.is_some() {
                return Err(DeploymentError::CaddyFragmentOnZentinel);
            }
            zentinel::service_spec(image.unwrap_or_else(|| ZENTINEL_IMAGE.to_owned()), machines)
        }
    })
}

/// True when this observation is the reserved Ingress Proxy Service.
#[must_use]
pub fn is_system_ingress(observation: &ContainerObservation) -> bool {
    observation.identity() == QualifiedService::system_ingress()
}

#[cfg(test)]
mod tests {
    use ployz_core::{IngressProxyBackend, MachineTarget, ServiceMode, UpdateOrder, VolumeSource};

    use super::*;

    #[tokio::test]
    async fn selected_backend_builds_only_its_concrete_service_spec() {
        let machines = vec![MachineTarget::parse("edge").unwrap()];
        let caddy = service_spec_for_backend(
            IngressProxyBackend::Caddy,
            Some("registry.test/caddy@sha256:caddy".into()),
            machines.clone(),
            Some("{ admin off }".into()),
        )
        .await
        .unwrap();
        let zentinel =
            service_spec_for_backend(IngressProxyBackend::Zentinel, None, machines.clone(), None)
                .await
                .unwrap();

        assert_eq!(caddy.name, QualifiedService::system_ingress().name);
        assert_eq!(caddy.mode, ServiceMode::Global);
        assert_eq!(caddy.placement.machines, machines);
        assert_eq!(
            caddy.container.command,
            ["caddy", "run", "-c", "/config/caddy/Caddyfile"]
        );
        assert_eq!(caddy.ports.len(), 3);
        assert!(caddy.ingress_proxy_fragment.is_some());

        assert_eq!(zentinel.name, QualifiedService::system_ingress().name);
        assert_eq!(zentinel.mode, ServiceMode::Global);
        assert_eq!(zentinel.container.image, ZENTINEL_IMAGE);
        assert_eq!(zentinel.container.command, ["-c", "/config/zentinel.kdl"]);
        assert_eq!(zentinel.container.cap_add, ["NET_BIND_SERVICE"]);
        assert_eq!(zentinel.container.cap_drop, ["ALL"]);
        assert_eq!(zentinel.update.order, Some(UpdateOrder::StopFirst));
        assert!(zentinel.ports.is_empty());
        assert!(zentinel.ingress_proxy_fragment.is_none());
        assert_eq!(
            zentinel
                .volume_graph
                .volumes()
                .iter()
                .filter_map(|volume| match &volume.source {
                    VolumeSource::Bind { machine_path, .. } => Some(machine_path.as_str()),
                    VolumeSource::Named { .. } | VolumeSource::Tmpfs { .. } => None,
                })
                .collect::<Vec<_>>(),
            ["/var/lib/ployz/ingress/zentinel"]
        );
    }

    #[tokio::test]
    async fn zentinel_refuses_a_caddy_fragment_before_building_a_spec() {
        let error = service_spec_for_backend(
            IngressProxyBackend::Zentinel,
            None,
            Vec::new(),
            Some("example.test { respond ok }".into()),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("Caddy"), "{error}");
    }
}
