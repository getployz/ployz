use std::{
    fs,
    net::{SocketAddr, TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use ployz::{
    connect::{Connector, SystemConnector},
    context::{Connection, SshDestination},
};
use ployz_core::{MachineRpcClient, MachineRpcServer, RpcRequest};
use ployzd::{machine::LocalMachineStore, rpc::MachineService};
use tokio::{
    net::{TcpListener as TokioTcpListener, UnixListener},
    sync::watch,
};
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tonic::transport::Server;

#[tokio::test]
#[ignore = "Layer 3: requires sudo, ssh, sshd, and ssh-keygen"]
async fn real_machine_discovery_matches_over_tcp_unix_and_system_ssh() {
    let root = std::env::temp_dir().join(format!("ployz-openssh-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let socket = root.join("ployz.sock");
    let store = Arc::new(Mutex::new(
        LocalMachineStore::open(root.join("data")).unwrap(),
    ));
    let machine_id = store.lock().unwrap().record().id.clone();
    let (reset, _) = watch::channel(false);
    let service = MachineService::new(store, reset);
    let tcp = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp_address = tcp.local_addr().unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let tcp_server = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(service.clone()))
            .serve_with_incoming(TcpListenerStream::new(tcp)),
    );
    let unix_server = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(service))
            .serve_with_incoming(UnixListenerStream::new(listener)),
    );
    let port = unused_address().port();
    let sshd = start_sshd(&root, port);
    wait_for_port(SocketAddr::from(([127, 0, 0, 1], port)));
    let ssh = ssh_wrapper(&root);
    let user = std::env::var("USER").unwrap();
    let connection =
        Connection::ssh(SshDestination::parse(format!("{user}@127.0.0.1:{port}")).unwrap())
            .with_ssh_key_file(root.join("client_key"))
            .unwrap();
    let connector = SystemConnector::new(ssh);

    for connection in [
        Connection::tcp(tcp_address),
        Connection::unix(&socket).unwrap(),
        connection,
    ] {
        let channel = connector.connect(&connection).await.unwrap();
        let actual = MachineRpcClient::new(channel)
            .describe_contract(RpcRequest::describe_contract().encode().unwrap())
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode_contract_description()
            .unwrap()
            .machine_id
            .clone();
        assert_eq!(actual, machine_id);
    }

    drop(sshd);
    tcp_server.abort();
    unix_server.abort();
    fs::remove_dir_all(root).unwrap();
}

fn start_sshd(root: &Path, port: u16) -> ChildGuard {
    let host_key = root.join("host_key");
    let client_key = root.join("client_key");
    generate_key(&host_key);
    generate_key(&client_key);
    let daemon = PathBuf::from(env!("CARGO_BIN_EXE_ployzd"));
    let remote = root.join("remote-command");
    fs::write(
        &remote,
        format!(
            "#!/bin/sh\n[ \"$SSH_ORIGINAL_COMMAND\" = true ] && exit 0\nexec '{}' --socket '{}' dial-stdio\n",
            daemon.display(),
            root.join("ployz.sock").display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&remote, fs::Permissions::from_mode(0o700)).unwrap();
    let public_key = fs::read_to_string(client_key.with_extension("pub")).unwrap();
    let authorized_keys = root.join("authorized_keys");
    fs::write(
        &authorized_keys,
        format!("command=\"{}\" {public_key}", remote.display(),),
    )
    .unwrap();
    let config = root.join("sshd_config");
    fs::write(
        &config,
        format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\nAuthorizedKeysFile {}\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM yes\nPermitRootLogin no\nStrictModes no\nAllowUsers {}\nLogLevel ERROR\n",
            host_key.display(),
            root.join("sshd.pid").display(),
            authorized_keys.display(),
            std::env::var("USER").unwrap(),
        ),
    )
    .unwrap();
    let child = Command::new("sudo")
        .args(["-n", "/usr/sbin/sshd", "-D", "-e", "-f"])
        .arg(config)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    ChildGuard(child)
}

fn generate_key(path: &Path) {
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

fn ssh_wrapper(root: &Path) -> PathBuf {
    let path = root.join("ssh");
    fs::write(
        &path,
        "#!/bin/sh\nexec /usr/bin/ssh -o ControlMaster=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn unused_address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

fn wait_for_port(address: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(address).is_err() {
        assert!(Instant::now() < deadline, "sshd did not start");
        thread::sleep(Duration::from_millis(10));
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
