use ployz_core::{
    BUILD_GRANT_HOLDER_CAPABILITY, BUILD_GRANT_PREVIEW_CAPABILITY, BUILD_GRANT_RULE_CAPABILITY,
    BUILD_SEALED_CAPABILITY, CERTIFICATE_POLICY_CAPABILITY, COMMIT_APPROVAL_CAPABILITY,
    CapabilityAdvertisement, CapabilityName, DEPLOY_PREVIEW_CAPABILITY, DEPLOY_RULE_CAPABILITY,
    DESCRIBE_CONTRACT_CAPABILITY, ENROLL_FOR_RECOVERY_CAPABILITY, ENROLL_MACHINE_CAPABILITY,
    ENSURE_CONTAINER_CAPABILITY, ENSURE_IMAGE_INGEST_CAPABILITY, ENV_SEALED_CAPABILITY,
    EXEC_CONTAINER_CAPABILITY, FORGET_MACHINE_CAPABILITY, GENERATE_SEALING_KEY_CAPABILITY,
    INSPECT_BUILD_GRANT_CAPABILITY, INSPECT_CONTAINER_CAPABILITY, INSPECT_MACHINE_CAPABILITY,
    INSPECT_STORAGE_CAPABILITY, INSPECT_VOLUME_COPY_CAPABILITY, INSTALL_SEALING_KEY_CAPABILITY,
    LEAVE_MACHINE_CAPABILITY, LIST_CONTAINERS_CAPABILITY, LIST_IMAGES_CAPABILITY,
    LIST_MACHINES_CAPABILITY, LIST_VOLUMES_CAPABILITY, LOG_HISTORY_CAPABILITY,
    MACHINE_STORAGE_OBSERVATION_CAPABILITY, MANAGEMENT_SCOPE_CAPABILITY, METRIC_HISTORY_CAPABILITY,
    MINT_BUILD_GRANT_CAPABILITY, PREPARE_VOLUMES_CAPABILITY, PREVIEW_REVIEW_CAPABILITY,
    PRUNE_IMAGES_CAPABILITY, PUBLISH_CERTIFICATE_MATERIAL_CAPABILITY, PULL_IMAGE_CAPABILITY,
    PULL_IMAGE_FROM_MACHINE_CAPABILITY, READ_CLUSTER_NAME_CAPABILITY,
    READ_ROUTED_ADDRESSES_CAPABILITY, READ_ROUTES_CAPABILITY, RECOVER_SEALING_KEY_CAPABILITY,
    REGISTER_MACHINE_CAPABILITY, REMOVE_TRUSTED_KEY_CAPABILITY, REMOVE_VOLUME_CAPABILITY,
    REQUEST_MACHINE_UPGRADE_CAPABILITY, RESTART_APPROVAL_CAPABILITY,
    RESTART_SESSION_GRANT_CAPABILITY, RETAINED_ENV_CAPABILITY, RULE_PASSKEY_CAPABILITY,
    RUNTIME_WATCH_CAPABILITY, Rpc, SEED_VOLUME_CAPABILITY, SELECT_ENDPOINT_CAPABILITY,
    SET_CLUSTER_NAME_CAPABILITY, SET_DEVICE_CAPABILITY, SET_DEVICE_ENFORCEMENT_CAPABILITY,
    SET_INGRESS_DOORS_CAPABILITY, SET_INGRESS_TUNNEL_CAPABILITY, SET_MANAGEMENT_CLIENT_CAPABILITY,
    SET_PASSKEY_CAPABILITY, SET_PRIVATE_ACCESS_CAPABILITY, SET_PUSH_TARGET_CAPABILITY,
    SET_VOLUME_COPY_CAPABILITY, SET_VOLUME_SIZE_CAPABILITY, SHARE_SEALING_KEY_CAPABILITY,
    TAIL_LOGS_CAPABILITY, UPDATE_CONTAINER_LIMITS_CAPABILITY, UPDATE_MACHINE_CAPABILITY,
    UPGRADE_EXACT_CAPABILITY, VOLUME_APPROVAL_CAPABILITY, VOLUME_FLOOR_CAPABILITY,
    VOLUME_SHRINK_CAPABILITY, WRAP_SEALING_KEY_CAPABILITY, op,
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
        (
            ENROLL_FOR_RECOVERY_CAPABILITY,
            "ployz.machine.enroll-for-recovery.v1",
        ),
        (WRAP_SEALING_KEY_CAPABILITY, "ployz.sealing.wrap.v1"),
        (RECOVER_SEALING_KEY_CAPABILITY, "ployz.sealing.recover.v1"),
        (REGISTER_MACHINE_CAPABILITY, "ployz.machine.register.v1"),
        (
            SET_MANAGEMENT_CLIENT_CAPABILITY,
            "ployz.machine.set-management-client.v1",
        ),
        (SET_DEVICE_CAPABILITY, "ployz.machine.set-device.v1"),
        (
            SET_PUSH_TARGET_CAPABILITY,
            "ployz.machine.set-push-target.v1",
        ),
        (
            SELECT_ENDPOINT_CAPABILITY,
            "ployz.network.select-endpoint.v1",
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
        (MANAGEMENT_SCOPE_CAPABILITY, "ployz.management.scope.v1"),
        (UPGRADE_EXACT_CAPABILITY, "ployz.machine.upgrade.exact.v1"),
        (ENV_SEALED_CAPABILITY, "ployz.env.sealed.v1"),
        (BUILD_SEALED_CAPABILITY, "ployz.build.sealed.v1"),
        (BUILD_GRANT_HOLDER_CAPABILITY, "ployz.build.grant.holder.v1"),
        (BUILD_GRANT_RULE_CAPABILITY, "ployz.build.grant.rule.v1"),
        (DEPLOY_RULE_CAPABILITY, "ployz.container.deploy-rule.v1"),
        (RULE_PASSKEY_CAPABILITY, "ployz.rule.passkey.v1"),
        (
            BUILD_GRANT_PREVIEW_CAPABILITY,
            "ployz.build.grant.preview.v1",
        ),
        (
            DEPLOY_PREVIEW_CAPABILITY,
            "ployz.container.deploy-preview.v1",
        ),
        (PREVIEW_REVIEW_CAPABILITY, "ployz.preview.review.v1"),
        (COMMIT_APPROVAL_CAPABILITY, "ployz.rule.commit-approval.v1"),
        (
            SET_DEVICE_ENFORCEMENT_CAPABILITY,
            "ployz.machine.set-device-enforcement.v2",
        ),
        (SET_PASSKEY_CAPABILITY, "ployz.machine.set-passkey.v1"),
        (
            REMOVE_TRUSTED_KEY_CAPABILITY,
            "ployz.machine.remove-trusted-key.v1",
        ),
        (
            RESTART_APPROVAL_CAPABILITY,
            "ployz.restart.device-approval.v2",
        ),
        (LIST_CONTAINERS_CAPABILITY, "ployz.container.list.v1"),
        (INSPECT_CONTAINER_CAPABILITY, "ployz.container.inspect.v1"),
        (ENSURE_CONTAINER_CAPABILITY, "ployz.container.ensure.v1"),
        (INSPECT_STORAGE_CAPABILITY, "ployz.storage.inspect.v1"),
        (PREPARE_VOLUMES_CAPABILITY, "ployz.storage.prepare.v1"),
        (LIST_VOLUMES_CAPABILITY, "ployz.volume.list.v1"),
        (REMOVE_VOLUME_CAPABILITY, "ployz.volume.remove.v1"),
        (
            UPDATE_CONTAINER_LIMITS_CAPABILITY,
            "ployz.container.limits.update.v1",
        ),
        (
            INSPECT_VOLUME_COPY_CAPABILITY,
            "ployz.volume.copy.inspect.v1",
        ),
        (SEED_VOLUME_CAPABILITY, "ployz.volume.seed.v1"),
        (SET_VOLUME_COPY_CAPABILITY, "ployz.volume.copy.set.v1"),
        (SET_VOLUME_SIZE_CAPABILITY, "ployz.volume.size.set.v1"),
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
        (READ_ROUTES_CAPABILITY, "ployz.serving.routes.v1"),
        (
            READ_ROUTED_ADDRESSES_CAPABILITY,
            "ployz.serving.routed-addresses.v1",
        ),
        (SET_INGRESS_DOORS_CAPABILITY, "ployz.ingress.doors.v1"),
        (SET_INGRESS_TUNNEL_CAPABILITY, "ployz.ingress.tunnel.v1"),
        (READ_CLUSTER_NAME_CAPABILITY, "ployz.cluster.name.read.v1"),
        (SET_CLUSTER_NAME_CAPABILITY, "ployz.cluster.name.v1"),
        (SET_PRIVATE_ACCESS_CAPABILITY, "ployz.private-access.v1"),
        (UPDATE_MACHINE_CAPABILITY, "ployz.machine.update.v1"),
        (GENERATE_SEALING_KEY_CAPABILITY, "ployz.sealing.generate.v1"),
        (SHARE_SEALING_KEY_CAPABILITY, "ployz.sealing.share.v1"),
        (INSTALL_SEALING_KEY_CAPABILITY, "ployz.sealing.install.v1"),
        (LEAVE_MACHINE_CAPABILITY, "ployz.machine.leave.v1"),
        (FORGET_MACHINE_CAPABILITY, "ployz.machine.forget.v1"),
        (
            CERTIFICATE_POLICY_CAPABILITY,
            "ployz.certificates.policy.v1",
        ),
        (LOG_HISTORY_CAPABILITY, "ployz.logs.history.v2"),
        (METRIC_HISTORY_CAPABILITY, "ployz.metrics.history.v1"),
        (RETAINED_ENV_CAPABILITY, "ployz.logs.retained-env.v1"),
        (TAIL_LOGS_CAPABILITY, "ployz.logs.tail.v1"),
        (RUNTIME_WATCH_CAPABILITY, "ployz.runtime.watch.v1"),
        (EXEC_CONTAINER_CAPABILITY, "ployz.container.exec.v1"),
        (VOLUME_FLOOR_CAPABILITY, "ployz.volume.copy.floor.v1"),
        (VOLUME_SHRINK_CAPABILITY, "ployz.volume.size.shrink.v1"),
        (VOLUME_APPROVAL_CAPABILITY, "ployz.volume.approval.v1"),
        (
            RESTART_SESSION_GRANT_CAPABILITY,
            "ployz.restart.session-grant.v2",
        ),
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
            "ployz.machine.enroll-for-recovery.v1",
            "ployz.sealing.wrap.v1",
            "ployz.sealing.recover.v1",
            "ployz.machine.register.v1",
            "ployz.machine.set-management-client.v1",
            "ployz.machine.join.pending.v1",
            "ployz.machine.join.approve.v1",
            "ployz.machine.set-device.v1",
            "ployz.machine.set-push-target.v1",
            "ployz.machine.set-device-enforcement.v2",
            "ployz.machine.set-passkey.v1",
            "ployz.machine.remove-trusted-key.v1",
            "ployz.network.select-endpoint.v1",
            "ployz.machine.list.v1",
            "ployz.machine.update.v1",
            "ployz.machine.upgrade.request.v1",
            "ployz.sealing.generate.v1",
            "ployz.sealing.share.v1",
            "ployz.sealing.install.v1",
            "ployz.machine.leave.v1",
            "ployz.machine.forget.v1",
            "ployz.machine.join.device-approval.v1",
            "ployz.certificates.policy.v1",
            "ployz.machine.storage-observation.v1",
            "ployz.management.scope.v1",
            "ployz.machine.upgrade.exact.v1",
            "ployz.env.sealed.v1",
            "ployz.build.sealed.v1",
            "ployz.build.grant.holder.v1",
            "ployz.build.grant.rule.v1",
            "ployz.build.grant.preview.v1",
            "ployz.restart.device-approval.v2",
            "ployz.restart.session-grant.v2",
            "ployz.restart.start-approval.v1",
            "ployz.restart.proposal.v1",
            "ployz.container.deploy-approval.v1",
            "ployz.container.deploy-proposal.v1",
            "ployz.container.deploy-rule.v1",
            "ployz.rule.passkey.v1",
            "ployz.container.deploy-preview.v1",
            "ployz.preview.review.v1",
            "ployz.rule.commit-approval.v1",
        ]
    );
    assert_eq!(
        names(CapabilityAdvertisement::Container),
        [
            "ployz.container.list.v1",
            "ployz.container.inspect.v1",
            "ployz.container.ensure.v1",
            "ployz.container.limits.update.v1",
            "ployz.storage.inspect.v1",
            "ployz.storage.prepare.v1",
            "ployz.volume.list.v1",
            "ployz.volume.remove.v1",
            "ployz.volume.copy.inspect.v1",
            "ployz.volume.seed.v1",
            "ployz.volume.copy.set.v1",
            "ployz.volume.size.set.v1",
            "ployz.image.list.v1",
            "ployz.image.prune.v1",
            "ployz.image.ingest.ensure.v1",
            "ployz.image.pull.v1",
            "ployz.image.pull-from-machine.v1",
            "ployz.build.grant.mint.v1",
            "ployz.build.grant.inspect.v1",
            "ployz.metrics.history.v1",
            "ployz.logs.retained-env.v1",
            "ployz.logs.history.v2",
            "ployz.logs.tail.v1",
            "ployz.container.exec.v1",
            "ployz.build.v1",
            "ployz.volume.copy.floor.v1",
            "ployz.volume.size.shrink.v1",
            "ployz.volume.approval.v1",
        ]
    );
    assert_eq!(
        names(CapabilityAdvertisement::Cluster),
        [
            "ployz.certificates.publish.v1",
            "ployz.serving.routes.v1",
            "ployz.serving.routed-addresses.v1",
            "ployz.ingress.doors.v1",
            "ployz.private-access.v1",
            "ployz.cluster.name.read.v1",
            "ployz.cluster.name.v1",
            "ployz.runtime.watch.v1",
            "ployz.ingress.tunnel.v1",
            "ployz.private-access.approval.v1",
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
        op::EnrollForRecovery::PATH,
        "/ployz.rpc.v1.MachineRpc/EnrollForRecovery"
    );
    assert_eq!(
        op::WrapSealingKey::PATH,
        "/ployz.rpc.v1.MachineRpc/WrapSealingKey"
    );
    assert_eq!(
        op::RecoverSealingKey::PATH,
        "/ployz.rpc.v1.MachineRpc/RecoverSealingKey"
    );
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
        op::SelectEndpoint::PATH,
        "/ployz.rpc.v1.MachineRpc/SelectEndpoint"
    );
    assert_eq!(
        op::RequestMachineUpgrade::PATH,
        "/ployz.rpc.v1.MachineRpc/RequestMachineUpgrade"
    );
    assert_eq!(
        op::GenerateSealingKey::PATH,
        "/ployz.rpc.v1.MachineRpc/GenerateSealingKey"
    );
    assert_eq!(
        op::ShareSealingKey::PATH,
        "/ployz.rpc.v1.MachineRpc/ShareSealingKey"
    );
    assert_eq!(
        op::InstallSealingKey::PATH,
        "/ployz.rpc.v1.MachineRpc/InstallSealingKey"
    );
}

/// Capabilities every Machine in the field serves. Serving rotation cut off daemons
/// that predate serving routes. Raise it only when no Machine runs older.
const FIELD_FLOOR: &[&str] = &[
    "ployz.build.grant.inspect.v1",
    "ployz.build.grant.mint.v1",
    "ployz.build.v1",
    "ployz.certificates.policy.v1",
    "ployz.certificates.publish.v1",
    "ployz.container.ensure.v1",
    "ployz.container.exec.v1",
    "ployz.container.inspect.v1",
    "ployz.container.list.v1",
    "ployz.image.ingest.ensure.v1",
    "ployz.image.list.v1",
    "ployz.image.prune.v1",
    "ployz.image.pull-from-machine.v1",
    "ployz.logs.history.v2",
    "ployz.logs.tail.v1",
    "ployz.machine.enroll.v1",
    "ployz.machine.forget.v1",
    "ployz.machine.inspect.v1",
    "ployz.machine.leave.v1",
    "ployz.machine.list.v1",
    "ployz.machine.register.v1",
    "ployz.machine.set-management-client.v1",
    "ployz.machine.storage-observation.v1",
    "ployz.machine.update.v1",
    "ployz.machine.upgrade.request.v1",
    "ployz.metrics.history.v1",
    "ployz.rpc.describe-contract.v1",
    "ployz.runtime.watch.v1",
    "ployz.serving.routes.v1",
    "ployz.storage.inspect.v1",
    "ployz.storage.prepare.v1",
    "ployz.volume.copy.floor.v1",
    "ployz.volume.copy.inspect.v1",
    "ployz.volume.copy.set.v1",
    "ployz.volume.list.v1",
    "ployz.volume.remove.v1",
    "ployz.volume.seed.v1",
];

/// Each capability above the floor, and what a caller does when a Machine lacks it.
const ABOVE_FLOOR: &[(&str, &str)] = &[
    (
        "ployz.network.select-endpoint.v1",
        "refused legibly: `server set --wg-endpoint` lists the Server as missed and still updates the target",
    ),
    (
        "ployz.machine.set-push-target.v1",
        "gated: the SDK checks the contract before calling",
    ),
    (
        "ployz.machine.set-device-enforcement.v2",
        "gated: `ployz device enforce` checks each Machine's contract first, names a Server whose daemon lacks it as missing and exits partial",
    ),
    (
        "ployz.machine.set-passkey.v1",
        "gated: `ployz device approve --passkey` checks each Machine's contract first, names a Server whose daemon lacks it as missing and exits partial",
    ),
    (
        "ployz.machine.remove-trusted-key.v1",
        "falls back: `ployz device rm` calls set-device.v1 Remove on an older daemon, which can hold no passkey",
    ),
    (
        "ployz.restart.device-approval.v2",
        "falls back: an older daemon ignores the approval on EnsureContainer and restarts as before, since it enforces nothing",
    ),
    (
        "ployz.restart.session-grant.v2",
        "gated: Cloud attaches a browser approval only for a Machine that advertises it; an older enforcing Machine refuses the restart and the dashboard names that Server. A grant naming its Environment's ID fails closed on a daemon that reads only v1 grants, which refuses unknown grant fields and lends the browser key nothing",
    ),
    (
        "ployz.restart.proposal.v1",
        "gated: Cloud sends a proposal step only to a Machine that advertises it, and names an older one as needing a newer Ployz before the person signs; an older enforcing Machine refuses the step's envelope as unknown bytes",
    ),
    (
        "ployz.restart.start-approval.v1",
        "gated: Cloud attaches a start approval only for a Machine that advertises it, and names an older one as needing a newer Ployz; the CLI signs one regardless, which an older enforcing Machine refuses as it refused an unapproved start",
    ),
    (
        "ployz.container.deploy-approval.v1",
        "falls back: an older daemon ignores the approval on EnsureContainer; one not enforcing creates as before, and one enforcing refuses every Operator creation as it did, naming the restart capability",
    ),
    (
        "ployz.container.deploy-proposal.v1",
        "gated: B5 must have Cloud send a deploy step of a proposal only to a Machine that advertises it, since nothing sends one yet; an older enforcing Machine refuses one, as it refuses any deploy approval it can't verify",
    ),
    (
        "ployz.container.deploy-rule.v1",
        "gated: Cloud attaches a rule proof only for a Machine that advertises it. An older daemon ignores `rule`, and under Device enforcement it refuses the Operator's creation for want of a deploy approval, so a rule deploy there fails closed",
    ),
    (
        "ployz.rule.passkey.v1",
        "gated: the dashboard signs a rule with a passkey only when every enforcing Machine advertises it. An older daemon refuses the rule's envelope for its unknown `assertions` field, and its `\"passkey\"` signer, so a rule deploy there fails closed",
    ),
    (
        "ployz.container.deploy-preview.v1",
        "falls back: an older daemon ignores `preview` on EnsureContainer; one not enforcing creates as before, and one enforcing refuses the Operator's creation for want of a deploy approval, so a preview there fails closed",
    ),
    (
        "ployz.preview.review.v1",
        "gated: Cloud relays a review at grant only to a Machine that advertises it, and the dashboard offers a review only when every enforcing Machine does. An older daemon refuses every reviewed preview build and deploy, naming a reviewed preview, so one there fails closed",
    ),
    (
        "ployz.rule.commit-approval.v1",
        "gated: Cloud relays a commit approval at grant only to a Machine that advertises it, and the CLI signs one only when every Server it lists does. An older daemon refuses the approval's field in the grant and in the rule proof as unknown, so a rollback there fails closed",
    ),
    (
        "ployz.image.pull.v1",
        "falls back: an older daemon pulls when it creates the Container",
    ),
    (
        "ployz.ingress.tunnel.v1",
        "gated: `ployz ingress use tunnel` checks each Machine's contract first and names a Server whose daemon lacks it as needing an upgrade; an older daemon refuses the Tunnel Door as unsupported",
    ),
    (
        "ployz.private-access.approval.v1",
        "falls back: an older daemon ignores `approval` on SetPrivateAccess; one not enforcing turns Private Access on as before, and one enforcing refuses an Operator's SetPrivateAccess as it did, naming the route",
    ),
    (
        "ployz.volume.size.set.v1",
        "gated: a caller checks the contract first and refuses the resize, naming the Server whose daemon lacks it",
    ),
    (
        "ployz.volume.approval.v1",
        "falls back: an older daemon ignores the approval and the Forget recipient. It checks no approval and grants nothing, so it pulls and serves the send stream as before, and its Forget writes no withdrawal",
    ),
    (
        "ployz.volume.size.shrink.v1",
        "gated: a caller checks the contract on every Machine holding a copy and refuses the shrink, naming the Server whose daemon lacks it",
    ),
    (
        "ployz.container.limits.update.v1",
        "gated: the planner updates in place only on a Machine advertising it, and replaces the Container on any other",
    ),
    (
        "ployz.logs.retained-env.v1",
        "falls back: the SDK asks it only after InspectContainer answers NotFound, and a refusal withholds the Container's Sealed lines as before",
    ),
    (
        "ployz.serving.routed-addresses.v1",
        "claims nothing: status names no stale address for a Server whose daemon refuses it",
    ),
    (
        "ployz.ingress.doors.v1",
        "gated: the CLI calls SetIngressDoors only on Machines advertising it, falls back: an older daemon keeps its running Ingress Proxy and the CLI names the Server as needing a newer Ployz, claims nothing: no proxy change on that Machine",
    ),
    (
        "ployz.cluster.name.read.v1",
        "refused legibly: `ployz cluster name` names the Server whose daemon lacks it",
    ),
    (
        "ployz.cluster.name.v1",
        "gated: `ployz cluster name <name>` and the SDK check the contract first and refuse, naming the Server whose daemon lacks it",
    ),
    (
        "ployz.private-access.v1",
        "gated: `ployz private-access on` and `off` check each Machine's contract first and name a Server whose daemon lacks it as needing an upgrade, so that Machine runs no router",
    ),
    (
        "ployz.management.scope.v1",
        "gated: an older daemon serves an Operator caller as Full, so a path that sends a Sealed value first checks every Machine in the Cluster advertises it, and refuses otherwise",
    ),
    (
        "ployz.machine.upgrade.exact.v1",
        "claims nothing: Ployz Cloud names only a Release Channel, and an older daemon still takes any exact version from an Operator",
    ),
    (
        "ployz.env.sealed.v1",
        "gated: a path that sends a Sealed value first checks every Machine in the Cluster advertises it, and refuses naming each Machine that lacks it",
    ),
    (
        "ployz.build.sealed.v1",
        "gated: Builder selection for a Build that reads a Sealed value considers only Machines advertising it, and refuses naming each Machine that lacks it",
    ),
    (
        "ployz.build.grant.holder.v1",
        "gated: a GitHub check-in mints for the runner's key only on a Machine advertising it; otherwise it mints the old grant, and a Build that reads a Sealed value goes to its next Builder",
    ),
    (
        "ployz.build.grant.rule.v1",
        "gated: a rule build's grant request carries `rule` only to a Machine advertising it. An older daemon ignores `rule`, and under Device enforcement it refuses the Operator's mint, so a rule build there fails closed",
    ),
    (
        "ployz.build.grant.preview.v1",
        "gated: a preview build's grant request goes only to a Machine advertising it, and otherwise the build fails with \"Previews under a preview rule need a newer Ployz on your servers\"",
    ),
    (
        "ployz.sealing.generate.v1",
        "gated: the Sealing Key backfill reads each Machine's contract first and never picks a generator that lacks it",
    ),
    (
        "ployz.sealing.share.v1",
        "gated: the backfill seals from a holder advertising it, and reports a Cluster whose holders all lack it",
    ),
    (
        "ployz.sealing.install.v1",
        "gated: the backfill skips a Machine that lacks it and reports it as skipped",
    ),
    (
        "ployz.machine.enroll-for-recovery.v1",
        "gated: founding for recovery reads the new daemon's contract first and refuses one that lacks it",
    ),
    (
        "ployz.sealing.wrap.v1",
        "gated: `ployz recovery rotate` reads the contract first and names a Server that lacks it",
    ),
    (
        "ployz.sealing.recover.v1",
        "gated: `ployz recover` reads the contract first and names a Server that lacks it",
    ),
    (
        "ployz.machine.set-device.v1",
        "gated: `ployz device` checks each Machine's contract first, names a Server whose daemon lacks it as missing and exits partial",
    ),
    (
        "ployz.machine.join.pending.v1",
        "refused legibly: `ployz device approve --join` stops at a Server whose daemon lacks it, and the refusal names the capability",
    ),
    (
        "ployz.machine.join.approve.v1",
        "unreached: ApproveJoin goes only to the Machine that listed the pending join, which has it",
    ),
    (
        "ployz.machine.join.device-approval.v1",
        "falls back: an older daemon never refuses Register with it, so the join proceeds unsigned as before",
    ),
];

/// A new RPC reaches Machines that predate it. Before adding a catalog row, decide
/// what callers do when a Machine refuses it, and record that in `ABOVE_FLOOR`.
#[test]
fn every_capability_above_the_field_floor_says_how_older_machines_are_handled() {
    let mut catalogued: Vec<String> = [
        CapabilityAdvertisement::Always,
        CapabilityAdvertisement::Container,
        CapabilityAdvertisement::Cluster,
    ]
    .into_iter()
    .flat_map(names)
    .collect();
    catalogued.sort();
    let mut known: Vec<&str> = FIELD_FLOOR
        .iter()
        .copied()
        .chain(ABOVE_FLOOR.iter().map(|(name, _)| *name))
        .collect();
    known.sort_unstable();
    let unhandled: Vec<&String> = catalogued
        .iter()
        .filter(|name| !known.contains(&name.as_str()))
        .collect();
    assert!(
        unhandled.is_empty(),
        "{unhandled:?} reach Machines older than the floor. Gate the call on the contract, \
         fall back, or let the refusal name the Server, then add a row to ABOVE_FLOOR"
    );
    let stale: Vec<&&str> = known
        .iter()
        .filter(|name| !catalogued.iter().any(|c| c == **name))
        .collect();
    assert!(stale.is_empty(), "{stale:?} are no longer catalogued");
}

macro_rules! catalog_routes {
    (
        package $package:literal
        unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident, $unary_scope:ident),)+ }
        server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident, $stream_scope:ident),)+ }
        bidi_streaming { $($bidi_variant:ident: ($bidi_method:ident, $bidi_route:literal, $bidi_command:literal, $bidi_capability:ident, $bidi_capability_name:literal, $bidi_advertisement:ident, $bidi_scope:ident),)+ }
        capabilities { $($(#[$extra_meta:meta])* $extra_capability:ident: ($extra_capability_name:literal, $extra_advertisement:ident),)+ }
    ) => {
        [
            $(($unary_capability_name, stringify!($unary_scope)),)+
            $(($stream_capability_name, stringify!($stream_scope)),)+
            $(($bidi_capability_name, stringify!($bidi_scope)),)+
        ]
    };
}

mod coverage {
    use std::collections::BTreeSet;

    use ployz_core::authority::coverage::{AUTHORIZATION, Authorization, CASES, Expected};

    fn routes() -> Vec<(&'static str, &'static str)> {
        ployz_core::rpc_catalog!(catalog_routes).to_vec()
    }

    fn catalogued(name: &str) -> bool {
        ployz_core::served_capabilities().contains(name)
    }

    #[test]
    fn every_route_has_one_authorization_class() {
        let routes = routes();
        for (capability, _) in &routes {
            let classes = AUTHORIZATION
                .iter()
                .filter(|(name, _)| name == capability)
                .count();
            assert_eq!(
                classes, 1,
                "{capability} has {classes} authorization classes, not one"
            );
        }
        for (name, _) in AUTHORIZATION {
            assert!(
                routes.iter().any(|(capability, _)| capability == name),
                "{name} is classed but is not a catalog route"
            );
        }
    }

    #[test]
    fn full_class_matches_catalog_scope() {
        for (capability, scope) in routes() {
            let class = ployz_core::authority::coverage::authorization(capability)
                .unwrap_or_else(|| panic!("{capability} has no class"));
            assert_eq!(
                class == Authorization::Full,
                scope == "Full",
                "{capability} is classed {class:?} but its catalog scope is {scope}"
            );
        }
    }

    /// `Pending` is a migration state: each entry names the slice that gates it, and a new
    /// route cannot join this list without an edit here.
    #[test]
    fn pending_routes_are_the_frozen_list() {
        let pending: Vec<(&str, &str)> = AUTHORIZATION
            .iter()
            .filter_map(|(name, class)| match class {
                Authorization::Pending(slice) => Some((*name, *slice)),
                Authorization::Read
                | Authorization::Full
                | Authorization::Signed(_)
                | Authorization::Bound(_) => None,
            })
            .collect();
        assert_eq!(
            pending,
            [
                ("ployz.machine.enroll.v1", "F7"),
                ("ployz.machine.enroll-for-recovery.v1", "F7"),
                ("ployz.volume.remove.v1", "E6"),
                ("ployz.volume.copy.set.v1", "E3 to E6"),
                ("ployz.volume.size.set.v1", "E7"),
            ]
        );
    }

    /// The branches the security plan's live matrix lists for a route with several
    /// mutations, as they exist at this slice. A later slice that gives a route a new
    /// branch adds it here and to `CASES`.
    const BRANCHES: &[(&str, &[&str])] = &[
        (
            "ployz.machine.enroll.v1",
            &["member, no capsule", "member, approved capsule", "founder"],
        ),
        (
            "ployz.container.ensure.v1",
            &["create", "start", "stop", "held", "out", "remove"],
        ),
        (
            "ployz.machine.upgrade.request.v1",
            &[
                "a release channel",
                "an exact version no channel resolves to",
                "a downgrade",
            ],
        ),
        ("ployz.private-access.v1", &["on", "off"]),
        (
            "ployz.sealing.generate.v1",
            &[
                "the Cluster publishes no key",
                "the Cluster already publishes one",
            ],
        ),
        (
            "ployz.sealing.install.v1",
            &[
                "sealed by an accepted Machine's key",
                "sealed by any other key",
            ],
        ),
        (
            "ployz.machine.set-management-client.v1",
            &[
                "from Cloud's own connection",
                "from the mesh, or naming a Device slot",
            ],
        ),
        (
            "ployz.volume.copy.set.v1",
            &[
                "a Writer want with a start",
                "a pull",
                "a cut",
                "promote, forget",
                "floor, snapshot",
            ],
        ),
        (
            "ployz.build.grant.mint.v1",
            &["with a rule and its proof", "without"],
        ),
    ];

    #[test]
    fn every_signed_and_bound_route_has_its_cases() {
        let mut seen = BTreeSet::new();
        for case in CASES {
            let class = ployz_core::authority::coverage::authorization(case.capability)
                .unwrap_or_else(|| panic!("case {case:?} names no catalog route"));
            assert!(
                seen.insert((case.capability, case.branch)),
                "{} has two cases for branch {:?}",
                case.capability,
                case.branch
            );
            assert_ne!(
                class,
                Authorization::Read,
                "{} is a read and needs no case",
                case.capability
            );
            if let Expected::Refused(names) = case.expected {
                assert!(!names.is_empty(), "{case:?} names no capability");
                for name in names {
                    assert!(catalogued(name), "{case:?} names uncatalogued {name}");
                }
                if let Authorization::Signed(statements) = class {
                    for name in names {
                        assert!(
                            *name == case.capability
                                || statements
                                    .iter()
                                    .any(|statement| statement.capabilities().contains(name)),
                            "{case:?} names {name}, which neither the route nor its statements are"
                        );
                    }
                }
            }
            if class == Authorization::Full || matches!(class, Authorization::Signed(&[])) {
                assert!(
                    matches!(case.expected, Expected::Refused(_)),
                    "{case:?} must be refused: no statement admits an Operator"
                );
            }
        }
        for (capability, class) in AUTHORIZATION {
            if *class != Authorization::Read {
                assert!(
                    CASES.iter().any(|case| case.capability == *capability),
                    "{capability} is classed {class:?} and has no case"
                );
            }
        }
        for (capability, branches) in BRANCHES {
            for branch in *branches {
                assert!(
                    seen.contains(&(*capability, *branch)),
                    "{capability} has no case for branch {branch:?}"
                );
            }
        }
    }
}
