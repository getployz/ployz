use ployz_core::{
    CERTIFICATE_POLICY_CAPABILITY, CONTAINER_LOGS_CAPABILITY, CREATE_CONTAINER_CAPABILITY,
    CREATE_DOMAIN_RECORDS_CAPABILITY, CREATE_VOLUME_CAPABILITY, CapabilityAdvertisement,
    CapabilityName, DESCRIBE_CONTRACT_CAPABILITY, ENSURE_GLOBAL_SLOT_CAPABILITY,
    ENSURE_IMAGE_INGEST_CAPABILITY, EXEC_CONTAINER_CAPABILITY, GET_CADDY_CONFIG_CAPABILITY,
    GET_DOMAIN_CAPABILITY, INITIALIZE_MACHINE_CAPABILITY, INSPECT_CONTAINER_CAPABILITY,
    INSPECT_MACHINE_CAPABILITY, INSPECT_VOLUME_CAPABILITY, INSPECT_WIREGUARD_CAPABILITY,
    JOIN_MACHINE_CAPABILITY, LIST_CONTAINERS_CAPABILITY, LIST_IMAGES_CAPABILITY,
    LIST_MACHINES_CAPABILITY, LIST_VOLUMES_CAPABILITY, MACHINE_LOGS_CAPABILITY,
    MACHINE_TOKEN_CAPABILITY, PULL_IMAGE_FROM_MACHINE_CAPABILITY, REGISTER_MACHINE_CAPABILITY,
    RELEASE_DOMAIN_CAPABILITY, REMOVE_CONTAINER_CAPABILITY, REMOVE_LOCAL_MACHINE_CAPABILITY,
    REMOVE_MACHINE_CAPABILITY, REMOVE_VOLUME_CAPABILITY, RESERVE_DOMAIN_CAPABILITY,
    RESET_MACHINE_CAPABILITY, RUNTIME_WATCH_CAPABILITY, Rpc, SET_CLOUD_PAIRING_CAPABILITY,
    START_CONTAINER_CAPABILITY, STOP_CONTAINER_CAPABILITY, UPDATE_MACHINE_CAPABILITY, op,
};

/// Capability constants are generated from the catalog, so a typo would stay
/// consistent everywhere. This restates the frozen spellings independently.
#[test]
fn catalogued_capabilities_keep_stable_spellings() {
    let frozen = [
        (
            DESCRIBE_CONTRACT_CAPABILITY,
            "ployz.rpc.describe-contract.v1",
        ),
        (INSPECT_MACHINE_CAPABILITY, "ployz.machine.inspect.v1"),
        (MACHINE_TOKEN_CAPABILITY, "ployz.machine.token.v1"),
        (INITIALIZE_MACHINE_CAPABILITY, "ployz.machine.initialize.v1"),
        (REGISTER_MACHINE_CAPABILITY, "ployz.machine.register.v1"),
        (JOIN_MACHINE_CAPABILITY, "ployz.machine.join.v1"),
        (
            SET_CLOUD_PAIRING_CAPABILITY,
            "ployz.machine.set-cloud-pairing.v1",
        ),
        (LIST_MACHINES_CAPABILITY, "ployz.machine.list.v1"),
        (LIST_CONTAINERS_CAPABILITY, "ployz.container.list.v1"),
        (INSPECT_CONTAINER_CAPABILITY, "ployz.container.inspect.v1"),
        (CREATE_CONTAINER_CAPABILITY, "ployz.container.create.v1"),
        (
            ENSURE_GLOBAL_SLOT_CAPABILITY,
            "ployz.container.ensure-global-slot.v1",
        ),
        (START_CONTAINER_CAPABILITY, "ployz.container.start.v1"),
        (STOP_CONTAINER_CAPABILITY, "ployz.container.stop.v1"),
        (REMOVE_CONTAINER_CAPABILITY, "ployz.container.remove.v1"),
        (CREATE_VOLUME_CAPABILITY, "ployz.volume.create.v1"),
        (LIST_VOLUMES_CAPABILITY, "ployz.volume.list.v1"),
        (INSPECT_VOLUME_CAPABILITY, "ployz.volume.inspect.v1"),
        (REMOVE_VOLUME_CAPABILITY, "ployz.volume.remove.v1"),
        (LIST_IMAGES_CAPABILITY, "ployz.image.list.v1"),
        (
            ENSURE_IMAGE_INGEST_CAPABILITY,
            "ployz.image.ingest.ensure.v1",
        ),
        (
            PULL_IMAGE_FROM_MACHINE_CAPABILITY,
            "ployz.image.pull-from-machine.v1",
        ),
        (GET_CADDY_CONFIG_CAPABILITY, "ployz.caddy.config.v1"),
        (RESERVE_DOMAIN_CAPABILITY, "ployz.dns.reserve.v1"),
        (GET_DOMAIN_CAPABILITY, "ployz.dns.show.v1"),
        (RELEASE_DOMAIN_CAPABILITY, "ployz.dns.release.v1"),
        (
            CREATE_DOMAIN_RECORDS_CAPABILITY,
            "ployz.dns.records.create.v1",
        ),
        (UPDATE_MACHINE_CAPABILITY, "ployz.machine.update.v1"),
        (
            REMOVE_LOCAL_MACHINE_CAPABILITY,
            "ployz.machine.remove-local.v1",
        ),
        (REMOVE_MACHINE_CAPABILITY, "ployz.machine.remove.v1"),
        (INSPECT_WIREGUARD_CAPABILITY, "ployz.wireguard.inspect.v1"),
        (RESET_MACHINE_CAPABILITY, "ployz.machine.reset.v1"),
        (
            CERTIFICATE_POLICY_CAPABILITY,
            "ployz.certificates.policy.v1",
        ),
        (CONTAINER_LOGS_CAPABILITY, "ployz.container.logs.v1"),
        (MACHINE_LOGS_CAPABILITY, "ployz.machine.logs.v1"),
        (RUNTIME_WATCH_CAPABILITY, "ployz.runtime.watch.v1"),
        (EXEC_CONTAINER_CAPABILITY, "ployz.container.exec.v1"),
    ];
    for (capability, spelling) in frozen {
        assert_eq!(capability, spelling);
        CapabilityName::parse(capability).unwrap();
    }
}

#[test]
fn advertised_capability_groups_match_the_frozen_catalog() {
    assert_eq!(
        names(CapabilityAdvertisement::Always),
        [
            "ployz.rpc.describe-contract.v1",
            "ployz.machine.inspect.v1",
            "ployz.machine.token.v1",
            "ployz.machine.initialize.v1",
            "ployz.machine.register.v1",
            "ployz.machine.join.v1",
            "ployz.machine.set-cloud-pairing.v1",
            "ployz.machine.list.v1",
            "ployz.machine.update.v1",
            "ployz.machine.remove-local.v1",
            "ployz.machine.remove.v1",
            "ployz.wireguard.inspect.v1",
            "ployz.machine.reset.v1",
            "ployz.runtime.watch.v1",
            "ployz.certificates.policy.v1",
        ]
    );
    assert_eq!(
        names(CapabilityAdvertisement::Container),
        [
            "ployz.container.list.v1",
            "ployz.container.inspect.v1",
            "ployz.container.create.v1",
            "ployz.container.ensure-global-slot.v1",
            "ployz.container.start.v1",
            "ployz.container.stop.v1",
            "ployz.container.remove.v1",
            "ployz.volume.create.v1",
            "ployz.volume.list.v1",
            "ployz.volume.inspect.v1",
            "ployz.volume.remove.v1",
            "ployz.image.list.v1",
            "ployz.image.ingest.ensure.v1",
            "ployz.image.pull-from-machine.v1",
            "ployz.container.logs.v1",
            "ployz.machine.logs.v1",
            "ployz.container.exec.v1",
        ]
    );
    assert_eq!(
        names(CapabilityAdvertisement::Caddy),
        ["ployz.caddy.config.v1"]
    );
    assert_eq!(
        names(CapabilityAdvertisement::Cluster),
        [
            "ployz.dns.reserve.v1",
            "ployz.dns.show.v1",
            "ployz.dns.release.v1",
            "ployz.dns.records.create.v1",
        ]
    );
}

fn names(class: CapabilityAdvertisement) -> Vec<String> {
    class
        .capabilities()
        .map(|name| name.as_str().to_owned())
        .collect()
}

#[test]
fn unary_grpc_paths_stay_on_the_machine_rpc_service() {
    assert_eq!(
        op::DescribeContract::PATH,
        "/ployz.rpc.v1.MachineRpc/DescribeContract"
    );
    assert_eq!(op::Reset::PATH, "/ployz.rpc.v1.MachineRpc/Reset");
    assert_eq!(
        op::GetCaddyConfig::PATH,
        "/ployz.rpc.v1.MachineRpc/GetCaddyConfig"
    );
    assert_eq!(
        op::EnsureImageIngest::PATH,
        "/ployz.rpc.v1.MachineRpc/EnsureImageIngest"
    );
    assert_eq!(
        op::PullImageFromMachine::PATH,
        "/ployz.rpc.v1.MachineRpc/PullImageFromMachine"
    );
    assert_eq!(
        op::CreateDomainRecords::PATH,
        "/ployz.rpc.v1.MachineRpc/CreateDomainRecords"
    );
    assert_eq!(
        op::SetCloudPairing::PATH,
        "/ployz.rpc.v1.MachineRpc/SetCloudPairing"
    );
    assert_eq!(
        op::EnsureGlobalSlot::PATH,
        "/ployz.rpc.v1.MachineRpc/EnsureGlobalSlot"
    );
}
