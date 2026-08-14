use std::{
    ffi::OsStr,
    fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use oci_spec::distribution::Reference;
use ployz_core::{
    Machine, MachineFailure, MachineSelector, MachineSuccess, PartialResult, UNREGISTRY_PORT,
    resolve_machine_selectors,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite, copy_bidirectional},
    net::{TcpListener, UnixListener},
    process::Command,
};

use crate::connect::Client;

const HELPER_IMAGE: &str = "alpine/socat:1.8.0.3";
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProxyMode {
    Native,
    Vm,
    Rootless,
}

#[derive(Debug, Error)]
pub enum PushError {
    #[error("invalid image reference '{reference}': {message}")]
    InvalidReference { reference: String, message: String },
    #[error("direct image push requires a tagged local reference")]
    DigestReference,
    #[error("unsupported platform '{0}'")]
    UnsupportedPlatform(String),
    #[error("image '{0}' not found locally")]
    ImageNotFound(String),
    #[error("Machine target selection failed: {0}")]
    TargetSelection(String),
    #[error("Cluster operation failed: {0}")]
    Cluster(String),
    #[error("Docker {action}: {diagnostic}")]
    Docker {
        action: &'static str,
        diagnostic: String,
    },
    #[error("image proxy {action}: {diagnostic}")]
    Proxy {
        action: &'static str,
        diagnostic: String,
    },
    #[error("Docker is not using the required containerd image store")]
    UnsupportedImageStore,
    #[error("unsupported Machine subnet {0}")]
    UnsupportedSubnet(String),
    #[error("image-push cleanup failed: {0}")]
    Cleanup(String),
    #[error("{primary}; cleanup: {cleanup}")]
    CleanupAfter {
        primary: Box<PushError>,
        cleanup: Box<PushError>,
    },
    #[error("{machine}: {source}")]
    Machine {
        machine: String,
        #[source]
        source: Box<PushError>,
    },
}

pub async fn push(
    client: &mut Client,
    image: &str,
    platform: Option<&str>,
    selectors: &[String],
) -> Result<PartialResult<(), PushError>, PushError> {
    // TODO(UT-022): without an explicit platform, Docker chooses what to push; target platforms are not inferred.
    let platform = platform.map(validated_platform).transpose()?;
    let reference = tagged_reference(image)?;
    let inspected = docker_output(["image", "inspect", image]).await?;
    if !inspected.status.success() {
        return Err(if not_found(&inspected) {
            PushError::ImageNotFound(image.into())
        } else {
            command_error("inspect local image", &inspected)
        });
    }
    let targets = select_targets(
        &client
            .list_machines()
            .await
            .map_err(|error| PushError::Cluster(error.to_string()))?,
        selectors,
    )?;
    let mode = detect_proxy_mode().await?;
    let mut result = PartialResult {
        successes: Vec::new(),
        failures: Vec::new(),
        omissions: Vec::new(),
    };
    for machine in targets {
        match push_to_machine(client, image, platform, &reference, &machine, mode).await {
            Ok(()) => result.successes.push(MachineSuccess {
                machine_id: machine.id,
                value: (),
            }),
            Err(source) => result.failures.push(MachineFailure {
                machine_id: machine.id,
                error: PushError::Machine {
                    machine: machine.name.to_string(),
                    source: Box::new(source),
                },
            }),
        }
    }
    Ok(result)
}

fn select_targets(
    observations: &[ployz_core::MachineObservation],
    selectors: &[String],
) -> Result<Vec<Machine>, PushError> {
    let machines = observations
        .iter()
        .map(|observation| observation.machine.clone())
        .collect::<Vec<_>>();
    let selectors = if selectors.is_empty() {
        vec![MachineSelector::parse("*").expect("wildcard selector is valid")]
    } else {
        selectors
            .iter()
            .map(MachineSelector::parse)
            .collect::<Result<_, _>>()
            .map_err(|error| PushError::TargetSelection(error.to_string()))?
    };
    resolve_machine_selectors(&machines, &selectors)
        .map_err(|error| PushError::TargetSelection(error.to_string()))
}

async fn push_to_machine(
    client: &Client,
    image: &str,
    platform: Option<&str>,
    reference: &(String, String),
    machine: &Machine,
    mode: ProxyMode,
) -> Result<(), PushError> {
    let store = client
        .list_images(
            Some("ployz-store-probe.invalid/never:match".into()),
            &[machine.id.to_string()],
        )
        .await
        .map_err(|error| PushError::Cluster(format!("check image store: {error}")))?;
    let store = store.successes.first().ok_or_else(|| {
        PushError::Cluster(
            store
                .failures
                .first()
                .map(|failure| failure.error.message.clone())
                .unwrap_or_else(|| "target returned no image-store result".into()),
        )
    })?;
    if !store.value.images.containerd_store {
        return Err(PushError::UnsupportedImageStore);
    }
    let network = machine.subnet.0;
    if network.prefix_len() != 24 {
        return Err(PushError::UnsupportedSubnet(network.to_string()));
    }
    let gateway = Ipv4Addr::from(u32::from(network.network()) + 1);
    let remote = format!("{gateway}:{UNREGISTRY_PORT}");
    client
        .dial_proxy("tcp", &remote)
        .await
        .map_err(|error| PushError::Cluster(format!("reach unregistry: {error}")))?;
    PushSession::run(client, remote, mode, image, platform, reference).await
}

struct PushSession {
    proxy: LocalProxy,
    helper: Option<Helper>,
    temporary: Option<String>,
}

impl PushSession {
    async fn run(
        client: &Client,
        remote: String,
        mode: ProxyMode,
        image: &str,
        platform: Option<&str>,
        reference: &(String, String),
    ) -> Result<(), PushError> {
        let mut session = Self {
            proxy: LocalProxy::listen(mode).await?,
            helper: None,
            temporary: None,
        };
        let outcome = session
            .push(client, remote, mode, image, platform, reference)
            .await;
        let cleanup = session.cleanup().await;
        match (outcome, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(primary), Err(cleanup)) => Err(PushError::CleanupAfter {
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }),
        }
    }

    async fn push(
        &mut self,
        client: &Client,
        remote: String,
        mode: ProxyMode,
        image: &str,
        platform: Option<&str>,
        reference: &(String, String),
    ) -> Result<(), PushError> {
        let push_port = match mode {
            ProxyMode::Native => self.proxy.port(),
            ProxyMode::Vm => {
                let destination = format!("TCP-CONNECT:host.docker.internal:{}", self.proxy.port());
                self.helper = Some(start_helper(&destination, None).await?);
                self.helper.as_ref().expect("helper was stored").port
            }
            ProxyMode::Rootless => {
                let socket = self
                    .proxy
                    .socket_path()
                    .expect("rootless proxy has a unix socket");
                let destination = format!("UNIX-CONNECT:{}", socket.display());
                let bind = format!("{}:{}", socket.display(), socket.display());
                self.helper = Some(start_helper(&destination, Some(bind)).await?);
                self.helper.as_ref().expect("helper was stored").port
            }
        };
        let temporary = temporary_reference(push_port, reference);
        let tagged = docker_output(["tag", image, &temporary]).await?;
        if !tagged.status.success() {
            return Err(command_error("tag image for push", &tagged));
        }
        self.temporary = Some(temporary.clone());
        let push = async {
            let mut command = Command::new("docker");
            command.arg("push");
            if let Some(platform) = platform {
                command.args(["--platform", platform]);
            }
            let status = command
                .arg(&temporary)
                .kill_on_drop(true)
                .status()
                .await
                .map_err(|error| PushError::Docker {
                    action: "push",
                    diagnostic: error.to_string(),
                })?;
            status.success().then_some(()).ok_or(PushError::Docker {
                action: "push",
                diagnostic: format!("exited with {status}"),
            })
        };
        // TODO(UT-023): direct push keeps Docker's progress stream; no quiet mode is exposed.
        tokio::select! {
            outcome = push => outcome,
            outcome = self.proxy.serve(client.clone(), remote) => outcome,
        }
    }

    async fn cleanup(&mut self) -> Result<(), PushError> {
        cleanup(
            self.temporary.as_deref(),
            self.helper.as_ref(),
            self.proxy.socket_path(),
        )
        .await
    }
}

fn proxy_mode(virtualized: bool, rootless: bool) -> ProxyMode {
    if virtualized {
        // TODO(UT-024): the virtualized-and-rootless combination uses the VM path.
        ProxyMode::Vm
    } else if rootless {
        ProxyMode::Rootless
    } else {
        ProxyMode::Native
    }
}

async fn detect_proxy_mode() -> Result<ProxyMode, PushError> {
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
    Ok(proxy_mode(virtualized, rootless))
}

enum LocalProxy {
    Tcp(TcpListener),
    Unix {
        listener: UnixListener,
        path: PathBuf,
    },
}

impl LocalProxy {
    async fn listen(mode: ProxyMode) -> Result<Self, PushError> {
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
            Self::Unix { path, .. } => Some(path.as_path()),
        }
    }

    async fn serve(&mut self, client: Client, remote: String) -> Result<(), PushError> {
        loop {
            match self {
                Self::Tcp(listener) => {
                    let (stream, _) =
                        listener.accept().await.map_err(|error| PushError::Proxy {
                            action: "accept TCP connection",
                            diagnostic: error.to_string(),
                        })?;
                    forward(stream, client.clone(), remote.clone());
                }
                Self::Unix { listener, .. } => {
                    let (stream, _) =
                        listener.accept().await.map_err(|error| PushError::Proxy {
                            action: "accept Unix connection",
                            diagnostic: error.to_string(),
                        })?;
                    forward(stream, client.clone(), remote.clone());
                }
            }
        }
    }
}

fn forward(
    stream: impl AsyncRead + AsyncWrite + Send + Unpin + 'static,
    client: Client,
    remote: String,
) {
    tokio::spawn(async move {
        if let Ok(mut target) = client.dial_proxy("tcp", &remote).await {
            let mut stream = stream;
            let _ = copy_bidirectional(&mut stream, &mut target).await;
        }
    });
}

struct Helper {
    id: String,
    port: u16,
}

async fn start_helper(destination: &str, bind: Option<String>) -> Result<Helper, PushError> {
    // TODO(UT-025): the helper image is intentionally fixed rather than configurable.
    let inspected = docker_output(["image", "inspect", HELPER_IMAGE]).await?;
    if !inspected.status.success() {
        if !not_found(&inspected) {
            return Err(command_error("inspect proxy helper image", &inspected));
        }
        let status = Command::new("docker")
            .args(["pull", HELPER_IMAGE])
            .status()
            .await
            .map_err(|error| PushError::Docker {
                action: "pull proxy helper image",
                diagnostic: error.to_string(),
            })?;
        if !status.success() {
            return Err(PushError::Docker {
                action: "pull proxy helper image",
                diagnostic: format!("exited with {status}"),
            });
        }
    }
    let name = format!(
        "ployz-push-proxy-{}-{}",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut command = Command::new("docker");
    command.args([
        "run",
        "--detach",
        "--rm",
        "--name",
        &name,
        "--publish",
        "127.0.0.1::5000",
        "--label",
        "ployz.managed",
    ]);
    if let Some(bind) = &bind {
        command.args(["--volume", bind]);
    }
    let created = command
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
        .output()
        .await
        .map_err(|error| PushError::Docker {
            action: "run proxy helper",
            diagnostic: error.to_string(),
        })?;
    if !created.status.success() {
        return Err(command_error("run proxy helper", &created));
    }
    let id = String::from_utf8_lossy(&created.stdout).trim().to_owned();
    let host_port = match helper_port(&id).await {
        Ok(port) => port,
        Err(error) => {
            let _ = remove_helper(&id).await;
            return Err(error);
        }
    };
    if let Err(error) = wait_for_port(host_port).await {
        let _ = remove_helper(&id).await;
        return Err(error);
    }
    Ok(Helper {
        id,
        port: host_port,
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

fn tagged_reference(image: &str) -> Result<(String, String), PushError> {
    let reference = image
        .parse::<Reference>()
        .map_err(|error| PushError::InvalidReference {
            reference: image.into(),
            message: error.to_string(),
        })?;
    if reference.digest().is_some() {
        return Err(PushError::DigestReference);
    }
    let tag = reference.tag().expect("parsed references have a tag");
    let suffix = format!(":{tag}");
    Ok((
        image.strip_suffix(&suffix).unwrap_or(image).to_owned(),
        tag.to_owned(),
    ))
}

fn temporary_reference(port: u16, (name, tag): &(String, String)) -> String {
    format!("127.0.0.1:{port}/{name}:{tag}")
}

fn validated_platform(platform: &str) -> Result<&str, PushError> {
    match platform {
        "linux/amd64" | "linux/arm64" => Ok(platform),
        _ => Err(PushError::UnsupportedPlatform(platform.into())),
    }
}

async fn cleanup(
    temporary: Option<&str>,
    helper: Option<&Helper>,
    socket: Option<&Path>,
) -> Result<(), PushError> {
    let mut errors = Vec::new();
    if let Some(temporary) = temporary
        && let Err(error) = remove_image(temporary).await
    {
        errors.push(error.to_string());
    }
    if let Some(helper) = helper
        && let Err(error) = remove_helper(&helper.id).await
    {
        errors.push(error.to_string());
    }
    if let Some(socket) = socket
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

async fn remove_image(image: &str) -> Result<(), PushError> {
    let output = docker_output(["image", "rm", image]).await?;
    if output.status.success() || not_found(&output) {
        Ok(())
    } else {
        Err(command_error("remove temporary image", &output))
    }
}

async fn remove_helper(id: &str) -> Result<(), PushError> {
    let output = docker_output(["rm", "--force", id]).await?;
    if output.status.success() || not_found(&output) {
        Ok(())
    } else {
        Err(command_error("remove proxy helper", &output))
    }
}

async fn docker_output<I, S>(args: I) -> Result<Output, PushError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| PushError::Docker {
            action: "run command",
            diagnostic: error.to_string(),
        })
}

fn command_error(action: &'static str, output: &Output) -> PushError {
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    PushError::Docker {
        action,
        diagnostic: diagnostic.trim().into(),
    }
}

fn not_found(output: &Output) -> bool {
    String::from_utf8_lossy(&output.stderr)
        .to_ascii_lowercase()
        .contains("no such")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{
        MachineId, MachineName, MachineObservation, MachineSubnet, ManagementAddress,
        MembershipObservation, WireGuardPublicKey,
    };

    fn machine(seed: u8) -> MachineObservation {
        MachineObservation {
            machine: Machine {
                id: MachineId::parse(format!("{seed:032x}")).unwrap(),
                name: MachineName::parse(format!("machine-{seed}")).unwrap(),
                subnet: MachineSubnet(format!("10.210.{seed}.0/24").parse().unwrap()),
                management_address: ManagementAddress("fd00::1".parse().unwrap()),
                public_key: WireGuardPublicKey([seed; 32]),
                advertised_endpoints: Vec::new(),
            },
            membership: MembershipObservation::Up,
            selected_endpoint: None,
        }
    }

    #[test]
    fn target_and_proxy_selection_preserve_the_explicit_contract() {
        let machines = [machine(1), machine(2)];
        assert_eq!(select_targets(&machines, &[]).unwrap().len(), 2);
        assert_eq!(
            select_targets(&machines, &["machine-2".into()])
                .unwrap()
                .first()
                .unwrap()
                .name
                .as_str(),
            "machine-2"
        );
        assert_eq!(select_targets(&machines, &["all".into()]).unwrap().len(), 2);
        assert!(select_targets(&machines, &["missing".into()]).is_err());
        assert_eq!(proxy_mode(false, false), ProxyMode::Native);
        assert_eq!(proxy_mode(false, true), ProxyMode::Rootless);
        assert_eq!(proxy_mode(true, false), ProxyMode::Vm);
        assert_eq!(proxy_mode(true, true), ProxyMode::Vm);
        let reference = temporary_reference(
            5000,
            &tagged_reference("registry.test/team/api:v1").unwrap(),
        );
        assert_eq!(reference, "127.0.0.1:5000/registry.test/team/api:v1");
        assert!(tagged_reference("registry.test/team/api@sha256:abc").is_err());
        assert!(validated_platform("linux/riscv64").is_err());
    }
}
