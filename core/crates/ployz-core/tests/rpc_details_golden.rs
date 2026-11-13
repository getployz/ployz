//! Frozen `RpcError.details` JSON and status bytes. Each literal is what a peer sends
//! today, so a change to how details are built or read cannot move a byte.

use std::num::NonZeroU64;

use ployz_core::{
    ContainerNotFoundReason, ContainerRefusal, ContainerRefusalReason, CopyRefusal, Departure,
    DiskFull, Evidence, ImageIngestReason, LimitsRefusal, MachineId, MachineName,
    ProvisionedVolumeMaximumBytes, PullPolicy, RecoveryRefusal, RpcError, RpcErrorCode,
    SealingRefusal, SetVolumeCopyRefused, SetVolumeSizeRefused, SizeRefusal, StorageCapacityError,
    SwitchError, VolumeGen,
};
use serde_json::{Value, json};

const MACHINE_ID: &str = "0123456789abcdef0123456789abcdef";

fn machine() -> MachineId {
    MachineId::parse(MACHINE_ID).unwrap()
}

fn bytes(n: u64) -> ProvisionedVolumeMaximumBytes {
    ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(n).unwrap())
}

fn read<E: Evidence>(details: &Value) -> Option<E> {
    RpcError::unavailable("m")
        .with_details(details.clone())
        .evidence()
}

fn wire(error: &RpcError) -> String {
    serde_json::to_string(error).unwrap()
}

#[test]
fn a_departure_mark_joins_details_as_an_object() {
    let server = MachineName::parse("lc-3").unwrap();
    let on_null = RpcError::conflict("m").overlapping_leave(&server);
    assert_eq!(
        on_null.details,
        json!({ "departing": { "server": "lc-3" } })
    );
    assert_eq!(
        wire(&on_null),
        r#"{"code":"conflict","message":"m","details":{"departing":{"server":"lc-3"}}}"#
    );
    assert_eq!(
        on_null.departure(),
        Some(Departure {
            server: server.clone()
        })
    );

    let on_object = RpcError::conflict("m")
        .with_details(json!({ "container": "b" }))
        .overlapping_leave(&server);
    assert_eq!(
        on_object.details,
        json!({ "container": "b", "departing": { "server": "lc-3" } })
    );

    for other in [json!("text"), json!(["a"]), json!(7)] {
        let unmarked = RpcError::conflict("m")
            .with_details(other.clone())
            .overlapping_leave(&server);
        assert_eq!(unmarked.details, other);
        assert_eq!(unmarked.departure(), None);
    }
    assert_eq!(
        RpcError::conflict("m")
            .with_details(json!({ "departing": { "server": 4 } }))
            .departure(),
        None
    );
}

#[test]
fn a_lost_reply_mark_keeps_the_machine_id_it_already_names() {
    let on_null = RpcError::internal("m").reply_lost_from(&machine());
    assert_eq!(
        on_null.details,
        json!({ "machine_id": MACHINE_ID, "reply_lost": true })
    );
    assert_eq!(
        wire(&on_null),
        format!(
            r#"{{"code":"internal","message":"m","details":{{"machine_id":"{MACHINE_ID}","reply_lost":true}}}}"#
        )
    );
    assert!(on_null.reply_lost());

    let named = RpcError::internal("m")
        .with_details(json!({ "machine_id": "earlier", "k": 1 }))
        .reply_lost_from(&machine());
    assert_eq!(
        named.details,
        json!({ "machine_id": "earlier", "k": 1, "reply_lost": true })
    );

    let unmarked = RpcError::internal("m")
        .with_details(json!("text"))
        .reply_lost_from(&machine());
    assert_eq!(unmarked.details, json!("text"));
    assert!(!unmarked.reply_lost());

    for not_true in [
        json!({ "reply_lost": false }),
        json!({ "reply_lost": "yes" }),
    ] {
        assert!(!RpcError::internal("m").with_details(not_true).reply_lost());
    }
    assert!(
        RpcError::internal("m")
            .with_details(json!({ "reply_lost": true }))
            .reply_lost()
    );
}

#[test]
fn an_unreached_mark_says_the_call_never_left() {
    let on_null = RpcError::unavailable("m").unreached();
    assert_eq!(on_null.details, json!({ "reached": false }));
    assert!(on_null.never_reached());

    let on_object = RpcError::unavailable("m")
        .with_details(json!({ "k": 1 }))
        .unreached();
    assert_eq!(on_object.details, json!({ "k": 1, "reached": false }));

    let unmarked = RpcError::unavailable("m")
        .with_details(json!([1]))
        .unreached();
    assert_eq!(unmarked.details, json!([1]));
    assert!(!unmarked.never_reached());

    for not_false in [json!({ "reached": true }), json!({ "reached": "no" })] {
        assert!(
            !RpcError::unavailable("m")
                .with_details(not_false)
                .never_reached()
        );
    }
}

#[test]
fn an_undialed_mark_never_creates_details() {
    let silent = RpcError::unanswered_target(RpcErrorCode::Unavailable, "m".into());
    assert_eq!(silent.details, json!({ "reason": "target_unanswered" }));
    assert!(silent.target_unanswered());
    assert!(!silent.undialed());

    let undialed = silent.undialed_target();
    assert_eq!(
        undialed.details,
        json!({ "reason": "target_unanswered", "undialed": true })
    );
    assert!(undialed.undialed());
    assert!(undialed.target_unanswered());

    let on_null = RpcError::unavailable("m").undialed_target();
    assert_eq!(on_null.details, Value::Null);
    assert!(!on_null.undialed());

    let on_text = RpcError::unavailable("m")
        .with_details(json!("text"))
        .undialed_target();
    assert_eq!(on_text.details, json!("text"));

    assert!(
        !RpcError::unavailable("m")
            .with_details(json!({ "undialed": false }))
            .undialed()
    );
}

#[test]
fn a_pull_names_its_image_only_on_details_that_say_nothing() {
    let missing =
        RpcError::not_found("no such image").pulling("ghcr.io/acme/app:1", PullPolicy::Always);
    assert_eq!(
        missing.details,
        json!({ "missing_image": "ghcr.io/acme/app:1", "pull_policy": "always" })
    );
    let named = missing.missing_image().unwrap();
    assert_eq!(
        (named.image, named.policy),
        ("ghcr.io/acme/app:1".to_owned(), Some(PullPolicy::Always))
    );

    for (policy, wire) in [
        (PullPolicy::Always, "always"),
        (PullPolicy::Missing, "missing"),
        (PullPolicy::Never, "never"),
    ] {
        assert_eq!(
            RpcError::not_found("m").pulling("i", policy).details,
            json!({ "missing_image": "i", "pull_policy": wire })
        );
    }

    let denied = RpcError::internal("pull failed")
        .with_causes(vec![
            "Error response from daemon: denied: requested access".into(),
        ])
        .pulling("i", PullPolicy::Missing);
    assert_eq!(
        denied.details,
        json!({ "missing_image": "i", "pull_policy": "missing" })
    );
    assert!(denied.missing_image().is_some());

    let plain_internal = RpcError::internal("boom").pulling("i", PullPolicy::Missing);
    assert_eq!(plain_internal.details, Value::Null);
    assert!(plain_internal.missing_image().is_none());

    let detailed = RpcError::not_found("m")
        .with_details(json!({ "k": 1 }))
        .pulling("i", PullPolicy::Missing);
    assert_eq!(detailed.details, json!({ "k": 1 }));
    assert!(detailed.missing_image().is_none());

    let no_policy = RpcError::not_found("m").with_details(json!({ "missing_image": "i" }));
    let named = no_policy.missing_image().unwrap();
    assert_eq!((named.image, named.policy), ("i".to_owned(), None));
    let wrong_policy =
        RpcError::not_found("m").with_details(json!({ "missing_image": "i", "pull_policy": 4 }));
    assert_eq!(wrong_policy.missing_image().unwrap().policy, None);
    let wrong_image = RpcError::not_found("m").with_details(json!({ "missing_image": 4 }));
    assert!(wrong_image.missing_image().is_none());
}

#[test]
fn a_full_disk_sits_under_its_own_key() {
    let full = DiskFull {
        docker_root: "/var/lib/docker".into(),
        free_bytes: 5_000_000,
    };
    assert_eq!(
        full.to_details(),
        json!({ "disk_full": { "docker_root": "/var/lib/docker", "free_bytes": 5_000_000 } })
    );
    let error = RpcError::internal("m").with_details(full.to_details());
    assert_eq!(error.disk_full(), Some(full));
    assert_eq!(
        RpcError::internal("m")
            .with_details(json!({ "disk_full": { "docker_root": 1 } }))
            .disk_full(),
        None
    );
    assert_eq!(RpcError::internal("m").disk_full(), None);
}

#[test]
fn a_size_refusal_is_the_whole_details_body() {
    let capacity = StorageCapacityError::PoolCannotGrow {
        required_bytes: 9,
        capacity_bytes: 4,
    };
    let unknown = StorageCapacityError::StorageCapacityUnknown {
        message: "no zpool".into(),
    };
    let refused = |reason, bound, capacity, floor| SetVolumeSizeRefused {
        reason,
        bound_bytes: bound,
        capacity,
        floor_bytes: floor,
    };
    for (refusal, code, details) in [
        (
            refused(SizeRefusal::NotFound, None, None, None),
            RpcErrorCode::NotFound,
            json!({ "reason": "not_found" }),
        ),
        (
            refused(SizeRefusal::NotProvisioned, None, None, None),
            RpcErrorCode::Conflict,
            json!({ "reason": "not_provisioned" }),
        ),
        (
            refused(SizeRefusal::Switching, Some(bytes(5)), None, None),
            RpcErrorCode::Conflict,
            json!({ "reason": "switching", "bound_bytes": 5 }),
        ),
        (
            refused(SizeRefusal::Shrink, Some(bytes(5)), None, None),
            RpcErrorCode::Conflict,
            json!({ "reason": "shrink", "bound_bytes": 5 }),
        ),
        (
            refused(SizeRefusal::BelowFloor, None, None, Some(bytes(22))),
            RpcErrorCode::Conflict,
            json!({ "reason": "below_floor", "floor_bytes": 22 }),
        ),
        (
            refused(SizeRefusal::NoCapacity, None, None, None),
            RpcErrorCode::Unavailable,
            json!({ "reason": "no_capacity" }),
        ),
        (
            refused(SizeRefusal::NoCapacity, None, Some(unknown), None),
            RpcErrorCode::Unavailable,
            json!({
                "reason": "no_capacity",
                "capacity": { "code": "storage_capacity_unknown", "message": "no zpool" },
            }),
        ),
        (
            refused(
                SizeRefusal::NoCapacity,
                Some(bytes(5)),
                Some(capacity),
                None,
            ),
            RpcErrorCode::Conflict,
            json!({
                "reason": "no_capacity",
                "bound_bytes": 5,
                "capacity": { "code": "pool_cannot_grow", "required_bytes": 9, "capacity_bytes": 4 },
            }),
        ),
    ] {
        let error = refusal.clone().rpc_error("m");
        assert_eq!(error.code, code, "{details}");
        assert_eq!(error.details, details);
        assert_eq!(read::<SetVolumeSizeRefused>(&error.details), Some(refusal));
    }
    assert_eq!(
        read::<SetVolumeSizeRefused>(&json!({ "reason": "elsewhere" })),
        None
    );
    assert_eq!(read::<SetVolumeSizeRefused>(&json!("not_found")), None);
}

#[test]
fn a_switch_refusal_is_the_reason_object() {
    for (refusal, code, reason) in [
        (
            SwitchError::Precondition,
            RpcErrorCode::Conflict,
            "precondition",
        ),
        (SwitchError::Busy, RpcErrorCode::Conflict, "busy"),
        (
            SwitchError::VolumeSwitching,
            RpcErrorCode::Conflict,
            "volume_switching",
        ),
        (SwitchError::NoWriter, RpcErrorCode::Conflict, "no_writer"),
        (
            SwitchError::RestorePointGone,
            RpcErrorCode::Conflict,
            "restore_point_gone",
        ),
        (
            SwitchError::NoCapacity,
            RpcErrorCode::Unavailable,
            "no_capacity",
        ),
    ] {
        let error = refusal.rpc_error("m");
        assert_eq!(error.code, code);
        assert_eq!(error.details, json!({ "reason": reason }));
        assert_eq!(read::<SwitchError>(&error.details), Some(refusal));
    }
    assert_eq!(read::<SwitchError>(&json!({ "reason": "unknown" })), None);
    assert_eq!(read::<SwitchError>(&json!("busy")), None);
    assert_eq!(read::<SwitchError>(&Value::Null), None);
}

#[test]
fn a_copy_refusal_keeps_a_null_copy_on_the_wire() {
    for (reason, code, wire) in [
        (CopyRefusal::Mismatch, RpcErrorCode::Conflict, "mismatch"),
        (
            CopyRefusal::NoTransition,
            RpcErrorCode::Conflict,
            "no_transition",
        ),
        (CopyRefusal::Busy, RpcErrorCode::Conflict, "busy"),
        (
            CopyRefusal::NoCapacity,
            RpcErrorCode::Unavailable,
            "no_capacity",
        ),
        (CopyRefusal::Behind, RpcErrorCode::Conflict, "behind"),
        (CopyRefusal::Written, RpcErrorCode::Conflict, "written"),
        (CopyRefusal::NoSource, RpcErrorCode::Conflict, "no_source"),
        (CopyRefusal::Stale, RpcErrorCode::Conflict, "stale"),
    ] {
        let refusal = SetVolumeCopyRefused {
            reason,
            copy: None,
            floor: None,
            receipt: None,
        };
        let error = refusal.clone().rpc_error("m");
        assert_eq!(error.code, code);
        assert_eq!(error.details, json!({ "reason": wire, "copy": null }));
        assert_eq!(read::<SetVolumeCopyRefused>(&error.details), Some(refusal));
    }
    let stale = SetVolumeCopyRefused {
        reason: CopyRefusal::Stale,
        copy: None,
        floor: Some(VolumeGen::new(7)),
        receipt: None,
    }
    .rpc_error("m");
    assert_eq!(
        stale.details,
        json!({ "reason": "stale", "copy": null, "floor": 7 })
    );
    assert_eq!(
        read::<SetVolumeCopyRefused>(&json!({ "reason": "stale" })),
        Some(SetVolumeCopyRefused {
            reason: CopyRefusal::Stale,
            copy: None,
            floor: None,
            receipt: None,
        }),
        "a refusal without its copy key reads as no copy"
    );
    assert_eq!(read::<SetVolumeCopyRefused>(&json!({ "copy": null })), None);
}

#[test]
fn a_storage_capacity_refusal_adds_its_shortfall_to_the_body() {
    let insufficient = StorageCapacityError::InsufficientStorage {
        required_growth_bytes: 10_000_000_000,
        available_bytes: 4_000_000_000,
        reserve_bytes: 2_000_000_000,
    }
    .into_rpc_error();
    assert_eq!(insufficient.code, RpcErrorCode::Conflict);
    assert_eq!(
        insufficient.details,
        json!({
            "code": "insufficient_storage",
            "required_growth_bytes": 10_000_000_000u64,
            "available_bytes": 4_000_000_000u64,
            "reserve_bytes": 2_000_000_000u64,
            "shortfall_bytes": 8_000_000_000u64,
            "suggestions": ["Free disk space", "Expand the disk", "Reduce requested volume sizes"],
        })
    );
    assert_eq!(
        serde_json::from_value::<StorageCapacityError>(insufficient.details).unwrap(),
        StorageCapacityError::InsufficientStorage {
            required_growth_bytes: 10_000_000_000,
            available_bytes: 4_000_000_000,
            reserve_bytes: 2_000_000_000,
        }
    );

    let pool = StorageCapacityError::PoolCannotGrow {
        required_bytes: 9,
        capacity_bytes: 4,
    }
    .into_rpc_error();
    assert_eq!(pool.code, RpcErrorCode::Conflict);
    assert_eq!(
        pool.details,
        json!({ "code": "pool_cannot_grow", "required_bytes": 9, "capacity_bytes": 4 })
    );

    let unknown = StorageCapacityError::StorageCapacityUnknown {
        message: "no zpool".into(),
    }
    .into_rpc_error();
    assert_eq!(unknown.code, RpcErrorCode::Unavailable);
    assert_eq!(
        unknown.details,
        json!({ "code": "storage_capacity_unknown", "message": "no zpool" })
    );

    let conflict = StorageCapacityError::VolumeSizeConflict {
        name: ployz_core::DockerVolumeName::parse("data").unwrap(),
    }
    .into_rpc_error();
    assert_eq!(conflict.code, RpcErrorCode::Conflict);
    assert_eq!(
        conflict.details,
        json!({ "code": "volume_size_conflict", "name": "data" })
    );
}

#[test]
fn a_container_refusal_carries_only_its_capability() {
    let refusal = |reason, capability: Option<&str>| ContainerRefusal {
        reason,
        message: "m".into(),
        capability: capability.map(str::to_owned),
    };
    let with = refusal(ContainerRefusalReason::InvalidIngress, Some("ployz.x.v1")).into_error();
    assert_eq!(with.code, RpcErrorCode::InvalidArgument);
    assert_eq!(with.details, json!({ "capability": "ployz.x.v1" }));
    assert_eq!(
        wire(&with),
        r#"{"code":"invalid_argument","message":"m","details":{"capability":"ployz.x.v1"}}"#
    );
    let without = refusal(ContainerRefusalReason::SlotOccupied, None).into_error();
    assert_eq!(without.code, RpcErrorCode::Conflict);
    assert_eq!(without.details, Value::Null);
    assert_eq!(
        wire(&without),
        r#"{"code":"conflict","message":"m","details":null}"#
    );
    for (reason, code) in [
        (ContainerRefusalReason::NotFound, RpcErrorCode::NotFound),
        (
            ContainerRefusalReason::NotParticipating,
            RpcErrorCode::Unavailable,
        ),
        (
            ContainerRefusalReason::SealingKeyMissing,
            RpcErrorCode::Unavailable,
        ),
        (
            ContainerRefusalReason::SealedValueUnopenable,
            RpcErrorCode::InvalidArgument,
        ),
        (ContainerRefusalReason::InRotation, RpcErrorCode::Conflict),
        (
            ContainerRefusalReason::ImagePullFailed,
            RpcErrorCode::Internal,
        ),
        (ContainerRefusalReason::Unrecognized, RpcErrorCode::Internal),
    ] {
        assert_eq!(refusal(reason, None).into_error().code, code);
    }
}

#[test]
fn sealing_and_recovery_refusals_sit_under_their_own_keys() {
    for (refusal, name) in [
        (SealingRefusal::Unreadable, "unreadable"),
        (SealingRefusal::Published, "published"),
        (SealingRefusal::AwaitsRecovery, "awaits_recovery"),
        (SealingRefusal::NotHeld, "not_held"),
        (SealingRefusal::HeldAnother, "held_another"),
        (SealingRefusal::UnknownMachine, "unknown_machine"),
        (
            SealingRefusal::WireGuardKeyMismatch,
            "wire_guard_key_mismatch",
        ),
        (SealingRefusal::Unopenable, "unopenable"),
        (SealingRefusal::SealedAnother, "sealed_another"),
    ] {
        assert_eq!(refusal.to_details(), json!({ "sealing": name }));
        let error = RpcError::conflict("m").with_details(refusal.to_details());
        assert_eq!(error.evidence::<SealingRefusal>(), Some(refusal));
        assert_eq!(error.evidence::<RecoveryRefusal>(), None);
    }
    for (refusal, name) in [
        (RecoveryRefusal::CloudRouted, "cloud_routed"),
        (RecoveryRefusal::NotHeld, "not_held"),
        (RecoveryRefusal::NothingAwaited, "nothing_awaited"),
        (RecoveryRefusal::NoBackup, "no_backup"),
        (RecoveryRefusal::Malformed, "malformed"),
        (RecoveryRefusal::OtherKey, "other_key"),
        (RecoveryRefusal::WrongCode, "wrong_code"),
        (RecoveryRefusal::HeldAnother, "held_another"),
    ] {
        assert_eq!(refusal.to_details(), json!({ "recovery": name }));
        let error = RpcError::conflict("m").with_details(refusal.to_details());
        assert_eq!(error.evidence::<RecoveryRefusal>(), Some(refusal));
        assert_eq!(error.evidence::<SealingRefusal>(), None);
    }
    let sealed_with_capability = RpcError::conflict("m")
        .with_details(json!({ "sealing": "published", "capability": "ployz.x.v1" }));
    assert_eq!(
        sealed_with_capability.evidence::<SealingRefusal>(),
        Some(SealingRefusal::Published)
    );
    assert_eq!(
        RpcError::conflict("m")
            .with_details(json!({ "sealing": "nope" }))
            .evidence::<SealingRefusal>(),
        None
    );
    assert_eq!(
        RpcError::conflict("m")
            .with_details(json!("sealing"))
            .evidence::<SealingRefusal>(),
        None
    );
}

#[test]
fn reason_keyed_refusals_read_the_reason_key_only() {
    for (reason, code, name) in [
        (
            ImageIngestReason::NotParticipating,
            RpcErrorCode::Unavailable,
            "not_participating",
        ),
        (
            ImageIngestReason::DockerUnavailable,
            RpcErrorCode::Unavailable,
            "docker_unavailable",
        ),
        (
            ImageIngestReason::UnsupportedContainerdStore,
            RpcErrorCode::Unsupported,
            "unsupported_containerd_store",
        ),
        (
            ImageIngestReason::ContainerdSocketMissing,
            RpcErrorCode::Unavailable,
            "containerd_socket_missing",
        ),
        (
            ImageIngestReason::StartFailed,
            RpcErrorCode::Internal,
            "start_failed",
        ),
    ] {
        let error = reason.rpc_error("m");
        assert_eq!(error.code, code);
        assert_eq!(error.details, json!({ "reason": name }));
        assert_eq!(read::<ImageIngestReason>(&error.details), Some(reason));
    }
    assert_eq!(
        read::<ImageIngestReason>(&json!({ "reason": "start_failed", "k": 1 })),
        Some(ImageIngestReason::StartFailed)
    );
    assert_eq!(read::<ImageIngestReason>(&json!("start_failed")), None);

    for (reason, name) in [
        (LimitsRefusal::OtherService, "other_service"),
        (LimitsRefusal::UpToDate, "up_to_date"),
        (LimitsRefusal::NeedsRecreate, "needs_recreate"),
        (LimitsRefusal::NotRunning, "not_running"),
        (LimitsRefusal::WouldLowerMemory, "would_lower_memory"),
        (LimitsRefusal::MemoryUnlimited, "memory_unlimited"),
    ] {
        assert_eq!(
            read::<LimitsRefusal>(&json!({ "reason": name })),
            Some(reason)
        );
    }
    assert_eq!(read::<LimitsRefusal>(&Value::Null), None);
    assert_eq!(
        read::<ContainerNotFoundReason>(&json!({ "reason": "no_resolved_spec" })),
        Some(ContainerNotFoundReason::NoResolvedSpec)
    );
    assert_eq!(
        read::<ContainerNotFoundReason>(&json!({ "reason": "else" })),
        None
    );
}

#[test]
fn unconfirmed_data_loss_is_the_whole_body_under_invalid_argument() {
    let error = ployz_core::UnconfirmedDataLoss {
        missing: vec![ployz_core::DataLoss::DockerVolume {
            id: ployz_core::DockerVolumeId {
                machine_id: machine(),
                name: ployz_core::DockerVolumeName::parse("logs").unwrap(),
            },
        }],
    }
    .into_rpc_error();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert_eq!(
        error.details,
        json!({
            "missing": [{
                "kind": "docker_volume",
                "id": { "machine_id": MACHINE_ID, "name": "logs" },
            }],
        })
    );
    assert!(ployz_core::UnconfirmedDataLoss::from_rpc_error(&error).is_some());
    let other_code = RpcError {
        code: RpcErrorCode::Conflict,
        ..error
    };
    assert!(ployz_core::UnconfirmedDataLoss::from_rpc_error(&other_code).is_none());
}

#[test]
fn the_status_codec_keeps_its_asymmetries() {
    #[derive(Debug, thiserror::Error)]
    #[error("wrapped")]
    struct Wrapped(#[source] std::io::Error);

    let bare = std::io::Error::other("disk");
    let wrapped = Wrapped(std::io::Error::other("disk"));

    // No cause and no details: no details bytes at all.
    let plain = ployz_core::caused_status(tonic::Code::Internal, &bare);
    assert!(plain.details().is_empty());
    assert_eq!(ployz_core::status_details(plain.details()), None);
    assert_eq!(ployz_core::status_causes(plain.details()), None);

    // Details present: `cause` is present even when it is empty.
    let hinted_bare = ployz_core::hinted_status(
        tonic::Code::NotFound,
        &bare,
        json!({ "next": ["ployz status"] }),
    );
    assert_eq!(
        hinted_bare.details(),
        br#"{"cause":[],"next":["ployz status"]}"#
    );
    assert_eq!(
        ployz_core::status_causes(hinted_bare.details()),
        Some(vec![])
    );
    assert_eq!(
        ployz_core::status_details(hinted_bare.details()),
        Some(json!({ "next": ["ployz status"] }))
    );
    let hinted = ployz_core::hinted_status(
        tonic::Code::NotFound,
        &wrapped,
        json!({ "capability": "ployz.x.v1", "next": ["ployz status"] }),
    );
    assert_eq!(hinted.message(), "wrapped");
    assert_eq!(
        hinted.details(),
        br#"{"capability":"ployz.x.v1","cause":["disk"],"next":["ployz status"]}"#
    );

    let caused = ployz_core::caused_status(tonic::Code::Unavailable, &wrapped);
    assert_eq!(caused.details(), br#"{"cause":["disk"]}"#);
    assert_eq!(
        ployz_core::status_details(caused.details()),
        Some(Value::Null)
    );

    // Details without a `cause` list, or not an object, are foreign.
    assert_eq!(ployz_core::status_details(br#"{"next":[]}"#), None);
    assert_eq!(ployz_core::status_details(b"[1]"), None);
    assert_eq!(ployz_core::status_causes(br#"{"cause":"disk"}"#), None);
    assert_eq!(ployz_core::status_causes(b"opaque"), None);

    // The router's markers ride in metadata and leave the body alone.
    let unanswered = ployz_core::target_unanswered_status(&wrapped);
    assert_eq!(unanswered.code(), tonic::Code::Unavailable);
    assert_eq!(unanswered.message(), "wrapped");
    assert_eq!(unanswered.details(), br#"{"cause":["disk"]}"#);
    assert_eq!(
        unanswered
            .metadata()
            .get("ployz-target-unanswered")
            .unwrap(),
        "1"
    );
    assert!(
        unanswered
            .metadata()
            .get("ployz-target-unreached")
            .is_none()
    );
    let unreached = ployz_core::target_unreached_status(&bare);
    assert!(unreached.details().is_empty());
    assert_eq!(
        unreached.metadata().get("ployz-target-unanswered").unwrap(),
        "1"
    );
    assert_eq!(
        unreached.metadata().get("ployz-target-unreached").unwrap(),
        "1"
    );

    // Large-packets marking edits the status in place.
    let dropped = ployz_core::large_packets_dropped_status(unanswered.clone(), "mtu".into());
    assert_eq!(dropped.code(), unanswered.code());
    assert_eq!(dropped.message(), unanswered.message());
    assert_eq!(
        dropped.details(),
        br#"{"cause":["disk","mtu"],"reason":"large_packets_dropped"}"#
    );
    assert_eq!(
        dropped.metadata().get("ployz-target-unanswered").unwrap(),
        "1"
    );
    let from_nothing = ployz_core::large_packets_dropped_status(
        ployz_core::target_unanswered_status(&bare),
        "mtu".into(),
    );
    assert_eq!(
        from_nothing.details(),
        br#"{"cause":["mtu"],"reason":"large_packets_dropped"}"#
    );
    assert_eq!(
        ployz_core::status_details(from_nothing.details()),
        Some(json!({ "reason": "large_packets_dropped" }))
    );
}

#[test]
fn a_missing_image_reads_only_from_object_details() {
    let not_found = |details: Value| RpcError::not_found("m").with_details(details);
    for positional in [json!(["i", "always"]), json!(["img"]), json!(["雪", null])] {
        assert_eq!(not_found(positional).missing_image(), None);
    }
    let named = not_found(json!({ "missing_image": "i", "pull_policy": "always" }));
    assert_eq!(
        named.missing_image().map(|m| (m.image, m.policy)),
        Some(("i".to_owned(), Some(PullPolicy::Always)))
    );
    let bad_policy = not_found(json!({ "missing_image": "i", "pull_policy": 4 }));
    assert_eq!(
        bad_policy.missing_image().map(|m| (m.image, m.policy)),
        Some(("i".to_owned(), None))
    );
}

#[test]
fn a_status_body_decodes_as_the_typed_cause_struct_does() {
    let causes = |body: &[u8]| ployz_core::status_causes(body);
    let details = |body: &[u8]| ployz_core::status_details(body);
    let deep = format!(
        r#"{{"cause":["a"],"x":{}{}}}"#,
        "[".repeat(200),
        "]".repeat(200)
    );
    for duplicated in [
        br#"{"cause":[],"cause":["a"]}"#.as_slice(),
        br#"{"cause":3,"cause":[]}"#,
        br#"{"cause":["a"],"cause":["b"]}"#,
    ] {
        assert_eq!(causes(duplicated), None);
        assert_eq!(details(duplicated), None);
    }
    for unparsable in [
        br#"{"cause":["a"],"x":1e400}"#.as_slice(),
        br#"{"cause":["a"],"x":"\ud800"}"#,
        deep.as_bytes(),
    ] {
        assert_eq!(causes(unparsable), Some(vec!["a".to_owned()]));
        assert_eq!(details(unparsable), None);
    }
    assert_eq!(
        causes(br#"[["a","b"]]"#),
        Some(vec!["a".to_owned(), "b".to_owned()])
    );
    assert_eq!(details(br#"[["a","b"]]"#), None);
    assert_eq!(causes(b"[[]]"), Some(vec![]));
    assert_eq!(
        causes(br#"{"cause":["a"],"next":"x"}"#),
        Some(vec!["a".to_owned()])
    );
    assert_eq!(
        details(br#"{"cause":["a"],"next":"x"}"#),
        Some(json!({ "next": "x" }))
    );
    assert_eq!(causes(b"opaque"), None);
}
