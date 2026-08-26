//! Ingress Proxy identity and deployment boundaries.

use ployz_core::{
    ContainerObservation, IngressProxyBackend, MachineTarget, QualifiedService,
    RequestedServiceSpec,
};
use thiserror::Error;

mod caddy;
mod envoy;
mod zentinel;

pub use caddy::IngressImageError;
pub use envoy::ENVOY_IMAGE;
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
    /// Caddy-only configuration was supplied for an Envoy Cluster.
    #[error("Caddy configuration cannot be deployed to the Envoy Ingress Proxy Backend")]
    CaddyFragmentOnEnvoy,
}

/// Build exactly one concrete spec from the immutable Cluster backend.
///
/// # Errors
///
/// Returns when the Caddy image cannot be discovered or Caddy-only
/// configuration is supplied to a backend that refuses fragments.
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
        IngressProxyBackend::Envoy => {
            if caddy_config.is_some() {
                return Err(DeploymentError::CaddyFragmentOnEnvoy);
            }
            envoy::service_spec(image.unwrap_or_else(|| ENVOY_IMAGE.to_owned()), machines)
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
    use std::num::NonZeroU16;

    use ployz_core::{
        HostBind, IngressProxyBackend, MachineTarget, PortPublication, ServiceMode,
        TransportProtocol, UpdateOrder, VolumeSource,
    };

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
        let envoy =
            service_spec_for_backend(IngressProxyBackend::Envoy, None, machines.clone(), None)
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
        assert!(
            zentinel
                .volume_graph
                .volumes()
                .iter()
                .filter_map(|volume| match &volume.source {
                    VolumeSource::Bind { machine_path, .. } => Some(machine_path.as_str()),
                    VolumeSource::Named { .. } | VolumeSource::Tmpfs { .. } => None,
                })
                .eq(["/var/lib/ployz/ingress/zentinel"])
        );

        assert_eq!(envoy.name, QualifiedService::system_ingress().name);
        assert_eq!(envoy.mode, ServiceMode::Global);
        assert_eq!(envoy.container.image, ENVOY_IMAGE);
        assert_eq!(
            envoy.container.command,
            ["envoy", "-c", "/config/bootstrap.yaml"]
        );
        assert!(envoy.container.cap_add.is_empty());
        assert_eq!(envoy.update.order, None);
        assert_eq!(
            envoy.ports,
            [
                PortPublication::Host {
                    bind: HostBind::All,
                    published_port: NonZeroU16::new(80).unwrap(),
                    container_port: NonZeroU16::new(8080).unwrap(),
                    transport_protocol: TransportProtocol::Tcp,
                },
                PortPublication::Host {
                    bind: HostBind::All,
                    published_port: NonZeroU16::new(443).unwrap(),
                    container_port: NonZeroU16::new(8443).unwrap(),
                    transport_protocol: TransportProtocol::Tcp,
                },
            ]
        );
        assert!(envoy.ingress_proxy_fragment.is_none());
        assert!(
            envoy
                .volume_graph
                .volumes()
                .iter()
                .filter_map(|volume| match &volume.source {
                    VolumeSource::Bind { machine_path, .. } => Some(machine_path.as_str()),
                    VolumeSource::Named { .. } | VolumeSource::Tmpfs { .. } => None,
                })
                .eq(["/var/lib/ployz/ingress/envoy"])
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

    #[tokio::test]
    async fn envoy_refuses_a_caddy_fragment_before_building_a_spec() {
        let error = service_spec_for_backend(
            IngressProxyBackend::Envoy,
            None,
            Vec::new(),
            Some("example.test { respond ok }".into()),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("Caddy"), "{error}");
        assert!(error.to_string().contains("Envoy"), "{error}");
    }
}
