use ployz_core::backup::{BackupItem, BackupPolicy, control_plane_backup_scope};

#[test]
fn canonical_backup_scope_includes_control_plane_state() {
    assert_eq!(
        items_with_policy(BackupPolicy::Included),
        vec![
            BackupItem::JetStreamDataDirectory,
            BackupItem::NatsCredentials,
            BackupItem::NatsServerConfig,
            BackupItem::PloyzDomainConfig,
            BackupItem::BackupManifest,
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
fn backup_scope_has_stable_wire_shape() {
    assert_eq!(
        serde_json::to_string(&BackupItem::JetStreamDataDirectory).expect("item serializes"),
        r#""jet_stream_data_directory""#
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

fn items_with_policy(policy: BackupPolicy) -> Vec<BackupItem> {
    control_plane_backup_scope()
        .filter(|entry| entry.policy == policy)
        .map(|entry| entry.item)
        .collect()
}
