use super::*;
use ployz::commands::parse_command;
use ployz::deploy_history::{ClusterFingerprint, DeployHistory};
use ployz::runtime::execute_command;
use ployz_core::ops::{DeployFailureClass, DeployRunningStage, HealthCheckFailure};
use support::dind::formation::ProductCliHarness;

const UMAMI_IMAGE: &str = "ghcr.io/umami-software/umami:postgresql-latest";
const POSTGRES_IMAGE: &str = "postgres:15-alpine";
const ACCEPTANCE_NAMESPACE: &str = "v1_acceptance";

/// The published v1 acceptance journey, kept as one cluster lifetime so each
/// step proves the state left by the preceding step.
#[tokio::test]
async fn scenario_v1_acceptance_five_steps_on_fresh_machines() {
    if !dind::e2e_enabled() {
        return;
    }

    let started_at = Instant::now();
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 2).await;
    with_evidence(&core.cluster, async {
        // Step 1: form three fresh machines through the product quick-start.
        for edge in core.cluster.edges() {
            add_and_join_edge(&core, edge).await;
        }
        for id in ["core_1", "edge_2", "edge_3"] {
            wait_for_machine_observations(&core, &machine_id(id)).await;
        }
        let machines = core
            .api
            .machine_list(&MachineListRequest {})
            .await
            .expect("list the formed cluster")
            .machines;
        assert_eq!(machines.len(), 3, "fresh cluster must contain three machines");
        assert!(
            machines
                .iter()
                .all(|machine| matches!(&machine.testimony, MachineTestimony::Answered { .. })),
            "all three machines must answer with live testimony: {machines:?}"
        );
        assert!(
            started_at.elapsed() < Duration::from_secs(15 * 60),
            "three-machine quick-start exceeded 15 minutes: {:?}",
            started_at.elapsed()
        );
        let harness = ProductCliHarness::new(core.cluster.core());
        let mut config = harness.runtime_config(&core.cluster, Some(&core.material));
        config.ops_watch_timeout = Some(DEPLOY_TERMINAL_BUDGET);
        let machine_list = execute_command(
            parse_command(["machine", "list"].map(str::to_owned))
                .expect("acceptance machine list command parses"),
            &config,
        )
        .await
        .expect("acceptance machine list executes");
        assert!(
            ["core_1", "edge_2", "edge_3"]
                .iter()
                .all(|machine| machine_list.stdout.contains(machine))
                && !machine_list.stdout.contains("no answer"),
            "CLI must show all three machines answering: {}",
            machine_list.stdout
        );

        // Step 2: deploy the real two-service Compose application through the
        // product CLI so the successful pinned request enters local history.
        let compose_dir = tempfile::tempdir().expect("create acceptance compose directory");
        let compose_path = compose_dir.path().join("compose.yaml");
        std::fs::write(&compose_path, umami_compose(UMAMI_IMAGE, 3000))
            .expect("write Umami Compose file");
        execute_compose(&compose_path, &config).await;

        let history = DeployHistory::new(
            config
                .deploy_history_root
                .clone()
                .expect("product harness configures deploy history"),
            ClusterFingerprint::from_connection(
                config.nats_url.as_deref().expect("product NATS URL"),
                config.nats_ca_file.as_deref().expect("product cluster CA"),
            )
            .expect("acceptance cluster fingerprint"),
            namespace_id(ACCEPTANCE_NAMESPACE),
        );
        let initial_entries = history.load().expect("load initial deploy history");
        let [initial] = initial_entries.as_slice() else {
            panic!("successful Compose deploy must record exactly one history entry");
        };
        let initial = initial.clone();
        let initial_events = terminal_operation_events(&core, &initial.operation_id).await;
        let initial_plan = initial_events.iter().find_map(|event| match event {
            OperationEvent::DeployPlanCreated { plan, .. } => Some(plan),
            _ => None,
        });
        assert_eq!(
            initial_plan
                .expect("Compose deploy records its dependency plan")
                .services
                .iter()
                .map(|service| service.service_id.as_str())
                .collect::<Vec<_>>(),
            vec!["db", "umami"],
            "Postgres must be planned before its Umami dependent"
        );

        let all_machines = std::iter::once(core.cluster.core())
            .chain(core.cluster.edges())
            .collect::<Vec<_>>();
        let mut workloads = Vec::new();
        for machine in &all_machines {
            for container in managed_workload_containers(&core, machine).await {
                if container
                    .labels
                    .get(NAMESPACE_ID_LABEL)
                    .is_some_and(|namespace| namespace == ACCEPTANCE_NAMESPACE)
                {
                    workloads.push((*machine, container));
                }
            }
        }
        assert_eq!(
            workloads.len(),
            3,
            "Compose deploy must run two Umami replicas and one Postgres replica: {workloads:?}"
        );
        let placements = workloads
            .iter()
            .map(|(machine, _)| machine.name.as_str())
            .collect::<BTreeSet<_>>();
        assert!(
            placements.len() >= 2,
            "application placement must span at least two machines: {placements:?}"
        );

        let database_count = workloads
            .iter()
            .filter(|(_, container)| {
                container
                    .labels
                    .get(SERVICE_ID_LABEL)
                    .is_some_and(|service| service == "db")
            })
            .count();
        assert_eq!(
            database_count, 1,
            "Compose deploy must run exactly one Postgres container: {workloads:?}"
        );
        let (database_machine, database) = workloads
            .iter()
            .find(|(_, container)| {
                container
                    .labels
                    .get(SERVICE_ID_LABEL)
                    .is_some_and(|service| service == "db")
            })
            .expect("Postgres container exists");
        let mount = core
            .exec_on(
                database_machine,
                &[
                    "docker",
                    "inspect",
                    "--format",
                    "{{range .Mounts}}{{if eq .Destination \"/var/lib/postgresql/data\"}}{{.Type}}:{{.Name}}{{end}}{{end}}",
                    &database.id,
                ],
            )
            .await;
        assert!(
            mount.success() && mount.stdout.trim().starts_with("volume:"),
            "Postgres data directory must use the declared named volume: {mount:?}"
        );
        let marker = core
            .exec_on(
                database_machine,
                &[
                    "docker",
                    "exec",
                    &database.id,
                    "sh",
                    "-c",
                    "printf retained >/var/lib/postgresql/data/ployz-acceptance-marker",
                ],
            )
            .await;
        assert!(marker.success(), "write Postgres volume marker: {marker:?}");

        let app_workloads = workloads
            .iter()
            .filter(|(_, container)| {
                container
                    .labels
                    .get(SERVICE_ID_LABEL)
                    .is_some_and(|service| service == "umami")
            })
            .collect::<Vec<_>>();
        assert_eq!(app_workloads.len(), 2, "Umami must have two replicas");
        assert!(app_workloads.iter().all(|(_, container)| {
            container.env.iter().any(|env| {
                env == "DATABASE_URL=postgresql://umami:umami@db:5432/umami"
            }) && container
                .env
                .iter()
                .any(|env| env == "APP_SECRET=ployz-v1-acceptance")
        }));
        assert_service_name_database_reachability(
            &core,
            app_workloads[0].0,
            &app_workloads[0].1,
        )
        .await;

        let client = connect_core_client(
            &core,
            NatsPrincipal::Controller,
            &core.material.controller_seed,
        )
        .await
        .expect("connect controller for acceptance route intent");
        let intent = NatsIntentReader::new(client)
            .intent()
            .await
            .expect("read acceptance route intent");
        let hostname = intent
            .route_bindings
            .iter()
            .find(|binding| binding.namespace_id == namespace_id(ACCEPTANCE_NAMESPACE))
            .expect("Umami auto HTTPS route is committed")
            .target
            .hostname
            .as_str()
            .to_owned();
        let certificate_chain = match &intent.managed_lease {
            ployz_core::state::ManagedLeaseProjection::Ready { bundle, .. } => {
                bundle.certificate_chain_pem.clone()
            }
            other => panic!("managed HTTPS lease must be ready: {other:?}"),
        };
        let response = gateway_https_get(
            core.cluster.core().published.gateway_tls,
            &hostname,
            &certificate_chain,
        )
        .await
        .expect("Umami answers through public HTTPS");
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "Umami HTTPS response was not successful: {response}"
        );

        // Step 3: two independent failed operations explain their own step and
        // cause entirely through typed status and durable events.
        let bad_image = core
            .api
            .deploy_submit(
                &reserved_deploy_request(
                    &core,
                    "idem_v1_acceptance_bad_image",
                    bad_image_deploy_target(),
                )
                .await,
            )
            .await
            .expect("bad-image deploy submits");
        let bad_image_status = wait_for_terminal_deploy_status(
            &core,
            &bad_image.operation_id,
            DEPLOY_TERMINAL_BUDGET,
        )
        .await;
        let OperationStatus::Deploy {
            state: DeployOperationState::Failed { failure: bad_image_failure },
            ..
        } = bad_image_status
        else {
            panic!("bad-image deploy must fail: {bad_image_status:?}");
        };
        assert_eq!(
            bad_image_failure.failure_class(),
            DeployFailureClass::ImageResolvePullFailed
        );
        assert!(matches!(
            &bad_image_failure,
            DeployOperationFailure::ImageResolutionFailed { .. }
                | DeployOperationFailure::ArtifactUnavailable {
                    reason: ArtifactUnavailableReason::ImagePullFailed { .. },
                    ..
                }
        ));
        assert_terminal_failure_event(&core, &bad_image.operation_id, &bad_image_failure).await;

        let bad_health = core
            .api
            .deploy_submit(
                &reserved_deploy_request(
                    &core,
                    "idem_v1_acceptance_bad_health",
                    failing_healthcheck_deploy_target(),
                )
                .await,
            )
            .await
            .expect("failing-healthcheck deploy submits");
        let bad_health_status = wait_for_terminal_deploy_status(
            &core,
            &bad_health.operation_id,
            DEPLOY_TERMINAL_BUDGET,
        )
        .await;
        let OperationStatus::Deploy {
            state: DeployOperationState::Failed { failure: health_failure },
            ..
        } = bad_health_status
        else {
            panic!("failing-healthcheck deploy must fail: {bad_health_status:?}");
        };
        assert_eq!(
            health_failure.failure_class(),
            DeployFailureClass::HealthGateFailed
        );
        assert!(matches!(
            &health_failure,
            DeployOperationFailure::HealthCheckFailed {
                health_check: HealthCheckFailure::ProbeFailed { .. },
                ..
            }
        ));
        let health_events = terminal_operation_events(&core, &bad_health.operation_id).await;
        assert!(health_events.iter().any(|event| matches!(
            event,
            OperationEvent::DeployRunning {
                stage: DeployRunningStage::WaitingForHealth,
                ..
            }
        )));
        assert!(health_events.iter().any(|event| matches!(
            event,
            OperationEvent::DeployFailed {
                operation_id,
                failure,
            } if operation_id == &bad_health.operation_id && failure == &health_failure
        )));

        // Step 4: the gateway keeps its last-known-good projection while the
        // Control-Plane Core daemon is stopped.
        let stopped = core
            .exec_on(core.cluster.core(), &["systemctl", "stop", "ployzd-control"])
            .await;
        assert!(stopped.success(), "stop Control-Plane Core: {stopped:?}");
        let response = gateway_https_get(
            core.cluster.core().published.gateway_tls,
            &hostname,
            &certificate_chain,
        )
        .await
        .expect("public HTTPS survives Control-Plane Core stop");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let started = core
            .exec_on(core.cluster.core(), &["systemctl", "start", "ployzd-control"])
            .await;
        assert!(started.success(), "restart Control-Plane Core: {started:?}");
        assert_unit_active(&core, core.cluster.core(), "ployzd-control").await;
        wait_for_machine_observations(&core, &machine_id("core_1")).await;

        // Step 5a: retry is a corrected explicit deploy, with a new operation.
        let mut corrected_health = failing_healthcheck_deploy_target();
        let [corrected_service] = corrected_health.services.as_mut_slice() else {
            panic!("healthcheck fixture must contain one service");
        };
        corrected_service.runtime.healthcheck = None;
        let retry = core
            .api
            .deploy_submit(
                &reserved_deploy_request(
                    &core,
                    "idem_v1_acceptance_corrected_health",
                    corrected_health,
                )
                .await,
            )
            .await
            .expect("corrected retry submits");
        assert_ne!(retry.operation_id, bad_health.operation_id);
        let retry_status = wait_for_terminal_deploy_status(
            &core,
            &retry.operation_id,
            DEPLOY_TERMINAL_BUDGET,
        )
        .await;
        assert!(matches!(
            retry_status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed { .. },
                ..
            }
        ));

        // Step 5b: deploy a bad application revision, then replay the first
        // digest-pinned local history entry as another explicit deploy.
        std::fs::write(&compose_path, umami_compose(WORKLOAD_IMAGE, 80))
            .expect("write bad application revision");
        execute_compose(&compose_path, &config).await;
        let entries = history.load().expect("load bad revision history");
        let [recorded_initial, bad_revision] = entries.as_slice() else {
            panic!("initial and bad deploys must both be recorded: {entries:?}");
        };
        assert_eq!(recorded_initial, &initial);
        assert_ne!(bad_revision.request.services, initial.request.services);

        let rollback = parse_command(
            [
                "deploy",
                "rollback",
                "--namespace",
                ACCEPTANCE_NAMESPACE,
                "--to",
                initial.operation_id.as_str(),
            ]
            .map(str::to_owned),
        )
        .expect("acceptance rollback command parses");
        execute_command(rollback, &config)
            .await
            .expect("acceptance rollback executes");

        let entries = history.load().expect("load rollback history");
        let [_, bad_revision, rollback_entry] = entries.as_slice() else {
            panic!("rollback must append a third successful deploy: {entries:?}");
        };
        assert_ne!(rollback_entry.operation_id, initial.operation_id);
        assert_ne!(rollback_entry.operation_id, bad_revision.operation_id);
        assert_eq!(rollback_entry.request.services, initial.request.services);
        assert!(rollback_entry.request.services.iter().all(|service| {
            service.image.pinned_digest().is_some()
        }));
        let rollback_events = terminal_operation_events(&core, &rollback_entry.operation_id).await;
        assert!(rollback_events.iter().any(|event| matches!(
            event,
            OperationEvent::DeploySubmitted { target, .. }
                if target.services == initial.request.services
        )));
        assert!(
            !rollback_events
                .iter()
                .any(|event| matches!(event, OperationEvent::DeployImageResolved { .. })),
            "rollback must replay pinned images without resolving mutable tags: {rollback_events:?}"
        );
        let restored = gateway_https_get(
            core.cluster.core().published.gateway_tls,
            &hostname,
            &certificate_chain,
        )
        .await
        .expect("rolled-back Umami answers through HTTPS");
        assert!(
            restored.starts_with("HTTP/1.1 200") && restored.contains("Umami"),
            "rollback did not restore Umami: {restored}"
        );
        let mut retained_marker = false;
        for machine in &all_machines {
            for container in managed_workload_containers(&core, machine).await {
                if container
                    .labels
                    .get(NAMESPACE_ID_LABEL)
                    .is_some_and(|namespace| namespace == ACCEPTANCE_NAMESPACE)
                    && container
                        .labels
                        .get(SERVICE_ID_LABEL)
                        .is_some_and(|service| service == "db")
                {
                    let marker = core
                        .exec_on(
                            machine,
                            &[
                                "docker",
                                "exec",
                                &container.id,
                                "cat",
                                "/var/lib/postgresql/data/ployz-acceptance-marker",
                            ],
                        )
                        .await;
                    retained_marker = marker.success() && marker.stdout.trim() == "retained";
                }
            }
        }
        assert!(retained_marker, "Postgres named volume lost its marker");
    })
    .await;

    finish(core).await;
}

async fn execute_compose(path: &Path, config: &ployz::runtime::PloyzctlRuntimeConfig) {
    let command = parse_command(
        [
            "deploy".to_owned(),
            "-f".to_owned(),
            path.display().to_string(),
            "--from-registry".to_owned(),
        ]
        .into_iter(),
    )
    .expect("acceptance Compose command parses");
    execute_command(command, config)
        .await
        .expect("acceptance Compose deploy succeeds");
}

fn umami_compose(app_image: &str, app_port: u16) -> String {
    let app_healthcheck = if app_image == UMAMI_IMAGE {
        r#"node -e \"fetch('http://127.0.0.1:3000').then(r => process.exit(r.ok ? 0 : 1)).catch(() => process.exit(1))\""#
    } else {
        "wget -qO- http://127.0.0.1/ >/dev/null"
    };
    format!(
        r#"name: {ACCEPTANCE_NAMESPACE}
services:
  db:
    image: {POSTGRES_IMAGE}
    environment:
      POSTGRES_DB: umami
      POSTGRES_USER: umami
      POSTGRES_PASSWORD: umami
    volumes:
      - postgres-data:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U umami -d umami"]
      interval: 2s
      timeout: 2s
      retries: 30
  umami:
    image: {app_image}
    depends_on: [db]
    environment:
      DATABASE_URL: postgresql://umami:umami@db:5432/umami
      APP_SECRET: ployz-v1-acceptance
    deploy:
      replicas: 2
    x-ports: [{app_port}]
    healthcheck:
      test: ["CMD-SHELL", "{app_healthcheck}"]
      interval: 2s
      timeout: 2s
      retries: 60
      start_period: 5s
volumes:
  postgres-data: {{}}
"#
    )
}

fn bad_image_deploy_target() -> DeployRequest {
    let mut target = failing_healthcheck_deploy_target();
    target.namespace_id = namespace_id("bad_image");
    let [service] = target.services.as_mut_slice() else {
        panic!("healthcheck fixture must contain one service");
    };
    service.service_id = service_id("svc_bad_image");
    service.image =
        ImageReference::try_new("ghcr.io/getployz/v1-acceptance-image-that-does-not-exist:never")
            .expect("valid missing image reference");
    service.runtime.healthcheck = None;
    target
}

async fn assert_terminal_failure_event(
    core: &CoreContext,
    operation: &ployz_core::ids::OperationId,
    expected: &DeployOperationFailure,
) {
    let events = terminal_operation_events(core, operation).await;
    assert!(events.iter().any(|event| matches!(
        event,
        OperationEvent::DeployFailed {
            operation_id,
            failure,
        } if operation_id == operation && failure == expected
    )));
}

async fn assert_service_name_database_reachability(
    core: &CoreContext,
    machine: &DindMachine,
    app: &ManagedWorkloadContainer,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = None;
    while Instant::now() < deadline {
        let outcome = core
            .exec_on(
                machine,
                &[
                    "docker",
                    "exec",
                    &app.id,
                    "node",
                    "-e",
                    "const net=require('net');const s=net.createConnection(5432,'db');s.setTimeout(2000);s.on('connect',()=>process.exit(0));s.on('error',()=>process.exit(1));s.on('timeout',()=>process.exit(1));",
                ],
            )
            .await;
        if outcome.success() {
            return;
        }
        last = Some(outcome);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("Umami could not reach Postgres by service name: {last:?}");
}
