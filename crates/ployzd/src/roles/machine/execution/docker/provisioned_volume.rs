//! Docker local-driver shapes for durable named volumes.

use std::collections::HashMap;

use bollard::Docker;
use bollard::errors::Error as BollardError;
use bollard::models::{Volume, VolumeCreateRequest};
use ployz_core::deploy::VolumeName;
use ployz_core::intent::{VolumeKind, VolumePinState};
use ployz_core::machine::VolumeEnsureFailure;
use ployz_core::storage::PROVISIONED_VOLUME_MOUNTPOINT;

use super::network::is_docker_object_missing;
use crate::roles::machine::volume::docker_volume_name;

pub(super) async fn ensure_docker_volume(
    docker: &Docker,
    volume: &VolumePinState,
) -> Result<(), VolumeEnsureFailure> {
    let request = match volume.kind() {
        VolumeKind::Plain => plain_volume_request(volume),
        VolumeKind::Provisioned { .. } => local_bind_volume_request(volume),
    };
    let storage_name = docker_volume_name(volume.namespace_id(), volume.volume_name());
    let expected_driver = "local";
    let expected_options = request.driver_opts.as_ref().cloned().unwrap_or_default();
    match docker.inspect_volume(&storage_name).await {
        Ok(existing) => validate_volume_shape(
            volume.volume_name(),
            existing,
            expected_driver,
            &expected_options,
        ),
        Err(error) if is_docker_object_missing(&error) => {
            let created = docker
                .create_volume(request)
                .await
                .map_err(|error| docker_ensure_failed(volume, error))?;
            validate_volume_shape(
                volume.volume_name(),
                created,
                expected_driver,
                &expected_options,
            )
        }
        Err(error) => Err(docker_ensure_failed(volume, error)),
    }
}

pub(super) fn local_bind_volume_request(volume: &VolumePinState) -> VolumeCreateRequest {
    let storage_name = docker_volume_name(volume.namespace_id(), volume.volume_name());
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

fn plain_volume_request(volume: &VolumePinState) -> VolumeCreateRequest {
    VolumeCreateRequest {
        name: Some(docker_volume_name(
            volume.namespace_id(),
            volume.volume_name(),
        )),
        driver: Some("local".to_owned()),
        ..Default::default()
    }
}

fn validate_volume_shape(
    volume_name: &VolumeName,
    observed: Volume,
    expected_driver: &str,
    expected_options: &HashMap<String, String>,
) -> Result<(), VolumeEnsureFailure> {
    if observed.driver == expected_driver && observed.options == *expected_options {
        return Ok(());
    }
    Err(VolumeEnsureFailure::DockerShapeMismatch {
        volume_name: volume_name.clone(),
        message: format!(
            "expected driver {expected_driver:?} with options {expected_options:?}, observed driver {:?} with options {:?}",
            observed.driver, observed.options
        ),
    })
}

fn docker_ensure_failed(volume: &VolumePinState, error: BollardError) -> VolumeEnsureFailure {
    let retained_dataset = match volume.kind() {
        VolumeKind::Plain => None,
        VolumeKind::Provisioned { dataset, .. } => Some(dataset.clone()),
    };
    VolumeEnsureFailure::DockerEnsureFailed {
        volume_name: volume.volume_name().clone(),
        retained_dataset,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::{DatasetName, VolumeMaxSizeBytes, ZfsPoolName};
    use ployz_core::ids::{MachineId, NamespaceId};

    fn provisioned_pin() -> VolumePinState {
        let namespace = NamespaceId::try_new("default").expect("namespace");
        let volume = VolumeName::try_new("data").expect("volume");
        VolumePinState::try_new(
            namespace.clone(),
            volume.clone(),
            MachineId::try_new("machine-1").expect("machine"),
            VolumeKind::Provisioned {
                dataset: DatasetName::for_volume(
                    &ZfsPoolName::try_new("tank").expect("pool"),
                    &namespace,
                    &volume,
                )
                .expect("dataset"),
                max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("quota"),
            },
        )
        .expect("pin")
    }

    #[test]
    fn existing_docker_volume_must_match_the_provisioned_bind_shape() {
        let pin = provisioned_pin();
        let request = local_bind_volume_request(&pin);
        let expected_options = request.driver_opts.expect("options");
        let wrong = Volume {
            name: request.name.expect("name"),
            driver: "local".to_owned(),
            options: HashMap::new(),
            ..Default::default()
        };

        assert!(matches!(
            validate_volume_shape(pin.volume_name(), wrong, "local", &expected_options),
            Err(VolumeEnsureFailure::DockerShapeMismatch { .. })
        ));
    }

    #[test]
    fn docker_failure_names_the_retained_dataset_for_retry() {
        let pin = provisioned_pin();
        let failure = docker_ensure_failed(
            &pin,
            BollardError::DockerResponseServerError {
                status_code: 500,
                message: "daemon unavailable".to_owned(),
            },
        );
        let VolumeKind::Provisioned { dataset, .. } = pin.kind() else {
            panic!("provisioned pin")
        };
        assert_eq!(
            failure,
            VolumeEnsureFailure::DockerEnsureFailed {
                volume_name: pin.volume_name().clone(),
                retained_dataset: Some(dataset.clone()),
                message: "Docker responded with status code 500: daemon unavailable".to_owned(),
            }
        );
    }
}
