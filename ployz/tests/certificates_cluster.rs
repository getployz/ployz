use std::{
    net::IpAddr,
    process::{self, Command},
    time::Duration,
};

use ployz_core::{
    CERTIFICATE_POLICY_CLUSTER_KEY, ContainerKind, GetIngressProxyConfigRequest, Machine,
    MachineTarget, MachineUpdate, ProjectName, PublicIpUpdate, ResolvedServiceSpec, ServiceId,
    StartContainerRequest, StopContainerRequest, op,
};
use ployz_testkit::{Cluster, ClusterPlan, fake_acme::FakeCa};

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn custom_https_hostname_obtains_a_certificate_from_a_fake_ca() {
    let ca = FakeCa::bind("0.0.0.0:0").await.unwrap();
    ca.set_advertised_host("host.docker.internal");
    let cluster = Cluster::create(plan("l3-acme", 1)).unwrap();
    cluster.wait_ready(Duration::from_secs(30)).await.unwrap();
    let first = cluster.initialize_first().await.unwrap();
    wait_machine_count(&cluster, 0, 1).await;
    publish_certificate_policy(&cluster, 0, &ca.directory_url());
    let ip = cluster.endpoint(0).unwrap().0.ip();
    publish_public_ip(&cluster, 0, &first, ip).await;
    ca.set_validation(validation_host(ip), 80);
    point_hostname(&cluster, [0], "app.example.com", &[ip]);

    let direct = cluster.api_address(0).unwrap();
    let mut client = connect(&direct).await;
    cli(&direct, &["ingress", "deploy", "--image", "caddy:2.10.2"]);
    wait_service(&mut client, "ingress", 1).await;

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

    point_dns(&cluster, "app.example.com", ip);
    let custom_id = ServiceId::random();
    let custom = create_and_start(
        &mut client,
        &first,
        service(custom_id, "api", "app.example.com", 443, "https"),
    )
    .await;
    wait_running(&mut client, &custom_id, 1).await;
    wait_config(&mut client, &first, |config| {
        config.contains("tls /config/caddy/certs/app.example.com-")
    })
    .await;
    assert_eq!(ca.ordered(), vec!["app.example.com".to_owned()]);
    assert!(certificate_bodies(&cluster, 0).contains("BEGIN CERTIFICATE"));
    wait_https(&cluster, 0, "app.example.com").await;

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
    delete_certificate(&cluster, 0, "app.example.com");
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
    assert!(!certificate_bodies(&cluster, 0).contains("app.example.com"));
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn several_machines_order_once_and_every_machine_answers() {
    let ca = FakeCa::bind("0.0.0.0:0").await.unwrap();
    ca.set_advertised_host("host.docker.internal");
    let cluster = Cluster::create(plan("l3-acme-multi", 2)).unwrap();
    let [first, second] = cluster.initialize_two().await.unwrap();
    publish_certificate_policy(&cluster, 0, &ca.directory_url());
    let first_ip = cluster.endpoint(0).unwrap().0.ip();
    let second_ip = cluster.endpoint(1).unwrap().0.ip();
    publish_public_ip(&cluster, 0, &first, first_ip).await;
    publish_public_ip(&cluster, 0, &second, second_ip).await;
    let other = if first.id < second.id { 1 } else { 0 };
    ca.set_validation(validation_host(cluster.endpoint(other).unwrap().0.ip()), 80);
    point_hostname(&cluster, [0, 1], "app.example.com", &[first_ip, second_ip]);
    point_hostname(&cluster, [0, 1], "web.example.com", &[first_ip, second_ip]);

    let direct = cluster.api_address(0).unwrap();
    let mut client = connect(&direct).await;
    cli(&direct, &["ingress", "deploy", "--image", "caddy:2.10.2"]);
    wait_service(&mut client, "ingress", 2).await;

    let app_id = ServiceId::random();
    let web_id = ServiceId::random();
    let broken_id = ServiceId::random();
    create_and_start(
        &mut client,
        &first,
        service(app_id, "api", "app.example.com", 443, "https"),
    )
    .await;
    create_and_start(
        &mut client,
        &first,
        service(web_id, "www", "web.example.com", 443, "https"),
    )
    .await;
    create_and_start(
        &mut client,
        &first,
        service(broken_id, "bad", "broken.example.com", 443, "https"),
    )
    .await;
    wait_running(&mut client, &app_id, 1).await;
    wait_running(&mut client, &web_id, 1).await;
    wait_running(&mut client, &broken_id, 1).await;

    wait_config(&mut client, &first, |config| {
        config.contains("tls /config/caddy/certs/app.example.com-")
            && config.contains("tls /config/caddy/certs/web.example.com-")
    })
    .await;
    wait_config(&mut client, &second, |config| {
        config.contains("tls /config/caddy/certs/app.example.com-")
            && config.contains("tls /config/caddy/certs/web.example.com-")
    })
    .await;

    let ordered = ca.ordered();
    assert_eq!(count_orders(&ordered, "app.example.com"), 1, "{ordered:?}");
    assert_eq!(count_orders(&ordered, "web.example.com"), 1, "{ordered:?}");
    assert!(
        count_orders(&ordered, "broken.example.com") <= 1,
        "{ordered:?}"
    );
    let bodies = certificate_bodies(&cluster, 0);
    assert!(bodies.contains("app.example.com"));
    assert!(bodies.contains("web.example.com"));
    wait_https(&cluster, 0, "app.example.com").await;
    wait_https(&cluster, 1, "app.example.com").await;
    wait_https(&cluster, 0, "web.example.com").await;
    wait_https(&cluster, 1, "web.example.com").await;
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn down_machine_does_not_block_ordering() {
    let ca = FakeCa::bind("0.0.0.0:0").await.unwrap();
    ca.set_advertised_host("host.docker.internal");
    let cluster = Cluster::create(plan("l3-acme-down", 2)).unwrap();
    let [first, second] = cluster.initialize_two().await.unwrap();
    publish_certificate_policy(&cluster, 0, &ca.directory_url());
    let (living_index, living, down_index) = if first.id < second.id {
        (1, &second, 0)
    } else {
        (0, &first, 1)
    };
    cluster.stop(down_index).unwrap();
    let living_ip = cluster.endpoint(living_index).unwrap().0.ip();
    publish_public_ip(&cluster, living_index, living, living_ip).await;
    ca.set_validation(validation_host(living_ip), 80);
    point_hostname(&cluster, [living_index], "app.example.com", &[living_ip]);

    let direct = cluster.api_address(living_index).unwrap();
    let mut client = connect(&direct).await;
    cli(
        &direct,
        &[
            "ingress",
            "deploy",
            "--image",
            "caddy:2.10.2",
            "--machine",
            living.id.as_str(),
        ],
    );
    wait_service(&mut client, "ingress", 1).await;

    let app_id = ServiceId::random();
    create_and_start(
        &mut client,
        living,
        service(app_id, "api", "app.example.com", 443, "https"),
    )
    .await;
    wait_running(&mut client, &app_id, 1).await;
    wait_config(&mut client, living, |config| {
        config.contains("tls /config/caddy/certs/app.example.com-")
    })
    .await;
    assert_eq!(ca.ordered(), vec!["app.example.com".to_owned()]);
    wait_https(&cluster, living_index, "app.example.com").await;
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn certificate_renews_before_expiry_without_restart() {
    let lifetime = Duration::from_secs(90);
    let ca = FakeCa::bind("0.0.0.0:0").await.unwrap();
    ca.set_advertised_host("host.docker.internal");
    ca.set_certificate_lifetime(lifetime);
    let cluster = Cluster::create(plan("l3-acme-renew", 1)).unwrap();
    cluster.wait_ready(Duration::from_secs(30)).await.unwrap();
    let first = cluster.initialize_first().await.unwrap();
    wait_machine_count(&cluster, 0, 1).await;
    publish_certificate_policy(&cluster, 0, &ca.directory_url());
    let ip = cluster.endpoint(0).unwrap().0.ip();
    publish_public_ip(&cluster, 0, &first, ip).await;
    ca.set_validation(validation_host(ip), 80);
    point_hostname(&cluster, [0], "app.example.com", &[ip]);

    let direct = cluster.api_address(0).unwrap();
    let mut client = connect(&direct).await;
    cli(&direct, &["ingress", "deploy", "--image", "caddy:2.10.2"]);
    wait_service(&mut client, "ingress", 1).await;
    let app_id = ServiceId::random();
    create_and_start(
        &mut client,
        &first,
        service(app_id, "api", "app.example.com", 443, "https"),
    )
    .await;
    wait_running(&mut client, &app_id, 1).await;
    wait_config(&mut client, &first, |config| {
        config.contains("tls /config/caddy/certs/app.example.com-")
    })
    .await;
    assert_eq!(ca.ordered(), vec!["app.example.com".to_owned()]);
    wait_https(&cluster, 0, "app.example.com").await;
    let issued = certificate_bodies(&cluster, 0);

    wait_until(Duration::from_secs(180), || {
        count_orders(&ca.ordered(), "app.example.com") >= 2
    })
    .await;
    wait_until(Duration::from_secs(60), || {
        certificate_bodies(&cluster, 0) != issued
    })
    .await;
    wait_https(&cluster, 0, "app.example.com").await;
    assert_eq!(count_orders(&ca.ordered(), "app.example.com"), 2);
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn machines_holding_the_same_certificate_renew_once() {
    let ca = FakeCa::bind("0.0.0.0:0").await.unwrap();
    ca.set_advertised_host("host.docker.internal");
    ca.set_certificate_lifetime(Duration::from_secs(180));
    let cluster = Cluster::create(plan("l3-acme-renew-multi", 2)).unwrap();
    let [first, second] = cluster.initialize_two().await.unwrap();
    publish_certificate_policy(&cluster, 0, &ca.directory_url());
    let first_ip = cluster.endpoint(0).unwrap().0.ip();
    let second_ip = cluster.endpoint(1).unwrap().0.ip();
    publish_public_ip(&cluster, 0, &first, first_ip).await;
    publish_public_ip(&cluster, 0, &second, second_ip).await;
    let other = if first.id < second.id { 1 } else { 0 };
    ca.set_validation(validation_host(cluster.endpoint(other).unwrap().0.ip()), 80);
    point_hostname(&cluster, [0, 1], "app.example.com", &[first_ip, second_ip]);

    let direct = cluster.api_address(0).unwrap();
    let mut client = connect(&direct).await;
    cli(&direct, &["ingress", "deploy", "--image", "caddy:2.10.2"]);
    wait_service(&mut client, "ingress", 2).await;
    let app_id = ServiceId::random();
    create_and_start(
        &mut client,
        &first,
        service(app_id, "api", "app.example.com", 443, "https"),
    )
    .await;
    wait_running(&mut client, &app_id, 1).await;
    wait_config(&mut client, &first, |config| {
        config.contains("tls /config/caddy/certs/app.example.com-")
    })
    .await;
    wait_config(&mut client, &second, |config| {
        config.contains("tls /config/caddy/certs/app.example.com-")
    })
    .await;
    assert_eq!(count_orders(&ca.ordered(), "app.example.com"), 1);
    let issued = certificate_bodies(&cluster, 0);

    wait_until(Duration::from_secs(180), || {
        count_orders(&ca.ordered(), "app.example.com") >= 2
    })
    .await;
    wait_until(Duration::from_secs(60), || {
        certificate_bodies(&cluster, 0) != issued
    })
    .await;
    wait_https(&cluster, 0, "app.example.com").await;
    wait_https(&cluster, 1, "app.example.com").await;
    tokio::time::sleep(Duration::from_secs(35)).await;
    assert_eq!(count_orders(&ca.ordered(), "app.example.com"), 2);
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn failed_renewal_keeps_serving_the_existing_certificate() {
    let ca = FakeCa::bind("0.0.0.0:0").await.unwrap();
    ca.set_advertised_host("host.docker.internal");
    ca.set_certificate_lifetime(Duration::from_secs(90));
    let cluster = Cluster::create(plan("l3-acme-renew-fail", 1)).unwrap();
    cluster.wait_ready(Duration::from_secs(30)).await.unwrap();
    let first = cluster.initialize_first().await.unwrap();
    wait_machine_count(&cluster, 0, 1).await;
    publish_certificate_policy(&cluster, 0, &ca.directory_url());
    let ip = cluster.endpoint(0).unwrap().0.ip();
    publish_public_ip(&cluster, 0, &first, ip).await;
    ca.set_validation(validation_host(ip), 80);
    point_hostname(&cluster, [0], "app.example.com", &[ip]);

    let direct = cluster.api_address(0).unwrap();
    let mut client = connect(&direct).await;
    cli(&direct, &["ingress", "deploy", "--image", "caddy:2.10.2"]);
    wait_service(&mut client, "ingress", 1).await;
    let app_id = ServiceId::random();
    create_and_start(
        &mut client,
        &first,
        service(app_id, "api", "app.example.com", 443, "https"),
    )
    .await;
    wait_running(&mut client, &app_id, 1).await;
    wait_config(&mut client, &first, |config| {
        config.contains("tls /config/caddy/certs/app.example.com-")
    })
    .await;
    wait_https(&cluster, 0, "app.example.com").await;
    let issued = certificate_bodies(&cluster, 0);
    ca.set_validation("127.0.0.1", 1);

    tokio::time::sleep(Duration::from_secs(95)).await;
    assert_eq!(certificate_bodies(&cluster, 0), issued);
    wait_https(&cluster, 0, "app.example.com").await;

    ca.set_validation(validation_host(ip), 80);
    wait_until(Duration::from_secs(180), || {
        certificate_bodies(&cluster, 0) != issued
    })
    .await;
    wait_https(&cluster, 0, "app.example.com").await;
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn joining_machine_serves_existing_certificate() {
    let ca = FakeCa::bind("0.0.0.0:0").await.unwrap();
    ca.set_advertised_host("host.docker.internal");
    let cluster = Cluster::create(plan("l3-acme-join", 2)).unwrap();
    cluster.wait_ready(Duration::from_secs(30)).await.unwrap();
    let first = cluster.initialize_first().await.unwrap();
    wait_machine_count(&cluster, 0, 1).await;
    publish_certificate_policy(&cluster, 0, &ca.directory_url());
    let ip = cluster.endpoint(0).unwrap().0.ip();
    publish_public_ip(&cluster, 0, &first, ip).await;
    ca.set_validation(validation_host(ip), 80);
    point_hostname(&cluster, [0], "app.example.com", &[ip]);

    let direct = cluster.api_address(0).unwrap();
    let mut client = connect(&direct).await;
    cli(&direct, &["ingress", "deploy", "--image", "caddy:2.10.2"]);
    wait_service(&mut client, "ingress", 1).await;
    let app_id = ServiceId::random();
    create_and_start(
        &mut client,
        &first,
        service(app_id, "api", "app.example.com", 443, "https"),
    )
    .await;
    wait_running(&mut client, &app_id, 1).await;
    wait_config(&mut client, &first, |config| {
        config.contains("tls /config/caddy/certs/app.example.com-")
    })
    .await;
    assert_eq!(ca.ordered(), vec!["app.example.com".to_owned()]);

    let second = cluster.add_machine(0, 1, "machine-2").await.unwrap();
    cli(
        &direct,
        &[
            "ingress",
            "deploy",
            "--image",
            "caddy:2.10.2",
            "--machine",
            second.id.as_str(),
        ],
    );
    wait_config(&mut client, &second, |config| {
        config.contains("tls /config/caddy/certs/app.example.com-")
    })
    .await;
    wait_https(&cluster, 1, "app.example.com").await;
    assert_eq!(ca.ordered(), vec!["app.example.com".to_owned()]);
}

fn plan(name: &str, machines: usize) -> ClusterPlan {
    ClusterPlan::new(&format!("{name}-{}", process::id()), machines).unwrap()
}

fn publish_certificate_policy(cluster: &Cluster, index: usize, directory_url: &str) {
    let body = serde_json::json!({ "directory_url": directory_url }).to_string();
    let payload = serde_json::to_string(&serde_json::json!([{
        "query": "INSERT INTO cluster (key, value, updated_at) VALUES (?, ?, datetime('now')) ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        "params": [CERTIFICATE_POLICY_CLUSTER_KEY, body],
    }]))
    .unwrap();
    corrosion_exec(cluster, index, "v1/transactions", &payload);
}

fn point_hostname(
    cluster: &Cluster,
    indices: impl IntoIterator<Item = usize>,
    hostname: &str,
    ips: &[IpAddr],
) {
    for index in indices {
        for ip in ips {
            cluster
                .machine_shell(
                    index,
                    &format!(
                        "grep -qxF '{ip} {hostname}' /etc/hosts || echo '{ip} {hostname}' >> /etc/hosts"
                    ),
                )
                .unwrap();
        }
    }
}

fn count_orders(ordered: &[String], hostname: &str) -> usize {
    ordered
        .iter()
        .filter(|name| name.as_str() == hostname)
        .count()
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn hostname_resolving_elsewhere_is_refused_then_issues_when_dns_points_here() {
    let ca = FakeCa::bind("0.0.0.0:0").await.unwrap();
    ca.set_advertised_host("host.docker.internal");
    let cluster = Cluster::create(plan("l3-acme-refuse", 1)).unwrap();
    cluster.wait_ready(Duration::from_secs(30)).await.unwrap();
    let first = cluster.initialize_first().await.unwrap();
    wait_machine_count(&cluster, 0, 1).await;
    publish_certificate_policy(&cluster, 0, &ca.directory_url());
    let ip = cluster.endpoint(0).unwrap().0.ip();
    publish_public_ip(&cluster, 0, &first, ip).await;
    ca.set_validation(validation_host(ip), 80);

    let direct = cluster.api_address(0).unwrap();
    let mut client = connect(&direct).await;
    cli(&direct, &["ingress", "deploy", "--image", "caddy:2.10.2"]);
    wait_service(&mut client, "ingress", 1).await;

    point_dns(
        &cluster,
        "outside.example.com",
        "198.51.100.10".parse().unwrap(),
    );
    let outside_id = ServiceId::random();
    create_and_start(
        &mut client,
        &first,
        service(outside_id, "api", "outside.example.com", 443, "https"),
    )
    .await;
    wait_running(&mut client, &outside_id, 1).await;
    wait_config(&mut client, &first, |config| {
        config.contains("# Skipped certificate issuance:")
            && config.contains("outside.example.com")
            && config.contains("198.51.100.10")
            && config.contains(&ip.to_string())
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(ca.ordered(), Vec::<String>::new());
    assert!(
        certificate_bodies(&cluster, 0).contains("198.51.100.10"),
        "{}",
        certificate_bodies(&cluster, 0)
    );

    point_dns(&cluster, "outside.example.com", ip);
    wait_config(&mut client, &first, |config| {
        config.contains("tls /config/caddy/certs/outside.example.com-")
    })
    .await;
    assert_eq!(ca.ordered(), vec!["outside.example.com".to_owned()]);
    assert!(certificate_bodies(&cluster, 0).contains("BEGIN CERTIFICATE"));
}

async fn publish_public_ip(cluster: &Cluster, entry: usize, machine: &Machine, ip: IpAddr) {
    cluster
        .update_machine(
            entry,
            machine.id.as_str(),
            MachineUpdate {
                public_ip: PublicIpUpdate::Set(ip),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

fn point_dns(cluster: &Cluster, hostname: &str, ip: IpAddr) {
    let script = format!(
        "if grep -q ' {hostname}$' /etc/hosts; then sed -i 's/^.* {hostname}$/{ip} {hostname}/' /etc/hosts; else echo '{ip} {hostname}' >> /etc/hosts; fi"
    );
    cluster.machine_shell(0, &script).unwrap();
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

async fn connect(direct: &str) -> ployz::connect::Client {
    ployz::connect::connect(
        std::path::Path::new("/missing-ployz-test-config"),
        Some(direct),
        None,
    )
    .await
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
        .create_container(
            machine.id,
            ContainerKind::ServiceContainer,
            ProjectName::parse("app").unwrap(),
            spec,
        )
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

async fn wait_machine_count(cluster: &Cluster, index: usize, count: usize) {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if cluster
                .machines(index)
                .await
                .is_ok_and(|machines| machines.len() == count)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_service(client: &mut ployz::connect::Client, name: &str, count: usize) -> ServiceId {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let live = client.live_services().await.unwrap();
            if let Some(service) = live.services().into_iter().find(|service| {
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
            .services()
            .into_iter()
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        match client
            .call::<op::GetIngressProxyConfig>(
                GetIngressProxyConfigRequest {},
                Some(&MachineTarget::from(&machine.id)),
            )
            .await
        {
            Ok(config) if expected(config.config()) => return config.config().to_owned(),
            Ok(config) if tokio::time::Instant::now() >= deadline => {
                panic!("Caddyfile did not converge:\n{}", config.config())
            }
            Err(error) if tokio::time::Instant::now() >= deadline => {
                panic!("Caddyfile was unavailable: {error}")
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_until(timeout: Duration, ready: impl Fn() -> bool) {
    tokio::time::timeout(timeout, async {
        loop {
            if ready() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_https(cluster: &Cluster, index: usize, hostname: &str) {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if curl_https(cluster, index, hostname).trim() == "ok" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
}

fn delete_certificate(cluster: &Cluster, index: usize, hostname: &str) {
    let payload = serde_json::to_string(&serde_json::json!([{
        "query": "DELETE FROM certificates WHERE hostname = ?",
        "params": [hostname],
    }]))
    .unwrap();
    corrosion_exec(cluster, index, "v1/transactions", &payload);
}

fn certificate_bodies(cluster: &Cluster, index: usize) -> String {
    corrosion_exec(
        cluster,
        index,
        "v1/queries",
        r#"{"query":"SELECT hostname, body FROM certificates","params":[]}"#,
    )
}

fn corrosion_exec(cluster: &Cluster, index: usize, path: &str, payload: &str) -> String {
    let quoted = format!("'{}'", payload.replace('\'', "'\"'\"'"));
    cluster
        .machine_shell(
            index,
            &format!(
                r#"token=$(cat /var/lib/ployz/corrosion/.api-token); curl --fail --silent --show-error --http2-prior-knowledge -H "Authorization: Bearer $token" -H 'Content-Type: application/json' --data-binary {quoted} http://127.0.0.1:51002/{path}"#
            ),
        )
        .unwrap()
}

fn curl_https(cluster: &Cluster, index: usize, hostname: &str) -> String {
    cluster
        .machine_shell(
            index,
            &format!(
                r#"cert=$(ls /var/lib/ployz/ingress/caddy/certs/{hostname}-*.crt | head -n1); curl -fsS --cacert "$cert" --resolve {hostname}:443:127.0.0.1 https://{hostname} || true"#
            ),
        )
        .unwrap_or_default()
}
