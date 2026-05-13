use super::*;

#[test]
fn deployable_machines_filters_by_participation() {
    let machines = vec![
        test_machine("enabled-a", MachineLifecycle::Active),
        test_machine("enabled-b", MachineLifecycle::Active),
        test_machine("draining", MachineLifecycle::Draining),
    ];

    let deployable = deployable_machines(&machines, &MachineId::new("local"));
    assert_eq!(
        deployable,
        vec![MachineId::new("enabled-a"), MachineId::new("enabled-b")]
    );
}

#[test]
fn deployable_machines_returns_empty_when_stored_machines_are_not_eligible() {
    let machines = vec![test_machine("draining", MachineLifecycle::Draining)];

    let deployable = deployable_machines(&machines, &MachineId::new("local"));
    assert!(deployable.is_empty());
}

#[test]
fn deployable_machines_falls_back_to_local_when_inventory_is_empty() {
    let deployable = deployable_machines(&[], &MachineId::new("local"));
    assert_eq!(deployable, vec![MachineId::new("local")]);
}

#[tokio::test]
async fn resolve_plan_rejects_existing_volume_quota_shrink() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = volume_manifest();
    seed_volume_with(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "2G",
        "0750",
        "999:999",
    )
    .await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("quota shrink should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeQuotaShrink {
            volume: "data".into()
        })
    );
}

#[tokio::test]
async fn resolve_plan_rejects_existing_volume_scope_mode_or_owner_changes() {
    let local_machine_id = MachineId::new("local");

    for (field, manifest, expected) in [
        (
            "scope",
            {
                let mut manifest = volume_manifest();
                let Some(volume) = manifest.volumes.first_mut() else {
                    panic!("expected volume");
                };
                volume.scope = VolumeScope::Shared;
                manifest
            },
            Error::Deploy(DeployError::VolumeScopeChange {
                volume: "data".into(),
            }),
        ),
        (
            "mode",
            {
                let mut manifest = volume_manifest();
                let Some(volume) = manifest.volumes.first_mut() else {
                    panic!("expected volume");
                };
                volume.mode = "0700".into();
                manifest
            },
            Error::Deploy(DeployError::VolumeModeChange {
                volume: "data".into(),
            }),
        ),
        (
            "owner",
            {
                let mut manifest = volume_manifest();
                let Some(volume) = manifest.volumes.first_mut() else {
                    panic!("expected volume");
                };
                volume.owner = "1000:1000".into();
                manifest
            },
            Error::Deploy(DeployError::VolumeOwnerChange {
                volume: "data".into(),
            }),
        ),
    ] {
        let store = seeded_store_with_machines(&["machine-a"]).await;
        seed_volume(&store, &manifest.namespace, "data", "machine-a").await;

        let error = match resolve_plan(&store, &local_machine_id, &manifest).await {
            Ok(_) => panic!("{field} change should fail"),
            Err(error) => error,
        };

        assert_eq!(error, expected, "{field} error should be typed");
    }
}

#[tokio::test]
async fn resolve_plan_rejects_invalid_stored_volume_quota_with_structured_error() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = volume_manifest();
    seed_volume_with(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "bogus",
        "0750",
        "999:999",
    )
    .await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("invalid stored quota should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeQuotaInvalid {
            volume: "data".into(),
            quota_kind: "current",
            message: "unsupported quota suffix in 'bogus'".into()
        })
    );
}

#[tokio::test]
async fn resolve_plan_global_service_targets_enabled_machines_in_order() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.27",
    )]);

    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-c", MachineLifecycle::Draining))
        .await
        .expect("seed machine-c");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [resolution] = plan.services() else {
        panic!("expected one service resolution");
    };
    let desired = resolution
        .slots
        .iter()
        .map(|slot| (slot.slot_id().clone(), slot.machine_id().clone()))
        .collect::<Vec<_>>();

    assert_eq!(
        desired,
        vec![
            (SlotId::new("slot-machine-a"), MachineId::new("machine-a")),
            (SlotId::new("slot-machine-b"), MachineId::new("machine-b")),
        ]
    );
}

#[tokio::test]
async fn resolve_plan_global_service_targets_home_and_compute_regions_only() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.27",
    )]);

    for machine in [
        test_machine_in_region("home", MachineLifecycle::Active, RegionRole::HomeData),
        test_machine_in_region("compute", MachineLifecycle::Active, RegionRole::Compute),
        test_machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        ),
        test_machine_in_region(
            "region-disabled",
            MachineLifecycle::Active,
            RegionRole::Disabled,
        ),
    ] {
        store
            .upsert_self_machine(&machine)
            .await
            .expect("seed machine");
    }

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [resolution] = plan.services() else {
        panic!("expected one service resolution");
    };
    let desired = resolution
        .slots
        .iter()
        .map(|slot| slot.machine_id().clone())
        .collect::<Vec<_>>();

    assert_eq!(
        desired,
        vec![MachineId::new("compute"), MachineId::new("home")]
    );
}

#[tokio::test]
async fn resolve_plan_fails_when_no_stored_machine_is_eligible_for_new_placement() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.27",
    )]);

    for machine in [
        test_machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        ),
        test_machine_in_region(
            "region-disabled",
            MachineLifecycle::Active,
            RegionRole::Disabled,
        ),
    ] {
        store
            .upsert_self_machine(&machine)
            .await
            .expect("seed machine");
    }

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("no eligible target should fail");
    assert!(matches!(
        error,
        Error::Deploy(DeployError::NoEligiblePlacementTargets)
    ));
}

#[tokio::test]
async fn resolve_plan_fails_new_volume_when_no_machine_is_eligible() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let mut service = test_service_spec("db", Placement::replicated(1), "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));

    for machine in [
        test_machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        ),
        test_machine_in_region(
            "region-disabled",
            MachineLifecycle::Active,
            RegionRole::Disabled,
        ),
    ] {
        store
            .upsert_self_machine(&machine)
            .await
            .expect("seed machine");
    }

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("new volume with no eligible target should fail");
    assert!(matches!(
        error,
        Error::Deploy(DeployError::NoEligiblePlacementTargets)
    ));
}

#[tokio::test]
async fn resolve_plan_allows_removal_only_when_no_new_placement_target_exists() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(Vec::new());

    store
        .upsert_self_machine(&test_machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        ))
        .await
        .expect("seed machine");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "old-api",
            "rev-old",
            vec![test_slot(
                "slot-0001",
                "region-draining",
                "inst-old",
                "rev-old",
            )],
        ))
        .await
        .expect("seed old-api release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("removal-only plan should not need a new placement target");

    let [removed] = plan.services() else {
        panic!("expected one removed service");
    };
    assert_eq!(removed.service, "old-api");
    assert_eq!(removed.action(), DeployChangeKind::Remove);
    let preview = plan.to_preview(Vec::new());
    assert!(preview.baseline.is_some());
    assert!(preview.service_sources.is_empty());
    assert!(preview.service_source_fingerprint.is_empty());
}

#[tokio::test]
async fn resolve_plan_includes_removed_service_participants() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let [current_spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let current_revision_hash = current_spec.revision_hash().expect("current revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Draining))
        .await
        .expect("seed machine-b");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "old-api",
            "rev-old",
            vec![test_slot("slot-0001", "machine-b", "inst-old", "rev-old")],
        ))
        .await
        .expect("seed old-api release");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &current_revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-current",
                &current_revision_hash,
            )],
        ))
        .await
        .expect("seed api release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    assert!(plan.participants().contains(&MachineId::new("machine-b")));
}

#[tokio::test]
async fn resolve_plan_fingerprint_is_stable_across_release_insert_order() {
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![
        test_service_spec("api", Placement::replicated(1), "nginx:1.27"),
        test_service_spec("worker", Placement::replicated(1), "busybox:1.0"),
    ]);
    let [api_spec, worker_spec] = manifest.services.as_slice() else {
        panic!("expected two services");
    };
    let api_revision = api_spec.revision_hash().expect("api revision");
    let worker_revision = worker_spec.revision_hash().expect("worker revision");

    let store_a = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    store_a
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &api_revision,
            vec![test_slot("slot-0001", "machine-a", "inst-a", &api_revision)],
        ))
        .await
        .expect("api release");
    store_a
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "worker",
            &worker_revision,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-b",
                &worker_revision,
            )],
        ))
        .await
        .expect("worker release");

    let store_b = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    store_b
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "worker",
            &worker_revision,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-b",
                &worker_revision,
            )],
        ))
        .await
        .expect("worker release");
    store_b
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &api_revision,
            vec![test_slot("slot-0001", "machine-a", "inst-a", &api_revision)],
        ))
        .await
        .expect("api release");

    let plan_a = resolve_plan(&store_a, &local_machine_id, &manifest)
        .await
        .expect("plan a");
    let plan_b = resolve_plan(&store_b, &local_machine_id, &manifest)
        .await
        .expect("plan b");

    assert_eq!(plan_a.fingerprint(), plan_b.fingerprint());
    assert_eq!(plan_a.baseline(), plan_b.baseline());
}

#[tokio::test]
async fn resolve_plan_includes_fresh_service_source_preview_evidence() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::replicated(1),
        "example/web:pr-39",
    )]);

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve fresh plan");
    let preview = plan.to_preview(Vec::new());

    let [source] = preview.service_sources.as_slice() else {
        panic!("expected one service source");
    };
    assert_eq!(source.service, "web");
    assert_eq!(source.mode, ServiceSourceMode::Fresh);
    assert_eq!(
        preview.service_source_fingerprint,
        plan.service_source_fingerprint()
    );
    assert_eq!(
        preview
            .baseline
            .as_ref()
            .expect("preview baseline")
            .components
            .service_sources,
        plan.service_source_fingerprint()
    );
}

#[tokio::test]
async fn participant_set_inspects_participants_in_parallel_for_noop_plan() {
    let store = seeded_store_with_machines(&[
        "machine-a",
        "machine-b",
        "machine-c",
        "machine-d",
        "machine-e",
    ])
    .await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.27",
    )]);
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &revision_hash,
            vec![
                test_slot("slot-machine-a", "machine-a", "inst-a", &revision_hash),
                test_slot("slot-machine-b", "machine-b", "inst-b", &revision_hash),
                test_slot("slot-machine-c", "machine-c", "inst-c", &revision_hash),
                test_slot("slot-machine-d", "machine-d", "inst-d", &revision_hash),
                test_slot("slot-machine-e", "machine-e", "inst-e", &revision_hash),
            ],
        ))
        .await
        .expect("seed release");

    let controller = FakeController {
        open_delay: Duration::from_millis(25),
        start_delay: Duration::from_millis(5),
        ..Default::default()
    };
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let factory = FakeParticipantClient::new(controller.clone());
    let deploy_id = DeployId::new("deploy-open");

    let (_participants, _events) =
        ParticipantSet::inspect(&factory, &plan, &local_machine_id, &deploy_id)
            .await
            .expect("inspect participants");

    assert_eq!(controller.max_open_seen(), 5);
    assert_eq!(controller.start_count(), 0);
}

#[tokio::test]
async fn ensure_plan_stable_rejects_post_lock_drift() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.27",
    )]);
    let drift_manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.28",
    )]);
    let [current_spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let current_revision = current_spec.revision_hash().expect("current revision");
    let [drift_spec] = drift_manifest.services.as_slice() else {
        panic!("expected one drift service");
    };
    let drift_revision = drift_spec.revision_hash().expect("drift revision");

    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &current_revision,
            vec![
                test_slot("slot-machine-a", "machine-a", "inst-a", &current_revision),
                test_slot("slot-machine-b", "machine-b", "inst-b", &current_revision),
            ],
        ))
        .await
        .expect("seed release");

    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &drift_revision,
            vec![
                test_slot("slot-machine-a", "machine-a", "inst-a2", &drift_revision),
                test_slot("slot-machine-b", "machine-b", "inst-b2", &drift_revision),
            ],
        ))
        .await
        .expect("drift release");
    let final_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("final plan");

    let error =
        ensure_plan_stable(&initial_plan, &final_plan, None).expect_err("plan drift should fail");
    assert_eq!(error, Error::Deploy(DeployError::ExecutionPlanChanged));
}

#[tokio::test]
async fn preview_surfaces_unreachable_participants_without_mutating_deploy_state() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(2),
        "nginx:1.27",
    )]);
    backend.reset_counts();
    let prober = FailingParticipantProbe {
        machine_id: MachineId::new("machine-b"),
    };

    let preview = preview(&store, &local_machine_id, &manifest, &prober)
        .await
        .expect("preview should surface reachability as warnings");

    assert!(preview.warnings.iter().any(|warning| {
        warning.contains("machine-b")
            && warning.contains("timeout")
            && warning.contains("injected probe timeout")
    }));
    assert_eq!(backend.deploy_status_write_count(), 0);
    assert_eq!(backend.commit_count(), 0);
}

#[tokio::test]
async fn started_candidates_rejects_missing_started_create_slot() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let prepared = PreparedDeploy::new(
        DeployId::new("deploy-1"),
        10,
        local_machine_id,
        plan,
        Vec::new(),
    )
    .expect("prepared deploy");

    let error = prepared
        .into_started(HashMap::new())
        .into_commit_plan(Vec::new(), Vec::new())
        .expect_err("missing started candidate should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::MissingStartedInstance {
            service: "api".into(),
            slot: "slot-0001".into()
        })
    );
}
