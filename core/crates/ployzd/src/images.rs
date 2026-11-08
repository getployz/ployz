//! The images this daemon runs for the Ingress Proxy, and the reserved specs built
//! on them. The daemon pins all three, so every Machine on one release runs the same
//! holder, Caddy and Tunnel Connector, and no client chooses them.

use ployz_core::{
    Ciphertext, IngressPorts, Namespace, QualifiedService, ResolvedServiceSpec,
    ResolvedUpdateConfig, ServiceId, caddy_service_spec, holder_service_spec, tunnel_sealed_token,
    tunnel_service_spec,
};

/// Caddy v2.11.9, resolved 2026-11-07.
pub(crate) const CADDY: &str = "docker.io/library/caddy:2@sha256:ce773ec7a742ce15ee6bf2bb5f65882b9e2a4487b10733e6f1484f86e6b5d8bc";

/// The holder sleeps and owns the Ingress Proxy's network namespace and host ports.
pub(crate) const PAUSE: &str = "registry.k8s.io/pause:3.10@sha256:ee6521f290b2168b6e0935a181d4cff9be1ac3f505666ef0e3c98fae8199917a";

/// cloudflared 2026.10.7, resolved 2026-11-08. The Tunnel Connector.
pub(crate) const CLOUDFLARED: &str = "docker.io/cloudflare/cloudflared:2026.10.7@sha256:4955524aeaa379e154023d5a027c250f7b35041a2e7e950cf13f144bc9b9c5ba";

/// The reserved Ingress door specs this daemon builds for one set of Doors: the
/// holder, the Caddy that joins it, and the Tunnel Connector when the Tunnel Door
/// is open.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DoorSpecs {
    pub holder: ResolvedServiceSpec,
    pub proxy: ResolvedServiceSpec,
    pub tunnel: Option<ResolvedServiceSpec>,
}

impl DoorSpecs {
    /// The holder publishing host ports 80 and 443, and the Caddy that joins it, for
    /// the Ingress Proxy Service `proxy`.
    #[must_use]
    pub(crate) fn public(proxy: ServiceId) -> Self {
        Self::new(proxy, IngressPorts::Public, None)
    }

    /// The holder with `ports`, the Caddy that joins it, and the Tunnel Connector
    /// running the tunnel `sealed_token` names, when there is one.
    #[must_use]
    pub(crate) fn new(
        proxy: ServiceId,
        ports: IngressPorts,
        sealed_token: Option<&Ciphertext>,
    ) -> Self {
        Self {
            holder: holder_spec(holder_service_id(proxy), ports),
            proxy: proxy_spec(proxy),
            tunnel: sealed_token.map(|token| tunnel_spec(tunnel_service_id(proxy), token)),
        }
    }

    /// Whether `spec` at `namespace` is a door spec this daemon builds: the Caddy
    /// for its own Service id, the holder for any proxy id with either Door, or
    /// the Tunnel Connector for the sealed token it carries.
    #[must_use]
    pub(crate) fn built_here(namespace: &Namespace, spec: &ResolvedServiceSpec) -> bool {
        let identity = QualifiedService::new(namespace.clone(), spec.name.clone());
        if identity == QualifiedService::system_ingress() {
            *spec == proxy_spec(spec.service_id)
        } else if identity == QualifiedService::system_ingress_holder() {
            [IngressPorts::Public, IngressPorts::Closed]
                .into_iter()
                .any(|ports| *spec == holder_spec(spec.service_id, ports))
        } else if identity == QualifiedService::system_tunnel() {
            tunnel_sealed_token(&spec.container.labels)
                .is_some_and(|token| *spec == tunnel_spec(spec.service_id, &token))
        } else {
            false
        }
    }
}

/// The holder's Service id, derived from the proxy's so every Machine agrees without
/// another allocated id. Domain-separated, so no other derivation collides with it.
fn holder_service_id(proxy: ServiceId) -> ServiceId {
    derived_service_id(b"ployz-ingress-holder\0", proxy)
}

/// The Tunnel Connector's Service id, derived like the holder's.
fn tunnel_service_id(proxy: ServiceId) -> ServiceId {
    derived_service_id(b"ployz-ingress-tunnel\0", proxy)
}

fn derived_service_id(domain: &[u8], proxy: ServiceId) -> ServiceId {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(proxy.as_str().as_bytes());
    let digest = hasher.finalize();
    ServiceId::parse(hex::encode(digest.get(..16).expect("SHA-256 has 32 bytes")))
        .expect("16 bytes of hex are a Service id")
}

fn holder_spec(service_id: ServiceId, ports: IngressPorts) -> ResolvedServiceSpec {
    holder_service_spec(PAUSE, ports)
        .to_resolved(service_id, ResolvedUpdateConfig::default())
        .expect("the built-in holder has no mounts")
}

fn tunnel_spec(service_id: ServiceId, sealed_token: &Ciphertext) -> ResolvedServiceSpec {
    tunnel_service_spec(CLOUDFLARED, sealed_token)
        .to_resolved(service_id, ResolvedUpdateConfig::default())
        .expect("the built-in connector mounts are valid")
}

fn proxy_spec(service_id: ServiceId) -> ResolvedServiceSpec {
    caddy_service_spec(CADDY)
        .to_resolved(service_id, ResolvedUpdateConfig::default())
        .expect("the built-in Caddy mounts are valid")
}

/// The Caddy an older CLI deploys: it publishes the host ports itself and joins no
/// holder. Tests build it to stand in for a legacy Deploy or a pre-upgrade Container.
#[cfg(test)]
pub(crate) fn legacy_caddy_for_tests(image: &str) -> ployz_core::RequestedServiceSpec {
    let mut spec = caddy_service_spec(image);
    spec.container.network = None;
    spec.ports = holder_service_spec(image, IngressPorts::Public).ports;
    spec
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{IngressWiring, validate_ingress_service_spec};

    #[test]
    fn the_legacy_fixture_is_the_older_clis_caddy() {
        let legacy = legacy_caddy_for_tests("caddy:2")
            .to_resolved(proxy_id(), ResolvedUpdateConfig::default())
            .unwrap();
        assert_eq!(
            validate_ingress_service_spec(&legacy).ok(),
            Some(IngressWiring::LegacyProxy)
        );
    }

    fn proxy_id() -> ServiceId {
        ServiceId::parse("c".repeat(32)).unwrap()
    }

    #[test]
    fn the_public_doors_are_deterministic_and_canonical() {
        let specs = DoorSpecs::public(proxy_id());
        assert_eq!(specs, DoorSpecs::public(proxy_id()));
        assert_ne!(specs.holder.service_id, specs.proxy.service_id);
        assert_ne!(
            specs.holder.service_id,
            DoorSpecs::public(ServiceId::parse("d".repeat(32)).unwrap())
                .holder
                .service_id,
            "another proxy id derives another holder id"
        );
        assert_eq!(
            validate_ingress_service_spec(&specs.holder).ok(),
            Some(IngressWiring::Holder(IngressPorts::Public))
        );
        assert_eq!(
            validate_ingress_service_spec(&specs.proxy).ok(),
            Some(IngressWiring::JoinedProxy)
        );
    }

    #[test]
    fn the_tunnel_doors_close_the_holder_and_add_the_connector() {
        let token = Ciphertext::from_bytes(vec![7; 48]);
        let specs = DoorSpecs::new(proxy_id(), IngressPorts::Closed, Some(&token));
        assert_eq!(
            validate_ingress_service_spec(&specs.holder).ok(),
            Some(IngressWiring::Holder(IngressPorts::Closed))
        );
        let tunnel = specs.tunnel.as_ref().unwrap();
        assert_eq!(
            validate_ingress_service_spec(tunnel).ok(),
            Some(IngressWiring::Tunnel)
        );
        assert_eq!(tunnel_sealed_token(&tunnel.container.labels), Some(token));
        assert_ne!(tunnel.service_id, specs.holder.service_id);
        assert_ne!(tunnel.service_id, specs.proxy.service_id);
        assert_eq!(
            DoorSpecs::new(proxy_id(), IngressPorts::Public, None),
            DoorSpecs::public(proxy_id())
        );
    }

    #[test]
    fn built_here_accepts_only_the_daemons_own_door_specs() {
        let specs = DoorSpecs::public(proxy_id());
        let system = Namespace::system();
        assert!(DoorSpecs::built_here(&system, &specs.holder));
        assert!(DoorSpecs::built_here(&system, &specs.proxy));
        let token = Ciphertext::from_bytes(vec![7; 48]);
        let tunnel = DoorSpecs::new(proxy_id(), IngressPorts::Closed, Some(&token));
        assert!(DoorSpecs::built_here(&system, &tunnel.holder));
        assert!(DoorSpecs::built_here(
            &system,
            tunnel.tunnel.as_ref().unwrap()
        ));
        let mut other_origin = tunnel.tunnel.unwrap();
        other_origin.container.command.pop();
        assert!(!DoorSpecs::built_here(&system, &other_origin));

        let mut other_image = specs.proxy.clone();
        other_image.container.image = "caddy:2.11.4".into();
        assert!(!DoorSpecs::built_here(&system, &other_image));

        let mut with_command = specs.holder.clone();
        with_command.container.command = vec!["sleep".into(), "infinity".into()];
        assert!(!DoorSpecs::built_here(&system, &with_command));

        let app = Namespace::parse("app").unwrap();
        assert!(!DoorSpecs::built_here(&app, &specs.proxy));
    }
}
