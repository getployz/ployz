use ployz_core::backup::{
    BackupArtifact, BackupArtifactKind, BackupArtifactLocation, BackupItem, BackupManifest,
    BackupPolicy, BackupRestoreSource, BackupTarget, BackupTargetValidationFailure,
    BackupTargetValidationField, RestoreStep, S3AddressingStyle, S3BackupRestoreSource,
    S3BackupTarget, control_plane_backup_scope, current_control_plane_bundle_scope,
    single_core_restore_contract,
};

#[test]
fn canonical_backup_scope_includes_control_plane_state() {
    assert_eq!(
        items_with_policy(BackupPolicy::Included),
        vec![
            BackupItem::CoreStateKv,
            BackupItem::OperationStateKv,
            BackupItem::ObservationStateKv,
            BackupItem::LockStateKv,
            BackupItem::BackupManifest,
            BackupItem::NatsCredentials,
            BackupItem::NatsServerConfig,
            BackupItem::PloyzDomainConfig,
            BackupItem::OperationEventStreams,
        ]
    );
}

#[test]
fn canonical_backup_scope_excludes_runtime_data() {
    assert_eq!(
        items_with_policy(BackupPolicy::Excluded),
        vec![
            BackupItem::DockerImages,
            BackupItem::ApplicationVolumes,
            BackupItem::ContainerRuntimeState,
            BackupItem::NodeLocalCache,
        ]
    );
}

#[test]
fn current_backup_bundle_scope_is_honest_about_captured_artifacts() {
    assert_eq!(
        current_bundle_items_with_policy(BackupPolicy::Included),
        vec![
            BackupItem::CoreStateKv,
            BackupItem::LockStateKv,
            BackupItem::BackupManifest,
        ]
    );
    assert_eq!(
        current_bundle_items_with_policy(BackupPolicy::Excluded),
        vec![
            BackupItem::OperationStateKv,
            BackupItem::ObservationStateKv,
            BackupItem::NatsCredentials,
            BackupItem::NatsServerConfig,
            BackupItem::PloyzDomainConfig,
            BackupItem::OperationEventStreams,
            BackupItem::DockerImages,
            BackupItem::ApplicationVolumes,
            BackupItem::ContainerRuntimeState,
            BackupItem::NodeLocalCache,
        ]
    );
    assert_eq!(
        BackupManifest::current_control_plane_kv_only().scope,
        current_control_plane_bundle_scope().collect::<Vec<_>>()
    );
}

#[test]
fn backup_target_artifact_and_restore_source_have_stable_s3_wire_shape() {
    let target = BackupTarget::s3(S3BackupTarget::new(
        "ployz-backups",
        "clusters/dev",
        "us-east-1",
        None,
        S3AddressingStyle::VirtualHosted,
    ));
    let source = BackupRestoreSource::s3(S3BackupRestoreSource::new(
        "ployz-backups",
        "clusters/dev/op_backup/manifest.json",
        "us-east-1",
        Some("https://s3.example.test".to_owned()),
        S3AddressingStyle::Path,
    ));
    let artifact = BackupArtifact::new(
        BackupArtifactLocation::s3(
            "ployz-backups",
            "clusters/dev/op_backup/control-plane-bundle.json",
            "us-east-1",
            None,
            S3AddressingStyle::VirtualHosted,
        ),
        BackupArtifactKind::ControlPlaneBundle,
        128,
        "0123456789abcdef",
    );

    assert_eq!(
        serde_json::to_string(&target).expect("target serializes"),
        r#"{"kind":"s3","bucket":"ployz-backups","key_prefix":"clusters/dev","region":"us-east-1","endpoint_url":null,"addressing_style":"virtual_hosted"}"#
    );
    assert_eq!(
        serde_json::to_string(&source).expect("restore source serializes"),
        r#"{"kind":"s3","bucket":"ployz-backups","manifest_key":"clusters/dev/op_backup/manifest.json","region":"us-east-1","endpoint_url":"https://s3.example.test","addressing_style":"path"}"#
    );
    assert_eq!(
        serde_json::to_string(&artifact).expect("artifact serializes"),
        r#"{"location":{"kind":"s3","bucket":"ployz-backups","key":"clusters/dev/op_backup/control-plane-bundle.json","region":"us-east-1","endpoint_url":null,"addressing_style":"virtual_hosted"},"kind":"control_plane_bundle","byte_count":128,"sha256_digest":"0123456789abcdef"}"#
    );

    let combined = format!(
        "{}{}{}",
        serde_json::to_string(&target).expect("target serializes"),
        serde_json::to_string(&source).expect("source serializes"),
        serde_json::to_string(&artifact).expect("artifact serializes")
    );
    assert!(!combined.contains("access_key"));
    assert!(!combined.contains("secret"));
    assert!(!combined.contains("token"));
}

#[test]
fn backup_target_rejects_empty_s3_create_preconditions() {
    let cases = [
        (
            BackupTarget::s3(S3BackupTarget::new(
                " ",
                "clusters/dev",
                "us-east-1",
                None,
                S3AddressingStyle::VirtualHosted,
            )),
            BackupTargetValidationField::Bucket,
        ),
        (
            BackupTarget::s3(S3BackupTarget::new(
                "ployz-backups",
                "\t",
                "us-east-1",
                None,
                S3AddressingStyle::VirtualHosted,
            )),
            BackupTargetValidationField::KeyPrefix,
        ),
        (
            BackupTarget::s3(S3BackupTarget::new(
                "ployz-backups",
                "clusters/dev",
                "",
                None,
                S3AddressingStyle::VirtualHosted,
            )),
            BackupTargetValidationField::Region,
        ),
        (
            BackupTarget::s3(S3BackupTarget::new(
                "ployz-backups",
                "clusters/dev",
                "us-east-1",
                Some("\n".to_owned()),
                S3AddressingStyle::VirtualHosted,
            )),
            BackupTargetValidationField::EndpointUrl,
        ),
    ];

    for (target, field) in cases {
        assert_eq!(
            target.validate_create().expect_err("target is invalid"),
            ployz_core::backup::BackupTargetValidationError {
                field,
                failure: BackupTargetValidationFailure::Empty,
            }
        );
    }
}

#[test]
fn backup_scope_has_stable_wire_shape() {
    assert_eq!(
        serde_json::to_string(&BackupItem::CoreStateKv).expect("item serializes"),
        r#""core_state_kv""#
    );
    assert_eq!(
        serde_json::from_str::<BackupPolicy>(r#""included""#).expect("policy deserializes"),
        BackupPolicy::Included
    );
}

#[test]
fn backup_scope_lists_every_item_once() {
    let all = control_plane_backup_scope()
        .map(|entry| entry.item)
        .collect::<Vec<_>>();

    assert_eq!(all, BackupItem::ALL);
    assert_eq!(
        all.len(),
        all.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    );
    for item in all {
        assert_eq!(
            item.policy(),
            control_plane_backup_scope()
                .find(|entry| entry.item == item)
                .expect("item is listed")
                .policy
        );
    }
}

#[test]
fn restore_contract_rebuilds_control_plane_then_observations() {
    assert_eq!(
        single_core_restore_contract().collect::<Vec<_>>(),
        vec![
            RestoreStep::RecreateControlPlaneAuthority,
            RestoreStep::RestoreJetStreamState,
            RestoreStep::WaitForNodeReconnects,
            RestoreStep::RebuildObservationsFromReality,
        ]
    );
}

#[test]
fn restore_contract_has_stable_wire_shape() {
    assert_eq!(
        serde_json::to_string(&RestoreStep::RebuildObservationsFromReality)
            .expect("restore step serializes"),
        r#""rebuild_observations_from_reality""#
    );
}

fn items_with_policy(policy: BackupPolicy) -> Vec<BackupItem> {
    control_plane_backup_scope()
        .filter(|entry| entry.policy == policy)
        .map(|entry| entry.item)
        .collect()
}

fn current_bundle_items_with_policy(policy: BackupPolicy) -> Vec<BackupItem> {
    current_control_plane_bundle_scope()
        .filter(|entry| entry.policy == policy)
        .map(|entry| entry.item)
        .collect()
}
