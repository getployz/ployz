use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream, UdpSocket};
use std::path::PathBuf;

use mvp_deploy::{DeployStatusPhase, InstanceId};
use mvp_node::{
    InitOptions, ProductDeployOptions, deploy_product_service_with_process, init_node,
    load_host_networking_snapshot, read_product_deploy_status,
};
use mvp_runtime::ProcessRuntime;

#[tokio::test]
async fn product_deploy_starts_runtime_and_projects_reachable_backend() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = init_node(
        InitOptions::new(temp.path())
            .with_island("prod")
            .with_node_id("node-a"),
    )
    .expect("init node");

    let runtime =
        ProcessRuntime::managed_http(state.paths().runtime_dir.clone(), mvp_node_binary());
    let report = deploy_product_service_with_process(
        ProductDeployOptions::new(temp.path())
            .with_deploy_id("deploy-1")
            .with_target_node("node-a")
            .with_service("web")
            .with_revision("rev-1")
            .with_hostname("web.example.test"),
        Some(runtime.clone()),
    )
    .await
    .expect("deploy service");

    assert_eq!(report.visible_nodes, 1);
    assert_eq!(report.host_network_backends, 1);
    let [backend] = report.active_backends.as_slice() else {
        panic!("expected one active backend");
    };
    assert!(backend.address.starts_with("127.0.0.1:"));
    let mut stream = TcpStream::connect(&backend.address).expect("connect deployed service");
    stream
        .write_all(b"GET / HTTP/1.1\r\nhost: web.example.test\r\n\r\n")
        .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    assert!(response.contains("instance=deploy-1-web-rev-1"));
    assert!(state.paths().gateway_snapshot.exists());
    assert!(state.paths().dns_snapshot.exists());
    assert!(
        load_host_networking_snapshot(&state)
            .expect("load host network snapshot")
            .is_some()
    );
    runtime
        .stop(&InstanceId::new("deploy-1-web-rev-1"))
        .expect("stop deployed instance");
}

#[tokio::test]
async fn product_deploy_fails_fast_when_local_daemon_owns_transport_port() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = init_node(
        InitOptions::new(temp.path())
            .with_island("prod")
            .with_node_id("node-a"),
    )
    .expect("init node");
    let _daemon_port =
        UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, state.p2panda_port()))
            .expect("bind daemon port");
    let runtime =
        ProcessRuntime::managed_http(state.paths().runtime_dir.clone(), mvp_node_binary());

    let error = deploy_product_service_with_process(
        ProductDeployOptions::new(temp.path())
            .with_deploy_id("deploy-1")
            .with_target_node("node-a")
            .with_service("web")
            .with_revision("rev-1")
            .with_hostname("web.example.test"),
        Some(runtime),
    )
    .await
    .expect_err("deploy should fail while daemon port is owned");

    assert!(
        error
            .to_string()
            .contains("stop it before standalone deploy")
    );
}

#[tokio::test]
async fn product_deploy_records_durable_failure_status() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = init_node(
        InitOptions::new(temp.path())
            .with_island("prod")
            .with_node_id("node-a"),
    )
    .expect("init node");
    let runtime =
        ProcessRuntime::managed_http(state.paths().runtime_dir.clone(), mvp_node_binary());

    deploy_product_service_with_process(
        ProductDeployOptions::new(temp.path())
            .with_deploy_id("deploy-fails")
            .with_target_node("missing-node")
            .with_service("web")
            .with_revision("rev-1")
            .with_hostname("web.example.test"),
        Some(runtime),
    )
    .await
    .expect_err("deploy should fail without a target responder");

    let status = read_product_deploy_status(temp.path(), "deploy-fails").expect("deploy status");
    let phases = status
        .statuses
        .iter()
        .map(|status| status.phase)
        .collect::<Vec<_>>();
    assert!(phases.contains(&DeployStatusPhase::Planned));
    assert!(phases.contains(&DeployStatusPhase::Failed));
    assert_failed_status_has_message(&status.statuses);
}

#[tokio::test]
async fn product_deploy_update_drains_and_stops_old_backend() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = init_node(
        InitOptions::new(temp.path())
            .with_island("prod")
            .with_node_id("node-a"),
    )
    .expect("init node");
    let runtime =
        ProcessRuntime::managed_http(state.paths().runtime_dir.clone(), mvp_node_binary());

    let first = deploy_product_service_with_process(
        ProductDeployOptions::new(temp.path())
            .with_deploy_id("deploy-1")
            .with_target_node("node-a")
            .with_service("web")
            .with_revision("rev-1")
            .with_hostname("web.example.test"),
        Some(runtime.clone()),
    )
    .await
    .expect("first deploy");
    let old_address = first.active_backends[0].address.clone();

    let second = deploy_product_service_with_process(
        ProductDeployOptions::new(temp.path())
            .with_deploy_id("deploy-2")
            .with_target_node("node-a")
            .with_service("web")
            .with_revision("rev-2")
            .with_hostname("web.example.test"),
        Some(runtime.clone()),
    )
    .await
    .expect("second deploy");

    assert_eq!(second.old_backends_to_drain.len(), 1);
    assert_eq!(second.old_backends_to_drain[0].address, old_address);
    assert_ne!(second.active_backends[0].address, old_address);
    assert!(TcpStream::connect(&old_address).is_err());
    assert!(TcpStream::connect(&second.active_backends[0].address).is_ok());
    runtime
        .stop(&InstanceId::new("deploy-2-web-rev-2"))
        .expect("stop updated instance");
}

#[tokio::test]
async fn product_deploy_keeps_old_backends_route_scoped() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = init_node(
        InitOptions::new(temp.path())
            .with_island("prod")
            .with_node_id("node-a"),
    )
    .expect("init node");
    let runtime =
        ProcessRuntime::managed_http(state.paths().runtime_dir.clone(), mvp_node_binary());

    let web = deploy_product_service_with_process(
        ProductDeployOptions::new(temp.path())
            .with_deploy_id("deploy-web")
            .with_target_node("node-a")
            .with_service("web")
            .with_revision("rev-1")
            .with_hostname("web.example.test"),
        Some(runtime.clone()),
    )
    .await
    .expect("web deploy");
    let api = deploy_product_service_with_process(
        ProductDeployOptions::new(temp.path())
            .with_deploy_id("deploy-api")
            .with_target_node("node-a")
            .with_service("api")
            .with_revision("rev-1")
            .with_hostname("api.example.test"),
        Some(runtime.clone()),
    )
    .await
    .expect("api deploy");

    assert_eq!(api.old_backends_to_drain, Vec::new());
    assert_eq!(api.active_backends.len(), 1);
    assert_ne!(
        api.active_backends[0].address,
        web.active_backends[0].address
    );
    assert!(TcpStream::connect(&web.active_backends[0].address).is_ok());
    assert!(TcpStream::connect(&api.active_backends[0].address).is_ok());
    runtime
        .stop(&InstanceId::new("deploy-web-web-rev-1"))
        .expect("stop web instance");
    runtime
        .stop(&InstanceId::new("deploy-api-api-rev-1"))
        .expect("stop api instance");
}

#[tokio::test]
async fn product_deploy_reserves_serving_epochs_between_failed_attempts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = init_node(
        InitOptions::new(temp.path())
            .with_island("prod")
            .with_node_id("node-a"),
    )
    .expect("init node");
    let runtime =
        ProcessRuntime::managed_http(state.paths().runtime_dir.clone(), mvp_node_binary());

    deploy_product_service_with_process(
        ProductDeployOptions::new(temp.path())
            .with_deploy_id("deploy-fails-1")
            .with_target_node("missing-node")
            .with_service("web")
            .with_revision("rev-1")
            .with_hostname("web.example.test"),
        Some(runtime.clone()),
    )
    .await
    .expect_err("first deploy should fail after reserving epoch");
    deploy_product_service_with_process(
        ProductDeployOptions::new(temp.path())
            .with_deploy_id("deploy-fails-2")
            .with_target_node("missing-node")
            .with_service("web")
            .with_revision("rev-2")
            .with_hostname("web.example.test"),
        Some(runtime.clone()),
    )
    .await
    .expect_err("second deploy should advance beyond reserved epoch");

    let first_status =
        read_product_deploy_status(temp.path(), "deploy-fails-1").expect("first deploy status");
    let second_status =
        read_product_deploy_status(temp.path(), "deploy-fails-2").expect("second deploy status");
    assert_failed_status_has_message(&first_status.statuses);
    assert_failed_status_has_message(&second_status.statuses);
}

fn mvp_node_binary() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_mvp-node")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_mvp-node")))
}

fn assert_failed_status_has_message(statuses: &[mvp_deploy::DeployStatusFact]) {
    assert!(
        statuses
            .iter()
            .find(|status| status.phase == DeployStatusPhase::Failed)
            .and_then(|status| status.message.as_deref())
            .is_some_and(|message| !message.is_empty())
    );
}
