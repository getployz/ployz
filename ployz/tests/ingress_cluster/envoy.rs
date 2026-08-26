//! Envoy founding tracer against the privileged Layer 3 cluster.

use std::{net::IpAddr, path::Path, process, time::Duration};

use ployz_core::{
    CERTIFICATE_POLICY_CLUSTER_KEY, ContainerAction, IngressProxyBackend, InspectRequest, Machine,
    MachineTarget, MachineUpdate, PublicIpUpdate, ResolvedServiceSpec, ServiceId,
    StopContainerRequest, op,
};
use ployz_testkit::{Cluster, ClusterPlan, fake_acme::FakeCa};

use super::{
    change_container, cli, create_and_start, curl_https, publish_certificate_row, request,
    self_signed_material, wait_config, wait_log_count, wait_running, wait_service,
};

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn envoy_founding_deploys_a_healthy_pinned_bridge_proxy() {
    let plan = ClusterPlan::new(&format!("l3-envoy-{}", process::id()), 1).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.wait_ready(Duration::from_secs(30)).await.unwrap();
    let machine = cluster
        .initialize_first_with_backend(IngressProxyBackend::Envoy)
        .await
        .unwrap();
    cluster.wait_ready(Duration::from_secs(60)).await.unwrap();
    wait_machine_present(&cluster, &machine).await;
    let direct = cluster.api_address(0).unwrap();
    let mut client =
        ployz::connect::connect(Path::new("/missing-ployz-test-config"), Some(&direct), None)
            .await
            .unwrap();

    let inspected = client
        .call::<op::Inspect>(InspectRequest::default(), None)
        .await
        .unwrap();
    assert_eq!(
        inspected.ingress_proxy_backend,
        Some(IngressProxyBackend::Envoy)
    );

    cli(&direct, &["ingress", "deploy"]);
    let ingress_id = wait_service(&mut client, "ingress", 1).await;
    let ingress = wait_running(&mut client, &ingress_id, 1).await.remove(0);
    assert_eq!(
        ingress.resolved_spec.container.image,
        ployz::ingress::ENVOY_IMAGE
    );
    assert_eq!(ingress.resolved_spec.ports.len(), 2);
    cluster
        .machine_shell(
            0,
            &format!(
                "test \"$(docker inspect --format '{{{{.HostConfig.NetworkMode}}}}' {})\" != host",
                ingress.container_id
            ),
        )
        .unwrap();
    cluster
        .machine_shell(
            0,
            &format!(
                "test \"$(docker inspect --format '{{{{(index (index .HostConfig.PortBindings \"8080/tcp\") 0).HostPort}}}}' {})\" = 80",
                ingress.container_id
            ),
        )
        .unwrap();
    cluster
        .machine_shell(
            0,
            &format!(
                "test \"$(docker inspect --format '{{{{(index (index .HostConfig.PortBindings \"8443/tcp\") 0).HostPort}}}}' {})\" = 443",
                ingress.container_id
            ),
        )
        .unwrap();
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn envoy_routes_http_through_file_watched_xds() {
    let plan = ClusterPlan::new(&format!("l3-envoy-http-{}", process::id()), 1).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.wait_ready(Duration::from_secs(30)).await.unwrap();
    let machine = cluster
        .initialize_first_with_backend(IngressProxyBackend::Envoy)
        .await
        .unwrap();
    cluster.wait_ready(Duration::from_secs(60)).await.unwrap();
    wait_machine_present(&cluster, &machine).await;
    let direct = cluster.api_address(0).unwrap();
    let mut client =
        ployz::connect::connect(Path::new("/missing-ployz-test-config"), Some(&direct), None)
            .await
            .unwrap();

    cli(&direct, &["ingress", "deploy"]);
    let ingress_id = wait_service(&mut client, "ingress", 1).await;
    let ingress = wait_running(&mut client, &ingress_id, 1).await.remove(0);
    let ingress_container = ingress.container_id;

    let api_id = ServiceId::random();
    let api_container = create_and_start(
        &mut client,
        &machine,
        http_service(api_id, "api", "envoy.test"),
    )
    .await;
    let observation = wait_running(&mut client, &api_id, 1).await.remove(0);
    let address = observation.address.unwrap();

    let serving = wait_config(&mut client, &machine, |config| {
        config.contains(&format!("address: {}", address.0))
            && config.contains("timeout: 60s")
            && config.contains("connect_timeout: 5s")
    })
    .await;
    assert_eq!(request(&cluster, "envoy.test"), (200, "ok\n".into()));
    assert_ne!(request(&cluster, "missing.test").0, 200);

    change_container(
        &mut client,
        machine.id,
        api_container,
        ContainerAction::Stop,
        Some(0),
    )
    .await;
    let empty = wait_config(&mut client, &machine, |config| {
        config.contains("address: 127.0.0.1")
            && config.contains("port_value: 1")
            && !config.contains(&format!("address: {}", address.0))
    })
    .await;
    assert_ne!(envoy_digest(&serving), envoy_digest(&empty));
    assert_eq!(request(&cluster, "envoy.test").0, 503);
    let still = wait_running(&mut client, &ingress_id, 1).await.remove(0);
    assert_eq!(still.container_id, ingress_container);

    change_container(
        &mut client,
        machine.id,
        api_container,
        ContainerAction::Start,
        None,
    )
    .await;
    let restored = wait_config(&mut client, &machine, |config| {
        config.contains(&format!("address: {}", address.0))
    })
    .await;
    assert_ne!(envoy_digest(&empty), envoy_digest(&restored));
    assert_eq!(request(&cluster, "envoy.test"), (200, "ok\n".into()));
    let after = wait_running(&mut client, &ingress_id, 1).await.remove(0);
    assert_eq!(after.container_id, ingress_container);

    for evidence in [
        "phase=\"validation\" outcome=\"accepted\"",
        "phase=\"activation\" outcome=\"activated\"",
    ] {
        wait_log_count(&cluster, evidence, 1).await;
    }
}

fn envoy_digest(config: &str) -> &str {
    config
        .split_once("projection_digest: ")
        .and_then(|(_, rest)| rest.split(['\n', ' ']).next())
        .expect("rendered Envoy config carries its projection digest")
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn envoy_serves_https_with_rotated_material_without_restart() {
    let plan = ClusterPlan::new(&format!("l3-envoy-https-{}", process::id()), 1).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.wait_ready(Duration::from_secs(30)).await.unwrap();
    let machine = cluster
        .initialize_first_with_backend(IngressProxyBackend::Envoy)
        .await
        .unwrap();
    cluster.wait_ready(Duration::from_secs(60)).await.unwrap();
    wait_machine_present(&cluster, &machine).await;
    let direct = cluster.api_address(0).unwrap();
    let mut client =
        ployz::connect::connect(Path::new("/missing-ployz-test-config"), Some(&direct), None)
            .await
            .unwrap();

    cli(&direct, &["ingress", "deploy"]);
    let ingress_id = wait_service(&mut client, "ingress", 1).await;
    let ingress = wait_running(&mut client, &ingress_id, 1).await.remove(0);
    let ingress_container = ingress.container_id;

    let api_id = ServiceId::random();
    let api: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": api_id,
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sh", "-c", "while true; do printf 'HTTP/1.1 200 OK\\r\\nContent-Length: 3\\r\\n\\r\\nok\\n' | nc -l -p 8080; done"],
            "pull_policy": "missing"
        },
        "ports": [
            {
                "mode": "ingress",
                "hostname": { "kind": "explicit", "hostname": "secure.example.test" },
                "load_balancer_port": 80,
                "container_port": 8080,
                "http_protocol": "http"
            },
            {
                "mode": "ingress",
                "hostname": { "kind": "explicit", "hostname": "secure.example.test" },
                "load_balancer_port": 443,
                "container_port": 8080,
                "http_protocol": "https"
            }
        ]
    }))
    .unwrap();
    create_and_start(&mut client, &machine, api).await;
    wait_running(&mut client, &api_id, 1).await;
    wait_config(&mut client, &machine, |config| {
        config.contains("ployz-http-secure.example.test")
            && !config.contains("/config/certs/secure.example.test-")
    })
    .await;
    assert_eq!(
        request(&cluster, "secure.example.test"),
        (200, "ok\n".into())
    );

    let (first_cert, first_key) = self_signed_material("secure.example.test", "first");
    publish_certificate_row(&cluster, 0, "secure.example.test", &first_cert, &first_key);
    wait_config(&mut client, &machine, |config| {
        config.contains("/config/certs/secure.example.test-")
    })
    .await;
    assert_eq!(curl_https(&cluster, 0, &first_cert).trim(), "ok");
    assert_eq!(
        request(&cluster, "secure.example.test"),
        (200, "ok\n".into())
    );

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
    assert_eq!(
        request(&cluster, "secure.example.test"),
        (200, "ok\n".into())
    );
    let after = wait_running(&mut client, &ingress_id, 1).await.remove(0);
    assert_eq!(after.container_id, ingress_container);
}

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn envoy_obtains_a_managed_certificate_over_http01() {
    let ca = FakeCa::bind("0.0.0.0:0").await.unwrap();
    ca.set_advertised_host("host.docker.internal");
    let plan = ClusterPlan::new(&format!("l3-envoy-acme-{}", process::id()), 1).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    cluster.wait_ready(Duration::from_secs(30)).await.unwrap();
    let first = cluster
        .initialize_first_with_backend(IngressProxyBackend::Envoy)
        .await
        .unwrap();
    wait_machine_present(&cluster, &first).await;
    publish_certificate_policy(&cluster, 0, &ca.directory_url());
    let ip = cluster.endpoint(0).unwrap().0.ip();
    publish_public_ip(&cluster, 0, &first, ip).await;
    ca.set_validation(validation_host(ip), 80);
    point_hostname(&cluster, 0, "app.example.com", ip);

    let direct = cluster.api_address(0).unwrap();
    let mut client =
        ployz::connect::connect(Path::new("/missing-ployz-test-config"), Some(&direct), None)
            .await
            .unwrap();
    cli(&direct, &["ingress", "deploy"]);
    wait_service(&mut client, "ingress", 1).await;

    let http_id = ServiceId::random();
    create_and_start(
        &mut client,
        &first,
        http_service(http_id, "plain", "plain.example.com"),
    )
    .await;
    wait_running(&mut client, &http_id, 1).await;
    wait_config(&mut client, &first, |config| {
        config.contains("ployz-http-plain.example.com")
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(ca.ordered(), Vec::<String>::new());
    assert_eq!(request(&cluster, "plain.example.com"), (200, "ok\n".into()));

    let custom_id = ServiceId::random();
    let custom = create_and_start(
        &mut client,
        &first,
        https_service(custom_id, "api", "app.example.com"),
    )
    .await;
    wait_running(&mut client, &custom_id, 1).await;
    wait_config(&mut client, &first, |config| {
        config.contains("/config/certs/app.example.com-")
    })
    .await;
    assert_eq!(ca.ordered(), vec!["app.example.com".to_owned()]);
    assert!(certificate_bodies(&cluster, 0).contains("BEGIN CERTIFICATE"));
    wait_issued_https(&cluster, 0, "app.example.com").await;
    assert_eq!(request(&cluster, "plain.example.com"), (200, "ok\n".into()));

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
    delete_certificate(&cluster, 0, "app.example.com");
    let tick_id = ServiceId::random();
    create_and_start(
        &mut client,
        &first,
        http_service(tick_id, "tick", "tick.example.com"),
    )
    .await;
    wait_running(&mut client, &tick_id, 1).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(ca.ordered(), vec!["app.example.com".to_owned()]);
    assert!(!certificate_bodies(&cluster, 0).contains("app.example.com"));
}

async fn wait_machine_present(cluster: &Cluster, machine: &Machine) {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if cluster
                .machines(0)
                .await
                .is_ok_and(|machines| machines.iter().any(|entry| entry.machine.id == machine.id))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
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

fn point_hostname(cluster: &Cluster, index: usize, hostname: &str, ip: IpAddr) {
    cluster
        .machine_shell(
            index,
            &format!(
                "grep -qxF '{ip} {hostname}' /etc/hosts || echo '{ip} {hostname}' >> /etc/hosts"
            ),
        )
        .unwrap();
}

fn validation_host(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => format!("[{address}]"),
    }
}

fn http_service(service_id: ServiceId, name: &str, hostname: &str) -> ResolvedServiceSpec {
    http_or_https_service(service_id, name, hostname, 80, "http")
}

fn https_service(service_id: ServiceId, name: &str, hostname: &str) -> ResolvedServiceSpec {
    http_or_https_service(service_id, name, hostname, 443, "https")
}

fn http_or_https_service(
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

async fn wait_issued_https(cluster: &Cluster, index: usize, hostname: &str) {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if curl_issued_https(cluster, index, hostname).trim() == "ok" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap();
}

fn curl_issued_https(cluster: &Cluster, index: usize, hostname: &str) -> String {
    cluster
        .machine_shell(
            index,
            &format!(
                r#"cert=$(ls /var/lib/ployz/ingress/envoy/certs/{hostname}-*.crt | head -n1); curl -fsS --cacert "$cert" --resolve {hostname}:443:127.0.0.1 https://{hostname} || true"#
            ),
        )
        .unwrap_or_default()
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
