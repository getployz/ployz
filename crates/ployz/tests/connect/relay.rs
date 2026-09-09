use std::{
    io,
    net::Ipv4Addr,
    path::PathBuf,
    pin::Pin,
    process::{Command, Output, Stdio},
    task::{Context, Poll},
    time::Duration,
};

use ployz::connect::{ConnectError, DialCredential, connect_relay};
use ployz_core::{DescribeContractRequest, MachineId, MachineRpcServer, op};
use ployz_relay::{
    ClientError, Open, PairingCredential, RegisterRequest, Relay, RelayClient, RelayWs, TunnelIo,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf},
    time::timeout,
};
use tonic::{codec::CompressionEncoding, transport::server::Connected};

use super::support::{DiscoveryService, test_description};

pub(super) const PAIRING: &str = "pairing-secret";
pub(super) const DIAL: &str = "dial-secret";

#[tokio::test]
async fn client_rpc_round_trip_through_relay_attach() {
    let description = test_description();
    let machine_id = description.machine_id;
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(machine_id, DiscoveryService::new(description.clone()))
        .await;

    let mut client = connect_relay(
        &session.url,
        dial_credential(),
        pairing_credential(),
        machine_id,
    )
    .await
    .unwrap();

    assert_eq!(
        client
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await
            .unwrap(),
        description
    );
}

#[tokio::test]
async fn bad_dial_credential_fails_closed() {
    let machine_id = MachineId::random();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(machine_id, DiscoveryService::new(test_description()))
        .await;
    let bad = DialCredential::parse("wrong-secret").unwrap();

    let result = timeout(
        Duration::from_secs(2),
        connect_relay(&session.url, bad, pairing_credential(), machine_id),
    )
    .await
    .expect("bad Dial Credential must not hang");
    let error = match result {
        Ok(_) => panic!("expected invalid Dial Credential to fail"),
        Err(error) => error,
    };

    assert!(
        matches!(error, ConnectError::InvalidDialCredential),
        "{error:?}"
    );
}

#[tokio::test]
async fn unknown_machine_id_fails_closed() {
    let registered = MachineId::random();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(registered, DiscoveryService::new(test_description()))
        .await;

    let result = timeout(
        Duration::from_secs(2),
        connect_relay(
            &session.url,
            dial_credential(),
            pairing_credential(),
            MachineId::random(),
        ),
    )
    .await
    .expect("unknown Machine ID must not hang");
    let error = match result {
        Ok(_) => panic!("expected unknown Machine ID to fail"),
        Err(error) => error,
    };

    assert!(matches!(error, ConnectError::UnknownMachine), "{error:?}");
}

#[tokio::test]
async fn sdk_script_temporary_files_are_removed_after_exit_and_timeout() {
    for mode in ["success", "failure", "timeout"] {
        let report = std::env::temp_dir().join(format!("sdk-temp-report-{}", uuid::Uuid::new_v4()));
        let mut command = tokio::process::Command::new("node");
        command
            .args([
                "-e",
                r#"
            const fs = require('node:fs');
            const path = require('node:path');
            const dir = fs.mkdtempSync(path.join(require('node:os').tmpdir(), 'ployz-sdk-check-'));
            fs.writeFileSync(path.join(dir, 'ployz-sdk.node'), 'test addon');
            fs.writeFileSync(process.argv[1] + '.tmp', dir);
            fs.renameSync(process.argv[1] + '.tmp', process.argv[1]);
            fs.writeSync(1, 'o'.repeat(128 * 1024));
            fs.writeSync(2, 'e'.repeat(128 * 1024));
            if (process.argv[2] === 'timeout') setInterval(() => {}, 1000);
            else process.exit(process.argv[2] === 'failure' ? 1 : 0);
        "#,
            ])
            .arg(&report)
            .arg(mode);
        let deadline = Duration::from_secs(60);
        let output = sdk_script_output(&mut command, deadline);
        tokio::pin!(output);
        let result = tokio::select! {
            result = &mut output => result,
            () = async {
                while !report.try_exists().unwrap() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }, if mode == "timeout" => {
                // Node has created the fixture. Expire the script deadline only now,
                // then restore real time for process termination and pipe draining.
                tokio::time::pause();
                tokio::time::advance(deadline).await;
                tokio::time::resume();
                output.await
            }
        };
        let dir = std::fs::read_to_string(&report).unwrap_or_else(|error| {
            panic!("{mode}: Node fixture report missing: {error}; subprocess result: {result:?}")
        });
        std::fs::remove_file(report).unwrap();
        let leaked = std::path::Path::new(&dir).exists();
        if leaked {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        assert!(!leaked, "{mode} left SDK files in {dir}");
        match mode {
            "timeout" => assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut),
            _ => {
                let output = result.unwrap();
                assert_eq!(output.status.success(), mode == "success");
                assert_eq!(output.stdout, vec![b'o'; 128 * 1024]);
                assert_eq!(output.stderr, vec![b'e'; 128 * 1024]);
            }
        }
    }
}

async fn sdk_script_output(
    command: &mut tokio::process::Command,
    deadline: Duration,
) -> io::Result<Output> {
    let temp = std::env::temp_dir().join(format!("ployz-sdk-run-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&temp)?;
    let result = async {
        let mut child = command
            .env("TMPDIR", &temp)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (status, read_out, read_err) = tokio::join!(
            async {
                match timeout(deadline, child.wait()).await {
                    Ok(status) => status,
                    Err(_) => {
                        // Reap Node before removing files it could still be creating.
                        child.kill().await?;
                        Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "SDK script timed out",
                        ))
                    }
                }
            },
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err),
        );
        read_out?;
        read_err?;
        Ok(Output {
            status: status?,
            stdout: out,
            stderr: err,
        })
    }
    .await;
    // Cleanup precedes error propagation, including spawn failures and timeouts.
    std::fs::remove_dir_all(temp)?;
    result
}

pub(super) struct RelaySession {
    pub(super) url: String,
    _server: tokio::task::JoinHandle<io::Result<()>>,
}

impl RelaySession {
    pub(super) async fn assert_sdk_script(
        &self,
        script: &str,
        machine_id: MachineId,
        environment: &[(&str, &str)],
    ) {
        let package = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ployz-sdk");
        let output = sdk_script_output(
            tokio::process::Command::new("node")
                .arg(package.join("tests").join(script))
                .env("PLOYZ_SDK_ADDON", native_addon())
                .env("PLOYZ_SDK_PACKAGE", package)
                .env("PLOYZ_RELAY_URL", &self.url)
                .env("PLOYZ_BEARER", DIAL)
                .env("PLOYZ_PAIRING", PAIRING)
                .env("PLOYZ_MACHINE_ID", machine_id.as_str())
                .envs(environment.iter().copied()),
            Duration::from_secs(20),
        )
        .await
        .unwrap_or_else(|error| panic!("{script} could not complete: {error}"));
        assert!(
            output.status.success(),
            "{script} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    pub(super) async fn start() -> Self {
        let relay = Relay::new(DialCredential::parse(DIAL).unwrap());
        let listen = (Ipv4Addr::LOCALHOST, 0).into();
        let (address, server, _) = relay.serve(listen).await.unwrap();
        Self {
            url: format!("http://{address}"),
            _server: server,
        }
    }

    pub(super) async fn spawn_machine(
        &self,
        machine_id: MachineId,
        service: DiscoveryService,
    ) -> FakeMachine {
        FakeMachine::register(&self.url, machine_id, service).await
    }
}

pub(super) struct FakeMachine {
    accept: tokio::task::JoinHandle<()>,
}

impl FakeMachine {
    async fn register(url: &str, machine_id: MachineId, service: DiscoveryService) -> Self {
        let client = RelayClient::new(&ployz_core::RelayEndpoint::parse(url).unwrap())
            .expect("test Relay URL is http");
        let mut register = client.register(PAIRING, &machine_id).await.unwrap();
        let url = url.to_owned();
        let accept = tokio::spawn(async move {
            let mut tunnels = tokio::task::JoinSet::new();
            while let Ok(Some(open)) = register.recv::<Open>().await {
                if let Some(nonce) = open.ping_nonce() {
                    let _ = register.send(&RegisterRequest::pong(nonce)).await;
                    continue;
                }
                let url = url.clone();
                let service = service.clone();
                tunnels.spawn(async move {
                    serve_attach(&url, open, service).await;
                });
            }
        });
        Self { accept }
    }

    pub(super) fn disconnect(&self) {
        self.accept.abort();
    }
}

async fn serve_attach(url: &str, open: Open, service: DiscoveryService) {
    let tunnel = RelayClient::new(&ployz_core::RelayEndpoint::parse(url).unwrap())
        .expect("test Relay URL is http")
        .attach(open.tunnel_id().expect("Open carries a Tunnel ID").as_str())
        .await
        .unwrap();
    let io = Incoming(tunnel.into_io());
    let _ = tonic::transport::Server::builder()
        .add_service(MachineRpcServer::new(service).send_compressed(CompressionEncoding::Gzip))
        .serve_with_incoming(tokio_stream::once(Ok::<_, io::Error>(io)))
        .await;
}

pub(super) async fn register_with_pairing(
    url: &str,
    pairing: &str,
    machine_id: MachineId,
) -> Result<RelayWs, ClientError> {
    RelayClient::new(&ployz_core::RelayEndpoint::parse(url).unwrap())?
        .register(pairing, &machine_id)
        .await
}

struct Incoming(TunnelIo);

impl Connected for Incoming {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {}
}

impl AsyncRead for Incoming {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(context, buffer)
    }
}

impl AsyncWrite for Incoming {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(context)
    }
}

fn pairing_credential() -> PairingCredential {
    PairingCredential::parse(PAIRING).unwrap()
}

fn dial_credential() -> DialCredential {
    DialCredential::parse(DIAL).unwrap()
}

fn native_addon() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.join("../..");
    let target = option_env!("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let names = ["libployz_sdk.so", "libployz_sdk.dylib", "ployz_sdk.dll"];
    for name in names {
        let path = target.join(profile).join(name);
        if path.is_file() {
            return path;
        }
    }
    let status = Command::new("cargo")
        .args(["build", "-p", "ployz-sdk", "--locked"])
        .current_dir(&workspace)
        .status()
        .expect("cargo build -p ployz-sdk");
    assert!(status.success(), "cargo build -p ployz-sdk failed");
    for name in names {
        let path = target.join(profile).join(name);
        if path.is_file() {
            return path;
        }
    }
    panic!(
        "ployz-sdk cdylib was not produced under {}",
        target.join(profile).display()
    );
}
