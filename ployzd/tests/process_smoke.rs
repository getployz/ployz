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
    AdvertisedEndpoint, DESCRIBE_CONTRACT_CAPABILITY, DescribeContractRequest, InspectRequest,
    JoinRequest, Machine, MachineId, MachineName, MachineRpcClient, Registered, op,
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
fn version_notify_socket_modes_and_signal() {
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
    assert!(metrics(metrics_address).contains(&format!(
        "ployz_ployzd_build_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    )));
    let disconnected = TcpStream::connect(metrics_address).unwrap();
    disconnected.shutdown(Shutdown::Both).unwrap();
    thread::sleep(Duration::from_millis(20));
    assert_eq!(describe(&socket).machine_id, first.machine_id);
    let stderr = terminate(&mut daemon.0);
    assert!(
        stderr.contains("started") && stderr.contains("uninitialized"),
        "start missing from journal stderr: {stderr}"
    );
    assert!(
        stderr.contains("shutting down") && stderr.contains("SIGTERM"),
        "SIGTERM shutdown missing from journal stderr: {stderr}"
    );

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
    let _ = terminate(&mut daemon.0);
}

#[test]
fn join_leaves_a_journal_line_and_records_the_restart() {
    let root = TestDir::new("ployzd-process-join");
    fs::create_dir_all(&root.0).unwrap();
    let notify_socket = root.0.join("notify.sock");
    let socket = root.0.join("run/ployz.sock");
    let (mut daemon, _) = start_daemon(&root.0.join("data"), &socket, &notify_socket);
    join(&socket);
    let stderr = wait_and_stderr(&mut daemon.0, "join");
    assert!(
        stderr.contains("join accepted"),
        "join missing from journal stderr: {stderr}"
    );
    assert!(
        stderr.contains("shutting down") && stderr.contains("restart requested"),
        "join restart reason missing from journal stderr: {stderr}"
    );
}

#[test]
fn verbosity_is_raised_without_editing_the_unit() {
    let root = TestDir::new("ployzd-process-verbosity");
    fs::create_dir_all(&root.0).unwrap();
    let notify_socket = root.0.join("notify.sock");

    let (mut quiet, _) = start_daemon_with(
        &root.0.join("quiet-data"),
        &root.0.join("quiet/ployz.sock"),
        &notify_socket,
        &[("PLOYZ_LOG", "error")],
        &[],
    );
    let stderr = terminate(&mut quiet.0);
    assert!(
        !stderr.contains("started"),
        "PLOYZ_LOG=error still emitted start: {stderr}"
    );

    let (mut debug, _) = start_daemon_with(
        &root.0.join("debug-data"),
        &root.0.join("debug/ployz.sock"),
        &notify_socket,
        &[("PLOYZ_LOG", "error")],
        &["--log-level", "debug"],
    );
    let stderr = terminate(&mut debug.0);
    assert!(
        stderr.contains("listening"),
        "--log-level debug missing listening line: {stderr}"
    );
}

#[test]
fn invalid_log_level_fails_before_start() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzd"))
        .args(["--log-level", "foo=notalevel"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error parsing level filter"),
        "init error missing from stderr: {stderr}"
    );
}

fn start_daemon(data_dir: &Path, socket: &Path, notify_socket: &Path) -> (ChildGuard, SocketAddr) {
    start_daemon_with(data_dir, socket, notify_socket, &[], &[])
}

fn start_daemon_with(
    data_dir: &Path,
    socket: &Path,
    notify_socket: &Path,
    env: &[(&str, &str)],
    extra_args: &[&str],
) -> (ChildGuard, SocketAddr) {
    let _ = fs::remove_file(notify_socket);
    let notify = UnixDatagram::bind(notify_socket).unwrap();
    notify
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let metrics_address = unused_address();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployzd"));
    command
        .args([
            "--data-dir",
            data_dir.to_str().unwrap(),
            "--socket",
            socket.to_str().unwrap(),
            "--metrics-address",
            &metrics_address.to_string(),
        ])
        .args(extra_args)
        .env("NOTIFY_SOCKET", notify_socket)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let child = ChildGuard(command.spawn().unwrap());
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

fn terminate(child: &mut Child) -> String {
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    wait_and_stderr(child, "signal")
}

fn wait_and_stderr(child: &mut Child, stage: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                let mut stderr = String::new();
                if let Some(mut output) = child.stderr.take() {
                    output.read_to_string(&mut stderr).unwrap();
                }
                assert!(status.success(), "ployzd exited with {status}: {stderr}");
                return stderr;
            }
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            None => panic!("ployzd did not exit after {stage}"),
        }
    }
}

fn inspect(path: &Path) -> ployz_core::MachineDetails {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        connect(path)
            .await
            .inspect(
                op::Inspect::into_request(InspectRequest {
                    advertised_endpoints: vec![AdvertisedEndpoint(
                        "192.0.2.1:51820".parse().unwrap(),
                    )],
                    ..Default::default()
                })
                .encode()
                .unwrap(),
            )
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode::<op::Inspect>()
            .unwrap()
    })
}

fn join(path: &Path) {
    let details = inspect(path);
    let peer_key = ployzd::network::WireGuardPrivateKey::generate().public_key();
    let assigned = Machine {
        id: MachineId::random(),
        name: MachineName::parse("joined").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        management_address: ployzd::network::management_address(details.public_key),
        public_key: details.public_key,
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.2:51820".parse().unwrap())],
        runtime: Default::default(),
    };
    let peer = Machine {
        id: MachineId::random(),
        name: MachineName::parse("peer").unwrap(),
        subnet: "10.210.0.0/24".parse().unwrap(),
        management_address: ployzd::network::management_address(peer_key),
        public_key: peer_key,
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
        runtime: Default::default(),
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        connect(path)
            .await
            .join(
                op::Join::into_request(JoinRequest {
                    registration: Registered {
                        assigned_machine: assigned,
                        visible_peers: vec![peer],
                        target_versions: [("actor".into(), 1)].into(),
                    },
                    wireguard_mtu: None,
                })
                .encode()
                .unwrap(),
            )
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode::<op::Join>()
            .unwrap();
    });
}
