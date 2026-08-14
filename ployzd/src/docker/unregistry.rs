use std::{
    collections::HashMap,
    net::Ipv4Addr,
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
};

use bollard::models::{
    ContainerCreateBody, HostConfig, HostConfigLogConfig, Mount, MountType, PortBinding,
    RestartPolicy, RestartPolicyNameEnum,
};

use crate::network::{DOCKER_NETWORK_NAME, UNREGISTRY_PORT};

use super::{Error, LocalDocker, ManagedService};

pub const IMAGE: &str = "ghcr.io/psviderski/unregistry:0.4.1";
const NAME: &str = "ployz-unregistry";
const CONTAINER_SOCKET: &str = "/run/containerd/containerd.sock";
const CONFIG_VERSION: &str = "1";
const SOCKETS: &[&str] = &[
    "/run/containerd/containerd.sock",
    "/run/docker/containerd/containerd.sock",
    "/var/run/containerd/containerd.sock",
    "/var/run/docker/containerd/containerd.sock",
];

impl LocalDocker {
    pub async fn start_unregistry(
        &self,
        gateway: Ipv4Addr,
        configured_socket: Option<&Path>,
    ) -> Result<Option<RunningUnregistry>, Error> {
        if !self.uses_containerd_store().await? {
            eprintln!(
                "WARNING: unregistry disabled: Docker is not using the containerd image store"
            );
            return Ok(None);
        }
        let Some(socket) = detect_socket(configured_socket) else {
            eprintln!("WARNING: unregistry disabled: no containerd socket was detected");
            return Ok(None);
        };
        let service = RunningUnregistry {
            service: ManagedService::new(self.client.clone(), NAME, IMAGE),
            socket,
            gateway,
        };
        service.start().await?;
        Ok(Some(service))
    }
}

pub struct RunningUnregistry {
    service: ManagedService,
    socket: PathBuf,
    gateway: Ipv4Addr,
}

impl RunningUnregistry {
    async fn start(&self) -> Result<(), Error> {
        self.service
            .ensure(self.config(), |container| self.matches(container))
            .await
            .map_err(Into::into)
    }

    fn matches(&self, container: &bollard::models::ContainerInspectResponse) -> bool {
        let socket = self.socket.to_string_lossy();
        let gateway = self.gateway.to_string();
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
                == Some(socket.as_ref())
            && labels
                .and_then(|labels| labels.get("ployz.unregistry.gateway"))
                .map(String::as_str)
                == Some(gateway.as_str())
            && labels
                .and_then(|labels| labels.get("ployz.unregistry.config-version"))
                .map(String::as_str)
                == Some(CONFIG_VERSION)
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

    pub async fn stop(&self) -> Result<(), Error> {
        self.service.stop().await.map_err(Into::into)
    }

    pub async fn cleanup(&self) -> Result<(), Error> {
        self.service.remove().await.map_err(Into::into)
    }
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
}
