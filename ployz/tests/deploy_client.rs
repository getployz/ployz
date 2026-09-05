//! Session-level preview/confirm/run behaviour against a fake Machine.
#[path = "deploy_client/support.rs"]
mod support;
use support::*;

use std::{num::NonZeroU64, process::Stdio, sync::atomic::Ordering, time::Duration};

use ployz::deploy::{
    DeployError, DeployEvent, DeployIntent, DeployOperation, DeployOutcome, DeployWarning,
    ExecutionError, FailedOperation, OperationStatus, PlanError, PruneRefusal, VolumeFate,
};
use ployz_core::{
    ContainerId, IngressProxyBackend, MachineId, MachineStorageObservation, OperationPhase,
    ProjectName, ProvisionedVolumeMaximumBytes, QualifiedService, RequestedServiceSpec,
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
async fn ingress_deploy_inherits_and_cannot_change_the_cluster_backend() {
    for (backend, image) in [
        (IngressProxyBackend::Caddy, "caddy:test"),
        (IngressProxyBackend::Zentinel, "zentinel:test"),
        (IngressProxyBackend::Envoy, "envoy:test"),
    ] {
        let service = DeployService::new(machine('a', "one")).with_ingress_backend(Some(backend));
        let created = service.created_specs();
        let (address, server) = listening(service).await;
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
        command.args([
            "--connect",
            &format!("tcp://{address}"),
            "ingress",
            "deploy",
            "--skip-health",
        ]);
        command.args(["--image", image]);
        let output = command.output().await.unwrap();
        assert!(
            output.status.success(),
            "{backend}: stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let specs = created.lock().unwrap();
        assert_eq!(specs.len(), 1);
        let spec = specs.first().unwrap();
        match backend {
            IngressProxyBackend::Caddy => {
                assert_eq!(spec.container.image, "caddy:test");
                assert_eq!(
                    spec.container.command,
                    ["caddy", "run", "-c", "/config/caddy/Caddyfile"]
                );
                assert_eq!(spec.ports.len(), 3);
            }
            IngressProxyBackend::Zentinel => {
                assert_eq!(spec.container.image, "zentinel:test");
                assert_eq!(spec.container.command, ["-c", "/config/zentinel.kdl"]);
                assert_eq!(spec.container.cap_add, ["NET_BIND_SERVICE"]);
                assert_eq!(spec.container.cap_drop, ["ALL"]);
                assert!(spec.ports.is_empty());
            }
            IngressProxyBackend::Envoy => {
                assert_eq!(spec.container.image, "envoy:test");
                assert_eq!(
                    spec.container.command,
                    ["envoy", "-c", "/config/bootstrap.yaml"]
                );
                assert!(spec.container.cap_add.is_empty());
                assert_eq!(spec.ports.len(), 2);
            }
        }
        server.abort();
    }
}

#[tokio::test]
async fn ingress_deploy_refuses_a_missing_cluster_backend_before_mutation() {
    let service = DeployService::new(machine('a', "one")).with_ingress_backend(None);
    let mutations = service.mutating_rpcs();
    let (address, server) = listening(service).await;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "ingress",
            "deploy",
            "--image",
            "example.test/ingress",
        ])
        .output()
        .await
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Cluster Ingress Proxy Backend is missing")
    );
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
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

        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args([
                "--connect",
                &format!("tcp://{address}"),
                "service",
                action,
                "web",
                "api",
            ])
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
    let mut volumes = requested.volume_graph.volumes().to_vec();
    let mounts = requested.volume_graph.mounts().to_vec();
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
    requested.volume_graph = ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap();
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
