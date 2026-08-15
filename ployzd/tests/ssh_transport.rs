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
use ployz_core::{DescribeContractRequest, MachineRpcClient, MachineRpcServer, op};
use ployzd::{machine::LocalMachineStore, rpc::MachineService};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional},
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
    let user = current_user();
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
            .machine_id
            .clone();
        assert_eq!(actual, machine_id);
    }

    drop(sshd);
    tcp_server.abort();
    unix_server.abort();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
#[ignore = "Layer 3: requires sudo, ssh, sshd, and ssh-keygen"]
async fn system_ssh_proxy_preserves_binary_bytes_and_listener_cancellation() {
    let root = std::env::temp_dir().join(format!("ployz-proxy-openssh-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let ssh_port = unused_address().port();
    let sshd = start_sshd(&root, ssh_port);
    wait_for_port(SocketAddr::from(([127, 0, 0, 1], ssh_port)));
    let user = current_user();
    let connection =
        Connection::ssh(SshDestination::parse(format!("{user}@127.0.0.1:{ssh_port}")).unwrap())
            .with_ssh_key_file(root.join("client_key"))
            .unwrap();
    let connector = SystemConnector::new(ssh_wrapper(&root));

    let echo = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_address = echo.local_addr().unwrap();
    let echo_server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = echo.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buffer = [0_u8; 64];
                while let Ok(read) = stream.read(&mut buffer).await {
                    if read == 0
                        || stream
                            .write_all(buffer.get(..read).unwrap_or_default())
                            .await
                            .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });

    let local = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_address = local.local_addr().unwrap();
    let bridge = tokio::spawn(async move {
        loop {
            let (mut downstream, _) = local.accept().await.unwrap();
            let connector = connector.clone();
            let connection = connection.clone();
            tokio::spawn(async move {
                let mut upstream = connector
                    .dial_proxy(&connection, "tcp", &echo_address.to_string())
                    .await
                    .unwrap();
                copy_bidirectional(&mut downstream, &mut upstream)
                    .await
                    .unwrap();
            });
        }
    });

    for payload in [b"first\0\xff".as_slice(), b"\0second\n".as_slice()] {
        let mut client = tokio::net::TcpStream::connect(local_address).await.unwrap();
        client.write_all(payload).await.unwrap();
        let mut echoed = vec![0; payload.len()];
        client.read_exact(&mut echoed).await.unwrap();
        assert_eq!(echoed, payload);
    }
    bridge.abort();
    let _ = bridge.await;
    assert!(tokio::net::TcpStream::connect(local_address).await.is_err());

    echo_server.abort();
    drop(sshd);
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
            current_user(),
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

fn current_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| {
        String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned()
    })
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
