//! Docker local-driver bindings for prepared Provisioned Volume datasets.

use std::collections::HashMap;

use bollard::Docker;
use bollard::models::VolumeCreateRequest;
use ployz_core::deploy::{ServiceVolumeMount, VolumeName};
use ployz_core::ids::NamespaceId;
use ployz_core::storage::PROVISIONED_VOLUME_MOUNTPOINT;

use crate::roles::machine::runner::MachineContainerRunnerError;
use crate::roles::machine::volume::docker_volume_name;

pub(super) async fn ensure_provisioned_volumes(
    docker: &Docker,
    namespace_id: &NamespaceId,
    mounts: &[ServiceVolumeMount],
    provisioned_volumes: &[VolumeName],
) -> Result<(), MachineContainerRunnerError> {
    for mount in mounts
        .iter()
        .filter(|mount| provisioned_volumes.contains(&mount.volume_name))
    {
        docker
            .create_volume(local_bind_volume_request(namespace_id, &mount.volume_name))
            .await
            .map_err(|error| MachineContainerRunnerError::Create {
                message: error.to_string(),
            })?;
    }
    Ok(())
}

pub(super) fn local_bind_volume_request(
    namespace_id: &NamespaceId,
    volume_name: &VolumeName,
) -> VolumeCreateRequest {
    let storage_name = docker_volume_name(namespace_id, volume_name);
    VolumeCreateRequest {
        name: Some(storage_name.clone()),
        driver: Some("local".to_owned()),
        driver_opts: Some(HashMap::from([
            (
                "device".to_owned(),
                format!("{PROVISIONED_VOLUME_MOUNTPOINT}/{storage_name}"),
            ),
            ("o".to_owned(), "bind".to_owned()),
            ("type".to_owned(), "none".to_owned()),
        ])),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::ContainerMountPath;

    #[tokio::test]
    async fn plain_volume_does_not_create_a_local_driver_binding() {
        let directory = tempfile::tempdir().expect("socket directory");
        let socket = directory.path().join("docker.sock");
        let _listener = tokio::net::UnixListener::bind(&socket).expect("stub Docker socket");
        let docker = Docker::connect_with_socket(
            socket.to_str().expect("UTF-8 socket path"),
            1,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("lazy Docker client");
        let volume_name = VolumeName::try_new("data").expect("volume");
        let mounts = [ServiceVolumeMount {
            volume_name,
            target: ContainerMountPath::try_new("/data").expect("mount path"),
        }];

        ensure_provisioned_volumes(
            &docker,
            &NamespaceId::try_new("default").expect("namespace"),
            &mounts,
            &[],
        )
        .await
        .expect("plain volumes use Docker's default named-volume behavior");
    }
}
