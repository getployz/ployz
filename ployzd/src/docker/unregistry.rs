use std::{
    collections::HashMap,
    net::{Ipv6Addr, SocketAddr},
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use bollard::models::{
    ContainerCreateBody, HostConfig, HostConfigLogConfig, Mount, MountType, RestartPolicyNameEnum,
};
use ployz_core::{
    ImageIngestDestination, ImageIngestOpened, ImageIngestReason, ManagementAddress, RpcError,
};
use tokio::{net::TcpStream, sync::Mutex};

use crate::network::UNREGISTRY_PORT;

use super::{Error, LocalDocker, ManagedService};

pub const IMAGE: &str = "ghcr.io/psviderski/unregistry:0.4.1";
const NAME: &str = "ployz-unregistry";
const CONTAINER_SOCKET_PARENT: &str = "/run/ployz-containerd";
const CONFIG_VERSION: &str = "2";
const READY_RETRY: Duration = Duration::from_millis(10);
const READY_TIMEOUT: Duration = Duration::from_secs(5);
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

/// Disposable Direct Image Transfer helper for one Machine.
pub struct ImageIngest {
    configured_socket: Option<PathBuf>,
    docker: Option<LocalDocker>,
    reconciliation: Mutex<()>,
}

impl ImageIngest {
    /// Image ingest that reconciles the helper on every `open`.
    #[must_use]
    pub fn new(configured_socket: Option<PathBuf>, docker: Option<LocalDocker>) -> Arc<Self> {
        Arc::new(Self {
            configured_socket,
            docker,
            reconciliation: Mutex::new(()),
        })
    }

    /// Start the helper if needed and return the Management Address TCP destination.
    ///
    /// # Errors
    ///
    /// Returns a named ingest RPC error when the Machine cannot ingest, or when
    /// the helper fails to start.
    pub async fn open(
        &self,
        management_address: ManagementAddress,
    ) -> Result<ImageIngestOpened, RpcError> {
        let opened = ImageIngestOpened {
            destination: ImageIngestDestination {
                management_address,
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
        let _reconciliation = self.reconciliation.lock().await;
        docker
            .reconcile_unregistry(management_address.0, &socket)
            .await
            .map_err(|error| ImageIngestReason::StartFailed.rpc_error(error.to_string()))?;
        wait_for_unregistry(SocketAddr::from((management_address.0, UNREGISTRY_PORT)))
            .await
            .map_err(|error| ImageIngestReason::StartFailed.rpc_error(error.to_string()))?;
        Ok(opened)
    }

    /// Remove the disposable helper, including one left by a previous process.
    ///
    /// # Errors
    ///
    /// Returns when Docker cannot remove the helper.
    pub async fn shutdown(&self) -> Result<(), Error> {
        let Some(docker) = &self.docker else {
            return Ok(());
        };
        let _reconciliation = self.reconciliation.lock().await;
        ManagedService::endpoint(docker.clone(), NAME, IMAGE)
            .remove()
            .await
            .map_err(Into::into)
    }
}

async fn wait_for_unregistry(address: SocketAddr) -> Result<(), Error> {
    tokio::time::timeout(READY_TIMEOUT, async {
        loop {
            if TcpStream::connect(address).await.is_ok() {
                return;
            }
            tokio::time::sleep(READY_RETRY).await;
        }
    })
    .await
    .map_err(|_| Error::UnregistryNotReady {
        address,
        timeout: READY_TIMEOUT,
    })
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

    /// Reconcile the image-ingest helper against observed Docker state.
    ///
    /// # Errors
    ///
    /// Returns when the helper cannot be created or started.
    pub(crate) async fn reconcile_unregistry(
        &self,
        management_address: Ipv6Addr,
        socket: &Path,
    ) -> Result<(), Error> {
        ManagedService::endpoint(self.clone(), NAME, IMAGE)
            .ensure_endpoint(unregistry_config(socket, management_address), |container| {
                unregistry_matches(container, socket, management_address)
            })
            .await
    }
}

fn unregistry_config(socket: &Path, management_address: Ipv6Addr) -> ContainerCreateBody {
    let parent = socket_parent(socket).expect("validated containerd socket parent");
    let socket_name = socket
        .file_name()
        .expect("validated containerd socket filename")
        .to_string_lossy();
    let container_socket = format!("{CONTAINER_SOCKET_PARENT}/{socket_name}");
    ContainerCreateBody {
        image: Some(IMAGE.into()),
        env: Some(vec![
            format!("UNREGISTRY_ADDR=[{management_address}]:{UNREGISTRY_PORT}"),
            "UNREGISTRY_CONTAINERD_NAMESPACE=moby".into(),
            format!("UNREGISTRY_CONTAINERD_SOCK={container_socket}"),
        ]),
        labels: Some(HashMap::from([
            ("ployz.managed".into(), String::new()),
            (
                "ployz.unregistry.socket".into(),
                socket.to_string_lossy().into_owned(),
            ),
            (
                "ployz.unregistry.management-address".into(),
                management_address.to_string(),
            ),
            (
                "ployz.unregistry.config-version".into(),
                CONFIG_VERSION.into(),
            ),
        ])),
        host_config: Some(HostConfig {
            mounts: Some(vec![Mount {
                typ: Some(MountType::BIND),
                source: Some(parent.to_string_lossy().into_owned()),
                target: Some(CONTAINER_SOCKET_PARENT.into()),
                read_only: Some(true),
                ..Default::default()
            }]),
            network_mode: Some("host".into()),
            log_config: Some(HostConfigLogConfig {
                typ: Some("local".into()),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Whether an observed helper exactly matches this Machine's ingest configuration.
#[must_use]
pub fn unregistry_matches(
    container: &bollard::models::ContainerInspectResponse,
    socket: &Path,
    management_address: Ipv6Addr,
) -> bool {
    let socket_path = socket.to_string_lossy();
    let Some(config) = container.config.as_ref() else {
        return false;
    };
    let Some(labels) = config.labels.as_ref() else {
        return false;
    };
    let expected_env = unregistry_config(socket, management_address)
        .env
        .expect("unregistry environment");
    let management_address = management_address.to_string();
    let Some(host) = container.host_config.as_ref() else {
        return false;
    };
    let Some(mounts) = host.mounts.as_deref() else {
        return false;
    };
    let [mount] = mounts else {
        return false;
    };
    let Some(parent) = socket_parent(socket) else {
        return false;
    };
    let no_restart = host
        .restart_policy
        .as_ref()
        .and_then(|policy| policy.name.as_ref())
        .is_none_or(|name| {
            matches!(
                name,
                RestartPolicyNameEnum::EMPTY | RestartPolicyNameEnum::NO
            )
        });

    let Some(env) = config.env.as_ref() else {
        return false;
    };

    config.image.as_deref() == Some(IMAGE)
        && expected_env.iter().all(|expected| env.contains(expected))
        && env
            .iter()
            .filter(|value| value.starts_with("UNREGISTRY_"))
            .count()
            == expected_env.len()
        && labels.get("ployz.unregistry.socket").map(String::as_str) == Some(socket_path.as_ref())
        && labels
            .get("ployz.unregistry.management-address")
            .map(String::as_str)
            == Some(management_address.as_str())
        && labels
            .get("ployz.unregistry.config-version")
            .map(String::as_str)
            == Some(CONFIG_VERSION)
        && labels.contains_key("ployz.managed")
        && mount.typ == Some(MountType::BIND)
        && mount.source.as_deref() == Some(parent.to_string_lossy().as_ref())
        && mount.target.as_deref() == Some(CONTAINER_SOCKET_PARENT)
        && mount.read_only == Some(true)
        && host.network_mode.as_deref() == Some("host")
        && host.port_bindings.as_ref().is_none_or(HashMap::is_empty)
        && no_restart
}

fn detect_socket(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return path
            .canonicalize()
            .ok()
            .filter(|path| is_socket(path) && socket_parent(path).is_some());
    }
    SOCKETS.iter().map(Path::new).find_map(|path| {
        path.canonicalize()
            .ok()
            .filter(|path| is_socket(path) && socket_parent(path).is_some())
    })
}

fn socket_parent(socket: &Path) -> Option<&Path> {
    socket.file_name()?;
    socket.parent().filter(|parent| *parent != Path::new("/"))
}

fn is_socket(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{
        AdvertisedEndpoint, LocalMachinePhase, MACHINE_API_PORT, Machine, MachineId, MachineName,
        WireGuardPublicKey,
    };

    /// Management address unregistry would bind when this Machine is active.
    fn unregistry_management_address(
        phase: &LocalMachinePhase,
        machine: Option<&Machine>,
    ) -> Option<Ipv6Addr> {
        match phase {
            LocalMachinePhase::Joining | LocalMachinePhase::Participating => {
                machine.map(|machine| machine.management_address().0)
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
            public_key: WireGuardPublicKey([2; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint(
                format!("192.0.2.2:{MACHINE_API_PORT}").parse().unwrap(),
            )],
            runtime: Default::default(),
        }
    }

    #[test]
    fn joining_machine_exposes_an_unregistry_management_address() {
        let machine = machine();
        assert_eq!(
            unregistry_management_address(&LocalMachinePhase::Joining, Some(&machine)),
            Some("fdcc::2".parse().unwrap())
        );
        assert_eq!(
            unregistry_management_address(&LocalMachinePhase::Participating, Some(&machine)),
            Some("fdcc::2".parse().unwrap())
        );
        assert_eq!(
            unregistry_management_address(&LocalMachinePhase::Uninitialized, Some(&machine)),
            None
        );
        assert_eq!(
            unregistry_management_address(&LocalMachinePhase::Resetting, Some(&machine)),
            None
        );
        assert_eq!(
            unregistry_management_address(&LocalMachinePhase::Joining, None),
            None
        );
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
        let alias = root.join("configured.sock");
        std::os::unix::fs::symlink(&socket, &alias).unwrap();
        assert_eq!(detect_socket(Some(&alias)), Some(socket.clone()));
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn socket_mount_rejects_a_root_parent_and_requires_a_filename() {
        assert_eq!(socket_parent(Path::new("/containerd.sock")), None);
        assert_eq!(socket_parent(Path::new("/")), None);
        assert_eq!(
            socket_parent(Path::new("/run/containerd/containerd.sock")),
            Some(Path::new("/run/containerd"))
        );
    }

    #[test]
    fn unregistry_uses_host_network_and_a_read_only_socket_parent_mount() {
        let root = temp_root("config");
        let socket = root.join("containerd.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let config = unregistry_config(&socket, "fdcc::2".parse().unwrap());

        assert_eq!(config.image.as_deref(), Some(IMAGE));
        assert_eq!(
            config.env,
            Some(vec![
                format!("UNREGISTRY_ADDR=[fdcc::2]:{UNREGISTRY_PORT}"),
                "UNREGISTRY_CONTAINERD_NAMESPACE=moby".into(),
                format!("UNREGISTRY_CONTAINERD_SOCK={CONTAINER_SOCKET_PARENT}/containerd.sock"),
            ])
        );
        assert_eq!(config.exposed_ports, None);
        let host = config.host_config.unwrap();
        assert_eq!(host.network_mode.as_deref(), Some("host"));
        assert_eq!(host.port_bindings, None);
        assert_eq!(host.restart_policy, None);
        assert_eq!(
            host.mounts,
            Some(vec![Mount {
                typ: Some(MountType::BIND),
                source: Some(root.to_string_lossy().into_owned()),
                target: Some(CONTAINER_SOCKET_PARENT.into()),
                read_only: Some(true),
                ..Default::default()
            }])
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn socket_recreation_keeps_the_parent_mounted_unregistry_current() {
        let root = temp_root("inode");
        let socket = root.join("containerd.sock");
        let management_address = Ipv6Addr::LOCALHOST;
        let first = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let bound = inspect_bound_to(&socket, management_address);
        assert!(unregistry_matches(&bound, &socket, management_address));

        drop(first);
        std::fs::remove_file(&socket).unwrap();
        let _recreated = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert!(unregistry_matches(&bound, &socket, management_address));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_waits_until_the_endpoint_accepts_tcp() {
        let reservation = std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);

        let waiting = tokio::spawn(wait_for_unregistry(address));
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        let _listener = tokio::net::TcpListener::bind(address).await.unwrap();
        tokio::time::advance(std::time::Duration::from_millis(10)).await;
        assert!(waiting.await.unwrap().is_ok());
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
        management_address: Ipv6Addr,
    ) -> bollard::models::ContainerInspectResponse {
        let config = unregistry_config(socket, management_address);
        bollard::models::ContainerInspectResponse {
            config: Some(bollard::models::ContainerConfig {
                image: config.image,
                env: config.env,
                exposed_ports: config.exposed_ports,
                labels: config.labels,
                ..Default::default()
            }),
            host_config: config.host_config,
            ..Default::default()
        }
    }
}
