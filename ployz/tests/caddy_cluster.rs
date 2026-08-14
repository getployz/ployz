use std::{process, time::Duration};

use ployz_core::{
    ContainerAction, ContainerKind, LogsOptions, Machine, MachineSelector, MembershipObservation,
    RequestedServiceSpec, ResolvedServiceSpec, ServiceId,
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
        assert!(config.contains("/.ployz-verify"));
    }

    let placed = ployz::caddy::service_spec(
        "caddy:2.10.2".into(),
        vec![MachineSelector::parse(machines[0].id.as_str()).unwrap()],
        None,
    )
    .unwrap();
    let caddy_id = deploy(&mut client, &placed).await;
    wait_running(&mut client, &caddy_id, 1).await;
    let all = ployz::caddy::service_spec("caddy:2.10.2".into(), Vec::new(), None).unwrap();
    assert_eq!(deploy(&mut client, &all).await, caddy_id);
    wait_running(&mut client, &caddy_id, 3).await;

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

    let cancellation = CancellationToken::new();
    let opened = client
        .open_service_logs(
            &[ployz::operator::ServiceArg {
                service: "caddy".into(),
                containers: Vec::new(),
            }],
            &[],
            LogsOptions {
                follow: false,
                tail: 1,
                since: String::new(),
                until: String::new(),
            },
            false,
            cancellation.clone(),
        )
        .await
        .unwrap();
    assert_eq!(opened.inputs.len(), 3);
    cancellation.cancel();

    let removed_address = observations
        .iter()
        .find(|container| container.machine_id == machines[1].id)
        .unwrap()
        .address
        .unwrap();
    client
        .change_container(
            machines[1].id.clone(),
            api_containers.get(1).expect("three API containers").clone(),
            ContainerAction::Stop,
            None,
            Some(0),
        )
        .await
        .unwrap();
    wait_config(&mut client, &machines[0], |config| {
        !config.contains(&format!("{}:8080", removed_address.0))
    })
    .await;
    client
        .change_container(
            machines[1].id.clone(),
            api_containers.get(1).expect("three API containers").clone(),
            ContainerAction::Start,
            None,
            None,
        )
        .await
        .unwrap();
    wait_config(&mut client, &machines[0], |config| {
        config.contains(&format!("{}:8080", removed_address.0))
    })
    .await;

    let stable = client
        .get_caddy_config(Some(&machines[0].id))
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
    let load_failure_container = create_and_start(&client, &machines[0], load_failure).await;
    wait_log_count(&cluster, "failed to update Caddy configuration", 1).await;
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
    create_and_start(&client, &machines[0], tick).await;
    wait_log_count(&cluster, "failed to update Caddy configuration", 2).await;
    assert_eq!(
        client
            .get_caddy_config(Some(&machines[0].id))
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
            machines[0].id.clone(),
            load_failure_container,
            ContainerAction::Stop,
            None,
            Some(0),
        )
        .await
        .unwrap();
    wait_config(&mut client, &machines[0], |config| config != stable).await;

    let retained_address = observations
        .iter()
        .find(|container| container.machine_id == machines[2].id)
        .unwrap()
        .address
        .unwrap();
    cluster.stop(2).unwrap();
    wait_down(&cluster, &machines[2]).await;
    let retained = client
        .get_caddy_config(Some(&machines[0].id))
        .await
        .unwrap();
    assert!(retained.contains(&format!("{}:8080", retained_address.0)));

    let broken_id = ServiceId::random();
    let broken: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": broken_id,
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
    create_and_start(&client, &machines[0], broken).await;
    let config = wait_config(&mut client, &machines[0], |config| {
        config.contains("Service 'broken': rendering failed")
    })
    .await;
    assert!(config.contains("http://example.test"));
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
        match client.get_caddy_config(Some(&machine.id)).await {
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
