use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::StreamExt;
use ployz_core::{
    ContainerId, ContainerLogsRequest, ExecConfig, ExecOptions, ExecRequestFrame,
    ExecResponseFrame, LogBody, LogEntry, LogsOptions, MachineId, MachineName, MachineRpcClient,
    MachineRpcServer, OpaquePayload, op,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::{Request, Status, transport::Server};

use super::*;

#[tokio::test]
async fn exec_forwards_output_while_docker_inspection_is_pending() {
    let root = TestRoot::new();
    let docker_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let docker_address = docker_listener.local_addr().unwrap();
    let (release_inspection, inspection_released) = tokio::sync::oneshot::channel();
    let docker_server = tokio::spawn(async move {
        let mut create = accept_docker_request(&docker_listener).await;
        create
            .write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 13\r\nConnection: close\r\n\r\n{\"Id\":\"slow\"}")
            .await
            .unwrap();
        drop(create);
        let mut hijack = accept_docker_request(&docker_listener).await;
        hijack
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n",
            )
            .await
            .unwrap();
        let mut inspect = accept_docker_request(&docker_listener).await;
        hijack
            .write_all(b"\x01\0\0\0\0\0\0\x04out\n\x02\0\0\0\0\0\0\x04err\n")
            .await
            .unwrap();
        inspection_released.await.unwrap();
        inspect
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 30\r\nConnection: close\r\n\r\n{\"Running\":false,\"ExitCode\":7}")
            .await
            .unwrap();
    });
    let docker = bollard::Docker::connect_with_http(
        &format!("http://{docker_address}"),
        5,
        bollard::API_DEFAULT_VERSION,
    )
    .unwrap();
    let machine_store = crate::machine::LocalMachineStore::open(&root.0).unwrap();
    let specs = MachineSpecStore::open(root.0.join("machine.db"))
        .await
        .unwrap();
    let runtime = ContainerRuntime::new(LocalDocker::from_client(docker), specs);
    let (restart, _) = tokio::sync::watch::channel(false);
    let service = crate::rpc::MachineService::new(Arc::new(Mutex::new(machine_store)), restart)
        .with_containers(runtime);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    let mut client = MachineRpcClient::connect(format!("http://{address}"))
        .await
        .unwrap();
    let config = exec_config(&ContainerId::parse("a".repeat(64)).unwrap(), ["sh"], false);
    let mut output = client
        .exec(Request::new(tokio_stream::iter([config])))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        ExecResponseFrame::decode(&output.message().await.unwrap().unwrap()).unwrap(),
        ExecResponseFrame::ExecId("slow".into())
    );
    let frames = tokio::time::timeout(Duration::from_secs(2), async {
        let stdout = output.message().await.unwrap().unwrap();
        let stderr = output.message().await.unwrap().unwrap();
        [
            ExecResponseFrame::decode(&stdout).unwrap(),
            ExecResponseFrame::decode(&stderr).unwrap(),
        ]
    })
    .await;
    release_inspection.send(()).unwrap();
    docker_server.await.unwrap();
    assert_eq!(
        frames.expect("exec output stalled behind the pending Docker inspection"),
        [
            ExecResponseFrame::Stdout(b"out\n".to_vec()),
            ExecResponseFrame::Stderr(b"err\n".to_vec()),
        ]
    );
    assert_eq!(
        ExecResponseFrame::decode(&output.message().await.unwrap().unwrap()).unwrap(),
        ExecResponseFrame::Exit(7)
    );
    assert!(output.message().await.unwrap().is_none());
    server.abort();
}

async fn accept_docker_request(listener: &tokio::net::TcpListener) -> tokio::net::TcpStream {
    let (stream, _) = listener.accept().await.unwrap();
    let mut reader = BufReader::new(stream);
    let mut content_length = 0;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap();
        }
    }
    reader
        .read_exact(&mut vec![0; content_length])
        .await
        .unwrap();
    reader.into_inner()
}

#[tokio::test]
#[ignore = "requires Docker and alpine:3.23.3"]
async fn l3_015_through_l3_024_exec_and_l3_069_logs_cross_the_real_docker_endpoint() {
    let _lock = DOCKER_NETWORK_LOCK.lock().await;
    let root = TestRoot::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut machine_store = crate::machine::LocalMachineStore::open(&root.0).unwrap();
    let machine = machine_store
        .initialize(
            MachineName::parse("stream-test").unwrap(),
            crate::machine::FoundingCluster {
                network: "10.210.0.0/16".parse().unwrap(),
                ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
            },
            None,
            vec![ployz_core::AdvertisedEndpoint(address)],
            None,
            None,
        )
        .unwrap();
    let machine_store = Arc::new(Mutex::new(machine_store));
    let specs = MachineSpecStore::open(root.0.join("machine.db"))
        .await
        .unwrap();
    let docker = LocalDocker::connect().unwrap();
    let runtime = ContainerRuntime::new(docker.clone(), specs);
    let created_network = ensure_ployz_network(&docker.client).await;
    let service_id = ServiceId::random();
    let service_name = ServiceName::parse("stream-api").unwrap();
    let mut spec = fixture_spec(&service_id, &service_name);
    spec.container.command = vec![
        "sh".into(),
        "-c".into(),
        "printf 'container-out\\n'; sleep 0.05; printf 'container-err\\n' >&2; sleep 30".into(),
    ];
    let created = runtime
        .create_for_test(
            &machine.id,
            TEST_GATEWAY,
            ContainerKind::ServiceContainer,
            &ProjectName::parse("app").unwrap(),
            &spec,
        )
        .await
        .unwrap();
    runtime.start(&created.container_id).await.unwrap();

    let (restart, _) = tokio::sync::watch::channel(false);
    let service =
        crate::rpc::MachineService::new(machine_store, restart).with_containers(runtime.clone());
    let server = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    let mut client = MachineRpcClient::connect(format!("http://{address}"))
        .await
        .unwrap();

    let frames = exec_frames(
        &mut client,
        &machine.id,
        &created.container_id,
        ["sh", "-c", "printf out; printf err >&2; exit 42"],
        false,
    )
    .await
    .unwrap();
    assert!(frames.contains(&ExecResponseFrame::Stdout(b"out".to_vec())));
    assert!(frames.contains(&ExecResponseFrame::Stderr(b"err".to_vec())));
    assert!(matches!(frames.last(), Some(ExecResponseFrame::Exit(42))));

    let open_config = exec_config(
        &created.container_id,
        ["sh", "-c", "sleep 30 & exit 7"],
        false,
    );
    let (request_sender, request_receiver) = tokio::sync::mpsc::channel(1);
    request_sender.send(open_config).await.unwrap();
    let mut request = Request::new(ReceiverStream::new(request_receiver));
    request
        .metadata_mut()
        .insert("machine", machine.id.as_str().parse().unwrap());
    let mut output = client.exec(request).await.unwrap().into_inner();
    let open_frames = tokio::time::timeout(Duration::from_secs(5), async {
        let mut frames = Vec::new();
        while let Some(frame) = output.message().await? {
            frames.push(ExecResponseFrame::decode(&frame).unwrap());
        }
        Ok::<_, Status>(frames)
    })
    .await
    .expect("exec output must close while the request stream remains open")
    .unwrap();
    assert!(matches!(
        open_frames.last(),
        Some(ExecResponseFrame::Exit(7))
    ));
    drop(request_sender);

    let detached = exec_frames(
        &mut client,
        &machine.id,
        &created.container_id,
        ["sh", "-c", "sleep 1"],
        true,
    )
    .await
    .unwrap();
    assert!(matches!(
        detached.as_slice(),
        [ExecResponseFrame::ExecId(_)]
    ));

    let empty = ExecRequestFrame::Config(ExecConfig {
        container_id: created.container_id,
        options: ExecOptions {
            command: vec![],
            attach_stdin: false,
            attach_stdout: true,
            attach_stderr: true,
            tty: false,
            detach: false,
        },
    })
    .encode()
    .unwrap();
    let mut request = Request::new(tokio_stream::iter([empty]));
    request
        .metadata_mut()
        .insert("machine", machine.id.as_str().parse().unwrap());
    assert_eq!(
        client.exec(request).await.unwrap_err().code(),
        tonic::Code::InvalidArgument
    );

    let logs = op::ContainerLogs::into_request(ContainerLogsRequest {
        container_id: created.container_id,
        options: LogsOptions {
            follow: false,
            tail: -1,
            since_unix_seconds: None,
            until_unix_seconds: None,
        },
    })
    .encode()
    .unwrap();
    let mut request = Request::new(logs);
    request
        .metadata_mut()
        .insert("machine", machine.id.as_str().parse().unwrap());
    let entries = client
        .container_logs(request)
        .await
        .unwrap()
        .into_inner()
        .map(|entry| LogEntry::decode(&entry.unwrap()).unwrap())
        .collect::<Vec<_>>()
        .await;
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry.body, LogBody::Stdout(_) | LogBody::Stderr(_)))
            .map(|entry| entry.body.clone())
            .collect::<Vec<_>>(),
        [
            LogBody::Stdout(b"container-out\n".to_vec()),
            LogBody::Stderr(b"container-err\n".to_vec()),
        ]
    );

    runtime
        .remove(&created.container_id, true, true)
        .await
        .unwrap();
    server.abort();
    cleanup_ployz_network(&docker.client, created_network).await;
}

async fn exec_frames<const N: usize>(
    client: &mut MachineRpcClient<tonic::transport::Channel>,
    machine_id: &MachineId,
    container_id: &ContainerId,
    command: [&str; N],
    detach: bool,
) -> Result<Vec<ExecResponseFrame>, Status> {
    let config = exec_config(container_id, command, detach);
    let mut request = Request::new(tokio_stream::iter([config]));
    request
        .metadata_mut()
        .insert("machine", machine_id.as_str().parse().unwrap());
    let mut stream = client.exec(request).await?.into_inner();
    let mut frames = Vec::new();
    while let Some(frame) = stream.message().await? {
        frames.push(ExecResponseFrame::decode(&frame).unwrap());
    }
    Ok(frames)
}

fn exec_config<const N: usize>(
    container_id: &ContainerId,
    command: [&str; N],
    detach: bool,
) -> OpaquePayload {
    ExecRequestFrame::Config(ExecConfig {
        container_id: *container_id,
        options: ExecOptions {
            command: command.into_iter().map(ToOwned::to_owned).collect(),
            attach_stdin: !detach,
            attach_stdout: !detach,
            attach_stderr: !detach,
            tty: false,
            detach,
        },
    })
    .encode()
    .unwrap()
}
