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
            let body = machine_id.as_str();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
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
