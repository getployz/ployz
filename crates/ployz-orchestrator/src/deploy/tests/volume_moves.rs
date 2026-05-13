use super::*;

#[tokio::test]
async fn resolve_plan_baseline_tracks_volume_move_changes() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut move_manifest = volume_manifest();
    move_manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-b".into(),
            },
        }],
        phases: Vec::new(),
    });
    let no_move_manifest = volume_manifest();
    let [spec] = no_move_manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    seed_volume_with_attached_services(
        &store,
        &no_move_manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    store
        .upsert_service_release(&test_release(
            &no_move_manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let no_move = resolve_plan(&store, &local_machine_id, &no_move_manifest)
        .await
        .expect("no move plan")
        .baseline();
    let with_move = resolve_plan(&store, &local_machine_id, &move_manifest)
        .await
        .expect("move plan")
        .baseline();

    assert_ne!(
        no_move.components.volume_moves,
        with_move.components.volume_moves
    );
    assert_ne!(no_move.fingerprint, with_move.fingerprint);
    assert!(
        no_move
            .changed_components(&with_move)
            .contains(&DeployBaselineComponent::VolumeMoves)
    );
}

#[tokio::test]
async fn resolve_plan_baseline_tracks_volume_clone_changes() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-b").await;
    let mut fresh_manifest = volume_manifest();
    fresh_manifest.namespace = Namespace::new("pr-39");
    let mut clone_manifest = fresh_manifest.clone();
    clone_manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace,
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
                expected_source_record_fingerprint: None,
            },
        }],
        phases: Vec::new(),
    });

    let fresh = resolve_plan(&store, &local_machine_id, &fresh_manifest)
        .await
        .expect("fresh plan")
        .baseline();
    let cloned = resolve_plan(&store, &local_machine_id, &clone_manifest)
        .await
        .expect("clone plan")
        .baseline();

    assert_ne!(
        fresh.components.volume_clones,
        cloned.components.volume_clones
    );
    assert_ne!(fresh.fingerprint, cloned.fingerprint);
    assert!(
        fresh
            .changed_components(&cloned)
            .contains(&DeployBaselineComponent::VolumeClones)
    );
}
