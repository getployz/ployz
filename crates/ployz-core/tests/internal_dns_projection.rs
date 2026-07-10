use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};

use ployz_core::ids::{MachineId, NamespaceId, ServiceId};
use ployz_core::internal_dns::{InternalServiceName, internal_dns_records};
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, MachineFactsSnapshot,
    ManagedContainerKind,
};
use ployz_test_support::containers;

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
        internal_dns_records(&[facts]),
        BTreeMap::from([(
            name,
            vec![Ipv4Addr::new(10, 198, 1, 9), Ipv4Addr::new(10, 198, 2, 8)]
        )])
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
