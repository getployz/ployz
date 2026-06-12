use ployz_core::backup::{
    BackupItem, BackupManifest, BackupPolicy, RestoreStep, control_plane_backup_scope,
    current_control_plane_bundle_scope, single_core_restore_contract,
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
            BackupItem::OperationStateKv,
            BackupItem::ObservationStateKv,
            BackupItem::LockStateKv,
            BackupItem::BackupManifest,
        ]
    );
    assert_eq!(
        current_bundle_items_with_policy(BackupPolicy::Excluded),
        vec![
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
