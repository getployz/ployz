use std::{
    process::{self, Command},
    time::Duration,
};

use ployz_core::{
    CADDY_VERIFY_PATH, ContainerAction, ContainerId, ContainerKind, GetCaddyConfigRequest,
    ListMachinesRequest, Machine, MachineId, MachineTarget, MembershipObservation,
    RequestedServiceSpec, ResolvedServiceSpec, ServiceId, StartContainerRequest,
    StopContainerRequest, op,
};
use ployz_testkit::{Cluster, ClusterPlan};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn caddy_projects_and_loads_cluster_services_on_three_machines() {
    let plan = ClusterPlan::new(&format!("l3-caddy-{}", process::id()), 3).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    let machines = cluster.initialize_three().await.unwrap();
    let direct = cluster.api_address(0).unwrap();
    let mut client = ployz::connect::connect(
        std::path::Path::new("/missing-ployz-test-config"),
        Some(&direct),
        None,
    )
    .await
    .unwrap();

    for machine in &machines {
        let config = wait_config(&mut client, machine, |config| {
            config.contains("admin API is not reachable")
        })
        .await;
        assert!(config.contains(CADDY_VERIFY_PATH));
    }

    cli(
        &direct,
        &[
            "caddy",
            "deploy",
            "--image",
            "caddy:2.10.2",
            "--machine",
            machines[0].id.as_str(),
        ],
    );
    let caddy_id = wait_service(&mut client, "caddy", 1).await;
    assert!(
        wait_running(&mut client, &caddy_id, 1)
            .await
            .iter()
            .all(|container| container.resolved_spec.container.image == "caddy:2.10.2")
    );
    cli(&direct, &["caddy", "deploy", "--image", "caddy:2.10.2"]);
    assert_eq!(wait_service(&mut client, "caddy", 3).await, caddy_id);
    assert!(
        wait_running(&mut client, &caddy_id, 3)
            .await
            .iter()
            .all(|container| container.resolved_spec.container.image == "caddy:2.10.2")
    );

    let api_id = ServiceId::random();
    let api: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": api_id,
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 3 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sh", "-c", "while true; do printf 'HTTP/1.1 200 OK\\r\\nContent-Length: 3\\r\\n\\r\\nok\\n' | nc -l -p 8080; done"],
            "pull_policy": "missing"
        },
        "ports": [{
            "mode": "ingress",
            "hostname": { "kind": "explicit", "hostname": "example.test" },
            "load_balancer_port": 80,
            "container_port": 8080,
            "http_protocol": "http"
        }],
        "caddy_config": "custom.example {\n\trespond \"custom\" 200\n}"
    }))
    .unwrap();
    let mut api_containers = Vec::new();
    for machine in &machines {
        api_containers.push(create_and_start(&mut client, machine, api.clone()).await);
    }
    let observations = wait_running(&mut client, &api_id, 3).await;
    for (index, machine) in machines.iter().enumerate() {
        let config = wait_config(&mut client, machine, |config| {
            config.contains("http://example.test")
                && config.contains("custom.example {")
                && config.contains("respond \"custom\" 200")
                && observations.iter().all(|container| {
                    config.contains(&format!("{}:8080", container.address.unwrap().0))
                })
        })
        .await;
        assert_eq!(
            cli(
                &direct,
                &["caddy", "config", "--machine", machine.id.as_str()],
            ),
            config
        );
        assert!(!config.contains("admin API is not reachable"));
        assert_eq!(
            cluster
                .machine_shell(index, "curl -fsS -H 'Host: example.test' http://127.0.0.1",)
                .unwrap()
                .trim(),
            "ok"
        );
        assert_eq!(
            cluster
                .machine_shell(
                    index,
                    &format!("curl -fsS http://127.0.0.1{CADDY_VERIFY_PATH}")
                )
                .unwrap()
                .trim(),
            machine.id.as_str()
        );
        assert_eq!(
            cluster
                .machine_shell(
                    index,
                    &format!(
                        "curl -fsS -H 'Host: example.test' http://127.0.0.1{CADDY_VERIFY_PATH}"
                    ),
                )
                .unwrap()
                .trim(),
            "ok"
        );
        cluster
            .machine_shell(index, "test ! -e /var/lib/ployz/caddy/caddy.json")
            .unwrap();
    }

    let logs = run_cli(&direct, &["caddy", "logs", "--tail", "1"]);
    let logs = [logs.stdout, logs.stderr].concat();
    assert!(String::from_utf8(logs).unwrap().contains(" caddy/"));

    assert_health_transition(&mut client, &machines, &api_containers, &observations).await;

    assert_start_first_gap(&cluster, &mut client, &machines).await;
    assert_failed_load_retry(&cluster, &mut client, &machines[0]).await;
    assert_membership_blind(&cluster, &mut client, &machines, &observations).await;
    assert_invalid_template(&mut client, &machines[0]).await;
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn certificate_material_in_cluster_state_is_served_without_restart() {
    let plan = ClusterPlan::new(&format!("l3-certs-{}", process::id()), 2).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.wait_ready(Duration::from_secs(30)).await.unwrap();
    let first = cluster.initialize_first().await.unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if cluster
                .machines(0)
                .await
                .is_ok_and(|machines| machines.len() == 1)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
    let direct = cluster.api_address(0).unwrap();
    let mut client = ployz::connect::connect(
        std::path::Path::new("/missing-ployz-test-config"),
        Some(&direct),
        None,
    )
    .await
    .unwrap();

    cli(&direct, &["caddy", "deploy", "--image", "caddy:2.10.2"]);
    wait_service(&mut client, "caddy", 1).await;

    let api: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": ServiceId::random(),
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sh", "-c", "while true; do printf 'HTTP/1.1 200 OK\\r\\nContent-Length: 3\\r\\n\\r\\nok\\n' | nc -l -p 8080; done"],
            "pull_policy": "missing"
        },
        "ports": [{
            "mode": "ingress",
            "hostname": { "kind": "explicit", "hostname": "secure.example.test" },
            "load_balancer_port": 443,
            "container_port": 8080,
            "http_protocol": "https"
        }]
    }))
    .unwrap();
    let service_id = api.service_id;
    create_and_start(&mut client, &first, api).await;
    wait_running(&mut client, &service_id, 1).await;
    let before = wait_config(&mut client, &first, |config| {
        config.contains("auto_https off")
    })
    .await;
    assert!(
        !before.contains("tls /config/certs/secure.example.test"),
        "{before}"
    );

    let (first_cert, first_key) = self_signed_material("secure.example.test", "first");
    publish_certificate_row(&cluster, 0, "secure.example.test", &first_cert, &first_key);
    wait_config(&mut client, &first, |config| {
        config.contains("tls /config/certs/secure.example.test-")
    })
    .await;
    assert_eq!(curl_https(&cluster, 0, &first_cert).trim(), "ok");

    let (second_cert, second_key) = self_signed_material("secure.example.test", "second");
    publish_certificate_row(
        &cluster,
        0,
        "secure.example.test",
        &second_cert,
        &second_key,
    );
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if curl_https(&cluster, 0, &second_cert).trim() == "ok" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
    assert!(curl_https(&cluster, 0, &first_cert).trim() != "ok");

    let second = cluster.add_machine(0, 1, "machine-2").await.unwrap();
    cli(
        &direct,
        &[
            "caddy",
            "deploy",
            "--image",
            "caddy:2.10.2",
            "--machine",
            second.id.as_str(),
        ],
    );
    wait_config(&mut client, &second, |config| {
        config.contains("tls /config/certs/secure.example.test-")
    })
    .await;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if curl_https(&cluster, 1, &second_cert).trim() == "ok" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
}

async fn assert_health_transition(
    client: &mut ployz::connect::Client,
    machines: &[Machine; 3],
    containers: &[ployz_core::ContainerId],
    observations: &[ployz_core::ContainerObservation],
) {
    let removed_address = observations
        .iter()
        .find(|container| container.machine_id == machines[1].id)
        .unwrap()
        .address
        .unwrap();
    for action in [ContainerAction::Stop, ContainerAction::Start] {
        change_container(
            client,
            machines[1].id,
            *containers.get(1).expect("three API containers"),
            action,
            (action == ContainerAction::Stop).then_some(0),
        )
        .await;
        wait_config(client, &machines[0], |config| {
            config.contains(&format!("{}:8080", removed_address.0))
                == (action == ContainerAction::Start)
        })
        .await;
    }
}

async fn assert_failed_load_retry(
    cluster: &Cluster,
    client: &mut ployz::connect::Client,
    machine: &Machine,
) {
    let stable = client
        .call::<op::GetCaddyConfig>(
            GetCaddyConfigRequest {},
            Some(&MachineTarget::from(&machine.id)),
        )
        .await
        .unwrap()
        .caddyfile;
    let load_failure: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": ServiceId::random(),
        "name": "load-failure",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sleep", "300"],
            "pull_policy": "missing"
        },
        "caddy_config": "load-failure.example {\n\ttls /missing/cert.pem /missing/key.pem\n\trespond bad\n}"
    }))
    .unwrap();
    let rejected = create_and_start(client, machine, load_failure).await;
    wait_log_count(cluster, "failed to update Caddy configuration", 1).await;
    let tick: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": ServiceId::random(),
        "name": "tick",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sleep", "300"],
            "pull_policy": "missing"
        }
    }))
    .unwrap();
    create_and_start(client, machine, tick).await;
    wait_log_count(cluster, "failed to update Caddy configuration", 2).await;
    assert_eq!(
        client
            .call::<op::GetCaddyConfig>(
                GetCaddyConfigRequest {},
                Some(&MachineTarget::from(&machine.id)),
            )
            .await
            .unwrap()
            .caddyfile,
        stable
    );
    assert_eq!(
        cluster
            .machine_shell(0, "curl -fsS -H 'Host: example.test' http://127.0.0.1")
            .unwrap()
            .trim(),
        "ok"
    );
    client
        .call::<op::StopContainer>(
            StopContainerRequest {
                container_id: rejected,
                signal: None,
                grace_period_seconds: Some(0),
            },
            Some(&MachineTarget::from(&machine.id)),
        )
        .await
        .unwrap();
    wait_config(client, machine, |config| config != stable).await;
}

async fn assert_membership_blind(
    cluster: &Cluster,
    client: &mut ployz::connect::Client,
    machines: &[Machine; 3],
    observations: &[ployz_core::ContainerObservation],
) {
    let retained_address = observations
        .iter()
        .find(|container| container.machine_id == machines[2].id)
        .unwrap()
        .address
        .unwrap();
    cluster.stop(2).unwrap();
    wait_down(cluster, &machines[2]).await;
    let retained = client
        .call::<op::GetCaddyConfig>(
            GetCaddyConfigRequest {},
            Some(&MachineTarget::from(&machines[0].id)),
        )
        .await
        .unwrap();
    assert!(
        retained
            .caddyfile
            .contains(&format!("{}:8080", retained_address.0))
    );
}

async fn assert_invalid_template(client: &mut ployz::connect::Client, machine: &Machine) {
    let broken: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": ServiceId::random(),
        "name": "broken",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sleep", "300"],
            "pull_policy": "missing"
        },
        "caddy_config": "{{unknown}}"
    }))
    .unwrap();
    create_and_start(client, machine, broken).await;
    let config = wait_config(client, machine, |config| {
        config.contains("Service 'broken': rendering failed")
    })
    .await;
    assert!(config.contains("http://example.test"));
}

fn cli(direct: &str, args: &[&str]) -> String {
    String::from_utf8(run_cli(direct, args).stdout).unwrap()
}

fn run_cli(direct: &str, args: &[&str]) -> process::Output {
    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            direct,
            "--ployz-config",
            "/missing-ployz-test-config",
        ])
        .args(args)
        .env("PLOYZ_HEALTH_MONITOR_PERIOD", "0s")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ployz {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

async fn wait_service(client: &mut ployz::connect::Client, name: &str, count: usize) -> ServiceId {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let live = client.live_services().await.unwrap();
            if let Some(service) = live.services.iter().find(|service| {
                service.containers.first().is_some_and(|container| {
                    container.as_observation().service_name.as_str() == name
                }) && service.containers.len() == count
            }) {
                return service.service_id;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap()
}

async fn assert_start_first_gap(
    cluster: &Cluster,
    client: &mut ployz::connect::Client,
    machines: &[Machine; 3],
) {
    let requested = |machine: &Machine, response: &str| {
        serde_json::from_value::<RequestedServiceSpec>(serde_json::json!({
            "name": "switch",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": {
                "image": "alpine:3.23.3",
                "command": ["sh", "-c", format!("while true; do printf 'HTTP/1.1 200 OK\\r\\nContent-Length: {}\\r\\n\\r\\n{}\\n' | nc -l -p 8081; done", response.len() + 1, response)],
                "pull_policy": "missing"
            },
            "placement": { "machines": [machine.id] },
            "ports": [{
                "mode": "ingress",
                "hostname": { "kind": "explicit", "hostname": "switch.test" },
                "load_balancer_port": 80,
                "container_port": 8081,
                "http_protocol": "http"
            }],
            "update": { "order": "start_first" }
        }))
        .unwrap()
    };
    let first = requested(&machines[0], "old");
    let service_id = deploy(client, &first).await;
    let old_address = wait_running(client, &service_id, 1)
        .await
        .into_iter()
        .next()
        .unwrap()
        .address
        .unwrap();
    wait_config(client, &machines[2], |config| {
        config.contains(&format!("{}:8081", old_address.0))
    })
    .await;

    cluster
        .machine_shell(2, "kill -STOP $(cat /run/ployzd.pid)")
        .unwrap();
    let replacement = requested(&machines[1], "new");
    assert_eq!(deploy(client, &replacement).await, service_id);
    let new_address = wait_running(client, &service_id, 1)
        .await
        .into_iter()
        .next()
        .unwrap()
        .address
        .unwrap();
    let delayed = cluster
        .machine_shell(2, "cat /var/lib/ployz/caddy/Caddyfile")
        .unwrap();
    assert!(delayed.contains(&format!("{}:8081", old_address.0)));
    assert!(!delayed.contains(&format!("{}:8081", new_address.0)));
    assert!(
        cluster
            .machine_shell(2, "curl -fsS -H 'Host: switch.test' http://127.0.0.1",)
            .is_err()
    );

    cluster
        .machine_shell(2, "kill -CONT $(cat /run/ployzd.pid)")
        .unwrap();
    wait_config(client, &machines[2], |config| {
        config.contains(&format!("{}:8081", new_address.0))
            && !config.contains(&format!("{}:8081", old_address.0))
    })
    .await;
    assert_eq!(
        cluster
            .machine_shell(2, "curl -fsS -H 'Host: switch.test' http://127.0.0.1",)
            .unwrap()
            .trim(),
        "new"
    );
}

async fn deploy(
    client: &mut ployz::connect::Client,
    requested: &RequestedServiceSpec,
) -> ServiceId {
    let machines = client
        .call::<op::ListMachines>(ListMachinesRequest {}, None)
        .await
        .unwrap()
        .machines;
    let live = client.live_services().await.unwrap();
    let snapshot = ployz::deploy::DeploySnapshot {
        machines,
        containers: live
            .containers
            .successes
            .into_iter()
            .flat_map(|success| success.value)
            .collect(),
        volumes: Vec::new(),
    };
    let plan = ployz::deploy::plan_deploy(
        &ployz::deploy::DeployIntent::apply_all(
            [requested],
            ployz::deploy::PlanOptions {
                skip_health_monitor: true,
                ..Default::default()
            },
        ),
        &snapshot,
    )
    .unwrap();
    let outcome = ployz::deploy::execute_plan(&plan, client, &CancellationToken::new()).await;
    assert!(outcome.failed.is_none(), "{outcome:?}");
    plan.operations
        .iter()
        .find_map(|operation| match operation {
            ployz::deploy::DeployOperation::RunContainer { spec, .. }
            | ployz::deploy::DeployOperation::RunHook { spec, .. } => Some(spec.service_id),
            ployz::deploy::DeployOperation::ReplaceContainer(replacement) => {
                Some(replacement.spec.service_id)
            }
            ployz::deploy::DeployOperation::CreateVolume { .. }
            | ployz::deploy::DeployOperation::StopContainer { .. }
            | ployz::deploy::DeployOperation::RemoveContainer { .. }
            | ployz::deploy::DeployOperation::StopHook { .. } => None,
        })
        .expect("deployed operations carry a Service ID")
}

async fn wait_down(cluster: &Cluster, machine: &Machine) {
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if cluster.machines(0).await.is_ok_and(|observations| {
                observations.iter().any(|observation| {
                    observation.machine.id == machine.id
                        && observation.membership == MembershipObservation::Down
                })
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_log_count(cluster: &Cluster, needle: &str, count: usize) {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if cluster
                .logs(0)
                .is_ok_and(|logs| logs.matches(needle).count() >= count)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
}

async fn create_and_start(
    client: &mut ployz::connect::Client,
    machine: &Machine,
    spec: ResolvedServiceSpec,
) -> ployz_core::ContainerId {
    let created = client
        .create_container(machine.id, ContainerKind::ServiceContainer, spec)
        .await
        .unwrap();
    start_container(client, machine.id, created.container_id).await;
    created.container_id
}

async fn start_container(
    client: &mut ployz::connect::Client,
    machine_id: MachineId,
    container_id: ContainerId,
) {
    client
        .call::<op::StartContainer>(
            StartContainerRequest { container_id },
            Some(&MachineTarget::from(&machine_id)),
        )
        .await
        .unwrap();
}

async fn change_container(
    client: &mut ployz::connect::Client,
    machine_id: MachineId,
    container_id: ContainerId,
    action: ContainerAction,
    grace_period_seconds: Option<i32>,
) {
    match action {
        ContainerAction::Start => start_container(client, machine_id, container_id).await,
        ContainerAction::Stop => {
            client
                .call::<op::StopContainer>(
                    StopContainerRequest {
                        container_id,
                        signal: None,
                        grace_period_seconds,
                    },
                    Some(&MachineTarget::from(&machine_id)),
                )
                .await
                .unwrap();
        }
        ContainerAction::Remove => unreachable!("health transition only starts and stops"),
    }
}

async fn wait_running(
    client: &mut ployz::connect::Client,
    service_id: &ServiceId,
    count: usize,
) -> Vec<ployz_core::ContainerObservation> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let live = client.live_services().await.unwrap();
        if let Some(service) = live
            .services
            .iter()
            .find(|service| service.service_id == *service_id)
        {
            let running = service
                .containers
                .iter()
                .map(|container| container.as_observation().clone())
                .filter(|container| {
                    matches!(
                        container.runtime,
                        ployz_core::ContainerRuntimeObservation::Running {
                            health: ployz_core::HealthObservation::Healthy
                                | ployz_core::HealthObservation::NotConfigured
                        }
                    )
                })
                .collect::<Vec<_>>();
            if running.len() == count {
                return running;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Service did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_config(
    client: &mut ployz::connect::Client,
    machine: &Machine,
    expected: impl Fn(&str) -> bool,
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        match client
            .call::<op::GetCaddyConfig>(
                GetCaddyConfigRequest {},
                Some(&MachineTarget::from(&machine.id)),
            )
            .await
        {
            Ok(config) if expected(&config.caddyfile) => return config.caddyfile,
            Ok(config) if tokio::time::Instant::now() >= deadline => {
                panic!("Caddyfile did not converge:\n{}", config.caddyfile)
            }
            Err(error) if tokio::time::Instant::now() >= deadline => {
                panic!("Caddyfile was unavailable: {error}")
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn self_signed_material(hostname: &str, label: &str) -> (String, String) {
    let root = std::env::temp_dir().join(format!(
        "ployz-l3-cert-{hostname}-{label}-{}",
        process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let cert = root.join("cert.pem");
    let key = root.join("key.pem");
    let status = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-keyout",
            key.to_str().unwrap(),
            "-out",
            cert.to_str().unwrap(),
            "-days",
            "1",
            "-nodes",
            "-subj",
            &format!("/CN={label}.{hostname}"),
            "-addext",
            &format!("subjectAltName=DNS:{hostname}"),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "openssl failed for {label}");
    (
        std::fs::read_to_string(cert).unwrap(),
        std::fs::read_to_string(key).unwrap(),
    )
}

fn publish_certificate_row(
    cluster: &Cluster,
    index: usize,
    hostname: &str,
    certificate: &str,
    private_key: &str,
) {
    let body = serde_json::json!({
        "certificate": certificate,
        "private_key": private_key,
    });
    let payload = serde_json::to_string(&serde_json::json!([{
        "query": "INSERT INTO certificates (hostname, body, updated_at) VALUES (?, ?, datetime('now')) ON CONFLICT (hostname) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
        "params": [hostname, body.to_string()],
    }]))
    .unwrap();
    let quoted = format!("'{}'", payload.replace('\'', "'\"'\"'"));
    cluster
        .machine_shell(
            index,
            &format!(
                r#"token=$(cat /var/lib/ployz/corrosion/.api-token); curl --fail --silent --show-error --http2-prior-knowledge -H "Authorization: Bearer $token" -H 'Content-Type: application/json' --data-binary {quoted} http://127.0.0.1:51002/v1/transactions"#
            ),
        )
        .unwrap();
}

fn curl_https(cluster: &Cluster, index: usize, certificate: &str) -> String {
    let quoted_cert = format!("'{}'", certificate.replace('\'', "'\"'\"'"));
    cluster
        .machine_shell(
            index,
            &format!(
                r#"printf %s {quoted_cert} > /tmp/expected.crt; curl -fsS --cacert /tmp/expected.crt --resolve secure.example.test:443:127.0.0.1 https://secure.example.test || true"#
            ),
        )
        .unwrap_or_default()
}
