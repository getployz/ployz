use super::*;

#[tokio::test]
async fn preview_includes_present_pull_never_image_availability() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let digest = test_image_digest('a');
    store
        .upsert_image_availability(&present_image_record("machine-a", digest.clone()))
        .await
        .expect("seed image availability");
    let mut service = test_service_spec("api", Placement::replicated(1), digest.as_str());
    service.template.pull_policy = PullPolicy::Never;
    let manifest = test_manifest(vec![service]);

    let preview = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect("preview");

    let [availability] = preview.image_availability.as_slice() else {
        panic!(
            "expected one image availability check: {:?}",
            preview.image_availability
        );
    };
    assert_eq!(availability.service, "api");
    assert_eq!(availability.slot_id, SlotId::new("slot-0001"));
    assert_eq!(availability.machine_id, MachineId::new("machine-a"));
    assert_eq!(availability.digest, digest);
}

#[tokio::test]
async fn preview_skips_unchanged_pull_never_tagged_service() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut existing = test_service_spec(
        "api",
        Placement::replicated(1),
        "example/api:already-running",
    );
    existing.template.pull_policy = PullPolicy::Never;
    let revision_hash = existing.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &Namespace::new("test"),
            "api",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-existing",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");
    let manifest = test_manifest(vec![existing]);

    let preview = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect("unchanged pull-never tag should not need a new image check");

    let [service] = preview.services.as_slice() else {
        panic!("expected one service");
    };
    assert_eq!(service.action, DeployChangeKind::Unchanged);
    assert!(preview.image_availability.is_empty());
}

#[tokio::test]
async fn preview_checks_pull_never_image_availability_for_replace_slots() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let old_digest = test_image_digest('a');
    let new_digest = test_image_digest('b');
    let mut old_service = test_service_spec("api", Placement::replicated(1), old_digest.as_str());
    old_service.template.pull_policy = PullPolicy::Never;
    let old_revision_hash = old_service.revision_hash().expect("old revision hash");
    store
        .upsert_service_release(&test_release(
            &Namespace::new("test"),
            "api",
            &old_revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-existing",
                &old_revision_hash,
            )],
        ))
        .await
        .expect("seed old release");
    store
        .upsert_image_availability(&present_image_record("machine-a", new_digest.clone()))
        .await
        .expect("seed image availability");
    let mut new_service = test_service_spec("api", Placement::replicated(1), new_digest.as_str());
    new_service.template.pull_policy = PullPolicy::Never;
    let manifest = test_manifest(vec![new_service]);

    let preview = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect("replace preview");

    let [service] = preview.services.as_slice() else {
        panic!("expected one service");
    };
    assert_eq!(service.action, DeployChangeKind::Replace);
    let [availability] = preview.image_availability.as_slice() else {
        panic!(
            "expected replace slot image availability: {:?}",
            preview.image_availability
        );
    };
    assert_eq!(availability.slot_id, SlotId::new("slot-0001"));
    assert_eq!(availability.machine_id, MachineId::new("machine-a"));
    assert_eq!(availability.digest, new_digest);
}

#[tokio::test]
async fn apply_success_persists_pull_never_image_availability_summary() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let digest = test_image_digest('a');
    store
        .upsert_image_availability(&present_image_record("machine-a", digest.clone()))
        .await
        .expect("seed image availability");
    let mut service = test_service_spec("api", Placement::replicated(1), digest.as_str());
    service.template.pull_policy = PullPolicy::Never;
    let manifest = test_manifest(vec![service]);
    let deploy_id = DeployId::new("deploy-image-availability");
    let participant_client = FakeParticipantClient::new(FakeController::default());

    let result = apply_with_deploy_id_and_preconditions(
        &store,
        &participant_client,
        &local_machine_id,
        &manifest,
        deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
        DeployApplyPreconditions::default(),
    )
    .await
    .expect("apply");

    assert_eq!(result.state, DeployState::Committed);
    let [availability] = result.preview.image_availability.as_slice() else {
        panic!(
            "expected apply result image availability: {:?}",
            result.preview.image_availability
        );
    };
    assert_eq!(availability.service, "api");
    assert_eq!(availability.machine_id, MachineId::new("machine-a"));
    assert_eq!(availability.digest, digest);
    let record = store
        .get_deploy(&deploy_id)
        .await
        .expect("get deploy")
        .expect("deploy record");
    let summary: DeployPreview =
        serde_json::from_str(&record.summary_json()).expect("summary preview");
    assert_eq!(
        summary.image_availability,
        result.preview.image_availability
    );
}

#[tokio::test]
async fn preview_rejects_pull_never_without_digest_reference() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let mut service = test_service_spec("api", Placement::replicated(1), "example/api:latest");
    service.template.pull_policy = PullPolicy::Never;
    let manifest = test_manifest(vec![service]);

    let error = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect_err("mutable pull-never image should fail preview");

    assert_eq!(
        error,
        Error::Deploy(DeployError::DeployImageDigestRequired {
            service: "api".into(),
            image: "example/api:latest".into(),
        })
    );
}

#[tokio::test]
async fn preview_rejects_missing_pull_never_image_availability() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let digest = test_image_digest('a');
    let mut service = test_service_spec("api", Placement::replicated(1), digest.as_str());
    service.template.pull_policy = PullPolicy::Never;
    let manifest = test_manifest(vec![service]);

    let error = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect_err("missing image availability should fail preview");

    assert_eq!(
        error,
        Error::Deploy(DeployError::DeployImageAvailabilityMissing {
            service: "api".into(),
            slot_id: "slot-0001".into(),
            machine_id: "machine-a".into(),
            image: digest.as_str().into(),
            digest: digest.as_str().into(),
        })
    );
}

#[tokio::test]
async fn preview_rejects_non_present_pull_never_image_availability() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let digest = test_image_digest('a');
    store
        .upsert_image_availability(&absent_image_record("machine-a", digest.clone()))
        .await
        .expect("seed image availability");
    let mut service = test_service_spec("api", Placement::replicated(1), digest.as_str());
    service.template.pull_policy = PullPolicy::Never;
    let manifest = test_manifest(vec![service]);

    let error = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect_err("absent image availability should fail preview");

    assert_eq!(
        error,
        Error::Deploy(DeployError::DeployImageAvailabilityNotPresent {
            service: "api".into(),
            slot_id: "slot-0001".into(),
            machine_id: "machine-a".into(),
            image: digest.as_str().into(),
            digest: digest.as_str().into(),
            state: "absent".into(),
        })
    );
}

#[tokio::test]
async fn preview_does_not_require_availability_for_registry_pull_policies() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut if_not_present =
        test_service_spec("api", Placement::replicated(1), "example/api:latest");
    if_not_present.template.pull_policy = PullPolicy::IfNotPresent;
    let mut always = test_service_spec("worker", Placement::replicated(1), "example/worker:latest");
    always.template.pull_policy = PullPolicy::Always;
    let manifest = test_manifest(vec![if_not_present, always]);

    let preview = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect("registry pull policies should not require availability records");

    assert!(preview.image_availability.is_empty());
}

#[tokio::test]
async fn apply_rejects_missing_pull_never_image_before_participant_inspect() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let digest = test_image_digest('a');
    let mut service = test_service_spec("api", Placement::replicated(1), digest.as_str());
    service.template.pull_policy = PullPolicy::Never;
    let manifest = test_manifest(vec![service]);
    let participant = UnsupportedParticipantClient::default();

    let error = apply_with_certificate_coordination(
        &store,
        &participant,
        &local_machine_id,
        &manifest,
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect_err("missing image should fail before participant inspect");

    assert!(matches!(
        error,
        Error::Deploy(DeployError::DeployImageAvailabilityMissing { .. })
    ));
    assert_eq!(participant.inspect_count(), 0);
}

#[tokio::test]
async fn resolve_plan_rejects_manifest_phase_named_deploy() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = test_manifest(vec![
        test_service_spec("db", Placement::replicated(1), "postgres:17"),
        test_service_spec("web", Placement::replicated(1), "nginx:1.27"),
    ]);
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![DeployPhaseIntent {
            phase_id: "deploy".into(),
            name: Some("Database".into()),
            after: Vec::new(),
            services: vec!["db".into()],
            volumes: Vec::new(),
            commit_policy: DeployPhaseCommitPolicy::Checkpoint,
            rollback_policy: DeployPhaseRollbackPolicy::ForwardOnly,
            advance_policy: DeployPhaseAdvancePolicy::Immediate,
        }],
    });

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("reserved phase id should be rejected");

    assert!(
        error.to_string().contains("reserved"),
        "expected reserved phase id error, got {error}"
    );
}

#[tokio::test]
async fn resolve_plan_rejects_manual_phase_advance_policy() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![DeployPhaseIntent {
            phase_id: "web".into(),
            name: Some("Web".into()),
            after: Vec::new(),
            services: vec!["web".into()],
            volumes: Vec::new(),
            commit_policy: DeployPhaseCommitPolicy::Checkpoint,
            rollback_policy: DeployPhaseRollbackPolicy::ForwardOnly,
            advance_policy: DeployPhaseAdvancePolicy::Manual,
        }],
    });

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("manual phase advancement should be rejected before execution");

    assert!(
        error.to_string().contains("advance policy Manual"),
        "expected manual advance policy error, got {error}"
    );
}

#[test]
fn replicated_one_reuses_existing_slot_machine() {
    let spec = test_service_spec("api", Placement::replicated(1), "nginx:latest");
    let machines = vec![MachineId::new("machine-a"), MachineId::new("machine-b")];
    let current_slots = [ServiceReleaseSlot {
        slot_id: SlotId::new("slot-0001"),
        machine_id: MachineId::new("machine-b"),
        active_instance_id: InstanceId::new("inst-1"),
        revision_hash: "rev-1".into(),
    }];

    let machine_map = HashMap::from([
        (
            MachineId::new("machine-a"),
            test_machine("machine-a", MachineLifecycle::Active),
        ),
        (
            MachineId::new("machine-b"),
            test_machine("machine-b", MachineLifecycle::Active),
        ),
    ]);

    let desired = desired_slots(
        &spec,
        &machines,
        Some(&current_slots),
        &machine_map,
        None,
        "rev-1",
        false,
    )
    .expect("desired slots");
    let [slot] = desired.as_slice() else {
        panic!("expected one desired slot");
    };
    assert_eq!(slot.slot_id, SlotId::new("slot-0001"));
    assert_eq!(slot.machine_id, MachineId::new("machine-b"));
}

#[test]
fn replicated_slot_relocates_from_draining_machine_during_deploy() {
    let spec = test_service_spec("api", Placement::replicated(2), "nginx:latest");
    let machines = vec![MachineId::new("machine-a")];
    let current_slots = [ServiceReleaseSlot {
        slot_id: SlotId::new("slot-0001"),
        machine_id: MachineId::new("machine-b"),
        active_instance_id: InstanceId::new("inst-1"),
        revision_hash: "rev-1".into(),
    }];

    let machine_map = HashMap::from([
        (
            MachineId::new("machine-a"),
            test_machine("machine-a", MachineLifecycle::Active),
        ),
        (
            MachineId::new("machine-b"),
            test_machine("machine-b", MachineLifecycle::Draining),
        ),
    ]);

    let desired = desired_slots(
        &spec,
        &machines,
        Some(&current_slots),
        &machine_map,
        None,
        "rev-1",
        false,
    )
    .expect("desired slots");

    assert_eq!(desired.len(), 2);
    assert_eq!(desired[0].slot_id, SlotId::new("slot-0001"));
    assert_eq!(desired[0].machine_id, MachineId::new("machine-a"));
    assert_eq!(desired[1].slot_id, SlotId::new("slot-0002"));
    assert_eq!(desired[1].machine_id, MachineId::new("machine-a"));
}

#[tokio::test]
async fn resolve_plan_marks_matching_release_unchanged() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    assert_eq!(
        service_plan.action(),
        crate::model::DeployChangeKind::Unchanged
    );
    assert_eq!(service_plan.service, "api");
}

#[tokio::test]
async fn resolve_plan_reuses_slot_machine_when_revision_changes() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.28",
    )]);
    let old_spec = test_service_spec("api", Placement::replicated(1), "nginx:1.27");
    let old_revision_hash = old_spec.revision_hash().expect("old revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &old_revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-1",
                &old_revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(
        service_plan.action(),
        crate::model::DeployChangeKind::Replace
    );
    assert_eq!(slot_plan.machine_id(), &MachineId::new("machine-b"));
}

#[tokio::test]
async fn resolve_plan_moves_replacement_off_region_draining_machine() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.28",
    )]);
    let old_spec = test_service_spec("api", Placement::replicated(1), "nginx:1.27");
    let old_revision_hash = old_spec.revision_hash().expect("old revision hash");

    store
        .upsert_self_machine(&test_machine_in_region(
            "compute",
            MachineLifecycle::Active,
            RegionRole::Compute,
        ))
        .await
        .expect("seed compute");
    store
        .upsert_self_machine(&test_machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        ))
        .await
        .expect("seed region-draining");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &old_revision_hash,
            vec![test_slot(
                "slot-0001",
                "region-draining",
                "inst-1",
                &old_revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(slot_plan.action(), DeployChangeKind::Replace);
    assert_eq!(slot_plan.machine_id(), &MachineId::new("compute"));
}

#[tokio::test]
async fn resolve_plan_pins_new_volume_to_existing_slot_machine() {
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
    let old_spec = test_service_spec("db", Placement::replicated(1), "postgres:16");
    let old_revision_hash = old_spec.revision_hash().expect("old revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &old_revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-1",
                &old_revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [volume] = plan.volumes() else {
        panic!("expected one volume");
    };
    assert_eq!(volume.machine_id, MachineId::new("machine-b"));
    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(slot_plan.machine_id(), &MachineId::new("machine-b"));
}

#[tokio::test]
async fn resolve_plan_keeps_existing_volume_on_region_draining_machine() {
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
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");

    store
        .upsert_self_machine(&test_machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        ))
        .await
        .expect("seed machine");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "region-draining",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "region-draining",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [volume] = plan.volumes() else {
        panic!("expected one volume");
    };
    assert_eq!(volume.machine_id, MachineId::new("region-draining"));
    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(slot_plan.machine_id(), &MachineId::new("region-draining"));
}

#[tokio::test]
async fn resolve_plan_moves_volume_backed_service_from_draining_machine() {
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
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
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
            &manifest.namespace,
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

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [volume] = plan.volumes() else {
        panic!("expected one planned volume");
    };
    assert_eq!(volume.machine_id, MachineId::new("machine-b"));
    assert_eq!(
        volume
            .movement()
            .map(|movement| (&movement.from_machine, &movement.to_machine)),
        Some((&MachineId::new("machine-a"), &MachineId::new("machine-b")))
    );

    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(service_plan.action(), DeployChangeKind::Replace);
    assert_eq!(slot_plan.action(), DeployChangeKind::Replace);
    assert_eq!(slot_plan.machine_id(), &MachineId::new("machine-b"));
}

#[tokio::test]
async fn resolve_plan_moves_draining_volume_only_to_storage_capable_target() {
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
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("seed machine-a");
    let mut compute_only = test_machine("machine-b", MachineLifecycle::Active);
    compute_only.storage_role = MachineStorageRole::Compute;
    store
        .upsert_self_machine(&compute_only)
        .await
        .expect("seed machine-b");
    store
        .upsert_self_machine(&test_machine("machine-c", MachineLifecycle::Active))
        .await
        .expect("seed machine-c");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
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
            &manifest.namespace,
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

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [volume] = plan.volumes() else {
        panic!("expected one planned volume");
    };
    assert_eq!(volume.machine_id, MachineId::new("machine-c"));
    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(slot_plan.machine_id(), &MachineId::new("machine-c"));
}

#[tokio::test]
async fn resolve_plan_moves_unattached_declared_volume_from_draining_machine() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let mut manifest = test_manifest(Vec::new());
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    seed_volume(&store, &manifest.namespace, "data", "machine-a").await;

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [volume] = plan.volumes() else {
        panic!("expected one planned volume");
    };
    assert_eq!(volume.machine_id, MachineId::new("machine-b"));
    assert!(volume.attached_services.is_empty());
    assert_eq!(
        volume
            .movement()
            .map(|movement| (&movement.from_machine, &movement.to_machine)),
        Some((&MachineId::new("machine-a"), &MachineId::new("machine-b")))
    );
    let preview = plan.to_preview(Vec::new());
    let [volume_move] = preview.volume_moves.as_slice() else {
        panic!("expected one preview volume move");
    };
    assert_eq!(volume_move.volume, "data");
    assert!(volume_move.attached_services.is_empty());
}

#[tokio::test]
async fn resolve_plan_moves_draining_volume_to_existing_service_volume_pin() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let mut service = test_service_spec("db", Placement::replicated(1), "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    service.template.mounts.push(Mount {
        source: MountSource::Volume("wal".into()),
        target: "/var/lib/postgresql/wal".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest
        .volumes
        .push(test_volume("wal", VolumeScope::Single));
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_self_machine(&test_machine("machine-c", MachineLifecycle::Active))
        .await
        .expect("seed machine-c");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "wal",
        "machine-c",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
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

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let data = plan
        .volumes()
        .iter()
        .find(|volume| volume.declaration.name == "data")
        .expect("data volume");
    assert_eq!(data.machine_id, MachineId::new("machine-c"));
    assert_eq!(
        data.movement()
            .map(|movement| (&movement.from_machine, &movement.to_machine)),
        Some((&MachineId::new("machine-a"), &MachineId::new("machine-c")))
    );
    let wal = plan
        .volumes()
        .iter()
        .find(|volume| volume.declaration.name == "wal")
        .expect("wal volume");
    assert_eq!(wal.machine_id, MachineId::new("machine-c"));
    assert!(wal.movement().is_none());
    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(slot_plan.machine_id(), &MachineId::new("machine-c"));
}

#[tokio::test]
async fn resolve_plan_moves_draining_volume_to_pending_sibling_move_target() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let mut service = test_service_spec("db", Placement::replicated(1), "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    service.template.mounts.push(Mount {
        source: MountSource::Volume("wal".into()),
        target: "/var/lib/postgresql/wal".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest
        .volumes
        .push(test_volume("wal", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "wal".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-d".into(),
                to_machine: "machine-c".into(),
            },
        }],
        phases: Vec::new(),
    });
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_self_machine(&test_machine("machine-c", MachineLifecycle::Active))
        .await
        .expect("seed machine-c");
    store
        .upsert_self_machine(&test_machine("machine-d", MachineLifecycle::Active))
        .await
        .expect("seed machine-d");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "wal",
        "machine-d",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
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

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let data = plan
        .volumes()
        .iter()
        .find(|volume| volume.declaration.name == "data")
        .expect("data volume");
    assert_eq!(data.machine_id, MachineId::new("machine-c"));
    assert_eq!(
        data.movement()
            .map(|movement| (&movement.from_machine, &movement.to_machine)),
        Some((&MachineId::new("machine-a"), &MachineId::new("machine-c")))
    );
    let wal = plan
        .volumes()
        .iter()
        .find(|volume| volume.declaration.name == "wal")
        .expect("wal volume");
    assert_eq!(wal.machine_id, MachineId::new("machine-c"));
    assert_eq!(
        wal.movement()
            .map(|movement| (&movement.from_machine, &movement.to_machine)),
        Some((&MachineId::new("machine-d"), &MachineId::new("machine-c")))
    );
    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(slot_plan.machine_id(), &MachineId::new("machine-c"));
}

#[tokio::test]
async fn resolve_plan_preserves_invalid_pending_sibling_move_error() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let mut service = test_service_spec("db", Placement::replicated(1), "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    service.template.mounts.push(Mount {
        source: MountSource::Volume("wal".into()),
        target: "/var/lib/postgresql/wal".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest
        .volumes
        .push(test_volume("wal", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "wal".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-x".into(),
                to_machine: "machine-c".into(),
            },
        }],
        phases: Vec::new(),
    });

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_self_machine(&test_machine("machine-c", MachineLifecycle::Active))
        .await
        .expect("seed machine-c");
    store
        .upsert_self_machine(&test_machine("machine-d", MachineLifecycle::Active))
        .await
        .expect("seed machine-d");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "wal",
        "machine-d",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("invalid explicit sibling move should still fail");

    assert!(matches!(
        error,
        Error::Deploy(DeployError::VolumeMoveSourceMismatch {
            volume,
            expected_machine,
            actual_machine,
        }) if volume == "wal" && expected_machine == "machine-x" && actual_machine == "machine-d"
    ));
}

#[tokio::test]
async fn resolve_plan_moves_existing_volume_and_attached_service_to_target_machine() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
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
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
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
            &manifest.namespace,
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

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve move plan");

    let [volume] = plan.volumes() else {
        panic!("expected one planned volume");
    };
    assert_eq!(volume.machine_id, MachineId::new("machine-b"));
    assert_eq!(
        volume
            .movement()
            .map(|movement| (&movement.from_machine, &movement.to_machine)),
        Some((&MachineId::new("machine-a"), &MachineId::new("machine-b")))
    );
    assert!(plan.participants().contains(&MachineId::new("machine-a")));
    assert!(plan.participants().contains(&MachineId::new("machine-b")));

    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(service_plan.action(), DeployChangeKind::Replace);
    assert_eq!(slot_plan.action(), DeployChangeKind::Replace);
    assert_eq!(slot_plan.machine_id(), &MachineId::new("machine-b"));

    let preview = plan.to_preview(Vec::new());
    let [volume_move] = preview.volume_moves.as_slice() else {
        panic!("expected one preview volume move");
    };
    assert_eq!(volume_move.volume, "data");
    assert_eq!(volume_move.from_machine, MachineId::new("machine-a"));
    assert_eq!(volume_move.to_machine, MachineId::new("machine-b"));
    assert_eq!(volume_move.attached_services, vec!["db"]);
    let baseline = preview.baseline.as_ref().expect("preview baseline");
    assert_eq!(
        baseline.components.volume_moves,
        plan.baseline().components.volume_moves
    );
    let [phase] = preview.phases.as_slice() else {
        panic!("expected one default deploy phase");
    };
    assert_eq!(phase.phase_id, DeployPhaseId::new("deploy"));
    assert_eq!(phase.name, "Deploy");
    assert_eq!(phase.order, 0);
    assert_eq!(phase.commit_policy, DeployPhaseCommitPolicy::EndOfDeploy);
    assert_eq!(phase.rollback_policy, DeployPhaseRollbackPolicy::Reversible);
    assert_eq!(phase.advance_policy, DeployPhaseAdvancePolicy::Immediate);
    assert_eq!(
        phase.participants,
        vec![MachineId::new("machine-a"), MachineId::new("machine-b")]
    );
    assert!(matches!(
        phase.work.as_slice(),
        [
            DeployPhaseWork::VolumeMove {
                volume,
                from_machine,
                to_machine,
                attached_services
            },
            DeployPhaseWork::Service { service, action }
        ] if volume == "data"
            && from_machine == &MachineId::new("machine-a")
            && to_machine == &MachineId::new("machine-b")
            && attached_services.as_slice() == ["db"]
            && service == "db"
            && *action == DeployChangeKind::Replace
    ));
}

#[tokio::test]
async fn resolve_plan_treats_volume_move_to_same_machine_as_noop() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-a".into(),
                to_machine: "machine-a".into(),
            },
        }],
        phases: Vec::new(),
    });
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
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
            &manifest.namespace,
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

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve no-op move plan");

    let [volume] = plan.volumes() else {
        panic!("expected one planned volume");
    };
    assert_eq!(volume.machine_id, MachineId::new("machine-a"));
    assert_eq!(volume.movement(), None);
    assert!(plan.to_preview(Vec::new()).volume_moves.is_empty());
    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    assert_eq!(service_plan.action(), DeployChangeKind::Unchanged);
}

#[tokio::test]
async fn resolve_plan_rejects_volume_move_source_mismatch() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Move {
                from_machine: "machine-b".into(),
                to_machine: "machine-a".into(),
            },
        }],
        phases: Vec::new(),
    });
    seed_volume(&store, &manifest.namespace, "data", "machine-a").await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("source mismatch should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeMoveSourceMismatch {
            volume: "data".into(),
            expected_machine: "machine-b".into(),
            actual_machine: "machine-a".into()
        })
    );
}

#[tokio::test]
async fn resolve_plan_rejects_volume_move_to_missing_ineligible_or_compute_only_target() {
    let local_machine_id = MachineId::new("local");
    for (target, maybe_machine, expected) in [
        (
            "missing",
            None,
            Error::Deploy(DeployError::VolumeMoveTargetMissing {
                volume: "data".into(),
                machine_id: "missing".into(),
            }),
        ),
        (
            "standby",
            Some({
                let mut machine = test_machine("standby", MachineLifecycle::Standby);
                machine.storage_role = StorageParticipation::Candidate.into();
                machine
            }),
            Error::Deploy(DeployError::VolumeMoveTargetIneligible {
                volume: "data".into(),
                machine_id: "standby".into(),
            }),
        ),
        (
            "compute-only",
            Some({
                let mut machine = test_machine("compute-only", MachineLifecycle::Active);
                machine.storage_role = MachineStorageRole::Compute;
                machine
            }),
            Error::Deploy(DeployError::VolumeMoveTargetNotStorageCapable {
                volume: "data".into(),
                machine_id: "compute-only".into(),
            }),
        ),
    ] {
        let store = seeded_store_with_machines(&["machine-a"]).await;
        if let Some(machine) = maybe_machine {
            store
                .upsert_self_machine(&machine)
                .await
                .expect("seed target");
        }
        let mut manifest = volume_manifest();
        manifest.intent = Some(DeployIntent {
            services: Vec::new(),
            volumes: vec![VolumeIntentHint {
                volume: "data".into(),
                intent: VolumeIntent::Move {
                    from_machine: "machine-a".into(),
                    to_machine: target.into(),
                },
            }],
            phases: Vec::new(),
        });
        seed_volume(&store, &manifest.namespace, "data", "machine-a").await;

        let error = resolve_plan(&store, &local_machine_id, &manifest)
            .await
            .expect_err("bad target should fail");

        assert_eq!(error, expected);
    }
}

#[tokio::test]
async fn resolve_plan_rejects_volume_move_for_shared_volume() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = test_manifest(Vec::new());
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Shared));
    manifest.intent = Some(DeployIntent {
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
    seed_volume_with_scope(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        VolumeScope::Shared,
    )
    .await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("shared volume move should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeMoveRequiresSingleScope {
            volume: "data".into()
        })
    );
}

#[tokio::test]
async fn resolve_plan_rejects_volume_move_for_global_attached_service() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut service = test_service_spec("db", Placement::Global, "postgres:17");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
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
    seed_volume(&store, &manifest.namespace, "data", "machine-a").await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("global service cannot attach a moved single-scope volume");

    match error {
        Error::Deploy(DeployError::ManifestInvalid { message }) => {
            assert!(
                message.contains("cannot use global placement with managed volumes"),
                "got: {message}"
            );
        }
        other => panic!("expected manifest validation failure, got: {other:?}"),
    }
}

#[tokio::test]
async fn resolve_plan_rejects_service_with_volumes_on_different_machines() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId::new("local");
    let mut service = test_service_spec("api", Placement::replicated(1), "nginx:1.28");
    service.template.mounts.push(Mount {
        source: MountSource::Volume("left".into()),
        target: "/left".into(),
        readonly: false,
    });
    service.template.mounts.push(Mount {
        source: MountSource::Volume("right".into()),
        target: "/right".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![service]);
    manifest
        .volumes
        .push(test_volume("left", VolumeScope::Single));
    manifest
        .volumes
        .push(test_volume("right", VolumeScope::Single));

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    seed_volume(&store, &manifest.namespace, "left", "machine-a").await;
    seed_volume(&store, &manifest.namespace, "right", "machine-b").await;

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("volume machine conflict should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::ServiceVolumesOnDifferentMachines {
            service: "api".into()
        })
    );
}

#[tokio::test]
async fn apply_commits_volume_records_and_sends_volume_payload_to_startup() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = volume_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let first =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("first deploy");

    let requests = controller.start_requests().await;
    let [request] = requests.as_slice() else {
        panic!("expected one start request");
    };
    let volumes: Vec<VolumeDeclaration> =
        serde_json::from_str(&request.volumes_json).expect("volumes json");
    let [volume] = volumes.as_slice() else {
        panic!("expected one volume declaration");
    };
    assert_eq!(volume.name, "data");
    assert_eq!(volume.scope, VolumeScope::Single);
    assert_eq!(request.service, "db");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected one committed volume");
    };
    assert_eq!(record.volume_name, "data");
    assert_eq!(record.machine_id, MachineId::new("machine-a"));
    assert_eq!(record.quota, "1G");
    assert_eq!(record.mode, "0750");
    assert_eq!(record.owner, "999:999");
    assert_eq!(record.attached_services, vec!["db"]);
    assert_eq!(record.created_by_deploy_id, first.deploy_id);
    let first_created_at = record.created_at;
    let first_created_by = record.created_by_deploy_id.clone();

    let second_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("second plan");
    let second =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, second_plan)
            .await
            .expect("second deploy");
    assert_eq!(controller.start_count(), 1);

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected one committed volume after redeploy");
    };
    assert_eq!(record.created_at, first_created_at);
    assert_eq!(record.created_by_deploy_id, first_created_by);
    assert_eq!(record.last_modified_by_deploy_id, first.deploy_id);
    assert_ne!(record.last_modified_by_deploy_id, second.deploy_id);
}

#[tokio::test]
async fn preview_plans_volume_clone_on_source_machine() {
    let (store, _backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-b").await;
    let mut manifest = volume_manifest();
    manifest.namespace = Namespace::new("pr-39");
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace: source_namespace.clone(),
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
                expected_source_record_fingerprint: None,
            },
        }],
        phases: Vec::new(),
    });

    let preview = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect("clone preview");
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");

    let [clone] = preview.volume_clones.as_slice() else {
        panic!("expected one volume clone");
    };
    assert_eq!(clone.volume, "data");
    assert_eq!(clone.source_namespace, source_namespace);
    assert_eq!(clone.source_volume, "data");
    assert_eq!(clone.source_machine, MachineId::new("machine-b"));
    assert_eq!(clone.target_machine, MachineId::new("machine-b"));
    assert_eq!(clone.data_policy, VolumeCloneDataPolicy::Raw);
    assert_eq!(clone.consistency, VolumeCloneConsistency::CrashConsistent);
    let baseline = preview.baseline.as_ref().expect("preview baseline");
    assert_eq!(
        baseline.components.volume_clones,
        plan.baseline().components.volume_clones
    );
    let [phase] = preview.phases.as_slice() else {
        panic!("expected synthetic deploy phase");
    };
    assert!(matches!(
        phase.work.as_slice(),
        [
            DeployPhaseWork::VolumeClone { volume, .. },
            DeployPhaseWork::Service { service, .. },
        ] if volume == "data" && service == "db"
    ));
    let [preflight] = preview.volume_clone_preflights.as_slice() else {
        panic!("expected one volume clone preflight");
    };
    assert_eq!(&preflight.phase_id, &phase.phase_id);
    assert_eq!(preflight.volumes, vec!["data".to_string()]);
    assert_eq!(
        preflight.action,
        VolumeClonePreflightAction::DrainAndRemoveBeforeCloneReplacement
    );
    assert_eq!(
        preflight.scope,
        VolumeClonePreflightScope::UncommittedNamespaceInstances
    );
}

#[tokio::test]
async fn resolve_plan_rejects_volume_clone_source_drift() {
    let (store, _backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-b").await;
    let mut manifest = volume_manifest();
    manifest.namespace = Namespace::new("pr-39");
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace: source_namespace.clone(),
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
                expected_source_record_fingerprint: Some("older-source-record".into()),
            },
        }],
        phases: Vec::new(),
    });

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("volume clone source drift should fail");

    assert!(matches!(
        error,
        Error::Deploy(DeployError::VolumeCloneSourceChanged {
            volume,
            source_namespace,
            source_volume,
            expected_source_record_fingerprint,
            actual_source_record_fingerprint,
        }) if volume == "data"
            && source_namespace == "prod"
            && source_volume == "data"
            && expected_source_record_fingerprint == "older-source-record"
            && !actual_source_record_fingerprint.is_empty()
    ));
}

#[tokio::test]
async fn apply_executes_volume_clone_before_startup_and_commits_lineage() {
    let (store, _backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-b").await;
    let mut manifest = volume_manifest();
    manifest.namespace = Namespace::new("pr-39");
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![VolumeIntentHint {
            volume: "data".into(),
            intent: VolumeIntent::Clone {
                source_namespace: source_namespace.clone(),
                source_volume: "data".into(),
                data_policy: VolumeCloneDataPolicy::Raw,
                consistency: VolumeCloneConsistency::CrashConsistent,
                expected_source_record_fingerprint: None,
            },
        }],
        phases: Vec::new(),
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("clone deploy");

    assert_eq!(result.state, DeployState::Committed);
    let preflight_event = result
        .events
        .iter()
        .position(|event| event.step == "preflight_clone_replacement")
        .expect("clone replacement preflight event");
    assert!(
        result.events[preflight_event].message.contains("data"),
        "expected clone preflight to name cloned volume: {:?}",
        result.events[preflight_event]
    );
    let clone_event = result
        .events
        .iter()
        .position(|event| event.step == "clone_volume")
        .expect("clone event");
    assert!(
        preflight_event < clone_event,
        "expected clone replacement preflight before clone event: {:?}",
        result.events
    );
    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.start_count(), 1);
    let clone_requests = controller.clone_requests().await;
    let [clone_request] = clone_requests.as_slice() else {
        panic!("expected clone request");
    };
    assert_eq!(clone_request.volume, "data");
    assert_eq!(clone_request.source_namespace, source_namespace);
    assert_eq!(clone_request.source_volume, "data");
    assert_eq!(clone_request.quota, "1G");
    let log = controller.operation_log().await;
    let clone_index = log
        .iter()
        .position(|entry| entry.starts_with("clone:data:machine-b:prod/data"))
        .expect("clone operation logged");
    let start_index = log
        .iter()
        .position(|entry| entry.starts_with("start:db:"))
        .expect("start operation logged");
    assert!(clone_index < start_index);

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list target volumes");
    let [record] = records.as_slice() else {
        panic!("expected cloned volume record");
    };
    assert_eq!(record.machine_id, MachineId::new("machine-b"));
    assert_eq!(record.created_by_deploy_id, result.deploy_id);

    let lineage = store
        .list_volume_branches(&manifest.namespace)
        .await
        .expect("list volume branches");
    let [branch] = lineage.as_slice() else {
        panic!("expected volume branch lineage");
    };
    assert_eq!(branch.volume_name, "data");
    assert_eq!(branch.source_namespace, source_namespace);
    assert_eq!(branch.source_volume_name, "data");
    assert_eq!(branch.source_machine, MachineId::new("machine-b"));
    assert_eq!(branch.target_machine, MachineId::new("machine-b"));
    assert_eq!(branch.data_policy, VolumeCloneDataPolicy::Raw);
    assert_eq!(branch.consistency, VolumeCloneConsistency::CrashConsistent);
    assert_eq!(branch.snapshot_guid, 84);
    assert_eq!(branch.deploy_id, result.deploy_id);
    assert_eq!(branch.commit_deploy_id, result.deploy_id);
    assert_eq!(branch.phase_id, Some(DeployPhaseId::new("deploy")));

    let reapply_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("lineage-matched clone reapply plan");
    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, reapply_plan)
        .await
        .expect("lineage-matched clone reapply");
    assert_eq!(controller.clone_count(), 1);

    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Draining))
        .await
        .expect("mark clone source machine draining");
    let move_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone lineage survives inferred move plan");
    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, move_plan)
        .await
        .expect("move cloned volume");
    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.move_count(), 1);
    let moved_records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list moved clone volume");
    let [moved_record] = moved_records.as_slice() else {
        panic!("expected moved clone volume record");
    };
    assert_eq!(moved_record.machine_id, MachineId::new("machine-a"));

    let moved_reapply_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone lineage reapply after move");
    apply_with_initial_plan(
        &store,
        &factory,
        &local_machine_id,
        &manifest,
        moved_reapply_plan,
    )
    .await
    .expect("clone reapply after move");
    assert_eq!(controller.clone_count(), 1);
}

#[tokio::test]
async fn preview_rejects_volume_clone_when_target_exists() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-a").await;
    let mut manifest = volume_manifest();
    seed_volume(&store, &manifest.namespace, "data", "machine-a").await;
    manifest.intent = Some(DeployIntent {
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

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("target clone collision should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeCloneTargetExists {
            volume: "data".into()
        })
    );
}

#[tokio::test]
async fn apply_cleans_uncommitted_volume_clone_when_startup_fails() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-a").await;
    let web = test_service_spec("web", Placement::replicated(1), "nginx:1.27");
    let mut manifest = test_manifest(vec![web]);
    manifest.namespace = Namespace::new("pr-39");
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
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
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_start_service: Some("web".into()),
        ..FakeController::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("startup failure should fail deploy");

    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.clone_cleanup_count(), 1);
    assert!(
        store
            .get_volume(&manifest.namespace, "data")
            .await
            .expect("get volume")
            .is_none()
    );
}

#[tokio::test]
async fn apply_keeps_volume_clone_when_attached_service_start_returns_error() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-a").await;
    let mut manifest = volume_manifest();
    manifest.namespace = Namespace::new("pr-39");
    manifest.intent = Some(DeployIntent {
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
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_start_after_create_service: Some("db".into()),
        ..FakeController::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("attached service startup failure should fail deploy");

    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.clone_cleanup_count(), 0);
    let log = controller.operation_log().await;
    assert!(
        log.iter().any(|entry| entry.starts_with("start:db:")),
        "expected db start attempt before deploy failed: {log:?}"
    );
}

#[tokio::test]
async fn apply_keeps_started_uncheckpointed_volume_clone_when_later_phase_fails() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-a").await;

    let mut db = test_service_spec("db", Placement::replicated(1), "postgres:17");
    db.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let web = test_service_spec("web", Placement::replicated(1), "nginx:1.27");
    let mut manifest = test_manifest(vec![db, web]);
    manifest.namespace = Namespace::new("pr-39");
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
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
        phases: vec![
            DeployPhaseIntent {
                phase_id: "db".into(),
                name: Some("Database".into()),
                after: Vec::new(),
                services: vec!["db".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
            DeployPhaseIntent {
                phase_id: "web".into(),
                name: Some("Web".into()),
                after: vec!["db".into()],
                services: vec!["web".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
        ],
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_start_service: Some("web".into()),
        ..FakeController::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("later phase startup failure should fail deploy");

    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.clone_cleanup_count(), 0);
    let log = controller.operation_log().await;
    assert!(
        log.iter().any(|entry| entry.starts_with("start:db:")),
        "expected db to start before later phase failed: {log:?}"
    );
}

#[tokio::test]
async fn apply_keeps_started_volume_clone_when_same_phase_later_service_fails() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-a").await;

    let mut db = test_service_spec("db", Placement::replicated(1), "postgres:17");
    db.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let web = test_service_spec("web", Placement::replicated(1), "nginx:1.27");
    let mut manifest = test_manifest(vec![db, web]);
    manifest.namespace = Namespace::new("pr-39");
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
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
        phases: vec![DeployPhaseIntent {
            phase_id: "app".into(),
            name: Some("App".into()),
            after: Vec::new(),
            services: vec!["db".into(), "web".into()],
            volumes: Vec::new(),
            commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
            rollback_policy: DeployPhaseRollbackPolicy::Reversible,
            advance_policy: DeployPhaseAdvancePolicy::Immediate,
        }],
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_start_service: Some("web".into()),
        ..FakeController::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("same phase startup failure should fail deploy");

    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.clone_cleanup_count(), 0);
    let log = controller.operation_log().await;
    let db_start = log
        .iter()
        .position(|entry| entry.starts_with("start:db:"))
        .expect("expected db to start before web failed");
    let web_start = log
        .iter()
        .position(|entry| entry.starts_with("start:web:"))
        .expect("expected web start attempt to fail deploy");
    assert!(
        db_start < web_start,
        "expected db start to precede failing web start: {log:?}"
    );
}

#[tokio::test]
async fn apply_drains_live_uncommitted_volume_clone_writers_before_retrying_clone() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-a").await;

    let mut db = test_service_spec("db", Placement::replicated(1), "postgres:17");
    db.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let web = test_service_spec("web", Placement::replicated(1), "nginx:1.27");
    let mut manifest = test_manifest(vec![db, web]);
    manifest.namespace = Namespace::new("pr-39");
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
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
        phases: vec![DeployPhaseIntent {
            phase_id: "app".into(),
            name: Some("App".into()),
            after: Vec::new(),
            services: vec!["db".into(), "web".into()],
            volumes: Vec::new(),
            commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
            rollback_policy: DeployPhaseRollbackPolicy::Reversible,
            advance_policy: DeployPhaseAdvancePolicy::Immediate,
        }],
    });

    let first_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let first_controller = FakeController {
        fail_start_service: Some("web".into()),
        ..FakeController::default()
    };
    let first_factory = FakeParticipantClient::new(first_controller.clone());
    apply_with_initial_plan(
        &store,
        &first_factory,
        &local_machine_id,
        &manifest,
        first_plan,
    )
    .await
    .expect_err("first deploy leaves started uncommitted clone writer");
    assert_eq!(first_controller.clone_cleanup_count(), 0);

    let retry_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("retry clone plan");
    let retry_controller = FakeController::default();
    retry_controller
        .set_inspect_instances(vec![test_instance_status(
            &manifest.namespace,
            "db",
            "slot-0001",
            "machine-a",
            "inst-db-old",
            "fake-revision",
        )])
        .await;
    let retry_factory = FakeParticipantClient::new(retry_controller.clone());

    let retry_result = apply_with_initial_plan(
        &store,
        &retry_factory,
        &local_machine_id,
        &manifest,
        retry_plan,
    )
    .await
    .expect("retry should drain old writer before cloning");

    assert_eq!(retry_controller.clone_count(), 1);
    let preflight_event = retry_result
        .events
        .iter()
        .position(|event| event.step == "preflight_clone_replacement")
        .expect("clone replacement preflight event");
    assert!(
        retry_result.events[preflight_event]
            .message
            .contains("data"),
        "expected clone preflight to name cloned volume: {:?}",
        retry_result.events[preflight_event]
    );
    let stop_event = retry_result
        .events
        .iter()
        .position(|event| event.step == "stop_uncommitted_instance")
        .expect("uncommitted instance stop event");
    let clone_event = retry_result
        .events
        .iter()
        .position(|event| event.step == "clone_volume")
        .expect("clone event");
    assert!(
        preflight_event < stop_event && stop_event < clone_event,
        "expected clone replacement preflight before stale candidate stop and clone: {:?}",
        retry_result.events
    );
    assert!(retry_controller.drain_count() >= 1);
    assert!(retry_controller.remove_count() >= 1);
    let log = retry_controller.operation_log().await;
    let drain = log
        .iter()
        .position(|entry| entry == "drain:inst-db-old")
        .expect("old writer should be drained before clone");
    let remove = log
        .iter()
        .position(|entry| entry == "remove:inst-db-old")
        .expect("old writer should be removed before clone");
    let clone = log
        .iter()
        .position(|entry| entry.starts_with("clone:data:machine-a:prod/data"))
        .expect("clone should run after old writer is removed");
    assert!(
        drain < remove && remove < clone,
        "expected old writer cleanup before clone retry: {log:?}"
    );
}

#[tokio::test]
async fn apply_drains_removed_uncommitted_volume_clone_candidates_before_retrying_clone() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-a").await;
    seed_volume(&store, &source_namespace, "cache", "machine-a").await;

    let mut db = test_service_spec("db", Placement::replicated(1), "postgres:17");
    db.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let web = test_service_spec("web", Placement::replicated(1), "nginx:1.27");
    let mut manifest = test_manifest(vec![db, web]);
    manifest.namespace = Namespace::new("pr-39");
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest
        .volumes
        .push(test_volume("cache", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![
            VolumeIntentHint {
                volume: "data".into(),
                intent: VolumeIntent::Clone {
                    source_namespace: source_namespace.clone(),
                    source_volume: "data".into(),
                    data_policy: VolumeCloneDataPolicy::Raw,
                    consistency: VolumeCloneConsistency::CrashConsistent,
                    expected_source_record_fingerprint: None,
                },
            },
            VolumeIntentHint {
                volume: "cache".into(),
                intent: VolumeIntent::Clone {
                    source_namespace,
                    source_volume: "cache".into(),
                    data_policy: VolumeCloneDataPolicy::Raw,
                    consistency: VolumeCloneConsistency::CrashConsistent,
                    expected_source_record_fingerprint: None,
                },
            },
        ],
        phases: vec![DeployPhaseIntent {
            phase_id: "app".into(),
            name: Some("App".into()),
            after: Vec::new(),
            services: vec!["db".into(), "web".into()],
            volumes: Vec::new(),
            commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
            rollback_policy: DeployPhaseRollbackPolicy::Reversible,
            advance_policy: DeployPhaseAdvancePolicy::Immediate,
        }],
    });

    let first_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let first_controller = FakeController {
        fail_start_service: Some("web".into()),
        ..FakeController::default()
    };
    let first_factory = FakeParticipantClient::new(first_controller.clone());
    apply_with_initial_plan(
        &store,
        &first_factory,
        &local_machine_id,
        &manifest,
        first_plan,
    )
    .await
    .expect_err("first deploy leaves started uncommitted clone candidate");
    assert_eq!(first_controller.clone_cleanup_count(), 0);

    let mut retry_manifest = manifest.clone();
    retry_manifest
        .services
        .retain(|service| service.name.as_str() == "web");
    let Some(intent) = retry_manifest.intent.as_mut() else {
        panic!("expected clone intent");
    };
    intent.phases = vec![
        DeployPhaseIntent {
            phase_id: "data".into(),
            name: Some("Data".into()),
            after: Vec::new(),
            services: Vec::new(),
            volumes: vec!["data".into()],
            commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
            rollback_policy: DeployPhaseRollbackPolicy::Reversible,
            advance_policy: DeployPhaseAdvancePolicy::Immediate,
        },
        DeployPhaseIntent {
            phase_id: "cache".into(),
            name: Some("Cache".into()),
            after: vec!["data".into()],
            services: vec!["web".into()],
            volumes: vec!["cache".into()],
            commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
            rollback_policy: DeployPhaseRollbackPolicy::Reversible,
            advance_policy: DeployPhaseAdvancePolicy::Immediate,
        },
    ];

    let retry_plan = resolve_plan(&store, &local_machine_id, &retry_manifest)
        .await
        .expect("retry clone plan");
    let retry_controller = FakeController::default();
    retry_controller
        .set_inspect_instances(vec![test_instance_status(
            &retry_manifest.namespace,
            "db",
            "slot-0001",
            "machine-a",
            "inst-db-old",
            "fake-revision",
        )])
        .await;
    let retry_factory = FakeParticipantClient::new(retry_controller.clone());

    let retry_result = apply_with_initial_plan(
        &store,
        &retry_factory,
        &local_machine_id,
        &retry_manifest,
        retry_plan,
    )
    .await
    .expect("retry should drain removed stale candidate before cloning");

    assert_eq!(retry_controller.clone_count(), 2);
    let preflight_messages = retry_result
        .events
        .iter()
        .filter(|event| event.step == "preflight_clone_replacement")
        .map(|event| event.message.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        preflight_messages.len(),
        2,
        "expected one clone replacement preflight per clone phase: {:?}",
        retry_result.events
    );
    assert!(
        preflight_messages
            .iter()
            .any(|message| message.contains("data")),
        "expected a clone preflight event to name data: {preflight_messages:?}"
    );
    assert!(
        preflight_messages
            .iter()
            .any(|message| message.contains("cache")),
        "expected a clone preflight event to name cache: {preflight_messages:?}"
    );
    let log = retry_controller.operation_log().await;
    assert_eq!(
        log.iter()
            .filter(|entry| entry.as_str() == "drain:inst-db-old")
            .count(),
        1,
        "stale candidate should only be drained once: {log:?}"
    );
    assert_eq!(
        log.iter()
            .filter(|entry| entry.as_str() == "remove:inst-db-old")
            .count(),
        1,
        "stale candidate should only be removed once: {log:?}"
    );
    let drain = log
        .iter()
        .position(|entry| entry == "drain:inst-db-old")
        .expect("removed stale candidate should be drained before clone");
    let remove = log
        .iter()
        .position(|entry| entry == "remove:inst-db-old")
        .expect("removed stale candidate should be removed before clone");
    let data_clone = log
        .iter()
        .position(|entry| entry.starts_with("clone:data:machine-a:prod/data"))
        .expect("data clone should run after stale candidate is removed");
    let cache_clone = log
        .iter()
        .position(|entry| entry.starts_with("clone:cache:machine-a:prod/cache"))
        .expect("cache clone should run after stale candidate is removed");
    assert!(
        drain < remove && remove < data_clone && remove < cache_clone,
        "expected removed stale candidate cleanup before clone retry: {log:?}"
    );
}

#[tokio::test]
async fn apply_does_not_drain_committed_service_before_creating_new_clone() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-a").await;

    let mut db = test_service_spec("db", Placement::replicated(1), "postgres:17");
    db.template.mounts.push(Mount {
        source: MountSource::Volume("data".into()),
        target: "/var/lib/postgresql/data".into(),
        readonly: false,
    });
    let mut manifest = test_manifest(vec![db]);
    manifest.namespace = Namespace::new("pr-39");
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
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
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "db",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-db-committed",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_clone_volume: Some("data".into()),
        ..FakeController::default()
    };
    controller
        .set_inspect_instances(vec![test_instance_status(
            &manifest.namespace,
            "db",
            "slot-0001",
            "machine-a",
            "inst-db-committed",
            &revision_hash,
        )])
        .await;
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("clone failure should fail deploy");

    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.drain_count(), 0);
    assert_eq!(controller.remove_count(), 0);
}

#[tokio::test]
async fn apply_surfaces_uncommitted_volume_clone_cleanup_failures() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-a").await;
    let web = test_service_spec("web", Placement::replicated(1), "nginx:1.27");
    let mut manifest = test_manifest(vec![web]);
    manifest.namespace = Namespace::new("pr-39");
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
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
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_start_service: Some("web".into()),
        fail_cleanup_clone_volume: Some("data".into()),
        ..FakeController::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("startup failure should fail deploy");

    assert_eq!(controller.clone_count(), 1);
    assert_eq!(controller.clone_cleanup_count(), 1);
    assert!(
        error
            .to_string()
            .contains("uncommitted volume clone cleanup failed"),
        "got: {error}"
    );
}

#[tokio::test]
async fn apply_cleans_successful_volume_clones_when_later_clone_fails() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_volume(&store, &source_namespace, "data", "machine-a").await;
    seed_volume(&store, &source_namespace, "cache", "machine-a").await;
    let mut manifest = volume_manifest();
    manifest.namespace = Namespace::new("pr-39");
    manifest
        .volumes
        .push(test_volume("cache", VolumeScope::Single));
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: vec![
            VolumeIntentHint {
                volume: "data".into(),
                intent: VolumeIntent::Clone {
                    source_namespace: source_namespace.clone(),
                    source_volume: "data".into(),
                    data_policy: VolumeCloneDataPolicy::Raw,
                    consistency: VolumeCloneConsistency::CrashConsistent,
                    expected_source_record_fingerprint: None,
                },
            },
            VolumeIntentHint {
                volume: "cache".into(),
                intent: VolumeIntent::Clone {
                    source_namespace,
                    source_volume: "cache".into(),
                    data_policy: VolumeCloneDataPolicy::Raw,
                    consistency: VolumeCloneConsistency::CrashConsistent,
                    expected_source_record_fingerprint: None,
                },
            },
        ],
        phases: Vec::new(),
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("clone plan");
    let controller = FakeController {
        fail_clone_volume: Some("cache".into()),
        ..FakeController::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("second clone failure should fail deploy");

    assert_eq!(controller.clone_count(), 2);
    assert_eq!(controller.clone_cleanup_count(), 2);
    assert_eq!(controller.start_count(), 0);
    assert!(
        store
            .list_volume_branches(&manifest.namespace)
            .await
            .expect("list branches")
            .is_empty()
    );
}

#[tokio::test]
async fn apply_restarts_attached_service_before_committing_volume_quota_change() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = volume_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let first =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("first deploy");

    let mut quota_manifest = volume_manifest();
    let Some(volume) = quota_manifest.volumes.first_mut() else {
        panic!("expected volume");
    };
    volume.quota = "2G".into();
    let quota_plan = resolve_plan(&store, &local_machine_id, &quota_manifest)
        .await
        .expect("quota plan");
    let [service] = quota_plan.services() else {
        panic!("expected one planned service");
    };
    assert_eq!(service.action(), crate::model::DeployChangeKind::Replace);

    let second = apply_with_initial_plan(
        &store,
        &factory,
        &local_machine_id,
        &quota_manifest,
        quota_plan,
    )
    .await
    .expect("quota deploy");

    assert_eq!(controller.start_count(), 2);
    let requests = controller.start_requests().await;
    let [_, quota_request] = requests.as_slice() else {
        panic!("expected two start requests");
    };
    let volumes: Vec<VolumeDeclaration> =
        serde_json::from_str(&quota_request.volumes_json).expect("volumes json");
    let [volume] = volumes.as_slice() else {
        panic!("expected one volume declaration");
    };
    assert_eq!(volume.quota, "2G");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected one committed volume");
    };
    assert_eq!(record.quota, "2G");
    assert_eq!(record.created_by_deploy_id, first.deploy_id);
    assert_eq!(record.last_modified_by_deploy_id, second.deploy_id);
}

#[tokio::test]
async fn apply_executes_volume_move_before_startup_and_commits_target_owner() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
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
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
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
    backend.reset_counts();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("move plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("move deploy");

    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(backend.commit_count(), 1);
    assert_eq!(controller.drain_count(), 1);
    assert_eq!(controller.remove_count(), 1);
    assert_eq!(controller.move_count(), 1);
    assert_eq!(controller.start_count(), 1);
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected moved volume record");
    };
    assert_eq!(record.machine_id, MachineId::new("machine-b"));
    let movements = store
        .list_volume_movements(&manifest.namespace)
        .await
        .expect("list volume movements");
    let [movement] = movements.as_slice() else {
        panic!("expected movement evidence");
    };
    assert_eq!(movement.volume_name, "data");
    assert_eq!(movement.from_machine, MachineId::new("machine-a"));
    assert_eq!(movement.to_machine, MachineId::new("machine-b"));
    assert_eq!(movement.final_machine, MachineId::new("machine-b"));
    assert_eq!(movement.deploy_id, result.deploy_id);
    assert_eq!(movement.commit_deploy_id, result.deploy_id);
    assert_eq!(movement.phase_id, Some(DeployPhaseId::new("deploy")));
    assert_eq!(movement.snapshot_guid, 42);
    assert_eq!(movement.bytes_transferred, 4096);
    let log = controller.operation_log().await;
    let drain_index = log
        .iter()
        .position(|entry| entry.starts_with("drain:"))
        .expect("drain operation logged");
    let remove_index = log
        .iter()
        .position(|entry| entry.starts_with("remove:"))
        .expect("remove operation logged");
    let move_index = log
        .iter()
        .position(|entry| entry.starts_with("move:data:"))
        .expect("move operation logged");
    let start_index = log
        .iter()
        .position(|entry| entry.starts_with("start:db:"))
        .expect("start operation logged");
    assert!(drain_index < remove_index);
    assert!(remove_index < move_index);
    assert!(move_index < start_index);
}

#[tokio::test]
async fn apply_executes_inferred_draining_volume_move_before_startup() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Draining))
        .await
        .expect("mark machine-a draining");
    let local_machine_id = MachineId::new("local");
    let manifest = volume_manifest();
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
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
    backend.reset_counts();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("inferred move plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("inferred move deploy");

    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(backend.commit_count(), 1);
    assert_eq!(controller.drain_count(), 1);
    assert_eq!(controller.remove_count(), 1);
    assert_eq!(controller.move_count(), 1);
    assert_eq!(controller.start_count(), 1);
    let move_requests = controller.move_requests().await;
    let [move_request] = move_requests.as_slice() else {
        panic!("expected one move request");
    };
    assert_eq!(move_request.from_machine, MachineId::new("machine-a"));
    assert_eq!(move_request.to_machine, MachineId::new("machine-b"));
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected moved volume record");
    };
    assert_eq!(record.machine_id, MachineId::new("machine-b"));
}

#[tokio::test]
async fn apply_stops_stale_live_volume_writers_before_move() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
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
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    backend.reset_counts();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("move plan");
    let controller = FakeController::default();
    controller
        .set_inspect_instances(vec![test_instance_status(
            &manifest.namespace,
            "db",
            "stale-slot",
            "machine-a",
            "stale-inst",
            &revision_hash,
        )])
        .await;
    let factory = FakeParticipantClient::new(controller.clone());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("move deploy");

    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(backend.commit_count(), 1);
    assert_eq!(controller.drain_count(), 1);
    assert_eq!(controller.remove_count(), 1);
    assert_eq!(controller.move_count(), 1);
    assert_eq!(controller.start_count(), 1);
    let log = controller.operation_log().await;
    let drain_index = log
        .iter()
        .position(|entry| entry == "drain:stale-inst")
        .expect("stale drain operation logged");
    let remove_index = log
        .iter()
        .position(|entry| entry == "remove:stale-inst")
        .expect("stale remove operation logged");
    let move_index = log
        .iter()
        .position(|entry| entry.starts_with("move:data:"))
        .expect("move operation logged");
    assert!(drain_index < remove_index);
    assert!(remove_index < move_index);
}

#[tokio::test]
async fn apply_fails_volume_move_before_startup_or_commit() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
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
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
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
    backend.reset_counts();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("move plan");
    let controller = FakeController {
        fail_move_volume: Some("data".into()),
        ..Default::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("move failure should fail deploy");

    assert_eq!(
        error,
        Error::Operation {
            operation: "fake_move_volume",
            message: "injected move failure for 'data'".into(),
        }
    );
    assert_eq!(backend.commit_count(), 0);
    assert_eq!(backend.deploy_status_write_count(), 2);
    assert_eq!(controller.drain_count(), 1);
    assert_eq!(controller.remove_count(), 1);
    assert_eq!(controller.move_count(), 1);
    assert_eq!(controller.start_count(), 0);
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected source volume record");
    };
    assert_eq!(record.machine_id, MachineId::new("machine-a"));
    assert!(
        store
            .list_volume_movements(&manifest.namespace)
            .await
            .expect("list volume movements")
            .is_empty()
    );
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state(), DeployState::Failed);
    assert!(
        last_update
            .summary_json()
            .contains("injected move failure for 'data'"),
        "failed deploy summary should mention the move error: {}",
        last_update.summary_json()
    );
    assert_default_phase_record(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "failed",
        Some("injected move failure for 'data'"),
    )
    .await;
}

#[tokio::test]
async fn apply_reuses_volume_move_snapshot_when_retrying_after_startup_failure() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
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
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
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
    backend.reset_counts();

    let controller = FakeController {
        fail_start_service: Some("db".into()),
        ..Default::default()
    };
    let first_client = FakeParticipantClient::new(controller.clone());
    let first_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("first plan");

    apply_with_initial_plan(
        &store,
        &first_client,
        &local_machine_id,
        &manifest,
        first_plan,
    )
    .await
    .expect_err("startup failure should fail first deploy");

    assert_eq!(backend.commit_count(), 0);
    assert_eq!(controller.move_count(), 1);
    assert_eq!(controller.start_count(), 1);
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected source volume record");
    };
    assert_eq!(record.machine_id, MachineId::new("machine-a"));

    let retry_controller = FakeController::default();
    let retry_client = FakeParticipantClient::new(retry_controller.clone());
    let retry_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("retry plan");

    let result = apply_with_initial_plan(
        &store,
        &retry_client,
        &local_machine_id,
        &manifest,
        retry_plan,
    )
    .await
    .expect("retry deploy");

    assert_eq!(result.state, DeployState::Committed);
    let first_requests = controller.move_requests().await;
    let retry_requests = retry_controller.move_requests().await;
    let [first_request] = first_requests.as_slice() else {
        panic!("expected one first move");
    };
    let [retry_request] = retry_requests.as_slice() else {
        panic!("expected one retry move");
    };
    assert_eq!(first_request.snapshot, retry_request.snapshot);
}

#[tokio::test]
async fn apply_stops_current_volume_writers_even_when_service_is_removed() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = volume_manifest();
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    manifest.services.clear();
    manifest.intent = Some(DeployIntent {
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
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
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
            &manifest.namespace,
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
    backend.reset_counts();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("move plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("move deploy");

    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(backend.commit_count(), 1);
    assert_eq!(controller.drain_count(), 1);
    assert_eq!(controller.remove_count(), 1);
    assert_eq!(controller.move_count(), 1);
    assert_eq!(controller.start_count(), 0);
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected moved volume record");
    };
    assert_eq!(record.machine_id, MachineId::new("machine-b"));
    let log = controller.operation_log().await;
    let drain_index = log
        .iter()
        .position(|entry| entry.starts_with("drain:"))
        .expect("drain operation logged");
    let remove_index = log
        .iter()
        .position(|entry| entry.starts_with("remove:"))
        .expect("remove operation logged");
    let move_index = log
        .iter()
        .position(|entry| entry.starts_with("move:data:"))
        .expect("move operation logged");
    assert!(drain_index < remove_index);
    assert!(remove_index < move_index);
}

#[tokio::test]
async fn apply_does_not_mark_committed_volume_move_failed_after_post_commit_status_error() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
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
    seed_volume_with_attached_services(
        &store,
        &manifest.namespace,
        "data",
        "machine-a",
        "1G",
        "0750",
        "999:999",
        vec!["db".into()],
    )
    .await;
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
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
    backend.reset_counts();
    backend.fail_committed_status_writes_after_first(true);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("move plan");
    let factory = FakeParticipantClient::new(FakeController::default());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect_err("post-commit status write should fail apply response");

    assert_eq!(backend.commit_count(), 1);
    assert_eq!(backend.deploy_status_write_count(), 3);
    let writes = backend.deploy_status_writes().await;
    assert_eq!(
        writes
            .iter()
            .filter(|record| record.state() == DeployState::Failed)
            .count(),
        0
    );
    let post_commit_attempt = writes
        .last()
        .expect("post-commit status write should have been attempted");
    assert_eq!(post_commit_attempt.state(), DeployState::CleanupPending);
    let committed_record = store
        .get_deploy(&post_commit_attempt.deploy_id)
        .await
        .expect("get deploy")
        .expect("post-commit deploy record");
    assert_eq!(committed_record.state(), DeployState::CleanupPending);
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected moved volume record");
    };
    assert_eq!(record.machine_id, MachineId::new("machine-b"));
    let movements = store
        .list_volume_movements(&manifest.namespace)
        .await
        .expect("list volume movements");
    assert_eq!(movements.len(), 1);
}

#[tokio::test]
async fn apply_rejects_volume_move_when_target_loses_eligibility_before_mutation() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
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
    seed_volume(&store, &manifest.namespace, "data", "machine-a").await;
    backend.reset_counts();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial move plan");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Standby))
        .await
        .expect("target becomes standby");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("target eligibility drift should fail before mutation");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeMoveTargetIneligible {
            volume: "data".into(),
            machine_id: "machine-b".into()
        })
    );
    assert_eq!(backend.commit_count(), 0);
    assert_eq!(backend.deploy_status_write_count(), 0);
    assert_eq!(controller.start_count(), 0);
}

#[tokio::test]
async fn apply_rejects_unsupported_volume_move_before_probe_or_inspect() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = volume_manifest();
    manifest.intent = Some(DeployIntent {
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
    seed_volume(&store, &manifest.namespace, "data", "machine-a").await;
    let participant = UnsupportedParticipantClient::default();
    let prober = FailingParticipantProbe {
        machine_id: MachineId::new("machine-b"),
    };

    let error = apply_with_certificate_coordination(
        &store,
        &participant,
        &local_machine_id,
        &manifest,
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &prober,
    )
    .await
    .expect_err("unsupported move should fail before participant work");

    assert_eq!(
        error,
        Error::Deploy(DeployError::VolumeMoveExecutionUnsupported {
            volume: "data".into()
        })
    );
    assert_eq!(participant.inspect_count(), 0);
}

#[tokio::test]
async fn apply_allows_unsupported_volume_move_client_when_plan_has_no_moves() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let participant = UnsupportedParticipantClient::default();
    let prober = FailingParticipantProbe {
        machine_id: MachineId::new("unused"),
    };

    let error = apply_with_certificate_coordination(
        &store,
        &participant,
        &local_machine_id,
        &manifest,
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &prober,
    )
    .await
    .expect_err("non-move deploy should reach participant startup");

    assert_eq!(
        error,
        Error::Operation {
            operation: "unsupported_participant",
            message: "start".into()
        }
    );
    assert_eq!(participant.inspect_count(), 1);
}

#[tokio::test]
async fn apply_deletes_volume_records_removed_from_manifest() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = volume_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller);

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect("first deploy");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected seeded volume");
    };
    assert_eq!(record.volume_name, "data");

    let next_manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.28",
    )]);
    let next_plan = resolve_plan(&store, &local_machine_id, &next_manifest)
        .await
        .expect("removal plan");
    apply_with_initial_plan(
        &store,
        &factory,
        &local_machine_id,
        &next_manifest,
        next_plan,
    )
    .await
    .expect("remove volume deploy");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes after removal");
    assert!(
        records.is_empty(),
        "expected volume record removed: {records:?}"
    );
}

#[tokio::test]
async fn apply_keeps_retained_volume_when_attached_service_is_removed() {
    // Regression: a service that mounts a volume can be removed from the manifest
    // while the volume itself is retained. The VolumeRecord must stay in the
    // store, but its attached_services must drop the now-deleted service so it
    // doesn't keep pointing at a name that no longer exists.
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = volume_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller);

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect("first deploy");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected seeded volume");
    };
    assert_eq!(record.volume_name, "data");
    assert_eq!(record.attached_services, vec!["db"]);

    // Replace `db` (which mounted `data`) with an unrelated `api` service while
    // keeping the volume declared.
    let mut next_manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.28",
    )]);
    next_manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    let next_plan = resolve_plan(&store, &local_machine_id, &next_manifest)
        .await
        .expect("removal plan");
    apply_with_initial_plan(
        &store,
        &factory,
        &local_machine_id,
        &next_manifest,
        next_plan,
    )
    .await
    .expect("redeploy without db");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes after service removal");
    let [record] = records.as_slice() else {
        panic!("expected volume retained, got: {records:?}");
    };
    assert_eq!(record.volume_name, "data");
    assert!(
        record.attached_services.is_empty(),
        "expected attached_services cleared, got: {:?}",
        record.attached_services
    );
}
