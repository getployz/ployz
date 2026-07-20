use super::*;

#[test]
fn transitive_later_phase_pin_places_the_whole_component() {
    let candidates = [machine_id("machine_a"), machine_id("machine_b")];
    let mut api = planning_input(1, candidates.clone());
    declare_plain_volume_mounts(&mut api, vec![volume_mount("shared_a", "/api")]);
    let mut worker = planning_input(1, candidates.clone());
    update_request(&mut worker.service, |request| {
        request.service_id = service_id("svc_worker");
        request.depends_on = vec![dependency("svc_api", DependencyCondition::Started)];
    });
    declare_plain_volume_mounts(
        &mut worker,
        vec![
            volume_mount("shared_a", "/input"),
            volume_mount("shared_b", "/output"),
        ],
    );
    let mut archive = planning_input(1, candidates);
    update_request(&mut archive.service, |request| {
        request.service_id = service_id("svc_archive");
        request.depends_on = vec![dependency("svc_worker", DependencyCondition::Started)];
    });
    declare_plain_volume_mounts(
        &mut archive,
        vec![
            volume_mount("shared_b", "/input"),
            volume_mount("archive_data", "/archive"),
        ],
    );
    archive.volume_pins = vec![volume_pin("archive_data", "machine_b")];

    let plan = plan_inputs(vec![api, worker, archive], Vec::new())
        .expect("transitive durable colocation places the full component");

    assert_eq!(plan.target_machines(), vec![machine_id("machine_b")]);
    assert_eq!(
        plan.volume_pin_commits,
        vec![
            volume_pin("shared_a", "machine_b"),
            volume_pin("shared_b", "machine_b"),
        ]
    );
}
#[test]
fn transitive_pinned_conflict_precedes_unrelated_live_admission() {
    let mut live = planning_input(1, [machine_id("machine_silent")]);
    declare_provisioned_volume(&mut live, "live_data", "/data", 600);
    live.storage_testimony = BTreeMap::new();
    let candidates = [machine_id("machine_a"), machine_id("machine_b")];
    let mut left = planning_input(1, candidates.clone());
    update_request(&mut left.service, |request| {
        request.service_id = service_id("svc_left")
    });
    declare_plain_volume_mounts(
        &mut left,
        vec![
            volume_mount("left_pin", "/left"),
            volume_mount("shared_a", "/shared"),
        ],
    );
    left.volume_pins = vec![volume_pin("left_pin", "machine_a")];
    let mut bridge = planning_input(1, candidates.clone());
    update_request(&mut bridge.service, |request| {
        request.service_id = service_id("svc_middle")
    });
    declare_plain_volume_mounts(
        &mut bridge,
        vec![
            volume_mount("shared_a", "/input"),
            volume_mount("shared_b", "/output"),
        ],
    );
    let mut right = planning_input(1, candidates);
    update_request(&mut right.service, |request| {
        request.service_id = service_id("svc_right")
    });
    declare_plain_volume_mounts(
        &mut right,
        vec![
            volume_mount("right_pin", "/right"),
            volume_mount("shared_b", "/shared"),
        ],
    );
    right.volume_pins = vec![volume_pin("right_pin", "machine_b")];

    assert_eq!(
        plan_inputs(vec![live, left, bridge, right], Vec::new()),
        Err(DeployPlanError::ConflictingVolumePins {
            service_id: service_id("svc_right"),
            machines: vec![machine_id("machine_a"), machine_id("machine_b")],
        })
    );
}
#[test]
fn volume_backed_service_pins_to_first_eligible_machine() {
    let mut input = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![volume_mount("postgres_data", "/var/lib/postgres")],
    );

    assert_eq!(
        plan_single_service(&input).expect("plan succeeds"),
        deploy_plan_with_volume_pins(
            &input,
            vec![run_step("machine_a", 1)],
            vec![volume_pin("postgres_data", "machine_a")],
            Vec::new(),
        )
    );
}

#[test]
fn duplicate_plain_volume_mounts_emit_one_pin_commit() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![
            volume_mount("data", "/data"),
            volume_mount("data", "/var/lib/data"),
        ],
    );

    assert_eq!(
        plan_single_service(&input).expect("duplicate mounts share one placement pin"),
        deploy_plan_with_volume_pins(
            &input,
            vec![run_step("machine_a", 1)],
            vec![volume_pin("data", "machine_a")],
            Vec::new(),
        )
    );
}

#[test]
fn new_plain_volume_pin_commits_are_sorted_by_volume_name() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![
            volume_mount("uploads", "/uploads"),
            volume_mount("data", "/data"),
        ],
    );

    assert_eq!(
        plan_single_service(&input).expect("new volume pins are deterministic"),
        deploy_plan_with_volume_pins(
            &input,
            vec![run_step("machine_a", 1)],
            vec![
                volume_pin("data", "machine_a"),
                volume_pin("uploads", "machine_a"),
            ],
            Vec::new(),
        )
    );
}

#[test]
fn provisioned_volume_birth_commits_and_ensures_the_same_dataset_backed_pin() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_provisioned_volume(&mut input, "data", "/data", 1024);

    let plan = plan_single_service(&input).expect("provisioned birth is admitted");
    let pin = provisioned_volume_pin("data", "machine_a");
    assert_eq!(plan.volume_pin_commits, vec![pin.clone()]);
    assert_eq!(plan.volume_ensures, vec![pin]);
}

#[test]
fn provisioned_volume_birth_skips_a_full_machine_for_later_capacity() {
    let mut input = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_provisioned_volume(&mut input, "data", "/data", 600);
    input.storage_testimony = BTreeMap::from([
        (
            machine_id("machine_a"),
            Some(ready_storage_with_capacity(500, Vec::new())),
        ),
        (
            machine_id("machine_b"),
            Some(ready_storage_with_capacity(1_000, Vec::new())),
        ),
    ]);

    let plan = plan_single_service(&input).expect("later capacity admits the volume birth");

    assert_eq!(plan.target_machines(), vec![machine_id("machine_b")]);
    assert_eq!(
        plan.volume_pin_commits,
        vec![provisioned_volume_pin_with_size("data", "machine_b", 600)]
    );
    assert_eq!(plan.volume_pin_commits, plan.volume_ensures);
}

#[test]
fn provisioned_volume_birth_skips_silent_and_unprepared_machines() {
    let mut input = planning_input(
        1,
        [
            machine_id("machine_silent"),
            machine_id("machine_unprepared"),
            machine_id("machine_ready"),
        ],
    );
    declare_provisioned_volume(&mut input, "data", "/data", 600);
    input.storage_testimony = BTreeMap::from([
        (
            machine_id("machine_unprepared"),
            Some(ployz_core::machine::StorageCapability::Unprepared),
        ),
        (
            machine_id("machine_ready"),
            Some(ready_storage_with_capacity(1_000, Vec::new())),
        ),
    ]);

    let plan = plan_single_service(&input).expect("later ready testimony admits the volume birth");

    assert_eq!(plan.target_machines(), vec![machine_id("machine_ready")]);
}

#[test]
fn provisioned_volume_birth_returns_the_first_candidate_failure_when_all_reject() {
    let mut input = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_provisioned_volume(&mut input, "data", "/data", 600);
    input.storage_testimony = BTreeMap::from([(
        machine_id("machine_b"),
        Some(ployz_core::machine::StorageCapability::Unprepared),
    )]);

    assert!(matches!(
        plan_single_service(&input),
        Err(DeployPlanError::VolumeAdmissionOnMachine {
            service_id: failed_service,
            machine_id: failed_machine,
            failure,
        }) if failed_service == service_id("svc_api")
            && failed_machine == machine_id("machine_a")
            && matches!(failure.as_ref(), VolumeAdmissionFailure::MachineSilent {
                machine_id: silent_machine,
            } if silent_machine == &machine_id("machine_a"))
    ));
}

#[test]
fn provisioned_volume_birth_without_candidates_retains_no_eligible_machines() {
    let mut input = planning_input(1, []);
    declare_provisioned_volume(&mut input, "data", "/data", 600);

    assert_eq!(
        plan_single_service(&input),
        Err(DeployPlanError::NoEligibleMachines {
            service_id: service_id("svc_api"),
        })
    );
}

#[test]
fn provisioned_pin_is_ensured_even_when_live_testimony_omits_its_dataset() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_provisioned_volume(&mut input, "data", "/data", 1024);
    input.volume_pins = vec![provisioned_volume_pin("data", "machine_a")];
    input.storage_testimony = BTreeMap::from([(
        machine_id("machine_a"),
        Some(ready_storage_with_capacity(1024 * 1024, Vec::new())),
    )]);

    assert_eq!(
        plan_single_service(&input).expect("dataset-backed pin is placeable"),
        deploy_plan(&input, vec![run_step("machine_a", 1)], Vec::new())
    );
}

#[test]
fn plain_declaration_rejects_a_provisioned_pin() {
    let mut input = planning_input(1, [machine_id("machine_b")]);
    declare_plain_volume_mounts(&mut input, vec![volume_mount("data", "/data")]);
    input.volume_pins = vec![provisioned_volume_pin("data", "machine_a")];

    assert!(matches!(
        plan_single_service(&input),
        Err(DeployPlanError::VolumeAdmissionOnMachine {
            machine_id: selected_machine_id,
            failure,
            ..
        })
        if selected_machine_id == machine_id("machine_a")
            && matches!(failure.as_ref(), VolumeAdmissionFailure::KindConversion {
                declaration: VolumeSpec::Plain,
                pin_kind: ployz_core::intent::VolumeKind::Provisioned { .. },
                ..
            })
    ));
}

#[test]
fn provisioned_declaration_rejects_a_plain_pin() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_provisioned_volume(&mut input, "data", "/data", 1024);
    input.volume_pins = vec![volume_pin("data", "machine_a")];

    assert!(matches!(
        plan_single_service(&input),
        Err(DeployPlanError::VolumeAdmissionOnMachine {
            machine_id: selected_machine_id,
            failure,
            ..
        })
        if selected_machine_id == machine_id("machine_a")
            && matches!(failure.as_ref(), VolumeAdmissionFailure::KindConversion {
                declaration: VolumeSpec::Provisioned { .. },
                pin_kind: ployz_core::intent::VolumeKind::Plain,
                ..
            })
    ));
}

#[test]
fn duplicate_pins_for_one_volume_fail_as_ambiguous_before_placement() {
    let mut input = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(&mut input, vec![volume_mount("data", "/data")]);
    input.volume_pins = vec![
        volume_pin("data", "machine_a"),
        volume_pin("data", "machine_b"),
    ];

    assert_eq!(
        plan_single_service(&input),
        Err(DeployPlanError::VolumeAdmission {
            service_id: service_id("svc_api"),
            failure: VolumeAdmissionFailure::AmbiguousPins {
                volume_name: VolumeName::try_new("data").expect("volume name"),
                pin_count: 2,
            },
        })
    );
}

#[test]
fn provisioned_growth_commits_and_ensures_the_replacement_pin() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_provisioned_volume(&mut input, "data", "/data", 2048);
    input.volume_pins = vec![provisioned_volume_pin("data", "machine_a")];
    input.storage_testimony = BTreeMap::from([(
        machine_id("machine_a"),
        Some(ready_storage_with_quota("data", 1024)),
    )]);

    let plan = plan_single_service(&input).expect("growth is admitted");
    let replacement = provisioned_volume_pin_with_size("data", "machine_a", 2048);
    assert_eq!(plan.volume_pin_commits, vec![replacement.clone()]);
    assert_eq!(plan.volume_ensures, vec![replacement]);
}

#[test]
fn provisioned_declaration_rejects_shrinking_a_pinned_maximum() {
    let mut input = planning_input(1, [machine_id("machine_b")]);
    declare_provisioned_volume(&mut input, "data", "/data", 512);
    input.volume_pins = vec![provisioned_volume_pin("data", "machine_a")];

    assert_eq!(
        plan_single_service(&input),
        Err(DeployPlanError::VolumeAdmissionOnMachine {
            service_id: service_id("svc_api"),
            machine_id: machine_id("machine_a"),
            failure: Box::new(VolumeAdmissionFailure::QuotaShrink {
                volume_name: VolumeName::try_new("data").expect("volume name"),
                declared_max_size_bytes: VolumeMaxSizeBytes::try_new(512).expect("non-zero size"),
                pinned_max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("non-zero size"),
            }),
        })
    );
}

#[test]
fn volume_backed_service_uses_existing_pin() {
    let mut input = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![volume_mount("postgres_data", "/var/lib/postgres")],
    );
    input.volume_pins = vec![volume_pin("postgres_data", "machine_b")];

    assert_eq!(
        plan_single_service(&input).expect("plan succeeds"),
        deploy_plan(&input, vec![run_step("machine_b", 1)], Vec::new())
    );
}

#[test]
fn volume_backed_service_fails_when_existing_pin_is_not_eligible() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![volume_mount("postgres_data", "/var/lib/postgres")],
    );
    input.volume_pins = vec![volume_pin("postgres_data", "machine_b")];

    assert_eq!(
        plan_single_service(&input),
        Err(DeployPlanError::NoEligibleMachines {
            service_id: service_id("svc_api"),
        })
    );
}

#[test]
fn volume_backed_service_reuses_only_the_container_on_the_pinned_machine() {
    let mut input = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![volume_mount("postgres_data", "/var/lib/postgres")],
    );
    input.volume_pins = vec![volume_pin("postgres_data", "machine_b")];
    input.existing_replicas = vec![
        existing_replica("machine_a", "ctr_off_pin"),
        existing_replica("machine_b", "ctr_pinned"),
    ];
    input.cleanup_candidates = vec![
        cleanup_container_observed("machine_a", "ctr_off_pin", true, None),
        cleanup_container_observed("machine_b", "ctr_pinned", true, None),
    ];

    assert_eq!(
        plan_single_service(&input).expect("plan succeeds"),
        deploy_plan(
            &input,
            vec![use_existing_step("machine_b", "ctr_pinned", 1)],
            vec![cleanup_container("machine_a", "ctr_off_pin")],
        )
    );
}

#[test]
fn later_pinned_service_constrains_an_earlier_shared_volume_assignment() {
    let mut first = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(&mut first, vec![volume_mount("data", "/data")]);
    let mut second = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    update_request(&mut second.service, |request| {
        request.service_id = service_id("svc_worker");
    });
    declare_plain_volume_mounts(
        &mut second,
        vec![
            volume_mount("data", "/data"),
            volume_mount("uploads", "/uploads"),
        ],
    );
    second.volume_pins = vec![volume_pin("uploads", "machine_b")];

    let plan = plan_inputs(vec![first, second], Vec::new())
        .expect("durable colocation applies before tentative placement");

    assert_eq!(plan.target_machines(), vec![machine_id("machine_b")]);
    assert_eq!(
        plan.volume_pin_commits,
        vec![volume_pin("data", "machine_b")]
    );
}

#[test]
fn later_phase_pin_constrains_an_earlier_phase_shared_volume() {
    let mut api = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(&mut api, vec![volume_mount("shared", "/data")]);
    let mut worker = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    update_request(&mut worker.service, |request| {
        request.service_id = service_id("svc_worker");
        request.depends_on = vec![dependency("svc_api", DependencyCondition::Started)];
    });
    declare_plain_volume_mounts(
        &mut worker,
        vec![
            volume_mount("shared", "/work"),
            volume_mount("worker_data", "/state"),
        ],
    );
    worker.volume_pins = vec![volume_pin("worker_data", "machine_b")];

    let plan = plan_inputs(vec![api, worker], Vec::new())
        .expect("later phase durable colocation precedes tentative placement");

    assert_eq!(plan.target_machines(), vec![machine_id("machine_b")]);
    assert_eq!(
        plan.volume_pin_commits,
        vec![volume_pin("shared", "machine_b")]
    );
}

#[test]
fn multi_service_redeploy_canonicalizes_identical_pin_fan_out() {
    let pin = volume_pin("data", "machine_a");
    let mut api = planning_input(1, [machine_id("machine_a")]);
    declare_plain_volume_mounts(&mut api, vec![volume_mount("data", "/data")]);
    api.volume_pins = vec![pin.clone()];
    let mut worker = planning_input(1, [machine_id("machine_a")]);
    update_request(&mut worker.service, |request| {
        request.service_id = service_id("svc_worker");
    });
    declare_plain_volume_mounts(&mut worker, vec![volume_mount("data", "/work")]);
    worker.volume_pins = vec![pin];

    let plan = plan_inputs(vec![api, worker], Vec::new())
        .expect("identical snapshots from each service are one durable pin");

    assert!(plan.volume_pin_commits.is_empty());
    assert_eq!(plan.target_machines(), vec![machine_id("machine_a")]);
}

#[test]
fn other_namespace_pin_does_not_assign_an_unpinned_current_namespace_volume() {
    let mut input = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(&mut input, vec![volume_mount("data", "/data")]);
    input.volume_pins = vec![VolumePinState::plain(
        namespace_id("other_namespace"),
        VolumeName::try_new("data").expect("volume name"),
        machine_id("machine_b"),
    )];

    let plan = plan_single_service(&input).expect("other namespace pin is irrelevant");

    assert_eq!(plan.target_machines(), vec![machine_id("machine_a")]);
    assert_eq!(
        plan.volume_pin_commits,
        vec![volume_pin("data", "machine_a")]
    );
}

#[test]
fn multi_service_provisioned_births_share_one_machine_capacity_snapshot() {
    let mut api = planning_input(1, [machine_id("machine_a")]);
    declare_provisioned_volume(&mut api, "api_data", "/data", 600);
    let mut worker = planning_input(1, [machine_id("machine_a")]);
    update_request(&mut worker.service, |request| {
        request.service_id = service_id("svc_worker");
    });
    declare_provisioned_volume(&mut worker, "worker_data", "/work", 600);
    let testimony = BTreeMap::from([(
        machine_id("machine_a"),
        Some(ready_storage_with_capacity(1_000, Vec::new())),
    )]);
    api.storage_testimony = testimony.clone();
    worker.storage_testimony = testimony;

    assert!(matches!(
        plan_inputs(vec![api, worker], Vec::new()),
        Err(DeployPlanError::VolumeAdmissionOnMachine {
            machine_id: selected_machine_id,
            failure,
            ..
        })
        if selected_machine_id == machine_id("machine_a")
            && matches!(failure.as_ref(), VolumeAdmissionFailure::CapacityExceeded {
                total_bytes: 1_000,
                provisioned_used_bytes: 0,
                free_bytes: 1_000,
                required_headroom_bytes: 1_200,
                requested_total_bytes: 1_200,
            })
    ));
}

#[test]
fn provisioned_births_reserve_tentative_quota_before_later_placement() {
    let candidates = [machine_id("machine_a"), machine_id("machine_b")];
    let mut api = planning_input(1, candidates.clone());
    declare_provisioned_volume(&mut api, "api_data", "/data", 600);
    let mut worker = planning_input(1, candidates);
    update_request(&mut worker.service, |request| {
        request.service_id = service_id("svc_worker");
    });
    declare_provisioned_volume(&mut worker, "worker_data", "/work", 600);
    let testimony = BTreeMap::from([
        (
            machine_id("machine_a"),
            Some(ready_storage_with_capacity(1_000, Vec::new())),
        ),
        (
            machine_id("machine_b"),
            Some(ready_storage_with_capacity(1_000, Vec::new())),
        ),
    ]);
    api.storage_testimony = testimony.clone();
    worker.storage_testimony = testimony;

    let plan = plan_inputs(vec![api, worker], Vec::new())
        .expect("later birth uses capacity not reserved by the earlier birth");

    assert_eq!(
        plan.volume_pin_commits,
        vec![
            provisioned_volume_pin_with_size("api_data", "machine_a", 600),
            provisioned_volume_pin_with_size("worker_data", "machine_b", 600),
        ]
    );
}

#[test]
fn unpinned_birth_accounts_for_later_phase_pinned_quota_growth() {
    let candidates = [machine_id("machine_a"), machine_id("machine_b")];
    let mut api = planning_input(1, candidates.clone());
    declare_provisioned_volume(&mut api, "api_data", "/data", 400);
    let mut worker = planning_input(1, candidates);
    update_request(&mut worker.service, |request| {
        request.service_id = service_id("svc_worker");
        request.depends_on = vec![dependency("svc_api", DependencyCondition::Started)];
    });
    declare_provisioned_volume(&mut worker, "worker_data", "/work", 700);
    worker.volume_pins = vec![provisioned_volume_pin_with_size(
        "worker_data",
        "machine_a",
        400,
    )];
    let testimony = BTreeMap::from([
        (
            machine_id("machine_a"),
            Some(ready_storage_with_quota_and_capacity(
                "worker_data",
                400,
                1_000,
            )),
        ),
        (
            machine_id("machine_b"),
            Some(ready_storage_with_capacity(1_000, Vec::new())),
        ),
    ]);
    api.storage_testimony = testimony.clone();
    worker.storage_testimony = testimony;

    let plan = plan_inputs(vec![api, worker], Vec::new())
        .expect("unpinned birth avoids capacity reserved by a later pinned phase");

    assert_eq!(
        plan.volume_pin_commits,
        vec![
            provisioned_volume_pin_with_size("api_data", "machine_b", 400),
            provisioned_volume_pin_with_size("worker_data", "machine_a", 700),
        ]
    );
}

#[test]
fn provisioned_durable_pin_never_moves_to_later_capacity() {
    let mut input = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_provisioned_volume(&mut input, "data", "/data", 600);
    input.volume_pins = vec![provisioned_volume_pin_with_size("data", "machine_a", 400)];
    input.storage_testimony = BTreeMap::from([
        (
            machine_id("machine_a"),
            Some(ready_storage_with_quota_and_capacity("data", 400, 500)),
        ),
        (
            machine_id("machine_b"),
            Some(ready_storage_with_capacity(1_000, Vec::new())),
        ),
    ]);

    assert!(matches!(
        plan_single_service(&input),
        Err(DeployPlanError::VolumeAdmissionOnMachine {
            machine_id: selected_machine_id,
            failure,
            ..
        }) if selected_machine_id == machine_id("machine_a")
            && matches!(failure.as_ref(), VolumeAdmissionFailure::CapacityExceeded { .. })
    ));
}

#[test]
fn admitted_provisioned_birth_reaches_container_planning_with_an_ensure() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_provisioned_volume(&mut input, "data", "/data", 512);

    let plan = plan_single_service(&input).expect("birth reaches container planning");
    assert_eq!(plan.volume_ensures.len(), 1);
    assert_eq!(plan.volume_pin_commits, plan.volume_ensures);
}

#[test]
fn service_with_volumes_on_different_pinned_machines_fails_planning() {
    let mut input = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![
            volume_mount("postgres_data", "/var/lib/postgres"),
            volume_mount("uploads", "/srv/uploads"),
        ],
    );
    input.volume_pins = vec![
        volume_pin("postgres_data", "machine_a"),
        volume_pin("uploads", "machine_b"),
    ];

    assert_eq!(
        plan_single_service(&input),
        Err(DeployPlanError::ConflictingVolumePins {
            service_id: service_id("svc_api"),
            machines: vec![machine_id("machine_a"), machine_id("machine_b")],
        })
    );
}

#[test]
fn unchanged_named_volume_replica_has_no_handoff() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    input.service.runtime.environment =
        runtime_with_env([("DATABASE_PASSWORD", "never-in-plan-evidence")]).environment;
    declare_plain_volume_mounts(&mut input, vec![volume_mount("data", "/data")]);
    input.volume_pins = vec![volume_pin("data", "machine_a")];
    input.existing_replicas = vec![existing_replica("machine_a", "ctr_current")];
    input.cleanup_candidates = vec![cleanup_container_observed(
        "machine_a",
        "ctr_current",
        true,
        None,
    )];

    let plan = plan_single_service(&input).expect("unchanged service plans");
    let [phase] = plan.phases.as_slice() else {
        panic!("one phase")
    };
    let [service] = phase.services.as_slice() else {
        panic!("one service")
    };

    assert!(matches!(service.work, DeployServiceWork::Ordinary { .. }));
    assert!(
        !serde_json::to_string(&plan)
            .expect("plan serializes")
            .contains("never-in-plan-evidence")
    );
}

#[test]
fn adding_a_first_volume_does_not_handoff_an_unobserved_old_container() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_plain_volume_mounts(&mut input, vec![volume_mount("data", "/data")]);
    input.volume_pins = vec![volume_pin("data", "machine_a")];
    input.cleanup_candidates = vec![cleanup_container_observed(
        "machine_a",
        "ctr_without_mount_testimony",
        true,
        None,
    )];

    let plan = plan_single_service(&input).expect("first volume plans");
    let [phase] = plan.phases.as_slice() else {
        panic!("one phase")
    };
    let [service] = phase.services.as_slice() else {
        panic!("one service")
    };
    assert!(matches!(service.work, DeployServiceWork::Ordinary { .. }));
}

#[test]
fn replacing_volume_a_with_b_does_not_handoff_the_old_a_consumer() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_plain_volume_mounts(&mut input, vec![volume_mount("volume_b", "/data")]);
    input.volume_pins = vec![volume_pin("volume_b", "machine_a")];
    input.cleanup_candidates = vec![with_named_volumes(
        cleanup_container_observed("machine_a", "ctr_volume_a", true, None),
        ["volume_a"],
    )];

    let plan = plan_single_service(&input).expect("replacement volume plans");
    let [phase] = plan.phases.as_slice() else {
        panic!("one phase")
    };
    let [service] = phase.services.as_slice() else {
        panic!("one service")
    };
    assert!(matches!(service.work, DeployServiceWork::Ordinary { .. }));
}

#[test]
fn canonical_physical_volume_testimony_drives_only_exact_replacement_handoff() {
    let mut input = planning_input(1, [machine_id("machine_b")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![volume_mount("postgres_data", "/var/lib/postgres")],
    );
    input.volume_pins = vec![volume_pin("postgres_data", "machine_b")];

    let first_deploy = plan_single_service(&input).expect("first deploy plans");
    let [first_phase] = first_deploy.phases.as_slice() else {
        panic!("one phase")
    };
    let [first_service] = first_phase.services.as_slice() else {
        panic!("one service")
    };
    assert!(matches!(
        first_service.work,
        DeployServiceWork::Ordinary { .. }
    ));

    let namespace_id = namespace_id("default");
    let unrelated_physical = VolumeName::try_new("cache")
        .expect("volume")
        .stable_storage_name(&namespace_id);
    let unrelated_logical =
        VolumeName::try_from_stable_storage_name(unrelated_physical, &namespace_id)
            .expect("canonical physical identity decodes");
    let mut unrelated = cleanup_container_observed("machine_b", "ctr_revision_a", true, None);
    unrelated.named_volume_names.insert(unrelated_logical);
    input.cleanup_candidates = vec![unrelated];
    let unrelated_plan = plan_single_service(&input).expect("unrelated volume replacement plans");
    let [unrelated_phase] = unrelated_plan.phases.as_slice() else {
        panic!("one phase")
    };
    let [unrelated_service] = unrelated_phase.services.as_slice() else {
        panic!("one service")
    };
    assert!(matches!(
        unrelated_service.work,
        DeployServiceWork::Ordinary { .. }
    ));

    let postgres_physical = VolumeName::try_new("postgres_data")
        .expect("volume")
        .stable_storage_name(&namespace_id);
    let postgres_logical =
        VolumeName::try_from_stable_storage_name(postgres_physical, &namespace_id)
            .expect("canonical physical identity decodes");
    let mut matching = cleanup_container_observed("machine_b", "ctr_revision_a", true, None);
    matching.named_volume_names.insert(postgres_logical);
    input.cleanup_candidates = vec![matching];
    let replacement = plan_single_service(&input).expect("revision B replacement plans");
    let [replacement_phase] = replacement.phases.as_slice() else {
        panic!("one phase")
    };
    let [replacement_service] = replacement_phase.services.as_slice() else {
        panic!("one service")
    };
    let DeployServiceWork::VolumeHandoff { participants, .. } = &replacement_service.work else {
        panic!("exact physical-to-logical volume identity requires handoff")
    };
    let [participant] = participants.as_slice() else {
        panic!("one handoff participant")
    };
    assert_eq!(
        participant.shared_volume_names.as_slice(),
        &[VolumeName::try_new("postgres_data").expect("volume")]
    );
}

#[test]
fn changed_named_volume_replacement_has_deterministic_exact_handoff_owners() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_plain_volume_mounts(
        &mut input,
        vec![
            volume_mount("uploads", "/uploads"),
            volume_mount("data", "/data"),
            volume_mount("data", "/data-copy"),
        ],
    );
    input.volume_pins = vec![
        volume_pin("uploads", "machine_a"),
        volume_pin("data", "machine_a"),
    ];
    input.cleanup_candidates = vec![
        with_named_volumes(
            cleanup_container_observed("machine_a", "ctr_z", false, None),
            ["uploads"],
        ),
        cleanup_container_observed("machine_b", "ctr_other_machine", true, None),
        with_named_volumes(
            cleanup_container_observed("machine_a", "ctr_a", true, None),
            ["unrelated", "data", "data"],
        ),
        with_named_volumes(
            cleanup_container_observed("machine_a", "ctr_a", true, None),
            ["data"],
        ),
    ];

    let plan = plan_single_service(&input).expect("replacement plans");
    let [phase] = plan.phases.as_slice() else {
        panic!("one phase")
    };
    let [service] = phase.services.as_slice() else {
        panic!("one service")
    };
    let DeployServiceWork::VolumeHandoff {
        replacement,
        remaining_steps,
        participants,
    } = &service.work
    else {
        panic!("same-machine replacement requires handoff")
    };
    assert_eq!(replacement.machine_id, machine_id("machine_a"));
    assert!(remaining_steps.is_empty());
    assert_eq!(
        participants.as_slice(),
        &[
            DeployVolumeHandoffParticipant {
                target: cleanup_container("machine_a", "ctr_a"),
                prior_state: DeployVolumeHandoffPriorState::Running,
                shared_volume_names: ployz_core::deploy::NonEmptyVolumeNames::try_new([
                    VolumeName::try_new("data").expect("volume"),
                ])
                .expect("shared volume"),
            },
            DeployVolumeHandoffParticipant {
                target: cleanup_container("machine_a", "ctr_z"),
                prior_state: DeployVolumeHandoffPriorState::Stopped,
                shared_volume_names: ployz_core::deploy::NonEmptyVolumeNames::try_new([
                    VolumeName::try_new("uploads").expect("volume"),
                ])
                .expect("shared volume"),
            },
        ]
    );
    assert_eq!(
        plan.cleanup_actions
            .iter()
            .filter(|action| action.target().machine_id == machine_id("machine_a"))
            .map(|action| action.target().container_id.clone())
            .collect::<Vec<_>>(),
        vec![container_id("ctr_a"), container_id("ctr_z")]
    );
}

#[test]
fn named_volume_handoff_does_not_cross_service_ownership() {
    let mut api = planning_input(1, [machine_id("machine_a")]);
    declare_plain_volume_mounts(&mut api, vec![volume_mount("shared", "/api")]);
    api.volume_pins = vec![volume_pin("shared", "machine_a")];
    api.cleanup_candidates = vec![with_named_volumes(
        cleanup_container_observed("machine_a", "ctr_api_old", true, None),
        ["shared"],
    )];

    let mut worker = planning_input(1, [machine_id("machine_a")]);
    update_request(&mut worker.service, |request| {
        request.service_id = service_id("svc_worker");
    });
    declare_plain_volume_mounts(&mut worker, vec![volume_mount("shared", "/worker")]);
    worker.volume_pins = vec![volume_pin("shared", "machine_a")];
    worker.cleanup_candidates = vec![with_named_volumes(
        cleanup_container_observed("machine_a", "ctr_api_old", true, None),
        ["shared"],
    )];

    let plan = plan_inputs(vec![api, worker], Vec::new()).expect("shared volume plans");
    let services = plan
        .phases
        .iter()
        .flat_map(|phase| &phase.services)
        .collect::<Vec<_>>();
    let api = services
        .iter()
        .find(|service| service.service_id == service_id("svc_api"))
        .expect("api plan");
    let worker = services
        .iter()
        .find(|service| service.service_id == service_id("svc_worker"))
        .expect("worker plan");

    assert!(matches!(api.work, DeployServiceWork::VolumeHandoff { .. }));
    assert!(matches!(worker.work, DeployServiceWork::Ordinary { .. }));
}

#[test]
fn retained_stopped_handoff_owner_still_reaches_post_promotion_cleanup() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    declare_plain_volume_mounts(&mut input, vec![volume_mount("data", "/data")]);
    input.volume_pins = vec![volume_pin("data", "machine_a")];
    input.service.keep = Some(ployz_core::deploy::ContainerRetentionCount::new(1));
    input.cleanup_candidates = vec![with_named_volumes(
        cleanup_container_observed("machine_a", "ctr_stopped", false, Some(10)),
        ["data"],
    )];

    let plan = plan_single_service(&input).expect("replacement plans");
    let [phase] = plan.phases.as_slice() else {
        panic!("one phase")
    };
    let [service] = phase.services.as_slice() else {
        panic!("one service")
    };

    assert!(matches!(
        service.work,
        DeployServiceWork::VolumeHandoff { .. }
    ));
    assert_eq!(
        plan.cleanup_actions
            .iter()
            .map(|action| action.target().clone())
            .collect::<Vec<_>>(),
        vec![cleanup_container("machine_a", "ctr_stopped")]
    );
}

#[test]
fn pinned_colocation_conflict_precedes_an_earlier_live_admission_failure() {
    let mut api = planning_input(1, [machine_id("machine_silent")]);
    declare_provisioned_volume(&mut api, "api_data", "/data", 600);
    api.storage_testimony = BTreeMap::new();

    let mut left = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    update_request(&mut left.service, |request| {
        request.service_id = service_id("svc_left");
    });
    declare_plain_volume_mounts(
        &mut left,
        vec![
            volume_mount("left_pin", "/left"),
            volume_mount("shared", "/shared"),
        ],
    );
    left.volume_pins = vec![volume_pin("left_pin", "machine_a")];

    let mut right = planning_input(1, [machine_id("machine_a"), machine_id("machine_b")]);
    update_request(&mut right.service, |request| {
        request.service_id = service_id("svc_right");
    });
    declare_plain_volume_mounts(
        &mut right,
        vec![
            volume_mount("right_pin", "/right"),
            volume_mount("shared", "/shared"),
        ],
    );
    right.volume_pins = vec![volume_pin("right_pin", "machine_b")];

    assert_eq!(
        plan_inputs(vec![api, left, right], Vec::new()),
        Err(DeployPlanError::ConflictingVolumePins {
            service_id: service_id("svc_right"),
            machines: vec![machine_id("machine_a"), machine_id("machine_b")],
        })
    );
}
