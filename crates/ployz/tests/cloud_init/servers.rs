//! Network listeners for the fake Machine daemon and Ingress probe.

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures_util::StreamExt as _;
use ployz_core::{MachineId, MachineRpcServer};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixListener},
    task::JoinHandle,
};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

use super::JoinDaemon;

pub async fn serve_machine(daemon: JoinDaemon) -> SocketAddr {
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = tcp.local_addr().unwrap();
    tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(daemon))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(tcp)),
    );
    address
}

pub async fn serve_ingress_probe(machine_id: MachineId) -> (JoinHandle<()>, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            // Probe retry coverage lives in dns::probe::tests; this fixture
            // verifies that founder enrollment reaches the real HTTP endpoint.
            let body = machine_id.as_str();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    (server, port)
}

pub async fn serve_local_machine(daemon: JoinDaemon) -> (String, PathBuf, Arc<AtomicUsize>) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let socket = std::env::temp_dir().join(format!(
        "ployz-cloud-enroll-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&socket);
    let unix = UnixListener::bind(&socket).unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let accepted = Arc::clone(&connections);
    let incoming = UnixListenerStream::new(unix).inspect(move |connection| {
        if connection.is_ok() {
            accepted.fetch_add(1, Ordering::SeqCst);
        }
    });
    tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(daemon))
            .serve_with_incoming(incoming),
    );
    (format!("unix://{}", socket.display()), socket, connections)
}

// Exercise the real SSH export path without host installation or public relays.
pub fn cli() -> tokio::process::Command {
    use std::os::unix::fs::PermissionsExt;
    static SSH: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let directory = SSH.get_or_init(|| {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("ssh");
        std::fs::write(
            &script,
            r#"#!/usr/bin/python3
import os, socket, sys, threading
args = sys.argv[1:]
if args[-1] == 'true':
    sys.exit(0)
if 'ployzd-tailcat export' in args[-1]:
    assert 'ployzd install --software-only --version' in args[-1]
    print('fixture-tailcat-capability')
    sys.exit(0)
assert args[-2:] == ['ployzd', 'dial-stdio'], args
port = int(args[args.index('-p') + 1])
sock = socket.create_connection(('127.0.0.1', port))
def send():
    try:
        while data := os.read(0, 65536):
            sock.sendall(data)
        sock.shutdown(socket.SHUT_WR)
    except OSError:
        pass
threading.Thread(target=send, daemon=True).start()
try:
    while data := sock.recv(65536):
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()
except (OSError, BrokenPipeError):
    pass
"#,
        )
        .unwrap();
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let export = directory.path().join("ployzd-tailcat");
        std::fs::write(
            &export,
            "#!/bin/sh\n[ \"$1\" = export ] || exit 1\nprintf '%s\\n' fixture-tailcat-capability\n",
        )
        .unwrap();
        std::fs::set_permissions(export, std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    });
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
    let paths = std::iter::once(directory.path().to_owned())
        .chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ))
        .collect::<Vec<_>>();
    command.env("PATH", std::env::join_paths(paths).unwrap());
    command
}
