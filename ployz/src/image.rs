use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use ployz_core::{Machine, MachineFailure, MachineSuccess, PartialResult, UNREGISTRY_PORT};
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DockerEnvironment {
    virtualized: bool,
    rootless: bool,
}

impl DockerEnvironment {
    fn proxy_mode(self) -> ProxyMode {
        if self.virtualized {
            // TODO(UT-024): the virtualized-and-rootless combination uses the VM path.
            ProxyMode::Vm
        } else if self.rootless {
            ProxyMode::Rootless
        } else {
            ProxyMode::Native
        }
    }
}

pub async fn push(
    client: &mut Client,
    image: &str,
    platform: Option<&str>,
    selectors: &[String],
) -> Result<PartialResult<(), String>, String> {
    // TODO(UT-022): without an explicit platform, Docker chooses what to push; target platforms are not inferred.
    let platform = platform.map(validated_platform).transpose()?;
    let reference = tagged_reference(image)?;
    let inspected = docker_output(["image", "inspect", image]).await?;
    if !inspected.status.success() {
        return Err(if not_found(&inspected) {
            format!("image '{image}' not found locally")
        } else {
            command_error("inspect local image", &inspected)
        });
    }
    let targets = select_targets(
        &client
            .list_machines()
            .await
            .map_err(|error| error.to_string())?,
        selectors,
    )?;
    let environment = detect_environment().await?;
    let mut result = PartialResult {
        successes: Vec::new(),
        failures: Vec::new(),
        omissions: Vec::new(),
    };
    for machine in targets {
        match push_to_machine(client, image, platform, reference, &machine, environment).await {
            Ok(()) => result.successes.push(MachineSuccess {
                machine_id: machine.id,
                value: (),
            }),
            Err(error) => result.failures.push(MachineFailure {
                machine_id: machine.id,
                error: format!("{}: {error}", machine.name),
            }),
        }
    }
    Ok(result)
}

fn select_targets(
    observations: &[ployz_core::MachineObservation],
    selectors: &[String],
) -> Result<Vec<Machine>, String> {
    let machines = observations
        .iter()
        .map(|observation| &observation.machine)
        .collect::<Vec<_>>();
    if selectors.is_empty() {
        return Ok(machines.into_iter().cloned().collect());
    }
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for selector in selectors {
        let machine = machines
            .iter()
            .find(|machine| machine.id.as_str() == selector || machine.name.as_str() == selector)
            .ok_or_else(|| format!("Machine selector not found: {selector}"))?;
        if seen.insert(machine.id.clone()) {
            selected.push((*machine).clone());
        }
    }
    Ok(selected)
}

async fn push_to_machine(
    client: &Client,
    image: &str,
    platform: Option<&str>,
    reference: (&str, &str),
    machine: &Machine,
    environment: DockerEnvironment,
) -> Result<(), String> {
    let store = client
        .list_images(
            Some("ployz-store-probe.invalid/never:match".into()),
            &[machine.id.to_string()],
        )
        .await
        .map_err(|error| format!("check image store: {error}"))?;
    let store = store.successes.first().ok_or_else(|| {
        store
            .failures
            .first()
            .map(|failure| failure.error.message.clone())
            .unwrap_or_else(|| "target returned no image-store result".into())
    })?;
    if !store.value.images.containerd_store {
        return Err("Docker is not using the required containerd image store".into());
    }
    let network = machine.subnet.0;
    if network.prefix_len() != 24 {
        return Err(format!("unsupported Machine subnet {network}"));
    }
    let gateway = Ipv4Addr::from(u32::from(network.network()) + 1);
    let remote = format!("{gateway}:{UNREGISTRY_PORT}");
    client
        .dial_proxy("tcp", &remote)
        .await
        .map_err(|error| format!("reach unregistry: {error}"))?;

    let mut proxy = LocalProxy::listen(environment.proxy_mode()).await?;
    let mut helper = None;
    let push_port = match environment.proxy_mode() {
        ProxyMode::Native => proxy.port(),
        ProxyMode::Vm => {
            let destination = format!("TCP-CONNECT:host.docker.internal:{}", proxy.port());
            let started = match start_helper(&destination, None).await {
                Ok(started) => started,
                Err(error) => {
                    remove_socket(proxy.socket_path());
                    return Err(error);
                }
            };
            let port = started.port;
            helper = Some(started);
            port
        }
        ProxyMode::Rootless => {
            let socket = proxy
                .socket_path()
                .expect("rootless proxy has a unix socket");
            let destination = format!("UNIX-CONNECT:{}", socket.display());
            let bind = format!("{}:{}", socket.display(), socket.display());
            let started = match start_helper(&destination, Some(bind)).await {
                Ok(started) => started,
                Err(error) => {
                    remove_socket(proxy.socket_path());
                    return Err(error);
                }
            };
            let port = started.port;
            helper = Some(started);
            port
        }
    };
    let temporary = temporary_reference(push_port, reference);
    let outcome = async {
        let tagged = docker_output(["tag", image, temporary.full.as_str()]).await?;
        if !tagged.status.success() {
            return Err(command_error("tag image for push", &tagged));
        }
        let push = async {
            let mut command = Command::new("docker");
            command.arg("push");
            if let Some(platform) = platform {
                command.args(["--platform", platform]);
            }
            command
                .arg(&temporary.full)
                .kill_on_drop(true)
                .status()
                .await
                .map_err(|error| format!("run Docker push: {error}"))
                .and_then(|status| {
                    status
                        .success()
                        .then_some(())
                        .ok_or_else(|| format!("Docker push exited with {status}"))
                })
        };
        // TODO(UT-023): direct push keeps Docker's progress stream; no quiet mode is exposed.
        tokio::select! {
            outcome = push => outcome,
            outcome = proxy.serve(client.clone(), remote) => outcome,
        }
    }
    .await;
    let cleanup = cleanup(&temporary.full, helper.as_ref(), proxy.socket_path()).await;
    match (outcome, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(format!("{error}; cleanup: {cleanup}")),
    }
}

async fn detect_environment() -> Result<DockerEnvironment, String> {
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
    Ok(DockerEnvironment {
        virtualized,
        rootless,
    })
}

enum LocalProxy {
    Tcp(TcpListener),
    Unix {
        listener: UnixListener,
        path: PathBuf,
    },
}

impl LocalProxy {
    async fn listen(mode: ProxyMode) -> Result<Self, String> {
        match mode {
            ProxyMode::Native | ProxyMode::Vm => TcpListener::bind("127.0.0.1:0")
                .await
                .map(Self::Tcp)
                .map_err(|error| format!("listen for image proxy: {error}")),
            ProxyMode::Rootless => {
                let path = std::env::temp_dir().join(format!(
                    "ployz-push-{}-{}.sock",
                    std::process::id(),
                    TEMP_ID.fetch_add(1, Ordering::Relaxed)
                ));
                let listener = UnixListener::bind(&path)
                    .map_err(|error| format!("listen on {}: {error}", path.display()))?;
                let mut permissions = fs::metadata(&path)
                    .map_err(|error| error.to_string())?
                    .permissions();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    permissions.set_mode(0o600);
                    fs::set_permissions(&path, permissions).map_err(|error| error.to_string())?;
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

    async fn serve(&mut self, client: Client, remote: String) -> Result<(), String> {
        loop {
            match self {
                Self::Tcp(listener) => {
                    let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
                    forward(stream, client.clone(), remote.clone());
                }
                Self::Unix { listener, .. } => {
                    let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
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

async fn start_helper(destination: &str, bind: Option<String>) -> Result<Helper, String> {
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
            .map_err(|error| format!("pull {HELPER_IMAGE}: {error}"))?;
        if !status.success() {
            return Err(format!("pull {HELPER_IMAGE} exited with {status}"));
        }
    }
    let host_port = reserve_port()?;
    let name = format!(
        "ployz-push-proxy-{}-{}",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    );
    let published = format!("127.0.0.1:{host_port}:5000");
    let mut command = Command::new("docker");
    command.args([
        "run",
        "--detach",
        "--rm",
        "--name",
        &name,
        "--publish",
        &published,
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
        .map_err(|error| format!("run proxy helper: {error}"))?;
    if !created.status.success() {
        return Err(command_error("run proxy helper", &created));
    }
    let id = String::from_utf8_lossy(&created.stdout).trim().to_owned();
    if let Err(error) = wait_for_port(host_port).await {
        let _ = remove_helper(&id).await;
        return Err(format!("proxy helper {id}: {error}"));
    }
    Ok(Helper {
        id,
        port: host_port,
    })
}

fn reserve_port() -> Result<u16, String> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|error| format!("reserve proxy port: {error}"))
}

async fn wait_for_port(port: u16) -> Result<(), String> {
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
    Err(format!("port 127.0.0.1:{port} did not become ready"))
}

struct TemporaryReference {
    full: String,
}

fn tagged_reference(image: &str) -> Result<(&str, &str), String> {
    if image.contains('@') {
        return Err("direct image push requires a tagged local reference".into());
    }
    let component = image.rsplit('/').next().unwrap_or(image);
    let (name, tag) = if let Some(offset) = component.rfind(':') {
        let split = image.len() - component.len() + offset;
        (&image[..split], &image[split + 1..])
    } else {
        (image, "latest")
    };
    Ok((name, tag))
}

fn temporary_reference(port: u16, (name, tag): (&str, &str)) -> TemporaryReference {
    TemporaryReference {
        full: format!("127.0.0.1:{port}/{name}:{tag}"),
    }
}

fn validated_platform(platform: &str) -> Result<&str, String> {
    match platform {
        "linux/amd64" | "linux/arm64" => Ok(platform),
        _ => Err(format!("unsupported platform '{platform}'")),
    }
}

async fn cleanup(
    temporary: &str,
    helper: Option<&Helper>,
    socket: Option<&Path>,
) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Err(error) = remove_image(temporary).await {
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
        Err(errors.join("; "))
    }
}

async fn remove_image(image: &str) -> Result<(), String> {
    let output = docker_output(["image", "rm", image]).await?;
    if output.status.success() || not_found(&output) {
        Ok(())
    } else {
        Err(command_error("remove temporary image", &output))
    }
}

async fn remove_helper(id: &str) -> Result<(), String> {
    let output = docker_output(["rm", "--force", id]).await?;
    if output.status.success() || not_found(&output) {
        Ok(())
    } else {
        Err(command_error("remove proxy helper", &output))
    }
}

async fn docker_output<I, S>(args: I) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| format!("run Docker: {error}"))
}

fn command_error(action: &str, output: &Output) -> String {
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    format!("{action}: {}", diagnostic.trim())
}

fn not_found(output: &Output) -> bool {
    String::from_utf8_lossy(&output.stderr)
        .to_ascii_lowercase()
        .contains("no such")
}

fn remove_socket(socket: Option<&Path>) {
    if let Some(socket) = socket {
        let _ = fs::remove_file(socket);
    }
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
        assert!(select_targets(&machines, &["missing".into()]).is_err());
        assert_eq!(DockerEnvironment::default().proxy_mode(), ProxyMode::Native);
        assert_eq!(
            DockerEnvironment {
                virtualized: true,
                rootless: false
            }
            .proxy_mode(),
            ProxyMode::Vm
        );
        assert_eq!(
            DockerEnvironment {
                virtualized: false,
                rootless: true
            }
            .proxy_mode(),
            ProxyMode::Rootless
        );
        assert_eq!(
            DockerEnvironment {
                virtualized: true,
                rootless: true
            }
            .proxy_mode(),
            ProxyMode::Vm
        );
        let reference =
            temporary_reference(5000, tagged_reference("registry.test/team/api:v1").unwrap());
        assert_eq!(reference.full, "127.0.0.1:5000/registry.test/team/api:v1");
        assert!(tagged_reference("registry.test/team/api@sha256:abc").is_err());
        assert!(validated_platform("linux/riscv64").is_err());
    }
}
