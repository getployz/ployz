mod test_dir;

use std::{
    fs,
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    os::unix::net::UnixDatagram,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use ployz_core::{
    DESCRIBE_CONTRACT_CAPABILITY, DescribeContractRequest, MachineRpcClient,
    RESET_MACHINE_CAPABILITY, ResetRequest, op,
};
use tonic::transport::{Channel, Endpoint};

use test_dir::TestDir;

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
    let duplicate = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args([
            "--data-dir",
            data_dir.to_str().unwrap(),
            "--socket",
            socket.to_str().unwrap(),
            "--metrics-address",
            &unused_address().to_string(),
        ])
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert_eq!(describe(&socket).machine_id, first.machine_id);
    assert!(metrics(metrics_address).contains(&format!(
        "ployz_ployzd_build_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    )));
    let disconnected = TcpStream::connect(metrics_address).unwrap();
    disconnected.shutdown(Shutdown::Both).unwrap();
    thread::sleep(Duration::from_millis(20));
    assert_eq!(describe(&socket).machine_id, first.machine_id);
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

    let existing_parent = root.0.join("existing");
    fs::create_dir(&existing_parent).unwrap();
    set_mode(&existing_parent, 0o711);
    let existing_socket = existing_parent.join("ployz.sock");
    let (mut daemon, _) = start_daemon(
        &root.0.join("second-data"),
        &existing_socket,
        &notify_socket,
    );
    assert_eq!(mode(&existing_parent), 0o711);
    Command::new("kill")
        .args(["-TERM", &daemon.0.id().to_string()])
        .status()
        .unwrap();
    wait_for_success(&mut daemon.0, "existing socket parent");
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
    let child = ChildGuard(child);
    let mut message = [0; 64];
    let length = notify.recv(&mut message).unwrap();
    assert_eq!(message.get(..length).unwrap(), b"READY=1\n");
    (child, metrics_address)
}

fn unused_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

async fn connect(path: &Path) -> MachineRpcClient<Channel> {
    let channel = Endpoint::from_shared(format!("unix:{}", path.display()))
        .unwrap()
        .connect()
        .await
        .unwrap();
    MachineRpcClient::new(channel)
}

fn describe(path: &Path) -> ployz_core::ContractDescription {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        connect(path)
            .await
            .describe_contract(
                op::DescribeContract::into_request(DescribeContractRequest {})
                    .encode()
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode::<op::DescribeContract>()
            .unwrap()
            .clone()
    })
}

fn reset(path: &Path) {
    let response = tokio::runtime::Runtime::new().unwrap().block_on(async {
        connect(path)
            .await
            .reset(op::Reset::into_request(ResetRequest {}).encode().unwrap())
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
    });
    response.decode::<ployz_core::op::Reset>().unwrap();
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
