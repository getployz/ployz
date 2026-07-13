//! Request-side adapter for reading a machine's WireGuard admission identity.

use crate::roles::machine::client::{MachineCallError, NatsMachineDataplaneReader, call_machine};
use crate::roles::machine::protocol::{
    MachineDataplanePublicKeyDomainError, MachineDataplanePublicKeyRpcOk,
    MachineDataplanePublicKeyRpcRequest,
};
use ployz_core::dataplane::{
    PloyzNativeMeshComponent, WireGuardEbpfPrepareError, WireGuardPublicKey,
};
use ployz_core::ids::MachineId;
use ployz_core::subjects::MachineServiceEndpoint;

impl NatsMachineDataplaneReader {
    pub(crate) async fn read_projection_public_key(
        &self,
        machine_id: &MachineId,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        let request = MachineDataplanePublicKeyRpcRequest {};
        call_machine::<MachineDataplanePublicKeyRpcOk, MachineDataplanePublicKeyDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::DataplanePublicKey,
            &request,
        )
        .await
        .map(|response| response.public_key)
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => WireGuardEbpfPrepareError::Unavailable {
                machine_id: machine_id.clone(),
                component: PloyzNativeMeshComponent::WireGuard,
                message: reason.failure_message(),
            },
            MachineCallError::Domain(error) => error.into_prepare_error(machine_id.clone()),
        })
    }
}

impl MachineDataplanePublicKeyDomainError {
    fn into_prepare_error(self, machine_id: MachineId) -> WireGuardEbpfPrepareError {
        match self {
            Self::Unavailable { component, message } => WireGuardEbpfPrepareError::Unavailable {
                machine_id,
                component,
                message,
            },
            Self::InvalidReport { message } => WireGuardEbpfPrepareError::InvalidReport { message },
        }
    }
}
