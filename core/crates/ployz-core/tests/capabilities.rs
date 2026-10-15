use ployz_core::{
    CERTIFICATE_POLICY_CAPABILITY, CapabilityAdvertisement, CapabilityName,
    DESCRIBE_CONTRACT_CAPABILITY, ENROLL_MACHINE_CAPABILITY, ENSURE_CONTAINER_CAPABILITY,
    ENSURE_IMAGE_INGEST_CAPABILITY, EXEC_CONTAINER_CAPABILITY, FORGET_MACHINE_CAPABILITY,
    INSPECT_BUILD_GRANT_CAPABILITY, INSPECT_CONTAINER_CAPABILITY, INSPECT_MACHINE_CAPABILITY,
    INSPECT_STORAGE_CAPABILITY, INSPECT_VOLUME_COPY_CAPABILITY, LEAVE_MACHINE_CAPABILITY,
    LIST_CONTAINERS_CAPABILITY, LIST_IMAGES_CAPABILITY, LIST_MACHINES_CAPABILITY,
    LIST_VOLUMES_CAPABILITY, LOG_HISTORY_CAPABILITY, MACHINE_STORAGE_OBSERVATION_CAPABILITY,
    METRIC_HISTORY_CAPABILITY, MINT_BUILD_GRANT_CAPABILITY, PREPARE_VOLUMES_CAPABILITY,
    PRUNE_IMAGES_CAPABILITY, PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY,
    PUBLISH_SERVING_POINTER_CAPABILITY, PULL_IMAGE_CAPABILITY, PULL_IMAGE_FROM_MACHINE_CAPABILITY,
    READ_SERVING_POINTER_CAPABILITY, REGISTER_MACHINE_CAPABILITY, REMOVE_VOLUME_CAPABILITY,
    REQUEST_MACHINE_UPGRADE_CAPABILITY, RUNTIME_WATCH_CAPABILITY, Rpc, SEED_VOLUME_CAPABILITY,
    SET_MANAGEMENT_CLIENT_CAPABILITY, SET_PUSH_TARGET_CAPABILITY, SET_VOLUME_COPY_CAPABILITY,
    TAIL_LOGS_CAPABILITY, UPDATE_MACHINE_CAPABILITY, VOLUME_FLOOR_CAPABILITY, op,
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
        (ENROLL_MACHINE_CAPABILITY, "ployz.machine.enroll.v1"),
        (REGISTER_MACHINE_CAPABILITY, "ployz.machine.register.v1"),
        (
            SET_MANAGEMENT_CLIENT_CAPABILITY,
            "ployz.machine.set-management-client.v1",
        ),
        (
            SET_PUSH_TARGET_CAPABILITY,
            "ployz.machine.set-push-target.v1",
        ),
        (LIST_MACHINES_CAPABILITY, "ployz.machine.list.v1"),
        (
            REQUEST_MACHINE_UPGRADE_CAPABILITY,
            "ployz.machine.upgrade.request.v1",
        ),
        (
            MACHINE_STORAGE_OBSERVATION_CAPABILITY,
            "ployz.machine.storage-observation.v1",
        ),
        (LIST_CONTAINERS_CAPABILITY, "ployz.container.list.v1"),
        (INSPECT_CONTAINER_CAPABILITY, "ployz.container.inspect.v1"),
        (ENSURE_CONTAINER_CAPABILITY, "ployz.container.ensure.v1"),
        (INSPECT_STORAGE_CAPABILITY, "ployz.storage.inspect.v1"),
        (PREPARE_VOLUMES_CAPABILITY, "ployz.storage.prepare.v1"),
        (LIST_VOLUMES_CAPABILITY, "ployz.volume.list.v1"),
        (REMOVE_VOLUME_CAPABILITY, "ployz.volume.remove.v1"),
        (
            INSPECT_VOLUME_COPY_CAPABILITY,
            "ployz.volume.copy.inspect.v1",
        ),
        (SEED_VOLUME_CAPABILITY, "ployz.volume.seed.v1"),
        (SET_VOLUME_COPY_CAPABILITY, "ployz.volume.copy.set.v1"),
        (LIST_IMAGES_CAPABILITY, "ployz.image.list.v1"),
        (PRUNE_IMAGES_CAPABILITY, "ployz.image.prune.v1"),
        (
            ENSURE_IMAGE_INGEST_CAPABILITY,
            "ployz.image.ingest.ensure.v1",
        ),
        (PULL_IMAGE_CAPABILITY, "ployz.image.pull.v1"),
        (
            PULL_IMAGE_FROM_MACHINE_CAPABILITY,
            "ployz.image.pull-from-machine.v1",
        ),
        (MINT_BUILD_GRANT_CAPABILITY, "ployz.build.grant.mint.v1"),
        (
            INSPECT_BUILD_GRANT_CAPABILITY,
            "ployz.build.grant.inspect.v1",
        ),
        (
            PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY,
            "ployz.certificates.publish.v1",
        ),
        (
            PUBLISH_SERVING_POINTER_CAPABILITY,
            "ployz.serving.publish.v1",
        ),
        (READ_SERVING_POINTER_CAPABILITY, "ployz.serving.read.v1"),
        (UPDATE_MACHINE_CAPABILITY, "ployz.machine.update.v1"),
        (LEAVE_MACHINE_CAPABILITY, "ployz.machine.leave.v1"),
        (FORGET_MACHINE_CAPABILITY, "ployz.machine.forget.v1"),
        (
            CERTIFICATE_POLICY_CAPABILITY,
            "ployz.certificates.policy.v1",
        ),
        (LOG_HISTORY_CAPABILITY, "ployz.logs.history.v2"),
        (METRIC_HISTORY_CAPABILITY, "ployz.metrics.history.v1"),
        (TAIL_LOGS_CAPABILITY, "ployz.logs.tail.v1"),
        (RUNTIME_WATCH_CAPABILITY, "ployz.runtime.watch.v1"),
        (EXEC_CONTAINER_CAPABILITY, "ployz.container.exec.v1"),
        (VOLUME_FLOOR_CAPABILITY, "ployz.volume.copy.floor.v1"),
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
            "ployz.machine.enroll.v1",
            "ployz.machine.register.v1",
            "ployz.machine.set-management-client.v1",
            "ployz.machine.set-push-target.v1",
            "ployz.machine.list.v1",
            "ployz.machine.update.v1",
            "ployz.machine.upgrade.request.v1",
            "ployz.machine.leave.v1",
            "ployz.machine.forget.v1",
            "ployz.certificates.policy.v1",
            "ployz.machine.storage-observation.v1",
        ]
    );
    assert_eq!(
        names(CapabilityAdvertisement::Container),
        [
            "ployz.container.list.v1",
            "ployz.container.inspect.v1",
            "ployz.container.ensure.v1",
            "ployz.storage.inspect.v1",
            "ployz.storage.prepare.v1",
            "ployz.volume.list.v1",
            "ployz.volume.remove.v1",
            "ployz.volume.copy.inspect.v1",
            "ployz.volume.seed.v1",
            "ployz.volume.copy.set.v1",
            "ployz.image.list.v1",
            "ployz.image.prune.v1",
            "ployz.image.ingest.ensure.v1",
            "ployz.image.pull.v1",
            "ployz.image.pull-from-machine.v1",
            "ployz.build.grant.mint.v1",
            "ployz.build.grant.inspect.v1",
            "ployz.metrics.history.v1",
            "ployz.logs.history.v2",
            "ployz.logs.tail.v1",
            "ployz.container.exec.v1",
            "ployz.build.v1",
            "ployz.volume.copy.floor.v1",
        ]
    );
    assert_eq!(
        names(CapabilityAdvertisement::Cluster),
        [
            "ployz.certificates.publish.v1",
            "ployz.serving.publish.v1",
            "ployz.serving.read.v1",
            "ployz.runtime.watch.v1",
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
    assert_eq!(op::Leave::PATH, "/ployz.rpc.v1.MachineRpc/Leave");
    assert_eq!(
        op::EnsureImageIngest::PATH,
        "/ployz.rpc.v1.MachineRpc/EnsureImageIngest"
    );
    assert_eq!(op::PullImage::PATH, "/ployz.rpc.v1.MachineRpc/PullImage");
    assert_eq!(
        op::PullImageFromMachine::PATH,
        "/ployz.rpc.v1.MachineRpc/PullImageFromMachine"
    );
    assert_eq!(
        op::PublishCertificateMaterial::PATH,
        "/ployz.rpc.v1.MachineRpc/PublishCertificateMaterial"
    );
    assert_eq!(
        op::SetManagementClient::PATH,
        "/ployz.rpc.v1.MachineRpc/SetManagementClient"
    );
    assert_eq!(
        op::SetPushTarget::PATH,
        "/ployz.rpc.v1.MachineRpc/SetPushTarget"
    );
    assert_eq!(
        op::RequestMachineUpgrade::PATH,
        "/ployz.rpc.v1.MachineRpc/RequestMachineUpgrade"
    );
}
