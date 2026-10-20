//! Ingress repair fixtures across several ingress Servers.

use std::collections::BTreeSet;

use ployz_core::{
    ContainerKind, ContainerObservation, ContainerObservationParts, ContainerRuntimeObservation,
    DeployIntent, HealthObservation, MachineObservation, MembershipObservation, Namespace,
    PlanOptions, ResolvedUpdateConfig, caddy_service_spec,
};

use super::DeploySnapshot;
use crate::global_fixtures::{container_id, service_id};

const IMAGE: &str = "caddy:2";

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
        resolved_spec: caddy_service_spec(IMAGE.into(), BTreeSet::new())
            .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
            .expect("Caddy mounts are scoped"),
        address: None,
        labels: Default::default(),
        rotation: None,
        r#gen: None,
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

pub(in crate::deploy) fn ingress_intent() -> DeployIntent {
    let spec = caddy_service_spec(IMAGE.into(), BTreeSet::new());
    DeployIntent::apply_all(Namespace::system(), [&spec], PlanOptions::default())
}
