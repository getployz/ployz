use std::{
    net::IpAddr,
    process::{self, Command},
    time::Duration,
};

use ployz_core::{
    ContainerKind, GetCaddyConfigRequest, Machine, MachineTarget, ResolvedServiceSpec, ServiceId,
    StartContainerRequest, StopContainerRequest, op,
};
use ployz_testkit::{Cluster, ClusterPlan, fake_acme::FakeCa};

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn custom_https_hostname_obtains_a_certificate_from_a_fake_ca() {
    let ca = FakeCa::bind("0.0.0.0:0").await.unwrap();
    ca.set_advertised_host("host.docker.internal");
    let mut plan = ClusterPlan::new(&format!("l3-acme-{}", process::id()), 1).unwrap();
    plan.machines
        .first_mut()
        .unwrap()
        .environment
        .insert("PLOYZ_ACME_DIRECTORY".to_owned(), ca.directory_url());
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
    let ip = cluster.endpoint(0).unwrap().0.ip();
    ca.set_validation(validation_host(ip), 80);

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

    let http_id = ServiceId::random();
    create_and_start(
        &mut client,
        &first,
        service(http_id, "plain", "plain.example.com", 80, "http"),
    )
    .await;
    wait_running(&mut client, &http_id, 1).await;
    wait_config(&mut client, &first, |config| {
        config.contains("auto_https off") && config.contains("http://plain.example.com")
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(ca.ordered(), Vec::<String>::new());

    let custom_id = ServiceId::random();
    let custom = create_and_start(
        &mut client,
        &first,
        service(custom_id, "api", "app.example.com", 443, "https"),
    )
    .await;
    wait_running(&mut client, &custom_id, 1).await;
    wait_config(&mut client, &first, |config| {
        config.contains("tls /config/certs/app.example.com-")
    })
    .await;
    assert_eq!(ca.ordered(), vec!["app.example.com".to_owned()]);
    assert!(certificate_bodies(&cluster).contains("BEGIN CERTIFICATE"));
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if curl_https(&cluster, "app.example.com").trim() == "ok" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();

    client
        .call::<op::StopContainer>(
            StopContainerRequest {
                container_id: custom,
                signal: None,
                grace_period_seconds: Some(0),
            },
            Some(&MachineTarget::from(&first.id)),
        )
        .await
        .unwrap();
    wait_config(&mut client, &first, |config| {
        !config.contains("https://app.example.com")
    })
    .await;
    delete_certificate(&cluster, "app.example.com");
    let tick_id = ServiceId::random();
    create_and_start(
        &mut client,
        &first,
        service(tick_id, "tick", "tick.example.com", 80, "http"),
    )
    .await;
    wait_running(&mut client, &tick_id, 1).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(ca.ordered(), vec!["app.example.com".to_owned()]);
    assert!(!certificate_bodies(&cluster).contains("app.example.com"));
}

fn validation_host(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => format!("[{address}]"),
    }
}

fn service(
    service_id: ServiceId,
    name: &str,
    hostname: &str,
    load_balancer_port: u16,
    http_protocol: &str,
) -> ResolvedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "service_id": service_id,
        "name": name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sh", "-c", "while true; do printf 'HTTP/1.1 200 OK\\r\\nContent-Length: 3\\r\\n\\r\\nok\\n' | nc -l -p 8080; done"],
            "pull_policy": "missing"
        },
        "ports": [{
            "mode": "ingress",
            "hostname": { "kind": "explicit", "hostname": hostname },
            "load_balancer_port": load_balancer_port,
            "container_port": 8080,
            "http_protocol": http_protocol
        }]
    }))
    .unwrap()
}

fn cli(direct: &str, args: &[&str]) {
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
    client
        .call::<op::StartContainer>(
            StartContainerRequest {
                container_id: created.container_id,
            },
            Some(&MachineTarget::from(&machine.id)),
        )
        .await
        .unwrap();
    created.container_id
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

async fn wait_running(client: &mut ployz::connect::Client, service_id: &ServiceId, count: usize) {
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
                        container.as_observation().runtime,
                        ployz_core::ContainerRuntimeObservation::Running {
                            health: ployz_core::HealthObservation::Healthy
                                | ployz_core::HealthObservation::NotConfigured
                        }
                    )
                })
                .count();
            if running == count {
                return;
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

fn delete_certificate(cluster: &Cluster, hostname: &str) {
    let payload = serde_json::to_string(&serde_json::json!([{
        "query": "DELETE FROM certificates WHERE hostname = ?",
        "params": [hostname],
    }]))
    .unwrap();
    corrosion_exec(cluster, "v1/transactions", &payload);
}

fn certificate_bodies(cluster: &Cluster) -> String {
    corrosion_exec(
        cluster,
        "v1/queries",
        r#"{"query":"SELECT hostname, body FROM certificates","params":[]}"#,
    )
}

fn corrosion_exec(cluster: &Cluster, path: &str, payload: &str) -> String {
    let quoted = format!("'{}'", payload.replace('\'', "'\"'\"'"));
    cluster
        .machine_shell(
            0,
            &format!(
                r#"token=$(cat /var/lib/ployz/corrosion/.api-token); curl --fail --silent --show-error --http2-prior-knowledge -H "Authorization: Bearer $token" -H 'Content-Type: application/json' --data-binary {quoted} http://127.0.0.1:51002/{path}"#
            ),
        )
        .unwrap()
}

fn curl_https(cluster: &Cluster, hostname: &str) -> String {
    cluster
        .machine_shell(
            0,
            &format!(
                r#"cert=$(ls /var/lib/ployz/caddy/certs/{hostname}-*.crt | head -n1); curl -fsS --cacert "$cert" --resolve {hostname}:443:127.0.0.1 https://{hostname} || true"#
            ),
        )
        .unwrap_or_default()
}
