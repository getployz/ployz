use super::*;

#[tokio::test]
async fn resolve_plan_includes_branch_source_preview_evidence() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    let source_spec = test_service_spec("web", Placement::replicated(1), "nginx:1.27");
    let source_revision_hash = source_spec.revision_hash().expect("source revision hash");
    seed_committed_service_release(&store, &source_namespace, source_spec).await;

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

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve branch plan");
    let preview = plan.to_preview(Vec::new());

    assert_eq!(preview.namespace, Namespace::new("pr-39"));
    assert_eq!(preview.services[0].action, DeployChangeKind::Create);
    let [source] = preview.service_sources.as_slice() else {
        panic!("expected one service source");
    };
    assert_eq!(source.service, "web");
    assert_eq!(
        source.mode,
        ServiceSourceMode::Branch {
            source_namespace: source_namespace.clone(),
            source_service: "web".into(),
            source_revision_hash: source_revision_hash.clone()
        }
    );
    assert_eq!(
        preview.service_source_fingerprint,
        plan.service_source_fingerprint()
    );
    assert_eq!(preview.baseline, Some(plan.baseline()));
    assert_eq!(preview.service_branch_sources.len(), 1);
    assert_eq!(preview.service_branch_sources[0].service, "web");
    assert_eq!(
        preview.service_branch_sources[0].source_revision_hash,
        source_revision_hash
    );
    let [phase] = preview.phases.as_slice() else {
        panic!("expected one default deploy phase");
    };
    assert!(matches!(
        phase.work.as_slice(),
        [DeployPhaseWork::Service { service, action }]
            if service == "web" && *action == DeployChangeKind::Create
    ));
    assert_eq!(
        plan.fingerprint().services[0]
            .branch_source()
            .map(|source| source.source_revision_hash.as_str()),
        Some(source_revision_hash.as_str())
    );
}

#[tokio::test]
async fn resolve_plan_rejects_missing_branch_source_release() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
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
                source_namespace: Namespace::new("prod"),
                source_service: "web".into(),
                expected_source_revision_hash: None,
            },
        }],
        volumes: Vec::new(),
        phases: Vec::new(),
    });

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("missing source release should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::BranchSourceMissingRelease {
            namespace: "prod".into(),
            service: "web".into()
        })
    );
}

#[tokio::test]
async fn resolve_plan_rejects_branch_source_missing_revision() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    store
        .upsert_service_release(&test_release(
            &source_namespace,
            "web",
            "missing-source-rev",
            Vec::new(),
        ))
        .await
        .expect("seed source release without revision");
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

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("missing source revision should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::BranchSourceMissingRevision {
            namespace: "prod".into(),
            service: "web".into(),
            revision_hash: "missing-source-rev".into()
        })
    );
}

#[tokio::test]
async fn resolve_plan_rejects_branch_source_revision_drift() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    let source_spec = test_service_spec("web", Placement::replicated(1), "nginx:1.27");
    let actual_revision_hash = source_spec.revision_hash().expect("source revision hash");
    seed_committed_service_release(&store, &source_namespace, source_spec).await;
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
                expected_source_revision_hash: Some("older-revision".into()),
            },
        }],
        volumes: Vec::new(),
        phases: Vec::new(),
    });

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("source revision drift should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::BranchSourceRevisionChanged {
            namespace: "prod".into(),
            service: "web".into(),
            expected_revision_hash: "older-revision".into(),
            actual_revision_hash,
        })
    );
}

#[tokio::test]
async fn resolve_plan_rejects_undecodable_branch_source_revision() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    store
        .commit_deploy(&DeployCommit {
            namespace: source_namespace.clone(),
            revisions: vec![ServiceRevisionRecord {
                namespace: source_namespace.clone(),
                service: "web".into(),
                revision_hash: "source-rev".into(),
                spec_json: "{not-json".into(),
                created_by: MachineId::new("seed"),
                created_at: 0,
            }],
            removed_services: Vec::new(),
            removed_volumes: Vec::new(),
            branch_lineage: Vec::new(),
            volume_movements: Vec::new(),
            volume_branches: Vec::new(),
            phase_commits: Vec::new(),
            releases: vec![test_release(
                &source_namespace,
                "web",
                "source-rev",
                Vec::new(),
            )],
            volumes: Vec::new(),
            deploy: test_deploy_record(&source_namespace, "seed-deploy"),
        })
        .await
        .expect("seed undecodable source revision");
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

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("undecodable source revision should fail");

    assert!(matches!(
        error,
        Error::Deploy(DeployError::BranchSourceSpecDecode {
            namespace,
            service,
            ..
        }) if namespace == "prod" && service == "web"
    ));
}

#[tokio::test]
async fn resolve_plan_rejects_branch_source_same_as_target() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
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
                source_namespace: Namespace::new("pr-39"),
                source_service: "web".into(),
                expected_source_revision_hash: None,
            },
        }],
        volumes: Vec::new(),
        phases: Vec::new(),
    });

    let error = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect_err("same-target branch source should fail");

    assert_eq!(
        error,
        Error::Deploy(DeployError::BranchSourceIsTarget {
            namespace: "pr-39".into(),
            service: "web".into()
        })
    );
}

#[tokio::test]
async fn ensure_plan_stable_rejects_branch_source_revision_drift() {
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

    let expected_baseline = initial_plan.baseline();
    let error = ensure_plan_stable(&initial_plan, &final_plan, Some(&expected_baseline))
        .expect_err("source revision drift should fail");
    let Error::Deploy(DeployError::DeployBaselineChanged { diff }) = error else {
        panic!("expected deploy baseline changed");
    };
    assert!(
        diff.changed_components()
            .contains(&DeployBaselineComponent::ServiceSources)
    );
    assert_ne!(
        initial_plan.service_source_fingerprint(),
        final_plan.service_source_fingerprint()
    );
}

#[tokio::test]
async fn apply_accepts_matching_service_source_baseline_and_commits_branch_lineage() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    let source_spec = test_service_spec("web", Placement::replicated(1), "nginx:1.27");
    let source_revision_hash = source_spec.revision_hash().expect("source revision");
    seed_committed_service_release(&store, &source_namespace, source_spec).await;

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

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("branch plan");
    let expected_baseline = plan.baseline();
    let participant_client = FakeParticipantClient::new(FakeController::default());
    let result = apply_with_deploy_id_and_preconditions(
        &store,
        &participant_client,
        &local_machine_id,
        &manifest,
        DeployId::new("deploy-branch-baseline"),
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
    .expect("matching baseline apply");

    assert_eq!(result.state, crate::model::DeployState::Committed);
    let lineage = store
        .list_service_branch_lineage(&manifest.namespace)
        .await
        .expect("branch lineage");
    let [lineage] = lineage.as_slice() else {
        panic!("expected one branch lineage record");
    };
    assert_eq!(lineage.source_namespace, source_namespace);
    assert_eq!(lineage.source_service, "web");
    assert_eq!(lineage.source_revision_hash, source_revision_hash);
}

#[tokio::test]
async fn apply_commits_branch_lineage_when_only_source_revision_changes() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    let source_spec = test_service_spec("web", Placement::replicated(1), "nginx:1.28");
    let source_revision_hash = source_spec.revision_hash().expect("source revision");
    seed_committed_service_release(&store, &source_namespace, source_spec).await;

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
    let target_revision_hash = manifest.services[0]
        .revision_hash()
        .expect("target revision");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "web",
            &target_revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-existing",
                &target_revision_hash,
            )],
        ))
        .await
        .expect("seed target release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("branch plan");
    let [service] = plan.services() else {
        panic!("expected one service");
    };
    assert_eq!(service.action(), DeployChangeKind::Unchanged);
    let expected_baseline = plan.baseline();
    let controller = FakeController::default();
    let participant_client = FakeParticipantClient::new(controller.clone());

    apply_with_deploy_id_and_preconditions(
        &store,
        &participant_client,
        &local_machine_id,
        &manifest,
        DeployId::new("deploy-branch-lineage-only"),
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
    .expect("matching baseline apply");

    assert_eq!(controller.start_count(), 0);
    let lineage = store
        .list_service_branch_lineage(&manifest.namespace)
        .await
        .expect("branch lineage");
    let [lineage] = lineage.as_slice() else {
        panic!("expected one branch lineage record");
    };
    assert_eq!(lineage.revision_hash, target_revision_hash);
    assert_eq!(lineage.source_namespace, source_namespace);
    assert_eq!(lineage.source_service, "web");
    assert_eq!(lineage.source_revision_hash, source_revision_hash);
}

#[tokio::test]
async fn apply_preserves_checkpointed_branch_lineage_after_final_commit() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    let source_spec = test_service_spec("db", Placement::replicated(1), "postgres:16");
    let source_revision_hash = source_spec.revision_hash().expect("source revision");
    seed_committed_service_release(&store, &source_namespace, source_spec).await;

    let mut manifest = checkpointed_db_web_manifest();
    manifest.namespace = Namespace::new("pr-39");
    let intent = manifest.intent.as_mut().expect("checkpointed intent");
    intent.services = vec![ServiceIntentHint {
        service: "db".into(),
        intent: ServiceIntent::Branch {
            source_namespace: source_namespace.clone(),
            source_service: "db".into(),
            expected_source_revision_hash: None,
        },
    }];

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("branch checkpoint plan");
    let expected_baseline = plan.baseline();
    let participant_client = FakeParticipantClient::new(FakeController::default());
    let result = apply_with_deploy_id_and_preconditions(
        &store,
        &participant_client,
        &local_machine_id,
        &manifest,
        DeployId::new("deploy-checkpoint-branch-baseline"),
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
    .expect("matching baseline checkpoint apply");

    assert_eq!(result.state, crate::model::DeployState::Committed);
    let lineage = store
        .list_service_branch_lineage(&manifest.namespace)
        .await
        .expect("branch lineage");
    let [lineage] = lineage.as_slice() else {
        panic!("expected checkpointed branch lineage record");
    };
    assert_eq!(lineage.service, "db");
    assert_eq!(lineage.source_namespace, source_namespace);
    assert_eq!(lineage.source_service, "db");
    assert_eq!(lineage.source_revision_hash, source_revision_hash);
    assert_eq!(lineage.deploy_id, result.deploy_id);
}

#[tokio::test]
async fn commit_plan_contains_branch_lineage() {
    let store = seeded_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId::new("local");
    let source_namespace = Namespace::new("prod");
    let source_spec = test_service_spec("web", Placement::replicated(1), "nginx:1.27");
    let source_revision_hash = source_spec.revision_hash().expect("source revision hash");
    seed_committed_service_release(&store, &source_namespace, source_spec).await;

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
    let target_revision_hash = manifest.services[0]
        .revision_hash()
        .expect("target revision");
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve branch plan");
    let slot = plan
        .services()
        .iter()
        .find(|service| service.service == "web")
        .and_then(|service| service.slots.first().map(|slot| slot.slot_id().clone()))
        .expect("web slot");
    let prepared = PreparedDeploy::new(
        DeployId::new("deploy-branch"),
        10,
        local_machine_id,
        plan,
        Vec::new(),
    )
    .expect("prepared deploy");
    let started = HashMap::from([(
        (String::from("web"), slot.as_str().to_owned()),
        test_instance_status(
            &manifest.namespace,
            "web",
            slot.as_str(),
            "machine-a",
            "web-inst-1",
            &target_revision_hash,
        ),
    )]);

    let commit_plan = prepared
        .into_started(started)
        .into_commit_plan(Vec::new(), Vec::new())
        .expect("commit plan");

    assert_eq!(commit_plan.commit().branch_lineage.len(), 1);
    let lineage = &commit_plan.commit().branch_lineage[0];
    assert_eq!(lineage.namespace, Namespace::new("pr-39"));
    assert_eq!(lineage.service, "web");
    assert_eq!(lineage.revision_hash, target_revision_hash);
    assert_eq!(lineage.source_namespace, source_namespace);
    assert_eq!(lineage.source_service, "web");
    assert_eq!(lineage.source_revision_hash, source_revision_hash);
    assert_eq!(lineage.deploy_id, DeployId::new("deploy-branch"));
}
