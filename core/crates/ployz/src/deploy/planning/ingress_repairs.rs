//! Ingress repair planning across several ingress Servers.

use std::collections::BTreeSet;

use ployz_core::{
    ContainerKind, ContainerObservation, ContainerObservationParts, ContainerRuntimeObservation,
    DeployIntent, HealthObservation, MachineObservation, MembershipObservation, Namespace,
    PlanOptions, QualifiedService, ResolvedUpdateConfig, caddy_service_spec,
};

use super::{DeploySnapshot, SlotAction, plan_ingress_repairs};
use crate::global_fixtures::{container_id, machine, service_id};

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

/// A repair that creates the Ingress Proxy on one empty Server plans from that Server
/// alone, so its Service group must still keep every other Server's running Ingress
/// Proxy: the publish after it rotates whatever it does not keep into retirement.
#[test]
fn a_create_on_one_empty_server_keeps_the_other_servers_running_ingress() {
    let empty = machine('1', "edge-1");
    let second = machine('2', "edge-2");
    let third = machine('3', "edge-3");
    let snapshot = servers(
        &[empty.clone(), second.clone(), third.clone()],
        vec![ingress_on('b', &second), ingress_on('c', &third)],
    );
    let intent = ingress_intent();

    let repairs = plan_ingress_repairs(&intent, &snapshot);

    let [repair] = repairs.as_slice() else {
        panic!("one empty Server to repair, got {repairs:?}");
    };
    assert_eq!(repair.machine_id, empty.id);
    let SlotAction::Create(Ok(plan)) = &repair.action else {
        panic!("a create on the empty Server, got {:?}", repair.action);
    };
    let ingress = QualifiedService::system_ingress().name;
    let group = plan
        .groups()
        .iter()
        .find(|group| group.service.as_ref() == Some(&ingress))
        .expect("the create has an Ingress Proxy group");
    let mut kept = group.kept.clone();
    kept.sort();
    assert_eq!(
        kept,
        vec![container_id('b'), container_id('c')],
        "the other Servers' running Ingress Proxies would be retired: {plan:?}"
    );
}
