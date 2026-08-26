use ployz_core::{CreateVolumeReport, DockerVolume, RpcError};

#[derive(Clone, Debug)]
pub struct ContainerOperationFailure {
    pub container_id: ployz_core::ContainerId,
    pub error: RpcError,
}

pub(crate) fn verified_created_volume(
    report: CreateVolumeReport,
) -> Result<DockerVolume, RpcError> {
    report.into_observation().map_err(|failure| {
        let mut error = failure.error;
        error.message = format!(
            "Docker Volume {} was created on {} but could not be verified: {}",
            failure.id.name, failure.id.machine_id, error.message
        );
        error
    })
}
