//! Reserved Ingress door validation at the Machine trust boundary.

use ployz_core::{IngressProxyServiceSpecError, Namespace, ResolvedServiceSpec};

use super::LocalMachineError;
use crate::images::DoorSpecs;

/// Admit one Service Container spec immediately before creation: at either reserved
/// Ingress door identity only a spec this daemon built itself, and elsewhere only a
/// spec in its own network.
///
/// # Errors
///
/// Returns when the spec is a reserved door this daemon did not build, or joins a
/// network from outside the reserved doors.
pub(crate) fn admit_reserved_door(
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
) -> Result<(), LocalMachineError> {
    let identity = ployz_core::QualifiedService::new(namespace.clone(), spec.name.clone());
    if identity.is_ingress_door() {
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
        assert!(admit_reserved_door(&Namespace::system(), &specs.holder).is_ok());
        assert!(admit_reserved_door(&Namespace::system(), &specs.proxy).is_ok());
    }

    #[test]
    fn a_door_the_daemon_did_not_build_is_refused() {
        let legacy = crate::images::legacy_caddy_for_tests("caddy:2")
            .to_resolved(
                ServiceId::parse("c".repeat(32)).unwrap(),
                Default::default(),
            )
            .unwrap();
        assert!(admit_reserved_door(&Namespace::system(), &legacy).is_err());
        let mut other = specs().proxy;
        other.container.image = "caddy:2".into();
        assert!(admit_reserved_door(&Namespace::system(), &other).is_err());
    }

    #[test]
    fn an_app_spec_may_not_join_the_holders_network() {
        let mut app = specs().proxy;
        app.name = "web".parse().unwrap();
        assert!(admit_reserved_door(&Namespace::parse("app").unwrap(), &app).is_err());
        app.container.network = None;
        assert!(admit_reserved_door(&Namespace::parse("app").unwrap(), &app).is_ok());
        app.container.network = Some(ServiceNetwork::Unrecognized);
        assert!(admit_reserved_door(&Namespace::parse("app").unwrap(), &app).is_err());
    }
}
