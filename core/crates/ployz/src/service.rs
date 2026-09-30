use ployz_core::RpcError;

#[derive(Clone, Debug)]
pub struct ContainerOperationFailure {
    pub container_id: ployz_core::ContainerId,
    pub error: RpcError,
}
