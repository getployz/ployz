//! Concurrent targeted bridge-capacity observations.

use std::collections::BTreeMap;

use ployz_core::{
    BridgeEndpointCapacity, InspectRequest, MachineId, MachineObservation, MachineTarget, RpcError,
    op,
};

use super::{Call, Client, fan_out};

pub(super) async fn observe(
    client: &Client,
    machines: &[MachineObservation],
) -> BTreeMap<MachineId, BridgeEndpointCapacity> {
    let observed = fan_out(machines, |machine| {
        let mut client = client.clone();
        async move {
            let response = client
                .call::<op::Inspect>(
                    InspectRequest {
                        telemetry: ployz_core::InspectTelemetry::BridgeCapacity,
                        ..Default::default()
                    },
                    Call::read()
                        .to(&MachineTarget::from(&machine.machine.id))
                        .budget(crate::connect::target_read_timeout()),
                )
                .await
                .map_err(crate::connect::rpc_error)?;
            Ok::<_, RpcError>(response.telemetry.map(|telemetry| telemetry.into_bridge()))
        }
    })
    .await;
    observed
        .successes
        .into_iter()
        .map(|success| (success.machine_id, success.value))
        .collect()
}
