//! The images this daemon runs for the Ingress Proxy, and the reserved specs built
//! on them. The daemon pins both, so every Machine on one release runs the same
//! holder and Caddy, and no client chooses them.

use ployz_core::{
    IngressPorts, Namespace, QualifiedService, ResolvedServiceSpec, ResolvedUpdateConfig,
    ServiceId, caddy_service_spec, holder_service_spec,
};

/// Caddy v2.11.9, resolved 2026-11-07.
pub(crate) const CADDY: &str = "docker.io/library/caddy:2@sha256:ce773ec7a742ce15ee6bf2bb5f65882b9e2a4487b10733e6f1484f86e6b5d8bc";

/// The holder sleeps and owns the Ingress Proxy's network namespace and host ports.
pub(crate) const PAUSE: &str = "registry.k8s.io/pause:3.10@sha256:ee6521f290b2168b6e0935a181d4cff9be1ac3f505666ef0e3c98fae8199917a";

/// The two reserved Ingress door specs this daemon builds for the Public Door.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DoorSpecs {
    pub holder: ResolvedServiceSpec,
    pub proxy: ResolvedServiceSpec,
}

impl DoorSpecs {
    /// The holder publishing host ports 80 and 443, and the Caddy that joins it, for
    /// the Ingress Proxy Service `proxy`.
    #[must_use]
    pub(crate) fn public(proxy: ServiceId) -> Self {
        Self {
            holder: holder_spec(holder_service_id(proxy)),
            proxy: proxy_spec(proxy),
        }
    }

    /// Whether `spec` at `namespace` is a door spec this daemon builds: the Caddy
    /// for its own Service id, or the holder for any proxy id.
    #[must_use]
    pub(crate) fn built_here(namespace: &Namespace, spec: &ResolvedServiceSpec) -> bool {
        let identity = QualifiedService::new(namespace.clone(), spec.name.clone());
        if identity == QualifiedService::system_ingress() {
            *spec == proxy_spec(spec.service_id)
        } else if identity == QualifiedService::system_ingress_holder() {
            *spec == holder_spec(spec.service_id)
        } else {
            false
        }
    }
}

/// The holder's Service id, derived from the proxy's so every Machine agrees without
/// another allocated id. Domain-separated, so no other derivation collides with it.
fn holder_service_id(proxy: ServiceId) -> ServiceId {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"ployz-ingress-holder\0");
    hasher.update(proxy.as_str().as_bytes());
    let digest = hasher.finalize();
    ServiceId::parse(hex::encode(digest.get(..16).expect("SHA-256 has 32 bytes")))
        .expect("16 bytes of hex are a Service id")
}

fn holder_spec(service_id: ServiceId) -> ResolvedServiceSpec {
    holder_service_spec(PAUSE, IngressPorts::Public)
        .to_resolved(service_id, ResolvedUpdateConfig::default())
        .expect("the built-in holder has no mounts")
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
    fn built_here_accepts_only_the_daemons_own_door_specs() {
        let specs = DoorSpecs::public(proxy_id());
        let system = Namespace::system();
        assert!(DoorSpecs::built_here(&system, &specs.holder));
        assert!(DoorSpecs::built_here(&system, &specs.proxy));

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
