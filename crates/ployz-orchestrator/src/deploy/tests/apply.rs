use super::*;

#[test]
fn deployable_machines_includes_compute_region_and_excludes_draining_regions() {
    let machines = vec![
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
    ];

    let deployable = deployable_machines(&machines, &MachineId::new("local"));
    assert_eq!(
        deployable,
        vec![MachineId::new("compute"), MachineId::new("home")]
    );
}

#[tokio::test]
async fn apply_commits_unattached_volume_declarations_without_service_restart() {
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller.clone());

    apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
        .await
        .expect("deploy");

    assert_eq!(controller.start_count(), 1);
    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes");
    let [record] = records.as_slice() else {
        panic!("expected one committed volume");
    };
    assert_eq!(record.volume_name, "data");
    assert!(record.attached_services.is_empty());
}

#[tokio::test]
async fn apply_preserves_unattached_volume_record_on_unchanged_redeploy() {
    // The redeploy-with-attached-service skip path is covered above. Pin the
    // analogous skip behavior for a volume with no attached service: declared
    // in the manifest, no service mounts it, last_modified_by_deploy_id stays
    // tied to the first deploy after a no-op redeploy.
    let (store, _backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    manifest
        .volumes
        .push(test_volume("data", VolumeScope::Single));

    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let controller = FakeController::default();
    let factory = FakeParticipantClient::new(controller);

    let first =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("first deploy");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes after first apply");
    let [record] = records.as_slice() else {
        panic!("expected one committed volume, got: {records:?}");
    };
    assert!(record.attached_services.is_empty());
    assert_eq!(record.last_modified_by_deploy_id, first.deploy_id);

    let second_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("second plan");
    let second =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, second_plan)
            .await
            .expect("second deploy");

    let records = store
        .list_volumes(&manifest.namespace)
        .await
        .expect("list volumes after redeploy");
    let [record] = records.as_slice() else {
        panic!("expected one committed volume after redeploy, got: {records:?}");
    };
    assert!(record.attached_services.is_empty());
    assert_eq!(
        record.last_modified_by_deploy_id, first.deploy_id,
        "unchanged unattached volume should not be rewritten"
    );
    assert_ne!(record.last_modified_by_deploy_id, second.deploy_id);
}

#[tokio::test]
async fn apply_rejects_unreachable_participant_before_inspect_or_commit() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(2),
        "nginx:1.27",
    )]);
    let controller = FakeController::default();
    let participant_client = FakeParticipantClient::new(controller.clone());
    let prober = FailingParticipantProbe {
        machine_id: MachineId::new("machine-b"),
    };

    let error = apply_with_certificate_coordination(
        &store,
        &participant_client,
        &local_machine_id,
        &manifest,
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &prober,
    )
    .await
    .expect_err("unreachable participant should block deploy");

    assert_eq!(
        error,
        Error::Deploy(DeployError::ParticipantsUnreachable {
            unreachable_count: 1,
            participant_count: 2,
            machine_ids: vec!["machine-b".into()]
        })
    );
    assert_eq!(backend.deploy_status_write_count(), 0);
    assert_eq!(backend.commit_count(), 0);
    assert_eq!(controller.max_open_seen(), 0);
    assert_eq!(controller.start_count(), 0);
}

#[tokio::test]
async fn apply_with_initial_plan_does_not_commit_when_participant_inspect_fails() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(2),
        "nginx:1.27",
    )]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_open_machine: Some("machine-b".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("apply should fail");

    assert!(error.to_string().contains("injected open failure"));
    assert_eq!(backend.commit_count(), 0);
}

#[tokio::test]
async fn apply_with_initial_plan_does_not_commit_when_start_candidate_fails() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(2),
        "nginx:1.27",
    )]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_start_service: Some("api".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("apply should fail");

    assert!(error.to_string().contains("injected start failure"));
    assert_eq!(backend.commit_count(), 0);
    let releases = store
        .list_service_releases(&manifest.namespace)
        .await
        .expect("list releases");
    assert!(releases.is_empty());
    let revisions = store
        .list_deploy_revisions(&manifest.namespace)
        .await
        .expect("list deploy revisions");
    assert!(
        revisions.is_empty(),
        "failed deploy must not publish uncommitted revision facts"
    );
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state(), DeployState::Failed);
    assert!(last_update.finished_at().is_some());
    assert!(
        last_update
            .summary_json()
            .contains("injected start failure for 'api'"),
        "failed deploy summary should mention the apply error: {}",
        last_update.summary_json()
    );
    assert_default_phase_record(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "failed",
        Some("injected start failure for 'api'"),
    )
    .await;
}

#[tokio::test]
async fn apply_failure_marks_unstarted_later_phases_failed() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = test_manifest(vec![
        test_service_spec("db", Placement::replicated(1), "postgres:17"),
        test_service_spec("web", Placement::replicated(1), "nginx:1.27"),
        test_service_spec("worker", Placement::replicated(1), "busybox:1.36"),
    ]);
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
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
            DeployPhaseIntent {
                phase_id: "worker".into(),
                name: Some("Worker".into()),
                after: vec!["web".into()],
                services: vec!["worker".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
                rollback_policy: DeployPhaseRollbackPolicy::Reversible,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
        ],
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_start_service: Some("db".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("db startup should abort deploy");

    assert!(
        error
            .to_string()
            .contains("injected start failure for 'db'")
    );
    assert_eq!(backend.commit_count(), 0);
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state(), DeployState::Failed);
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "db",
        "failed",
        Some("injected start failure for 'db'"),
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "web",
        "failed",
        Some("injected start failure for 'db'"),
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "worker",
        "failed",
        Some("injected start failure for 'db'"),
    )
    .await;
}

#[tokio::test]
async fn apply_with_initial_plan_sets_cleanup_pending_after_cleanup_failure() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    let old_instance = test_instance_status(
        &manifest.namespace,
        "api",
        "slot-0001",
        "machine-a",
        "old-instance",
        &revision_hash,
    );
    store
        .record_instance_status(&old_instance)
        .await
        .expect("seed old instance");
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_remove_instance: Some("old-instance".into()),
        ..Default::default()
    });

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("apply");

    assert_eq!(result.state, crate::model::DeployState::CleanupPending);
    assert_eq!(backend.commit_count(), 1);
    // applying -> post-commit pending -> post-warning pending -> cleanup_pending
    assert_eq!(backend.deploy_status_write_count(), 4);
    let status_writes = backend.deploy_status_writes().await;
    assert!(
        status_writes
            .iter()
            .all(|record| record.state() != DeployState::Committed),
        "committed status must not be visible until post-commit cleanup succeeds: {status_writes:?}"
    );
    let commit_index = result
        .events
        .iter()
        .position(|event| event.step == "commit")
        .expect("commit event");
    let cleanup_pending_index = result
        .events
        .iter()
        .position(|event| event.step == "cleanup_pending")
        .expect("cleanup pending event");
    assert!(commit_index < cleanup_pending_index);
    assert!(
        result
            .events
            .iter()
            .filter(|event| event.step == "commit")
            .count()
            == 1
    );
    assert_eq!(factory.controller.drain_count(), 1);
    assert_eq!(factory.controller.remove_count(), 1);
}

#[tokio::test]
async fn apply_with_initial_plan_commits_once_after_all_starts_finish() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![
        test_service_spec("api", Placement::replicated(2), "nginx:1.27"),
        test_service_spec("worker", Placement::replicated(2), "busybox:1.0"),
    ]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        start_delay: Duration::from_millis(10),
        ..Default::default()
    });

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("apply");

    assert_eq!(result.state, crate::model::DeployState::Committed);
    assert_eq!(backend.commit_count(), 1);
    let commit_index = result
        .events
        .iter()
        .position(|event| event.step == "commit")
        .expect("commit event");
    let last_start_index = result
        .events
        .iter()
        .rposition(|event| event.step == "start_candidate")
        .expect("start events");
    assert!(last_start_index < commit_index);
    assert!(
        result
            .events
            .iter()
            .enumerate()
            .skip(commit_index + 1)
            .all(|(_, event)| event.step != "start_candidate")
    );
    assert_default_phase_record(
        &store,
        &manifest.namespace,
        &result.deploy_id,
        "succeeded",
        None,
    )
    .await;
}

#[tokio::test]
async fn commit_plan_contains_removed_services() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "worker",
        Placement::replicated(1),
        "busybox:1.0",
    )]);
    let revision_hash = "old-rev";
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            revision_hash,
            vec![test_slot("slot-0001", "machine-a", "inst-1", revision_hash)],
        ))
        .await
        .expect("seed release");
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let worker_slot = plan
        .services()
        .iter()
        .find(|service| service.service == "worker")
        .and_then(|service| service.slots.first().map(|slot| slot.slot_id().clone()))
        .expect("worker slot");
    let worker_revision_hash = manifest.services[0].revision_hash().expect("revision hash");
    let prepared = PreparedDeploy::new(
        DeployId::new("deploy-1"),
        10,
        local_machine_id,
        plan,
        Vec::new(),
    )
    .expect("prepared deploy");
    let started = HashMap::from([(
        (String::from("worker"), worker_slot.as_str().to_owned()),
        test_instance_status(
            &manifest.namespace,
            "worker",
            worker_slot.as_str(),
            "machine-a",
            "worker-inst-1",
            &worker_revision_hash,
        ),
    )]);

    let commit_plan = prepared
        .into_started(started)
        .into_commit_plan(Vec::new(), Vec::new())
        .expect("commit plan");

    assert_eq!(commit_plan.commit().removed_services, vec!["api"]);
    assert_eq!(commit_plan.commit().releases.len(), 1);
    assert_eq!(
        commit_plan.commit().deploy.state(),
        crate::model::DeployState::Committed
    );
    assert!(commit_plan.commit().deploy.committed_at().is_some());
    assert_eq!(
        commit_plan.commit().deploy.committed_at(),
        commit_plan.commit().deploy.finished_at()
    );
}
