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

use crate::connect::{BoxProxyStream, Client};

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
        cancellation: &mut Cancellation,
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
                if let Ok(mut target) = client.dial_proxy("tcp", &remote).await {
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
