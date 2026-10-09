use ployz_core::{
    ACCEPT_HAND_OFF_CAPABILITY, ADOPT_LEASE_CAPABILITY, BEGIN_ROUND_CAPABILITY,
    CERTIFICATE_POLICY_CAPABILITY, CLEAR_FINAL_CAPABILITY, CLOSE_VOLUME_CAPABILITY,
    COMMIT_SNAPSHOTS_CAPABILITY, CONTAINER_LOGS_CAPABILITY, CREATE_CONTAINER_CAPABILITY,
    CREATE_VOLUME_CAPABILITY, CapabilityAdvertisement, CapabilityName, DECLARE_MIRROR_CAPABILITY,
    DESCRIBE_CONTRACT_CAPABILITY, DESTROY_MIRROR_CAPABILITY, END_BUILD_GRANT_CAPABILITY,
    ENSURE_IMAGE_INGEST_CAPABILITY, EXEC_CONTAINER_CAPABILITY, FORGET_LOGS_CAPABILITY,
    FORGET_SNAPSHOTS_CAPABILITY, FREEZE_VOLUME_CAPABILITY, GET_INGRESS_PROXY_CONFIG_CAPABILITY,
    HAND_OVER_VOLUME_CAPABILITY, INITIALIZE_MACHINE_CAPABILITY, INSPECT_CONTAINER_CAPABILITY,
    INSPECT_MACHINE_CAPABILITY, INSPECT_MACHINE_UPGRADE_CAPABILITY, INSPECT_RECEIVE_CAPABILITY,
    INSPECT_STORAGE_CAPABILITY, INSPECT_VOLUME_CAPABILITY, INSPECT_VOLUME_COPY_CAPABILITY,
    INSPECT_WIREGUARD_CAPABILITY, JOIN_MACHINE_CAPABILITY, LIST_CONTAINERS_CAPABILITY,
    LIST_IMAGES_CAPABILITY, LIST_MACHINES_CAPABILITY, LIST_VOLUMES_CAPABILITY,
    LOG_HISTORY_CAPABILITY, MACHINE_LOGS_CAPABILITY, MACHINE_STORAGE_OBSERVATION_CAPABILITY,
    MACHINE_TOKEN_CAPABILITY, MARK_CONTAINER_STOPPING_CAPABILITY, MINT_BUILD_GRANT_CAPABILITY,
    PREPARE_VOLUMES_CAPABILITY, PROMOTE_VOLUME_CAPABILITY, PRUNE_MIRROR_CAPABILITY,
    PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY, PULL_IMAGE_FROM_MACHINE_CAPABILITY,
    REGISTER_MACHINE_CAPABILITY, REMOVE_CONTAINER_CAPABILITY, REMOVE_IMAGES_CAPABILITY,
    REMOVE_LOCAL_MACHINE_CAPABILITY, REMOVE_MACHINE_CAPABILITY, REMOVE_VOLUME_CAPABILITY,
    REQUEST_MACHINE_UPGRADE_CAPABILITY, RESET_MACHINE_CAPABILITY, RESTORE_VOLUME_CAPABILITY,
    RUNTIME_WATCH_CAPABILITY, Rpc, SET_MANAGEMENT_CLIENT_CAPABILITY, START_CONTAINER_CAPABILITY,
    START_HANDED_CONTAINER_CAPABILITY, START_RECEIVE_CAPABILITY, STOP_CONTAINER_CAPABILITY,
    THAW_VOLUME_CAPABILITY, UPDATE_MACHINE_CAPABILITY, WARM_SNAPSHOT_CAPABILITY,
    WITHDRAW_VOLUME_CAPABILITY, op,
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
            SET_MANAGEMENT_CLIENT_CAPABILITY,
            "ployz.machine.set-management-client.v1",
        ),
        (LIST_MACHINES_CAPABILITY, "ployz.machine.list.v1"),
        (
            REQUEST_MACHINE_UPGRADE_CAPABILITY,
            "ployz.machine.upgrade.request.v1",
        ),
        (
            INSPECT_MACHINE_UPGRADE_CAPABILITY,
            "ployz.machine.upgrade.inspect.v1",
        ),
        (
            MACHINE_STORAGE_OBSERVATION_CAPABILITY,
            "ployz.machine.storage-observation.v1",
        ),
        (LIST_CONTAINERS_CAPABILITY, "ployz.container.list.v1"),
        (INSPECT_CONTAINER_CAPABILITY, "ployz.container.inspect.v1"),
        (CREATE_CONTAINER_CAPABILITY, "ployz.container.create.v1"),
        (START_CONTAINER_CAPABILITY, "ployz.container.start.v1"),
        (STOP_CONTAINER_CAPABILITY, "ployz.container.stop.v1"),
        (
            MARK_CONTAINER_STOPPING_CAPABILITY,
            "ployz.container.mark-stopping.v1",
        ),
        (REMOVE_CONTAINER_CAPABILITY, "ployz.container.remove.v1"),
        (CREATE_VOLUME_CAPABILITY, "ployz.volume.create.v1"),
        (INSPECT_STORAGE_CAPABILITY, "ployz.storage.inspect.v1"),
        (PREPARE_VOLUMES_CAPABILITY, "ployz.storage.prepare.v1"),
        (LIST_VOLUMES_CAPABILITY, "ployz.volume.list.v1"),
        (INSPECT_VOLUME_CAPABILITY, "ployz.volume.inspect.v1"),
        (REMOVE_VOLUME_CAPABILITY, "ployz.volume.remove.v1"),
        (
            INSPECT_VOLUME_COPY_CAPABILITY,
            "ployz.volume.copy.inspect.v1",
        ),
        (ADOPT_LEASE_CAPABILITY, "ployz.volume.lease.adopt.v1"),
        (WITHDRAW_VOLUME_CAPABILITY, "ployz.volume.withdraw.v1"),
        (FREEZE_VOLUME_CAPABILITY, "ployz.volume.freeze.v1"),
        (HAND_OVER_VOLUME_CAPABILITY, "ployz.volume.hand-over.v1"),
        (THAW_VOLUME_CAPABILITY, "ployz.volume.thaw.v1"),
        (CLOSE_VOLUME_CAPABILITY, "ployz.volume.close.v1"),
        (
            ACCEPT_HAND_OFF_CAPABILITY,
            "ployz.volume.accept-hand-off.v1",
        ),
        (PROMOTE_VOLUME_CAPABILITY, "ployz.volume.promote.v1"),
        (
            START_HANDED_CONTAINER_CAPABILITY,
            "ployz.volume.start-handed-container.v1",
        ),
        (CLEAR_FINAL_CAPABILITY, "ployz.volume.clear-final.v1"),
        (RESTORE_VOLUME_CAPABILITY, "ployz.volume.restore.v1"),
        (DECLARE_MIRROR_CAPABILITY, "ployz.volume.mirror.declare.v1"),
        (BEGIN_ROUND_CAPABILITY, "ployz.volume.round.begin.v1"),
        (COMMIT_SNAPSHOTS_CAPABILITY, "ployz.volume.round.commit.v1"),
        (WARM_SNAPSHOT_CAPABILITY, "ployz.volume.round.warm.v1"),
        (START_RECEIVE_CAPABILITY, "ployz.volume.receive.start.v1"),
        (
            INSPECT_RECEIVE_CAPABILITY,
            "ployz.volume.receive.inspect.v1",
        ),
        (PRUNE_MIRROR_CAPABILITY, "ployz.volume.round.prune.v1"),
        (DESTROY_MIRROR_CAPABILITY, "ployz.volume.mirror.destroy.v1"),
        (FORGET_SNAPSHOTS_CAPABILITY, "ployz.volume.mirror.forget.v1"),
        (LIST_IMAGES_CAPABILITY, "ployz.image.list.v1"),
        (REMOVE_IMAGES_CAPABILITY, "ployz.image.remove.v1"),
        (
            ENSURE_IMAGE_INGEST_CAPABILITY,
            "ployz.image.ingest.ensure.v1",
        ),
        (
            PULL_IMAGE_FROM_MACHINE_CAPABILITY,
            "ployz.image.pull-from-machine.v1",
        ),
        (MINT_BUILD_GRANT_CAPABILITY, "ployz.build.grant.mint.v1"),
        (END_BUILD_GRANT_CAPABILITY, "ployz.build.grant.end.v1"),
        (
            GET_INGRESS_PROXY_CONFIG_CAPABILITY,
            "ployz.ingress.config.v1",
        ),
        (
            PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY,
            "ployz.certificates.publish.v1",
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
        (LOG_HISTORY_CAPABILITY, "ployz.logs.history.v2"),
        (FORGET_LOGS_CAPABILITY, "ployz.logs.forget.v1"),
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
            "ployz.machine.set-management-client.v1",
            "ployz.machine.list.v1",
            "ployz.machine.update.v1",
            "ployz.machine.upgrade.request.v1",
            "ployz.machine.upgrade.inspect.v1",
            "ployz.machine.remove-local.v1",
            "ployz.machine.remove.v1",
            "ployz.wireguard.inspect.v1",
            "ployz.machine.reset.v1",
            "ployz.runtime.watch.v1",
            "ployz.certificates.policy.v1",
            "ployz.machine.storage-observation.v1",
        ]
    );
    assert_eq!(
        names(CapabilityAdvertisement::Container),
        [
            "ployz.container.list.v1",
            "ployz.container.inspect.v1",
            "ployz.container.create.v1",
            "ployz.container.start.v1",
            "ployz.container.stop.v1",
            "ployz.container.mark-stopping.v1",
            "ployz.container.remove.v1",
            "ployz.volume.create.v1",
            "ployz.storage.inspect.v1",
            "ployz.storage.prepare.v1",
            "ployz.volume.list.v1",
            "ployz.volume.inspect.v1",
            "ployz.volume.remove.v1",
            "ployz.volume.copy.inspect.v1",
            "ployz.volume.lease.adopt.v1",
            "ployz.volume.withdraw.v1",
            "ployz.volume.freeze.v1",
            "ployz.volume.hand-over.v1",
            "ployz.volume.thaw.v1",
            "ployz.volume.close.v1",
            "ployz.volume.accept-hand-off.v1",
            "ployz.volume.promote.v1",
            "ployz.volume.start-handed-container.v1",
            "ployz.volume.clear-final.v1",
            "ployz.volume.restore.v1",
            "ployz.volume.mirror.declare.v1",
            "ployz.volume.round.begin.v1",
            "ployz.volume.round.commit.v1",
            "ployz.volume.round.warm.v1",
            "ployz.volume.receive.start.v1",
            "ployz.volume.receive.inspect.v1",
            "ployz.volume.round.prune.v1",
            "ployz.volume.mirror.destroy.v1",
            "ployz.volume.mirror.forget.v1",
            "ployz.image.list.v1",
            "ployz.image.remove.v1",
            "ployz.image.ingest.ensure.v1",
            "ployz.image.pull-from-machine.v1",
            "ployz.build.grant.mint.v1",
            "ployz.build.grant.end.v1",
            "ployz.logs.forget.v1",
            "ployz.logs.history.v2",
            "ployz.container.logs.v1",
            "ployz.machine.logs.v1",
            "ployz.container.exec.v1",
            "ployz.build.v1",
        ]
    );
    assert_eq!(
        names(CapabilityAdvertisement::Ingress),
        ["ployz.ingress.config.v1"]
    );
    assert_eq!(
        names(CapabilityAdvertisement::Cluster),
        [
            "ployz.container.observations.v1",
            "ployz.certificates.publish.v1",
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
        op::GetIngressProxyConfig::PATH,
        "/ployz.rpc.v1.MachineRpc/GetIngressProxyConfig"
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
        op::PublishCertificateMaterial::PATH,
        "/ployz.rpc.v1.MachineRpc/PublishCertificateMaterial"
    );
    assert_eq!(
        op::SetManagementClient::PATH,
        "/ployz.rpc.v1.MachineRpc/SetManagementClient"
    );
    assert_eq!(
        op::RequestMachineUpgrade::PATH,
        "/ployz.rpc.v1.MachineRpc/RequestMachineUpgrade"
    );
    assert_eq!(
        op::InspectMachineUpgrade::PATH,
        "/ployz.rpc.v1.MachineRpc/InspectMachineUpgrade"
    );
}
