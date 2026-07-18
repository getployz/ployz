use super::*;
use ployz::commands::parse_command;
use ployz::deploy::history_store::{ClusterFingerprint, DeployHistory, DeployHistoryEntry};
use ployz::dispatcher::{PloyzctlRuntimeConfig, execute_command};
use ployz_core::operation::{DeployFailureClass, DeployRunningStage, HealthCheckFailure};
use support::dind::formation::ProductCliHarness;

const ACCEPTANCE_NAMESPACE: &str = "v1_acceptance";

struct DeployedApp {
    application: DeployHistoryEntry,
    hostname: String,
}

struct FailedDeploys {
    bad_health: ployz_core::ids::OperationId,
}

/// The published v1 acceptance journey, kept as one cluster lifetime so each
/// step proves the state left by the preceding step.
#[tokio::test]
async fn group_v1_acceptance() {
    if !dind::e2e_enabled() {
        return;
    }

    let started_at = Instant::now();
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 2).await;
    with_evidence(&core.cluster, async {
        let harness = ProductCliHarness::new(core.cluster.core());
        let mut config = harness.runtime_config(&core.cluster, Some(&core.material));
        config.ops_watch_timeout = Some(DEPLOY_TERMINAL_BUDGET);
        let compose_dir = tempfile::tempdir().expect("create acceptance Compose directory");
        let compose_path = compose_dir.path().join("compose.yaml");
        let history = deploy_history(&config);

        step_1_form_fresh_cluster(&core, &config, started_at).await;
        let app = step_2_deploy_real_application(&core, &config, &history, &compose_path).await;
        let failed = step_3_explain_failures(&core, &config, compose_dir.path()).await;
        step_4_keep_https_while_core_is_stopped(&core, &app).await;
        step_5_retry_and_rollback(&core, &config, &history, &compose_path, &app, &failed).await;
    })
    .await;

    finish(core).await;
}

async fn step_1_form_fresh_cluster(
    core: &CoreContext,
    config: &PloyzctlRuntimeConfig,
    started_at: Instant,
) {
    wait_for_machine_observations(core, &machine_id("core_1")).await;
    for (index, edge) in core.cluster.edges().iter().enumerate() {
        add_and_join_edge(core, edge).await;
        wait_for_machine_observations(core, &machine_id(&format!("edge_{}", index + 2))).await;
    }
    wait_for_fresh_peer_handshakes(core).await;
    let machines = core
        .api
        .machine_list(&MachineListRequest {})
        .await
        .expect("list the formed cluster")
        .machines;
    assert_eq!(
        machines.len(),
        3,
        "fresh cluster must contain three machines"
    );
    assert!(
        started_at.elapsed() < Duration::from_secs(15 * 60),
        "three-machine quick-start exceeded 15 minutes: {:?}",
        started_at.elapsed()
    );

    let machine_list = execute_command(
        parse_command(["machine", "list"].map(str::to_owned))
            .expect("acceptance machine list command parses"),
        config,
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
}

async fn step_2_deploy_real_application(
    core: &CoreContext,
    config: &PloyzctlRuntimeConfig,
    history: &DeployHistory,
    compose_path: &Path,
) -> DeployedApp {
    std::fs::write(compose_path, umami_compose()).expect("write Umami Compose file");
    execute_compose(compose_path, config).await;

    let application_entries = history.load().expect("load application deploy history");
    let [application] = application_entries.as_slice() else {
        panic!("full application Deploy must record exactly one history entry");
    };
    let client = connect_core_client(
        core,
        NatsPrincipal::Controller,
        &core.material.controller_seed,
    )
    .await
    .expect("connect controller for acceptance intent");

    let all_machines = all_machines(core);
    let workloads = namespace_workloads(core, &all_machines, ACCEPTANCE_NAMESPACE).await;
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

    let (database_machine, database) = workloads
        .iter()
        .find(|(_, container)| service_is(container, "db"))
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
        .filter(|(_, container)| service_is(container, "umami"))
        .collect::<Vec<_>>();
    let [_, _] = app_workloads.as_slice() else {
        panic!("Umami must have two replicas: {app_workloads:?}");
    };
    assert!(app_workloads.iter().all(|(_, container)| {
        container
            .env
            .iter()
            .any(|env| env == "DATABASE_URL=postgresql://umami:umami@db:5432/umami")
            && container
                .env
                .iter()
                .any(|env| env == "APP_SECRET=ployz-v1-acceptance")
    }));
    let app_on_other_machine = app_workloads
        .iter()
        .find(|(machine, _)| machine.name != database_machine.name)
        .expect("Umami must run on a different machine from Postgres");
    assert_service_name_database_reachability(
        core,
        app_on_other_machine.0,
        &app_on_other_machine.1,
    )
    .await;

    let intent = read_intent(&client, CONNECT_TIMEOUT)
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
    let response =
        gateway_https_get_unverified(core.cluster.core().published.gateway_tls, &hostname)
            .await
            .expect("Umami answers through public HTTPS");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "Umami HTTPS response was not successful: {response}"
    );

    DeployedApp {
        application: application.clone(),
        hostname,
    }
}

async fn step_3_explain_failures(
    core: &CoreContext,
    config: &PloyzctlRuntimeConfig,
    compose_dir: &Path,
) -> FailedDeploys {
    let bad_image_path = compose_dir.join("bad-image.yaml");
    std::fs::write(&bad_image_path, bad_image_compose()).expect("write bad-image Compose file");
    let bad_image = submit_compose(&bad_image_path, config).await;
    let bad_image_failure = terminal_deploy_failure(core, &bad_image).await;
    assert_eq!(
        bad_image_failure.failure_class(),
        DeployFailureClass::ImageResolvePullFailed,
        "unexpected bad-image failure: {bad_image_failure:?}"
    );
    assert!(matches!(
        bad_image_failure,
        DeployOperationFailure::ImageResolutionFailed { .. }
            | DeployOperationFailure::ArtifactUnavailable {
                reason: ArtifactUnavailableReason::ImagePullFailed { .. },
                ..
            }
    ));

    let bad_health_path = compose_dir.join("bad-health.yaml");
    std::fs::write(&bad_health_path, failing_health_compose())
        .expect("write failing-health Compose file");
    let bad_health = submit_compose(&bad_health_path, config).await;
    let health_failure = terminal_deploy_failure(core, &bad_health).await;
    assert_eq!(
        health_failure.failure_class(),
        DeployFailureClass::HealthGateFailed
    );
    assert!(matches!(
        health_failure,
        DeployOperationFailure::HealthCheckFailed {
            health_check: HealthCheckFailure::ProbeFailed { .. },
            ..
        }
    ));
    let health_events = terminal_operation_events(core, &bad_health).await;
    assert!(health_events.iter().any(|event| matches!(
        event,
        OperationEvent::DeployRunning {
            stage: DeployRunningStage::WaitingForHealth,
            ..
        }
    )));

    FailedDeploys { bad_health }
}

async fn step_4_keep_https_while_core_is_stopped(core: &CoreContext, app: &DeployedApp) {
    let stopped = core
        .exec_on(
            core.cluster.core(),
            &["systemctl", "stop", "ployzd-control"],
        )
        .await;
    assert!(stopped.success(), "stop Control-Plane Core: {stopped:?}");
    let response =
        gateway_https_get_unverified(core.cluster.core().published.gateway_tls, &app.hostname)
            .await
            .expect("public HTTPS survives Control-Plane Core stop");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");

    let started = core
        .exec_on(
            core.cluster.core(),
            &["systemctl", "start", "ployzd-control"],
        )
        .await;
    assert!(started.success(), "restart Control-Plane Core: {started:?}");
    assert_unit_active(core, core.cluster.core(), "ployzd-control").await;
    wait_for_machine_observations(core, &machine_id("core_1")).await;
    wait_for_fresh_peer_handshakes(core).await;
}

async fn step_5_retry_and_rollback(
    core: &CoreContext,
    config: &PloyzctlRuntimeConfig,
    history: &DeployHistory,
    compose_path: &Path,
    app: &DeployedApp,
    failed: &FailedDeploys,
) {
    let corrected_health_path = compose_path.with_file_name("corrected-health.yaml");
    std::fs::write(&corrected_health_path, corrected_health_compose())
        .expect("write corrected-health Compose file");
    let retry = submit_compose(&corrected_health_path, config).await;
    assert_ne!(retry, failed.bad_health);
    let retry_status = wait_for_terminal_deploy_status(core, &retry, DEPLOY_TERMINAL_BUDGET).await;
    assert!(matches!(
        retry_status,
        OperationStatus::Deploy {
            state: DeployOperationState::Completed { .. },
            ..
        }
    ));

    std::fs::write(compose_path, bad_deploy_input_compose()).expect("write bad deploy input");
    execute_compose(compose_path, config).await;
    let entries = history.load().expect("load bad deploy-input history");
    let [recorded_application, bad_deploy_entry] = entries.as_slice() else {
        panic!("application Deploy and bad Deploy must be recorded: {entries:?}");
    };
    assert_eq!(recorded_application, &app.application);
    assert_ne!(
        bad_deploy_entry.request.services,
        app.application.request.services
    );

    let rollback = parse_command(
        [
            "deploy",
            "rollback",
            "--namespace",
            ACCEPTANCE_NAMESPACE,
            "--to",
            app.application.operation_id.as_str(),
        ]
        .map(str::to_owned),
    )
    .expect("acceptance rollback command parses");
    execute_command(rollback, config)
        .await
        .expect("acceptance rollback executes");

    let entries = history.load().expect("load rollback history");
    let [recorded_application, bad_deploy_entry, rollback_entry] = entries.as_slice() else {
        panic!("rollback must append after application Deploy and bad Deploy: {entries:?}");
    };
    assert_eq!(recorded_application, &app.application);
    assert_ne!(rollback_entry.operation_id, app.application.operation_id);
    assert_ne!(rollback_entry.operation_id, bad_deploy_entry.operation_id);
    assert_eq!(
        rollback_entry.request.services,
        app.application.request.services
    );

    let restored =
        gateway_https_get_unverified(core.cluster.core().published.gateway_tls, &app.hostname)
            .await
            .expect("rolled-back Umami answers through HTTPS");
    assert!(
        restored.starts_with("HTTP/1.1 200") && restored.contains("Umami"),
        "rollback did not restore Umami: {restored}"
    );

    let all_machines = all_machines(core);
    let workloads = namespace_workloads(core, &all_machines, ACCEPTANCE_NAMESPACE).await;
    let mut retained_marker = false;
    for (machine, container) in workloads
        .iter()
        .filter(|(_, container)| service_is(container, "db"))
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
    assert!(retained_marker, "Postgres named volume lost its marker");
}

fn deploy_history(config: &PloyzctlRuntimeConfig) -> DeployHistory {
    DeployHistory::new(
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
    )
}

async fn execute_compose(path: &Path, config: &PloyzctlRuntimeConfig) {
    let command = compose_command(path, false);
    execute_command(command, config)
        .await
        .expect("acceptance Compose deploy succeeds");
}

async fn submit_compose(
    path: &Path,
    config: &PloyzctlRuntimeConfig,
) -> ployz_core::ids::OperationId {
    let output = execute_command(compose_command(path, true), config)
        .await
        .expect("acceptance Compose deploy submits");
    let operation = output
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("operation "))
        .expect("detached deploy reports its operation id");
    ployz_core::ids::OperationId::try_new(operation).expect("CLI reports a valid operation id")
}

fn compose_command(path: &Path, detach: bool) -> ployz::commands::PloyzctlCommand {
    let mut args = vec![
        "deploy".to_owned(),
        "-f".to_owned(),
        path.display().to_string(),
    ];
    if detach {
        args.push("--detach".to_owned());
    }
    parse_command(args).expect("acceptance Compose command parses")
}

async fn terminal_deploy_failure(
    core: &CoreContext,
    operation: &ployz_core::ids::OperationId,
) -> DeployOperationFailure {
    let status = wait_for_terminal_deploy_status(core, operation, DEPLOY_TERMINAL_BUDGET).await;
    let OperationStatus::Deploy {
        state: DeployOperationState::Failed { failure },
        ..
    } = status
    else {
        panic!("acceptance deploy must fail: {status:?}");
    };
    let events = terminal_operation_events(core, operation).await;
    assert!(events.iter().any(|event| matches!(
        event,
        OperationEvent::DeployFailed {
            operation_id,
            failure: event_failure,
        } if operation_id == operation && event_failure == &failure
    )));
    failure
}

fn all_machines(core: &CoreContext) -> Vec<&DindMachine> {
    std::iter::once(core.cluster.core())
        .chain(core.cluster.edges())
        .collect()
}

async fn namespace_workloads<'a>(
    core: &CoreContext,
    machines: &[&'a DindMachine],
    namespace: &str,
) -> Vec<(&'a DindMachine, ManagedWorkloadContainer)> {
    let mut workloads = Vec::new();
    for machine in machines {
        for container in managed_workload_containers(core, machine).await {
            if container
                .labels
                .get(NAMESPACE_ID_LABEL)
                .is_some_and(|value| value == namespace)
            {
                workloads.push((*machine, container));
            }
        }
    }
    workloads
}

fn service_is(container: &ManagedWorkloadContainer, service: &str) -> bool {
    container
        .labels
        .get(SERVICE_ID_LABEL)
        .is_some_and(|value| value == service)
}

fn database_service_compose() -> String {
    format!(
        r#"  db:
    image: ${{PLOYZ_DIND_POSTGRES_IMAGE:-postgres:15-alpine}}
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
"#
    )
}

fn umami_compose() -> &'static str {
    include_str!("../fixtures/v1-acceptance-umami.yaml")
}

fn bad_deploy_input_compose() -> String {
    let database = database_service_compose();
    format!(
        r#"name: {ACCEPTANCE_NAMESPACE}
services:
{database}  umami:
    image: {WORKLOAD_IMAGE}
    deploy:
      replicas: 2
    x-ports: [auto:bad-input:80]
    healthcheck:
      test: ["CMD-SHELL", "wget -qO- http://127.0.0.1/ >/dev/null"]
      interval: 2s
      timeout: 2s
      retries: 60
volumes:
  postgres-data: {{}}
"#
    )
}

fn bad_image_compose() -> &'static str {
    r#"name: bad_image
services:
  app:
    image: ghcr.io/getployz/v1-acceptance-image-that-does-not-exist:never
"#
}

fn failing_health_compose() -> String {
    format!(
        r#"name: bad_health
services:
  app:
    image: {WORKLOAD_IMAGE}
    healthcheck:
      test: ["CMD-SHELL", "exit 1"]
      interval: 1s
      timeout: 1s
      retries: 2
"#
    )
}

fn corrected_health_compose() -> String {
    format!(
        r#"name: bad_health
services:
  app:
    image: {WORKLOAD_IMAGE}
"#
    )
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
