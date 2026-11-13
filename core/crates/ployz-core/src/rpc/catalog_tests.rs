//! Pins the Machine RPC catalog's names: each route, its JSON command, its capability,
//! how it is advertised, the scope it needs and its shape are frozen values.

use super::*;
use serde_json::json;

/// `(route, command, capability, advertisement, scope, shape)`, in catalog order.
const ROWS: &[(&str, &str, &str, &str, &str, &str)] = &[
    (
        "DescribeContract",
        "describe_contract",
        "ployz.rpc.describe-contract.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "Inspect",
        "inspect",
        "ployz.machine.inspect.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "Enroll",
        "enroll",
        "ployz.machine.enroll.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "EnrollForRecovery",
        "enroll_for_recovery",
        "ployz.machine.enroll-for-recovery.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "WrapSealingKey",
        "wrap_sealing_key",
        "ployz.sealing.wrap.v1",
        "Always",
        "Full",
        "Unary",
    ),
    (
        "RecoverSealingKey",
        "recover_sealing_key",
        "ployz.sealing.recover.v1",
        "Always",
        "Full",
        "Unary",
    ),
    (
        "Register",
        "register",
        "ployz.machine.register.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "SetManagementClient",
        "set_management_client",
        "ployz.machine.set-management-client.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "PendingJoins",
        "pending_joins",
        "ployz.machine.join.pending.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "ApproveJoin",
        "approve_join",
        "ployz.machine.join.approve.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "SetDevice",
        "set_device",
        "ployz.machine.set-device.v1",
        "Always",
        "Full",
        "Unary",
    ),
    (
        "SetPushTarget",
        "set_push_target",
        "ployz.machine.set-push-target.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "SetDeviceEnforcement",
        "set_device_enforcement",
        "ployz.machine.set-device-enforcement.v2",
        "Always",
        "Full",
        "Unary",
    ),
    (
        "ActivateEnforcement",
        "activate_enforcement",
        "ployz.machine.activate.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "SetPasskey",
        "set_passkey",
        "ployz.machine.set-passkey.v1",
        "Always",
        "Full",
        "Unary",
    ),
    (
        "RemoveTrustedKey",
        "remove_trusted_key",
        "ployz.machine.remove-trusted-key.v1",
        "Always",
        "Full",
        "Unary",
    ),
    (
        "SelectEndpoint",
        "select_endpoint",
        "ployz.network.select-endpoint.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "ListMachines",
        "list_machines",
        "ployz.machine.list.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "UpdateMachine",
        "update_machine",
        "ployz.machine.update.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "RequestMachineUpgrade",
        "request_machine_upgrade",
        "ployz.machine.upgrade.request.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "GenerateSealingKey",
        "generate_sealing_key",
        "ployz.sealing.generate.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "ShareSealingKey",
        "share_sealing_key",
        "ployz.sealing.share.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "InstallSealingKey",
        "install_sealing_key",
        "ployz.sealing.install.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "SetRecipientKey",
        "set_recipient_key",
        "ployz.sealing.recipient.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "ResealValues",
        "reseal_values",
        "ployz.sealing.reseal.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "Leave",
        "leave",
        "ployz.machine.leave.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "Forget",
        "forget",
        "ployz.machine.forget.v1",
        "Always",
        "Operator",
        "Unary",
    ),
    (
        "ListContainers",
        "list_containers",
        "ployz.container.list.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "InspectContainer",
        "inspect_container",
        "ployz.container.inspect.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "EnsureContainer",
        "ensure_container",
        "ployz.container.ensure.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "UpdateContainerLimits",
        "update_container_limits",
        "ployz.container.limits.update.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "InspectStorage",
        "inspect_storage",
        "ployz.storage.inspect.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "PrepareVolumes",
        "prepare_volumes",
        "ployz.storage.prepare.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "ListVolumes",
        "list_volumes",
        "ployz.volume.list.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "RemoveVolume",
        "remove_volume",
        "ployz.volume.remove.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "InspectVolumeCopy",
        "inspect_volume_copy",
        "ployz.volume.copy.inspect.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "SeedVolume",
        "seed_volume",
        "ployz.volume.seed.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "SetVolumeCopy",
        "set_volume_copy",
        "ployz.volume.copy.set.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "SetVolumeSize",
        "set_volume_size",
        "ployz.volume.size.set.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "ListImages",
        "list_images",
        "ployz.image.list.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "PruneImages",
        "prune_images",
        "ployz.image.prune.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "EnsureImageIngest",
        "ensure_image_ingest",
        "ployz.image.ingest.ensure.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "PullImage",
        "pull_image",
        "ployz.image.pull.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "PullImageFromMachine",
        "pull_image_from_machine",
        "ployz.image.pull-from-machine.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "MintBuildGrant",
        "mint_build_grant",
        "ployz.build.grant.mint.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "InspectBuildGrant",
        "inspect_build_grant",
        "ployz.build.grant.inspect.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "PublishCertificateMaterial",
        "publish_certificate_material",
        "ployz.certificates.publish.v1",
        "Cluster",
        "Operator",
        "Unary",
    ),
    (
        "ReadRoutes",
        "read_routes",
        "ployz.serving.routes.v1",
        "Cluster",
        "Operator",
        "Unary",
    ),
    (
        "ReadRoutedAddresses",
        "read_routed_addresses",
        "ployz.serving.routed-addresses.v1",
        "Cluster",
        "Operator",
        "Unary",
    ),
    (
        "SetIngressDoors",
        "set_ingress_doors",
        "ployz.ingress.doors.v1",
        "Cluster",
        "Operator",
        "Unary",
    ),
    (
        "SetPrivateAccess",
        "set_private_access",
        "ployz.private-access.v1",
        "Cluster",
        "Operator",
        "Unary",
    ),
    (
        "ReadClusterName",
        "read_cluster_name",
        "ployz.cluster.name.read.v1",
        "Cluster",
        "Operator",
        "Unary",
    ),
    (
        "SetClusterName",
        "set_cluster_name",
        "ployz.cluster.name.v1",
        "Cluster",
        "Operator",
        "Unary",
    ),
    (
        "SetClusterDoors",
        "set_cluster_doors",
        "ployz.cluster.doors.v1",
        "Cluster",
        "Operator",
        "Unary",
    ),
    (
        "MetricHistory",
        "metric_history",
        "ployz.metrics.history.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "RetainedEnv",
        "retained_env",
        "ployz.logs.retained-env.v1",
        "Container",
        "Operator",
        "Unary",
    ),
    (
        "LogHistory",
        "log_history",
        "ployz.logs.history.v2",
        "Container",
        "Operator",
        "ServerStreaming",
    ),
    (
        "TailLogs",
        "tail_logs",
        "ployz.logs.tail.v1",
        "Container",
        "Operator",
        "ServerStreaming",
    ),
    (
        "RuntimeWatch",
        "runtime_watch",
        "ployz.runtime.watch.v1",
        "Cluster",
        "Operator",
        "ServerStreaming",
    ),
    (
        "Exec",
        "exec",
        "ployz.container.exec.v1",
        "Container",
        "Full",
        "BidiStreaming",
    ),
    (
        "Build",
        "build",
        "ployz.build.v1",
        "Container",
        "Operator",
        "BidiStreaming",
    ),
];

/// Capabilities that name no route, in catalog order: `(capability, advertisement)`.
const EXTRAS: &[(&str, &str)] = &[
    ("ployz.machine.join.device-approval.v1", "Always"),
    ("ployz.machine.enforced.v1", "Always"),
    ("ployz.sealing.recipient-recovery.v1", "Always"),
    ("ployz.volume.copy.floor.v1", "Container"),
    ("ployz.volume.size.shrink.v1", "Container"),
    ("ployz.volume.approval.v1", "Container"),
    ("ployz.volume.copy.handoff.v1", "Container"),
    ("ployz.certificates.policy.v1", "Always"),
    ("ployz.machine.storage-observation.v1", "Always"),
    ("ployz.management.scope.v1", "Always"),
    ("ployz.machine.upgrade.exact.v1", "Always"),
    ("ployz.machine.upgrade.channel.v1", "Always"),
    ("ployz.env.sealed.v1", "Always"),
    ("ployz.build.sealed.v1", "Always"),
    ("ployz.build.grant.holder.v1", "Always"),
    ("ployz.build.grant.rule.v1", "Always"),
    ("ployz.build.grant.preview.v1", "Always"),
    ("ployz.restart.device-approval.v2", "Always"),
    ("ployz.restart.session-grant.v2", "Always"),
    ("ployz.restart.start-approval.v1", "Always"),
    ("ployz.ingress.tunnel.v1", "Cluster"),
    ("ployz.private-access.approval.v1", "Cluster"),
    ("ployz.restart.proposal.v1", "Always"),
    ("ployz.teardown.v1", "Always"),
    ("ployz.container.deploy-approval.v1", "Always"),
    ("ployz.container.deploy-proposal.v1", "Always"),
    ("ployz.container.deploy-rule.v1", "Always"),
    ("ployz.rule.passkey.v1", "Always"),
    ("ployz.volume.passkey.v1", "Always"),
    ("ployz.container.deploy-preview.v1", "Always"),
    ("ployz.preview.review.v1", "Always"),
    ("ployz.rule.commit-approval.v1", "Always"),
];

macro_rules! catalog_rows {
    (
        package $package:literal
        unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident, $unary_scope:ident),)+ }
        server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident, $stream_scope:ident),)+ }
        bidi_streaming { $($bidi_variant:ident: ($bidi_method:ident, $bidi_route:literal, $bidi_command:literal, $bidi_capability:ident, $bidi_capability_name:literal, $bidi_advertisement:ident, $bidi_scope:ident),)+ }
        capabilities { $($(#[$extra_meta:meta])* $extra_capability:ident: ($extra_capability_name:literal, $extra_advertisement:ident),)+ }
    ) => {
        (
            vec![
                $((stringify!($unary_variant), stringify!($unary_method), $unary_route, $unary_command, $unary_capability_name, stringify!($unary_advertisement), stringify!($unary_scope), "Unary"),)+
                $((stringify!($stream_variant), stringify!($stream_method), $stream_route, $stream_command, $stream_capability_name, stringify!($stream_advertisement), stringify!($stream_scope), "ServerStreaming"),)+
                $((stringify!($bidi_variant), stringify!($bidi_method), $bidi_route, $bidi_command, $bidi_capability_name, stringify!($bidi_advertisement), stringify!($bidi_scope), "BidiStreaming"),)+
            ],
            vec![$(($extra_capability_name, stringify!($extra_advertisement)),)+],
        )
    };
}

#[test]
fn every_row_names_its_route_command_and_capability_as_pinned() {
    let (rows, extras) = crate::rpc_catalog!(catalog_rows);
    let mut seen = Vec::new();
    for (variant, method, route, command, capability, advertisement, scope, shape) in rows {
        assert_eq!(route, variant, "{variant}: the route is the variant");
        assert_eq!(command, method, "{variant}: the command is the method");
        seen.push((route, command, capability, advertisement, scope, shape));
    }
    assert_eq!(seen, ROWS);
    assert_eq!(extras, EXTRAS);
}

#[test]
fn a_command_is_the_wire_tag_of_its_request_except_a_bidi_stream() {
    for (_, command, _, _, _, shape) in ROWS {
        let tag = serde_json::from_value::<RpcRequestBody>(json!({ "command": command }));
        let unknown = tag.unwrap_err().to_string().contains("unknown variant");
        assert_eq!(unknown, *shape == "BidiStreaming", "{command}");

        let header = OpaquePayload::new(
            serde_json::to_vec(&json!({ "protocol_major": PROTOCOL_MAJOR, "command": command }))
                .unwrap(),
        );
        let unsupported = matches!(
            header.decode_request(),
            Err(CodecError::UnsupportedCommand(_))
        );
        assert_eq!(unsupported, *shape == "BidiStreaming", "{command}");
    }
}

#[test]
fn the_path_scope_capability_and_fanout_of_every_route_read_from_its_row() {
    for (route, _, capability, _, scope, shape) in ROWS {
        let path = format!("/ployz.rpc.v1.MachineRpc/{route}");
        assert_eq!(capability_of(&path), Some(*capability), "{route}");
        assert_eq!(
            scope_of(&path).map(|scope| format!("{scope:?}")),
            Some((*scope).to_owned()),
            "{route}"
        );
        assert_eq!(supports_fanout(&path), *shape != "BidiStreaming", "{route}");
    }
    for path in [
        "/ployz.rpc.v1.MachineRpc/Nope",
        "/other.Service/Inspect",
        "",
    ] {
        assert_eq!(capability_of(path), None);
        assert_eq!(scope_of(path), None);
        assert!(!supports_fanout(path));
    }
}

#[test]
fn each_advertisement_lists_its_capabilities_in_catalog_order() {
    for class in [
        CapabilityAdvertisement::Always,
        CapabilityAdvertisement::Container,
        CapabilityAdvertisement::Cluster,
    ] {
        let expected: Vec<&str> = ROWS
            .iter()
            .map(|(_, _, capability, advertisement, _, _)| (*capability, *advertisement))
            .chain(EXTRAS.iter().copied())
            .filter(|(_, advertisement)| *advertisement == format!("{class:?}"))
            .map(|(capability, _)| capability)
            .collect();
        let listed: Vec<String> = class
            .capabilities()
            .map(|name| name.as_str().to_owned())
            .collect();
        assert_eq!(listed, expected, "{class:?}");
    }
}
