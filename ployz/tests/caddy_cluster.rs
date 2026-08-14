use std::{
    process::{self, Command},
    time::Duration,
};

use ployz_core::{
    ContainerAction, ContainerKind, Machine, MachineSelector, MembershipObservation,
    RequestedServiceSpec, ResolvedServiceSpec, ServiceId,
};
use ployz_testkit::{Cluster, ClusterPlan};
use semver::Version;
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
        assert!(config.contains("/.ployz-verify"));
    }

    let selected_image = ployz::caddy::latest_image().await.unwrap();
    let selected_tag = selected_image
        .strip_prefix("caddy:")
        .expect("Caddy image has the repository prefix");
    let selected_version = Version::parse(selected_tag).expect("Caddy image has a semver tag");
    assert_eq!(selected_version.major, 2);
    assert!(selected_version.pre.is_empty() && selected_version.build.is_empty());
    assert_eq!(selected_version.to_string(), selected_tag);
    for index in 0..machines.len() {
        cluster
            .machine_shell(index, &format!("docker tag caddy:2.10.2 {selected_image}"))
            .unwrap();
    }
    cli(
        &direct,
        &["caddy", "deploy", "--machine", machines[0].id.as_str()],
    );
    let caddy_id = wait_service(&mut client, "caddy", 1).await;
    assert!(
        wait_running(&mut client, &caddy_id, 1)
            .await
            .iter()
            .all(|container| container.resolved_spec.container.image == selected_image)
    );
    cli(&direct, &["caddy", "deploy"]);
    assert_eq!(wait_service(&mut client, "caddy", 3).await, caddy_id);
    assert!(
        wait_running(&mut client, &caddy_id, 3)
            .await
            .iter()
            .all(|container| container.resolved_spec.container.image == selected_image)
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
            "hostname": "example.test",
            "load_balancer_port": 80,
            "container_port": 8080,
            "http_protocol": "http"
        }],
        "caddy_config": "custom.example {\n\trespond \"custom\" 200\n}"
    }))
    .unwrap();
    let mut api_containers = Vec::new();
    for machine in &machines {
        api_containers.push(create_and_start(&client, machine, api.clone()).await);
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
                .machine_shell(index, "curl -fsS http://127.0.0.1/.ployz-verify")
                .unwrap()
                .trim(),
            machine.id.as_str()
        );
        assert_eq!(
            cluster
                .machine_shell(
                    index,
                    "curl -fsS -H 'Host: example.test' http://127.0.0.1/.ployz-verify",
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
        client
            .change_container(
                machines[1].id.clone(),
                containers.get(1).expect("three API containers").clone(),
                action,
                None,
                (action == ContainerAction::Stop).then_some(0),
            )
            .await
            .unwrap();
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
        .get_caddy_config(Some(MachineSelector::from(&machine.id)))
        .await
        .unwrap();
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
            .get_caddy_config(Some(MachineSelector::from(&machine.id)))
            .await
            .unwrap(),
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
        .change_container(
            machine.id.clone(),
            rejected,
            ContainerAction::Stop,
            None,
            Some(0),
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
        .get_caddy_config(Some(MachineSelector::from(&machines[0].id)))
        .await
        .unwrap();
    assert!(retained.contains(&format!("{}:8080", retained_address.0)));
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
                service
                    .containers
                    .first()
                    .is_some_and(|container| container.service_name.as_str() == name)
                    && service.containers.len() == count
            }) {
                return service.service_id.clone();
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
                "hostname": "switch.test",
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
    let machines = client.list_machines().await.unwrap();
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
        requested,
        &snapshot,
        ServiceId::random(),
        ployz::deploy::PlanOptions::default(),
    )
    .unwrap();
    let outcome = ployz::deploy::execute_plan(&plan, client, &CancellationToken::new()).await;
    assert!(outcome.failed.is_none(), "{outcome:?}");
    plan.service_id
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
    client: &ployz::connect::Client,
    machine: &Machine,
    spec: ResolvedServiceSpec,
) -> ployz_core::ContainerId {
    let created = client
        .create_container(machine.id.clone(), ContainerKind::ServiceContainer, spec)
        .await
        .unwrap();
    client
        .change_container(
            machine.id.clone(),
            created.container_id.clone(),
            ContainerAction::Start,
            None,
            None,
        )
        .await
        .unwrap();
    created.container_id
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
                .filter(|container| {
                    matches!(
                        container.runtime,
                        ployz_core::ContainerRuntimeObservation::Running {
                            health: ployz_core::HealthObservation::Healthy
                                | ployz_core::HealthObservation::NotConfigured
                        }
                    )
                })
                .cloned()
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
            .get_caddy_config(Some(MachineSelector::from(&machine.id)))
            .await
        {
            Ok(config) if expected(&config) => return config,
            Ok(config) if tokio::time::Instant::now() >= deadline => {
                panic!("Caddyfile did not converge:\n{config}")
            }
            Err(error) if tokio::time::Instant::now() >= deadline => {
                panic!("Caddyfile was unavailable: {error}")
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
