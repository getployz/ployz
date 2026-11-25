use ployz_core::labels;
use ployz_core::{ServiceId, ServiceName};
use std::{sync::Arc, time::Duration};

use futures_util::StreamExt;
use ployz_core::{
    ContainerId, ExecConfig, ExecOptions, ExecRequestFrame, ExecResponseFrame, LiveLogTarget,
    LogBody, LogEntry, LogsOptions, MachineName, MachineRpcClient, OpaquePayload, TailLogsRequest,
    op,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::{Request, Status, transport::Server};

use super::*;
use crate::machine_api::MachineApi;

#[tokio::test]
async fn exec_forwards_output_while_docker_inspection_is_pending() {
    let root = TestDir::new("ployzd-docker-observer");
    let docker_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let docker_address = docker_listener.local_addr().unwrap();
    let (release_inspection, inspection_released) = tokio::sync::oneshot::channel();
    let container_id = ContainerId::parse("a".repeat(64)).unwrap();
    let spec = crate::docker::test_support::spec_with_sources(vec![]);
    let managed = serde_json::json!({
        "Id": container_id.as_str(),
        "Config": {"Labels": {
            labels::MANAGED: "",
            labels::NAMESPACE: "app",
            labels::SERVICE_ID: spec.service_id.to_string(),
            labels::SERVICE_NAME: spec.name.to_string(),
        }},
    })
    .to_string();
    let docker_server = tokio::spawn(async move {
        let mut inspect_container = accept_docker_request(&docker_listener).await;
        inspect_container
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{managed}",
                    managed.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        drop(inspect_container);
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
    specs
        .config_operation()
        .await
        .put(&container_id, None, &spec, None)
        .await
        .unwrap();
    let runtime = ContainerRuntime::new(LocalDocker::from_client(docker), specs);
    let owner = crate::machine::RecordOwner::spawn(machine_store).unwrap();
    let api = MachineApi::builder(owner.clone())
        .with_local(crate::machine::LocalMachine::new(owner).with_containers(Some(runtime)))
        .build();
    let socket_dir = tempfile::tempdir().unwrap();
    let (mut client, server) = crate::machine_api::test_socket::served(
        api,
        &crate::machine_api::test_socket::socket_in(socket_dir.path()),
    )
    .await;
    let config = exec_config(&container_id, ["sh"], false);
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

#[tokio::test]
async fn an_exec_whose_client_goes_away_stops_its_command() {
    use std::os::unix::process::ExitStatusExt as _;
    let status = abandoned_exec(false).await;
    assert_eq!(
        status.signal(),
        Some(rustix::process::Signal::TERM.as_raw())
    );
}

#[tokio::test]
async fn an_exec_still_writing_when_its_client_goes_away_stops_its_command() {
    use std::os::unix::process::ExitStatusExt as _;
    let status = abandoned_exec(true).await;
    assert_eq!(
        status.signal(),
        Some(rustix::process::Signal::TERM.as_raw())
    );
}

/// Run an exec whose command is a local `sleep`, writing output the client never
/// reads when `noisy`, then drop the client; how the command ended.
async fn abandoned_exec(noisy: bool) -> std::process::ExitStatus {
    let root = TestDir::new("ployzd-exec-abandoned");
    let command = ExecCommand::spawn();
    let start = if noisy {
        ExecStart::Noisy
    } else {
        ExecStart::Attached
    };
    let (runtime, container_id, docker_server) = exec_runtime(&root, &command, start).await;
    let machine_store = crate::machine::LocalMachineStore::open(root.0.join("machine")).unwrap();
    let owner = crate::machine::RecordOwner::spawn(machine_store).unwrap();
    let api = MachineApi::builder(owner.clone())
        .with_local(crate::machine::LocalMachine::new(owner).with_containers(Some(runtime)))
        .build();
    let socket_dir = tempfile::tempdir().unwrap();
    let (mut client, server) = crate::machine_api::test_socket::served(
        api,
        &crate::machine_api::test_socket::socket_in(socket_dir.path()),
    )
    .await;
    let mut output = client
        .exec(Request::new(tokio_stream::iter([exec_config(
            &container_id,
            ["sleep", "300"],
            false,
        )])))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        ExecResponseFrame::decode(&output.message().await.unwrap().unwrap()).unwrap(),
        ExecResponseFrame::ExecId("slow".into())
    );
    // An interrupted `ployz exec -T` goes away without the command's exit.
    drop(output);
    drop(client);

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = command.exited() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            panic!("the exec's command outlived its client");
        }
        // real-wait: the command is a separate process, and its exit signals nothing in-process.
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    docker_server.abort();
    server.abort();
    status
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_devices_exec_has_stopped_when_its_removal_returns() {
    removed_devices_exec(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_devices_detached_exec_has_stopped_when_its_removal_returns() {
    removed_devices_exec(true).await;
}

/// A Device whose key also holds Cloud's slot runs a command in a Service's Container,
/// detached when `detach`, then its Device is removed: the command has stopped by the time
/// `device rm` returns, and the client's stream has ended.
async fn removed_devices_exec(detach: bool) {
    {
        let root = TestDir::new("ployzd-exec-removed-device");
        let command = ExecCommand::spawn();
        let start = if detach {
            ExecStart::Detached
        } else {
            ExecStart::Attached
        };
        let (runtime, container_id, docker_server) = exec_runtime(&root, &command, start).await;
        let mut store = crate::machine::test_support::founder(&root.0.join("machine"), "exec");
        let device = crate::machine::test_support::device_in_two_slots(&mut store);
        let owner = crate::machine::RecordOwner::spawn(store).unwrap();
        let local = crate::machine::LocalMachine::new(owner).with_containers(Some(runtime));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(
            Server::builder()
                .add_routes(crate::machine_api::device_routes(
                    crate::machine_api::MachineService::over(local.clone()),
                    device.into(),
                ))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );
        let mut client = MachineRpcClient::connect(format!("http://{address}"))
            .await
            .unwrap();
        let mut output = client
            .exec(Request::new(tokio_stream::iter([exec_config(
                &container_id,
                ["sleep", "300"],
                detach,
            )])))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            ExecResponseFrame::decode(&output.message().await.unwrap().unwrap()).unwrap(),
            ExecResponseFrame::ExecId("slow".into())
        );

        local
            .clone()
            .on_local_socket()
            .set_device(ployz_core::SetDeviceRequest::Remove {
                code: device.code(),
            })
            .await
            .unwrap();

        assert!(
            command.exited().is_some(),
            "a {} exec's command still runs once its Device's removal returned",
            if detach { "detached" } else { "attached" }
        );
        let rest = tokio::time::timeout(Duration::from_secs(10), async {
            let mut frames = Vec::new();
            while let Ok(Some(frame)) = output.message().await {
                frames.push(ExecResponseFrame::decode(&frame).unwrap());
            }
            frames
        })
        .await
        .expect("the exec's stream ends once its command stopped");
        assert!(
            !rest
                .iter()
                .any(|frame| matches!(frame, ExecResponseFrame::Stdout(_))),
            "{rest:?}"
        );
        docker_server.abort();
        server.abort();
    }
}

/// A Device whose key also holds Cloud's slot starts a detached exec and goes away while
/// Docker starts it. The command Docker started is still the Device's: removing the Device
/// stops it before `device rm` returns.
#[tokio::test(flavor = "multi_thread")]
async fn a_detached_exec_whose_caller_left_mid_start_stops_with_its_device() {
    let root = TestDir::new("ployzd-exec-left-mid-start");
    let command = ExecCommand::spawn();
    let reached = Arc::new(tokio::sync::Notify::new());
    let permit = Arc::new(tokio::sync::Semaphore::new(0));
    let stalls = Stalls {
        start: Some((Arc::clone(&reached), Arc::clone(&permit))),
        ..Stalls::default()
    };
    let (runtime, container_id, docker_server) =
        stalling_exec_runtime(&root, &command, ExecStart::Detached, stalls).await;
    let (local, device, address, server) = device_exec_server(&root, runtime).await;
    let mut client = MachineRpcClient::connect(format!("http://{address}"))
        .await
        .unwrap();
    let call = tokio::spawn(async move {
        client
            .exec(Request::new(tokio_stream::iter([exec_config(
                &container_id,
                ["sleep", "300"],
                true,
            )])))
            .await
    });
    reached.notified().await;
    call.abort();
    let _ = call.await;
    // real-wait: the server drops the call's handler on its own connection task once the
    // client's reset arrives, which signals nothing in-process.
    tokio::time::sleep(Duration::from_millis(500)).await;
    permit.add_permits(1);

    local
        .on_local_socket()
        .set_device(ployz_core::SetDeviceRequest::Remove {
            code: device.code(),
        })
        .await
        .unwrap();

    assert!(
        command.exited().is_some(),
        "a detached exec whose caller left mid-start still runs once its Device's removal returned"
    );
    docker_server.abort();
    server.abort();
}

/// A Device whose key also holds Cloud's slot runs a command, then Docker stops answering
/// about it as the Device is removed. `device rm` returns within its bound and says the exec
/// didn't confirm it stopped, rather than that it did.
#[tokio::test(flavor = "multi_thread")]
async fn a_removal_says_when_docker_cannot_confirm_an_exec_stopped() {
    let root = TestDir::new("ployzd-exec-unconfirmed");
    let command = ExecCommand::spawn();
    let stalls = Stalls::default();
    let (runtime, container_id, docker_server) =
        stalling_exec_runtime(&root, &command, ExecStart::Attached, stalls.clone()).await;
    let (local, device, address, server) = device_exec_server(&root, runtime).await;
    let mut client = MachineRpcClient::connect(format!("http://{address}"))
        .await
        .unwrap();
    let mut output = client
        .exec(Request::new(tokio_stream::iter([exec_config(
            &container_id,
            ["sleep", "300"],
            false,
        )])))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        ExecResponseFrame::decode(&output.message().await.unwrap().unwrap()).unwrap(),
        ExecResponseFrame::ExecId("slow".into())
    );
    stalls
        .inspect
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let removed = tokio::time::timeout(
        crate::machine::STOP_WITHIN,
        local
            .on_local_socket()
            .set_device(ployz_core::SetDeviceRequest::Remove {
                code: device.code(),
            }),
    )
    .await
    .expect("the removal waited past its bound on a stuck exec");

    let Err(crate::machine::LocalMachineError::WorkNotStopped(unstopped)) = removed else {
        panic!("the removal said the exec stopped without knowing: {removed:?}");
    };
    assert_eq!(unstopped.first().0, crate::machine::Work::Exec);
    docker_server.abort();
    server.abort();
}

/// This Machine's API, over `runtime`, served to a Device whose key also holds Cloud's slot
/// as its own connection would reach it: the Machine, the Device and the address it dials.
async fn device_exec_server(
    root: &TestDir,
    runtime: ContainerRuntime,
) -> (
    crate::machine::LocalMachine,
    ployz_core::DeviceKey,
    std::net::SocketAddr,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let mut store = crate::machine::test_support::founder(&root.0.join("machine"), "exec");
    let device = crate::machine::test_support::device_in_two_slots(&mut store);
    let owner = crate::machine::RecordOwner::spawn(store).unwrap();
    let local = crate::machine::LocalMachine::new(owner).with_containers(Some(runtime));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(
        Server::builder()
            .add_routes(crate::machine_api::device_routes(
                crate::machine_api::MachineService::over(local.clone()),
                device.into(),
            ))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    (local, device, address, server)
}

/// The local process an exec's command runs as, killed if a test leaves it running.
struct ExecCommand(std::sync::Arc<std::sync::Mutex<std::process::Child>>);

impl ExecCommand {
    fn spawn() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(
            std::process::Command::new("sleep")
                .arg("300")
                .spawn()
                .unwrap(),
        )))
    }

    fn exited(&self) -> Option<std::process::ExitStatus> {
        self.0.lock().unwrap().try_wait().unwrap()
    }
}

impl Drop for ExecCommand {
    fn drop(&mut self) {
        let _ = self.0.lock().unwrap().kill();
    }
}

/// How the stand-in Docker answers an exec's start.
#[derive(Clone, Copy)]
enum ExecStart {
    Attached,
    /// Attached, writing far more output than the response channel holds, so forwarding
    /// waits on a send.
    Noisy,
    Detached,
}

/// How the stand-in Docker misbehaves, as a test turns it.
#[derive(Clone, Default)]
struct Stalls {
    /// When set, the exec's start reaches the stand-in, which notifies this and answers
    /// only once it holds a permit.
    start: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Semaphore>)>,
    /// While set, an exec inspect is never answered.
    inspect: Arc<std::sync::atomic::AtomicBool>,
}

/// A runtime over a stand-in Docker for one exec, `slow`, in a managed Container whose
/// process is this test's, so `command` shares its cgroup as an exec's process does. Docker
/// reports the exec running with `command`'s PID until it exits.
async fn exec_runtime(
    root: &TestDir,
    command: &ExecCommand,
    start: ExecStart,
) -> (ContainerRuntime, ContainerId, tokio::task::JoinHandle<()>) {
    stalling_exec_runtime(root, command, start, Stalls::default()).await
}

/// [`exec_runtime`] over a stand-in Docker that stalls as `stalls` says.
async fn stalling_exec_runtime(
    root: &TestDir,
    command: &ExecCommand,
    start: ExecStart,
    stalls: Stalls,
) -> (ContainerRuntime, ContainerId, tokio::task::JoinHandle<()>) {
    let docker_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let docker_address = docker_listener.local_addr().unwrap();
    let container_id = ContainerId::parse("b".repeat(64)).unwrap();
    let spec = crate::docker::test_support::spec_with_sources(vec![]);
    let managed = serde_json::json!({
        "Id": container_id.as_str(),
        "State": {"Pid": std::process::id()},
        "Config": {"Labels": {
            labels::MANAGED: "",
            labels::NAMESPACE: "app",
            labels::SERVICE_ID: spec.service_id.to_string(),
            labels::SERVICE_NAME: spec.name.to_string(),
        }},
    })
    .to_string();
    let pid = command.0.lock().unwrap().id();
    let docker_command = std::sync::Arc::clone(&command.0);
    let docker_server = tokio::spawn(async move {
        let mut hijacked = Vec::new();
        loop {
            let (mut stream, path) = accept_docker_request_to(&docker_listener).await;
            let body = if path.starts_with("/containers/") && path.ends_with("/json") {
                managed.clone()
            } else if path.ends_with("/exec") {
                r#"{"Id":"slow"}"#.to_owned()
            } else if path.starts_with("/exec/slow/start") {
                if let (ExecStart::Detached, Some((reached, permit))) = (start, &stalls.start) {
                    reached.notify_one();
                    let permit = Arc::clone(permit);
                    tokio::spawn(async move {
                        let _permit = permit.acquire().await.unwrap();
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                    });
                    continue;
                }
                match start {
                    ExecStart::Detached => String::new(),
                    ExecStart::Attached | ExecStart::Noisy => {
                        stream
                            .write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n")
                            .await
                            .unwrap();
                        if matches!(start, ExecStart::Noisy) {
                            tokio::spawn(async move {
                                let mut frame = b"\x01\0\0\0\0\0\x04\0".to_vec();
                                frame.extend([b'x'; 1024]);
                                for _ in 0..4096 {
                                    if stream.write_all(&frame).await.is_err() {
                                        return;
                                    }
                                }
                                std::future::pending::<()>().await;
                            });
                        } else {
                            hijacked.push(stream);
                        }
                        continue;
                    }
                }
            } else {
                if stalls.inspect.load(std::sync::atomic::Ordering::SeqCst) {
                    hijacked.push(stream);
                    continue;
                }
                let exited = docker_command.lock().unwrap().try_wait().unwrap();
                match exited {
                    None => format!(
                        r#"{{"Running":true,"Pid":{pid},"ContainerID":"{}"}}"#,
                        "b".repeat(64)
                    ),
                    Some(_) => r#"{"Running":false,"ExitCode":143}"#.to_owned(),
                }
            };
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let docker = bollard::Docker::connect_with_http(
        &format!("http://{docker_address}"),
        5,
        bollard::API_DEFAULT_VERSION,
    )
    .unwrap();
    let specs = MachineSpecStore::open(root.0.join("machine.db"))
        .await
        .unwrap();
    specs
        .config_operation()
        .await
        .put(&container_id, None, &spec, None)
        .await
        .unwrap();
    (
        ContainerRuntime::new(LocalDocker::from_client(docker), specs),
        container_id,
        docker_server,
    )
}

/// Accept one request to the fake Docker, returning its stream and path.
async fn accept_docker_request_to(
    listener: &tokio::net::TcpListener,
) -> (tokio::net::TcpStream, String) {
    let (stream, _) = listener.accept().await.unwrap();
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    reader.read_line(&mut request).await.unwrap();
    let path = request.split_whitespace().nth(1).unwrap_or_default();
    let path = match path.strip_prefix("/v") {
        Some(versioned) => versioned.find('/').map_or(path, |at| &versioned[at..]),
        None => path,
    };
    let path = path.split('?').next().unwrap_or_default().to_owned();
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
    (reader.into_inner(), path)
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
    let root = TestDir::new("ployzd-docker-observer");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut machine_store = crate::machine::LocalMachineStore::open(&root.0).unwrap();
    machine_store
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Founder {
                initial_policy: Default::default(),
                name: MachineName::parse("stream-test").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(address)],
                grant: None,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    let machine = machine_store.record().machine().unwrap().clone();
    let machine_store = crate::machine::RecordOwner::spawn(machine_store).unwrap();
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
        .lifecycle()
        .create_for_test(
            &machine.id,
            TEST_GATEWAY,
            ContainerKind::ServiceContainer,
            &Namespace::parse("app").unwrap(),
            &spec,
        )
        .await
        .unwrap();
    runtime
        .lifecycle()
        .start(&created.container_id)
        .await
        .unwrap();

    let api = MachineApi::builder(machine_store.clone())
        .with_local(
            crate::machine::LocalMachine::new(machine_store).with_containers(Some(runtime.clone())),
        )
        .build();
    let server =
        tokio::spawn(Server::builder().serve_with_incoming(api, TcpListenerStream::new(listener)));
    let mut client = MachineRpcClient::connect(format!("http://{address}"))
        .await
        .unwrap();

    let frames = exec_frames(
        &mut client,
        &created.container_id,
        ["sh", "-c", "printf out; printf err >&2; exit 42"],
        false,
    )
    .await
    .unwrap();
    assert!(frames.contains(&ExecResponseFrame::Stdout(b"out".to_vec())));
    assert!(frames.contains(&ExecResponseFrame::Stderr(b"err".to_vec())));
    assert!(matches!(frames.last(), Some(ExecResponseFrame::Exit(42))));

    let channel = tonic::transport::Endpoint::from_shared(format!("http://{address}"))
        .unwrap()
        .initial_stream_window_size(1024)
        .initial_connection_window_size(1024)
        .connect()
        .await
        .unwrap();
    let mut slow_client = MachineRpcClient::new(channel);
    let config = exec_config(
        &created.container_id,
        ["sh", "-c", "head -c 8388608 /dev/zero"],
        false,
    );
    let mut output = slow_client
        .exec(Request::new(tokio_stream::iter([config])))
        .await
        .unwrap()
        .into_inner();
    let mut bytes = 0;
    let mut exit = None;
    while let Some(frame) = output.message().await.unwrap() {
        match ExecResponseFrame::decode(&frame).unwrap() {
            ExecResponseFrame::ExecId(_) => {}
            ExecResponseFrame::Stdout(output) => {
                assert!(output.iter().all(|byte| *byte == 0));
                bytes += output.len();
                // real-wait: a slow reader makes real Docker exec output back up behind flow control.
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            ExecResponseFrame::Exit(code) => exit = Some(code),
            other @ (ExecResponseFrame::Stderr(_) | ExecResponseFrame::Error(_)) => {
                panic!("unexpected exec frame: {other:?}")
            }
        }
    }
    assert_eq!(bytes, 8_388_608);
    assert_eq!(exit, Some(0));

    let open_config = exec_config(
        &created.container_id,
        ["sh", "-c", "sleep 30 & exit 7"],
        false,
    );
    let (request_sender, request_receiver) = tokio::sync::mpsc::channel(1);
    request_sender.send(open_config).await.unwrap();
    let request = Request::new(ReceiverStream::new(request_receiver));
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
    let request = Request::new(tokio_stream::iter([empty]));
    assert_eq!(
        client.exec(request).await.unwrap_err().code(),
        tonic::Code::InvalidArgument
    );

    let logs = op::TailLogs::into_request(TailLogsRequest {
        target: LiveLogTarget::Container(created.container_id),
        options: LogsOptions {
            follow: false,
            tail: -1,
            since_nanos: None,
            until_unix_seconds: None,
        },
    })
    .encode()
    .unwrap();
    let request = Request::new(logs);
    let entries = client
        .tail_logs(request)
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
        .lifecycle()
        .remove(&created.container_id, true, true)
        .await
        .unwrap();
    server.abort();
    cleanup_ployz_network(&docker.client, created_network).await;
}

async fn exec_frames<const N: usize>(
    client: &mut MachineRpcClient<tonic::transport::Channel>,
    container_id: &ContainerId,
    command: [&str; N],
    detach: bool,
) -> Result<Vec<ExecResponseFrame>, Status> {
    let config = exec_config(container_id, command, detach);
    let request = Request::new(tokio_stream::iter([config]));
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
