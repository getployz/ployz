use super::{MachineCallError, call_machine};
use crate::roles::machine::MachineRuntimeUnavailableReason;
use crate::roles::machine::protocol::{
    MachineVolumeTestimonyDomainError, MachineVolumeTestimonyResult, MachineVolumeTestimonyRpcOk,
    MachineVolumeTestimonyRpcRequest,
};
use ployz_core::ids::MachineId;
use ployz_core::intent::VolumePinState;
use ployz_nats::subjects::MachineServiceEndpoint;
use std::time::Duration;

pub const DEFAULT_VOLUME_TESTIMONY_RPC_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Debug, Clone)]
pub struct NatsMachineVolumeTestimonyReader {
    client: async_nats::Client,
    request_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineVolumeTestimonyReadError {
    #[error(
        "machine {} volume testimony unavailable: {}",
        machine_id.as_str(),
        reason.failure_message().as_str()
    )]
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
    #[error(
        "machine {} rejected volume testimony pin for {}",
        responder_machine_id.as_str(),
        expected_machine_id.as_str()
    )]
    MachineMismatch {
        expected_machine_id: MachineId,
        responder_machine_id: MachineId,
    },
}

impl NatsMachineVolumeTestimonyReader {
    #[must_use]
    pub fn new(client: async_nats::Client) -> Self {
        Self {
            client,
            request_timeout: DEFAULT_VOLUME_TESTIMONY_RPC_TIMEOUT,
        }
    }

    #[must_use]
    #[cfg(test)]
    pub const fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    pub async fn volume_testimony(
        &self,
        machine_id: &MachineId,
        pins: Vec<VolumePinState>,
    ) -> Result<Vec<MachineVolumeTestimonyResult>, MachineVolumeTestimonyReadError> {
        call_machine::<MachineVolumeTestimonyRpcOk, MachineVolumeTestimonyDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::VolumeTestimony,
            &MachineVolumeTestimonyRpcRequest { pins },
        )
        .await
        .map(|ok| ok.results)
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => MachineVolumeTestimonyReadError::Unavailable {
                machine_id: machine_id.clone(),
                reason,
            },
            MachineCallError::Domain(MachineVolumeTestimonyDomainError::MachineMismatch {
                expected_machine_id,
                responder_machine_id,
            }) => MachineVolumeTestimonyReadError::MachineMismatch {
                expected_machine_id,
                responder_machine_id,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::DEFAULT_VOLUME_TESTIMONY_RPC_TIMEOUT;
    use crate::roles::machine::VOLUME_TESTIMONY_ENDPOINT_TIMEOUT;

    #[test]
    fn client_budget_exceeds_endpoint_budget() {
        assert!(DEFAULT_VOLUME_TESTIMONY_RPC_TIMEOUT > VOLUME_TESTIMONY_ENDPOINT_TIMEOUT);
    }
}
