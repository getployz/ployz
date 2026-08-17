use std::{
    collections::HashMap,
    net::Ipv4Addr,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use bollard::models::{
    ContainerCreateBody, HostConfig, HostConfigLogConfig, Mount, MountType, PortBinding,
    RestartPolicy, RestartPolicyNameEnum,
};
use ployz_core::{
    ImageIngestDestination, ImageIngestOpened, ImageIngestReason, MachineGateway, RpcError,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::network::{DOCKER_NETWORK_NAME, UNREGISTRY_PORT};

use super::{Error, LocalDocker, ManagedService};

pub const IMAGE: &str = "ghcr.io/psviderski/unregistry:0.4.1";
const NAME: &str = "ployz-unregistry";
const CONTAINER_SOCKET: &str = "/run/containerd/containerd.sock";
const CONFIG_VERSION: &str = "1";
const SOCKET_INODE_LABEL: &str = "ployz.unregistry.socket-inode";
const SOCKET_REFRESH: Duration = Duration::from_secs(2);
const SOCKETS: &[&str] = &[
    "/run/containerd/containerd.sock",
    "/run/docker/containerd/containerd.sock",
    "/var/run/containerd/containerd.sock",
    "/var/run/docker/containerd/containerd.sock",
];

/// Store and socket gates for image ingest on this Machine.
pub(crate) enum ImageIngestPrerequisite {
    Ready(PathBuf),
    UnsupportedStore,
    MissingSocket,
}

impl ImageIngestPrerequisite {
    fn socket(self) -> Result<PathBuf, RpcError> {
        match self {
            Self::Ready(socket) => Ok(socket),
            Self::UnsupportedStore => Err(ImageIngestReason::UnsupportedContainerdStore
                .rpc_error("Docker is not using the containerd image store")),
            Self::MissingSocket => Err(ImageIngestReason::ContainerdSocketMissing
                .rpc_error("no containerd socket was detected")),
        }
    }
}

/// Image ingest for one Machine: first `open` starts the helper; later opens reuse it.
pub struct ImageIngest {
    slot: Mutex<Option<StartedIngest>>,
    configured_socket: Option<PathBuf>,
    refresh_shutdown: CancellationToken,
    docker: Option<LocalDocker>,
}

struct StartedIngest {
    running: Arc<RunningUnregistry>,
    refresh: tokio::task::JoinHandle<()>,
}

impl ImageIngest {
    /// Empty ingest that starts the helper on first `open`.
    #[must_use]
    pub fn new(
        configured_socket: Option<PathBuf>,
        refresh_shutdown: CancellationToken,
        docker: Option<LocalDocker>,
    ) -> Arc<Self> {
        Arc::new(Self {
            slot: Mutex::new(None),
            configured_socket,
            refresh_shutdown,
            docker,
        })
    }

    /// Start the helper if needed and return the Machine Gateway TCP destination.
    ///
    /// # Errors
    ///
    /// Returns a named ingest RPC error when the Machine cannot ingest, or when
    /// the helper fails to start.
    pub async fn open(&self, gateway: MachineGateway) -> Result<ImageIngestOpened, RpcError> {
        let opened = ImageIngestOpened {
            destination: ImageIngestDestination {
                gateway,
                port: UNREGISTRY_PORT,
            },
        };
        let Some(docker) = &self.docker else {
            return Err(ImageIngestReason::DockerUnavailable.rpc_error("Docker is not available"));
        };
        let socket = match docker
            .image_ingest_prerequisite(self.configured_socket.as_deref())
            .await
        {
            Ok(prerequisite) => prerequisite.socket()?,
            Err(error) => {
                return Err(ImageIngestReason::DockerUnavailable.rpc_error(error.to_string()));
            }
        };
        let mut slot = self.slot.lock().await;
        if slot.is_some() {
            return Ok(opened);
        }
        let running = docker
            .start_unregistry(gateway.0, socket)
            .await
            .map_err(|error| ImageIngestReason::StartFailed.rpc_error(error.to_string()))?;
        let running = Arc::new(running);
        let refresh = {
            let running = Arc::clone(&running);
            let shutdown = self.refresh_shutdown.clone();
            tokio::spawn(async move {
                let _ = running.keep_socket_current(shutdown).await;
            })
        };
        *slot = Some(StartedIngest { running, refresh });
        Ok(opened)
    }

    /// Stop the helper if this process started it.
    ///
    /// # Errors
    ///
    /// Returns when Docker cannot stop the helper.
    pub async fn stop(&self) -> Result<(), Error> {
        let Some(started) = self.slot.lock().await.take() else {
            return Ok(());
        };
        started.refresh.abort();
        started.running.stop().await
    }

    /// Remove the helper, including a leftover from a previous process.
    ///
    /// # Errors
    ///
    /// Returns when Docker cannot remove the helper.
    pub async fn cleanup(&self) -> Result<(), Error> {
        if let Some(started) = self.slot.lock().await.take() {
            started.refresh.abort();
            return started.running.cleanup().await;
        }
        let Some(docker) = &self.docker else {
            return Ok(());
        };
        ManagedService::new(docker.client.clone(), NAME, IMAGE)
            .remove()
            .await
            .map_err(Into::into)
    }
}

impl LocalDocker {
    /// Whether Docker's image store and a containerd socket are ready for ingest.
    ///
    /// # Errors
    ///
    /// Returns when Docker info cannot be read.
    pub(crate) async fn image_ingest_prerequisite(
        &self,
        configured_socket: Option<&Path>,
    ) -> Result<ImageIngestPrerequisite, Error> {
        if !self.uses_containerd_store().await? {
            return Ok(ImageIngestPrerequisite::UnsupportedStore);
        }
        Ok(match detect_socket(configured_socket) {
            Some(socket) => ImageIngestPrerequisite::Ready(socket),
            None => ImageIngestPrerequisite::MissingSocket,
        })
    }

    /// Start the image-ingest helper on this socket.
    ///
    /// # Errors
    ///
    /// Returns when the helper cannot be created or started.
    pub(crate) async fn start_unregistry(
        &self,
        gateway: Ipv4Addr,
        socket: PathBuf,
    ) -> Result<RunningUnregistry, Error> {
        let service = RunningUnregistry {
            service: ManagedService::new(self.client.clone(), NAME, IMAGE),
            socket,
            gateway,
        };
        service.start().await?;
        Ok(service)
    }
}

pub(crate) struct RunningUnregistry {
    service: ManagedService,
    socket: PathBuf,
    gateway: Ipv4Addr,
}

impl RunningUnregistry {
    async fn start(&self) -> Result<(), Error> {
        self.service
            .ensure(self.config(), |container| {
                unregistry_matches(container, &self.socket, self.gateway)
            })
            .await
            .map_err(Into::into)
    }

    fn config(&self) -> ContainerCreateBody {
        let port_bindings = HashMap::from([(
            "5000/tcp".into(),
            Some(vec![PortBinding {
                host_ip: Some(self.gateway.to_string()),
                host_port: Some(UNREGISTRY_PORT.to_string()),
            }]),
        )]);
        ContainerCreateBody {
            image: Some(IMAGE.into()),
            env: Some(vec![
                "UNREGISTRY_ADDR=:5000".into(),
                "UNREGISTRY_CONTAINERD_NAMESPACE=moby".into(),
                format!("UNREGISTRY_CONTAINERD_SOCK={CONTAINER_SOCKET}"),
            ]),
            exposed_ports: Some(vec!["5000/tcp".into()]),
            labels: Some(HashMap::from([
                ("ployz.managed".into(), String::new()),
                (
                    "ployz.unregistry.socket".into(),
                    self.socket.to_string_lossy().into_owned(),
                ),
                ("ployz.unregistry.gateway".into(), self.gateway.to_string()),
                (
                    "ployz.unregistry.config-version".into(),
                    CONFIG_VERSION.into(),
                ),
                (
                    SOCKET_INODE_LABEL.into(),
                    socket_inode_label(&self.socket).unwrap_or_default(),
                ),
            ])),
            host_config: Some(HostConfig {
                mounts: Some(vec![Mount {
                    typ: Some(MountType::BIND),
                    source: Some(self.socket.to_string_lossy().into_owned()),
                    target: Some(CONTAINER_SOCKET.into()),
                    read_only: Some(false),
                    ..Default::default()
                }]),
                port_bindings: Some(port_bindings),
                network_mode: Some(DOCKER_NETWORK_NAME.into()),
                restart_policy: Some(RestartPolicy {
                    name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                    ..Default::default()
                }),
                log_config: Some(HostConfigLogConfig {
                    typ: Some("local".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    pub async fn keep_socket_current(&self, shutdown: CancellationToken) -> Result<(), Error> {
        let mut ticks = tokio::time::interval(SOCKET_REFRESH);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticks.tick().await;
        loop {
            tokio::select! {
                _ = ticks.tick() => {
                    if let Err(error) = self.start().await {
                        eprintln!("WARNING: unregistry socket refresh failed: {error}");
                    }
                }
                () = shutdown.cancelled() => return Ok(()),
            }
        }
    }

    pub async fn stop(&self) -> Result<(), Error> {
        self.service.stop().await.map_err(Into::into)
    }

    pub async fn cleanup(&self) -> Result<(), Error> {
        self.service.remove().await.map_err(Into::into)
    }
}

/// Whether a running unregistry still matches this Machine's socket, gateway, and live socket inode.
#[must_use]
pub fn unregistry_matches(
    container: &bollard::models::ContainerInspectResponse,
    socket: &Path,
    gateway: Ipv4Addr,
) -> bool {
    let socket_name = socket.to_string_lossy();
    let gateway = gateway.to_string();
    let labels = container
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref());
    container
        .config
        .as_ref()
        .and_then(|config| config.image.as_deref())
        == Some(IMAGE)
        && labels
            .and_then(|labels| labels.get("ployz.unregistry.socket"))
            .map(String::as_str)
            == Some(socket_name.as_ref())
        && labels
            .and_then(|labels| labels.get("ployz.unregistry.gateway"))
            .map(String::as_str)
            == Some(gateway.as_str())
        && labels
            .and_then(|labels| labels.get("ployz.unregistry.config-version"))
            .map(String::as_str)
            == Some(CONFIG_VERSION)
        && bound_to_current_socket_inode(labels, socket)
}

fn bound_to_current_socket_inode(labels: Option<&HashMap<String, String>>, socket: &Path) -> bool {
    match socket_inode_label(socket) {
        None => true,
        Some(current) => {
            labels
                .and_then(|labels| labels.get(SOCKET_INODE_LABEL))
                .map(String::as_str)
                == Some(current.as_str())
        }
    }
}

fn socket_inode_label(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    metadata.file_type().is_socket().then(|| {
        // ctime distinguishes a replacement file when the filesystem reuses the inode number.
        format!(
            "{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec()
        )
    })
}

fn detect_socket(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return is_socket(path).then(|| path.to_owned());
    }
    SOCKETS
        .iter()
        .map(Path::new)
        .find(|path| is_socket(path))
        .map(Path::to_path_buf)
}

fn is_socket(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{
        AdvertisedEndpoint, LocalMachinePhase, Machine, MachineId, MachineName, ManagementAddress,
        WireGuardPublicKey,
    };

    /// Gateway unregistry would bind when this Machine already has a subnet.
    fn unregistry_gateway(
        phase: &LocalMachinePhase,
        machine: Option<&Machine>,
    ) -> Option<Ipv4Addr> {
        match phase {
            LocalMachinePhase::Joining | LocalMachinePhase::Participating => {
                machine.map(|machine| machine.subnet.gateway().0)
            }
            LocalMachinePhase::Uninitialized
            | LocalMachinePhase::Resetting
            | LocalMachinePhase::Unrecognized(_) => None,
        }
    }

    fn machine() -> Machine {
        Machine {
            id: MachineId::parse("a".repeat(32)).unwrap(),
            name: MachineName::parse("machine-2").unwrap(),
            subnet: "10.210.2.0/24".parse().unwrap(),
            management_address: ManagementAddress("fdcc::2".parse().unwrap()),
            public_key: WireGuardPublicKey([2; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.2:51000".parse().unwrap())],
            runtime: Default::default(),
        }
    }

    #[test]
    fn joining_machine_exposes_an_unregistry_gateway() {
        let machine = machine();
        assert_eq!(
            unregistry_gateway(&LocalMachinePhase::Joining, Some(&machine)),
            Some(Ipv4Addr::new(10, 210, 2, 1))
        );
        assert_eq!(
            unregistry_gateway(&LocalMachinePhase::Participating, Some(&machine)),
            Some(Ipv4Addr::new(10, 210, 2, 1))
        );
        assert_eq!(
            unregistry_gateway(&LocalMachinePhase::Uninitialized, Some(&machine)),
            None
        );
        assert_eq!(
            unregistry_gateway(&LocalMachinePhase::Resetting, Some(&machine)),
            None
        );
        assert_eq!(unregistry_gateway(&LocalMachinePhase::Joining, None), None);
    }

    #[test]
    fn ingest_prerequisites_map_to_named_reasons() {
        let unsupported = ImageIngestPrerequisite::UnsupportedStore
            .socket()
            .unwrap_err();
        assert_eq!(
            ImageIngestReason::from_details(&unsupported.details),
            Some(ImageIngestReason::UnsupportedContainerdStore)
        );
        let missing = ImageIngestPrerequisite::MissingSocket.socket().unwrap_err();
        assert_eq!(
            ImageIngestReason::from_details(&missing.details),
            Some(ImageIngestReason::ContainerdSocketMissing)
        );
        let socket = PathBuf::from("/run/containerd/containerd.sock");
        assert_eq!(
            ImageIngestPrerequisite::Ready(socket.clone())
                .socket()
                .unwrap(),
            socket
        );
    }

    #[test]
    fn configured_containerd_socket_wins_and_regular_files_are_rejected() {
        let root = std::env::temp_dir().join(format!("ployz-unregistry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let regular = root.join("regular");
        std::fs::write(&regular, "not a socket").unwrap();
        assert_ne!(detect_socket(Some(&regular)), Some(regular));
        let socket = root.join("containerd.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert_eq!(detect_socket(Some(&socket)), Some(socket.clone()));
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn running_unregistry_is_replaced_when_containerd_recreates_the_socket_inode() {
        let root = temp_root("inode");
        let socket = root.join("containerd.sock");
        let gateway = Ipv4Addr::new(10, 210, 0, 1);
        let first = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let bound = inspect_bound_to(&socket, gateway);
        assert!(unregistry_matches(&bound, &socket, gateway));
        let original = socket_inode_label(&socket).unwrap();

        drop(first);
        std::fs::remove_file(&socket).unwrap();
        let started = std::time::Instant::now();
        let _recreated = loop {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            if socket_inode_label(&socket).as_ref() != Some(&original) {
                break listener;
            }
            drop(listener);
            std::fs::remove_file(&socket).unwrap();
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "recreated socket kept the original identity {original}"
            );
        };
        assert!(!unregistry_matches(&bound, &socket, gateway));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn running_unregistry_is_replaced_when_recorded_socket_inode_does_not_match() {
        let root = temp_root("stale-label");
        let socket = root.join("containerd.sock");
        let gateway = Ipv4Addr::new(10, 210, 0, 1);
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let stale = inspect_bound_to_inode(&socket, gateway, "0:0:0:0".into());
        assert!(!unregistry_matches(&stale, &socket, gateway));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unregistry_without_inode_label_is_replaced_when_the_socket_exists() {
        let root = temp_root("missing-inode");
        let socket = root.join("containerd.sock");
        let gateway = Ipv4Addr::new(10, 210, 0, 1);
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let mut bound = inspect_bound_to(&socket, gateway);
        bound
            .config
            .as_mut()
            .unwrap()
            .labels
            .as_mut()
            .unwrap()
            .remove(SOCKET_INODE_LABEL);
        assert!(!unregistry_matches(&bound, &socket, gateway));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_containerd_socket_does_not_force_unregistry_replacement() {
        let root = temp_root("missing-sock");
        let socket = root.join("containerd.sock");
        let gateway = Ipv4Addr::new(10, 210, 0, 1);
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let bound = inspect_bound_to(&socket, gateway);
        drop(listener);
        std::fs::remove_file(&socket).unwrap();
        assert!(unregistry_matches(&bound, &socket, gateway));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ployz-unregistry-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn inspect_bound_to(
        socket: &Path,
        gateway: Ipv4Addr,
    ) -> bollard::models::ContainerInspectResponse {
        inspect_bound_to_inode(
            socket,
            gateway,
            socket_inode_label(socket).expect("test socket"),
        )
    }

    fn inspect_bound_to_inode(
        socket: &Path,
        gateway: Ipv4Addr,
        inode: String,
    ) -> bollard::models::ContainerInspectResponse {
        bollard::models::ContainerInspectResponse {
            config: Some(bollard::models::ContainerConfig {
                image: Some(IMAGE.into()),
                labels: Some(HashMap::from([
                    (
                        "ployz.unregistry.socket".into(),
                        socket.to_string_lossy().into_owned(),
                    ),
                    ("ployz.unregistry.gateway".into(), gateway.to_string()),
                    (
                        "ployz.unregistry.config-version".into(),
                        CONFIG_VERSION.into(),
                    ),
                    (SOCKET_INODE_LABEL.into(), inode),
                ])),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}
