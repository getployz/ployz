mod common;

use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    os::unix::net::UnixDatagram,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use hyper_util::rt::TokioIo;
use ployz_core::{
    DESCRIBE_CONTRACT_CAPABILITY, MachineRpcClient, RESET_MACHINE_CAPABILITY, RpcRequest,
};
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint};
use tower::service_fn;

use common::TestDir;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn daemon_create_reopen_signal_and_reset_lifecycle() {
    let version = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .arg("version")
        .output()
        .unwrap();
    assert!(version.status.success());
    assert_eq!(
        version.stdout,
        format!("{}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    );

    let root = TestDir::new("ployzd-process");
    fs::create_dir_all(&root.0).unwrap();
    let data_dir = root.0.join("data");
    let socket = root.0.join("run/ployz.sock");
    let notify_socket = root.0.join("notify.sock");

    let (mut daemon, metrics_address) = start_daemon(&data_dir, &socket, &notify_socket);
    assert_eq!(mode(socket.parent().unwrap()), 0o750);
    assert_eq!(mode(&socket), 0o660);
    let first = describe(&socket);
    assert_eq!(first.daemon_version, env!("CARGO_PKG_VERSION"));
    assert!(first.supports(DESCRIBE_CONTRACT_CAPABILITY));
    assert!(first.supports(RESET_MACHINE_CAPABILITY));
    assert!(metrics(metrics_address).contains(&format!(
        "ployz_ployzd_build_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    )));
    Command::new("kill")
        .args(["-TERM", &daemon.0.id().to_string()])
        .status()
        .unwrap();
    wait_for_success(&mut daemon.0, "signal");

    let (mut daemon, _) = start_daemon(&data_dir, &socket, &notify_socket);
    assert_eq!(describe(&socket).machine_id, first.machine_id);

    reset(&socket);
    wait_for_success(&mut daemon.0, "reset");
    assert!(!data_dir.exists());
}

fn start_daemon(data_dir: &Path, socket: &Path, notify_socket: &Path) -> (ChildGuard, SocketAddr) {
    let _ = fs::remove_file(notify_socket);
    let notify = UnixDatagram::bind(notify_socket).unwrap();
    notify
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let metrics_address = unused_address();
    let child = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args([
            "--data-dir",
            data_dir.to_str().unwrap(),
            "--socket",
            socket.to_str().unwrap(),
            "--metrics-address",
            &metrics_address.to_string(),
        ])
        .env("NOTIFY_SOCKET", notify_socket)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut message = [0; 64];
    let length = notify.recv(&mut message).unwrap();
    assert_eq!(message.get(..length).unwrap(), b"READY=1\n");
    (ChildGuard(child), metrics_address)
}

fn unused_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

async fn connect(path: &Path) -> MachineRpcClient<Channel> {
    let path = path.to_owned();
    let channel = Endpoint::from_static("http://[::]:50051")
        .connect_with_connector(service_fn(move |_| {
            let path = path.clone();
            async move { UnixStream::connect(path).await.map(TokioIo::new) }
        }))
        .await
        .unwrap();
    MachineRpcClient::new(channel)
}

fn describe(path: &Path) -> ployz_core::ContractDescription {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        connect(path)
            .await
            .describe_contract(RpcRequest::describe_contract().encode().unwrap())
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode_contract_description()
            .unwrap()
            .clone()
    })
}

fn reset(path: &Path) {
    let response = tokio::runtime::Runtime::new().unwrap().block_on(async {
        connect(path)
            .await
            .reset(RpcRequest::reset().encode().unwrap())
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
    });
    response.decode_reset_accepted().unwrap();
}

fn metrics(address: SocketAddr) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match TcpStream::connect(address) {
            Ok(mut stream) => {
                stream.write_all(b"GET /metrics HTTP/1.0\r\n\r\n").unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                return response;
            }
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("metrics server did not start: {error}"),
        }
    }
}

fn wait_for_success(child: &mut Child, stage: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                let mut stderr = String::new();
                if let Some(mut output) = child.stderr.take() {
                    output.read_to_string(&mut stderr).unwrap();
                }
                assert!(status.success(), "ployzd exited with {status}: {stderr}");
                return;
            }
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            None => panic!("ployzd did not exit after {stage}"),
        }
    }
}
