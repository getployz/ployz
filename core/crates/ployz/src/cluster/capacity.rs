//! Concurrent targeted bridge-capacity observations.

use std::collections::BTreeMap;

use ployz_core::{
    BridgeEndpointCapacity, InspectRequest, MachineId, MachineObservation, MachineTarget, RpcError,
    op,
};

use super::{Call, Client, fan_out};

impl Client {
    pub(crate) async fn bridge_capacity(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, RpcError> {
        let details = self
            .call::<op::Inspect>(
                InspectRequest {
                    telemetry: ployz_core::InspectTelemetry::BridgeCapacity,
                    ..Default::default()
                },
                Call::target_read(&MachineTarget::from(machine_id)),
            )
            .await
            .map_err(crate::connect::rpc_error)?;
        Ok(details.telemetry.map(|telemetry| telemetry.into_bridge()))
    }
}

pub(super) async fn observe(
    client: &Client,
    machines: &[MachineObservation],
) -> BTreeMap<MachineId, BridgeEndpointCapacity> {
    let observed = fan_out(machines, |machine| {
        let mut client = client.clone();
        async move { client.bridge_capacity(&machine.machine.id).await }
    })
    .await;
    observed
        .successes
        .into_iter()
        .map(|success| (success.machine_id, success.value))
        .collect()
}
