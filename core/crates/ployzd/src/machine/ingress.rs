//! Reserved Ingress door and Private Access router validation at the Machine trust
//! boundary.

use ipnet::Ipv4Net;
use ployz_core::{IngressProxyServiceSpecError, Namespace, ResolvedServiceSpec};

use super::LocalMachineError;
use crate::images::{DoorSpecs, private_access_built_here};

/// Whether `namespace` and `spec` name the Private Access router, which is admitted
/// against the Cluster network.
pub(crate) fn is_private_access_router(namespace: &Namespace, spec: &ResolvedServiceSpec) -> bool {
    ployz_core::QualifiedService::new(namespace.clone(), spec.name.clone())
        .is_private_access_router()
}

/// Admit one Service Container spec immediately before creation: at a reserved
/// Ingress door or the Private Access router identity only a spec this daemon built
/// itself, and elsewhere only a spec in its own network. `network` is the Cluster
/// network the router spec is built for, and the caller reads it only for the router.
///
/// # Errors
///
/// Returns when the spec is a reserved identity this daemon did not build, or joins a
/// network from outside the reserved doors.
pub(crate) fn admit_reserved_door(
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    network: Option<Ipv4Net>,
) -> Result<(), LocalMachineError> {
    let identity = ployz_core::QualifiedService::new(namespace.clone(), spec.name.clone());
    if identity.is_private_access_router() {
        if !network.is_some_and(|network| private_access_built_here(namespace, spec, network)) {
            return Err(LocalMachineError::PrivateAccessRouterSpec);
        }
    } else if identity.is_ingress_door() {
        if !DoorSpecs::built_here(namespace, spec) {
            return Err(IngressProxyServiceSpecError.into());
        }
    } else if spec.container.network.is_some() {
        return Err(IngressProxyServiceSpecError.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{ServiceId, ServiceNetwork};

    fn specs() -> DoorSpecs {
        DoorSpecs::public(ServiceId::parse("c".repeat(32)).unwrap())
    }

    #[test]
    fn the_daemons_own_doors_are_admitted() {
        let specs = specs();
        assert!(admit_reserved_door(&Namespace::system(), &specs.holder, None).is_ok());
        assert!(admit_reserved_door(&Namespace::system(), &specs.proxy, None).is_ok());
    }

    #[test]
    fn a_door_the_daemon_did_not_build_is_refused() {
        let legacy = crate::images::legacy_caddy_for_tests("caddy:2")
            .to_resolved(
                ServiceId::parse("c".repeat(32)).unwrap(),
                Default::default(),
            )
            .unwrap();
        assert!(admit_reserved_door(&Namespace::system(), &legacy, None).is_err());
        let mut other = specs().proxy;
        other.container.image = "caddy:2".into();
        assert!(admit_reserved_door(&Namespace::system(), &other, None).is_err());
    }

    fn router(network: Ipv4Net) -> ResolvedServiceSpec {
        crate::images::private_access_router(
            network,
            &ployz_core::Ciphertext::from_bytes(vec![9; 48]),
            ployz_core::PrivateAccessStateId::random(),
        )
    }

    // Catches the router being admitted for any network, image or namespace other than
    // the one this daemon built it for: the key file is mounted into whatever runs.
    #[test]
    fn the_daemons_own_router_is_admitted_and_nothing_else_under_its_name() {
        let network: Ipv4Net = "10.210.0.0/16".parse().unwrap();
        let spec = router(network);
        assert!(admit_reserved_door(&Namespace::system(), &spec, Some(network)).is_ok());
        assert!(admit_reserved_door(&Namespace::system(), &spec, None).is_err());
        let other: Ipv4Net = "10.211.0.0/16".parse().unwrap();
        assert!(admit_reserved_door(&Namespace::system(), &spec, Some(other)).is_err());
        let mut swapped = spec.clone();
        swapped.container.image = "tailscale/tailscale:latest".into();
        assert!(admit_reserved_door(&Namespace::system(), &swapped, Some(network)).is_err());
        let mut mounted = spec;
        mounted.container.environment.insert(
            "TS_ROUTES".into(),
            ployz_core::EnvValue::Text("0.0.0.0/0".into()),
        );
        assert!(admit_reserved_door(&Namespace::system(), &mounted, Some(network)).is_err());
    }

    // Catches a refusal naming the wrong kind of spec: the wire text differs between a
    // door and the router, and each reserved identity is admitted only as the daemon's own.
    #[test]
    fn each_reserved_identity_is_refused_in_its_own_words() {
        let door_words = "this isn't an Ingress Proxy spec Ployz builds";
        let router_words = "This isn't a router spec Ployz builds.";
        let token = ployz_core::Ciphertext::from_bytes(vec![7; 48]);
        let id = ServiceId::parse("c".repeat(32)).unwrap();
        let tunnel = DoorSpecs::new(id, ployz_core::IngressPorts::Closed, Some(&token));
        let pair = tunnel.tunnel.clone().unwrap();
        let system = Namespace::system();
        for own in [&tunnel.holder, &tunnel.proxy, &pair.origin, &pair.connector] {
            assert!(
                admit_reserved_door(&system, own, None).is_ok(),
                "{}",
                own.name
            );
            let mut foreign = own.clone();
            foreign.container.image = "example.test/other:1".into();
            let refused = admit_reserved_door(&system, &foreign, None).unwrap_err();
            assert!(
                matches!(refused, LocalMachineError::IngressProxyServiceSpec(_)),
                "{}",
                own.name
            );
            assert_eq!(refused.to_string(), door_words, "{}", own.name);
        }
        let mut renamed = tunnel.holder.clone();
        renamed.name = pair.connector.name.clone();
        let refused = admit_reserved_door(&system, &renamed, None).unwrap_err();
        assert_eq!(refused.to_string(), door_words);

        let network: Ipv4Net = "10.210.0.0/16".parse().unwrap();
        let refused = admit_reserved_door(&system, &router(network), None).unwrap_err();
        assert!(matches!(
            refused,
            LocalMachineError::PrivateAccessRouterSpec
        ));
        assert_eq!(refused.to_string(), router_words);
        let mut door_under_the_router_name = tunnel.holder;
        door_under_the_router_name.name = router(network).name;
        let refused =
            admit_reserved_door(&system, &door_under_the_router_name, Some(network)).unwrap_err();
        assert_eq!(refused.to_string(), router_words);
        let refused =
            admit_reserved_door(&Namespace::parse("app").unwrap(), &router(network), None)
                .and_then(|()| {
                    admit_reserved_door(&Namespace::parse("app").unwrap(), &pair.connector, None)
                })
                .unwrap_err();
        assert_eq!(refused.to_string(), door_words);
    }

    #[test]
    fn an_app_spec_may_not_join_the_holders_network() {
        let mut app = specs().proxy;
        app.name = "web".parse().unwrap();
        assert!(admit_reserved_door(&Namespace::parse("app").unwrap(), &app, None).is_err());
        app.container.network = None;
        assert!(admit_reserved_door(&Namespace::parse("app").unwrap(), &app, None).is_ok());
        app.container.network = Some(ServiceNetwork::Unrecognized);
        assert!(admit_reserved_door(&Namespace::parse("app").unwrap(), &app, None).is_err());
    }
}
