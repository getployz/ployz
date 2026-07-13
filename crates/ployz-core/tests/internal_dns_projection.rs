use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};

use ployz_core::dataplane::MachineEndpointSubnet;
use ployz_core::ids::{MachineId, NamespaceId, ServiceId};
use ployz_core::internal_dns::{InternalServiceName, internal_dns_records};
use ployz_core::machine::MachineName;
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, MachineFactsSnapshot,
    ManagedContainerKind,
};
use ployz_core::roles::InstallRolePolicy;
use ployz_core::state::{
    ActiveMachineState, ControlPlaneEpoch, IntentSnapshot, MachineLifecycle, ManagedLeaseProjection,
};
use ployz_test_support::fixtures::serving_target_entry;
use ployz_test_support::ids::{machine_id, operation_id};
use ployz_test_support::{containers, fixtures};

#[test]
fn internal_dns_projection_returns_sorted_unique_running_service_ipv4_addresses() {
    let observations = [
        containers::observation("machine_a", "ctr_1")
            .with(containers::identity("db").namespace("default"))
            .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 2, 8)))
            .build(),
        containers::observation("machine_a", "ctr_2")
            .with(containers::identity("db").namespace("default"))
            .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 1, 9)))
            .build(),
        containers::observation("machine_a", "ctr_3")
            .with(containers::identity("db").namespace("default"))
            .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 2, 8)))
            .build(),
        containers::observation("machine_a", "ctr_job")
            .with(
                containers::identity("db")
                    .namespace("default")
                    .kind(ManagedContainerKind::Job),
            )
            .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 3, 10)))
            .build(),
        ployz_core::machine_runtime::ManagedContainerObservation {
            state: ContainerRuntimeState::Exited,
            ..containers::observation("machine_a", "ctr_exited")
                .with(containers::identity("db").namespace("default"))
                .build()
        },
    ];
    let machine_id = MachineId::try_new("machine_a").expect("machine id");
    let facts = MachineFactsSnapshot::try_new(
        machine_id.clone(),
        MachineContainerObservationSnapshot::try_new(machine_id, observations)
            .expect("container facts"),
        None,
        ployz_test_support::fixtures::test_disk_space(),
        ployz_core::image::OciPlatform::current(),
        1,
    )
    .expect("machine facts");
    let name = InternalServiceName::try_from_ids(
        &ServiceId::try_new("db").expect("service id"),
        &NamespaceId::try_new("default").expect("namespace id"),
    )
    .expect("internal service name");

    assert_eq!(
        internal_dns_records(&intent(["machine_a"], "entry_test"), &[facts]),
        BTreeMap::from([(
            name,
            vec![Ipv4Addr::new(10, 198, 1, 9), Ipv4Addr::new(10, 198, 2, 8)]
        )])
    );
}

#[test]
fn internal_dns_projection_excludes_facts_from_removed_machines() {
    let facts = machine_facts(
        "machine_removed",
        [containers::observation("machine_removed", "ctr_1")
            .with(containers::identity("db"))
            .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 2, 8)))],
    );

    assert_eq!(
        internal_dns_records(&intent(["machine_a"], "entry_test"), &[facts]),
        BTreeMap::new()
    );
}

#[test]
fn internal_dns_projection_excludes_retained_failed_and_old_revision_containers() {
    let facts = machine_facts(
        "machine_a",
        [
            containers::observation("machine_a", "ctr_failed")
                .with(containers::identity("db").entry("entry_failed"))
                .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 2, 7))),
            containers::observation("machine_a", "ctr_old")
                .with(containers::identity("db").entry("entry_old"))
                .running_at(IpAddr::V4(Ipv4Addr::new(10, 198, 2, 8))),
        ],
    );

    assert_eq!(
        internal_dns_records(&intent(["machine_a"], "entry_current"), &[facts]),
        BTreeMap::new()
    );
}

#[test]
fn internal_service_name_deserialization_rejects_invalid_names() {
    let error = serde_json::from_str::<InternalServiceName>("\"db.internal\"")
        .expect_err("missing namespace is invalid");

    assert!(error.to_string().contains("internal service name"));
}

#[test]
fn internal_service_name_from_ids_rejects_dns_labels_over_63_bytes() {
    let service_id = ServiceId::try_new("s".repeat(64)).expect("service id");
    let namespace_id = NamespaceId::try_new("default").expect("namespace id");

    assert!(InternalServiceName::try_from_ids(&service_id, &namespace_id).is_err());
}

#[test]
fn internal_service_name_from_ids_rejects_noncanonical_service_id_case() {
    let service_id = ServiceId::try_new("Database").expect("service id");
    let namespace_id = NamespaceId::try_new("default").expect("namespace id");

    assert!(InternalServiceName::try_from_ids(&service_id, &namespace_id).is_err());
}

#[test]
fn internal_service_name_from_ids_rejects_noncanonical_namespace_id_case() {
    let service_id = ServiceId::try_new("database").expect("service id");
    let namespace_id = NamespaceId::try_new("Default").expect("namespace id");

    assert!(InternalServiceName::try_from_ids(&service_id, &namespace_id).is_err());
}

#[test]
fn internal_service_name_query_parsing_canonicalizes_ascii_case() {
    let name = InternalServiceName::try_new("Database.Default.INTERNAL")
        .expect("operator query is case-insensitive");

    assert_eq!(name.as_str(), "database.default.internal");
}

fn intent<const N: usize>(machines: [&str; N], entry: &str) -> IntentSnapshot {
    IntentSnapshot {
        epoch: ControlPlaneEpoch::initial(),
        core_machine_id: machine_id("machine_a"),
        active_machines: machines.into_iter().map(active_machine).collect(),
        dataplane_projection: ployz_core::dataplane::DataplaneProjection::try_new(Vec::new(), None)
            .expect("empty projection"),
        route_bindings: Vec::new(),
        serving_target_entries: vec![serving_target_entry("db", entry)],
        volume_pins: Vec::new(),
        nats_authorizations: Vec::new(),
        managed_lease: ManagedLeaseProjection::Unacquired,
        custom_certificates: Vec::new(),
        acme_http01_challenges: Vec::new(),
    }
}

fn active_machine(id: &str) -> ActiveMachineState {
    ActiveMachineState {
        machine_id: machine_id(id),
        name: MachineName::try_new(id).expect("machine name"),
        activated_by: operation_id(&format!("op_{id}")),
        roles: InstallRolePolicy::install_all(),
        lifecycle: MachineLifecycle::Active,
        control_endpoints: Vec::new(),
        mesh_endpoints: Vec::new(),
        endpoint_subnet: MachineEndpointSubnet::try_new("10.198.0.0/24")
            .expect("valid endpoint subnet"),
        wireguard_public_key: ployz_core::dataplane::WireGuardPublicKey::try_new(format!(
            "public-{id}"
        ))
        .expect("public key"),
    }
}

fn machine_facts(
    machine: &str,
    observations: impl IntoIterator<Item = containers::ManagedContainerObservationBuilder>,
) -> MachineFactsSnapshot {
    MachineFactsSnapshot::try_new(
        machine_id(machine),
        containers::snapshot(machine, observations),
        None,
        fixtures::test_disk_space(),
        ployz_core::image::OciPlatform::current(),
        1,
    )
    .expect("machine facts")
}
