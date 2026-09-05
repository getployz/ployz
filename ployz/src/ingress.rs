//! Ingress Proxy identity and deployment boundaries.

use ployz_core::{
    ContainerObservation, IngressProxyFragment, MachineTarget, QualifiedService,
    RequestedServiceSpec, caddy_service_spec,
};

mod caddy;
pub use caddy::IngressImageError;

/// Build the Caddy ingress Service Spec.
///
/// # Errors
///
/// Returns when the Caddy image cannot be discovered.
pub async fn service_spec(
    image: Option<String>,
    machines: Vec<MachineTarget>,
    fragment: Option<IngressProxyFragment>,
) -> Result<RequestedServiceSpec, IngressImageError> {
    let image = match image {
        Some(image) => image,
        None => caddy::latest_image().await?,
    };
    Ok(caddy_service_spec(image, machines, fragment))
}

/// True when this observation is the reserved Ingress Proxy Service.
#[must_use]
pub fn is_system_ingress(observation: &ContainerObservation) -> bool {
    observation.identity() == QualifiedService::system_ingress()
}

#[cfg(test)]
mod tests {
    use ployz_core::{IngressProxyFragment, MachineTarget, ServiceMode};

    use super::*;

    #[tokio::test]
    async fn builds_the_caddy_service_spec() {
        let machines = vec![MachineTarget::parse("edge").unwrap()];
        let caddy = service_spec(
            Some("registry.test/caddy@sha256:caddy".into()),
            machines.clone(),
            Some(IngressProxyFragment::parse("{ admin off }").unwrap()),
        )
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
        assert_eq!(
            caddy
                .ingress_proxy_fragment
                .as_ref()
                .map(IngressProxyFragment::as_str),
            Some("{ admin off }")
        );
    }
}
