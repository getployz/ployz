//! Concurrent targeted bridge-capacity observations.

use std::collections::BTreeMap;

use futures_util::future::join_all;
use ployz_core::{
    BridgeEndpointCapacity, InspectRequest, MachineId, MachineObservation, MachineTarget, op,
};

use super::{Call, Client};

pub(super) async fn observe(
    client: &Client,
    machines: &[MachineObservation],
) -> BTreeMap<MachineId, BridgeEndpointCapacity> {
    let requests = machines.iter().filter_map(|machine| {
        let machine_id = machine.machine.id;
        machine.invites_rpc().then(|| {
            let mut client = client.clone();
            async move {
                let result = client
                    .call::<op::Inspect>(
                        InspectRequest {
                            telemetry: ployz_core::InspectTelemetry::BridgeCapacity,
                            ..Default::default()
                        },
                        Call::read()
                            .to(&MachineTarget::from(&machine_id))
                            .budget(crate::connect::target_read_timeout()),
                    )
                    .await
                    .map_err(crate::connect::rpc_error);
                (machine_id, result)
            }
        })
    });
    join_all(requests)
        .await
        .into_iter()
        .filter_map(|(machine_id, response)| {
            let bridge = response.ok()?.telemetry?.into_bridge();
            Some((machine_id, bridge))
        })
        .collect()
}
