use ployz_core::{
    CreateVolumeReport, CreateVolumeRequest, DockerVolume, MachineId, MachineTarget, RpcError, op,
};

use crate::connect::{Client, TARGET_RPC_TIMEOUT};

#[derive(Clone, Debug)]
pub struct ContainerOperationFailure {
    pub container_id: ployz_core::ContainerId,
    pub error: RpcError,
}

pub(crate) async fn create_volume_on_machine(
    client: &Client,
    machine_id: &MachineId,
    request: CreateVolumeRequest,
) -> Result<DockerVolume, RpcError> {
    let report = client
        .invoke::<op::CreateVolume>(
            request,
            &MachineTarget::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await?;
    verified_created_volume(report)
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
