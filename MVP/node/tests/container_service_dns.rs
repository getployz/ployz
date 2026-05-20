#![cfg(feature = "docker-runtime")]

use std::net::{IpAddr, SocketAddr};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use mvp_bus::IslandId;
use mvp_deploy::{InstanceId, RevisionId};
use mvp_identity::NodeId;
use mvp_mesh::ContainerSubnet;
use mvp_projection::{
    BackendEndpoint, DnsSnapshotFile, GatewayRouteProjection, GatewaySnapshotFile, RouteId,
    ServiceName, write_serving_generation_manifest,
};
use mvp_runtime::{
    ContainerNetworkBackend, DockerBridgeNetwork, DockerBridgeNetworkConfig, DockerRuntime,
    DockerRuntimeConfig, RuntimeBackend, RuntimeInstanceSpec,
};
use mvp_serving::{ServingActorHandle, ServingSnapshotPaths, WireServingState, spawn_dns_server};

#[tokio::test]
async fn docker_container_resolves_service_dns_on_bridge_when_enabled() {
    if std::env::var("MVP_DOCKER_SERVICE_DNS_SMOKE")
        .ok()
        .as_deref()
        != Some("1")
    {
        eprintln!("skipping Docker service-DNS smoke; set MVP_DOCKER_SERVICE_DNS_SMOKE=1 to run");
        return;
    }
    let uid = Command::new("id").arg("-u").output().expect("id -u").stdout;
    assert_eq!(
        String::from_utf8_lossy(&uid).trim(),
        "0",
        "Docker service-DNS smoke binds port 53 on the bridge gateway and requires root"
    );

    let temp = tempfile::tempdir().expect("tempdir");
    let subnet = ContainerSubnet::new("10.210.92.0/24".parse().expect("valid subnet"))
        .expect("container subnet");
    let bridge = Arc::new(
        DockerBridgeNetwork::connect(DockerBridgeNetworkConfig::new(
            format!("ployz-mvp-service-dns-test-{}", std::process::id()),
            subnet,
        ))
        .expect("docker bridge"),
    );
    let bridge_state = bridge.ensure().expect("ensure bridge");
    let runtime = DockerRuntime::connect_with_container_network(
        DockerRuntimeConfig::new(
            NodeId::new("node-a"),
            temp.path().join("runtime"),
            "busybox:latest",
        )
        .with_command([
            "sh",
            "-c",
            "mkdir -p /www && echo ok >/www/index.html && httpd -f -p 8080 -h /www",
        ])
        .with_service_port(8080)
        .with_readiness_timeout(Duration::from_secs(10))
        .with_stop_timeout(Duration::from_secs(1))
        .with_dns_server(bridge_state.gateway_ip.to_string()),
        bridge.clone(),
    )
    .expect("docker runtime");
    let echo_spec = RuntimeInstanceSpec::new(
        InstanceId::new(format!("echo-{}", std::process::id())),
        ServiceName::new("echo"),
        RevisionId::new("rev-1"),
    );
    let echo = runtime.start(&echo_spec).expect("start echo");
    let paths = write_serving_snapshots(temp.path(), &echo.address);
    let actor = ServingActorHandle::spawn(IslandId::new("prod"), paths, Duration::from_secs(60))
        .expect("serving actor");
    let dns_addr = SocketAddr::new(IpAddr::V4(bridge_state.gateway_ip.address()), 53);
    let dns = spawn_dns_server(dns_addr, WireServingState::new(actor))
        .await
        .expect("dns server");
    let dns_server = bridge_state.gateway_ip.to_string();

    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--network",
            bridge_state.name.as_str(),
            "--dns",
            dns_server.as_str(),
            "busybox:latest",
            "wget",
            "-qO-",
            "http://echo.service.example.test:8080/",
        ])
        .output()
        .expect("run client container");

    let _ = dns.shutdown().await;
    let _ = runtime.stop(&echo_spec.instance_id);
    let _ = bridge.remove();

    assert!(
        output.status.success(),
        "client failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
}

fn write_serving_snapshots(root: &std::path::Path, backend: &str) -> ServingSnapshotPaths {
    let paths = ServingSnapshotPaths::new(root.join("gateway.json"), root.join("dns.json"));
    let gateway = GatewaySnapshotFile {
        schema_version: 1,
        island: "prod".to_string(),
        revision: "rev-1".to_string(),
        gateway_commit_id: "gateway-1".to_string(),
        route_commit_id: "route-1".to_string(),
        routes: vec![GatewayRouteProjection {
            route_id: RouteId::new("echo"),
            hostnames: vec!["echo.example.test".to_string()],
            backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-a"),
                address: backend.to_string(),
            }],
            old_backends_to_drain: Vec::new(),
        }],
        acme_http01: Vec::new(),
    };
    let dns = DnsSnapshotFile {
        schema_version: 1,
        island: "prod".to_string(),
        revision: "rev-1".to_string(),
        dns_commit_id: "dns-1".to_string(),
        records: Vec::new(),
    };
    std::fs::write(
        &paths.gateway,
        serde_json::to_vec(&gateway).expect("gateway json"),
    )
    .expect("write gateway");
    std::fs::write(&paths.dns, serde_json::to_vec(&dns).expect("dns json")).expect("write dns");
    write_serving_generation_manifest(&paths.gateway, &paths.dns).expect("serving generation");
    paths
}
