//! Frozen wire text of the Machine RPC DTOs and error model. Each literal
//! is what a peer sends today, so moving a type between modules cannot change a byte.

use ployz_core::{
    ClusterNamed, CodecError, ContainerNotFoundReason, ContainerNow, ContainerRefusal,
    ContainerRefusalReason, ContainerWant, ContractDescription, CreateVolumeRequest,
    DescribeContractRequest, DeviceEnforcementSet, DeviceKey, DeviceSet, DiskFull, DiskSpace,
    DoorChange, EnsureImageIngestRequest, EnvironmentValues, Evidence, ExposedPort, ForgetRequest,
    Forgotten, HealthVerdict, ImageIngestReason, ImagePulled, ImageRemovalOutcome, ImageSummary,
    ImagesRemoved, IngressDoors, IngressDoorsSet, IngressStage, InspectContainerRequest,
    InspectStorageRequest, JoinApproved, LeaveRequest, Left, LimitsRefusal, ListContainersRequest,
    ListImagesRequest, ListMachinesRequest, ListVolumesRequest, LiveLogTarget, LocalMachineRemoved,
    LogDirection, LogHistoryRequest, LogsOptions, MachineImages, MachineList, PeerImagePull,
    PendingJoins, PendingJoinsRequest, PrepareVolumesRequest, PreparedVolumes,
    PrivateAccessProblem, PrivateAccessSet, PruneImagesRequest, PublishCertificateMaterialRequest,
    PullImageRequest, PushTargetSet, ReadClusterNameRequest, ReadRoutedAddressesRequest,
    RemoveTrustedKeyRequest, RemoveVolumeRequest, RetainedEnvReply, RetainedEnvRequest,
    RoutedAddresses, RoutesRead, RpcError, RpcErrorCode, RuntimeWatchRequest,
    SetDeviceEnforcementRequest, SetDeviceRequest, SetManagementClientRequest,
    SetManagementClientResponse, SetPrivateAccessRequest, SetPushTargetRequest, TailLogsRequest,
    TrustedKeyRemoved, VolumeInventory, VolumeObservationFailure, VolumeRemoved,
};
use serde_json::{Value, json};

const MACHINE_ID: &str = "0123456789abcdef0123456789abcdef";
const CONTAINER_ID: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";

macro_rules! wire {
    ($($ty:ty => $literal:expr;)+) => {$(
        let literal: Value = $literal;
        let value: $ty = serde_json::from_value(literal.clone())
            .unwrap_or_else(|error| panic!("{}: {error}", stringify!($ty)));
        assert_eq!(
            serde_json::to_value(&value).unwrap(),
            literal,
            "{}",
            stringify!($ty)
        );
    )+};
}

#[test]
fn membership_and_machine_dtos_keep_their_wire_text() {
    let code = DeviceKey::from_bytes([1; 32]).code().to_string();
    wire! {
        ForgetRequest => json!({ "machine_id": MACHINE_ID });
        Forgotten => json!({ "status": "forgotten" });
        LeaveRequest => json!({});
        Left => json!({ "status": "left", "phase": "resetting" });
        PendingJoinsRequest => json!({});
        PendingJoins => json!({ "joins": [] });
        JoinApproved => json!({});
        LocalMachineRemoved => json!({
            "reset_warning": "target is unreachable; removing shared rows: gone",
            "departed": { "kind": "unreachable", "cause": "gone" },
        });
        DescribeContractRequest => json!({});
        ListMachinesRequest => json!({});
        RuntimeWatchRequest => json!({});
        MachineList => json!({ "enrollment": null, "machines": [] });
        ContractDescription => json!({
            "machine_id": MACHINE_ID,
            "protocol_major": 1,
            "daemon_version": "0.0.0",
            "capabilities": [],
        });
        ReadClusterNameRequest => json!({});
        ClusterNamed => json!({ "name": null });
        SetPushTargetRequest => json!({ "url": "https://cloud.example/observe" });
        PushTargetSet => json!({ "url": null });
        SetManagementClientRequest => json!({ "kind": "set", "label": "cloud" });
        SetManagementClientResponse => json!({ "capability": null });
        SetDeviceRequest => json!({ "kind": "remove", "code": code });
        DeviceSet => json!({ "outcome": "not_present", "code": code });
        RemoveTrustedKeyRequest => json!({ "code": code });
        TrustedKeyRemoved => json!({
            "outcome": "removed_device",
            "code": code,
            "approved_passkeys": [],
        });
        SetDeviceEnforcementRequest => json!({ "enforced": false });
        DeviceEnforcementSet => json!({ "enforced": true });
    }
}

#[test]
fn container_and_log_dtos_keep_their_wire_text() {
    wire! {
        EnvironmentValues => json!("included");
        ListContainersRequest => json!({ "environment": "redacted" });
        InspectContainerRequest => json!({ "container_id": CONTAINER_ID });
        RetainedEnvRequest => json!({ "container_id": CONTAINER_ID });
        RetainedEnvReply => json!({ "environment": { "A": "1" } });
        ContainerWant => json!({
            "state": "out",
            "container": { "id": CONTAINER_ID },
            "allow_empty": false,
            "expect": null,
        });
        ContainerWant => json!({
            "state": "gone",
            "container": { "id": CONTAINER_ID },
            "drain": 3000,
            "anonymous_volumes": "keep",
        });
        ContainerNow => json!({ "is": "absent", "of": { "id": CONTAINER_ID } });
        HealthVerdict => json!({ "verdict": "healthy" });
        HealthVerdict => json!({ "verdict": "timed_out", "deadline_millis": 5 });
        ContainerRefusal => json!({ "reason": "last_in", "message": "m" });
        ContainerRefusalReason => json!("held_by_other_run");
        LimitsRefusal => json!("needs_recreate");
        ContainerNotFoundReason => json!("no_resolved_spec");
        LogsOptions => json!({
            "follow": true,
            "tail": 10,
            "since_nanos": null,
            "until_unix_seconds": null,
        });
        LiveLogTarget => json!({ "container": CONTAINER_ID });
        TailLogsRequest => json!({
            "target": { "container": CONTAINER_ID },
            "options": {
                "follow": false,
                "tail": 1,
                "since_nanos": 7,
                "until_unix_seconds": null,
            },
        });
        LogHistoryRequest => json!({
            "namespace": "prod",
            "service": null,
            "deployment": null,
            "container_id": null,
            "since_nanos": null,
            "until_nanos": null,
            "direction": "backward",
            "limit": 10,
            "cursor": null,
        });
        LogDirection => json!("forward");
    }
}

#[test]
fn image_dtos_keep_their_wire_text() {
    wire! {
        ListImagesRequest => json!({ "reference": null, "last_tagged": false });
        EnsureImageIngestRequest => json!({});
        ImageIngestReason => json!("start_failed");
        PeerImagePull => json!({ "mode": "reference", "image": "nginx" });
        PullImageRequest => json!({ "image": "nginx", "policy": "missing" });
        ImagePulled => json!({ "exposed_ports": [{ "port": 80, "protocol": "tcp" }] });
        ExposedPort => json!({ "port": 53, "protocol": "udp" });
        PruneImagesRequest => json!({ "repositories": ["app"] });
        ImagesRemoved => json!({
            "results": [{ "reference": "app:1", "outcome": { "status": "failed", "message": "m" } }],
        });
        ImageRemovalOutcome => json!({ "status": "in_use" });
        ImageSummary => json!({
            "id": "sha256:1",
            "repo_tags": ["app:1"],
            "created": 1,
            "size": 2,
            "containers": 0,
            "platforms": ["linux/amd64"],
            "last_tagged": null,
        });
        MachineImages => json!({
            "containerd_store": false,
            "images": [],
            "docker_root": { "total_bytes": 2, "free_bytes": 1 },
        });
        DiskSpace => json!({ "total_bytes": 2, "free_bytes": 1 });
    }
}

#[test]
fn ingress_and_volume_dtos_keep_their_wire_text() {
    wire! {
        IngressDoors => json!(["public", "tunnel"]);
        DoorChange => json!("rejoined");
        IngressStage => json!("holder");
        IngressDoorsSet => json!({});
        SetPrivateAccessRequest => json!({ "enabled": false });
        PrivateAccessProblem => json!("key_refused");
        PrivateAccessSet => json!({});
        PublishCertificateMaterialRequest => json!({
            "hostname": "example.com",
            "change": { "action": "clear" },
        });
        RoutesRead => json!({ "routes": {}, "complete": false, "down": [] });
        ReadRoutedAddressesRequest => json!({});
        RoutedAddresses => json!({ "routes": null });
        CreateVolumeRequest => json!({
            "name": "data",
            "driver": "local",
            "options": {},
            "labels": {},
        });
        ListVolumesRequest => json!({});
        RemoveVolumeRequest => json!({ "name": "data", "force": true });
        VolumeInventory => json!({ "volumes": [], "failures": [] });
        VolumeRemoved => json!({});
        InspectStorageRequest => json!({});
        PrepareVolumesRequest => json!({ "specs": [] });
        PreparedVolumes => json!({ "names": ["data"] });
        VolumeObservationFailure => json!({
            "id": { "machine_id": MACHINE_ID, "name": "data" },
            "error": { "code": "unavailable", "message": "inspect failed", "details": null },
        });
    }
}

#[test]
fn certificate_material_debug_never_prints_the_private_key() {
    let change: ployz_core::CertificateMaterialChange = serde_json::from_value(json!({
        "action": "set",
        "certificate_chain_pem": "chain",
        "private_key_pem": "secret",
    }))
    .unwrap();
    assert_eq!(
        format!("{change:?}"),
        r#"Set { certificate_chain_pem: "chain", private_key_pem: "<redacted>" }"#
    );
}

#[test]
fn volume_observation_failure_says_what_to_do() {
    let failure: VolumeObservationFailure = serde_json::from_value(json!({
        "id": { "machine_id": MACHINE_ID, "name": "data" },
        "error": { "code": "unavailable", "message": "inspect failed", "details": null },
    }))
    .unwrap();
    assert_eq!(
        failure.to_string(),
        format!(
            "Docker Volume data on Machine {MACHINE_ID} could not be inspected: inspect failed; resolve the reported Docker or storage error on that Machine, then inspect the Volume again"
        )
    );
}

#[test]
fn the_error_model_keeps_its_wire_text_and_retry_table() {
    for (code, wire, retry) in [
        (RpcErrorCode::InvalidArgument, "invalid_argument", "never"),
        (RpcErrorCode::NotFound, "not_found", "never"),
        (RpcErrorCode::Ambiguous, "ambiguous", "never"),
        (RpcErrorCode::Unsupported, "unsupported", "never"),
        (RpcErrorCode::Unavailable, "unavailable", "now"),
        (RpcErrorCode::Conflict, "conflict", "never"),
        (RpcErrorCode::Internal, "internal", "never"),
        (RpcErrorCode::Unauthenticated, "unauthenticated", "never"),
        (
            RpcErrorCode::ConfirmationRequired,
            "confirmation_required",
            "never",
        ),
        (RpcErrorCode::from("forbidden"), "forbidden", "never"),
    ] {
        let error = RpcError::new(code.clone(), "m");
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            format!(r#"{{"code":"{wire}","message":"m","details":null}}"#)
        );
        assert_eq!(format!("{:?}", code.retry()).to_lowercase(), retry);
    }
    assert_eq!(ployz_core::FORBIDDEN_CODE, "forbidden");
    assert_eq!(ployz_core::APPROVAL_REQUIRED_CODE, "approval_required");

    let leave = RpcError::conflict("m")
        .with_details(json!({ "k": 1 }))
        .overlapping_leave(&ployz_core::MachineName::parse("lc-3").unwrap())
        .reply_lost_from(&ployz_core::MachineId::parse(MACHINE_ID).unwrap())
        .unreached()
        .with_causes(vec!["a".into(), "b".into()]);
    assert_eq!(
        serde_json::to_value(&leave).unwrap(),
        json!({
            "code": "conflict",
            "message": "m",
            "details": {
                "k": 1,
                "departing": { "server": "lc-3" },
                "reply_lost": true,
                "machine_id": MACHINE_ID,
                "reached": false,
            },
            "cause": ["a", "b"],
        })
    );
    assert_eq!(leave.departure().unwrap().server.as_str(), "lc-3");
    assert!(leave.reply_lost() && leave.never_reached());

    let unanswered =
        RpcError::unanswered_target(RpcErrorCode::Unavailable, "gone".into()).undialed_target();
    assert!(unanswered.target_unanswered() && unanswered.undialed());
    assert_eq!(
        unanswered.details,
        json!({ "reason": "target_unanswered", "undialed": true })
    );

    let pull = RpcError::not_found("pull failed").pulling("nginx", ployz_core::PullPolicy::Never);
    assert_eq!(
        pull.details,
        json!({ "missing_image": "nginx", "pull_policy": "never" })
    );
    assert_eq!(pull.missing_image().unwrap().image, "nginx");
    let refused = RpcError::internal("error from registry: denied")
        .pulling("private/app", ployz_core::PullPolicy::Always);
    assert_eq!(refused.missing_image().unwrap().image, "private/app");
    assert_eq!(
        RpcError::internal("lookup registry.example: no such host").unresolved_host(),
        Some("registry.example")
    );
    assert_eq!(
        RpcError::unavailable("dial: lookup registry.example: no such host")
            .unreached()
            .unresolved_host(),
        None
    );

    let full = DiskFull {
        docker_root: "/var/lib/docker".into(),
        free_bytes: 5_000_000,
    };
    assert_eq!(
        full.to_details(),
        json!({ "disk_full": { "docker_root": "/var/lib/docker", "free_bytes": 5_000_000 } })
    );
    assert_eq!(
        full.to_string(),
        "The Server's disk is full: 5 MB free under /var/lib/docker"
    );
    let disk_full = RpcError::internal("m").with_details(full.to_details());
    assert_eq!(disk_full.disk_full(), Some(full));

    let refusal = RpcError::unsupported("m")
        .with_details(json!({ "target": "lc-3", "capability": "ployz.x.v1" }))
        .refusal()
        .unwrap();
    assert_eq!(
        refusal.to_string(),
        "Server lc-3 needs a newer Ployz. It cannot serve ployz.x.v1."
    );

    assert_eq!(ployz_core::TARGET_UNANSWERED_KEY, "ployz-target-unanswered");
    assert_eq!(ployz_core::TARGET_UNREACHED_KEY, "ployz-target-unreached");
    assert_eq!(ployz_core::NO_SUCH_SERVER_REASON, "no_such_server");
    assert_eq!(ployz_core::DEPARTING_KEY, "departing");
    assert_eq!(
        ployz_core::LARGE_PACKETS_DROPPED_REASON,
        "large_packets_dropped"
    );
}

#[derive(Debug, thiserror::Error)]
#[error("wrapped")]
struct Wrapped(#[source] std::io::Error);

#[test]
fn status_helpers_keep_their_details_wire() {
    let inner = std::io::Error::other("disk");
    let outer = RpcError::caused(RpcErrorCode::Internal, &inner);
    assert_eq!((outer.message.as_str(), outer.cause.len()), ("disk", 0));
    assert_eq!(
        RpcError::internal("m").with_cause(&inner).cause,
        vec!["disk".to_owned()]
    );

    let status = ployz_core::caused_status(tonic::Code::Unavailable, &inner);
    assert_eq!(status.message(), "disk");
    assert!(status.details().is_empty());

    let wrapped = Wrapped(std::io::Error::other("disk"));
    let status = ployz_core::caused_status(tonic::Code::Unavailable, &wrapped);
    assert_eq!(status.message(), "wrapped");
    assert_eq!(status.details(), br#"{"cause":["disk"]}"#);
    assert_eq!(
        ployz_core::status_causes(status.details()),
        Some(vec!["disk".to_owned()])
    );
    assert_eq!(ployz_core::status_causes(b"opaque"), None);
    assert_eq!(
        ployz_core::status_details(status.details()),
        Some(Value::Null)
    );

    let hinted = ployz_core::hinted_status(
        tonic::Code::NotFound,
        &wrapped,
        json!({ "next": ["ployz status"] }),
    );
    assert_eq!(
        ployz_core::status_details(hinted.details()),
        Some(json!({ "next": ["ployz status"] }))
    );

    let unanswered = ployz_core::target_unanswered_status(&wrapped);
    assert_eq!(unanswered.code(), tonic::Code::Unavailable);
    assert_eq!(
        unanswered
            .metadata()
            .get(ployz_core::TARGET_UNANSWERED_KEY)
            .unwrap(),
        "1"
    );
    let unreached = ployz_core::target_unreached_status(&wrapped);
    assert_eq!(
        unreached
            .metadata()
            .get(ployz_core::TARGET_UNREACHED_KEY)
            .unwrap(),
        "1"
    );
    let dropped = ployz_core::large_packets_dropped_status(unanswered, "mtu".into());
    assert_eq!(
        ployz_core::status_details(dropped.details()),
        Some(json!({ "reason": "large_packets_dropped" }))
    );
    assert_eq!(
        ployz_core::status_causes(dropped.details()),
        Some(vec!["disk".to_owned(), "mtu".to_owned()])
    );
}

#[test]
fn codec_errors_keep_their_sentences() {
    for (error, text) in [
        (
            CodecError::UnsupportedCommand("nope".into()),
            r#"unsupported RPC command "nope""#,
        ),
        (
            CodecError::UnsupportedProtocolMajor {
                requested: 2,
                supported: 1,
            },
            "unsupported protocol major 2; this endpoint supports 1",
        ),
        (
            CodecError::UnexpectedResponse {
                expected: "left",
                actual: "forgotten".into(),
            },
            "expected response kind left, received forgotten",
        ),
        (
            CodecError::UnexpectedRequest {
                expected: "leave",
                actual: "forget".into(),
            },
            "expected request command leave, received forget",
        ),
    ] {
        assert_eq!(error.to_string(), text);
    }
}
