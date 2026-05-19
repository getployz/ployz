use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use hickory_proto::op::{Message, MessageType, OpCode, Query};
use hickory_proto::rr::{Name, RData, RecordType};
use mvp_deploy::InstanceId;
use mvp_node::{
    InitOptions, ProductDeployOptions, ServingRoleProcessStatus, ServingRoleRequest,
    ServingRoleResponse, ServingRoleSuccess, deploy_product_service_with_process, init_node,
};
use mvp_runtime::ProcessRuntime;

#[test]
fn gateway_and_dns_roles_serve_snapshots_without_daemon() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = init_node(
        InitOptions::new(temp.path())
            .with_island("prod")
            .with_node_id("node-a"),
    )
    .expect("init node");
    let runtime =
        ProcessRuntime::managed_http(state.paths().runtime_dir.clone(), mvp_node_binary());
    let async_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_io()
        .enable_time()
        .build()
        .expect("tokio runtime");
    async_runtime
        .block_on(deploy_product_service_with_process(
            ProductDeployOptions::new(temp.path())
                .with_deploy_id("deploy-1")
                .with_target_node("node-a")
                .with_service("web")
                .with_revision("rev-1")
                .with_hostname("web.example.test"),
            Some(runtime.clone()),
        ))
        .expect("deploy service");

    let gateway_control = temp.path().join("gateway.sock");
    let dns_control = temp.path().join("dns.sock");
    let mut gateway = SpawnedRole::start(
        "gateway",
        temp.path(),
        "127.0.0.1:0",
        gateway_control.as_path(),
    );
    let mut dns = SpawnedRole::start("dns", temp.path(), "127.0.0.1:0", dns_control.as_path());

    let gateway_status = wait_for_status(gateway_control.as_path());
    let dns_status = wait_for_status(dns_control.as_path());

    assert!(
        http_get(gateway_status.listen_addr, "web.example.test")
            .contains("instance=deploy-1-web-rev-1")
    );
    assert_eq!(
        dns_a_lookup(dns_status.listen_addr, "web.example.test"),
        "127.0.0.1"
    );

    std::fs::write(state.paths().gateway_snapshot.as_path(), b"not json")
        .expect("corrupt gateway snapshot");
    let reload = control_request(gateway_control.as_path(), &ServingRoleRequest::Reload);
    assert!(matches!(reload, ServingRoleResponse::Failure(_)));
    let status_after_bad_reload = wait_for_status(gateway_control.as_path());
    assert!(status_after_bad_reload.serving.last_failure.is_some());
    assert!(
        http_get(gateway_status.listen_addr, "web.example.test")
            .contains("instance=deploy-1-web-rev-1")
    );

    shutdown_role(gateway_control.as_path());
    shutdown_role(dns_control.as_path());
    gateway.wait();
    dns.wait();
    runtime
        .stop(&InstanceId::new("deploy-1-web-rev-1"))
        .expect("stop deployed instance");
}

struct SpawnedRole {
    child: Child,
}

impl SpawnedRole {
    fn start(command: &str, state: &Path, listen: &str, control: &Path) -> Self {
        let child = Command::new(mvp_node_binary())
            .arg(command)
            .arg("--state")
            .arg(state)
            .arg("--listen")
            .arg(listen)
            .arg("--control")
            .arg(control)
            .spawn()
            .expect("spawn serving role");
        Self { child }
    }

    fn wait(&mut self) {
        let status = self.child.wait().expect("wait serving role");
        assert!(status.success());
    }
}

impl Drop for SpawnedRole {
    fn drop(&mut self) {
        if self.child.try_wait().expect("poll serving role").is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn wait_for_status(control: &Path) -> ServingRoleProcessStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match control_request(control, &ServingRoleRequest::Readiness) {
            ServingRoleResponse::Success(ServingRoleSuccess::Status(status)) => return *status,
            ServingRoleResponse::Success(ServingRoleSuccess::Reloaded(status)) => return *status,
            ServingRoleResponse::Success(ServingRoleSuccess::Shutdown) => {
                panic!("role shut down before readiness")
            }
            ServingRoleResponse::Failure(failure) => {
                assert!(
                    Instant::now() < deadline,
                    "role readiness failed: {failure:?}"
                );
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn shutdown_role(control: &Path) {
    let response = control_request(control, &ServingRoleRequest::Shutdown);
    assert_eq!(
        response,
        ServingRoleResponse::Success(ServingRoleSuccess::Shutdown)
    );
}

fn control_request(control: &Path, request: &ServingRoleRequest) -> ServingRoleResponse {
    let bytes = serde_json::to_vec(request).expect("encode request");
    let mut stream = connect_control(control);
    stream.write_all(&bytes).expect("write request");
    stream
        .shutdown(std::net::Shutdown::Write)
        .expect("shutdown write");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    serde_json::from_slice(&response).expect("decode response")
}

fn connect_control(control: &Path) -> std::os::unix::net::UnixStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match std::os::unix::net::UnixStream::connect(control) {
            Ok(stream) => return stream,
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "connect control '{}': {error}",
                    control.display()
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

fn http_get(addr: SocketAddr, host: &str) -> String {
    let mut stream = TcpStream::connect(addr).expect("connect gateway");
    stream
        .write_all(format!("GET / HTTP/1.1\r\nhost: {host}\r\n\r\n").as_bytes())
        .expect("write gateway request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read gateway response");
    response
        .split_once("\r\n\r\n")
        .map(|(_headers, body)| body.to_string())
        .expect("http response body")
}

fn dns_a_lookup(addr: SocketAddr, host: &str) -> String {
    let name = Name::from_ascii(format!("{host}.")).expect("query name");
    let mut request = Message::new(7, MessageType::Query, OpCode::Query);
    request.add_query(Query::query(name, RecordType::A));
    let request = request.to_vec().expect("encode dns query");

    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind dns client");
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set dns timeout");
    socket.send_to(&request, addr).expect("send dns query");
    let mut packet = [0_u8; 1232];
    let (len, _) = socket.recv_from(&mut packet).expect("receive dns response");
    let response = Message::from_vec(&packet[..len]).expect("decode dns response");
    response
        .answers
        .iter()
        .find_map(|record| match &record.data {
            RData::A(address) => Some(address.to_string()),
            _ => None,
        })
        .expect("A answer")
}

fn mvp_node_binary() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_mvp-node")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_mvp-node")))
}
