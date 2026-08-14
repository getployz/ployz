use std::{
    collections::{BTreeSet, HashMap},
    fs,
    net::Ipv4Addr,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use bollard::{
    Docker,
    errors::Error as DockerError,
    models::{ContainerCreateBody, HostConfig, PortBinding},
    query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, PushImageOptionsBuilder,
        RemoveContainerOptionsBuilder, RemoveImageOptionsBuilder, TagImageOptionsBuilder,
    },
};
use futures_util::TryStreamExt;
use ployz_core::{Machine, MachineFailure, MachineSuccess, PartialResult};
use tokio::{
    io::{AsyncRead, AsyncWrite, copy_bidirectional},
    net::{TcpListener, UnixListener},
};

use crate::connect::Client;

const HELPER_IMAGE: &str = "alpine/socat:1.8.0.3";
const UNREGISTRY_PORT: u16 = 51500;
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
    let platform = platform.map(platform_json).transpose()?;
    let docker = Docker::connect_with_defaults().map_err(|error| error.to_string())?;
    docker.inspect_image(image).await.map_err(|error| {
        if is_not_found(&error) {
            format!("image '{image}' not found locally")
        } else {
            format!("inspect image '{image}' locally: {error}")
        }
    })?;
    let targets = select_targets(
        &client
            .list_machines()
            .await
            .map_err(|error| error.to_string())?,
        selectors,
    )?;
    let environment = detect_environment(&docker).await?;
    let mut result = PartialResult {
        successes: Vec::new(),
        failures: Vec::new(),
        omissions: Vec::new(),
    };
    for machine in targets {
        match push_to_machine(
            client,
            &docker,
            image,
            platform.as_deref(),
            &machine,
            environment,
        )
        .await
        {
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
    docker: &Docker,
    image: &str,
    platform: Option<&str>,
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
            let started = match start_helper(docker, &destination, None).await {
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
            let started = match start_helper(docker, &destination, Some(bind)).await {
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
    let temporary = temporary_reference(push_port, image)?;
    let outcome = async {
        docker
            .tag_image(
                image,
                Some(
                    TagImageOptionsBuilder::default()
                        .repo(&temporary.repository)
                        .tag(&temporary.tag)
                        .build(),
                ),
            )
            .await
            .map_err(|error| format!("tag image for push: {error}"))?;
        let push = async {
            let mut options = PushImageOptionsBuilder::default().tag(&temporary.tag);
            if let Some(platform) = platform {
                options = options.platform(platform);
            }
            docker
                .push_image(&temporary.repository, Some(options.build()), None)
                .try_collect::<Vec<_>>()
                .await
                .map(|_| ())
                .map_err(|error| format!("push image: {error}"))
        };
        // TODO(UT-023): direct push keeps Docker's progress stream; no quiet mode is exposed.
        tokio::select! {
            outcome = push => outcome,
            outcome = proxy.serve(client.clone(), remote) => outcome,
        }
    }
    .await;
    let cleanup = cleanup(
        docker,
        &temporary.full,
        helper.as_ref(),
        proxy.socket_path(),
    )
    .await;
    match (outcome, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(format!("{error}; cleanup: {cleanup}")),
    }
}

async fn detect_environment(docker: &Docker) -> Result<DockerEnvironment, String> {
    let info = docker
        .info()
        .await
        .map_err(|error| format!("get Docker info: {error}"))?;
    let rootless = info
        .security_options
        .unwrap_or_default()
        .iter()
        .any(|option| option.contains("rootless"));
    let name = info.name.unwrap_or_default().to_ascii_lowercase();
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

    fn socket_path(&self) -> Option<&PathBuf> {
        match self {
            Self::Tcp(_) => None,
            Self::Unix { path, .. } => Some(path),
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

async fn start_helper(
    docker: &Docker,
    destination: &str,
    bind: Option<String>,
) -> Result<Helper, String> {
    // TODO(UT-025): the helper image is intentionally fixed rather than configurable.
    if let Err(error) = docker.inspect_image(HELPER_IMAGE).await {
        if !is_not_found(&error) {
            return Err(error.to_string());
        }
        docker
            .create_image(
                Some(
                    CreateImageOptionsBuilder::default()
                        .from_image(HELPER_IMAGE)
                        .build(),
                ),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await
            .map_err(|error| format!("pull {HELPER_IMAGE}: {error}"))?;
    }
    let host_port = reserve_port()?;
    let port_bindings = HashMap::from([(
        "5000/tcp".into(),
        Some(vec![PortBinding {
            host_ip: Some("127.0.0.1".into()),
            host_port: Some(host_port.to_string()),
        }]),
    )]);
    let config = ContainerCreateBody {
        image: Some(HELPER_IMAGE.into()),
        entrypoint: Some(Vec::new()),
        cmd: Some(vec![
            "timeout".into(),
            "1800".into(),
            "socat".into(),
            "TCP-LISTEN:5000,fork,reuseaddr".into(),
            destination.into(),
        ]),
        exposed_ports: Some(vec!["5000/tcp".into()]),
        labels: Some(HashMap::from([("ployz.managed".into(), String::new())])),
        host_config: Some(HostConfig {
            auto_remove: Some(true),
            binds: bind.map(|bind| vec![bind]),
            port_bindings: Some(port_bindings),
            ..Default::default()
        }),
        ..Default::default()
    };
    let name = format!(
        "ployz-push-proxy-{}-{}",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    );
    let created = docker
        .create_container(
            Some(CreateContainerOptionsBuilder::default().name(&name).build()),
            config,
        )
        .await
        .map_err(|error| format!("create proxy helper: {error}"))?;
    if let Err(error) = docker.start_container(&created.id, None).await {
        let _ = docker
            .remove_container(
                &created.id,
                Some(RemoveContainerOptionsBuilder::default().force(true).build()),
            )
            .await;
        return Err(format!("start proxy helper: {error}"));
    }
    if let Err(error) = wait_for_port(host_port).await {
        let _ = docker
            .remove_container(
                &created.id,
                Some(RemoveContainerOptionsBuilder::default().force(true).build()),
            )
            .await;
        return Err(format!("proxy helper {}: {error}", created.id));
    }
    Ok(Helper {
        id: created.id,
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
    repository: String,
    tag: String,
    full: String,
}

fn temporary_reference(port: u16, image: &str) -> Result<TemporaryReference, String> {
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
    let repository = format!("127.0.0.1:{port}/{name}");
    Ok(TemporaryReference {
        full: format!("{repository}:{tag}"),
        repository,
        tag: tag.into(),
    })
}

fn platform_json(platform: &str) -> Result<String, String> {
    match platform {
        "linux/amd64" => Ok(r#"{"os":"linux","architecture":"amd64"}"#.into()),
        "linux/arm64" => Ok(r#"{"os":"linux","architecture":"arm64"}"#.into()),
        _ => Err(format!("unsupported platform '{platform}'")),
    }
}

async fn cleanup(
    docker: &Docker,
    temporary: &str,
    helper: Option<&Helper>,
    socket: Option<&PathBuf>,
) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Err(error) = docker
        .remove_image(
            temporary,
            Some(RemoveImageOptionsBuilder::default().force(false).build()),
            None,
        )
        .await
        && !is_not_found(&error)
    {
        errors.push(error.to_string());
    }
    if let Some(helper) = helper
        && let Err(error) = docker
            .remove_container(
                &helper.id,
                Some(RemoveContainerOptionsBuilder::default().force(true).build()),
            )
            .await
        && !is_not_found(&error)
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

fn remove_socket(socket: Option<&PathBuf>) {
    if let Some(socket) = socket {
        let _ = fs::remove_file(socket);
    }
}

fn is_not_found(error: &DockerError) -> bool {
    matches!(
        error,
        DockerError::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
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
        let reference = temporary_reference(5000, "registry.test/team/api:v1").unwrap();
        assert_eq!(reference.full, "127.0.0.1:5000/registry.test/team/api:v1");
    }
}
