//! Concrete Caddy deployment wiring for the Ingress Proxy.

use oci_client::{
    Client, ParseError, Reference, errors::OciDistributionError, secrets::RegistryAuth,
};
use ployz_core::{IngressProxyBackend, IngressProxyFragment, MachineTarget, RequestedServiceSpec};
use semver::Version;
use thiserror::Error;

/// Failure while discovering the current Caddy image for ingress deployment.
#[derive(Debug, Error)]
pub enum IngressImageError {
    /// The configured image reference could not be parsed.
    #[error("parse Caddy image reference: {0}")]
    Reference(#[from] ParseError),
    /// Docker Hub tags could not be listed.
    #[error("list Docker Hub Caddy tags: {0}")]
    ListTags(#[from] OciDistributionError),
}

/// Discover the latest stable Caddy 2 image used by the current ingress backend.
///
/// # Errors
///
/// Returns [`IngressImageError`] when the image reference is invalid or Docker
/// Hub cannot list its tags.
pub async fn latest_image() -> Result<String, IngressImageError> {
    let reference = "docker.io/library/caddy:latest".parse::<Reference>()?;
    let response = Client::default()
        .list_tags(&reference, &RegistryAuth::Anonymous, None, None)
        .await?;
    Ok(select_image(&response.tags))
}

#[must_use]
fn select_image(tags: &[String]) -> String {
    tags.iter()
        .filter_map(|tag| {
            Version::parse(tag).ok().filter(|version| {
                version.major == 2
                    && version.pre.is_empty()
                    && version.build.is_empty()
                    && version.to_string() == *tag
            })
        })
        .max()
        .map_or_else(
            || "caddy:latest".into(),
            |version| format!("caddy:{version}"),
        )
}

#[must_use]
/// Build the concrete Caddy Service Spec behind the neutral ingress identity.
pub fn service_spec(
    image: String,
    machines: Vec<MachineTarget>,
    caddy_config: Option<String>,
) -> RequestedServiceSpec {
    let fragment = caddy_config
        .filter(|config| !config.trim().is_empty())
        .map(IngressProxyFragment::parse_caddy)
        .transpose()
        .expect("non-empty Caddy configuration is valid");
    IngressProxyBackend::Caddy
        .requested_service_spec(image, machines, fragment)
        .expect("Caddy profile accepts Caddy fragments")
}

#[cfg(test)]
mod tests {
    use ployz_core::{PortPublication, ServiceMode, ServiceName, TransportProtocol};

    use super::*;

    #[test]
    fn selects_only_the_greatest_bare_two_x_y_tag() {
        assert_eq!(
            select_image(&[
                "2.9.1".into(),
                "2.10.0".into(),
                "2.11.0-rc.1".into(),
                "2.10".into(),
                "latest".into(),
                "3.0.0".into(),
            ]),
            "caddy:2.10.0"
        );
        assert_eq!(select_image(&["latest".into()]), "caddy:latest");
    }

    #[test]
    fn service_spec_uses_neutral_identity_roots_and_concrete_caddy_wiring() {
        let spec = service_spec("caddy:2.10.0".into(), Vec::new(), None);

        assert_eq!(spec.name, ServiceName::parse("ingress").unwrap());
        assert_eq!(spec.mode, ServiceMode::Global);
        assert_eq!(
            spec.container.command,
            ["caddy", "run", "-c", "/config/caddy/Caddyfile"]
        );
        assert_eq!(
            spec.container
                .environment
                .get("CADDY_ADMIN")
                .map(String::as_str),
            Some("unix//run/ingress/caddy/admin.sock")
        );
        assert_eq!(spec.ports.len(), 3);
        assert!(matches!(
            spec.ports.get(2),
            Some(PortPublication::Host {
                transport_protocol: TransportProtocol::Udp,
                ..
            })
        ));
        assert_eq!(spec.mounts().len(), 3);
        assert_eq!(
            spec.volume_graph()
                .volumes()
                .iter()
                .filter_map(|volume| match volume.source.kind() {
                    ployz_core::RawVolumeSource::Bind { machine_path, .. } =>
                        Some(machine_path.as_str()),
                    ployz_core::RawVolumeSource::External { .. }
                    | ployz_core::RawVolumeSource::Ordinary { .. }
                    | ployz_core::RawVolumeSource::Provisioned { .. }
                    | ployz_core::RawVolumeSource::Tmpfs { .. } => None,
                })
                .collect::<Vec<_>>(),
            ["/var/lib/ployz/ingress", "/run/ployz/ingress"]
        );
        assert!(spec.config_graph().mounts().is_empty());
    }
}
