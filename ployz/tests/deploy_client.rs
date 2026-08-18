//! Session-level preview/confirm/run behaviour against a fake Machine.
#[path = "deploy_client/support.rs"]
mod support;
use support::*;

use std::sync::atomic::Ordering;

use ployz::deploy::{
    DeployError, DeployEvent, DeployIntent, DeployOperation, DeployOutcome, DeployWarning,
    ExecutionError, FailedOperation, OperationStatus, PlanError, PruneRefusal,
};
use ployz_core::{ContainerId, OperationPhase, ProjectName, RequestedServiceSpec};
use tokio_util::sync::CancellationToken;

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
    let (mut client, server) = connected(DeployService::new(machine.clone())).await;
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
    server.abort();
}

#[tokio::test]
async fn deploy_returns_the_completed_prefix_failed_op_and_unexecuted_suffix() {
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
            operation: DeployOperation::CreateVolume { volume, .. },
            error: ExecutionError::Machine { .. },
        } if volume.reference.as_str() == "data"
    ));
    assert_eq!(unexecuted.len(), 1);
    assert!(matches!(
        unexecuted.first(),
        Some(DeployOperation::RunContainer { spec, .. }) if spec.name.as_str() == "web"
    ));
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
                "hostname": { "kind": "assign_from_cluster_domain" },
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
            ployz_core::PortPublication::Ingress {
                hostname: ployz_core::IngressHostname::Explicit { hostname },
                ..
            } => Some(hostname.as_str()),
            ployz_core::PortPublication::Ingress {
                hostname: ployz_core::IngressHostname::AssignFromClusterDomain,
                ..
            }
            | ployz_core::PortPublication::Host { .. } => None,
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
            DeployWarning::IngressHostname(message) => {
                message.contains("preview-deploy.invalid")
                    && message.contains("192.0.2.1")
                    && !message.to_ascii_lowercase().contains("certificate")
            }
            DeployWarning::ObservationFailed { .. } | DeployWarning::ObservationOmitted { .. } => {
                false
            }
        }),
        "DNS warning must match the CLI body: {:?}",
        preview.warnings
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
            "hostname": { "kind": "assign_from_cluster_domain" },
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
async fn confirm_emits_all_pending_before_any_machine_rpc() {
    let machine = machine('a', "one");
    let (mut client, server) = connected(DeployService::new(machine)).await;
    let preview = client
        .preview(DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
            spec("web"),
            skip_health(),
        ))
        .await
        .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let outcome = client
        .confirm(&preview, &CancellationToken::new(), Some(tx))
        .await;
    let first = rx.recv().await.expect("first progress event");
    let DeployEvent::Progress {
        rows, completed, ..
    } = &first
    else {
        panic!("expected progress: {first:?}");
    };
    assert_eq!(*completed, 0);
    assert!(
        rows.iter()
            .all(|row| matches!(row.status, OperationStatus::Pending)),
        "first event must be all pending: {rows:?}"
    );
    assert!(matches!(outcome, DeployOutcome::Success { .. }));
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
    debug.container_id = ContainerId::parse("2".repeat(64)).unwrap();
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
    debug.container_id = ContainerId::parse("2".repeat(64)).unwrap();
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
