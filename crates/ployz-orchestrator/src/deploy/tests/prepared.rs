use super::*;

#[tokio::test]
async fn resolve_plan_preview_includes_default_phase_for_basic_manifest() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:latest",
    )]);

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let preview = plan.to_preview(Vec::new());

    let [phase] = preview.phases.as_slice() else {
        panic!("expected one default deploy phase");
    };
    assert_eq!(phase.phase_id, DeployPhaseId::new("deploy"));
    assert_eq!(phase.name, "Deploy");
    assert_eq!(phase.order, 0);
    assert_eq!(phase.participants, vec![MachineId::new("machine-a")]);
    assert_eq!(phase.commit_policy, DeployPhaseCommitPolicy::EndOfDeploy);
    assert_eq!(phase.rollback_policy, DeployPhaseRollbackPolicy::Reversible);
    assert_eq!(phase.advance_policy, DeployPhaseAdvancePolicy::Immediate);
    assert!(matches!(
        phase.work.as_slice(),
        [DeployPhaseWork::Service { service, action }]
            if service == "api" && *action == DeployChangeKind::Create
    ));
    assert!(preview.volume_clone_preflights.is_empty());
}

#[tokio::test]
async fn resolve_plan_baseline_tracks_participant_changes() {
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "agent",
        Placement::Global,
        "busybox:1.0",
    )]);
    let store_a = seeded_store_with_machines(&["machine-a"]).await;
    let store_b = seeded_store_with_machines(&["machine-a", "machine-b"]).await;

    let baseline_a = resolve_plan(&store_a, &local_machine_id, &manifest)
        .await
        .expect("plan a")
        .baseline();
    let baseline_b = resolve_plan(&store_b, &local_machine_id, &manifest)
        .await
        .expect("plan b")
        .baseline();

    assert_ne!(
        baseline_a.components.participants,
        baseline_b.components.participants
    );
    assert_ne!(baseline_a.fingerprint, baseline_b.fingerprint);
}

#[tokio::test]
async fn resolve_plan_baseline_tracks_phase_work_changes() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let base_manifest = test_manifest(vec![
        test_service_spec("db", Placement::replicated(1), "postgres:17"),
        test_service_spec("web", Placement::replicated(1), "nginx:1.27"),
    ]);
    let phased_manifest = checkpointed_db_web_manifest();

    let baseline_a = resolve_plan(&store, &local_machine_id, &base_manifest)
        .await
        .expect("base plan")
        .baseline();
    let baseline_b = resolve_plan(&store, &local_machine_id, &phased_manifest)
        .await
        .expect("phased plan")
        .baseline();

    assert_ne!(baseline_a.components.phases, baseline_b.components.phases);
    assert_ne!(baseline_a.fingerprint, baseline_b.fingerprint);
}

#[tokio::test]
async fn ensure_plan_stable_preserves_execution_plan_changed_without_service_source_baseline() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_committed_service_release(
        &store,
        &source_namespace,
        test_service_spec("web", Placement::replicated(1), "nginx:1.27"),
    )
    .await;

    let mut manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::replicated(1),
        "example/web:pr-39",
    )]);
    manifest.namespace = Namespace::new("pr-39");
    manifest.intent = Some(DeployIntent {
        services: vec![ServiceIntentHint {
            service: "web".into(),
            intent: ServiceIntent::Branch {
                source_namespace: source_namespace.clone(),
                source_service: "web".into(),
                expected_source_revision_hash: None,
            },
        }],
        volumes: Vec::new(),
        phases: Vec::new(),
    });

    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial branch plan");
    seed_committed_service_release(
        &store,
        &source_namespace,
        test_service_spec("web", Placement::replicated(1), "nginx:1.28"),
    )
    .await;
    let final_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("final branch plan");

    let error = ensure_plan_stable(&initial_plan, &final_plan, None)
        .expect_err("unguarded source revision drift should fail");

    assert_eq!(error, Error::Deploy(DeployError::ExecutionPlanChanged));
}

#[tokio::test]
async fn apply_rejects_empty_deploy_baseline_before_participant_inspect() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::replicated(1),
        "example/web:pr-39",
    )]);
    let controller = FakeController::default();
    let participant_client = FakeParticipantClient::new(controller.clone());
    let empty_baseline = empty_deploy_baseline();

    let error = apply_with_deploy_id_and_preconditions(
        &store,
        &participant_client,
        &local_machine_id,
        &manifest,
        DeployId::new("deploy-empty-source-baseline"),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
        DeployApplyPreconditions {
            expected_baseline: Some(empty_baseline),
        },
    )
    .await
    .expect_err("empty deploy baseline should fail");

    assert!(matches!(
        error,
        Error::Deploy(DeployError::DeployOptionInvalid { .. })
    ));
    assert_eq!(controller.max_open_seen(), 0);
    assert_eq!(controller.start_count(), 0);
}

#[tokio::test]
async fn apply_rejects_stale_service_source_baseline_before_participant_inspect() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    seed_committed_service_release(
        &store,
        &source_namespace,
        test_service_spec("web", Placement::replicated(1), "nginx:1.27"),
    )
    .await;

    let mut manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::replicated(1),
        "example/web:pr-39",
    )]);
    manifest.namespace = Namespace::new("pr-39");
    manifest.intent = Some(DeployIntent {
        services: vec![ServiceIntentHint {
            service: "web".into(),
            intent: ServiceIntent::Branch {
                source_namespace: source_namespace.clone(),
                source_service: "web".into(),
                expected_source_revision_hash: None,
            },
        }],
        volumes: Vec::new(),
        phases: Vec::new(),
    });

    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial branch plan");
    let expected_baseline = initial_plan.baseline();
    seed_committed_service_release(
        &store,
        &source_namespace,
        test_service_spec("web", Placement::replicated(1), "nginx:1.28"),
    )
    .await;
    let actual_baseline = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("post-drift branch plan")
        .baseline();

    let controller = FakeController::default();
    let participant_client = FakeParticipantClient::new(controller.clone());
    let error = apply_with_deploy_id_and_preconditions(
        &store,
        &participant_client,
        &local_machine_id,
        &manifest,
        DeployId::new("deploy-stale-source"),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
        DeployApplyPreconditions {
            expected_baseline: Some(expected_baseline.clone()),
        },
    )
    .await
    .expect_err("stale service source baseline should fail");

    let Error::Deploy(DeployError::DeployBaselineChanged { diff }) = error else {
        panic!("expected deploy baseline changed");
    };
    assert_eq!(diff.expected, expected_baseline);
    assert_eq!(diff.actual, actual_baseline);
    assert!(
        diff.changed_components()
            .contains(&DeployBaselineComponent::ServiceSources)
    );
    assert_eq!(controller.max_open_seen(), 0);
    assert_eq!(controller.start_count(), 0);
}

#[tokio::test]
async fn apply_rejects_stale_participant_baseline_before_participant_inspect() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "agent",
        Placement::Global,
        "busybox:1.0",
    )]);
    let expected_baseline = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan")
        .baseline();
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed second participant");
    let actual_baseline = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("post-drift plan")
        .baseline();

    let controller = FakeController::default();
    let participant_client = FakeParticipantClient::new(controller.clone());
    let error = apply_with_deploy_id_and_preconditions(
        &store,
        &participant_client,
        &local_machine_id,
        &manifest,
        DeployId::new("deploy-stale-participants"),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
        DeployApplyPreconditions {
            expected_baseline: Some(expected_baseline.clone()),
        },
    )
    .await
    .expect_err("stale participant baseline should fail");

    let Error::Deploy(DeployError::DeployBaselineChanged { diff }) = error else {
        panic!("expected deploy baseline changed");
    };
    assert_eq!(diff.expected, expected_baseline);
    assert_eq!(diff.actual, actual_baseline);
    assert!(
        diff.changed_components()
            .contains(&DeployBaselineComponent::Participants)
    );
    assert_eq!(controller.max_open_seen(), 0);
    assert_eq!(controller.start_count(), 0);
}

#[tokio::test]
async fn prepare_stores_preview_evidence_and_apply_prepared_marks_applied() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let prepared = prepare(
        &store,
        &local_machine_id,
        &manifest,
        &NoopParticipantProbe,
        DeployId::new("prepare-apply"),
        60,
    )
    .await
    .expect("prepare deploy");

    assert_eq!(prepared.state, PreparedDeployState::Prepared);
    assert_eq!(
        prepared.preview,
        preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
            .await
            .expect("preview")
    );
    assert_eq!(prepared.namespace, manifest.namespace);
    assert_eq!(prepared.preview.baseline, Some(prepared.baseline.clone()));
    assert!(prepared.baseline.is_canonical());
    assert_eq!(
        store
            .get_prepared_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get prepared")
            .expect("prepared exists"),
        prepared
    );

    let participant_client = FakeParticipantClient::new(FakeController::default());
    let result = apply_prepared_with_certificate_coordination(
        &store,
        &participant_client,
        &local_machine_id,
        prepared.prepared_deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect("apply prepared");

    assert_eq!(result.deploy_id, prepared.prepared_deploy_id);
    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(
        store
            .get_prepared_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get applied prepared")
            .expect("prepared still exists")
            .state,
        PreparedDeployState::Applied
    );

    let retry_controller = FakeController::default();
    let retry_client = FakeParticipantClient::new(retry_controller.clone());
    let retry = apply_prepared_with_certificate_coordination(
        &store,
        &retry_client,
        &local_machine_id,
        prepared.prepared_deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect("retry applied prepared deploy should replay");

    assert_eq!(retry.deploy_id, prepared.prepared_deploy_id);
    assert_eq!(retry.state, DeployState::Committed);
    assert_eq!(retry_controller.start_count(), 0);
}

#[tokio::test]
async fn apply_prepared_rejects_terminal_record_states_before_participant_inspect() {
    for state in [
        PreparedDeployState::Applied,
        PreparedDeployState::Expired,
        PreparedDeployState::Superseded,
    ] {
        let store = seeded_store_with_machines(&["machine-a"]).await;
        let local_machine_id = MachineId::new("local");
        let manifest = test_manifest(vec![test_service_spec(
            "web",
            Placement::replicated(1),
            "nginx:1.27",
        )]);
        let mut prepared = prepare(
            &store,
            &local_machine_id,
            &manifest,
            &NoopParticipantProbe,
            DeployId::new(format!("prepare-{state}")),
            60,
        )
        .await
        .expect("prepare deploy");
        prepared.state = state;
        store
            .write_prepared_deploy(&prepared)
            .await
            .expect("seed terminal prepared state");

        let controller = FakeController::default();
        let participant_client = FakeParticipantClient::new(controller.clone());
        let error = apply_prepared_with_certificate_coordination(
            &store,
            &participant_client,
            &local_machine_id,
            prepared.prepared_deploy_id.clone(),
            Arc::new(NoopIssuanceCoordinator),
            Arc::new(NoopAcmeAccountCoordinator),
            Arc::new(LocalHttp01ChallengeReadiness),
            Arc::new(NoopAcmeIssuerFactory::default()),
            &NoopParticipantProbe,
        )
        .await
        .expect_err("terminal prepared deploy should fail");

        assert!(matches!(
            error,
            Error::Deploy(DeployError::PreparedDeployNotApplicable {
                state: actual,
                ..
            }) if actual == state
        ));
        assert_eq!(controller.max_open_seen(), 0);
        assert_eq!(controller.start_count(), 0);
        assert_eq!(
            store
                .get_prepared_deploy(&prepared.prepared_deploy_id)
                .await
                .expect("get prepared")
                .expect("prepared exists")
                .state,
            state
        );
    }
}

#[tokio::test]
async fn apply_prepared_rejects_expired_record_before_participant_inspect() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let prepared = prepare(
        &store,
        &local_machine_id,
        &manifest,
        &NoopParticipantProbe,
        DeployId::new("prepare-expired"),
        0,
    )
    .await
    .expect("prepare expired deploy");

    let controller = FakeController::default();
    let participant_client = FakeParticipantClient::new(controller.clone());
    let error = apply_prepared_with_certificate_coordination(
        &store,
        &participant_client,
        &local_machine_id,
        prepared.prepared_deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect_err("expired prepared deploy should fail");

    assert!(matches!(
        error,
        Error::Deploy(DeployError::PreparedDeployExpired { .. })
    ));
    assert_eq!(controller.max_open_seen(), 0);
    assert_eq!(controller.start_count(), 0);
    assert_eq!(
        store
            .get_prepared_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get expired prepared")
            .expect("prepared exists")
            .state,
        PreparedDeployState::Expired
    );
}

#[tokio::test]
async fn apply_prepared_marks_expired_before_checking_stale_baseline() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "agent",
        Placement::Global,
        "busybox:1.0",
    )]);
    let prepared = prepare(
        &store,
        &local_machine_id,
        &manifest,
        &NoopParticipantProbe,
        DeployId::new("prepare-expired-stale"),
        0,
    )
    .await
    .expect("prepare expired deploy");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed second participant");

    let controller = FakeController::default();
    let participant_client = FakeParticipantClient::new(controller.clone());
    let error = apply_prepared_with_certificate_coordination(
        &store,
        &participant_client,
        &local_machine_id,
        prepared.prepared_deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect_err("expired stale prepared deploy should fail");

    assert!(matches!(
        error,
        Error::Deploy(DeployError::PreparedDeployExpired { .. })
    ));
    assert_eq!(controller.max_open_seen(), 0);
    assert_eq!(controller.start_count(), 0);
    assert_eq!(
        store
            .get_prepared_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get expired prepared")
            .expect("prepared exists")
            .state,
        PreparedDeployState::Expired
    );
}

#[tokio::test]
async fn apply_prepared_rejects_stale_baseline_before_participant_inspect() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "agent",
        Placement::Global,
        "busybox:1.0",
    )]);
    let prepared = prepare(
        &store,
        &local_machine_id,
        &manifest,
        &NoopParticipantProbe,
        DeployId::new("prepare-stale"),
        60,
    )
    .await
    .expect("prepare deploy");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed second participant");

    let controller = FakeController::default();
    let participant_client = FakeParticipantClient::new(controller.clone());
    let error = apply_prepared_with_certificate_coordination(
        &store,
        &participant_client,
        &local_machine_id,
        prepared.prepared_deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect_err("stale prepared baseline should fail");

    let Error::Deploy(DeployError::DeployBaselineChanged { diff }) = error else {
        panic!("expected deploy baseline changed");
    };
    assert!(
        diff.changed_components()
            .contains(&DeployBaselineComponent::Participants)
    );
    assert_eq!(controller.max_open_seen(), 0);
    assert_eq!(controller.start_count(), 0);
    assert_eq!(
        store
            .get_prepared_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get stale prepared")
            .expect("prepared exists")
            .state,
        PreparedDeployState::Superseded
    );
}

#[tokio::test]
async fn apply_prepared_rejects_mismatched_manifest_namespace_before_participant_inspect() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let mut prepared = prepare(
        &store,
        &local_machine_id,
        &manifest,
        &NoopParticipantProbe,
        DeployId::new("prepare-mismatched-namespace"),
        60,
    )
    .await
    .expect("prepare deploy");
    manifest.namespace = Namespace::new("other");
    prepared.manifest_json = serde_json::to_string(&manifest).expect("serialize manifest");
    store
        .write_prepared_deploy(&prepared)
        .await
        .expect("write malformed prepared deploy");

    let controller = FakeController::default();
    let participant_client = FakeParticipantClient::new(controller.clone());
    let error = apply_prepared_with_certificate_coordination(
        &store,
        &participant_client,
        &local_machine_id,
        prepared.prepared_deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect_err("mismatched prepared manifest should fail");

    assert!(matches!(
        error,
        Error::Deploy(DeployError::PreparedDeployInvalid { .. })
    ));
    assert_eq!(controller.max_open_seen(), 0);
    assert_eq!(controller.start_count(), 0);
}

#[tokio::test]
async fn apply_prepared_replays_existing_terminal_committed_deploy_before_expiry() {
    for state in [DeployState::Committed, DeployState::CleanupPending] {
        let store = seeded_store_with_machines(&["machine-a"]).await;
        let local_machine_id = MachineId::new("local");
        let manifest = test_manifest(vec![test_service_spec(
            "web",
            Placement::replicated(1),
            "nginx:1.27",
        )]);
        let prepared = prepare(
            &store,
            &local_machine_id,
            &manifest,
            &NoopParticipantProbe,
            DeployId::new(format!("prepare-replay-{state}")),
            0,
        )
        .await
        .expect("prepare deploy");
        store
            .write_deploy_status(&prepared_deploy_status(&prepared, state))
            .await
            .expect("seed terminal deploy status");

        let controller = FakeController::default();
        let participant_client = FakeParticipantClient::new(controller.clone());
        let result = apply_prepared_with_certificate_coordination(
            &store,
            &participant_client,
            &local_machine_id,
            prepared.prepared_deploy_id.clone(),
            Arc::new(NoopIssuanceCoordinator),
            Arc::new(NoopAcmeAccountCoordinator),
            Arc::new(LocalHttp01ChallengeReadiness),
            Arc::new(NoopAcmeIssuerFactory::default()),
            &NoopParticipantProbe,
        )
        .await
        .expect("replay committed prepared deploy");

        assert_eq!(result.state, state);
        assert_eq!(controller.max_open_seen(), 0);
        assert_eq!(controller.start_count(), 0);
        assert_eq!(
            store
                .get_prepared_deploy(&prepared.prepared_deploy_id)
                .await
                .expect("get prepared")
                .expect("prepared exists")
                .state,
            PreparedDeployState::Applied
        );
    }
}

#[tokio::test]
async fn apply_prepared_supersedes_existing_checkpoint_terminal_deploy() {
    for state in [
        DeployState::CheckpointCommitted,
        DeployState::FailedAfterCheckpoint,
    ] {
        let store = seeded_store_with_machines(&["machine-a"]).await;
        let local_machine_id = MachineId::new("local");
        let manifest = test_manifest(vec![test_service_spec(
            "web",
            Placement::replicated(1),
            "nginx:1.27",
        )]);
        let prepared = prepare(
            &store,
            &local_machine_id,
            &manifest,
            &NoopParticipantProbe,
            DeployId::new(format!("prepare-checkpoint-{state}")),
            60,
        )
        .await
        .expect("prepare deploy");
        store
            .write_deploy_status(&prepared_deploy_status(&prepared, state))
            .await
            .expect("seed checkpoint deploy status");

        let controller = FakeController::default();
        let participant_client = FakeParticipantClient::new(controller.clone());
        let error = apply_prepared_with_certificate_coordination(
            &store,
            &participant_client,
            &local_machine_id,
            prepared.prepared_deploy_id.clone(),
            Arc::new(NoopIssuanceCoordinator),
            Arc::new(NoopAcmeAccountCoordinator),
            Arc::new(LocalHttp01ChallengeReadiness),
            Arc::new(NoopAcmeIssuerFactory::default()),
            &NoopParticipantProbe,
        )
        .await
        .expect_err("checkpoint terminal prepared deploy should fail");

        assert!(matches!(
            error,
            Error::Deploy(DeployError::PreparedDeployNotApplicable {
                state: PreparedDeployState::Superseded,
                ..
            })
        ));
        assert_eq!(controller.max_open_seen(), 0);
        assert_eq!(controller.start_count(), 0);
        assert_eq!(
            store
                .get_prepared_deploy(&prepared.prepared_deploy_id)
                .await
                .expect("get prepared")
                .expect("prepared exists")
                .state,
            PreparedDeployState::Superseded
        );
    }
}

#[tokio::test]
async fn apply_prepared_keeps_prepared_after_post_commit_status_error() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let prepared = prepare(
        &store,
        &local_machine_id,
        &manifest,
        &NoopParticipantProbe,
        DeployId::new("prepare-post-commit-error"),
        60,
    )
    .await
    .expect("prepare deploy");
    backend.reset_counts();
    backend.fail_committed_status_writes(true);

    let participant_client = FakeParticipantClient::new(FakeController::default());
    let error = apply_prepared_with_certificate_coordination(
        &store,
        &participant_client,
        &local_machine_id,
        prepared.prepared_deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect_err("post-commit status failure should preserve original error");

    assert!(
        error
            .to_string()
            .contains("injected committed status failure")
    );
    assert_eq!(
        store
            .get_prepared_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get prepared")
            .expect("prepared exists")
            .state,
        PreparedDeployState::Prepared
    );
}

#[tokio::test]
async fn apply_prepared_repairs_terminal_status_from_durable_commit() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let prepared = prepare(
        &store,
        &local_machine_id,
        &manifest,
        &NoopParticipantProbe,
        DeployId::new("prepare-terminal-status-fails"),
        60,
    )
    .await
    .expect("prepare deploy");
    backend.reset_counts();
    backend.fail_committed_status_writes(true);
    backend.fail_succeeded_phase_upsert(true);

    let participant_client = FakeParticipantClient::new(FakeController::default());
    let error = apply_prepared_with_certificate_coordination(
        &store,
        &participant_client,
        &local_machine_id,
        prepared.prepared_deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect_err("committed status write failure should fail response");

    assert!(
        error
            .to_string()
            .contains("injected committed status failure")
    );
    assert_eq!(backend.commit_count(), 1);
    assert_eq!(
        store
            .get_prepared_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get prepared")
            .expect("prepared exists")
            .state,
        PreparedDeployState::Prepared
    );
    assert!(!matches!(
        store
            .get_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get deploy")
            .map(|record| record.state()),
        Some(DeployState::Committed | DeployState::CleanupPending)
    ));

    backend.fail_committed_status_writes(false);
    backend.fail_succeeded_phase_upsert(false);
    backend.reset_counts();
    let retry_controller = FakeController::default();
    let retry_participant_client = FakeParticipantClient::new(retry_controller.clone());
    let retry = apply_prepared_with_certificate_coordination(
        &store,
        &retry_participant_client,
        &local_machine_id,
        prepared.prepared_deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect("retry should repair terminal status from durable commit");

    assert_eq!(retry.deploy_id, prepared.prepared_deploy_id);
    assert_eq!(retry.state, DeployState::Committed);
    assert_eq!(retry_controller.start_count(), 0);
    assert_eq!(backend.commit_count(), 0);
    assert!(
        backend.succeeded_phase_upsert_count() > 0,
        "retry should repair phase records from durable phase commits"
    );
    assert_eq!(
        store
            .get_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get repaired deploy")
            .expect("deploy status repaired")
            .state(),
        DeployState::Committed
    );
    assert_eq!(
        store
            .get_prepared_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get prepared")
            .expect("prepared exists")
            .state,
        PreparedDeployState::Applied
    );
}

#[tokio::test]
async fn apply_prepared_recovers_final_commit_over_stale_checkpoint_status() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = checkpointed_db_web_manifest();
    let prepared = prepare(
        &store,
        &local_machine_id,
        &manifest,
        &NoopParticipantProbe,
        DeployId::new("prepare-checkpoint-final-status-fails"),
        60,
    )
    .await
    .expect("prepare deploy");
    backend.reset_counts();
    backend.fail_committed_status_writes(true);

    let participant_client = FakeParticipantClient::new(FakeController::default());
    let error = apply_prepared_with_certificate_coordination(
        &store,
        &participant_client,
        &local_machine_id,
        prepared.prepared_deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect_err("final committed status write failure should fail response");

    assert!(
        error
            .to_string()
            .contains("injected committed status failure")
    );
    assert_eq!(
        store
            .get_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get checkpoint deploy")
            .expect("checkpoint status remains")
            .state(),
        DeployState::CheckpointCommitted
    );
    assert_eq!(
        store
            .get_prepared_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get prepared")
            .expect("prepared exists")
            .state,
        PreparedDeployState::Prepared
    );

    backend.fail_committed_status_writes(false);
    backend.reset_counts();
    let retry_controller = FakeController::default();
    let retry_participant_client = FakeParticipantClient::new(retry_controller.clone());
    let retry = apply_prepared_with_certificate_coordination(
        &store,
        &retry_participant_client,
        &local_machine_id,
        prepared.prepared_deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect("retry should prefer durable final commit over checkpoint status");

    assert_eq!(retry.deploy_id, prepared.prepared_deploy_id);
    assert_eq!(retry.state, DeployState::Committed);
    assert_eq!(retry_controller.start_count(), 0);
    assert_eq!(backend.commit_count(), 0);
    assert_eq!(
        store
            .get_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get repaired deploy")
            .expect("deploy status repaired")
            .state(),
        DeployState::Committed
    );
    assert_eq!(
        store
            .get_prepared_deploy(&prepared.prepared_deploy_id)
            .await
            .expect("get prepared")
            .expect("prepared exists")
            .state,
        PreparedDeployState::Applied
    );
}

#[tokio::test]
async fn apply_prepared_repairs_phases_before_replaying_committed_status() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "web",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let prepared = prepare(
        &store,
        &local_machine_id,
        &manifest,
        &NoopParticipantProbe,
        DeployId::new("prepare-committed-phase-repair"),
        60,
    )
    .await
    .expect("prepare deploy");
    backend.fail_succeeded_phase_upsert(true);

    let participant_client = FakeParticipantClient::new(FakeController::default());
    let first = apply_prepared_with_certificate_coordination(
        &store,
        &participant_client,
        &local_machine_id,
        prepared.prepared_deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect("first apply should commit despite best-effort phase projection failure");

    assert_eq!(first.state, DeployState::Committed);
    backend.fail_succeeded_phase_upsert(false);
    backend.reset_counts();
    let retry_controller = FakeController::default();
    let retry_participant_client = FakeParticipantClient::new(retry_controller.clone());
    let retry = apply_prepared_with_certificate_coordination(
        &store,
        &retry_participant_client,
        &local_machine_id,
        prepared.prepared_deploy_id.clone(),
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
    .expect("retry should replay committed status");

    assert_eq!(retry.state, DeployState::Committed);
    assert_eq!(retry_controller.start_count(), 0);
    assert_eq!(backend.commit_count(), 0);
    assert!(
        backend.succeeded_phase_upsert_count() > 0,
        "committed fast-path replay should repair phase records from durable phase commits"
    );
}

#[tokio::test]
async fn phase_startup_uses_one_worker_per_machine_but_parallel_across_machines() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![
        test_service_spec("api", Placement::replicated(2), "nginx:1.27"),
        test_service_spec("worker", Placement::replicated(2), "busybox:1.0"),
    ]);

    let controller = FakeController {
        start_delay: Duration::from_millis(40),
        ..Default::default()
    };
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let factory = FakeParticipantClient::new(controller.clone());
    let deploy_id = DeployId::new("deploy-phase");
    let (participants, _events) =
        ParticipantSet::inspect(&factory, &plan, &local_machine_id, &deploy_id)
            .await
            .expect("inspect participants");
    let startup = run_phase_startup(&store, &factory, &participants, &plan)
        .await
        .expect("run startup");

    assert_eq!(startup.started.len(), 4);
    assert_eq!(controller.start_count(), 4);
    assert!(controller.max_global_start_seen() >= 2);
    assert_eq!(controller.max_machine_start_seen("machine-a"), 1);
    assert_eq!(controller.max_machine_start_seen("machine-b"), 1);
}

#[tokio::test]
async fn phase_startup_stops_scheduling_machine_queues_after_failure() {
    let machine_names = (0..65)
        .map(|index| format!("machine-{index:02}"))
        .collect::<Vec<_>>();
    let machine_refs = machine_names.iter().map(String::as_str).collect::<Vec<_>>();
    let store = seeded_store_with_machines(&machine_refs).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.27",
    )]);
    let controller = FakeController {
        fail_start_service: Some("api".into()),
        ..Default::default()
    };
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let factory = FakeParticipantClient::new(controller.clone());
    let deploy_id = DeployId::new("deploy-phase");
    let (participants, _events) =
        ParticipantSet::inspect(&factory, &plan, &local_machine_id, &deploy_id)
            .await
            .expect("inspect participants");

    run_phase_startup(&store, &factory, &participants, &plan)
        .await
        .expect_err("startup failure should fail phase");

    assert_eq!(controller.start_count(), 64);
}

#[tokio::test]
async fn run_phase_startup_waits_for_previous_phase_before_next_phase() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![
        test_service_spec("api", Placement::replicated(2), "nginx:1.27"),
        test_service_spec("worker", Placement::replicated(2), "busybox:1.0"),
    ]);
    let mut plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let [first, second] = plan.services_mut() else {
        panic!("expected two planned services");
    };
    first.set_phase_for_test(0);
    second.set_phase_for_test(1);

    let controller = FakeController {
        start_delay: Duration::from_millis(20),
        ..Default::default()
    };
    let factory = FakeParticipantClient::new(controller.clone());
    let deploy_id = DeployId::new("deploy-test");
    let (participants, _events) =
        ParticipantSet::inspect(&factory, &plan, &local_machine_id, &deploy_id)
            .await
            .expect("inspect participants");

    let startup = run_phase_startup(&store, &factory, &participants, &plan)
        .await
        .expect("run phases");

    assert_eq!(startup.started.len(), 4);
    let log = controller.start_log().await;
    let first_worker = log
        .iter()
        .position(|entry| entry.contains("worker"))
        .expect("worker start present");
    let last_api = log
        .iter()
        .rposition(|entry| entry.contains("api"))
        .expect("api start present");
    assert!(last_api < first_worker);
}

#[tokio::test]
async fn apply_marks_default_phase_failed_when_commit_fails_after_phase_work() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    backend.fail_commit(true);
    let factory = FakeParticipantClient::new(FakeController::default());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("commit failure should fail deploy");

    assert!(error.to_string().contains("injected commit failure"));
    assert_eq!(backend.commit_count(), 1);
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state(), DeployState::Failed);
    assert_default_phase_record(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "failed",
        Some("injected commit failure"),
    )
    .await;
}

#[tokio::test]
async fn apply_records_committed_status_when_phase_success_evidence_fails() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    backend.fail_succeeded_phase_upsert(true);
    let factory = FakeParticipantClient::new(FakeController::default());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("phase success evidence failure should not fail committed deploy");

    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(backend.commit_count(), 1);
    let writes = backend.deploy_status_writes().await;
    assert!(
        writes
            .iter()
            .any(|record| record.state() == DeployState::Committed),
        "committed deploy status should be written even when phase evidence fails"
    );
    assert!(
        writes
            .iter()
            .all(|record| record.state() != DeployState::Failed),
        "phase evidence failure must not mark committed deploy failed"
    );
    let committed_record = store
        .get_deploy(&result.deploy_id)
        .await
        .expect("get deploy")
        .expect("committed deploy record");
    assert_eq!(committed_record.state(), DeployState::Committed);
    assert_default_phase_record(
        &store,
        &manifest.namespace,
        &result.deploy_id,
        "succeeded",
        None,
    )
    .await;
    let phase = store
        .get_deploy_phase(
            &manifest.namespace,
            &result.deploy_id,
            &DeployPhaseId::new("deploy"),
        )
        .await
        .expect("get phase")
        .expect("phase");
    assert_eq!(phase.commit_deploy_id(), Some(result.deploy_id));
}

#[tokio::test]
async fn apply_marks_phase_succeeded_when_first_committed_status_write_fails() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::replicated(1),
        "nginx:1.27",
    )]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    backend.fail_committed_status_writes(true);
    let factory = FakeParticipantClient::new(FakeController::default());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("committed status write failure should fail response");

    assert!(
        error
            .to_string()
            .contains("injected committed status failure")
    );
    assert_eq!(backend.commit_count(), 1);
    let writes = backend.deploy_status_writes().await;
    assert!(
        writes
            .iter()
            .all(|record| record.state() != DeployState::Failed),
        "post-commit status failure must not mark committed deploy failed"
    );
    let post_commit_attempt = writes
        .iter()
        .find(|record| record.state() == DeployState::CleanupPending)
        .expect("post-commit status write should have been attempted");
    assert_default_phase_record(
        &store,
        &manifest.namespace,
        &post_commit_attempt.deploy_id,
        "succeeded",
        None,
    )
    .await;
}

#[tokio::test]
async fn resolve_plan_builds_manifest_phase_work_in_dependency_order() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = test_manifest(vec![
        test_service_spec("db", Placement::replicated(1), "postgres:17"),
        test_service_spec("web", Placement::replicated(1), "nginx:1.27"),
    ]);
    manifest.intent = Some(DeployIntent {
        services: Vec::new(),
        volumes: Vec::new(),
        phases: vec![
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
                phase_id: "db".into(),
                name: Some("Database".into()),
                after: Vec::new(),
                services: vec!["db".into()],
                volumes: Vec::new(),
                commit_policy: DeployPhaseCommitPolicy::Checkpoint,
                rollback_policy: DeployPhaseRollbackPolicy::ForwardOnly,
                advance_policy: DeployPhaseAdvancePolicy::Immediate,
            },
        ],
    });

    let preview = preview(&store, &local_machine_id, &manifest, &NoopParticipantProbe)
        .await
        .expect("preview");

    let [db_phase, web_phase] = preview.phases.as_slice() else {
        panic!("expected two phases, got {:?}", preview.phases);
    };
    assert_eq!(db_phase.phase_id, DeployPhaseId::new("db"));
    assert_eq!(db_phase.order, 0);
    assert_eq!(db_phase.commit_policy, DeployPhaseCommitPolicy::Checkpoint);
    assert!(db_phase.after.is_empty());
    assert_eq!(
        db_phase.work.as_slice(),
        &[DeployPhaseWork::Service {
            service: "db".into(),
            action: DeployChangeKind::Create,
        }]
    );
    assert_eq!(web_phase.phase_id, DeployPhaseId::new("web"));
    assert_eq!(web_phase.order, 1);
    assert_eq!(web_phase.after, vec![DeployPhaseId::new("db")]);
    assert_eq!(
        web_phase.commit_policy,
        DeployPhaseCommitPolicy::EndOfDeploy
    );
    assert_eq!(
        web_phase.work.as_slice(),
        &[DeployPhaseWork::Service {
            service: "web".into(),
            action: DeployChangeKind::Create,
        }]
    );
}

#[tokio::test]
async fn apply_checkpoint_phase_commits_before_final_phase() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = checkpointed_db_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController::default());

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("apply");

    assert_eq!(result.state, DeployState::Committed);
    assert_eq!(backend.commit_count(), 2);
    let writes = backend.deploy_status_writes().await;
    assert!(
        writes
            .iter()
            .any(|record| record.deploy_id == result.deploy_id
                && record.state() == DeployState::CheckpointCommitted),
        "checkpoint should write status against the original deploy id"
    );
    let releases = store
        .list_service_releases(&manifest.namespace)
        .await
        .expect("list releases");
    assert!(
        releases.iter().any(|release| release.service == "db"),
        "checkpoint commit should publish db release"
    );
    assert!(
        releases.iter().any(|release| release.service == "web"),
        "final commit should publish web release"
    );
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &result.deploy_id,
        "db",
        "succeeded",
        None,
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &result.deploy_id,
        "web",
        "succeeded",
        None,
    )
    .await;
    let db_phase = store
        .get_deploy_phase(
            &manifest.namespace,
            &result.deploy_id,
            &DeployPhaseId::new("db"),
        )
        .await
        .expect("get db phase")
        .expect("db phase");
    assert_eq!(
        db_phase.commit_deploy_id(),
        Some(DeployId::new(format!(
            "{}:phase:db",
            result.deploy_id.as_str()
        )))
    );
    let web_phase = store
        .get_deploy_phase(
            &manifest.namespace,
            &result.deploy_id,
            &DeployPhaseId::new("web"),
        )
        .await
        .expect("get web phase")
        .expect("web phase");
    assert_eq!(web_phase.commit_deploy_id(), Some(result.deploy_id));
}

#[tokio::test]
async fn apply_failure_after_checkpoint_preserves_committed_phase_facts() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = checkpointed_db_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_start_service: Some("web".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("web startup should fail after db checkpoint");

    assert!(
        error
            .to_string()
            .contains("injected start failure for 'web'")
    );
    assert_eq!(backend.commit_count(), 1);
    let last_update = backend
        .last_deploy_status_write()
        .await
        .expect("failed deploy record should be written");
    assert_eq!(last_update.state(), DeployState::FailedAfterCheckpoint);
    let releases = store
        .list_service_releases(&manifest.namespace)
        .await
        .expect("list releases");
    assert!(
        releases.iter().any(|release| release.service == "db"),
        "checkpointed db release must remain committed"
    );
    assert!(
        releases.iter().all(|release| release.service != "web"),
        "failed final phase must not publish web release"
    );
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "db",
        "succeeded",
        None,
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "web",
        "failed",
        Some("injected start failure for 'web'"),
    )
    .await;
    let db_phase = store
        .get_deploy_phase(
            &manifest.namespace,
            &last_update.deploy_id,
            &DeployPhaseId::new("db"),
        )
        .await
        .expect("get db phase")
        .expect("db phase");
    assert_eq!(
        db_phase.commit_deploy_id(),
        Some(DeployId::new(format!(
            "{}:phase:db",
            last_update.deploy_id.as_str()
        )))
    );
    let web_phase = store
        .get_deploy_phase(
            &manifest.namespace,
            &last_update.deploy_id,
            &DeployPhaseId::new("web"),
        )
        .await
        .expect("get web phase")
        .expect("web phase");
    assert_eq!(web_phase.commit_deploy_id(), None);
}

#[tokio::test]
async fn apply_failure_after_end_of_deploy_phase_fails_pending_phase() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = end_of_deploy_db_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_start_service: Some("web".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("web startup should fail after db phase work");

    assert!(
        error
            .to_string()
            .contains("injected start failure for 'web'")
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
        Some("injected start failure for 'web'"),
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "web",
        "failed",
        Some("injected start failure for 'web'"),
    )
    .await;
}

#[tokio::test]
async fn apply_failure_after_end_of_deploy_phase_fails_prior_pending_phases() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let mut manifest = test_manifest(vec![
        test_service_spec("db", Placement::replicated(1), "postgres:17"),
        test_service_spec("web", Placement::replicated(1), "nginx:1.27"),
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
        ],
    });
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController {
        fail_start_service: Some("web".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("web startup should fail after db phase work");

    assert!(
        error
            .to_string()
            .contains("injected start failure for 'web'")
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
        Some("injected start failure for 'web'"),
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "web",
        "failed",
        Some("injected start failure for 'web'"),
    )
    .await;
}

#[tokio::test]
async fn apply_running_phase_record_failure_marks_current_and_unstarted_phases_failed() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = end_of_deploy_db_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    backend.fail_running_phase_upsert(true);
    let factory = FakeParticipantClient::new(FakeController::default());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("running phase record failure should abort deploy");

    assert!(error.to_string().contains("injected running phase failure"));
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
        Some("injected running phase failure"),
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "web",
        "failed",
        Some("injected running phase failure"),
    )
    .await;
}

#[tokio::test]
async fn apply_pending_phase_seed_failure_marks_already_written_phases_failed() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = end_of_deploy_db_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    backend.fail_pending_phase_upsert_on(2);
    let factory = FakeParticipantClient::new(FakeController::default());

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("pending phase seed failure should abort deploy");

    assert!(error.to_string().contains("injected pending phase failure"));
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
        Some("injected pending phase failure"),
    )
    .await;
    assert!(
        store
            .get_deploy_phase(
                &manifest.namespace,
                &last_update.deploy_id,
                &DeployPhaseId::new("web")
            )
            .await
            .expect("get web phase")
            .is_none(),
        "phase whose initial pending write failed should not have a phase record"
    );
}

#[tokio::test]
async fn apply_checkpoint_commit_failure_fails_prior_end_of_deploy_phase() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = end_of_deploy_db_checkpoint_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeParticipantClient::new(FakeController::default());
    backend.fail_commit(true);

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("checkpoint commit failure should fail deploy");

    assert!(error.to_string().contains("injected commit failure"));
    assert_eq!(backend.commit_count(), 1);
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
        Some("injected commit failure"),
    )
    .await;
    assert_phase_record_state(
        &store,
        &manifest.namespace,
        &last_update.deploy_id,
        "web",
        "failed",
        Some("injected commit failure"),
    )
    .await;
}

#[tokio::test]
async fn apply_checkpointed_service_commits_changed_mounted_volume() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let manifest = checkpointed_db_volume_web_manifest();
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let db_phase = initial_plan
        .fingerprint()
        .phases
        .into_iter()
        .find(|phase| phase.phase_id == DeployPhaseId::new("db"))
        .expect("db phase");
    assert!(
        db_phase
            .work
            .iter()
            .any(|work| matches!(work, DeployPhaseWork::Volume { volume, .. } if volume == "data")),
        "changed mounted volume should be owned by the checkpointed service phase"
    );
    let factory = FakeParticipantClient::new(FakeController {
        fail_start_service: Some("web".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("web startup should fail after db checkpoint");

    assert!(
        error
            .to_string()
            .contains("injected start failure for 'web'")
    );
    assert_eq!(backend.commit_count(), 1);
    let volume = store
        .get_volume(&manifest.namespace, "data")
        .await
        .expect("get volume")
        .expect("checkpointed volume record");
    assert_eq!(volume.attached_services, vec!["db"]);
    let releases = store
        .list_service_releases(&manifest.namespace)
        .await
        .expect("list releases");
    assert!(
        releases.iter().any(|release| release.service == "db"),
        "checkpointed db release must be committed with its volume"
    );
}

#[tokio::test]
async fn prepared_deploy_builds_applying_record_and_revisions() {
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
        local_machine_id.clone(),
        plan,
        Vec::new(),
    )
    .expect("prepared deploy");

    assert_eq!(
        prepared.applying_record().state(),
        crate::model::DeployState::Applying
    );
    assert_eq!(prepared.applying_record().started_at, 10);
    assert_eq!(prepared.applying_record().committed_at(), None);
    assert_eq!(prepared.revisions().len(), 1);
    let [revision] = prepared.revisions() else {
        panic!("expected one revision");
    };
    assert_eq!(revision.service, "api");
    assert_eq!(revision.created_by, local_machine_id);
}
