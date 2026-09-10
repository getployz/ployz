use super::support::DiscoveryService;
use ployz::{connect::SystemConnector, context::Connection, sdk};
use ployz_core::{MachineId, MachineRpcServer, RpcError};
use std::{
    io,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{io::AsyncReadExt, time::timeout};
use tonic::codec::CompressionEncoding;

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

pub(super) struct UnixSession {
    pub(super) directory: String,
    _temp: tempfile::TempDir,
}

pub(super) async fn connect(directory: &str, machine_id: &str) -> Result<sdk::Session, RpcError> {
    sdk::connect_connections(
        vec![Connection::unix(format!("{directory}/{machine_id}.sock")).unwrap()],
        Arc::new(SystemConnector::default()),
    )
    .await
}

impl UnixSession {
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
                .env("PLOYZ_SOCKET_DIRECTORY", &self.directory)
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
        let temp = tempfile::tempdir().unwrap();
        Self {
            directory: temp.path().to_str().unwrap().to_owned(),
            _temp: temp,
        }
    }

    pub(super) async fn spawn_machine(
        &self,
        machine_id: MachineId,
        service: DiscoveryService,
    ) -> FakeMachine {
        let listener =
            tokio::net::UnixListener::bind(format!("{}/{machine_id}.sock", self.directory))
                .unwrap();
        let sockets = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::clone(&sockets);
        let accept = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let socket = stream.into_std().unwrap();
                accepted.lock().unwrap().push(socket.try_clone().unwrap());
                let stream = tokio::net::UnixStream::from_std(socket).unwrap();
                let service = service.clone();
                connections.spawn(async move {
                    let _ = tonic::transport::Server::builder()
                        .add_service(
                            MachineRpcServer::new(service)
                                .send_compressed(CompressionEncoding::Gzip),
                        )
                        .serve_with_incoming(tokio_stream::once(Ok::<_, io::Error>(stream)))
                        .await;
                });
            }
        });
        FakeMachine { accept, sockets }
    }
}

pub(super) struct FakeMachine {
    accept: tokio::task::JoinHandle<()>,
    sockets: Arc<Mutex<Vec<std::os::unix::net::UnixStream>>>,
}
impl FakeMachine {
    pub(super) fn disconnect(&self) {
        self.accept.abort();
        // Tonic owns each accepted connection after the server future is dropped.
        for socket in self.sockets.lock().unwrap().drain(..) {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
    }
}
impl Drop for FakeMachine {
    fn drop(&mut self) {
        self.disconnect();
    }
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
