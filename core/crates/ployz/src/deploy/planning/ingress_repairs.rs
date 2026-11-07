//! Ingress repair fixtures across several ingress Servers.

use ployz_core::{
    ContainerKind, ContainerObservation, ContainerObservationParts, ContainerRuntimeObservation,
    HealthObservation, MachineObservation, MembershipObservation, Namespace, ResolvedUpdateConfig,
    caddy_service_spec,
};

use super::DeploySnapshot;
use crate::global_fixtures::{container_id, service_id};

pub(in crate::deploy) fn ingress_on(
    hex: char,
    machine: &ployz_core::Machine,
) -> ContainerObservation {
    ContainerObservation::try_from(ContainerObservationParts {
        container_id: container_id(hex),
        display_name: format!("ingress-{hex}"),
        created_at_unix_nanos: 0,
        machine_id: machine.id,
        namespace: Namespace::system(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: caddy_service_spec("caddy:test")
            .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
            .expect("Caddy mounts are scoped"),
        address: None,
        labels: Default::default(),
        rotation: None,
        r#gen: None,
        stopped: false,
        run: None,
    })
    .unwrap()
}

/// Up ingress Servers `machines`, running `containers`.
pub(in crate::deploy) fn servers(
    machines: &[ployz_core::Machine],
    containers: Vec<ContainerObservation>,
) -> DeploySnapshot {
    DeploySnapshot {
        machines: machines
            .iter()
            .map(|machine| MachineObservation::new(machine.clone(), MembershipObservation::Up))
            .collect(),
        containers,
        ..Default::default()
    }
}
