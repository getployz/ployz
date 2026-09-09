use std::{
    fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, UnixListener},
    process::{Child, Command},
    task::JoinSet,
};

use crate::connect::{BoxProxyStream, Client, ConnectError, UNARY_RETRY_DELAYS};

use super::{Cancellation, PushError, command_error, docker_output, not_found, stop_command};

const HELPER_IMAGE: &str = "alpine/socat:1.8.0.3";
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProxyMode {
    Native,
    Vm,
    Rootless,
}

pub(super) fn mode_for(virtualized: bool, rootless: bool) -> ProxyMode {
    if virtualized {
        // TODO: the virtualized-and-rootless combination uses the VM path.
        ProxyMode::Vm
    } else if rootless {
        ProxyMode::Rootless
    } else {
        ProxyMode::Native
    }
}

pub(super) async fn detect_mode() -> Result<ProxyMode, PushError> {
    let output = docker_output([
        "info",
        "--format",
        "{{.Name}}\n{{range .SecurityOptions}}{{println .}}{{end}}",
    ])
    .await?;
    if !output.status.success() {
        return Err(command_error("get Docker info", &output));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let name = lines.next().unwrap_or_default().to_ascii_lowercase();
    let rootless = lines.any(|option| option.contains("rootless"));
    let virtualized = if cfg!(target_os = "macos") {
        name != "orbstack"
    } else {
        ["docker-desktop", "rancher-desktop", "colima"]
            .iter()
            .any(|known| name.contains(known))
    };
    Ok(mode_for(virtualized, rootless))
}

pub(super) struct ImageProxy {
    listener: Listener,
    helper: Option<String>,
    command: Option<Child>,
    connections: JoinSet<()>,
    push_port: u16,
}

impl ImageProxy {
    pub(super) async fn open(
        mode: ProxyMode,
        cancellation: &mut Cancellation<'_>,
    ) -> Result<Self, PushError> {
        let mut proxy = Self {
            listener: Listener::bind(mode).await?,
            helper: None,
            command: None,
            connections: JoinSet::new(),
            push_port: 0,
        };
        let setup = cancellation.race(proxy.setup(mode)).await.flatten();
        match setup {
            Ok(()) => Ok(proxy),
            Err(primary) => match proxy.cleanup().await {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(PushError::CleanupAfter {
                    primary: Box::new(primary),
                    cleanup: Box::new(cleanup),
                }),
            },
        }
    }

    async fn setup(&mut self, mode: ProxyMode) -> Result<(), PushError> {
        self.push_port = match mode {
            ProxyMode::Native => self.listener.port(),
            ProxyMode::Vm => {
                let destination =
                    format!("TCP-CONNECT:host.docker.internal:{}", self.listener.port());
                self.start_helper(&destination, None).await?
            }
            ProxyMode::Rootless => {
                let socket = self
                    .listener
                    .socket_path()
                    .expect("rootless proxy has a unix socket");
                let destination = format!("UNIX-CONNECT:{}", socket.display());
                let bind = format!("{}:{}", socket.display(), socket.display());
                self.start_helper(&destination, Some(bind)).await?
            }
        };
        Ok(())
    }

    pub(super) fn push_port(&self) -> u16 {
        self.push_port
    }

    async fn helper_port(&self) -> Result<u16, PushError> {
        let id = self.helper.as_ref().expect("helper was stored");
        let port = helper_port(id).await?;
        wait_for_port(port).await?;
        Ok(port)
    }

    async fn start_helper(
        &mut self,
        destination: &str,
        bind: Option<String>,
    ) -> Result<u16, PushError> {
        let name = format!(
            "ployz-push-proxy-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        );
        self.helper = Some(name);
        self.command = Some(start_helper(
            self.helper.as_ref().expect("helper was stored"),
            destination,
            bind,
        )?);
        let status = self
            .command
            .as_mut()
            .expect("helper command was stored")
            .wait()
            .await
            .map_err(|error| PushError::Docker {
                action: "run proxy helper",
                diagnostic: error.to_string(),
            })?;
        if !status.success() {
            return Err(PushError::Docker {
                action: "run proxy helper",
                diagnostic: format!("exited with {status}"),
            });
        }
        self.helper_port().await
    }

    pub(super) async fn serve(&mut self, client: Client, remote: String) -> Result<(), PushError> {
        loop {
            let stream = self.listener.accept().await?;
            let client = client.clone();
            let remote = remote.clone();
            self.connections.spawn(async move {
                if let Ok(mut target) = dial_with_retry(&client, &remote).await {
                    let mut stream = stream;
                    let _ = copy_bidirectional(&mut stream, &mut target).await;
                }
            });
        }
    }

    pub(super) async fn cleanup(&mut self) -> Result<(), PushError> {
        let mut errors = Vec::new();
        if let Some(command) = &mut self.command
            && let Err(error) = stop_command(command).await
        {
            errors.push(error.to_string());
        }
        self.connections.shutdown().await;
        if let Some(helper) = &self.helper
            && let Err(error) = remove_helper(helper).await
        {
            errors.push(error.to_string());
        }
        if let Some(socket) = self.listener.socket_path()
            && let Err(error) = fs::remove_file(socket)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            errors.push(error.to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(PushError::Cleanup(errors.join("; ")))
        }
    }
}

enum Listener {
    Tcp(TcpListener),
    Unix {
        listener: UnixListener,
        path: PathBuf,
    },
}

impl Listener {
    async fn bind(mode: ProxyMode) -> Result<Self, PushError> {
        match mode {
            ProxyMode::Native | ProxyMode::Vm => TcpListener::bind("127.0.0.1:0")
                .await
                .map(Self::Tcp)
                .map_err(|error| PushError::Proxy {
                    action: "listen",
                    diagnostic: error.to_string(),
                }),
            ProxyMode::Rootless => {
                let path = std::env::temp_dir().join(format!(
                    "ployz-push-{}-{}.sock",
                    std::process::id(),
                    TEMP_ID.fetch_add(1, Ordering::Relaxed)
                ));
                let listener = UnixListener::bind(&path).map_err(|error| PushError::Proxy {
                    action: "listen",
                    diagnostic: format!("{}: {error}", path.display()),
                })?;
                let permissions = (|| {
                    use std::os::unix::fs::PermissionsExt;
                    let mut permissions = fs::metadata(&path)?.permissions();
                    permissions.set_mode(0o600);
                    fs::set_permissions(&path, permissions)
                })();
                if let Err(error) = permissions {
                    let _ = fs::remove_file(&path);
                    return Err(PushError::Proxy {
                        action: "secure rootless socket",
                        diagnostic: error.to_string(),
                    });
                }
                Ok(Self::Unix { listener, path })
            }
        }
    }

    fn port(&self) -> u16 {
        match self {
            Self::Tcp(listener) => listener
                .local_addr()
                .expect("bound TCP listener has an address")
                .port(),
            Self::Unix { .. } => 0,
        }
    }

    fn socket_path(&self) -> Option<&Path> {
        match self {
            Self::Tcp(_) => None,
            Self::Unix { path, .. } => Some(path),
        }
    }

    async fn accept(&self) -> Result<BoxProxyStream, PushError> {
        match self {
            Self::Tcp(listener) => listener
                .accept()
                .await
                .map(|(stream, _)| Box::new(stream) as BoxProxyStream)
                .map_err(|error| PushError::Proxy {
                    action: "accept TCP connection",
                    diagnostic: error.to_string(),
                }),
            Self::Unix { listener, .. } => listener
                .accept()
                .await
                .map(|(stream, _)| Box::new(stream) as BoxProxyStream)
                .map_err(|error| PushError::Proxy {
                    action: "accept Unix connection",
                    diagnostic: error.to_string(),
                }),
        }
    }
}

/// Retry a dropped Machine dial while Docker's accepted connection stays open.
pub(super) async fn dial_with_retry(
    client: &Client,
    remote: &str,
) -> Result<BoxProxyStream, ConnectError> {
    let mut delays = UNARY_RETRY_DELAYS.iter().copied();
    loop {
        match client.dial_proxy("tcp", remote).await {
            Ok(stream) => return Ok(stream),
            Err(error) if error.is_retryable() => {
                let Some(delay) = delays.next() else {
                    return Err(error);
                };
                tokio::time::sleep(delay).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn start_helper(name: &str, destination: &str, bind: Option<String>) -> Result<Child, PushError> {
    // TODO: the helper image is intentionally fixed rather than configurable.
    let mut command = Command::new("docker");
    command.args([
        "run",
        "--detach",
        "--rm",
        "--name",
        name,
        "--publish",
        "127.0.0.1::5000",
        "--label",
        "ployz.managed",
    ]);
    if let Some(bind) = &bind {
        command.args(["--volume", bind]);
    }
    command
        .args([
            "--entrypoint",
            "",
            HELPER_IMAGE,
            "timeout",
            "1800",
            "socat",
            "TCP-LISTEN:5000,fork,reuseaddr",
            destination,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| PushError::Docker {
            action: "run proxy helper",
            diagnostic: error.to_string(),
        })
}

async fn helper_port(id: &str) -> Result<u16, PushError> {
    let output = docker_output(["port", id, "5000/tcp"]).await?;
    if !output.status.success() {
        return Err(command_error("inspect proxy helper port", &output));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .and_then(|address| address.rsplit_once(':'))
        .and_then(|(_, port)| port.parse().ok())
        .ok_or_else(|| PushError::Docker {
            action: "inspect proxy helper port",
            diagnostic: "Docker returned no host port".into(),
        })
}

async fn wait_for_port(port: u16) -> Result<(), PushError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .is_ok()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(PushError::Proxy {
        action: "wait for helper",
        diagnostic: format!("port 127.0.0.1:{port} did not become ready"),
    })
}

async fn remove_helper(id: &str) -> Result<(), PushError> {
    let output = docker_output(["rm", "--force", id]).await?;
    if output.status.success() || not_found(&output) {
        Ok(())
    } else {
        Err(command_error("remove proxy helper", &output))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };
    use tonic::transport::{Channel, Endpoint};

    use super::*;
    use crate::{
        cluster::Client,
        connect::{BoxProxyStream, ConnectError, Connector, UNARY_RETRY_DELAYS},
        context::{Connection, ConnectionSource},
    };

    struct RecoveringDial {
        failures: AtomicUsize,
        target: String,
    }

    struct FailingDial {
        attempts: Arc<AtomicUsize>,
        error: fn() -> ConnectError,
    }

    fn unused_connect() -> Result<Channel, ConnectError> {
        Err(ConnectError::Attempt("unused".into()))
    }

    fn network_reset() -> ConnectError {
        ConnectError::Attempt("network reset".into())
    }

    fn unsupported_proxy() -> ConnectError {
        ConnectError::ProxyUnsupported("tcp".into())
    }

    fn client(connector: impl Connector + 'static) -> Client {
        Client::new(
            Endpoint::from_static("http://127.0.0.1:1").connect_lazy(),
            Connection::unix("/tmp/ployz-unused.sock").unwrap(),
            ConnectionSource::Direct,
            Arc::new(connector),
        )
    }

    #[tonic::async_trait]
    impl Connector for RecoveringDial {
        async fn connect(&self, _connection: &Connection) -> Result<Channel, ConnectError> {
            unused_connect()
        }

        async fn dial_proxy(
            &self,
            _connection: &Connection,
            _network: &str,
            _address: &str,
        ) -> Result<BoxProxyStream, ConnectError> {
            if self.failures.fetch_add(1, Ordering::SeqCst) < 2 {
                return Err(network_reset());
            }
            TcpStream::connect(&self.target)
                .await
                .map(|stream| Box::new(stream) as BoxProxyStream)
                .map_err(ConnectError::from)
        }
    }

    #[tonic::async_trait]
    impl Connector for FailingDial {
        async fn connect(&self, _connection: &Connection) -> Result<Channel, ConnectError> {
            unused_connect()
        }

        async fn dial_proxy(
            &self,
            _connection: &Connection,
            _network: &str,
            _address: &str,
        ) -> Result<BoxProxyStream, ConnectError> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Err((self.error)())
        }
    }

    #[tokio::test]
    async fn an_interrupt_from_the_build_prevents_image_push_work() {
        let mut client = client(FailingDial {
            attempts: Arc::new(AtomicUsize::new(0)),
            error: || ConnectError::from(std::io::Error::other("must not dial")),
        });
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();
        let result = super::super::push(
            &mut client,
            super::super::ImageContent::tagged("api:v1"),
            None,
            &[],
            &cancellation,
        )
        .await;
        assert!(matches!(result, Err(PushError::Cancelled)), "{result:?}");
    }

    #[tokio::test]
    async fn image_proxy_holds_docker_connection_across_a_dropped_machine_dial() {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = echo.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (mut stream, _) = echo.accept().await.unwrap();
            let mut buffer = [0; 5];
            stream.read_exact(&mut buffer).await.unwrap();
            stream.write_all(&buffer).await.unwrap();
        });

        let token = tokio_util::sync::CancellationToken::new();
        let mut cancellation = super::super::Cancellation::new(&token);
        let mut proxy = ImageProxy::open(ProxyMode::Native, &mut cancellation)
            .await
            .unwrap();
        let port = proxy.push_port();
        let client = client(RecoveringDial {
            failures: AtomicUsize::new(0),
            target,
        });
        let remote = "[fd00::1]:5000".into();
        let serving = tokio::spawn(async move { proxy.serve(client, remote).await });

        let mut docker = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        docker.write_all(b"hello").await.unwrap();
        let mut buffer = [0; 5];
        tokio::time::timeout(Duration::from_secs(5), docker.read_exact(&mut buffer))
            .await
            .expect("docker connection stayed open across the dropped machine dial")
            .unwrap();
        assert_eq!(&buffer, b"hello");
        serving.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn dial_with_retry_stops_after_the_bounded_delays() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let client = client(FailingDial {
            attempts: Arc::clone(&attempts),
            error: network_reset,
        });
        let Err(error) = dial_with_retry(&client, "127.0.0.1:1").await else {
            panic!("retryable dials should exhaust");
        };
        assert!(matches!(error, ConnectError::Attempt(_)));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1 + UNARY_RETRY_DELAYS.len()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dial_with_retry_does_not_retry_unsupported_proxy_dials() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let client = client(FailingDial {
            attempts: Arc::clone(&attempts),
            error: unsupported_proxy,
        });
        let Err(error) = dial_with_retry(&client, "127.0.0.1:1").await else {
            panic!("unsupported proxy dials should not retry");
        };
        assert!(matches!(error, ConnectError::ProxyUnsupported(_)));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
