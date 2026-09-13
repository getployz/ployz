//! Session-level preview/confirm/run behaviour against a fake Machine.
#[path = "deploy_client/removal.rs"]
mod removal;
#[path = "deploy_client/support.rs"]
mod support;
use support::*;

use std::{num::NonZeroU64, process::Stdio, sync::atomic::Ordering, time::Duration};

use ployz::deploy::{
    DeployError, DeployEvent, DeployIntent, DeployOperation, DeployOutcome, DeployWarning,
    ExecutionError, FailedOperation, OperationStatus, PlanError, PlanOptions, PruneRefusal,
    VolumeFate,
};
use ployz_core::{
    ContainerId, MachineId, MachineStorageObservation, OperationPhase, ProjectName,
    ProvisionedVolumeMaximumBytes, QualifiedService, RequestedServiceSpec,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn changes_review_reports_removed_services_and_the_planners_prune_refusals() {
    for (selected, refusal, incomplete, expected) in [
        (false, None, false, None),
        (true, None, false, Some(PruneRefusal::SelectedServices)),
        (
            false,
            Some(ployz_core::ComposePruneRefusal::FilteredProfiles),
            false,
            Some(PruneRefusal::FilteredProfiles),
        ),
        (
            false,
            Some(ployz_core::ComposePruneRefusal::GuessedProjectName),
            false,
            Some(PruneRefusal::GuessedProjectName),
        ),
        (false, None, true, Some(PruneRefusal::IncompleteSnapshot)),
    ] {
        let one = machine('a', "one");
        let two = machine('b', "two");
        let mut service =
            DeployService::new(one.clone()).with_machines(vec![one.clone(), two.clone()]);
        if incomplete {
            service = service.fail_container_listing(two.machine.id);
        }
        let mutations = service.mutating_rpcs();
        let mut foreign = running_container(&one, &spec("foreign")).into_parts();
        foreign.project_name = ProjectName::parse("elsewhere").unwrap();
        let mut profiled = running_container(&one, &spec("profiled")).into_parts();
        profiled.container_id = ContainerId::parse("2".repeat(64)).unwrap();
        foreign.container_id = ContainerId::parse("3".repeat(64)).unwrap();
        *service.listed_containers().lock().unwrap() = vec![
            running_container(&one, &spec("removed")),
            profiled.try_into().unwrap(),
            foreign.try_into().unwrap(),
        ];
        let project = ployz::compose::parse_normalized(
            "services: {web: {image: nginx}, profiled: {image: nginx, profiles: [optional]}}",
            std::env::temp_dir(),
        )
        .unwrap();
        let candidate = project.capture(
            ProjectName::parse("app").unwrap(),
            PlanOptions {
                selected: if selected {
                    vec![ployz_core::ServiceAttempt {
                        name: spec("web").name,
                    }]
                } else {
                    vec![]
                },
                ..Default::default()
            },
            vec![],
            refusal,
            vec![],
        );
        let (mut client, server) = connected(service).await;
        let review = client.changes(&candidate).await.unwrap();
        assert_eq!(
            review.would_remove,
            [QualifiedService::new(
                ProjectName::parse("app").unwrap(),
                spec("removed").name
            )]
        );
        assert_eq!(review.prune_refusal, expected);
        assert_eq!(mutations.load(Ordering::SeqCst), 0);
        server.abort();
    }
}

#[tokio::test]
async fn changes_review_uses_the_capture_and_fresh_mixed_evidence_without_mutations() {
    let one = machine('a', "one");
    let two = machine('b', "two");
    let mut omitted = machine('c', "omitted");
    omitted.membership = ployz_core::MembershipObservation::Down;
    let failed = machine('d', "failed");
    let service = DeployService::new(one.clone())
        .with_machines(vec![
            one.clone(),
            two.clone(),
            omitted.clone(),
            failed.clone(),
        ])
        .fail_container_listing(failed.machine.id);
    let mutations = service.mutating_rpcs();
    let listed = service.listed_containers();
    let mut old = spec("web");
    old.container.command = vec!["old".into()];
    old.container
        .environment
        .insert("PLAIN".into(), "stale-metadata".into());
    let mut newer = old.clone();
    newer.container.command = vec!["captured".into()];
    *listed.lock().unwrap() = vec![
        running_container(&one, &old),
        running_container(&two, &newer),
    ];
    let directory = std::env::temp_dir().join(format!("ployz-capture-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("compose.yaml");
    let yaml = "services: {web: {image: web, command: [captured], environment: {TOKEN: 'secret://token', PLAIN: from-docker}}, absent: {image: alpine}}\nsecrets: {token: {x-command: 'exit 99'}}";
    std::fs::write(&source, yaml).unwrap();
    let project = ployz::compose::load_project(&ployz::compose::LoadOptions {
        files: vec![source.clone()],
        working_dir: Some(directory.clone()),
        ..Default::default()
    })
    .unwrap();
    let candidate = project.capture(
        ProjectName::parse("app").unwrap(),
        Default::default(),
        vec![],
        None,
        vec![source.clone()],
    );
    std::fs::write(&source, yaml.replace("[captured]", "[later-edit]")).unwrap();
    let (mut client, server) = connected(service).await;
    let first = client.changes(&candidate).await.unwrap();
    assert_eq!(first.candidate_id, candidate.id());
    assert_eq!(first.observer_machine_id, one.machine.id);
    assert_eq!(
        first.compared_settings,
        ployz_core::COMPARED_SERVICE_SETTINGS
    );
    assert_eq!(first.omissions, [omitted.machine.id]);
    assert_eq!(first.failures.len(), 1);
    assert_eq!(
        first.failures.first().unwrap().machine_id,
        failed.machine.id
    );
    assert_eq!(
        first.failures.first().unwrap().error,
        ployz_core::RpcErrorCode::Unavailable
    );
    let web = first
        .services
        .iter()
        .find(|row| row.name.as_str() == "web")
        .unwrap();
    assert_eq!(web.command, ["captured"]);
    assert_eq!(web.observations.len(), 2);
    assert_eq!(
        web.observations
            .iter()
            .filter(|row| row.changes.iter().any(|change| change.setting == "command"))
            .count(),
        1
    );
    let absent = first
        .services
        .iter()
        .find(|row| row.name.as_str() == "absent")
        .unwrap();
    assert!(absent.observations.is_empty());
    assert_eq!(absent.missing_on, [one.machine.id, two.machine.id]);
    let json = serde_json::to_string(&first).unwrap();
    assert!(!json.contains("secret://token"));
    assert!(!json.contains("live-only-sentinel"));
    assert!(!json.contains("stale-metadata"));
    assert!(web.observations.iter().all(|observation| {
        observation.environment.iter().any(|row| {
            row.key == "PLAIN" && row.evidence == ployz_core::config::EnvironmentEvidence::Same
        })
    }));
    assert!(
        web.observations
            .iter()
            .all(
                |observation| observation.environment.iter().any(|row| row.key == "TOKEN"
                    && matches!(
                        row.evidence,
                        ployz_core::config::EnvironmentEvidence::NotChecked { .. }
                    ))
            )
    );
    assert!(!json.contains("exit 99"));
    assert!(!json.contains("later-edit"));
    listed.lock().unwrap().clear();
    let second = client.changes(&candidate).await.unwrap();
    assert!(
        second
            .services
            .iter()
            .all(|row| row.observations.is_empty())
    );
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn exec_honors_remote_exit_while_terminal_stdin_remains_open() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone()).with_exec_exit(17);
    service
        .listed_containers()
        .lock()
        .unwrap()
        .push(running_container(&machine, &spec("web")));
    let (address, server) = listening(service).await;
    let command = format!(
        "{} --connect tcp://{address} exec -T web true",
        env!("CARGO_BIN_EXE_ployz")
    );
    let mut exec = tokio::process::Command::new("script")
        .args(["--quiet", "--return", "--command", &command, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let terminal_stdin = exec.stdin.take().unwrap();

    let status = tokio::time::timeout(Duration::from_secs(2), exec.wait())
        .await
        .expect("CLI must exit before terminal stdin closes")
        .unwrap();

    assert_eq!(status.code(), Some(17));
    drop(terminal_stdin);
    server.abort();
}

#[tokio::test]
async fn captured_a_deploys_while_edited_b_is_reviewed_and_cancellation_retains_unattempted_work() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone()).hold_health();
    let created = service.created_specs();
    let listed = service.listed_containers();
    let (mut deploy_client, deploy_server) = connected(service.clone()).await;
    let (mut review_client, review_server) = connected(service).await;
    let root =
        std::env::temp_dir().join(format!("ployz-edit-during-deploy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("compose.yaml");
    let yaml = "services:\n  web:\n    image: nginx\n    command: [A]\n    healthcheck: {test: [CMD, 'true']}\n  tail:\n    image: nginx\n    depends_on: [web]\n";
    std::fs::write(&path, yaml).unwrap();
    let capture = || {
        ployz::compose::load_project(&ployz::compose::LoadOptions {
            files: vec![path.clone()],
            working_dir: Some(root.clone()),
            ..Default::default()
        })
        .unwrap()
        .capture(
            ProjectName::parse("app").unwrap(),
            Default::default(),
            vec![],
            None,
            vec![path.clone()],
        )
    };
    let a = capture();
    let plan = deploy_client.preview(a.intent().clone()).await.unwrap();
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let running = deploy_client.confirm(&plan, &cancel, Some(tx));
    tokio::pin!(running);
    let mut b = None;
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                event = rx.recv() => {
                    if b.is_none() && matches!(event, Some(DeployEvent::Progress { ref rows, .. }) if rows.iter().any(|row| matches!(row.status, OperationStatus::Running { phase: OperationPhase::WaitingForHealth { .. } }))) {
                        let observed = created.lock().unwrap().first().unwrap().to_requested();
                        assert_eq!(observed.container.command, ["A"]);
                        *listed.lock().unwrap() = vec![running_container(&machine, &observed)];
                        std::fs::write(&path, yaml.replace("[A]", "[B]")).unwrap();
                        let candidate = capture();
                        let review = review_client.changes(&candidate).await.unwrap();
                        let web = review.services.iter().find(|service| service.name.as_str() == "web").unwrap();
                        assert!(web.observations.iter().any(|observation| observation.changes.iter().any(|change| change.setting == "command" && change.before == serde_json::json!(["A"]) && change.after == serde_json::json!(["B"]))));
                        b = Some(candidate);
                        cancel.cancel();
                    }
                }
                outcome = &mut running => break outcome,
            }
        }
    }).await.unwrap();
    let DeployOutcome::Failed {
        failed, unexecuted, ..
    } = outcome
    else {
        panic!("expected cancellation")
    };
    assert!(matches!(
        failed,
        FailedOperation::Operation {
            error: ExecutionError::Cancelled | ExecutionError::Health { .. },
            ..
        }
    ));
    assert!(!unexecuted.is_empty());
    assert!(
        created
            .lock()
            .unwrap()
            .iter()
            .all(|spec| spec.container.command == ["A"])
    );
    listed.lock().unwrap().clear();
    let after = review_client.changes(b.as_ref().unwrap()).await.unwrap();
    assert!(
        after
            .services
            .iter()
            .all(|service| service.observations.is_empty())
    );
    deploy_server.abort();
    review_server.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn ingress_deploy_builds_the_caddy_spec() {
    let service = DeployService::new(machine('a', "one"));
    let created = service.created_specs();
    let (address, server) = listening(service).await;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "ingress",
            "deploy",
            "--image",
            "caddy:test",
            "--skip-health",
        ])
        .output()
        .await
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let specs = created.lock().unwrap();
    let spec = specs.first().unwrap();
    assert_eq!(spec.container.image, "caddy:test");
    assert_eq!(
        spec.container.command,
        ["caddy", "run", "-c", "/config/caddy/Caddyfile"]
    );
    assert_eq!(spec.ports.len(), 3);
    server.abort();
}

#[tokio::test]
async fn deploy_creates_containers_owned_by_the_intent_project() {
    let service = DeployService::new(machine('a', "one"));
    let created = service.created_projects();
    let (mut client, server) = connected(service).await;
    client
        .run(
            DeployIntent::apply_one(
                ProjectName::parse("shop").unwrap(),
                spec("web"),
                skip_health(),
            ),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        *created.lock().unwrap(),
        [ProjectName::parse("shop").unwrap()]
    );
    server.abort();
}

#[tokio::test]
async fn deploy_returns_success_for_a_completed_run() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone());
    let observation_rpcs = service.observation_rpcs();
    let (mut client, server) = connected(service).await;
    let spec = spec("web");

    let outcome = client
        .run(
            DeployIntent::apply_one(ProjectName::parse("app").unwrap(), spec, skip_health()),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

    let DeployOutcome::Success { completed } = outcome else {
        panic!("expected success: {outcome:?}");
    };
    assert_eq!(completed.len(), 1);
    assert!(matches!(
        completed.first(),
        Some(DeployOperation::RunContainer {
            machine_id,
            spec,
            skip_health_monitor: true,
        }) if *machine_id == machine.machine.id && spec.name.as_str() == "web"
    ));
    assert_eq!(observation_rpcs.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn deploy_waits_for_the_replicated_serving_container_after_start() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine).with_observation_barrier();
    let observation_rpcs = service.observation_rpcs();
    let (mut client, server) = connected(service).await;

    let outcome = client
        .run(
            DeployIntent::apply_one(
                ProjectName::parse("app").unwrap(),
                spec("web"),
                skip_health(),
            ),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    assert_eq!(observation_rpcs.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn deploy_barrier_requires_every_capable_machine_and_uses_waiting_rounds() {
    let first = machine('a', "one");
    let second = machine('b', "two");
    let service = DeployService::new(first.clone())
        .with_machines(vec![first, second.clone()])
        .with_observation_barrier()
        .delay_observations(second.machine.id, 1);
    let requests = service.observation_requests();
    let (mut client, server) = connected(service).await;

    let outcome = client
        .run(
            DeployIntent::apply_one(
                ProjectName::parse("app").unwrap(),
                spec("web"),
                skip_health(),
            ),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for machine_id in [
        MachineId::parse("a".repeat(32)).unwrap(),
        MachineId::parse("b".repeat(32)).unwrap(),
    ] {
        let waits = requests
            .iter()
            .filter(|(target, _, _)| *target == machine_id)
            .map(|(_, _, wait)| *wait)
            .collect::<Vec<_>>();
        let [first_wait, second_wait] = waits.as_slice() else {
            panic!("expected two observation rounds: {waits:?}");
        };
        assert_eq!(*first_wait, 0);
        assert!(*second_wait > 0);
        assert!(
            requests
                .iter()
                .filter(|(target, _, _)| *target == machine_id)
                .all(|(_, ids, _)| ids.len() == 1)
        );
    }
    server.abort();
}

#[tokio::test]
async fn deploy_barrier_propagates_a_reached_store_error() {
    let service = DeployService::new(machine('a', "one"))
        .with_observation_barrier()
        .fail_observations("cluster store failed");
    let (mut client, server) = connected(service).await;

    let outcome = client
        .run(
            DeployIntent::apply_one(
                ProjectName::parse("app").unwrap(),
                spec("web"),
                skip_health(),
            ),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

    assert!(matches!(
        &outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Operation {
                error: ExecutionError::Machine { error, .. },
                ..
            },
            ..
        } if error.code == ployz_core::RpcErrorCode::Internal
            && error.message.contains("cluster store failed")
    ));
    server.abort();
}

#[tokio::test]
async fn deploy_cancellation_aborts_an_in_flight_observation_wait() {
    let service = DeployService::new(machine('a', "one"))
        .with_observation_barrier()
        .hold_observations();
    let (mut client, server) = connected(service).await;
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    });

    let outcome = client
        .run(
            DeployIntent::apply_one(
                ProjectName::parse("app").unwrap(),
                spec("web"),
                skip_health(),
            ),
            &cancellation,
            None,
        )
        .await
        .unwrap();

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Cancelled,
                    ..
                },
                ..
            }
        ),
        "{outcome:?}"
    );
    server.abort();
}

#[tokio::test]
async fn service_lifecycle_commands_wait_for_their_successful_service_containers() {
    for (action, dropped) in [("start", false), ("stop", true), ("rm", true)] {
        let machine = machine('a', "one");
        let mut service = DeployService::new(machine.clone()).with_observation_barrier();
        if dropped {
            service = service.with_dropped_observations();
        }
        let mut api = running_container(&machine, &spec("api"));
        api.try_update(|parts| parts.container_id = ContainerId::parse("2".repeat(64)).unwrap())
            .unwrap();
        service
            .listed_containers()
            .lock()
            .unwrap()
            .extend([running_container(&machine, &spec("web")), api]);
        let observation_rpcs = service.observation_rpcs();
        let observation_requests = service.observation_requests();
        let (address, server) = listening(service).await;

        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
        if action == "rm" {
            command.arg("service").arg(action).arg("--yes");
        } else {
            command.arg("service").arg(action);
        }
        let output = command
            .args(["--connect", &format!("tcp://{address}"), "web", "api"])
            .output()
            .await
            .unwrap();

        assert!(
            output.status.success(),
            "{action}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(observation_rpcs.load(Ordering::SeqCst), 1, "{action}");
        assert_eq!(
            observation_requests
                .lock()
                .unwrap()
                .first()
                .unwrap()
                .1
                .len(),
            2
        );
        server.abort();
    }
}

#[tokio::test]
async fn provisioned_volume_deploy_reaches_container_creation() {
    let mut target = machine('a', "one");
    target.storage = Some(MachineStorageObservation::Ready);
    let service = DeployService::new(target);
    let created = service.created_projects();
    let (mut client, server) = connected(service).await;
    let mut requested = spec("web");
    add_named_volume(&mut requested, "data");
    let mut volumes = requested.volume_graph().volumes().to_vec();
    let mounts = requested.volume_graph().mounts().to_vec();
    let source = &mut volumes
        .first_mut()
        .expect("fixture mounts one volume")
        .source;
    let (name, labels) = match source.kind() {
        ployz_core::RawVolumeSource::Ordinary { name, labels, .. } => {
            (name.clone(), labels.clone())
        }
        ployz_core::RawVolumeSource::External { .. }
        | ployz_core::RawVolumeSource::Bind { .. }
        | ployz_core::RawVolumeSource::Provisioned { .. }
        | ployz_core::RawVolumeSource::Tmpfs { .. } => unreachable!("fixture starts ordinary"),
    };
    *source = ployz_core::RawVolumeSource::Provisioned {
        name,
        maximum_bytes: ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(157_286_400).unwrap()),
        labels,
    }
    .admit()
    .expect("valid volume declaration");
    requested
        .set_volume_graph(ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap())
        .unwrap();
    let intent =
        DeployIntent::apply_one(ProjectName::parse("app").unwrap(), requested, skip_health());

    let outcome = client
        .run(intent, &CancellationToken::new(), None)
        .await
        .unwrap();

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    assert_eq!(
        *created.lock().unwrap(),
        [ProjectName::parse("app").unwrap()]
    );
    server.abort();
}

#[tokio::test]
async fn volume_ensure_failure_is_reported_on_the_container_operation() {
    let machine = machine('a', "one");
    let (mut client, server) =
        connected(DeployService::new(machine.clone()).fail_create_volume("volume create failed"))
            .await;
    let mut spec = spec("web");
    add_named_volume(&mut spec, "data");

    let outcome = client
        .run(
            DeployIntent::apply_one(ProjectName::parse("app").unwrap(), spec, skip_health()),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

    let DeployOutcome::Failed {
        completed,
        failed,
        unexecuted,
    } = outcome
    else {
        panic!("expected partial failure: {outcome:?}");
    };
    assert!(completed.is_empty());
    assert!(matches!(
        failed,
        FailedOperation::Operation {
            operation: DeployOperation::RunContainer { spec, .. },
            error: ExecutionError::Machine {
                action: ployz_core::MachineAction::CreateContainer,
                ..
            },
        } if spec.name.as_str() == "web"
    ));
    assert!(unexecuted.is_empty());
    server.abort();
}

#[tokio::test]
async fn created_but_unverified_volume_fails_the_container_operation() {
    let machine = machine('a', "one");
    let (mut client, server) = connected(
        DeployService::new(machine)
            .fail_create_volume_verification("Docker inspect response was malformed"),
    )
    .await;
    let mut spec = spec("web");
    add_named_volume(&mut spec, "data");

    let outcome = client
        .run(
            DeployIntent::apply_one(ProjectName::parse("app").unwrap(), spec, skip_health()),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

    let DeployOutcome::Failed {
        completed,
        failed,
        unexecuted,
    } = outcome
    else {
        panic!("expected partial failure: {outcome:?}");
    };
    assert!(completed.is_empty());
    let FailedOperation::Operation {
        operation: DeployOperation::RunContainer { .. },
        error:
            ExecutionError::Machine {
                action: ployz_core::MachineAction::CreateContainer,
                error,
            },
    } = &failed
    else {
        panic!("unexpected failed operation: {failed:?}");
    };
    assert!(
        error.message.contains("was created") && error.message.contains("could not be verified"),
        "{}",
        error.message
    );
    assert!(unexecuted.is_empty());
    server.abort();
}

#[tokio::test]
async fn deploy_surfaces_a_planning_error_instead_of_an_outcome() {
    let (mut client, server) = connected(DeployService::empty()).await;

    let error = client
        .run(
            DeployIntent::apply_one(
                ProjectName::parse("app").unwrap(),
                spec("web"),
                skip_health(),
            ),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        DeployError::Plan(PlanError::NoEligibleMachines { .. })
    ));
    assert!(
        error
            .to_string()
            .contains("no Machines in the Deploy Snapshot"),
        "{error}"
    );
    server.abort();
}

#[tokio::test]
async fn preview_returns_operations_and_mutates_nothing() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone());
    let mutating = service.mutating_rpcs();
    let (mut client, server) = connected(service).await;
    let spec = spec("web");

    let preview = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
            spec,
            skip_health(),
        ))
        .await
        .unwrap();

    assert_eq!(mutating.load(Ordering::SeqCst), 0);
    assert_eq!(preview.operations.len(), 1);
    assert!(matches!(
        preview.operations.first().map(|row| &row.operation),
        Some(DeployOperation::RunContainer {
            machine_id,
            spec,
            skip_health_monitor: true,
        }) if *machine_id == machine.machine.id && spec.name.as_str() == "web"
    ));
    server.abort();
}

#[tokio::test]
async fn confirm_executes_the_previewed_operations_without_re_planning() {
    let machine = machine('a', "one");
    let spec = spec("web");
    let service = DeployService::new(machine.clone());
    let mutating = service.mutating_rpcs();
    let listed = service.listed_containers();
    let (mut client, server) = connected(service).await;
    let intent = DeployIntent::apply_one(
        ProjectName::parse("app").unwrap(),
        spec.clone(),
        skip_health(),
    );

    let preview = client.preview(intent).await.unwrap();
    assert_eq!(mutating.load(Ordering::SeqCst), 0);
    assert!(matches!(
        preview.operations.first().map(|row| &row.operation),
        Some(DeployOperation::RunContainer { spec, .. }) if spec.name.as_str() == "web"
    ));

    listed
        .lock()
        .unwrap()
        .push(running_container(&machine, &spec));

    let outcome = client
        .confirm(&preview, &CancellationToken::new(), None)
        .await;
    assert!(mutating.load(Ordering::SeqCst) > 0);
    let DeployOutcome::Success { completed } = outcome else {
        panic!("expected success: {outcome:?}");
    };
    assert_eq!(completed.len(), 1);
    assert!(
        matches!(
            completed.first(),
            Some(DeployOperation::RunContainer { spec, .. }) if spec.name.as_str() == "web"
        ),
        "confirm must execute the previewed RunContainer: {completed:?}"
    );
    server.abort();
}

#[tokio::test]
async fn preview_expands_ingress_and_includes_dns_warnings() {
    let mut machine = machine('a', "one");
    machine.machine.public_ip = Some("192.0.2.1".parse().unwrap());
    let service = DeployService::new(machine).with_domain("opaque.ployz.example");
    let mutating = service.mutating_rpcs();
    let (mut client, server) = connected(service).await;
    let spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "web",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "always" },
        "ports": [
            {
                "mode": "ingress",
                "hostname": { "kind": "cluster_domain" },
                "load_balancer_port": 443,
                "container_port": 8080,
                "http_protocol": "https"
            },
            {
                "mode": "ingress",
                "hostname": { "kind": "explicit", "hostname": "preview-deploy.invalid" },
                "load_balancer_port": 80,
                "container_port": 8080,
                "http_protocol": "http"
            }
        ]
    }))
    .unwrap();

    let preview = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
            spec,
            skip_health(),
        ))
        .await
        .unwrap();

    assert_eq!(mutating.load(Ordering::SeqCst), 0);
    let Some(DeployOperation::RunContainer { spec, .. }) =
        preview.operations.first().map(|row| &row.operation)
    else {
        panic!("expected RunContainer: {preview:?}");
    };
    let hostnames: Vec<_> = spec
        .ports
        .iter()
        .filter_map(|port| match port {
            ployz_core::PortPublication::Ingress { hostname, .. } => hostname
                .as_explicit_host()
                .map(ployz_core::IngressHost::as_str),
            ployz_core::PortPublication::Host { .. } => None,
        })
        .collect();
    assert!(
        hostnames.contains(&"web-app.opaque.ployz.example"),
        "ingress expansion must assign the hosted hostname: {hostnames:?}"
    );
    assert!(
        hostnames.contains(&"preview-deploy.invalid"),
        "explicit ingress hostname must remain: {hostnames:?}"
    );
    assert!(
        preview.warnings.iter().any(|warning| match warning {
            DeployWarning::IngressHostname { message } => {
                message.contains("preview-deploy.invalid")
                    && message.contains("192.0.2.1")
                    && !message.to_ascii_lowercase().contains("certificate")
            }
            DeployWarning::ObservationFailed { .. }
            | DeployWarning::ObservationOmitted { .. }
            | DeployWarning::StorageHeadroom { .. }
            | DeployWarning::UnbudgetedDiskUsage
            | DeployWarning::StorageObservationUnknown { .. }
            | DeployWarning::ObserverRelativeHostnameConflict
            | DeployWarning::SkippedDependencyHealth { .. } => false,
        }),
        "DNS warning must match the CLI body: {:?}",
        preview.warnings
    );
    server.abort();
}

#[tokio::test]
async fn preview_expands_a_chosen_cluster_domain_label_without_a_project_suffix() {
    let mut machine = machine('a', "one");
    machine.machine.public_ip = Some("192.0.2.1".parse().unwrap());
    let service = DeployService::new(machine).with_domain("opaque.ployz.example");
    let (mut client, server) = connected(service).await;
    let spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "web",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "always" },
        "ports": [{
            "mode": "ingress",
            "hostname": { "kind": "cluster_domain", "label": "api" },
            "load_balancer_port": 80,
            "container_port": 8080,
            "http_protocol": "http"
        }]
    }))
    .unwrap();

    let preview = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("shop").unwrap(),
            spec,
            skip_health(),
        ))
        .await
        .unwrap();

    let Some(DeployOperation::RunContainer { spec, .. }) =
        preview.operations.first().map(|row| &row.operation)
    else {
        panic!("expected RunContainer: {preview:?}");
    };
    let hostnames: Vec<_> = spec
        .ports
        .iter()
        .filter_map(|port| match port {
            ployz_core::PortPublication::Ingress { hostname, .. } => hostname
                .as_explicit_host()
                .map(ployz_core::IngressHost::as_str),
            ployz_core::PortPublication::Host { .. } => None,
        })
        .collect();
    assert_eq!(hostnames, ["api.opaque.ployz.example"]);
    server.abort();
}

#[tokio::test]
async fn preview_rejects_a_visible_owner_of_an_expanded_chosen_label() {
    let mut machine = machine('a', "one");
    machine.machine.public_ip = Some("192.0.2.1".parse().unwrap());
    let service = DeployService::new(machine.clone()).with_domain("opaque.ployz.example");
    let mut owner_spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "web",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "always" },
        "ports": [{
            "mode": "ingress",
            "hostname": { "kind": "explicit", "hostname": "api.opaque.ployz.example" },
            "load_balancer_port": 80,
            "container_port": 8080,
            "http_protocol": "http"
        }]
    }))
    .unwrap();
    let mut owner = running_container(&machine, &owner_spec);
    owner
        .try_update(|parts| parts.project_name = ProjectName::parse("blog").unwrap())
        .unwrap();
    service.listed_containers().lock().unwrap().push(owner);
    let (mut client, server) = connected(service).await;
    owner_spec.name = ployz_core::ServiceName::parse("api").unwrap();
    owner_spec.ports = vec![ployz_core::PortPublication::Ingress {
        hostname: ployz_core::IngressHostname::cluster_domain_label("api").unwrap(),
        load_balancer_port: 80.try_into().unwrap(),
        container_port: 8080.try_into().unwrap(),
        http_protocol: ployz_core::HttpProtocol::Http,
    }];

    let error = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("shop").unwrap(),
            owner_spec,
            skip_health(),
        ))
        .await
        .unwrap_err();

    assert!(matches!(
        &error,
        DeployError::Plan(PlanError::HostnameConflict { hostname, owner })
            if hostname.as_str() == "api.opaque.ployz.example"
                && *owner == QualifiedService::parse("blog/web").unwrap()
    ));
    assert_eq!(
        error.to_string(),
        "hostname api.opaque.ployz.example is already published by blog/web"
    );
    server.abort();
}

#[tokio::test]
async fn preview_rejects_a_combined_ingress_label_over_63_characters() {
    let mut machine = machine('a', "one");
    machine.machine.public_ip = Some("192.0.2.1".parse().unwrap());
    let service = DeployService::new(machine).with_domain("opaque.ployz.example");
    let (mut client, server) = connected(service).await;
    let spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "always" },
        "ports": [{
            "mode": "ingress",
            "hostname": { "kind": "cluster_domain" },
            "load_balancer_port": 443,
            "container_port": 8080,
            "http_protocol": "https"
        }]
    }))
    .unwrap();

    let error = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
            spec,
            skip_health(),
        ))
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "generated Ingress Hostname label \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" exceeds the 63-character DNS label limit; shorten the Service Name or Project Name, or supply a custom hostname"
    );
    server.abort();
}

#[tokio::test]
async fn preview_surfaces_a_planning_error_instead_of_a_preview() {
    let (mut client, server) = connected(DeployService::empty()).await;

    let error = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
            spec("web"),
            skip_health(),
        ))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        DeployError::Plan(PlanError::NoEligibleMachines { .. })
    ));
    server.abort();
}

#[tokio::test]
async fn preview_project_removal_refuses_the_reserved_project() {
    let (mut client, server) = connected(DeployService::empty()).await;
    let error = client
        .preview_project_removal(&ProjectName::system(), VolumeFate::Preserve)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        DeployError::Project(ployz::project::ProjectError::Reserved { .. })
    ));
    server.abort();
}

#[tokio::test]
async fn confirm_ignores_changed_preview_payload_and_replays_with_fresh_pending_rows() {
    let machine = machine('a', "one");
    let target = machine.machine.id;
    let service = DeployService::new(machine);
    let created = service.created_specs();
    let (mut client, server) = connected(service).await;
    let plan = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
            spec("web"),
            skip_health(),
        ))
        .await
        .unwrap();
    let mut displayed: ployz_core::DeployPreview =
        serde_json::from_value(serde_json::to_value(plan.preview()).unwrap()).unwrap();
    displayed.project_name = ProjectName::parse("forged").unwrap();
    let row = displayed.operations.first_mut().unwrap();
    row.machine_id = MachineId::random();
    row.index = 42;
    row.status = OperationStatus::Completed;
    row.operation = DeployOperation::RemoveContainer {
        machine_id: row.machine_id,
        container_id: ContainerId::parse("f".repeat(64)).unwrap(),
    };

    for _ in 0..2 {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = client
            .confirm(&plan, &CancellationToken::new(), Some(tx))
            .await;
        let first = rx.recv().await.expect("first progress event");
        let DeployEvent::Progress {
            rows,
            completed,
            total,
        } = first
        else {
            panic!("expected initial progress");
        };
        assert_eq!((completed, total), (0, 1));
        let row = rows.first().unwrap();
        assert_eq!(row.index, 0);
        assert_eq!(row.machine_id, target);
        assert_eq!(row.operation.machine_id(), target);
        assert_eq!(row.status, OperationStatus::Pending);
        assert!(matches!(
            row.operation,
            DeployOperation::RunContainer { .. }
        ));
        assert!(matches!(outcome, DeployOutcome::Success { .. }));
        while let Ok(event) = rx.try_recv() {
            if let DeployEvent::Progress { rows, .. } = event {
                assert!(
                    rows.iter()
                        .all(|row| row.machine_id == target && row.index == 0)
                );
            }
        }
    }
    let created = created.lock().unwrap();
    assert_eq!(created.len(), 2);
    assert!(created.iter().all(|spec| spec.name.as_str() == "web"));
    server.abort();
}

#[tokio::test]
async fn empty_target_is_noop_and_confirm_succeeds_with_zero_operations() {
    let machine = machine('a', "one");
    let (mut client, server) = connected(DeployService::new(machine)).await;
    let preview = client
        .preview(DeployIntent::new(
            ProjectName::parse("app").unwrap(),
            Vec::new(),
            skip_health(),
        ))
        .await
        .unwrap();
    assert!(preview.noop());
    assert!(preview.operations.is_empty());
    let outcome = client
        .confirm(&preview, &CancellationToken::new(), None)
        .await;
    assert_eq!(
        outcome,
        DeployOutcome::Success {
            completed: Vec::new()
        }
    );
    server.abort();
}

#[tokio::test]
async fn full_preview_confirms_prune_operations_without_replanning() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone());
    let mut debug = running_container(&machine, &spec("debug"));
    debug
        .try_update(|parts| parts.container_id = ContainerId::parse("2".repeat(64)).unwrap())
        .unwrap();
    service.listed_containers().lock().unwrap().push(debug);
    let (mut client, server) = connected(service).await;
    let preview = client
        .preview(DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
            [&spec("web")],
            skip_health(),
        ))
        .await
        .unwrap();
    assert_eq!(preview.prune_refusal, None);
    assert!(
        preview.operations.iter().any(|row| {
            matches!(
                row.operation,
                DeployOperation::RemoveContainer { container_id, .. }
                    if container_id.as_str() == "2".repeat(64)
            )
        }),
        "full preview must include the prune: {:?}",
        preview.operations
    );
    let planned: Vec<_> = preview
        .operations
        .iter()
        .map(|row| row.operation.clone())
        .collect();
    let outcome = client
        .confirm(&preview, &CancellationToken::new(), None)
        .await;
    assert_eq!(outcome, DeployOutcome::Success { completed: planned });
    server.abort();
}

#[tokio::test]
async fn partial_preview_does_not_prune_an_unselected_imperative_service() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone());
    let mut debug = running_container(&machine, &spec("debug"));
    debug
        .try_update(|parts| parts.container_id = ContainerId::parse("2".repeat(64)).unwrap())
        .unwrap();
    service.listed_containers().lock().unwrap().push(debug);
    let (mut client, server) = connected(service).await;
    let preview = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
            spec("web"),
            skip_health(),
        ))
        .await
        .unwrap();
    assert_eq!(preview.prune_refusal, Some(PruneRefusal::SelectedServices));
    assert!(
        !preview
            .operations
            .iter()
            .any(|row| matches!(row.operation, DeployOperation::RemoveContainer { .. })),
        "partial preview must not prune: {:?}",
        preview.operations
    );
    server.abort();
}

#[tokio::test]
async fn abort_during_health_wait_settles_a_cancelled_outcome() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine).hold_health();
    let (mut client, server) = connected(service).await;
    let mut options = skip_health();
    options.skip_health_monitor = false;
    let preview = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
            health_spec("web"),
            options,
        ))
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let running = client.confirm(&preview, &cancel, Some(tx));
    tokio::pin!(running);
    loop {
        tokio::select! {
            event = rx.recv() => {
                let Some(event) = event else { break };
                if let DeployEvent::Progress { rows, .. } = &event
                    && rows.iter().any(|row| {
                        matches!(
                            &row.status,
                            OperationStatus::Running {
                                phase: OperationPhase::WaitingForHealth { .. },
                            }
                        )
                    })
                {
                    cancel.cancel();
                }
            }
            outcome = &mut running => {
                let DeployOutcome::Failed { failed, .. } = outcome else {
                    panic!("expected cancelled failure: {outcome:?}");
                };
                assert!(matches!(
                    failed,
                    FailedOperation::Operation {
                        error: ExecutionError::Cancelled | ExecutionError::Health { .. },
                        ..
                    }
                ));
                break;
            }
        }
    }
    server.abort();
}

#[tokio::test]
async fn wait_phases_carry_elapsed_and_deadline_clocks() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine).hold_health();
    let (mut client, server) = connected(service).await;
    let mut options = skip_health();
    options.skip_health_monitor = false;
    let preview = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
            health_spec("web"),
            options,
        ))
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let running = client.confirm(&preview, &cancel, Some(tx));
    tokio::pin!(running);
    let mut saw_clocks = false;
    loop {
        tokio::select! {
            event = rx.recv() => {
                let Some(event) = event else { break };
                if let DeployEvent::Progress { rows, .. } = &event {
                    saw_clocks |= rows.iter().any(|row| matches!(
                        &row.status,
                        OperationStatus::Running {
                            phase: OperationPhase::WaitingForHealth {
                                deadline_ms,
                                ..
                            },
                        } if *deadline_ms > 0
                    ));
                    if saw_clocks {
                        cancel.cancel();
                    }
                }
            }
            outcome = &mut running => {
                let _ = outcome;
                break;
            }
        }
    }
    assert!(
        saw_clocks,
        "wait phases must include elapsed_ms/deadline_ms"
    );
    server.abort();
}
