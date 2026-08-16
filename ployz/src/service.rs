use ployz_core::{CreateVolumeRequest, MachineId, MachineSelector, RpcError, op};

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
) -> Result<(), RpcError> {
    client
        .invoke::<op::CreateVolume>(
            request,
            &MachineSelector::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|_| ())
}
