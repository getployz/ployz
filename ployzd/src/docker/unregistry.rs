use std::{
    collections::HashMap,
    net::Ipv4Addr,
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
};

use bollard::{
    errors::Error as DockerError,
    models::{
        ContainerCreateBody, HostConfig, HostConfigLogConfig, Mount, MountType, PortBinding,
        RestartPolicy, RestartPolicyNameEnum,
    },
    query_parameters::{
        CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder,
    },
};
use futures_util::TryStreamExt;

use crate::network::DOCKER_NETWORK_NAME;

use super::{Error, LocalDocker};

pub const IMAGE: &str = "ghcr.io/psviderski/unregistry:0.4.1";
const NAME: &str = "ployz-unregistry";
const CONTAINER_SOCKET: &str = "/run/containerd/containerd.sock";
const PORT: u16 = 51500;
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
            docker: self.clone(),
            socket,
            gateway,
        };
        service.start().await?;
        Ok(Some(service))
    }
}

pub struct RunningUnregistry {
    docker: LocalDocker,
    socket: PathBuf,
    gateway: Ipv4Addr,
}

impl RunningUnregistry {
    async fn start(&self) -> Result<(), Error> {
        match self.docker.client.inspect_container(NAME, None).await {
            Ok(container) if self.matches(&container) => {
                if !container
                    .state
                    .and_then(|state| state.running)
                    .unwrap_or(false)
                {
                    self.docker.client.start_container(NAME, None).await?;
                }
            }
            Ok(_) => {
                self.cleanup().await?;
                self.create().await?;
            }
            Err(error) if is_not_found(&error) => self.create().await?,
            Err(error) => return Err(error.into()),
        }
        Ok(())
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
    }

    async fn create(&self) -> Result<(), Error> {
        if let Err(error) = self.docker.client.inspect_image(IMAGE).await {
            if !is_not_found(&error) {
                return Err(error.into());
            }
            self.docker
                .client
                .create_image(
                    Some(
                        CreateImageOptionsBuilder::default()
                            .from_image(IMAGE)
                            .build(),
                    ),
                    None,
                    None,
                )
                .try_collect::<Vec<_>>()
                .await?;
        }
        let port_bindings = HashMap::from([(
            "5000/tcp".into(),
            Some(vec![PortBinding {
                host_ip: Some(self.gateway.to_string()),
                host_port: Some(PORT.to_string()),
            }]),
        )]);
        let config = ContainerCreateBody {
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
        };
        self.docker
            .client
            .create_container(
                Some(CreateContainerOptionsBuilder::default().name(NAME).build()),
                config,
            )
            .await?;
        self.docker.client.start_container(NAME, None).await?;
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), Error> {
        match self.docker.client.stop_container(NAME, None).await {
            Ok(()) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn cleanup(&self) -> Result<(), Error> {
        self.stop().await?;
        match self
            .docker
            .client
            .remove_container(
                NAME,
                Some(RemoveContainerOptionsBuilder::default().v(true).build()),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(error.into()),
        }
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
