use ployz_core::{
    CreateVolumeRequest, MachineId, MachineSelector, PartialResult, RpcError, ServiceSelectorError,
    op,
};
use thiserror::Error;

use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT};

#[derive(Debug, Error)]
pub enum ServiceClientError {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Selector(#[from] ServiceSelectorError),
}

pub struct LifecycleResult {
    pub observations: PartialResult<Vec<ployz_core::ContainerObservation>, RpcError>,
    pub outcomes: PartialResult<ployz_core::ContainerId, ContainerOperationFailure>,
}

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
