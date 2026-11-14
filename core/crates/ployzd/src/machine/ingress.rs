//! Reserved Ingress door and Private Access router validation at the Machine trust
//! boundary.

use ipnet::Ipv4Net;
use ployz_core::{IngressProxyServiceSpecError, Namespace, ResolvedServiceSpec};

use super::LocalMachineError;
use crate::images::{DoorSpecs, private_access_built_here};

/// Admit a spec at the Private Access router identity: only the router this daemon
/// builds for `network`, the Cluster network.
///
/// # Errors
///
/// Returns when the spec is not the router this daemon built.
pub(crate) fn admit_router(
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    network: Ipv4Net,
) -> Result<(), LocalMachineError> {
    if private_access_built_here(namespace, spec, network) {
        Ok(())
    } else {
        Err(LocalMachineError::PrivateAccessRouterSpec)
    }
}

/// Admit a spec at a reserved Ingress door identity: only a spec this daemon built.
///
/// # Errors
///
/// Returns when the spec is not one this daemon built.
pub(crate) fn admit_door(
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
) -> Result<(), LocalMachineError> {
    if DoorSpecs::built_here(namespace, spec) {
        Ok(())
    } else {
        Err(IngressProxyServiceSpecError.into())
    }
}

/// Admit a spec at any identity that is not reserved: it may not join a network.
///
/// # Errors
///
/// Returns when the spec joins a network from outside the reserved doors.
pub(crate) fn admit_unreserved(spec: &ResolvedServiceSpec) -> Result<(), LocalMachineError> {
    if spec.container.network.is_some() {
        Err(IngressProxyServiceSpecError.into())
    } else {
        Ok(())
    }
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
        assert!(admit_door(&Namespace::system(), &specs.holder).is_ok());
        assert!(admit_door(&Namespace::system(), &specs.proxy).is_ok());
    }

    #[test]
    fn a_door_the_daemon_did_not_build_is_refused() {
        let legacy = crate::images::legacy_caddy_for_tests("caddy:2")
            .to_resolved(
                ServiceId::parse("c".repeat(32)).unwrap(),
                Default::default(),
            )
            .unwrap();
        assert!(admit_door(&Namespace::system(), &legacy).is_err());
        let mut other = specs().proxy;
        other.container.image = "caddy:2".into();
        assert!(admit_door(&Namespace::system(), &other).is_err());
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
        assert!(admit_router(&Namespace::system(), &spec, network).is_ok());
        let other: Ipv4Net = "10.211.0.0/16".parse().unwrap();
        assert!(admit_router(&Namespace::system(), &spec, other).is_err());
        let mut swapped = spec.clone();
        swapped.container.image = "tailscale/tailscale:latest".into();
        assert!(admit_router(&Namespace::system(), &swapped, network).is_err());
        let mut mounted = spec;
        mounted.container.environment.insert(
            "TS_ROUTES".into(),
            ployz_core::EnvValue::Text("0.0.0.0/0".into()),
        );
        assert!(admit_router(&Namespace::system(), &mounted, network).is_err());
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
            assert!(admit_door(&system, own).is_ok(), "{}", own.name);
            let mut foreign = own.clone();
            foreign.container.image = "example.test/other:1".into();
            let refused = admit_door(&system, &foreign).unwrap_err();
            assert!(
                matches!(refused, LocalMachineError::IngressProxyServiceSpec(_)),
                "{}",
                own.name
            );
            assert_eq!(refused.to_string(), door_words, "{}", own.name);
        }
        let mut renamed = tunnel.holder.clone();
        renamed.name = pair.connector.name.clone();
        let refused = admit_door(&system, &renamed).unwrap_err();
        assert_eq!(refused.to_string(), door_words);

        let network: Ipv4Net = "10.210.0.0/16".parse().unwrap();
        let other: Ipv4Net = "10.211.0.0/16".parse().unwrap();
        let refused = admit_router(&system, &router(network), other).unwrap_err();
        assert!(matches!(
            refused,
            LocalMachineError::PrivateAccessRouterSpec
        ));
        assert_eq!(refused.to_string(), router_words);
        let mut door_under_the_router_name = tunnel.holder;
        door_under_the_router_name.name = router(network).name;
        let refused = admit_router(&system, &door_under_the_router_name, network).unwrap_err();
        assert_eq!(refused.to_string(), router_words);
        let refused = admit_unreserved(&pair.connector).unwrap_err();
        assert_eq!(refused.to_string(), door_words);
    }

    #[test]
    fn an_app_spec_may_not_join_the_holders_network() {
        let mut app = specs().proxy;
        app.name = "web".parse().unwrap();
        assert!(admit_unreserved(&app).is_err());
        app.container.network = None;
        assert!(admit_unreserved(&app).is_ok());
        app.container.network = Some(ServiceNetwork::Unrecognized);
        assert!(admit_unreserved(&app).is_err());
    }
}
