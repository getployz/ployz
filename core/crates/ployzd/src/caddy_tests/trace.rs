//! Trace page facts in the projection and their rendering.

use super::{ingress, observation, projection};
use crate::docker::LABEL_DEPLOYMENT;
use crate::ingress::{IngressProjection, PublicationTrace};
use ployz_core::{
    ContainerKind, ContainerObservation, ContainerRuntimeObservation, DeploymentLogId,
    HealthObservation, HttpProtocol, MachineId, Namespace, service_containers,
};
use std::collections::BTreeMap;
use std::num::NonZeroUsize;

fn machine(digit: char) -> MachineId {
    MachineId::parse(digit.to_string().repeat(32)).unwrap()
}

fn api(suffix: u8, machine_id: &MachineId, created_at: i64) -> ContainerObservation {
    let mut container = observation(
        suffix,
        machine_id,
        "api",
        Some([10, 210, 1, suffix]),
        vec![ingress("api.example", 80, HttpProtocol::Http)],
    );
    container
        .try_update(|parts| parts.created_at_unix_nanos = created_at)
        .unwrap();
    container
}

fn with(
    mut container: ContainerObservation,
    update: impl FnOnce(&mut ployz_core::ContainerObservationParts),
) -> ContainerObservation {
    container.try_update(update).unwrap();
    container
}

fn labeled(container: ContainerObservation, deploy: &str) -> ContainerObservation {
    with(container, |parts| {
        parts
            .labels
            .insert(LABEL_DEPLOYMENT.to_owned(), deploy.to_owned());
    })
}

fn derive(containers: Vec<ContainerObservation>) -> IngressProjection {
    projection(
        &machine('a'),
        "node-a",
        &service_containers(containers),
        &BTreeMap::new(),
    )
}

fn trace(projection: &IngressProjection) -> &PublicationTrace {
    &projection
        .sites
        .iter()
        .find(|site| site.hostname.as_str() == "api.example")
        .and_then(|site| site.publication.as_ref())
        .expect("api.example is published")
        .trace
}

fn placement(entries: &[(char, usize)]) -> BTreeMap<MachineId, NonZeroUsize> {
    entries
        .iter()
        .map(|(digit, count)| (machine(*digit), NonZeroUsize::new(*count).unwrap()))
        .collect()
}

fn running(health: HealthObservation) -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running { health }
}

#[test]
fn placement_counts_every_owner_container_in_any_state_and_excludes_hooks() {
    let projection = derive(vec![
        api(1, &machine('a'), 1),
        with(api(2, &machine('a'), 2), |parts| {
            parts.runtime = ContainerRuntimeObservation::Exited { code: 137 };
        }),
        with(api(3, &machine('b'), 3), |parts| {
            parts.runtime = running(HealthObservation::Unhealthy);
        }),
        with(api(4, &machine('c'), 4), |parts| {
            parts.runtime = running(HealthObservation::Starting);
        }),
        with(api(5, &machine('a'), 5), |parts| {
            parts.kind = ContainerKind::PreDeployHook;
        }),
    ]);

    assert_eq!(
        trace(&projection).placement,
        placement(&[('a', 2), ('b', 1), ('c', 1)])
    );
}

#[test]
fn placement_counts_rollout_overlap_and_stopped_leftovers() {
    let projection = derive(vec![
        labeled(api(1, &machine('a'), 1), "old"),
        labeled(api(2, &machine('a'), 2), "new"),
        with(labeled(api(3, &machine('b'), 0), "older"), |parts| {
            parts.runtime = ContainerRuntimeObservation::Exited { code: 0 };
        }),
    ]);

    let trace = trace(&projection);
    assert_eq!(trace.placement, placement(&[('a', 2), ('b', 1)]));
    assert_eq!(trace.deploy, Some(DeploymentLogId::parse("new").unwrap()));
}

#[test]
fn placement_counts_only_the_hostname_owner() {
    // The oldest publisher owns a contested hostname.
    let blog = with(api(9, &machine('b'), 9), |parts| {
        parts.namespace = Namespace::parse("blog").unwrap();
    });
    let projection = derive(vec![api(1, &machine('a'), 1), blog]);

    assert_eq!(trace(&projection).placement, placement(&[('a', 1)]));
}

#[test]
fn deploy_names_the_newest_container_even_when_it_is_stopped() {
    let projection = derive(vec![
        labeled(api(1, &machine('a'), 1), "running"),
        with(labeled(api(2, &machine('b'), 2), "crashed"), |parts| {
            parts.runtime = ContainerRuntimeObservation::Exited { code: 1 };
        }),
    ]);

    assert_eq!(
        trace(&projection).deploy,
        Some(DeploymentLogId::parse("crashed").unwrap())
    );
}

#[test]
fn deploy_never_borrows_an_older_sibling_s_label() {
    let unlabeled = derive(vec![
        labeled(api(1, &machine('a'), 1), "old"),
        api(2, &machine('a'), 2),
    ]);
    let malformed = derive(vec![
        labeled(api(1, &machine('a'), 1), "old"),
        labeled(api(2, &machine('a'), 2), "not a deploy id"),
    ]);

    assert_eq!(trace(&unlabeled).deploy, None);
    assert_eq!(trace(&malformed).deploy, None);
}

#[test]
fn deploy_breaks_creation_ties_by_container_id() {
    let containers = vec![
        labeled(api(2, &machine('a'), 7), "second"),
        labeled(api(1, &machine('a'), 7), "first"),
    ];
    let mut reversed = containers.clone();
    reversed.reverse();

    assert_eq!(
        trace(&derive(containers)).deploy,
        Some(DeploymentLogId::parse("second").unwrap())
    );
    assert_eq!(
        trace(&derive(reversed)).deploy,
        Some(DeploymentLogId::parse("second").unwrap())
    );
}

#[test]
fn health_changes_that_keep_endpoints_leave_the_projection_unchanged() {
    let before = derive(vec![
        api(1, &machine('a'), 1),
        with(api(2, &machine('b'), 2), |parts| {
            parts.runtime = running(HealthObservation::Starting);
        }),
        with(api(3, &machine('b'), 3), |parts| {
            parts.runtime = ContainerRuntimeObservation::Exited { code: 137 };
        }),
    ]);
    let after = derive(vec![
        with(api(1, &machine('a'), 1), |parts| {
            parts.runtime = running(HealthObservation::Failing);
        }),
        with(api(2, &machine('b'), 2), |parts| {
            parts.runtime = running(HealthObservation::Unhealthy);
        }),
        with(api(3, &machine('b'), 3), |parts| {
            parts.runtime = ContainerRuntimeObservation::Exited { code: 1 };
        }),
    ]);

    assert_eq!(before, after);
}

#[test]
fn count_and_label_changes_change_the_projection() {
    let base = || vec![labeled(api(1, &machine('a'), 1), "one")];
    let mut leftover = base();
    leftover.push(with(api(2, &machine('b'), 0), |parts| {
        parts.runtime = ContainerRuntimeObservation::Exited { code: 0 };
    }));
    let relabeled = vec![labeled(api(1, &machine('a'), 1), "two")];

    assert_ne!(derive(base()), derive(leftover));
    assert_ne!(derive(base()), derive(relabeled));
}
