use super::{ServiceLogLine, bounded_ops_list, missing_machine_ids, service_log_lines};
use crate::control::projection::runtime_state::{
    derive_instances, derive_releases, derive_revisions, missing_links, service_snapshot,
};
use ployz_core::intent::RouteBindingState;
use ployz_core::machine::runtime::{
    MachineContainerObservationSnapshot, MachineFactsSnapshot, ManagedContainerKind,
};
use ployz_core::operation::{
    EventSequence, OperationStatus, RouteHostname, RoutePort, RouteTarget,
};
use ployz_sdk_types::{
    ServiceContainerMembership, ServiceMachineTestimony, ServiceSnapshot, ServiceTestimony,
};
use ployz_test_support::containers;
use ployz_test_support::fixtures::serving_target_entry_in;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, service_id,
};
use std::collections::BTreeMap;

#[test]
fn operation_list_returns_newest_hundred_with_more_evidence() {
    let statuses = (0..101)
        .rev()
        .map(|index| namespace_remove_status(index, false))
        .collect::<Vec<_>>();

    let result = bounded_ops_list(statuses, false);

    assert!(result.has_more);
    let [first, middle @ .., last] = result.operations.as_slice() else {
        panic!("expected a full operation page");
    };
    assert_eq!(middle.len(), 98);
    assert_eq!(first.status.id().as_str(), "op_100");
    assert_eq!(last.status.id().as_str(), "op_001");
}

#[test]
fn operation_list_applies_active_filter_before_cap() {
    let mut statuses = (0..101)
        .map(|index| namespace_remove_status(index, false))
        .collect::<Vec<_>>();
    statuses.extend((101..151).map(|index| namespace_remove_status(index, true)));
    statuses.reverse();

    let result = bounded_ops_list(statuses, true);

    assert!(result.has_more);
    let [first, middle @ .., last] = result.operations.as_slice() else {
        panic!("expected a full operation page");
    };
    assert_eq!(middle.len(), 98);
    assert_eq!(first.status.id().as_str(), "op_100");
    assert_eq!(last.status.id().as_str(), "op_001");
}

fn namespace_remove_status(index: usize, terminal: bool) -> OperationStatus {
    let mut status = OperationStatus::namespace_remove_accepted(
        ployz_test_support::ids::operation_id(&format!("op_{index:03}")),
        namespace_id("team-a"),
        EventSequence::first(),
    );
    if terminal {
        let OperationStatus::NamespaceRemove { state, .. } = &mut status else {
            unreachable!("constructor returns namespace removal status");
        };
        *state = ployz_core::operation::NamespaceRemoveOperationState::Completed;
    }
    status
}

#[test]
fn service_snapshot_keeps_silent_machines_as_no_answer() {
    let active = serving_target_entry_in("team-a", "web", "entry_a");
    let observed = containers::observation("machine_a", "ctr_web")
        .with(
            containers::identity("web")
                .namespace("team-a")
                .entry("entry_a"),
        )
        .running_unroutable()
        .build();
    let machine_ids = vec![
        ployz_test_support::ids::machine_id("machine_a"),
        ployz_test_support::ids::machine_id("machine_b"),
    ];
    let machine_a_facts = machine_facts("machine_a", [observed]);
    let facts = BTreeMap::from([(machine_a_facts.machine_id().clone(), machine_a_facts)]);

    let snapshot = service_snapshot(active, &[], &machine_ids, &facts);

    assert_eq!(snapshot.testimony.ready_container_count, 1);
    let [answered, no_answer] = snapshot.testimony.machines.as_slice() else {
        panic!("one row per intent machine");
    };
    match answered {
        ServiceMachineTestimony::Answered {
            machine_id,
            containers,
        } => {
            assert_eq!(machine_id.as_str(), "machine_a");
            assert_eq!(containers.len(), 1);
        }
        ServiceMachineTestimony::NoAnswer { .. } => panic!("machine_a answered"),
    }
    match no_answer {
        ServiceMachineTestimony::NoAnswer { machine_id } => {
            assert_eq!(machine_id.as_str(), "machine_b");
        }
        ServiceMachineTestimony::Answered { .. } => panic!("machine_b did not answer"),
    }
}

#[test]
fn service_snapshot_keeps_retained_service_containers_from_inactive_entries() {
    let active = serving_target_entry_in("team-a", "web", "entry_active");
    let active_container = containers::observation("machine_a", "ctr_active")
        .with(
            containers::identity("web")
                .namespace("team-a")
                .entry("entry_active"),
        )
        .running_unroutable()
        .build();
    let retained_container = containers::observation("machine_a", "ctr_retained")
        .with(
            containers::identity("web")
                .namespace("team-a")
                .entry("entry_failed"),
        )
        .exited()
        .build();
    let machine_ids = vec![ployz_test_support::ids::machine_id("machine_a")];
    let machine_a_facts = machine_facts("machine_a", [active_container, retained_container]);
    let facts = BTreeMap::from([(machine_a_facts.machine_id().clone(), machine_a_facts)]);

    let snapshot = service_snapshot(active, &[], &machine_ids, &facts);

    assert_eq!(snapshot.testimony.ready_container_count, 1);
    assert_eq!(snapshot.testimony.observed_container_count, 2);
    let [ServiceMachineTestimony::Answered { containers, .. }] =
        snapshot.testimony.machines.as_slice()
    else {
        panic!("machine_a answered");
    };
    assert_eq!(
        containers
            .iter()
            .map(|container| (
                container.observation.container_id.as_str(),
                container.membership,
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                "ctr_active",
                ServiceContainerMembership::ServingTargetMember
            ),
            ("ctr_retained", ServiceContainerMembership::RetainedEvidence),
        ],
    );
}

fn machine_facts(
    machine_id_value: &str,
    containers: impl IntoIterator<Item = ployz_core::machine::runtime::ManagedContainerObservation>,
) -> MachineFactsSnapshot {
    let machine_id = ployz_test_support::ids::machine_id(machine_id_value);
    MachineFactsSnapshot::try_new(
        machine_id.clone(),
        MachineContainerObservationSnapshot::try_new(machine_id, containers)
            .expect("valid container snapshot"),
        None,
        ployz_test_support::fixtures::test_disk_space(),
        ployz_core::image::OciPlatform::current(),
        1,
    )
    .expect("valid machine facts")
}

/// Containers whose service has no serving target entry still surface
/// as instances and revisions under their own identity namespace:
/// Docker is execution reality, so orphaned containers are evidence,
/// not noise (missing_link_count separately reports the mismatch).
#[test]
fn orphaned_containers_surface_as_instances_and_revisions() {
    let orphan = containers::observation("machine_a", "ctr_orphan")
        .with(containers::identity("svc_orphan").namespace("team-a"))
        .running_unroutable()
        .build();

    let instances = derive_instances(std::slice::from_ref(&orphan));
    let [instance] = instances.as_slice() else {
        panic!("orphaned container is projected as an instance");
    };
    assert_eq!(instance.namespace_id, namespace_id("team-a"));
    assert_eq!(instance.service_id, service_id("svc_orphan"));

    let revisions = derive_revisions(&[], &[orphan]);
    let [revision] = revisions.as_slice() else {
        panic!("orphaned container is projected as a revision");
    };
    assert_eq!(revision.namespace_id, namespace_id("team-a"));
}

/// Hook containers are operation evidence, not service instances:
/// they never fabricate a service revision.
#[test]
fn hook_containers_do_not_evidence_revisions() {
    let job = containers::observation("machine_a", "ctr_job")
        .with(containers::identity("svc_api").kind(ManagedContainerKind::Job))
        .running_unroutable()
        .build();

    assert!(derive_revisions(&[], &[job]).is_empty());
}

/// A route for a namespace with no serving entry is a missing link
/// even when another namespace serves the same service name.
#[test]
fn missing_links_are_namespace_scoped() {
    let serving = ServiceSnapshot {
        active: serving_target_entry_in("team-a", "web", "entry_a"),
        route_bindings: Vec::new(),
        testimony: ServiceTestimony {
            ready_container_count: 0,
            observed_container_count: 0,
            machines: Vec::new(),
        },
    };
    let dangling_route = RouteBindingState {
        id: ployz_core::ids::RouteBindingId::try_new("route_team_b")
            .expect("valid route binding id"),
        namespace_id: namespace_id("team-b"),
        target: RouteTarget::new(
            RouteHostname::try_new("b.example.com").expect("valid route hostname"),
        ),
        endpoint_port: RoutePort::try_new(8080).expect("valid route port"),
        service_id: service_id("web"),
        origin: ployz_core::ingress::RouteBindingOrigin::Declared,
    };

    assert_eq!(missing_links(&[serving], &[dangling_route], &[]), 1);
}

/// Two namespaces sharing a service name must not cross-attribute
/// route releases: the lookup is namespace-scoped, so team-b's route
/// resolves team-b's entry even when team-a inserted last.
#[test]
fn releases_resolve_entries_per_namespace() {
    let serving = |namespace: &str, entry: &str| ServiceSnapshot {
        active: serving_target_entry_in(namespace, "web", entry),
        route_bindings: Vec::new(),
        testimony: ServiceTestimony {
            ready_container_count: 0,
            observed_container_count: 0,
            machines: Vec::new(),
        },
    };
    let route = RouteBindingState {
        id: ployz_core::ids::RouteBindingId::try_new("route_team_b")
            .expect("valid route binding id"),
        namespace_id: namespace_id("team-b"),
        target: RouteTarget::new(
            RouteHostname::try_new("b.example.com").expect("valid route hostname"),
        ),
        endpoint_port: RoutePort::try_new(8080).expect("valid route port"),
        service_id: service_id("web"),
        origin: ployz_core::ingress::RouteBindingOrigin::Declared,
    };

    let releases = derive_releases(
        &[serving("team-b", "entry_b"), serving("team-a", "entry_a")],
        &[route],
    );

    let release = releases
        .iter()
        .find(|release| !release.routes.is_empty())
        .expect("team-b's route lands on one release");
    assert_eq!(release.namespace_id, namespace_id("team-b"));
    assert_eq!(
        release.namespace_revision_entry_id,
        namespace_revision_entry_id("entry_b")
    );
}

#[test]
fn service_log_lines_sort_by_docker_timestamps_and_report_missing_machines() {
    let mut lines = Vec::new();
    lines.extend(service_log_lines(
        &machine_id("machine_b"),
        &container_id("ctr_b"),
        "2026-07-09T02:00:02Z second\n",
    ));
    lines.extend(service_log_lines(
        &machine_id("machine_a"),
        &container_id("ctr_a"),
        "2026-07-09T02:00:01Z first\n",
    ));
    for machine_id in missing_machine_ids(
        &[machine_id("machine_a"), machine_id("machine_c")],
        &BTreeMap::from([(
            machine_id("machine_a"),
            containers::snapshot("machine_a", []),
        )]),
    ) {
        lines.push(ServiceLogLine {
            sort_key: None,
            text: format!("machine {}: no answer\n", machine_id.as_str()),
        });
    }

    lines.sort_by(|left, right| {
        left.sort_key
            .cmp(&right.sort_key)
            .then(left.text.cmp(&right.text))
    });

    let rendered = lines.into_iter().map(|line| line.text).collect::<String>();
    assert_eq!(
        rendered,
        "machine machine_c: no answer\n\
machine_a ctr_a | first\n\
machine_b ctr_b | second\n"
    );
}
