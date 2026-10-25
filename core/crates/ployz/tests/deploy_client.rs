//! Session-level preview/confirm/run behaviour against a fake Machine.
#[path = "deploy_client/cleanup_report.rs"]
mod cleanup_report;
#[path = "deploy_client/operate.rs"]
mod operate;
#[path = "deploy_client/runner_cancellation.rs"]
mod runner_cancellation;
#[path = "deploy_client/support.rs"]
#[allow(
    dead_code,
    reason = "the SDK agent tests share this mock and use helpers this suite does not"
)]
mod support;
use support::*;

use std::{num::NonZeroU64, process::Stdio, sync::atomic::Ordering, time::Duration};

use ployz::deploy::{
    DeployError, DeployEvent, DeployIntent, DeployOperation, DeployOutcome, DeployWarning,
    ExecutionError, FailedOperation, OperationStatus, PlanError, PruneRefusal, VolumeFate,
};
use ployz_core::{
    ContainerId, MachineId, MachineStorageObservation, Namespace, OperationPhase,
    ProvisionedVolumeMaximumBytes, QualifiedService, RequestedServiceSpec, ServiceName,
    ServiceOutcome, ServiceResult,
};
use tokio_util::sync::CancellationToken;

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
async fn deploy_creates_containers_owned_by_the_intent_namespace() {
    let service = DeployService::new(machine('a', "one"));
    let created = service.created_namespaces();
    let (mut client, server) = connected(service).await;
    client
        .run(
            DeployIntent::apply_one(
                Namespace::parse("shop").unwrap(),
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
        [Namespace::parse("shop").unwrap()]
    );
    server.abort();
}

#[tokio::test]
async fn deploy_returns_success_for_a_completed_run() {
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone());
    let (mut client, server) = connected(service).await;
    let spec = spec("web");

    let outcome = client
        .run(
            DeployIntent::apply_one(Namespace::parse("app").unwrap(), spec, skip_health()),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

    let DeployOutcome::Success { completed, .. } = outcome else {
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
    server.abort();
}

#[tokio::test]
async fn service_lifecycle_commands_change_each_named_service_container() {
    // A stop holds each In copy out of rotation before it stops it.
    for (action, rpcs) in [("start", 2), ("stop", 4)] {
        let machine = machine('a', "one");
        let service = DeployService::new(machine.clone());
        let mut api = deployed_container(&machine, &spec("api"));
        api.try_update(|parts| parts.container_id = ContainerId::parse("2".repeat(64)).unwrap())
            .unwrap();
        service
            .listed_containers()
            .lock()
            .unwrap()
            .extend([deployed_container(&machine, &spec("web")), api]);
        let mutating_rpcs = service.mutating_rpcs();
        let (address, server) = listening(service).await;

        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args(["service", action])
            .args(["--connect", &format!("tcp://{address}"), "web", "api"])
            .output()
            .await
            .unwrap();

        assert!(
            output.status.success(),
            "{action}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(mutating_rpcs.load(Ordering::SeqCst), rpcs, "{action}");
        server.abort();
    }
}

#[tokio::test]
async fn provisioned_volume_deploy_reaches_container_creation() {
    let mut target = machine('a', "one");
    target.storage = Some(MachineStorageObservation::Ready);
    let service = DeployService::new(target);
    let created = service.created_namespaces();
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
        DeployIntent::apply_one(Namespace::parse("app").unwrap(), requested, skip_health());

    let outcome = client
        .run(intent, &CancellationToken::new(), None)
        .await
        .unwrap();

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    assert_eq!(*created.lock().unwrap(), [Namespace::parse("app").unwrap()]);
    server.abort();
}

fn replicated_on_both(name: &str) -> RequestedServiceSpec {
    let mut spec = spec(name);
    spec.mode =
        serde_json::from_value(serde_json::json!({ "mode": "replicated", "replicas": 2 })).unwrap();
    spec
}

#[tokio::test]
async fn deploy_pulls_an_always_image_on_every_target_before_any_container_change() {
    let (one, two) = (machine('a', "one"), machine('b', "two"));
    let service = DeployService::new(one.clone()).with_machines(vec![one.clone(), two.clone()]);
    let (pulls, creation_pulls, created) = (
        service.image_pulls(),
        service.creation_pulls(),
        service.created_specs(),
    );
    let (mut client, server) = connected(service).await;

    let outcome = client
        .run(
            DeployIntent::apply_one(
                Namespace::parse("app").unwrap(),
                replicated_on_both("web"),
                skip_health(),
            ),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(created.lock().unwrap().len(), 2);
    let pulls = pulls.lock().unwrap().clone();
    let pulled = pulls
        .iter()
        .map(|pull| pull.machine)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(pulled, [one.machine.id, two.machine.id].into());
    for pull in &pulls {
        assert_eq!(pull.image, "nginx");
        assert_eq!(pull.policy, ployz_core::PullPolicy::Always);
        assert_eq!(
            pull.mutations_before, 0,
            "pulled after a Container change: {pull:?}"
        );
    }
    assert_eq!(
        creation_pulls.load(Ordering::SeqCst),
        0,
        "creation pulled an image the pull phase already placed"
    );
    server.abort();
}

#[tokio::test]
async fn a_registry_pull_failure_on_one_machine_fails_the_deploy_with_nothing_touched() {
    let (one, two) = (machine('a', "one"), machine('b', "two"));
    let service = DeployService::new(one.clone())
        .with_machines(vec![one, two.clone()])
        .fail_pulls_on(two.machine.id);
    let (mutating, created) = (service.mutating_rpcs(), service.created_specs());
    let (mut client, server) = connected(service).await;

    let outcome = client
        .run(
            DeployIntent::apply_one(
                Namespace::parse("app").unwrap(),
                replicated_on_both("web"),
                skip_health(),
            ),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

    let DeployOutcome::Failed { completed, .. } = outcome else {
        panic!("expected a failed Deploy: {outcome:?}");
    };
    assert!(completed.is_empty());
    assert!(created.lock().unwrap().is_empty());
    assert_eq!(mutating.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn a_missing_image_fails_the_deploy_naming_the_image() {
    let one = machine('a', "one");
    let service = DeployService::new(one.clone())
        .missing_image()
        .fail_pulls_on(one.machine.id);
    let (mut client, server) = connected(service).await;

    let outcome = client
        .run(
            DeployIntent::apply_one(Namespace::parse("app").unwrap(), spec("web"), skip_health()),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

    let DeployOutcome::Failed {
        failed: FailedOperation::Operation { error, .. },
        ..
    } = outcome
    else {
        panic!("expected a failed Deploy: {outcome:?}");
    };
    let failure = ployz_store::Failure::of(&error, &[]);
    assert_eq!(
        failure.reason,
        "Image nginx not found, or its registry needs credentials"
    );
    assert_eq!(
        failure.cause,
        ["Docker responded with status code 404: nginx:latest: not found"]
    );
    server.abort();
}

#[tokio::test]
async fn an_image_absent_under_pull_policy_never_says_nothing_was_pulled() {
    let one = machine('a', "one");
    let service = DeployService::new(one.clone())
        .missing_image()
        .fail_pulls_on(one.machine.id);
    let (mut client, server) = connected(service).await;
    let mut web = spec("web");
    web.container.pull_policy = ployz_core::PullPolicy::Never;

    let outcome = client
        .run(
            DeployIntent::apply_one(Namespace::parse("app").unwrap(), web, skip_health()),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

    let DeployOutcome::Failed {
        failed: FailedOperation::Operation { error, .. },
        ..
    } = outcome
    else {
        panic!("expected a failed Deploy: {outcome:?}");
    };
    assert_eq!(
        ployz_store::Failure::of(&error, &[]).reason,
        "Image nginx is not on this Server, and the pull policy is Never"
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
            DeployIntent::apply_one(Namespace::parse("app").unwrap(), spec, skip_health()),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

    let DeployOutcome::Failed {
        completed,
        failed,
        unexecuted,
        ..
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

#[tokio::test(start_paused = true)]
async fn created_but_unverified_volume_fails_the_container_operation() {
    let machine = machine('a', "one");
    let (mut client, server) = connected_in_memory(
        DeployService::new(machine)
            .fail_create_volume_verification("Docker inspect response was malformed"),
    )
    .await;
    let mut spec = spec("web");
    add_named_volume(&mut spec, "data");

    let outcome = client
        .run(
            DeployIntent::apply_one(Namespace::parse("app").unwrap(), spec, skip_health()),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

    let DeployOutcome::Failed {
        completed,
        failed,
        unexecuted,
        ..
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
            DeployIntent::apply_one(Namespace::parse("app").unwrap(), spec("web"), skip_health()),
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
            Namespace::parse("app").unwrap(),
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
        Namespace::parse("app").unwrap(),
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
    let DeployOutcome::Success { completed, .. } = outcome else {
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
async fn preview_includes_dns_warnings() {
    let mut machine = machine('a', "one");
    machine.machine.public_ip = Some("192.0.2.1".parse().unwrap());
    let service = DeployService::new(machine);
    let mutating = service.mutating_rpcs();
    let (mut client, server) = connected(service).await;
    let spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "web",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "always" },
        "ports": [
            {
                "mode": "ingress",
                "hostname": "preview-deploy.invalid",
                "load_balancer_port": 80,
                "container_port": 8080,
                "http_protocol": "http"
            }
        ]
    }))
    .unwrap();

    let preview = client
        .preview(DeployIntent::apply_one(
            Namespace::parse("app").unwrap(),
            spec,
            skip_health(),
        ))
        .await
        .unwrap();

    assert_eq!(mutating.load(Ordering::SeqCst), 0);
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
            | DeployWarning::LeftRunning { .. }
            | DeployWarning::MixedAfterReturn { .. }
            | DeployWarning::ObserverRelativeHostnameConflict
            | DeployWarning::SkippedDependencyHealth { .. }
            | DeployWarning::ImagePortMismatch { .. }
            | DeployWarning::NotInStore { .. } => false,
        }),
        "DNS warning must match the CLI body: {:?}",
        preview.warnings
    );
    server.abort();
}

#[tokio::test]
async fn preview_rejects_a_visible_owner_of_the_hostname() {
    let mut machine = machine('a', "one");
    machine.machine.public_ip = Some("192.0.2.1".parse().unwrap());
    let service = DeployService::new(machine.clone());
    let mut owner_spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "web",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "always" },
        "ports": [{
            "mode": "ingress",
            "hostname": "api.opaque.ployz.example",
            "load_balancer_port": 80,
            "container_port": 8080,
            "http_protocol": "http"
        }]
    }))
    .unwrap();
    let mut owner = running_container(&machine, &owner_spec);
    owner
        .try_update(|parts| parts.namespace = Namespace::parse("blog").unwrap())
        .unwrap();
    service.listed_containers().lock().unwrap().push(owner);
    let (mut client, server) = connected(service).await;
    owner_spec.name = ployz_core::ServiceName::parse("api").unwrap();

    let error = client
        .preview(DeployIntent::apply_one(
            Namespace::parse("shop").unwrap(),
            owner_spec,
            skip_health(),
        ))
        .await
        .unwrap_err();

    assert!(matches!(
        &error,
        DeployError::Plan(PlanError::HostnameConflict { hostname, owner, .. })
            if hostname.as_str() == "api.opaque.ployz.example"
                && *owner == QualifiedService::parse("blog/web").unwrap()
    ));
    assert_eq!(
        error.to_string(),
        "hostname api.opaque.ployz.example is already published by blog/web: remove it from that Service first, or use another hostname"
    );
    server.abort();
}

#[tokio::test]
async fn preview_surfaces_a_planning_error_instead_of_a_preview() {
    let (mut client, server) = connected(DeployService::empty()).await;

    let error = client
        .preview(DeployIntent::apply_one(
            Namespace::parse("app").unwrap(),
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
async fn preview_namespace_removal_refuses_the_reserved_namespace() {
    let (mut client, server) = connected(DeployService::empty()).await;
    let error = client
        .preview_namespace_removal(&Namespace::system(), VolumeFate::Preserve)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        DeployError::Namespace(ployz::namespace::NamespaceError::Reserved { .. })
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
            Namespace::parse("app").unwrap(),
            spec("web"),
            skip_health(),
        ))
        .await
        .unwrap();
    let mut displayed: ployz_core::DeployPreview =
        serde_json::from_value(serde_json::to_value(plan.preview()).unwrap()).unwrap();
    displayed.namespace = Namespace::parse("forged").unwrap();
    let row = displayed.operations.first_mut().unwrap();
    row.machine_id = MachineId::random();
    row.index = 42;
    row.status = OperationStatus::Completed;
    row.operation = DeployOperation::RemoveContainer {
        machine_id: row.machine_id,
        container_id: ContainerId::parse("f".repeat(64)).unwrap(),
        purpose: ployz_core::RemoveContainerPurpose::Rollout,
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
            Namespace::parse("app").unwrap(),
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
            completed: Vec::new(),
            services: Vec::new(),
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
            Namespace::parse("app").unwrap(),
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
    assert_eq!(
        outcome,
        DeployOutcome::Success {
            completed: planned,
            services: vec![ServiceOutcome {
                service: ServiceName::parse("web").unwrap(),
                result: ServiceResult::New,
            }],
        }
    );
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
            Namespace::parse("app").unwrap(),
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
            Namespace::parse("app").unwrap(),
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
                                phase: OperationPhase::CreatingContainer,
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
                        error: ExecutionError::Cancelled,
                        ..
                    }
                ));
                break;
            }
        }
    }
    server.abort();
}

/// Cloud's runner claims a Store Deployment, deploys it and records it into Applied
/// State; a second delivery of the same Deployment finds nothing to run, and a
/// cancel stops it.
#[tokio::test]
async fn cloud_runner_deploys_a_store_deployment_once() {
    use ployz_store::{
        Actor, Admit, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId,
        DeploymentStatus, EnvironmentId, EnvironmentRef, NodeStatus, OrganizationId, ProjectId,
        ProjectName, RunnerId, SealingKey, ServiceLineageId,
    };
    use std::sync::Arc;

    let store =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap());
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ServiceName::parse("web").unwrap(),
                image: Some("nginx".into()),
                template: None,
            },
        )
        .unwrap();
    let id = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    let (address, server) = listening(DeployService::new(machine('a', "one"))).await;
    let run = |runner: &str| {
        ployz::sdk::run_deployment(
            ployz_core::Operator::Cloud,
            Arc::clone(&store),
            id.clone(),
            RunnerId::parse(runner).unwrap(),
            vec![ployz::context::Connection::tcp(address)],
            Ok(Default::default()),
        )
    };

    let summary = run("cloud-run-1").await.unwrap();
    assert_eq!(summary.status, DeploymentStatus::Applied);
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id: id.clone() })
        .unwrap();
    assert!(
        view.nodes
            .iter()
            .all(|node| node.outcome == NodeStatus::Deployed)
    );
    assert!(view.preview.is_some());

    // A duplicate delivery runs as another runner and finds it ended.
    let duplicate = run("cloud-run-2").await.unwrap_err();
    assert_eq!(duplicate.code, ployz_core::RpcErrorCode::Conflict);
    assert_eq!(
        store
            .read(&who, &ployz_store::DeploymentQuery { id: id.clone() })
            .unwrap()
            .deployment
            .status,
        DeploymentStatus::Applied
    );

    // Cancelled while it runs, the runner stops it.
    let holding = DeployService::new(machine('a', "one")).hold_health();
    let (held, held_server) = listening(holding.clone()).await;
    let second = DeploymentId::parse("00000000-0000-4000-8000-000000000102").unwrap();
    let mut admit = Deploy {
        id: second.clone(),
        environment: EnvironmentRef::default(),
        services: Vec::new(),
        version: None,
        upload: None,
        accept_volume_loss: Vec::new(),
        message: None,
        prune: false,
        no_prune: false,
    };
    store
        .write_trusted(
            &who,
            &Admit::Deploy(admit.clone()),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    let running = tokio::spawn(ployz::sdk::run_deployment(
        ployz_core::Operator::Cloud,
        Arc::clone(&store),
        second.clone(),
        RunnerId::parse("cloud-run-3").unwrap(),
        vec![ployz::context::Connection::tcp(held)],
        Ok(Default::default()),
    ));
    holding.health_waited().await;
    store
        .write(
            &who,
            &ployz_store::Cancel {
                deployment: second.clone(),
            },
        )
        .unwrap();
    let cancelled = running.await.unwrap().unwrap();
    assert_eq!(cancelled.status, DeploymentStatus::Cancelled);

    // Cancelled while queued, it never runs.
    admit.id = DeploymentId::parse("00000000-0000-4000-8000-000000000103").unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Deploy(admit.clone()),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    store
        .write(
            &who,
            &ployz_store::Cancel {
                deployment: admit.id.clone(),
            },
        )
        .unwrap();
    let never = ployz::sdk::run_deployment(
        ployz_core::Operator::Cloud,
        Arc::clone(&store),
        admit.id.clone(),
        RunnerId::parse("cloud-run-4").unwrap(),
        vec![ployz::context::Connection::tcp(address)],
        Ok(Default::default()),
    )
    .await
    .unwrap_err();
    assert_eq!(never.code, ployz_core::RpcErrorCode::Conflict);
    server.abort();
    held_server.abort();
}

#[tokio::test]
async fn observing_a_server_that_cannot_be_reached_names_it_and_why() {
    use ployz_core::{DockerVolumeName, MachineId};
    use ployz_store::ObservationStage;

    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = closed.local_addr().unwrap();
    drop(closed);
    let server = MachineId::parse("a".repeat(32)).unwrap();
    let sought = vec![DockerVolumeName::parse("shop-production_data").unwrap()];

    let observed = ployz::sdk::observe_volumes(
        vec![ployz::context::Connection::tcp(address).with_machine_id(server)],
        sought.clone(),
    )
    .await
    .unwrap();

    assert_eq!(observed.sought, sought);
    assert!(observed.held.is_empty());
    assert_eq!(observed.unanswered, [server]);
    let failure = observed.failure.unwrap();
    assert_eq!(failure.stage, ObservationStage::Unreachable);
    let deepest = failure.cause.last().unwrap_or(&failure.reason);
    assert!(deepest.contains("Connection refused"), "{failure:?}");
}

#[tokio::test]
async fn observing_a_server_that_fails_the_check_returns_without_asking_it_again() {
    use ployz_core::{DockerVolumeName, RpcError, RpcErrorCode};
    use ployz_store::ObservationStage;

    let one = machine('a', "one").machine.id;
    let mut service = DeployService::new(machine('a', "one"));
    let described = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    service.describe_once = Some(std::sync::Arc::clone(&described));
    *service.machines_error.lock().unwrap() = Some(RpcError {
        code: RpcErrorCode::Internal,
        message: "The Machine list is unavailable.".into(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    });
    let (address, server) = listening(service).await;
    let sought = vec![DockerVolumeName::parse("shop-production_data").unwrap()];

    let observed = tokio::time::timeout(
        Duration::from_secs(6),
        ployz::sdk::observe_volumes(vec![ployz::context::Connection::tcp(address)], sought),
    )
    .await
    .expect("a failed check returns its refusal")
    .unwrap();

    assert_eq!(
        observed.failure.unwrap().stage,
        ObservationStage::Answered { server: Some(one) }
    );
    assert_eq!(described.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn observing_through_a_daemon_that_refuses_does_not_name_a_server_it_did_not_hear() {
    use ployz_core::{DockerVolumeName, MachineId};
    use ployz_store::ObservationStage;

    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let down = closed.local_addr().unwrap();
    drop(closed);
    let one = MachineId::parse("a".repeat(32)).unwrap();
    let service = DeployService::new(machine('b', "two"));
    *service.describe_refusal.lock().unwrap() = Some("denied by two".into());
    let (address, server) = listening(service).await;
    let sought = vec![DockerVolumeName::parse("shop-production_data").unwrap()];

    let observed = ployz::sdk::observe_volumes(
        vec![
            ployz::context::Connection::tcp(down).with_machine_id(one),
            ployz::context::Connection::tcp(address),
        ],
        sought,
    )
    .await
    .unwrap();

    let failure = observed.failure.unwrap();
    assert_eq!(failure.stage, ObservationStage::Answered { server: None });
    let said = std::iter::once(&failure.reason)
        .chain(&failure.cause)
        .any(|line| line.contains("denied by two"));
    assert!(said, "{failure:?}");
    server.abort();
}

#[tokio::test]
async fn observing_a_server_that_answers_with_an_error_names_it_and_the_error() {
    use ployz_core::{DockerVolumeName, RpcError, RpcErrorCode};
    use ployz_store::ObservationStage;

    let one = machine('a', "one").machine.id;
    let service = DeployService::new(machine('a', "one"));
    *service.machines_error.lock().unwrap() = Some(RpcError {
        code: RpcErrorCode::Internal,
        message: "The Machine list is unavailable.".into(),
        details: serde_json::Value::Null,
        cause: vec!["database is locked".into()],
    });
    let (address, server) = listening(service).await;
    let sought = vec![DockerVolumeName::parse("shop-production_data").unwrap()];

    let observed = ployz::sdk::observe_volumes(
        vec![ployz::context::Connection::tcp(address).with_machine_id(one)],
        sought.clone(),
    )
    .await
    .unwrap();

    assert_eq!(observed.sought, sought);
    assert!(observed.held.is_empty());
    assert_eq!(observed.unanswered, [one]);
    let failure = observed.failure.unwrap();
    assert_eq!(
        failure.stage,
        ObservationStage::Answered { server: Some(one) }
    );
    assert_eq!(failure.reason, "The Machine list is unavailable.");
    assert_eq!(failure.cause, ["database is locked"]);
    server.abort();
}

#[tokio::test]
async fn deletion_review_holds_a_mirror_slot_as_the_server_keeping_it() {
    use ployz_core::{CopyRole, DockerVolumeId, DockerVolumeName, ProvisionedCopy};
    use std::sync::Arc;

    let service = DeployService::new(machine('a', "one"));
    let copies = Arc::clone(&service.copies);
    let (address, server) = listening(service).await;
    let copy = |role| ProvisionedCopy {
        role,
        maximum_bytes: ployz_core::ProvisionedVolumeMaximumBytes::new(
            NonZeroU64::new(ployz_core::STORAGE_GIB).unwrap(),
        ),
        used_bytes: 0,
        slot_role: None,
    };
    let name = |suffix: &str| DockerVolumeName::parse(format!("shop-production_{suffix}")).unwrap();
    copies.lock().unwrap().extend([
        (name("slot"), copy(CopyRole::Slot)),
        (name("unlisted-writer"), copy(CopyRole::Writer)),
        (name("unsought-slot"), copy(CopyRole::Slot)),
    ]);

    let observed = ployz::sdk::observe_volumes(
        vec![ployz::context::Connection::tcp(address)],
        vec![name("slot"), name("unlisted-writer"), name("gone")],
    )
    .await
    .unwrap();
    assert_eq!(
        observed.held,
        [DockerVolumeId {
            machine_id: ployz_core::MachineId::parse("a".repeat(32)).unwrap(),
            name: name("slot"),
        }]
    );
    assert!(observed.unanswered.is_empty());
    server.abort();
}

/// Compensation removes a replacement that crashed, yet its failed row still
/// shows the replacement's last output lines.
#[tokio::test]
async fn failed_deploy_whose_container_is_removed_still_reports_its_output_lines() {
    use ployz_store::{
        Actor, Admit, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId,
        DeploymentStatus, EnvironmentId, EnvironmentRef, OrganizationId, ProjectId, ProjectName,
        RowState, RunnerId, SealingKey, ServiceLineageId, Trusted,
    };
    use std::sync::Arc;

    let store =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap());
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ServiceName::parse("web").unwrap(),
                image: Some("nginx".into()),
                template: None,
            },
        )
        .unwrap();
    let id = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &Trusted::default(),
        )
        .unwrap();
    let machine = machine('a', "one");
    let service = DeployService::new(machine.clone()).crash_containers();
    service
        .listed_containers()
        .lock()
        .unwrap()
        .push(running_container_in(
            &machine,
            &spec("web"),
            "shop-production",
            'f',
        ));
    let removed = service.removed_containers();
    let (address, server) = listening(service).await;

    let summary = ployz::sdk::run_deployment(
        ployz_core::Operator::Cloud,
        Arc::clone(&store),
        id.clone(),
        RunnerId::parse("cloud-run-1").unwrap(),
        vec![ployz::context::Connection::tcp(address)],
        Ok(Default::default()),
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(summary.status, DeploymentStatus::Failed);
    let replacement = ContainerId::parse(format!("{:064x}", 1)).unwrap();
    assert!(removed.lock().unwrap().contains(&replacement));
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id })
        .unwrap();
    let logs = view
        .nodes
        .iter()
        .flat_map(|node| &node.rows)
        .filter_map(|row| {
            let RowState::Failed { log, .. } = &row.state else {
                return None;
            };
            let Some(ployz_store::tools::LogText::Shown(excerpt)) = log else {
                return Some(Err(log.clone()));
            };
            Some(Ok(excerpt.lines().to_vec()))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        logs,
        [Ok((3..=10)
            .map(|n| format!("line {n}"))
            .chain([
                "plain from-docker".to_owned(),
                "token=[redacted]".to_owned()
            ])
            .collect::<Vec<_>>())]
    );
}

/// A replacement Docker OOM-killed fails the Deployment as out of memory, naming
/// the memLimit it ran under; the health check stays the cause.
#[tokio::test]
async fn failed_deploy_whose_container_ran_out_of_memory_says_so() {
    use ployz_store::{
        Actor, Admit, Change, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId,
        DeploymentStatus, Edit, EnvironmentId, EnvironmentRef, OrganizationId, Outcome, ProjectId,
        ProjectName, RowState, RunnerId, SealingKey, ServiceLineageId, SettingPath, Trusted,
    };
    use std::sync::Arc;

    for oom in [true, false] {
        let store = Arc::new(
            ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap(),
        );
        let who = Actor::system(OrganizationId::parse("org").unwrap());
        store
            .write(
                &who,
                &CreateProject {
                    id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                    name: ProjectName::parse("shop").unwrap(),
                    default_environment: EnvironmentId::parse(
                        "00000000-0000-4000-8000-000000000002",
                    )
                    .unwrap(),
                },
            )
            .unwrap();
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ployz_core::ServiceName::parse("web").unwrap(),
                    image: Some("nginx".into()),
                    template: None,
                },
            )
            .unwrap();
        store
            .write(
                &who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![Change::Set {
                        path: SettingPath::parse("web.memLimit").unwrap(),
                        value: serde_json::json!("16MiB"),
                    }],
                },
            )
            .unwrap();
        let id = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
        store
            .write_trusted(
                &who,
                &Admit::Deploy(Deploy {
                    id: id.clone(),
                    environment: EnvironmentRef::default(),
                    services: Vec::new(),
                    version: None,
                    upload: None,
                    accept_volume_loss: Vec::new(),
                    message: None,
                    prune: false,
                    no_prune: false,
                }),
                &Trusted::default(),
            )
            .unwrap();
        let machine = machine('a', "one");
        let service = DeployService::new(machine.clone());
        let service = if oom {
            service.oom_kill_containers()
        } else {
            service.crash_containers()
        };
        let (address, server) = listening(service).await;

        let summary = ployz::sdk::run_deployment(
            ployz_core::Operator::Cloud,
            Arc::clone(&store),
            id.clone(),
            RunnerId::parse("cloud-run-1").unwrap(),
            vec![ployz::context::Connection::tcp(address)],
            Ok(Default::default()),
        )
        .await
        .unwrap();
        server.abort();

        assert_eq!(summary.status, DeploymentStatus::Failed);
        let view = store
            .read(&who, &ployz_store::DeploymentQuery { id })
            .unwrap();
        let Some(Outcome::Executed {
            reason: Some(reason),
            cause,
            ..
        }) = &view.deployment.outcome
        else {
            panic!(
                "expected an executed failure, got {:?}",
                view.deployment.outcome
            );
        };
        let rows = view
            .nodes
            .iter()
            .flat_map(|node| &node.rows)
            .filter_map(|row| {
                if let RowState::Failed { out_of_memory, .. } = &row.state {
                    Some(*out_of_memory)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if oom {
            assert_eq!(
                reason,
                "web: The Container ran out of memory under its 16 MiB memLimit"
            );
            assert_eq!(
                rows,
                [Some(ployz_core::OutOfMemory {
                    memory_bytes: Some(ployz_core::ByteQuantity::try_from(16 << 20).unwrap()),
                })]
            );
            assert!(
                cause.iter().any(|cause| cause.contains("exited with code")),
                "{cause:?}"
            );
        } else {
            assert_eq!(reason, "web: The Container exited with code 1");
            assert_eq!(cause, &["token=[redacted]"]);
            assert_eq!(rows, [None]);
        }
    }
}

/// A Container Docker keeps restarting fails the Deployment with the exit code and
/// last line its Server's Log Store holds, scrubbed like the row's log, so the
/// headline Cloud joins from them reads as the crash and not `restarting`.
#[tokio::test]
async fn failed_deploy_whose_container_keeps_restarting_names_its_exit_and_last_line() {
    restarting_deploy_names_its_exit_and_last_line(
        DeployService::new(machine('a', "one")).restart_containers(),
        Some("token=[redacted]"),
    )
    .await;
}

/// The last line is the one the exited run printed, not the next run's first.
#[tokio::test]
async fn failed_deploy_whose_container_restarted_names_the_exited_runs_last_line() {
    restarting_deploy_names_its_exit_and_last_line(
        DeployService::new(machine('a', "one")).reboot_containers(),
        Some("token=[redacted]"),
    )
    .await;
}

/// A run that exited printing nothing has no last line, so no earlier run's line
/// stands in for it.
#[tokio::test]
async fn failed_deploy_whose_last_run_printed_nothing_names_no_line() {
    restarting_deploy_names_its_exit_and_last_line(
        DeployService::new(machine('a', "one")).reboot_containers_through_a_silent_run(),
        None,
    )
    .await;
}

async fn restarting_deploy_names_its_exit_and_last_line(
    service: DeployService,
    last_line: Option<&str>,
) {
    use ployz_store::{
        Actor, Admit, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId,
        DeploymentStatus, EnvironmentId, EnvironmentRef, OrganizationId, Outcome, ProjectId,
        ProjectName, RowState, RunnerId, SealingKey, ServiceLineageId, Trusted,
    };
    use std::sync::Arc;

    let store =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap());
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ServiceName::parse("web").unwrap(),
                image: Some("nginx".into()),
                template: None,
            },
        )
        .unwrap();
    let id = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &Trusted::default(),
        )
        .unwrap();
    let (address, server) = listening(service).await;

    let summary = ployz::sdk::run_deployment(
        ployz_core::Operator::Cloud,
        Arc::clone(&store),
        id.clone(),
        RunnerId::parse("cloud-run-1").unwrap(),
        vec![ployz::context::Connection::tcp(address)],
        Ok(Default::default()),
    )
    .await
    .unwrap();
    server.abort();

    assert_eq!(summary.status, DeploymentStatus::Failed);
    let view = store
        .read(&who, &ployz_store::DeploymentQuery { id })
        .unwrap();
    let Some(Outcome::Executed {
        reason: Some(reason),
        cause,
        ..
    }) = &view.deployment.outcome
    else {
        panic!(
            "expected an executed failure, got {:?}",
            view.deployment.outcome
        );
    };
    assert_eq!(
        reason,
        "web: The Container exited with code 1 and is restarting"
    );
    let last_line: Vec<String> = last_line.into_iter().map(str::to_owned).collect();
    assert_eq!(cause, &last_line);
    let headline = cause
        .last()
        .map_or_else(|| reason.clone(), |cause| format!("{reason}: {cause}"));
    assert_eq!(
        headline,
        match last_line.first() {
            Some(line) =>
                format!("web: The Container exited with code 1 and is restarting: {line}"),
            None => "web: The Container exited with code 1 and is restarting".to_owned(),
        }
    );
    let rows = view
        .nodes
        .iter()
        .flat_map(|node| &node.rows)
        .filter_map(|row| {
            let RowState::Failed { reason, cause, .. } = &row.state else {
                return None;
            };
            Some((reason.as_str(), cause.clone()))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [(
            "The Container exited with code 1 and is restarting",
            last_line
        )]
    );
}

#[tokio::test]
async fn cloud_runner_applies_an_environment_removal() {
    use ployz_store::{
        Actor, Admit, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId,
        DeploymentStatus, EnvironmentId, EnvironmentRef, OrganizationId, ProjectId, ProjectName,
        Removal, Remove, RunnerId, SealingKey, ServiceLineageId, Trusted,
    };
    use std::{collections::BTreeMap, sync::Arc};

    let store =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap());
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ServiceName::parse("web").unwrap(),
                image: Some("nginx".into()),
                template: None,
            },
        )
        .unwrap();
    let service = DeployService::new(machine('a', "one"));
    let (address, server) = listening(service).await;
    let run = |id: DeploymentId, runner: &str| {
        ployz::sdk::run_deployment(
            ployz_core::Operator::Cloud,
            Arc::clone(&store),
            id,
            RunnerId::parse(runner).unwrap(),
            vec![ployz::context::Connection::tcp(address)],
            Ok(Default::default()),
        )
    };

    let deployed = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: deployed.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &Trusted::default(),
        )
        .unwrap();
    let ran = run(deployed, "cloud-run-1").await.unwrap();
    assert_eq!(ran.status, DeploymentStatus::Applied);

    let remove = Remove {
        project: ProjectName::parse("shop").unwrap(),
        environment: None,
        version: None,
        accept_volume_loss: BTreeMap::new(),
        every_copy: BTreeMap::new(),
    };
    let version = store
        .write_trusted(&who, &remove, &Trusted::default())
        .unwrap_err()
        .details
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap()
        .to_owned();
    let Removal::Removing { left, .. } = store
        .write_trusted(
            &who,
            &Remove {
                version: Some(version),
                ..remove
            },
            &Trusted::default(),
        )
        .unwrap()
    else {
        panic!("removed without the Servers");
    };
    let removal = left
        .first()
        .unwrap()
        .deployment
        .as_ref()
        .unwrap()
        .id
        .clone();
    let ran = run(removal, "cloud-run-2").await.unwrap();
    assert_eq!(ran.status, DeploymentStatus::Applied);
    assert!(ran.remove);
    server.abort();
}

#[tokio::test]
async fn cloud_runner_deletes_only_the_docker_volumes_a_deploy_accepted() {
    use ployz_store::{
        Actor, Admit, ConfigStore, CreateProject, CreateService, CreateVolume, Deploy,
        DeploymentId, DeploymentStatus, EnvironmentId, EnvironmentRef, Mount, OrganizationId,
        ProjectId, ProjectName, RemovalsQuery, RemoveVolume, RunnerId, SealingKey,
        ServiceLineageId, Trusted, VolumeId, VolumeName, VolumesQuery,
    };
    use std::sync::Arc;

    let store =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap());
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    let web = ployz_core::ServiceName::parse("web").unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: web.clone(),
                image: Some("postgres".into()),
                template: None,
            },
        )
        .unwrap();
    let data = VolumeName::parse("data").unwrap();
    store
        .write(
            &who,
            &CreateVolume {
                shared_writes: false,
                storage_locked: false,
                restored_from: None,
                storage: ployz_core::config::VolumeKind::Docker {},
                id: VolumeId::parse("00000000-0000-4000-8000-000000000004").unwrap(),
                environment: EnvironmentRef::default(),
                name: data.clone(),
                mounts: vec![Mount {
                    service: web,
                    path: "/var/lib/postgresql".into(),
                }],
            },
        )
        .unwrap();
    let service = DeployService::new(machine('a', "one"));
    let held = Arc::clone(&service.volumes);
    let (address, server) = listening(service).await;
    let connections = || vec![ployz::context::Connection::tcp(address)];
    let deploy = |n: u8, accept: Vec<VolumeName>, trusted: Trusted, version: Option<String>| {
        let id = DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{n:02}")).unwrap();
        store
            .write_trusted(
                &who,
                &Admit::Deploy(Deploy {
                    id: id.clone(),
                    environment: EnvironmentRef::default(),
                    services: Vec::new(),
                    version,
                    upload: None,
                    accept_volume_loss: accept,
                    message: None,
                    prune: false,
                    no_prune: false,
                }),
                &trusted,
            )
            .map(|_| id)
    };

    let first = deploy(1, Vec::new(), Trusted::default(), None).unwrap();
    let ran = ployz::sdk::run_deployment(
        ployz_core::Operator::Cloud,
        Arc::clone(&store),
        first,
        RunnerId::parse("cloud-run-1").unwrap(),
        connections(),
        Ok(Default::default()),
    )
    .await
    .unwrap();
    assert_eq!(ran.status, DeploymentStatus::Applied);
    let docker = ployz_core::DockerVolumeName::parse(
        "shop-production_vol-00000000-0000-4000-8000-000000000004",
    )
    .unwrap();
    let on = |letter: char| ployz_core::DockerVolume {
        id: ployz_core::DockerVolumeId {
            machine_id: ployz_core::MachineId::parse(letter.to_string().repeat(32)).unwrap(),
            name: docker.clone(),
        },
        options: Default::default(),
        labels: Default::default(),
        storage: ployz_core::DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    };
    held.lock().unwrap().push(on('a'));

    store
        .write(
            &who,
            &RemoveVolume {
                environment: EnvironmentRef::default(),
                volume: data.clone(),
            },
        )
        .unwrap();
    let sought = store
        .read(&who, &RemovalsQuery::default())
        .unwrap()
        .volumes
        .into_iter()
        .map(|volume| volume.docker_volume)
        .collect();
    let observed = ployz::sdk::observe_volumes(connections(), sought)
        .await
        .unwrap();
    assert_eq!(observed.held.len(), 1);
    let trusted = Trusted {
        volumes: Some(observed),
        ..Trusted::default()
    };
    // Unaccepted it refuses; accepted, the runner deletes exactly the reviewed one.
    let refused = deploy(2, Vec::new(), trusted.clone(), None).unwrap_err();
    assert_eq!(refused.code, ployz_core::RpcErrorCode::ConfirmationRequired);
    // The acceptance is bound to the reviewed version.
    let version = refused
        .details
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let second = deploy(2, vec![data], trusted, version).unwrap();
    // A same-named Docker Volume that appeared after the review is never deleted.
    held.lock().unwrap().push(on('b'));
    let ran = ployz::sdk::run_deployment(
        ployz_core::Operator::Cloud,
        Arc::clone(&store),
        second,
        RunnerId::parse("cloud-run-2").unwrap(),
        connections(),
        Ok(Default::default()),
    )
    .await
    .unwrap();
    assert_eq!(ran.status, DeploymentStatus::Applied);
    assert_eq!(*held.lock().unwrap(), [on('b')]);
    assert!(
        store
            .read(&who, &VolumesQuery::default())
            .unwrap()
            .volumes
            .is_empty()
    );
    server.abort();
}

/// A Git Service GitHub couldn't take, in an Organization that builds on GitHub only,
/// never builds on the Servers: the runner records why nothing ran.
#[tokio::test]
async fn cloud_runner_builds_nothing_on_servers_the_build_order_leaves_out() {
    use ployz_core::config::ServiceGitAccess;
    use ployz_store::{
        Actor, Admit, AuthorizedRepository, BuildOrder, Command, ConfigStore, CreateGitService,
        CreateProject, Deploy, DeploymentId, DeploymentStatus, EnvironmentId, EnvironmentRef,
        GithubBuildId, GithubEnd, OrganizationId, Outcome, ProjectId, ProjectName, RunnerId,
        SealingKey, ServiceLineageId, SetBuildOrder, Trusted,
    };
    use std::sync::Arc;

    for skipped in [
        "acme/web has no build workflow",
        "acme/web has no build workflow.",
    ] {
        let store = Arc::new(
            ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap(),
        );
        let who = Actor::system(OrganizationId::parse("org").unwrap());
        store
            .write(
                &who,
                &CreateProject {
                    id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                    name: ProjectName::parse("shop").unwrap(),
                    default_environment: EnvironmentId::parse(
                        "00000000-0000-4000-8000-000000000002",
                    )
                    .unwrap(),
                },
            )
            .unwrap();
        let trusted = Trusted {
            repositories: vec![AuthorizedRepository {
                repository: ployz_store::RepositoryName::parse("acme/web").unwrap(),
                repository_id: ployz_store::RepositoryId::parse(11).unwrap(),
                access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
                default_branch: ployz_store::BranchName::parse("main").unwrap(),
                branches: Vec::new(),
            }],
            ..Trusted::default()
        };
        store
            .write_trusted(
                &who,
                &CreateGitService {
                    id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ployz_core::ServiceName::parse("web").unwrap(),
                    repository: ployz_store::RepositoryName::parse("acme/web").unwrap(),
                    branch: None,
                },
                &trusted,
            )
            .unwrap();
        store
            .write(
                &who,
                &Command::SetBuildOrder(SetBuildOrder {
                    build_order: Some(BuildOrder::GithubOnly),
                }),
            )
            .unwrap();
        let id = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
        store
            .write_trusted(
                &who,
                &Admit::Deploy(Deploy {
                    id: id.clone(),
                    environment: EnvironmentRef::default(),
                    services: Vec::new(),
                    version: None,
                    upload: None,
                    accept_volume_loss: Vec::new(),
                    message: None,
                    prune: false,
                    no_prune: false,
                }),
                &Trusted::default(),
            )
            .unwrap();
        let web = ployz_core::ServiceName::parse("web").unwrap();
        store
            .pin(
                &id,
                &[(
                    web.clone(),
                    ployz_store::CommitSha::parse("a".repeat(40)).unwrap(),
                )]
                .into(),
            )
            .unwrap();
        let build = GithubBuildId {
            deployment: id.clone(),
            service: web,
        };
        let skipped = GithubEnd::Skipped {
            message: skipped.into(),
        };
        store.github_end(&build, None, &skipped).unwrap();

        let (address, server) = listening(DeployService::new(machine('a', "one"))).await;
        let summary = ployz::sdk::run_deployment(
            ployz_core::Operator::Cloud,
            Arc::clone(&store),
            id.clone(),
            RunnerId::parse("cloud-run-1").unwrap(),
            vec![ployz::context::Connection::tcp(address)],
            Ok(Default::default()),
        )
        .await
        .unwrap();
        assert_eq!(summary.status, DeploymentStatus::Failed);
        let Some(Outcome::NotExecuted { reason, .. }) = store
            .read(&who, &ployz_store::DeploymentQuery { id: id.clone() })
            .unwrap()
            .deployment
            .outcome
        else {
            panic!("nothing ran")
        };
        assert_eq!(
            reason,
            "Build failed. web: acme/web has no build workflow. No other Builder in your Build Order can take it"
        );
        server.abort();
    }
}

#[tokio::test]
async fn a_runner_that_cannot_reach_its_server_names_it() {
    use ployz_store::{
        Actor, Admit, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId,
        DeploymentStatus, EnvironmentId, EnvironmentRef, OrganizationId, Outcome, ProjectId,
        ProjectName, RunnerId, SealingKey, ServiceLineageId,
    };
    use std::sync::Arc;

    let store =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap());
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ServiceName::parse("web").unwrap(),
                image: Some("nginx".into()),
                template: None,
            },
        )
        .unwrap();
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let outcome = |deployment: &str, connection: serde_json::Value| {
        let (store, who) = (Arc::clone(&store), who.clone());
        let id = DeploymentId::parse(deployment).unwrap();
        async move {
            store
                .write_trusted(
                    &who,
                    &Admit::Deploy(Deploy {
                        id: id.clone(),
                        environment: EnvironmentRef::default(),
                        services: Vec::new(),
                        version: None,
                        upload: None,
                        accept_volume_loss: Vec::new(),
                        message: None,
                        prune: false,
                        no_prune: false,
                    }),
                    &ployz_store::Trusted::default(),
                )
                .unwrap();
            // As Cloud hands them over: the same descriptors the SDK parses.
            let connections = serde_json::from_value(serde_json::json!([connection])).unwrap();
            let summary = ployz::sdk::run_deployment(
                ployz_core::Operator::Cloud,
                Arc::clone(&store),
                id.clone(),
                RunnerId::parse("cloud-run-1").unwrap(),
                connections,
                Ok(Default::default()),
            )
            .await
            .unwrap();
            assert_eq!(summary.status, DeploymentStatus::Failed);
            match store
                .read(&who, &ployz_store::DeploymentQuery { id })
                .unwrap()
                .deployment
                .outcome
            {
                Some(Outcome::NotExecuted { reason, cause, .. }) => (reason, cause),
                other => panic!("{other:?}"),
            }
        }
    };

    let machine = "0123456789abcdef0123456789abcdef";
    let (reason, cause) = outcome(
        "00000000-0000-4000-8000-000000000101",
        serde_json::json!({ "tcp": closed, "machine_id": machine, "name": "box-node-09c8" }),
    )
    .await;
    assert_eq!(
        reason,
        "Server box-node-09c8 isn't answering. Check that its Ployz daemon is running, then retry."
    );
    assert!(
        cause.iter().all(|line| !line.contains("isn't answering")),
        "{cause:?}"
    );
    assert!(!cause.is_empty(), "the transport detail stays as the cause");

    // Cloud never learned its name: the Machine ID says which.
    let (reason, _) = outcome(
        "00000000-0000-4000-8000-000000000102",
        serde_json::json!({ "tcp": closed, "machine_id": machine }),
    )
    .await;
    assert_eq!(
        reason,
        format!(
            "Server {machine} isn't answering. Check that its Ployz daemon is running, then retry."
        )
    );

    // A connection that names no Server, as `--connect` gives the CLI's runner: it says
    // the one Server's daemon, not "your Servers", and the sentence is not repeated.
    let (reason, cause) = outcome(
        "00000000-0000-4000-8000-000000000103",
        serde_json::json!({ "tcp": closed }),
    )
    .await;
    assert_eq!(reason, "Ployz couldn't reach your Server's daemon.");
    assert!(
        cause.iter().all(|line| !line.contains("reach your Server")),
        "{cause:?}"
    );
}
