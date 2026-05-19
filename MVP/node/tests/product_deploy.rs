use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;

use mvp_deploy::InstanceId;
use mvp_node::{
    InitOptions, ProductDeployOptions, deploy_product_service_with_process, init_node,
    load_host_networking_snapshot,
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

fn mvp_node_binary() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_mvp-node")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_mvp-node")))
}
